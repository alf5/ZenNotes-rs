//! Workflow file I/O — port of apps/desktop/src/main/workflows.ts.
//!
//! Workflows are plain `.md` files under `.zennotes/workflows/`. Like
//! templates.rs this module is deliberately parse-free: it moves raw bytes and
//! never interprets the format. The grammar lives in
//! `@shared/workflows/parse`, so the format has exactly one home and the Rust
//! side never has to learn a new node kind.
//!
//! The applier (journal, rollback, run ledgers) is NOT here: its ops are
//! transformed by the vendored `@shared/workflows/apply-ops`, so it runs in
//! src/bridge/workflows.ts over the generic `db_*` file primitives — the same
//! split the CSV databases use.

use std::fs;
use std::path::{Path, PathBuf};

use crate::ipc::types::{ImportedWorkflowFile, WorkflowFile};

const WORKFLOWS_REL_DIR: &str = ".zennotes/workflows";

/// The most a workflow file may weigh on import, in bytes. Upstream's
/// MAX_IMPORT_BYTES: three orders of magnitude above the largest preset, and
/// small enough that a file picked by mistake is refused after one stat.
const MAX_IMPORT_BYTES: u64 = 512 * 1024;

/// The longest stem we will write. Leaves room for `.md` plus the atomic-write
/// temp suffix inside NAME_MAX (255), so pasting a paragraph as a workflow
/// name truncates instead of failing the save with ENAMETOOLONG.
const MAX_SLUG_LENGTH: usize = 64;

fn workflows_dir(root: &Path) -> PathBuf {
    root.join(".zennotes").join("workflows")
}

fn source_path_for_name(name: &str) -> String {
    format!("{WORKFLOWS_REL_DIR}/{name}")
}

/// The filename stem is the workflow id: it is what `call` steps reference and
/// what keybindings resolve against, so it has to survive a round-trip through
/// the filesystem unchanged.
fn workflow_id_for_name(name: &str) -> String {
    let lower = name.to_lowercase();
    if lower.ends_with(".md") {
        name[..name.len() - 3].to_string()
    } else {
        name.to_string()
    }
}

/// A filename stem that cannot leave the workflows directory: lowercase
/// letters, digits and single dashes. Mirrors `safeSlug` in templates.rs.
///
/// Defensive on purpose. The slug is derived from the workflow's `name`, which
/// is whatever the user typed into the frontmatter, so `My Books!`,
/// `../../evil` and `/etc/passwd` all arrive here. Dropping every separator
/// (rather than rejecting) means the save still succeeds, which is the right
/// call for a field the user is editing live.
fn safe_slug(slug: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in slug.to_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
        if out.len() >= MAX_SLUG_LENGTH {
            break;
        }
    }
    // After the truncation, so cutting onto a dash cannot leave a trailing one.
    let cleaned = out.trim_matches('-').to_string();
    if cleaned.is_empty() {
        "workflow".to_string()
    } else {
        cleaned
    }
}

/// Resolve a vault-relative sourcePath to an absolute path, rejecting anything
/// outside the flat workflows directory (no traversal, no subdirectories).
///
/// Every workflow read/write passes through this gate; a second copy of the
/// check would be a second chance to get it wrong.
pub fn resolve_workflow_path(root: &Path, source_path: &str) -> Result<PathBuf, String> {
    let dir = workflows_dir(root);
    let abs = PathBuf::from(crate::vault::config::resolve_path(
        &root.join(source_path).to_string_lossy(),
    ));
    let dir_abs = crate::vault::config::resolve_path(&dir.to_string_lossy());
    let abs_str = abs.to_string_lossy().to_string();
    let sep = std::path::MAIN_SEPARATOR;
    match abs_str.strip_prefix(&format!("{dir_abs}{sep}")) {
        Some(r) if !r.contains(sep) => {}
        _ => {
            return Err(format!(
                "Refusing workflow path outside workflows dir: {source_path}"
            ))
        }
    }
    if !abs_str.to_lowercase().ends_with(".md") {
        return Err(format!("Workflow path must be a .md file: {source_path}"));
    }
    Ok(abs)
}

/// Every workflow file in the vault, raw. A missing directory is the normal
/// case (most vaults never author a workflow), so it reads as an empty list
/// rather than an error the renderer would have to special-case.
pub fn list_workflow_files(root: &Path) -> Vec<WorkflowFile> {
    let Ok(entries) = fs::read_dir(workflows_dir(root)) else {
        return Vec::new();
    };
    let mut files: Vec<WorkflowFile> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || !name.to_lowercase().ends_with(".md") {
            continue;
        }
        let source_path = source_path_for_name(&name);
        let Ok(abs) = resolve_workflow_path(root, &source_path) else {
            continue;
        };
        match fs::read_to_string(&abs) {
            Ok(raw) => files.push(WorkflowFile {
                id: workflow_id_for_name(&name),
                source_path,
                raw,
            }),
            Err(e) => eprintln!("[workflows] skipping unreadable workflow {name}: {e}"),
        }
    }
    // By id, because that is the order the palette and the workflow list show,
    // and read_dir order is filesystem-dependent.
    files.sort_by(|a, b| a.id.cmp(&b.id));
    files
}

/// Create or overwrite a workflow file.
///
/// Overwrites rather than de-duplicating, which is where this parts ways with
/// `write_custom_template`: the filename stem *is* the workflow id that `call`
/// steps and keybindings resolve against, so saving the same workflow twice has
/// to land on the same file instead of quietly forking into `reading-log-2`.
pub fn write_workflow_file(
    root: &Path,
    slug: &str,
    raw: &str,
    previous_source_path: Option<&str>,
) -> Result<WorkflowFile, String> {
    let name = format!("{}.md", safe_slug(slug));
    let source_path = source_path_for_name(&name);
    // Through the same gate as list and delete even though `safe_slug` has
    // already made an escape impossible: one guard on every path that touches
    // disk means there is only one place this can be got wrong.
    let abs = resolve_workflow_path(root, &source_path)?;
    // Resolved before anything is written, so a malicious previousSourcePath is
    // rejected up front rather than halfway through a rename.
    let prev_abs = match previous_source_path {
        Some(prev) => Some(resolve_workflow_path(root, prev)?),
        None => None,
    };

    let dir = workflows_dir(root);
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
    // Temp file + rename: a crash mid-save must not truncate a workflow the
    // user already had, and the vault watcher must never observe half a file.
    write_atomic(&abs, raw)?;

    // A rename during an edit. The old file goes only after the new one is
    // durably in place, so a failed write leaves the original as the surviving
    // copy instead of destroying both. Compare canonical paths rather than
    // spelling, so a case-insensitive filesystem cannot delete the file that
    // was just saved.
    if let Some(prev_abs) = prev_abs {
        let same = fs::canonicalize(&prev_abs)
            .ok()
            .zip(fs::canonicalize(&abs).ok())
            .map(|(a, b)| a == b)
            .unwrap_or(false);
        if !same {
            let _ = fs::remove_file(&prev_abs);
        }
    }

    Ok(WorkflowFile {
        id: workflow_id_for_name(&name),
        source_path,
        raw: raw.to_string(),
    })
}

/// Delete a workflow file. An already-missing file is success: the caller asked
/// for it to be gone, and that is the state it is in.
pub fn delete_workflow_file(root: &Path, source_path: &str) -> Result<(), String> {
    let abs = resolve_workflow_path(root, source_path)?;
    let _ = fs::remove_file(abs);
    Ok(())
}

/// A filename for a native save dialog: no separators, no traversal, `.md`.
///
/// The suggestion comes from the renderer, which derived it from a workflow id,
/// which came from a filename the user may have typed. It is only a default in
/// a dialog the user then confirms, but a default that walks out of the folder
/// the dialog opened in is still a surprise worth refusing.
pub fn safe_export_filename(name: &str) -> String {
    let stem = if name.to_lowercase().ends_with(".md") {
        &name[..name.len() - 3]
    } else {
        name
    };
    let mut out = String::new();
    let mut prev_dash = false;
    for c in stem.chars() {
        // Separators, the Windows-reserved set, control characters and
        // whitespace all collapse to the same single dash.
        let bad = matches!(c, '/' | '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*')
            || c.is_control()
            || c.is_whitespace();
        if bad {
            if !prev_dash {
                out.push('-');
                prev_dash = true;
            }
        } else {
            out.push(c);
            prev_dash = false;
        }
        if out.len() >= MAX_SLUG_LENGTH {
            break;
        }
    }
    // Last, so truncating onto a separator cannot leave one behind, and so the
    // `..` a traversal attempt left as text goes with it rather than heading a
    // filename with a run of dots.
    let cleaned = out.trim_matches(|c| c == '-' || c == '.').to_string();
    format!("{}.md", if cleaned.is_empty() { "workflow" } else { &cleaned })
}

/// Read a file the user picked, as a workflow candidate.
///
/// Returns bytes and a stem, and nothing else: whether this is a workflow at
/// all is the review's question, in the renderer (`@shared/workflows/share`),
/// and answering it here would be a second parser to keep in step with the
/// first. Refuses a file too large to be one before reading it, so a wrong pick
/// costs a stat rather than a gigabyte.
pub fn read_workflow_import_file(abs: &Path) -> Result<ImportedWorkflowFile, String> {
    let meta = fs::metadata(abs).map_err(|e| format!("read failed: {e}"))?;
    if !meta.is_file() {
        return Err("That is not a file.".into());
    }
    if meta.len() > MAX_IMPORT_BYTES {
        return Err(format!(
            "That file is {} KB, which is far larger than any workflow.",
            meta.len().div_ceil(1024)
        ));
    }
    let bytes = fs::read(abs).map_err(|e| format!("read failed: {e}"))?;
    // Re-checked on what actually arrived, which closes the tail of the window
    // between the stat and the read (a file appended to in between).
    if bytes.len() as u64 > MAX_IMPORT_BYTES {
        return Err(format!(
            "That file is {} KB, which is far larger than any workflow.",
            (bytes.len() as u64).div_ceil(1024)
        ));
    }
    let raw = String::from_utf8(bytes).map_err(|_| "That file is not UTF-8 text.".to_string())?;
    let name = abs
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    Ok(ImportedWorkflowFile {
        name: workflow_id_for_name(&name),
        raw,
    })
}

fn write_atomic(abs: &Path, text: &str) -> Result<(), String> {
    let Some(parent) = abs.parent() else {
        return fs::write(abs, text).map_err(|e| format!("write failed: {e}"));
    };
    fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    let tmp = parent.join(format!(
        ".{}.{}.tmp",
        abs.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        std::process::id()
    ));
    fs::write(&tmp, text).map_err(|e| format!("write failed: {e}"))?;
    fs::rename(&tmp, abs).map_err(|e| format!("rename failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::layout;

    fn vault() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        layout::ensure_vault_layout(dir.path()).unwrap();
        dir
    }

    #[test]
    fn slug_is_filesystem_safe() {
        assert_eq!(safe_slug("Reading Log"), "reading-log");
        assert_eq!(safe_slug("../../evil"), "evil");
        assert_eq!(safe_slug("/etc/passwd"), "etc-passwd");
        assert_eq!(safe_slug("!!!"), "workflow");
        assert!(safe_slug(&"x".repeat(200)).len() <= MAX_SLUG_LENGTH);
    }

    #[test]
    fn resolve_refuses_paths_outside_the_workflows_dir() {
        let v = vault();
        assert!(resolve_workflow_path(v.path(), ".zennotes/workflows/a.md").is_ok());
        assert!(resolve_workflow_path(v.path(), ".zennotes/workflows/sub/a.md").is_err());
        assert!(resolve_workflow_path(v.path(), ".zennotes/templates/a.md").is_err());
        assert!(resolve_workflow_path(v.path(), ".zennotes/workflows/../../a.md").is_err());
        // Not a .md file.
        assert!(resolve_workflow_path(v.path(), ".zennotes/workflows/a.txt").is_err());
    }

    #[test]
    fn write_overwrites_rather_than_forking_the_id() {
        let v = vault();
        let a = write_workflow_file(v.path(), "Reading Log", "one", None).unwrap();
        let b = write_workflow_file(v.path(), "Reading Log", "two", None).unwrap();
        assert_eq!(a.source_path, b.source_path);
        assert_eq!(a.id, "reading-log");
        let listed = list_workflow_files(v.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].raw, "two");
    }

    #[test]
    fn a_rename_removes_the_previous_file() {
        let v = vault();
        let first = write_workflow_file(v.path(), "old name", "body", None).unwrap();
        write_workflow_file(v.path(), "new name", "body", Some(&first.source_path)).unwrap();
        let listed = list_workflow_files(v.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "new-name");
    }

    #[test]
    fn listing_skips_dotfiles_and_the_runs_dir() {
        let v = vault();
        write_workflow_file(v.path(), "keep", "body", None).unwrap();
        let dir = workflows_dir(v.path());
        fs::create_dir_all(dir.join(".runs")).unwrap();
        fs::write(dir.join(".runs/r.json"), "{}").unwrap();
        fs::write(dir.join(".hidden.md"), "x").unwrap();
        fs::write(dir.join("notes.txt"), "x").unwrap();
        let listed = list_workflow_files(v.path());
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "keep");
    }

    #[test]
    fn delete_is_idempotent() {
        let v = vault();
        let f = write_workflow_file(v.path(), "gone", "body", None).unwrap();
        assert!(delete_workflow_file(v.path(), &f.source_path).is_ok());
        assert!(delete_workflow_file(v.path(), &f.source_path).is_ok());
        assert!(list_workflow_files(v.path()).is_empty());
    }

    #[test]
    fn export_filename_cannot_traverse() {
        assert_eq!(safe_export_filename("reading-log"), "reading-log.md");
        assert_eq!(safe_export_filename("../../etc/passwd"), "etc-passwd.md");
        assert_eq!(safe_export_filename("a b\tc"), "a-b-c.md");
        assert_eq!(safe_export_filename("..."), "workflow.md");
    }

    #[test]
    fn import_refuses_an_oversized_file() {
        let v = vault();
        let big = v.path().join("big.md");
        fs::write(&big, "x".repeat((MAX_IMPORT_BYTES + 1) as usize)).unwrap();
        assert!(read_workflow_import_file(&big).is_err());

        let ok = v.path().join("ok.md");
        fs::write(&ok, "# hi").unwrap();
        let imported = read_workflow_import_file(&ok).unwrap();
        assert_eq!(imported.name, "ok");
        assert_eq!(imported.raw, "# hi");
    }
}
