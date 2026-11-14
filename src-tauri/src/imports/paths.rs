//! The import's virtual paths. The webview never names a real folder: it
//! names `/<kind>/<place>/<path inside it>`, and this side decides where that
//! is. The places:
//!
//! - `picked`: the folder the last dialog picked, read only, for the scan;
//! - `source`: the folder the running import reads, read only;
//! - `staging`: the run's working folder under the app's data folder;
//! - `vault`: the vault the run began in.
//!
//! Reading the picked folder follows a link only when it stays inside it, so
//! nothing outside the folder the person chose is ever read. Writing refuses
//! any link below the place's top, so nothing written lands outside it.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::session::Kind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Picked,
    Source,
    Staging,
    Vault,
}

#[derive(Clone, Debug)]
pub struct VirtualPath {
    pub kind: Kind,
    pub scope: Scope,
    pub parts: Vec<String>,
    pub text: String,
}

fn bad_path(text: &str) -> String {
    format!("EINVAL: not an import path, '{text}'")
}

fn bad_segment(segment: &str) -> bool {
    segment == "."
        || segment == ".."
        || segment.contains('\0')
        || (cfg!(windows) && (segment.contains('\\') || segment.contains(':')))
}

pub fn parse(text: &str) -> Result<VirtualPath, String> {
    let rest = text.strip_prefix('/').ok_or_else(|| bad_path(text))?;
    let mut segments = rest.split('/');
    let kind = Kind::parse(segments.next().unwrap_or("")).map_err(|_| bad_path(text))?;
    let scope = match segments.next() {
        Some("picked") => Scope::Picked,
        Some("source") => Scope::Source,
        Some("staging") => Scope::Staging,
        Some("vault") => Scope::Vault,
        _ => return Err(bad_path(text)),
    };
    let mut parts = Vec::new();
    for segment in segments {
        if segment.is_empty() {
            continue;
        }
        if bad_segment(segment) {
            return Err(bad_path(text));
        }
        parts.push(segment.to_string());
    }
    Ok(VirtualPath { kind, scope, parts, text: text.to_string() })
}

/// Node's error shape (`ENOENT: …, open '<path>'`), so the webview's copy of
/// upstream's code can test `error.code` as upstream does. The path in it is
/// the virtual one.
pub fn io_message(error: &io::Error, syscall: &str, text: &str) -> String {
    use io::ErrorKind::*;
    let code = match error.kind() {
        NotFound => "ENOENT",
        AlreadyExists => "EEXIST",
        PermissionDenied => "EACCES",
        CrossesDevices => "EXDEV",
        NotADirectory => "ENOTDIR",
        IsADirectory => "EISDIR",
        DirectoryNotEmpty => "ENOTEMPTY",
        _ => "EIO",
    };
    format!("{code}: {error}, {syscall} '{text}'")
}

/// A virtual path resolved against its place.
#[derive(Clone, Debug)]
pub struct Place {
    pub vpath: VirtualPath,
    pub base: PathBuf,
    pub abs: PathBuf,
}

impl Place {
    pub fn new(vpath: VirtualPath, base: PathBuf) -> Place {
        let mut abs = base.clone();
        for part in &vpath.parts {
            abs.push(part);
        }
        Place { vpath, base, abs }
    }

    pub fn scope(&self) -> Scope {
        self.vpath.scope
    }

    pub fn kind(&self) -> Kind {
        self.vpath.kind
    }

    /// The path inside its place, POSIX style.
    pub fn rel(&self) -> String {
        self.vpath.parts.join("/")
    }

    pub fn err(&self, error: &io::Error, syscall: &str) -> String {
        io_message(error, syscall, &self.vpath.text)
    }

    pub fn denied(&self) -> String {
        format!("EPERM: operation not permitted, '{}'", self.vpath.text)
    }

    fn escapes(&self) -> String {
        format!("EPERM: path leaves the folder it names, '{}'", self.vpath.text)
    }

    /// For the picked folder and the source: the path, resolved through any
    /// link, still lies inside the folder (resolved too).
    pub fn check_inside_through_links(&self) -> Result<(), String> {
        let base = fs::canonicalize(&self.base).map_err(|e| self.err(&e, "stat"))?;
        let real = fs::canonicalize(&self.abs).map_err(|e| self.err(&e, "stat"))?;
        if real.starts_with(&base) {
            Ok(())
        } else {
            Err(self.escapes())
        }
    }

    /// For staging and the vault: no link anywhere between the place's top
    /// and the path, so whatever is written stays inside.
    pub fn check_no_links(&self) -> Result<(), String> {
        let mut current = self.base.clone();
        for part in &self.vpath.parts {
            current.push(part);
            match fs::symlink_metadata(&current) {
                Ok(meta) if meta.file_type().is_symlink() => return Err(self.escapes()),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) => return Err(self.err(&error, "lstat")),
            }
        }
        Ok(())
    }

    /// What reading needs, by place.
    pub fn check_readable(&self) -> Result<(), String> {
        match self.scope() {
            Scope::Picked | Scope::Source => self.check_inside_through_links(),
            Scope::Staging => self.check_no_links(),
            Scope::Vault => Err(self.denied()),
        }
    }
}

/// Upstream's `isInside`, on real paths: `child` is `parent` or below it.
pub fn is_inside(parent: &Path, child: &Path) -> bool {
    let real = |p: &Path| {
        fs::canonicalize(p).unwrap_or_else(|_| {
            PathBuf::from(crate::vault::config::resolve_path(&p.to_string_lossy()))
        })
    };
    real(child).starts_with(real(parent))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_four_places_and_refuses_anything_else() {
        let p = parse("/obsidian/source/Projects/Plan.md").unwrap();
        assert_eq!(p.kind, Kind::Obsidian);
        assert_eq!(p.scope, Scope::Source);
        assert_eq!(p.parts, vec!["Projects", "Plan.md"]);
        assert_eq!(parse("/notion/staging").unwrap().parts.len(), 0);
        assert_eq!(parse("/logseq/vault/inbox/").unwrap().parts, vec!["inbox"]);
        assert_eq!(parse("/logseq/picked").unwrap().scope, Scope::Picked);

        for bad in [
            "obsidian/source/a",
            "/obsidian/elsewhere/a",
            "/word/source/a",
            "/obsidian",
            "/obsidian/source/../../etc/passwd",
            "/obsidian/vault/a/./b",
            "/obsidian/staging/a/..",
            "/obsidian/source/a\0b",
            "/",
            "",
        ] {
            assert!(parse(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn a_link_out_of_the_picked_folder_is_not_followed() {
        let scratch = tempfile::tempdir().unwrap();
        let source = scratch.path().join("source");
        let outside = scratch.path().join("outside");
        fs::create_dir_all(source.join("notes")).unwrap();
        fs::create_dir_all(&outside).unwrap();
        fs::write(source.join("notes/a.md"), "a").unwrap();
        fs::write(outside.join("secret.md"), "secret").unwrap();
        let place = |text: &str| Place::new(parse(text).unwrap(), source.clone());

        assert!(place("/obsidian/source/notes/a.md").check_readable().is_ok());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, source.join("Linked")).unwrap();
            std::os::unix::fs::symlink(source.join("notes/a.md"), source.join("alias.md")).unwrap();
            let out = place("/obsidian/source/Linked/secret.md");
            assert!(out.check_readable().unwrap_err().starts_with("EPERM"));
            // A link that stays inside the folder is fine to read.
            assert!(place("/obsidian/source/alias.md").check_readable().is_ok());
        }
        assert!(place("/obsidian/source/missing.md").check_readable().unwrap_err().starts_with("ENOENT"));
    }

    #[cfg(unix)]
    #[test]
    fn nothing_is_written_through_a_link() {
        let scratch = tempfile::tempdir().unwrap();
        let vault = scratch.path().join("vault");
        let outside = scratch.path().join("outside");
        fs::create_dir_all(&vault).unwrap();
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, vault.join("inbox")).unwrap();
        let place = |text: &str| Place::new(parse(text).unwrap(), vault.clone());
        assert!(place("/notion/vault/inbox/Notion Import").check_no_links().is_err());
        assert!(place("/notion/vault/Notion Import/a.md").check_no_links().is_ok());
        assert!(place("/notion/vault/inbox").check_no_links().is_err());
    }

    #[test]
    fn overlap_is_checked_both_ways_on_real_paths() {
        let scratch = tempfile::tempdir().unwrap();
        let vault = scratch.path().join("vault");
        fs::create_dir_all(vault.join("inner")).unwrap();
        assert!(is_inside(scratch.path(), &vault));
        assert!(is_inside(&vault, &vault.join("inner")));
        assert!(is_inside(&vault, &vault));
        assert!(!is_inside(&vault.join("inner"), &vault));
        assert!(!is_inside(&vault, &scratch.path().join("vault2")));
    }
}
