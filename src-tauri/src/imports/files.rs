//! The file operations the webview's copy of upstream's run code makes, one
//! per `fs` call it used (src/bridge/imports/fs.ts), each allowed only where
//! upstream's run would make it:
//!
//! | operation      | picked/source | staging | vault                          |
//! |----------------|---------------|---------|--------------------------------|
//! | read_dir, read | yes           | yes     | no                             |
//! | stat           | yes           | yes     | yes                            |
//! | write          | no            | yes     | only a new file (`wx`)         |
//! | append         | no            | yes     | only inside a folder it moved in |
//! | mkdir          | no            | yes     | yes                            |
//! | copy           | from          | to      | no                             |
//! | utimes, rm, rename | no        | yes     | no                             |
//! | move in        | no            | from    | to a name nothing has yet      |
//!
//! So an import reads only the folder the person picked, and in the vault it
//! never overwrites, deletes or moves a thing that was there.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::paths::{Place, Scope};

#[derive(Debug, Serialize, PartialEq)]
pub struct EntryInfo {
    pub name: String,
    /// "file", "dir", "symlink" or "other": the entry itself, not followed,
    /// as `readdir(…, { withFileTypes: true })` reports it.
    pub kind: &'static str,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatInfo {
    pub is_file: bool,
    pub is_directory: bool,
    pub size: u64,
    pub atime_ms: f64,
    pub mtime_ms: f64,
    /// 0 where the system keeps no birth time, as Node reports it.
    pub birthtime_ms: f64,
}

fn ms(time: io::Result<SystemTime>) -> f64 {
    time.ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0)
}

impl StatInfo {
    fn of(meta: &fs::Metadata) -> StatInfo {
        StatInfo {
            is_file: meta.is_file(),
            is_directory: meta.is_dir(),
            size: meta.len(),
            atime_ms: ms(meta.accessed()),
            mtime_ms: ms(meta.modified()),
            birthtime_ms: ms(meta.created()),
        }
    }
}

fn system_time(ms: f64) -> SystemTime {
    if !ms.is_finite() {
        return UNIX_EPOCH;
    }
    let span = Duration::from_secs_f64(ms.abs() / 1000.0);
    if ms >= 0.0 {
        UNIX_EPOCH + span
    } else {
        UNIX_EPOCH - span
    }
}

fn same_kind(a: &Place, b: &Place) -> bool {
    a.kind() == b.kind()
}

pub fn read_dir(place: &Place) -> Result<Vec<EntryInfo>, String> {
    place.check_readable()?;
    let entries = fs::read_dir(&place.abs).map_err(|e| place.err(&e, "scandir"))?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| place.err(&e, "scandir"))?;
        let kind = match entry.file_type() {
            Ok(t) if t.is_symlink() => "symlink",
            Ok(t) if t.is_dir() => "dir",
            Ok(t) if t.is_file() => "file",
            _ => "other",
        };
        out.push(EntryInfo { name: entry.file_name().to_string_lossy().into_owned(), kind });
    }
    Ok(out)
}

/// UTF-8, with a bad sequence replaced, as `readFile(…, 'utf8')` reads.
pub fn read_text(place: &Place) -> Result<String, String> {
    place.check_readable()?;
    let bytes = fs::read(&place.abs).map_err(|e| place.err(&e, "open"))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Follows links, as `fs.stat` does. In the vault it only ever answers
/// "is something there", so a link there counts even when it leads nowhere.
pub fn stat(place: &Place) -> Result<StatInfo, String> {
    let meta = if place.scope() == Scope::Vault {
        fs::symlink_metadata(&place.abs).map(|own| fs::metadata(&place.abs).unwrap_or(own))
    } else {
        place.check_readable()?;
        fs::metadata(&place.abs)
    };
    meta.map(|m| StatInfo::of(&m)).map_err(|e| place.err(&e, "stat"))
}

pub fn write_text(place: &Place, text: &str, exclusive: bool) -> Result<(), String> {
    match place.scope() {
        Scope::Staging => {}
        Scope::Vault if exclusive => {}
        _ => return Err(place.denied()),
    }
    place.check_no_links()?;
    let mut options = fs::OpenOptions::new();
    options.write(true);
    if exclusive {
        options.create_new(true);
    } else {
        options.create(true).truncate(true);
    }
    let mut file = options.open(&place.abs).map_err(|e| place.err(&e, "open"))?;
    file.write_all(text.as_bytes()).map_err(|e| place.err(&e, "write"))
}

pub fn append_text(place: &Place, text: &str, moved_in: &[PathBuf]) -> Result<(), String> {
    match place.scope() {
        Scope::Staging => {}
        Scope::Vault if moved_in.iter().any(|dir| place.abs.starts_with(dir) && place.abs != *dir) => {}
        _ => return Err(place.denied()),
    }
    place.check_no_links()?;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&place.abs)
        .map_err(|e| place.err(&e, "open"))?;
    file.write_all(text.as_bytes()).map_err(|e| place.err(&e, "write"))
}

pub fn mkdir(place: &Place) -> Result<(), String> {
    if !matches!(place.scope(), Scope::Staging | Scope::Vault) {
        return Err(place.denied());
    }
    place.check_no_links()?;
    fs::create_dir_all(&place.abs).map_err(|e| place.err(&e, "mkdir"))
}

pub fn copy(from: &Place, to: &Place) -> Result<(), String> {
    if !matches!(from.scope(), Scope::Source | Scope::Staging) || to.scope() != Scope::Staging || !same_kind(from, to) {
        return Err(to.denied());
    }
    from.check_readable()?;
    to.check_no_links()?;
    fs::copy(&from.abs, &to.abs).map(|_| ()).map_err(|e| to.err(&e, "copyfile"))
}

fn set_file_times(path: &Path, accessed: SystemTime, modified: SystemTime) -> io::Result<()> {
    let times = fs::FileTimes::new().set_accessed(accessed).set_modified(modified);
    let file = match fs::OpenOptions::new().write(true).open(path) {
        Ok(file) => file,
        // A folder opens read only (Unix); Windows can't open one this way.
        Err(_) => fs::File::open(path)?,
    };
    file.set_times(times)
}

pub fn utimes(place: &Place, atime_ms: f64, mtime_ms: f64) -> Result<(), String> {
    if place.scope() != Scope::Staging {
        return Err(place.denied());
    }
    place.check_no_links()?;
    set_file_times(&place.abs, system_time(atime_ms), system_time(mtime_ms))
        .map_err(|e| place.err(&e, "utime"))
}

/// `fs.rm(…, { recursive: true, force: true })`, in staging only.
pub fn remove(place: &Place) -> Result<(), String> {
    if place.scope() != Scope::Staging {
        return Err(place.denied());
    }
    place.check_no_links()?;
    let result = match fs::symlink_metadata(&place.abs) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(&place.abs),
        Ok(_) => fs::remove_file(&place.abs),
        Err(error) => Err(error),
    };
    match result {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(place.err(&error, "rm")),
        _ => Ok(()),
    }
}

pub fn rename(from: &Place, to: &Place) -> Result<(), String> {
    if from.scope() != Scope::Staging || to.scope() != Scope::Staging || !same_kind(from, to) {
        return Err(to.denied());
    }
    from.check_no_links()?;
    to.check_no_links()?;
    crate::vault::atomic::rename_with_retry(&from.abs, &to.abs).map_err(|e| to.err(&e, "rename"))
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target)?;
            let meta = entry.metadata()?;
            set_file_times(&target, meta.accessed()?, meta.modified()?)?;
        }
    }
    Ok(())
}

/// Upstream's `moveIntoVault`: one rename, so the import appears whole. A
/// vault on another volume gets a copy under a dot name that no walker
/// descends into, renamed into place when complete. The name must be free:
/// nothing already in the vault is replaced. Returns the folder made.
pub fn move_into_vault(from: &Place, to: &Place) -> Result<PathBuf, String> {
    if from.scope() != Scope::Staging || to.scope() != Scope::Vault || !same_kind(from, to) || to.vpath.parts.is_empty() {
        return Err(to.denied());
    }
    from.check_no_links()?;
    to.check_no_links()?;
    if fs::symlink_metadata(&to.abs).is_ok() {
        let taken = io::Error::new(io::ErrorKind::AlreadyExists, "file already exists");
        return Err(to.err(&taken, "rename"));
    }
    match fs::rename(&from.abs, &to.abs) {
        Ok(()) => return Ok(to.abs.clone()),
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {}
        Err(error) => return Err(to.err(&error, "rename")),
    }
    let name = to.abs.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let partial = to.abs.with_file_name(format!(".{name}.partial"));
    let _ = fs::remove_dir_all(&partial);
    copy_tree(&from.abs, &partial).map_err(|e| to.err(&e, "cp"))?;
    fs::rename(&partial, &to.abs).map_err(|e| to.err(&e, "rename"))?;
    Ok(to.abs.clone())
}

/// Upstream's `writeNoteCreatedAt` (note-creation-metadata.ts): a created
/// date a note brought with it, kept beside it where a rename keeps it.
pub fn write_note_created_at(place: &Place, created_at: f64) -> Result<(), String> {
    if place.scope() != Scope::Vault {
        return Err(place.denied());
    }
    let rel = place.rel();
    let lower = rel.to_lowercase();
    if !(lower.ends_with(".md") || lower.ends_with(".excalidraw")) {
        return Ok(());
    }
    // Nothing a reader would accept (it wants a positive whole number).
    if !created_at.is_finite() || created_at.trunc() <= 0.0 {
        return Ok(());
    }
    place.check_no_links()?;
    let metadata = crate::vault::sidecars::note_metadata_path(&place.base, &rel, false)?;
    if let Some(parent) = metadata.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
    }
    // Byte-identical to upstream's JSON.stringify({ version, createdAt }) + '\n'.
    let body = format!("{{\"version\":1,\"createdAt\":{}}}\n", created_at.trunc() as i64);
    crate::vault::atomic::write_file_atomic(&metadata, &body)
}

#[cfg(test)]
mod tests {
    use super::super::paths::parse;
    use super::*;

    struct Places {
        _scratch: tempfile::TempDir,
        source: PathBuf,
        staging: PathBuf,
        vault: PathBuf,
    }

    fn places() -> Places {
        let scratch = tempfile::tempdir().unwrap();
        let source = scratch.path().join("source");
        let staging = scratch.path().join("staging");
        let vault = scratch.path().join("vault");
        fs::create_dir_all(source.join("notes")).unwrap();
        fs::create_dir_all(&vault).unwrap();
        fs::write(source.join("notes/a.md"), "a\n").unwrap();
        fs::write(source.join("photo.png"), [1u8, 2, 3]).unwrap();
        Places { _scratch: scratch, source, staging, vault }
    }

    impl Places {
        fn at(&self, text: &str) -> Place {
            let vpath = parse(text).unwrap();
            let base = match vpath.scope {
                Scope::Picked | Scope::Source => self.source.clone(),
                Scope::Staging => self.staging.clone(),
                Scope::Vault => self.vault.clone(),
            };
            Place::new(vpath, base)
        }
    }

    #[test]
    fn the_source_is_only_ever_read() {
        let p = places();
        let names: Vec<_> = read_dir(&p.at("/obsidian/source")).unwrap().into_iter().map(|e| (e.name, e.kind)).collect();
        assert!(names.contains(&("notes".into(), "dir")));
        assert!(names.contains(&("photo.png".into(), "file")));
        assert_eq!(read_text(&p.at("/obsidian/source/notes/a.md")).unwrap(), "a\n");
        assert!(stat(&p.at("/obsidian/source/notes/a.md")).unwrap().is_file);

        for denied in [
            write_text(&p.at("/obsidian/source/notes/a.md"), "x", false),
            write_text(&p.at("/obsidian/source/new.md"), "x", true),
            append_text(&p.at("/obsidian/source/notes/a.md"), "x", &[p.source.clone()]),
            mkdir(&p.at("/obsidian/source/new")),
            remove(&p.at("/obsidian/source/notes")),
            utimes(&p.at("/obsidian/source/notes/a.md"), 0.0, 0.0),
            rename(&p.at("/obsidian/source/notes/a.md"), &p.at("/obsidian/source/b.md")),
            copy(&p.at("/obsidian/staging/x"), &p.at("/obsidian/source/x")),
        ] {
            assert!(denied.unwrap_err().starts_with("EPERM"));
        }
        assert_eq!(fs::read_to_string(p.source.join("notes/a.md")).unwrap(), "a\n");
    }

    #[test]
    fn staging_is_the_import_s_own() {
        let p = places();
        mkdir(&p.at("/obsidian/staging/out/notes")).unwrap();
        copy(&p.at("/obsidian/source/photo.png"), &p.at("/obsidian/staging/out/photo.png")).unwrap();
        assert_eq!(fs::read(p.staging.join("out/photo.png")).unwrap(), vec![1, 2, 3]);
        write_text(&p.at("/obsidian/staging/out/notes/a.md"), "changed", false).unwrap();
        append_text(&p.at("/obsidian/staging/out/notes/a.md"), "!", &[]).unwrap();
        assert_eq!(read_text(&p.at("/obsidian/staging/out/notes/a.md")).unwrap(), "changed!");
        utimes(&p.at("/obsidian/staging/out/notes/a.md"), 1_620_284_889_000.0, 1_620_284_889_000.0).unwrap();
        assert_eq!(stat(&p.at("/obsidian/staging/out/notes/a.md")).unwrap().mtime_ms, 1_620_284_889_000.0);
        rename(&p.at("/obsidian/staging/out/notes/a.md"), &p.at("/obsidian/staging/out/notes/b.md")).unwrap();
        let err = stat(&p.at("/obsidian/staging/out/notes/a.md")).unwrap_err();
        assert!(err.starts_with("ENOENT: ") && err.ends_with("stat '/obsidian/staging/out/notes/a.md'"), "{err}");
        // Copying between two imports' places is refused.
        assert!(copy(&p.at("/logseq/source/photo.png"), &p.at("/obsidian/staging/x.png")).is_err());
        remove(&p.at("/obsidian/staging")).unwrap();
        assert!(!p.staging.exists());
        remove(&p.at("/obsidian/staging")).unwrap();
    }

    #[test]
    fn the_vault_only_gains_new_things() {
        let p = places();
        fs::write(p.vault.join("Mine.md"), "mine").unwrap();
        mkdir(&p.at("/obsidian/staging/out")).unwrap();
        write_text(&p.at("/obsidian/staging/out/Report.md"), "report", false).unwrap();

        // Never overwritten, never removed, never renamed over.
        assert!(write_text(&p.at("/obsidian/vault/Mine.md"), "x", false).unwrap_err().starts_with("EPERM"));
        assert!(write_text(&p.at("/obsidian/vault/Mine.md"), "x", true).unwrap_err().starts_with("EEXIST"));
        assert!(append_text(&p.at("/obsidian/vault/Mine.md"), "x", &[]).unwrap_err().starts_with("EPERM"));
        assert!(remove(&p.at("/obsidian/vault/Mine.md")).unwrap_err().starts_with("EPERM"));
        assert!(read_text(&p.at("/obsidian/vault/Mine.md")).unwrap_err().starts_with("EPERM"));
        let err = move_into_vault(&p.at("/obsidian/staging/out"), &p.at("/obsidian/vault/Mine.md")).unwrap_err();
        assert!(err.starts_with("EEXIST"), "{err}");
        assert!(move_into_vault(&p.at("/obsidian/staging/out"), &p.at("/obsidian/vault")).is_err());
        assert_eq!(fs::read_to_string(p.vault.join("Mine.md")).unwrap(), "mine");

        // A new file, a new folder, and the import moved in whole.
        write_text(&p.at("/obsidian/vault/New.md"), "new", true).unwrap();
        mkdir(&p.at("/obsidian/vault/inbox")).unwrap();
        let moved = move_into_vault(&p.at("/obsidian/staging/out"), &p.at("/obsidian/vault/inbox/Obsidian Import")).unwrap();
        assert_eq!(fs::read_to_string(moved.join("Report.md")).unwrap(), "report");
        assert!(!p.staging.join("out").exists());
        // Appending only inside what this run moved in.
        let place = p.at("/obsidian/vault/inbox/Obsidian Import/Report.md");
        assert!(append_text(&place, "\nmore", &[]).is_err());
        append_text(&place, "\nmore", &[moved.clone()]).unwrap();
        assert_eq!(fs::read_to_string(moved.join("Report.md")).unwrap(), "report\nmore");
        assert!(append_text(&p.at("/obsidian/vault/New.md"), "x", &[moved]).is_err());
        assert!(stat(&p.at("/obsidian/vault/inbox/Obsidian Import")).unwrap().is_directory);
    }

    #[test]
    fn created_dates_go_beside_the_note() {
        let p = places();
        fs::write(p.vault.join("a.md"), "a").unwrap();
        write_note_created_at(&p.at("/notion/vault/a.md"), 1_612_325_106_000.7).unwrap();
        let sidecar = crate::vault::sidecars::note_metadata_path(&p.vault, "a.md", false).unwrap();
        assert_eq!(fs::read_to_string(&sidecar).unwrap(), "{\"version\":1,\"createdAt\":1612325106000}\n");
        assert_eq!(crate::vault::sidecars::read_note_created_at(&p.vault, "a.md", 0), 1_612_325_106_000);
        // Not a note: nothing written.
        write_note_created_at(&p.at("/notion/vault/a.png"), 1.0).unwrap();
        assert!(crate::vault::sidecars::note_metadata_path(&p.vault, "a.png", false).map(|p| !p.exists()).unwrap());
        assert!(write_note_created_at(&p.at("/notion/staging/a.md"), 1.0).is_err());
    }
}
