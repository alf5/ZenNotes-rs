//! Following a link from a note that has no vault — port of
//! apps/desktop/src/main/standalone-links.ts (upstream #626).
//!
//! A markdown file opened from outside every known vault lives in a standalone
//! window with no vault behind it, and every resolver in the app is
//! vault-bound: wikilinks look up the vault's note index, relative links
//! resolve against a vault-relative note path. None of that has anything to
//! stand on for a loose file, so every link in that window was dead.
//!
//! The one thing a loose file does have is a directory. This resolves a link
//! the way the file's author meant it: a relative href from the file's own
//! directory, a `[[wikilink]]` by name within that directory's tree, and hands
//! the result back to the opener, which already knows whether an absolute
//! markdown path belongs to a known vault or gets a standalone window.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// How far below the note's directory a bare wikilink name is looked for. A
/// generated wiki nests a few levels at most; a whole home directory is not a
/// place to go looking for `[[notes]]`.
const WIKILINK_SEARCH_DEPTH: usize = 4;
/// Upper bound on directory entries visited for one lookup.
const WIKILINK_SEARCH_BUDGET: usize = 5000;

const SKIPPED_DIRS: [&str; 2] = ["node_modules", ".git"];
const MARKDOWN_EXTENSIONS: [&str; 2] = [".md", ".markdown"];

/// A link clicked inside a standalone external-file window. Deserialization is
/// the guard: anything that is not one of these two shapes is refused before it
/// reaches the filesystem.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ExternalFileLink {
    Wikilink { target: String },
    Href { href: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    Markdown(PathBuf),
    File(PathBuf),
}

fn is_markdown_path(path: &Path) -> bool {
    let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    MARKDOWN_EXTENSIONS.iter().any(|ext| name.ends_with(ext))
}

/// The file a link from `note_path` names, or None when it names nothing on
/// disk. Web URLs, mail links, in-page anchors and app schemes are not files
/// and resolve to None here; the renderer keeps those on their own paths.
pub fn resolve_standalone_link(note_path: &Path, link: &ExternalFileLink) -> Option<LinkTarget> {
    let note_dir = note_path.parent()?.to_path_buf();
    let abs = match link {
        ExternalFileLink::Wikilink { target } => resolve_wikilink(&note_dir, target),
        ExternalFileLink::Href { href } => resolve_href(&note_dir, href),
    }?;
    Some(if is_markdown_path(&abs) { LinkTarget::Markdown(abs) } else { LinkTarget::File(abs) })
}

fn resolve_href(note_dir: &Path, raw_href: &str) -> Option<PathBuf> {
    let href = raw_href.trim();
    if href.is_empty() || href.starts_with('#') {
        return None;
    }
    let candidate = if href.len() > 7 && href[..7].eq_ignore_ascii_case("file://") {
        let rest = &href[7..];
        let rest = rest.strip_prefix("localhost").unwrap_or(rest);
        PathBuf::from(
            urlencoding::decode(rest).map(|c| c.into_owned()).unwrap_or_else(|_| rest.to_string()),
        )
    } else {
        // Any other scheme (https:, mailto:, zen-asset:) is not a path from
        // this directory. A Windows drive letter looks like a scheme and is
        // not one, so it is excluded from the test.
        if has_uri_scheme(href) && !is_windows_drive_path(href) {
            return None;
        }
        let path_only = href.split(['?', '#']).next().unwrap_or(href);
        // A stray `%` in a hand-written link is still a path.
        let decoded = urlencoding::decode(path_only)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| path_only.to_string());
        if decoded.is_empty() {
            return None;
        }
        let as_path = PathBuf::from(&decoded);
        if as_path.is_absolute() {
            as_path
        } else {
            normalize(&note_dir.join(as_path))
        }
    };
    existing_file(&candidate)
}

/// True for `scheme:` at the head of the string, per RFC 3986's scheme rule.
fn has_uri_scheme(value: &str) -> bool {
    let Some(colon) = value.find(':') else { return false };
    if colon == 0 {
        return false;
    }
    let scheme = &value[..colon];
    let mut chars = scheme.chars();
    chars.next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false)
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

fn is_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// Resolve `.`/`..` textually. The target need not exist yet, so
/// `fs::canonicalize` is not usable here.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `candidate` when it is a file, else the markdown file it names without an
/// extension (`[Readme](../README)` for `README.md`), else None.
fn existing_file(candidate: &Path) -> Option<PathBuf> {
    if candidate.is_file() {
        return Some(candidate.to_path_buf());
    }
    if candidate.extension().is_some() {
        return None;
    }
    for ext in MARKDOWN_EXTENSIONS {
        let with_ext = PathBuf::from(format!("{}{ext}", candidate.to_string_lossy()));
        if with_ext.is_file() {
            return Some(with_ext);
        }
    }
    None
}

fn resolve_wikilink(note_dir: &Path, raw_target: &str) -> Option<PathBuf> {
    // `[[Doc|alias]]`, `[[Doc#Heading]]` and `[[Doc^block]]` all name Doc.
    let target = raw_target
        .split('|')
        .next()
        .unwrap_or(raw_target)
        .split(['#', '^'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if target.is_empty() {
        return None;
    }
    let lower = target.to_lowercase();
    // A path-like target is Obsidian's "relative to this note" form.
    if target.contains('/') || MARKDOWN_EXTENSIONS.iter().any(|e| lower.ends_with(e)) {
        return existing_file(&normalize(&note_dir.join(&target)));
    }
    find_by_name(note_dir, &target)
}

/// The shallowest markdown file named `name` (case-insensitively, extension
/// aside) under `root`, ties broken alphabetically for a stable answer.
fn find_by_name(root: &Path, name: &str) -> Option<PathBuf> {
    let wanted = name.to_lowercase();
    let mut visited = 0usize;
    let mut level: Vec<PathBuf> = vec![root.to_path_buf()];
    for _ in 0..=WIKILINK_SEARCH_DEPTH {
        if level.is_empty() {
            break;
        }
        let mut next: Vec<PathBuf> = Vec::new();
        let mut hits: Vec<PathBuf> = Vec::new();
        level.sort();
        for dir in &level {
            let Ok(entries) = fs::read_dir(dir) else { continue };
            for entry in entries.flatten() {
                visited += 1;
                if visited > WIKILINK_SEARCH_BUDGET {
                    hits.sort();
                    return hits.into_iter().next();
                }
                let full = entry.path();
                let entry_name = entry.file_name().to_string_lossy().to_string();
                let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                if is_dir {
                    if !entry_name.starts_with('.') && !SKIPPED_DIRS.contains(&entry_name.as_str()) {
                        next.push(full);
                    }
                    continue;
                }
                if !is_markdown_path(&full) {
                    continue;
                }
                let stem = full.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                if stem.to_lowercase() == wanted {
                    hits.push(full);
                }
            }
        }
        if !hits.is_empty() {
            hits.sort();
            return hits.into_iter().next();
        }
        level = next;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("note.md"), "x").unwrap();
        fs::write(root.join("README.md"), "x").unwrap();
        fs::write(root.join("photo.png"), "x").unwrap();
        fs::create_dir_all(root.join("sub/deeper")).unwrap();
        fs::write(root.join("sub/Target.md"), "x").unwrap();
        fs::write(root.join("sub/deeper/Deep.markdown"), "x").unwrap();
        fs::create_dir_all(root.join("node_modules")).unwrap();
        fs::write(root.join("node_modules/Target.md"), "x").unwrap();
        dir
    }

    fn note(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("note.md")
    }

    #[test]
    fn a_relative_href_resolves_against_the_notes_directory() {
        let d = tree();
        let got = resolve_standalone_link(&note(&d), &ExternalFileLink::Href { href: "sub/Target.md".into() });
        assert_eq!(got, Some(LinkTarget::Markdown(d.path().join("sub/Target.md"))));
    }

    #[test]
    fn an_extensionless_href_finds_the_markdown_file() {
        let d = tree();
        let got = resolve_standalone_link(&note(&d), &ExternalFileLink::Href { href: "README".into() });
        assert_eq!(got, Some(LinkTarget::Markdown(d.path().join("README.md"))));
    }

    #[test]
    fn a_non_markdown_file_resolves_as_a_plain_file() {
        let d = tree();
        let got = resolve_standalone_link(&note(&d), &ExternalFileLink::Href { href: "photo.png".into() });
        assert_eq!(got, Some(LinkTarget::File(d.path().join("photo.png"))));
    }

    #[test]
    fn web_mail_and_anchor_links_are_not_files() {
        let d = tree();
        for href in ["https://example.com/x.md", "mailto:a@b.c", "#heading", "zen-asset://x"] {
            let got = resolve_standalone_link(&note(&d), &ExternalFileLink::Href { href: href.into() });
            assert_eq!(got, None, "{href} should not resolve");
        }
    }

    #[test]
    fn a_percent_encoded_href_is_decoded() {
        let d = tree();
        fs::write(d.path().join("a b.md"), "x").unwrap();
        let got = resolve_standalone_link(&note(&d), &ExternalFileLink::Href { href: "a%20b.md".into() });
        assert_eq!(got, Some(LinkTarget::Markdown(d.path().join("a b.md"))));
    }

    #[test]
    fn a_query_or_anchor_is_stripped_before_the_lookup() {
        let d = tree();
        let got = resolve_standalone_link(
            &note(&d),
            &ExternalFileLink::Href { href: "sub/Target.md#heading".into() },
        );
        assert_eq!(got, Some(LinkTarget::Markdown(d.path().join("sub/Target.md"))));
    }

    #[test]
    fn a_wikilink_is_found_by_name_below_the_directory() {
        let d = tree();
        let got = resolve_standalone_link(
            &note(&d),
            &ExternalFileLink::Wikilink { target: "target".into() },
        );
        assert_eq!(got, Some(LinkTarget::Markdown(d.path().join("sub/Target.md"))));
    }

    #[test]
    fn a_wikilink_carrying_an_alias_or_anchor_still_names_the_file() {
        let d = tree();
        for target in ["Target|see this", "Target#Heading", "Target^block"] {
            let got = resolve_standalone_link(
                &note(&d),
                &ExternalFileLink::Wikilink { target: target.into() },
            );
            assert_eq!(got, Some(LinkTarget::Markdown(d.path().join("sub/Target.md"))), "{target}");
        }
    }

    #[test]
    fn the_wikilink_search_skips_node_modules() {
        let d = tree();
        // Both trees hold Target.md; the node_modules copy must never win.
        let got = resolve_standalone_link(
            &note(&d),
            &ExternalFileLink::Wikilink { target: "Target".into() },
        );
        assert_eq!(got, Some(LinkTarget::Markdown(d.path().join("sub/Target.md"))));
    }

    #[test]
    fn a_missing_target_resolves_to_nothing() {
        let d = tree();
        let got = resolve_standalone_link(
            &note(&d),
            &ExternalFileLink::Wikilink { target: "nope".into() },
        );
        assert_eq!(got, None);
    }

    #[test]
    fn a_traversing_href_stays_textual_and_must_still_exist() {
        let d = tree();
        let got = resolve_standalone_link(
            &note(&d),
            &ExternalFileLink::Href { href: "../../../etc/hostname".into() },
        );
        // Whatever it resolves to, it is not this vault's business, and the
        // file must exist for the opener to be handed anything at all.
        assert!(matches!(got, None | Some(LinkTarget::File(_))));
    }
}
