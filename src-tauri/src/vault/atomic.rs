//! Atomic file writes — port of apps/desktop/src/main/atomic-write.ts and
//! `writeFileAtomic` in vault.ts.
//!
//! A plain `fs::write` truncates first, and a watcher echo of the previous
//! save can read the file inside that window: the renderer then sees an empty
//! "external change" and replaces the open buffer with it, wiping the note
//! (upstream #585). Temp file + rename means no reader can ever observe a
//! half-written note.
//!
//! One deliberate difference: the temp file is a dotfile (`.<name>.<pid>.<stamp>.tmp`)
//! beside the target rather than upstream's `<name>.<pid>.<stamp>.tmp`. The
//! watcher and the listings already skip dot-prefixed names, so the temp file
//! never surfaces as a stray asset or a phantom change event.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ATOMIC_RENAME_ATTEMPTS: u32 = 20;
static ATOMIC_WRITE_SEQUENCE: AtomicU32 = AtomicU32::new(0);

fn now_ms() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// A reader that temporarily denies replacing the destination (Windows
/// antivirus, a search indexer): EACCES / EPERM / EBUSY, or Windows' sharing
/// violation, which Node also reports as one of those.
fn transient_rename_error(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        return true;
    }
    matches!(error.raw_os_error(), Some(code) if (cfg!(unix) && code == 16) || (cfg!(windows) && code == 32))
}

/// Port of `renameWithRetry`: wait out a reader that briefly holds the file.
pub fn rename_with_retry(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut attempt = 1;
    loop {
        match fs::rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if attempt < ATOMIC_RENAME_ATTEMPTS && transient_rename_error(&error) => {
                std::thread::sleep(Duration::from_millis((1u64 << (attempt - 1)).min(25)));
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// Port of `atomicWriteTarget`: follow a symlink to the file it points at, so
/// an atomic write lands on the target instead of replacing the link. A
/// dangling link resolves to the path it names, which is where a plain write
/// would have created the file.
pub fn atomic_write_target(abs: &Path) -> PathBuf {
    match fs::symlink_metadata(abs) {
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::canonicalize(abs).unwrap_or_else(|_| match fs::read_link(abs) {
                // `join` with an absolute link text replaces, like path.resolve.
                Ok(text) => abs.parent().map(|dir| dir.join(&text)).unwrap_or(text),
                Err(_) => abs.to_path_buf(),
            })
        }
        _ => abs.to_path_buf(),
    }
}

/// Port of `writeFileAtomic`: temp file beside the (link-resolved) target,
/// created exclusively, fsynced, given the target's mode, renamed over it.
pub fn write_file_atomic(abs: &Path, data: &str) -> Result<(), String> {
    let target = atomic_write_target(abs);
    let parent = target.parent().map(Path::to_path_buf).unwrap_or_default();
    fs::create_dir_all(&parent).map_err(|e| format!("mkdir failed: {e}"))?;
    let sequence = ATOMIC_WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed) % 1000;
    let name = target.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let tmp = parent.join(format!(".{name}.{}.{}{sequence:03}.tmp", std::process::id(), now_ms()));
    let existing = fs::metadata(&target).ok().map(|m| m.permissions());
    // create_new rather than truncate: a temp file that somehow already exists
    // means another writer is mid-flight, and failing the save (the note stays
    // dirty and the next save retries) beats two writers sharing one file.
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| format!("write failed: {e}"))?;
    if let Err(error) = file.write_all(data.as_bytes()) {
        drop(file);
        let _ = fs::remove_file(&tmp);
        return Err(format!("write failed: {error}"));
    }
    if let Err(error) = file.sync_all() {
        eprintln!("fsync failed for atomic write: {error}");
    }
    drop(file);
    let finish = || -> std::io::Result<()> {
        if let Some(permissions) = existing {
            fs::set_permissions(&tmp, permissions)?;
        }
        rename_with_retry(&tmp, &target)
    };
    finish().map_err(|error| {
        let _ = fs::remove_file(&tmp);
        format!("write failed: {error}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_content_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("Note.md");
        fs::write(&file, "old").unwrap();
        write_file_atomic(&file, "new").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "new");
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(names.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn writes_through_a_symlink_and_keeps_the_link_and_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("real.md");
        fs::write(&target, "old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("link.md");
        std::os::unix::fs::symlink("real.md", &link).unwrap();
        write_file_atomic(&link, "new").unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        assert_eq!(fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
