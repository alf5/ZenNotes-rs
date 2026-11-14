//! A `Backend` that shows an MCP client only what its scope allows
//! (ZenNotes/tui#21, scope.rs), ported from upstream
//! apps/desktop/src/mcp/scoped-backend.ts. Every tool runs against one when a
//! scope is active, so no handler can skip the check.
//!
//! A path is resolved against the vault's own listings before the scope
//! check, never trusted as given: the listings walk real directories and
//! regular files only, so a path through a symbolic link, a case variant of a
//! hidden folder on a case-insensitive volume, or a hidden directory simply
//! has no match. Whatever the client is never shown does not exist for it,
//! and the error for a hidden path is the same bytes as for a missing one.
//!
//! One is made per tool call; the listings it reads are cached for that call.
//! Upstream's branches for a vault on a ZenNotes server (links the server
//! marks, its own link rewrites, its refusals) are not ported, since this
//! build serves local vaults only.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use serde_json::Value;

use super::assets::{
    self, asset_not_found, clean_request, deleted_asset_not_found, folders_as_listed, form_dir_containing, match_asset,
    DeletedAsset, ImportedAsset,
};
use super::backend::{Backend, LocalBackend};
use super::ops::{copy_title, extname, goes_through_link, sanitize_title, AssetMeta, FolderRef, NoteContent, NoteMeta, ReplaceResult, TextMatch};
use super::scope::{
    access_at_least, access_info, access_refusal, fold, fold_case, is_inside, is_inside_folded, is_restricted, js_trim, Access,
    Scope,
};
use super::tasks::VaultTask;
use crate::ipc::types::NoteComment;
use crate::vault::layout::{ATTACHMENTS_DIRS, FOLDERS};

/// Kept out of the root-mode note tree however the system folders are
/// mapped: the asset directories and the internal one.
const RESERVED_NON_SYSTEM_ROOT_NAMES: [&str; 4] = ["assets", "attachements", "_assets", ".zennotes"];

const NOT_OFFERED: &str = "That is not available to an assistant with scoped access.";

const STALE_DENY: &str = "This assistant's scope hides a folder or note that does not exist in this vault (renamed or deleted outside ZenNotes, or it belongs to another vault), so nothing is shared until the MCP client's --deny-path entries are fixed.";

const ASSETS_DIR: &str = "assets";

const NO_COPY_NAME: &str = "This assistant cannot give the note that name.";

const RECORD_PAGE_STAYS_PUT: &str =
    "This assistant cannot rename, move, copy, archive, trash or delete a database's record page.";

/// How many taken names the check of a new note's path goes through.
const MAX_LANDING_STEPS: usize = 10000;

fn not_found(rel: &str) -> String {
    format!("Note not found: {rel}")
}

fn folder_not_found(subpath: &str) -> String {
    format!("Folder not found: {subpath}")
}

/// The stderr hints, once per entry for the life of the process. Only stderr
/// names them, which lands in the client's logs and not in front of the model.
static STALE_DENY_LOGGED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);
static LINKED_FOLDER_LOGGED: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

fn hint_once(set: &Mutex<HashSet<String>>, key: &str, message: impl FnOnce() -> String) {
    if set.lock().map(|mut s| s.insert(key.to_string())).unwrap_or(false) {
        eprintln!("{}", message());
    }
}

fn hint_linked_folder(kind: &str, dir: &str) {
    hint_once(&LINKED_FOLDER_LOGGED, dir, || {
        format!("[zennotes-mcp] the {kind} folder {dir} is a symbolic link, so it is hidden from a client with --allow-path or --deny-path")
    });
}

/// `cafe` finds `Café`, as a loose spelling should. `Café` does not find
/// `Cafe`: someone who wrote the accent meant that folder.
fn accents_agree(entry: &str, canonical: &str) -> bool {
    fold_case(entry) == fold_case(canonical) || fold_case(entry) == fold(entry)
}

fn join_rel(dir: &str, sub: &str) -> String {
    match (dir.is_empty(), sub.is_empty()) {
        (true, _) => sub.to_string(),
        (_, true) => dir.to_string(),
        _ => format!("{dir}/{sub}"),
    }
}

/// `path.posix.dirname`.
fn dirname(p: &str) -> &str {
    match p.rfind('/') {
        Some(0) => "/",
        Some(i) => &p[..i],
        None => ".",
    }
}

/// Every folder above a path, nearest first.
fn ancestors_of(p: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = dirname(p);
    while at != "." && at != "/" {
        out.push(at.to_string());
        at = dirname(at);
    }
    out
}

/// The vault's spelling of `p` among known paths: exact, else the one path
/// equal to it after case and Unicode form, else the one equal after accent
/// folding too.
pub fn spell(p: &str, known: &[String]) -> Option<String> {
    let cased = fold_case(p);
    let folded = fold(p);
    let mut near: Vec<&String> = Vec::new();
    let mut loose: Vec<&String> = Vec::new();
    for k in known {
        if k == p {
            return Some(k.clone());
        }
        if fold_case(k) == cased && !near.contains(&k) {
            near.push(k);
        }
        if fold(k) == folded && !loose.contains(&k) {
            loose.push(k);
        }
    }
    let pick = if near.is_empty() { loose } else { near };
    (pick.len() == 1).then(|| pick[0].clone())
}

/// A path with whitespace around it is not one the listings hold.
fn trims_differently(raw: &str) -> bool {
    js_trim(raw) != raw
}

/// The system folder that owns a top-level directory name (case-insensitive).
fn system_folder_for_dir_name(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    FOLDERS.iter().copied().find(|f| *f == lower)
}

/// Whether a top-level name a client typed is an asset, internal or system
/// directory, in any case.
fn is_reserved_root_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    RESERVED_NON_SYSTEM_ROOT_NAMES.iter().any(|r| *r == lower) || system_folder_for_dir_name(name).is_some()
}

/// The bucket a vault-relative path classifies into by its first segment.
fn folder_for_relative_path(rel: &str) -> Option<&'static str> {
    let top = rel.split('/').next().unwrap_or("");
    if top.is_empty() || top.starts_with('.') {
        return None;
    }
    if let Some(system) = system_folder_for_dir_name(top) {
        return Some(system);
    }
    let hidden = RESERVED_NON_SYSTEM_ROOT_NAMES.iter().chain(["quick", "archive", "trash"].iter());
    if hidden.into_iter().any(|h| *h == top) {
        return None;
    }
    Some("inbox")
}

/// A note's directory relative to its bucket root, '' at the root.
fn folder_subpath_of(rel: &str, folder: &str, primary_at_root: bool) -> String {
    let dir = dirname(rel);
    if dir == "." {
        return String::new();
    }
    if folder == "inbox" && primary_at_root {
        return dir.to_string();
    }
    let top = folder;
    if dir.to_lowercase() == top.to_lowercase() {
        return String::new();
    }
    match (dir.get(..top.len()), dir.as_bytes().get(top.len())) {
        (Some(head), Some(b'/')) if dir.len() > top.len() + 1 && head.to_lowercase() == top.to_lowercase() => {
            dir[top.len() + 1..].to_string()
        }
        _ => String::new(),
    }
}

/// The vault as one call sees it.
pub struct ScopeView {
    root: std::path::PathBuf,
    primary_at_root: bool,
    /// Every listed folder, visible or not.
    all_folders: Vec<FolderRef>,
    /// The visible notes.
    notes: Vec<NoteMeta>,
    /// Every listed note, visible or not.
    by_path: HashMap<String, NoteMeta>,
    note_paths: Vec<String>,
    /// Every listed folder by its vault-relative directory.
    folders: HashMap<String, FolderRef>,
    /// The folder paths plus the bucket directories, for spelling lookups.
    dirs: Vec<String>,
    allow: Vec<String>,
    deny: Vec<String>,
    missing_allow: Vec<String>,
    ancestors: HashSet<String>,
    /// The system folders that are links.
    linked_folders: Vec<String>,
}

impl ScopeView {
    /// Whether anything at all sits at a vault-relative path. A listing
    /// cannot answer that, because it leaves links out.
    fn occupied(&self, rel: &str) -> bool {
        match fs::symlink_metadata(self.root.join(rel)) {
            Ok(_) => true,
            Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        }
    }

    /// Whether every segment of `rel` that exists is a real folder under the
    /// name its parent lists, as a destination must be.
    fn real_dirs(&self, rel: &str) -> bool {
        if rel.is_empty() || rel == "." {
            return true;
        }
        let mut walk = self.root.clone();
        for seg in rel.split('/') {
            let parent = walk.clone();
            walk.push(seg);
            match fs::symlink_metadata(&walk) {
                Ok(info) if !info.is_dir() => return false,
                Ok(_) => {}
                Err(e) => return e.kind() == std::io::ErrorKind::NotFound,
            }
            let folded = fold(seg);
            let listed = match fs::read_dir(&parent) {
                Ok(read) => read.flatten().any(|e| fold(&e.file_name().to_string_lossy()) == folded),
                Err(e) => return e.kind() == std::io::ErrorKind::NotFound,
            };
            if !listed {
                return false;
            }
        }
        true
    }

    /// The one answer for every folder a client may not put a file in.
    fn asset_refusal(&self) -> String {
        if self.allow.is_empty() {
            "This assistant cannot put files there.".into()
        } else {
            format!("This assistant can put files only inside: {}.", self.allow.join(", "))
        }
    }

    fn bucket_dir(&self, folder: &str) -> String {
        if folder == "inbox" && self.primary_at_root { String::new() } else { folder.to_string() }
    }

    fn folder_rel(&self, entry: &FolderRef) -> String {
        join_rel(&self.bucket_dir(&entry.folder), &entry.subpath)
    }

    fn covers(&self, p: &str) -> bool {
        self.allow.is_empty() || self.allow.iter().any(|entry| is_inside(p, entry))
    }

    /// A path without the system folder it sits in, the same path when it
    /// sits in none and '' when it names one.
    fn without_system_folder(&self, p: &str, fold_with: fn(&str) -> String) -> String {
        let folded = fold_with(p);
        for folder in FOLDERS {
            let dir = self.bucket_dir(folder);
            if !dir.is_empty() && is_inside(&folded, &fold_with(&dir)) {
                return p.split('/').skip(dir.split('/').count()).collect::<Vec<_>>().join("/");
            }
        }
        p.to_string()
    }

    /// A note keeps its folders when it is archived or trashed, so what a deny
    /// entry hides stays hidden there.
    fn denied(&self, p: &str) -> bool {
        let in_system_folder = self.without_system_folder(p, fold_case);
        self.deny.iter().any(|entry| {
            if is_inside_folded(p, entry) {
                return true;
            }
            let within = self.without_system_folder(entry, fold);
            !within.is_empty() && in_system_folder != p && is_inside_folded(&in_system_folder, &within)
        })
    }

    pub fn visible(&self, p: &str) -> bool {
        self.covers(p) && !self.denied(p)
    }

    /// Whether a deny entry lies at or below `p`.
    fn deny_inside(&self, p: &str) -> bool {
        let in_system_folder = self.without_system_folder(p, fold_case);
        self.deny.iter().any(|entry| {
            if is_inside_folded(entry, p) {
                return true;
            }
            !in_system_folder.is_empty()
                && in_system_folder != p
                && is_inside_folded(&self.without_system_folder(entry, fold), &in_system_folder)
        })
    }

    /// A client's note path resolved to a visible listed note.
    fn note(&self, raw: &str) -> Option<&NoteMeta> {
        let clean = clean_request(raw)?;
        let spelled = if self.by_path.contains_key(&clean) { Some(clean) } else { spell(&clean, &self.note_paths) };
        let meta = self.by_path.get(&spelled?)?;
        self.visible(&meta.path).then_some(meta)
    }

    /// The one answer for every destination a client may not use, hidden or
    /// not, so it confirms nothing about hidden folders.
    fn create_refusal(&self) -> String {
        if self.allow.is_empty() {
            "This assistant cannot put notes there.".into()
        } else {
            format!("This assistant can create and move notes only inside: {}.", self.allow.join(", "))
        }
    }

    /// Where a note or folder would go, as the subpath under its bucket in the
    /// vault's spelling, or a refusal. The directory must sit in the bucket
    /// asked for and inside an allow entry, not merely above one.
    fn destination(&self, folder: &str, raw_sub: &str) -> Result<String, String> {
        let replaced = raw_sub.replace('\\', "/");
        let mut sub = replaced.trim_matches('/').to_string();
        if !sub.is_empty() {
            sub = clean_request(&sub).ok_or_else(|| self.create_refusal())?;
        }
        // A database folder's insides are its record pages, not notes.
        if sub.split('/').any(|seg| seg.to_lowercase().ends_with(".base")) {
            return Err(self.create_refusal());
        }
        let bucket = self.bucket_dir(folder);
        if bucket.is_empty() && !sub.is_empty() && is_reserved_root_name(sub.split('/').next().unwrap_or("")) {
            return Err(self.create_refusal());
        }
        let rel = join_rel(&bucket, &sub);
        if !rel.is_empty() && folder_for_relative_path(&format!("{rel}/x.md")) != Some(folder) {
            return Err(self.create_refusal());
        }
        // Existing folders keep their spelling; new segments are created as given.
        let mut spelled = bucket.clone();
        let mut first_new: Option<String> = None;
        if !sub.is_empty() {
            for seg in sub.split('/') {
                let next = join_rel(&spelled, seg);
                let listed = spell(&next, &self.dirs);
                if listed.is_none() && first_new.is_none() {
                    first_new = Some(next.clone());
                }
                spelled = listed.unwrap_or(next);
            }
        }
        if !self.visible(&spelled) {
            return Err(self.create_refusal());
        }
        // A segment the listings do not know is new only when nothing is
        // there: the walks skip symbolic links.
        if first_new.is_some_and(|n| self.occupied(&n)) {
            return Err(self.create_refusal());
        }
        if bucket.is_empty() {
            return Ok(spelled);
        }
        let rest = &spelled[bucket.len()..];
        Ok(rest.strip_prefix('/').unwrap_or(rest).to_string())
    }

    /// Refuses a name that lands on a path a deny entry hides. The search for
    /// a free name walks what is really there, as the write will.
    fn refuse_hidden_landing(&self, dir: &str, title: &str, ext: &str) -> Result<(), String> {
        if self.deny.is_empty() {
            return Ok(());
        }
        let mut candidate = title.to_string();
        for n in 2..=MAX_LANDING_STEPS {
            let rel = join_rel(dir, &format!("{candidate}{ext}"));
            if !self.occupied(&rel) {
                return if self.visible(&rel) { Ok(()) } else { Err(self.create_refusal()) };
            }
            candidate = format!("{title} {n}");
        }
        // No name was checked, so none can be vouched for.
        Err(self.create_refusal())
    }

    fn is_linked_folder(&self, dir: &str) -> bool {
        self.linked_folders.iter().any(|entry| is_inside_folded(dir, entry))
    }

    /// A client's (bucket, subpath) resolved to a visible listed folder.
    fn folder(&self, folder: &str, raw_sub: &str) -> Option<(String, FolderRef)> {
        let clean = clean_request(raw_sub)?;
        let rel = spell(&join_rel(&self.bucket_dir(folder), &clean), &self.dirs)?;
        let entry = self.folders.get(&rel)?;
        (entry.folder == folder && self.visible(&rel)).then(|| (rel.clone(), entry.clone()))
    }
}

pub struct ScopedBackend<'a> {
    pub inner: &'a LocalBackend,
    pub scope: &'a Scope,
    view: OnceCell<Result<ScopeView, String>>,
    assets: OnceCell<Result<Vec<AssetMeta>, String>>,
}

impl<'a> ScopedBackend<'a> {
    pub fn new(inner: &'a LocalBackend, scope: &'a Scope) -> Self {
        Self { inner, scope, view: OnceCell::new(), assets: OnceCell::new() }
    }

    fn load(&self) -> Result<&ScopeView, String> {
        self.view.get_or_init(|| self.build()).as_ref().map_err(Clone::clone)
    }

    /// The system folders that are, or sit under, a link.
    fn linked_system_folders(&self) -> Vec<String> {
        let primary_at_root = self.inner.primary_at_root();
        FOLDERS
            .iter()
            .filter(|f| !(**f == "inbox" && primary_at_root))
            .filter(|f| goes_through_link(&self.inner.root, f))
            .map(|f| f.to_string())
            .collect()
    }

    fn build(&self) -> Result<ScopeView, String> {
        let restricted = is_restricted(self.scope);
        let linked_folders = if restricted { self.linked_system_folders() } else { Vec::new() };
        let notes = self.inner.list_notes()?;
        let all_folders = self.inner.list_folders()?;
        // A database folder is not a folder to a client with a path scope.
        let folders: Vec<FolderRef> = if restricted {
            all_folders.iter().filter(|e| form_dir_containing(&e.subpath).is_none()).cloned().collect()
        } else {
            all_folders.clone()
        };
        let mut v = ScopeView {
            root: super::ops::abs_root(&self.inner.root),
            primary_at_root: self.inner.primary_at_root(),
            all_folders: folders.clone(),
            notes: Vec::new(),
            by_path: HashMap::new(),
            note_paths: Vec::new(),
            folders: HashMap::new(),
            dirs: Vec::new(),
            allow: Vec::new(),
            deny: Vec::new(),
            missing_allow: Vec::new(),
            ancestors: HashSet::new(),
            linked_folders: Vec::new(),
        };
        for folder in FOLDERS {
            let dir = v.bucket_dir(folder);
            if !dir.is_empty() {
                v.dirs.push(dir);
            }
        }
        for entry in &folders {
            let rel = v.folder_rel(entry);
            v.folders.insert(rel.clone(), entry.clone());
            v.dirs.push(rel);
        }
        let mut known: Vec<String> = Vec::new();
        for note in &notes {
            v.by_path.insert(note.path.clone(), note.clone());
            v.note_paths.push(note.path.clone());
            known.push(note.path.clone());
        }
        known.extend(v.dirs.iter().cloned());
        // A database folder is there to name, even with no record page yet.
        let database_dirs: Vec<FolderRef> = all_folders
            .iter()
            .filter(|e| !folders.contains(e))
            .cloned()
            .chain(super::ops::list_database_dirs(&self.inner.root))
            .collect();
        for entry in &database_dirs {
            known.push(v.folder_rel(entry));
        }

        // An entry can name an attachment folder or an asset, which the
        // listings above do not show, so those are read only when needed.
        let asset_names: OnceCell<Vec<String>> = OnceCell::new();
        let known_assets = || -> Result<&Vec<String>, String> {
            if let Some(names) = asset_names.get() {
                return Ok(names);
            }
            let mut names: Vec<String> = ATTACHMENTS_DIRS.iter().map(|d| d.to_string()).collect();
            for asset in self.inner.list_assets()? {
                let ancestors = ancestors_of(&asset.path);
                names.push(asset.path);
                names.extend(ancestors);
            }
            Ok(asset_names.get_or_init(|| names))
        };

        // Entries take the vault's own spelling, so `--allow-path /projects`
        // finds `Projects/`.
        for entry in &self.scope.allow {
            let mut canonical = match spell(entry, &known) {
                Some(found) => Some(found),
                None => spell(entry, known_assets()?),
            };
            if canonical.as_deref().is_none_or(|c| !accents_agree(entry, c)) {
                v.missing_allow.push(entry.clone());
                canonical = Some(entry.clone());
            }
            let canonical = canonical.unwrap_or_default();
            for ancestor in ancestors_of(&canonical) {
                v.ancestors.insert(ancestor);
            }
            v.allow.push(canonical);
        }
        // A deny entry that matches nothing was renamed or deleted outside
        // ZenNotes, or belongs to another vault: what it hid may now sit under
        // a new name, so the scope shares nothing until it is fixed.
        for entry in &self.scope.deny {
            let folded = fold(entry);
            let named = |p: &String| fold(p) == folded;
            if !known.iter().any(named) && !known_assets()?.iter().any(named) {
                hint_once(&STALE_DENY_LOGGED, entry, || {
                    format!("[zennotes-mcp] --deny-path {entry} matches nothing in this vault, so the MCP server shares nothing until it is fixed")
                });
                return Err(STALE_DENY.into());
            }
            v.deny.push(entry.clone());
        }
        // A path scope cannot say what is behind a linked system folder, so
        // nothing is, whatever the allow entries say.
        for dir in linked_folders {
            hint_linked_folder("system", &dir);
            v.linked_folders.push(dir.clone());
            v.deny.push(dir);
        }
        // A local asset walk goes through a linked attachment folder too.
        if restricted {
            for dir in ATTACHMENTS_DIRS {
                if goes_through_link(&self.inner.root, dir) {
                    hint_linked_folder("attachment", dir);
                    v.deny.push(dir.to_string());
                }
            }
        }
        v.notes = notes.into_iter().filter(|n| v.visible(&n.path)).collect();
        Ok(v)
    }

    fn need(&self, access: Access) -> Result<(), String> {
        if access_at_least(self.scope.access, access) { Ok(()) } else { Err(access_refusal(self.scope)) }
    }

    /// A client with a path scope may not see the database a page belongs
    /// to, so it reads and edits a page and leaves it where it is.
    fn keep_record_page(&self, meta: &NoteMeta) -> Result<(), String> {
        if is_restricted(self.scope) && form_dir_containing(&meta.path).is_some() {
            return Err(RECORD_PAGE_STAYS_PUT.into());
        }
        Ok(())
    }

    /// Loads the view and resolves a note: the first step of every per-note
    /// operation.
    fn note_for(&self, rel: &str, access: Access) -> Result<NoteMeta, String> {
        self.need(access)?;
        self.load()?.note(rel).cloned().ok_or_else(|| not_found(rel))
    }

    /// The asset listing this call sees, read once. An asset is visible when
    /// its path is.
    fn visible_assets(&self) -> Result<&Vec<AssetMeta>, String> {
        self.assets
            .get_or_init(|| {
                let v = self.load()?;
                Ok(self.inner.list_assets()?.into_iter().filter(|a| v.visible(&a.path)).collect())
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Resolves a client's path to a visible listed asset. A hidden file
    /// answers exactly like a missing one.
    fn asset_for(&self, raw: &str, access: Access) -> Result<AssetMeta, String> {
        self.need(access)?;
        self.load()?;
        let all = self.visible_assets()?;
        let found = if trims_differently(raw) { None } else { match_asset(all, raw) };
        found.cloned().ok_or_else(|| asset_not_found(raw))
    }

    /// The folders the listings establish, plus the attachment folders, which
    /// listings hide.
    fn known_asset_dirs(&self, v: &ScopeView) -> Vec<String> {
        let mut known = v.dirs.clone();
        known.extend(ATTACHMENTS_DIRS.iter().map(|d| d.to_string()));
        if let Ok(assets) = self.visible_assets() {
            for asset in assets {
                known.extend(ancestors_of(&asset.path));
            }
        }
        known
    }

    /// A folder a file may be moved to, in the vault's spelling, or a refusal.
    fn asset_destination(&self, v: &ScopeView, raw: &str) -> Result<String, String> {
        let clean = match clean_request(raw) {
            Some(c) if !trims_differently(raw) => c,
            _ => return Err(v.asset_refusal()),
        };
        let known = self.known_asset_dirs(v);
        let mut spelled = String::new();
        for seg in clean.split('/') {
            let next = join_rel(&spelled, seg);
            spelled = spell(&next, &known).unwrap_or(next);
        }
        if !v.visible(&spelled) || !v.real_dirs(&spelled) {
            return Err(v.asset_refusal());
        }
        match folders_as_listed(&self.inner.root, &spelled) {
            Some(listed) if v.visible(&listed) => Ok(listed),
            _ => Err(v.asset_refusal()),
        }
    }

    /// Whether this client sees a path at all, for a tool about to name one it
    /// did not get from a listing. If that cannot be told, it does not.
    pub fn shares_path(&self, rel: &str) -> bool {
        match self.load() {
            Ok(v) if rel == "." || rel.is_empty() => v.allow.is_empty(),
            Ok(v) => v.visible(rel),
            Err(_) => false,
        }
    }

    /// The answer for a folder this client may not put a file in.
    pub fn destination_refusal(&self) -> String {
        match self.load() {
            Ok(v) => v.asset_refusal(),
            Err(e) => e,
        }
    }

    /// The scope as vault_info reports it, with entries in the vault's
    /// spelling, or why it shares nothing.
    pub fn access_info(&self) -> Value {
        match self.load() {
            Ok(v) => access_info(self.scope, &v.allow, &v.missing_allow, None),
            Err(e) => access_info(self.scope, &self.scope.allow, &[], Some(&e)),
        }
    }

    /// A lifecycle move lands under the system folder at the note's own
    /// subpath, so a link on that way takes it elsewhere.
    fn refuse_linked_exit(&self, meta: &NoteMeta, target: &str) -> Result<(), String> {
        let v = self.load()?;
        let bucket = v.bucket_dir(target);
        if v.is_linked_folder(&bucket) {
            return Err(v.create_refusal());
        }
        let sub = folder_subpath_of(&meta.path, &meta.folder, v.primary_at_root);
        if goes_through_link(&self.inner.root, &join_rel(&bucket, &sub)) {
            return Err(v.create_refusal());
        }
        Ok(())
    }

    /// A note brought back to the inbox must land where the client can see
    /// it: the same subpath under the inbox bucket, in the note's own
    /// spelling.
    fn restore_target(&self, rel: &str) -> Result<NoteMeta, String> {
        let meta = self.note_for(rel, Access::Write)?;
        let v = self.load()?;
        let sub = folder_subpath_of(&meta.path, &meta.folder, v.primary_at_root);
        if v.destination("inbox", &sub)? != sub {
            return Err(v.create_refusal());
        }
        Ok(meta)
    }

    /// A folder a client may rename or delete as a whole: visible, and
    /// holding nothing hidden.
    fn whole_folder(&self, folder: &str, subpath: &str, verb: &str) -> Result<(&ScopeView, String), String> {
        self.need(Access::Write)?;
        let v = self.load()?;
        let (rel, entry) = v.folder(folder, subpath).ok_or_else(|| folder_not_found(subpath))?;
        if v.deny_inside(&rel) {
            return Err(format!("This assistant cannot {verb} that folder."));
        }
        Ok((v, entry.subpath))
    }
}

impl Backend for ScopedBackend<'_> {
    fn root(&self) -> &Path {
        &self.inner.root
    }
    fn scoped(&self) -> Option<&ScopedBackend<'_>> {
        Some(self)
    }
    fn primary_at_root(&self) -> bool {
        self.inner.primary_at_root()
    }
    fn template_folder(&self) -> Option<String> {
        self.inner.template_folder()
    }
    fn vault_settings(&self) -> Value {
        self.inner.vault_settings()
    }

    fn list_notes(&self) -> Result<Vec<NoteMeta>, String> {
        Ok(self.load()?.notes.clone())
    }

    /// The visible folders plus the folders above an allow entry, which are
    /// listed by name so the tree makes sense, while nothing directly inside
    /// them is visible.
    fn list_folders(&self) -> Result<Vec<FolderRef>, String> {
        let v = self.load()?;
        Ok(v.all_folders
            .iter()
            .filter(|e| {
                let rel = v.folder_rel(e);
                v.visible(&rel) || (v.ancestors.contains(&rel) && !v.denied(&rel))
            })
            .cloned()
            .collect())
    }

    fn list_assets(&self) -> Result<Vec<AssetMeta>, String> {
        Ok(self.visible_assets()?.clone())
    }

    fn read_asset(&self, rel: &str) -> Result<Vec<u8>, String> {
        let meta = self.asset_for(rel, Access::Read)?;
        self.inner.read_asset(&meta.path)
    }

    /// Only ever writes to the vault's assets folder, so that folder has to
    /// be shared. A name that lands on a deny entry is undone rather than
    /// left where the client cannot see it.
    fn import_asset(&self, filename: &str, bytes: &[u8]) -> Result<ImportedAsset, String> {
        self.need(Access::Write)?;
        let v = self.load()?;
        let refused = "This assistant cannot add files: the vault's assets folder is not shared with it.".to_string();
        if !v.visible(ASSETS_DIR) || !v.real_dirs(ASSETS_DIR) {
            return Err(refused);
        }
        let got = self.inner.import_asset(filename, bytes)?;
        if !v.visible(&got.path) {
            let _ = self.inner.delete_asset(&got.path);
            return Err(refused);
        }
        Ok(got)
    }

    /// Links are rewritten in visible notes only.
    fn rename_asset(&self, rel: &str, name: &str) -> Result<AssetMeta, String> {
        let meta = self.asset_for(rel, Access::Write)?;
        let v = self.load()?;
        let dir = dirname(&meta.path);
        if !v.visible(&join_rel(if dir == "." { "" } else { dir }, name)) {
            return Err("This assistant cannot give the file that name.".into());
        }
        if !is_restricted(self.scope) {
            return self.inner.rename_asset(&meta.path, name);
        }
        assets::rename_asset(&self.inner.root, &meta.path, name, Some(&|p: &str| v.visible(p)))
    }

    fn move_asset(&self, rel: &str, target_dir: &str) -> Result<AssetMeta, String> {
        let meta = self.asset_for(rel, Access::Write)?;
        let v = self.load()?;
        let dir = self.asset_destination(v, target_dir)?;
        if !is_restricted(self.scope) {
            return self.inner.move_asset(&meta.path, &dir);
        }
        assets::move_asset(&self.inner.root, &meta.path, &dir, Some(&|p: &str| v.visible(p))).map_err(|e| {
            if assets::is_destination_refused(&e) { v.asset_refusal() } else { e }
        })
    }

    fn duplicate_asset(&self, rel: &str) -> Result<AssetMeta, String> {
        let meta = self.asset_for(rel, Access::Write)?;
        let v = self.load()?;
        let ext = extname(&meta.name);
        let stem = &meta.name[..meta.name.len() - ext.len()];
        let dir = dirname(&meta.path);
        let dir = if dir == "." { "" } else { dir };
        if !v.visible(&join_rel(dir, &format!("{stem} copy{ext}"))) {
            return Err("This assistant cannot give the file that name.".into());
        }
        let copy = self.inner.duplicate_asset(&meta.path)?;
        // A numbered name can land where the first name would not.
        if !v.visible(&copy.path) {
            let _ = self.inner.delete_asset(&copy.path);
            return Err("This assistant cannot give the file that name.".into());
        }
        Ok(copy)
    }

    fn delete_asset(&self, rel: &str) -> Result<DeletedAsset, String> {
        let meta = self.asset_for(rel, Access::Write)?;
        self.inner.delete_asset(&meta.path)
    }

    /// What was deleted from where the client can see. The store itself is
    /// internal; only its entries' original paths decide.
    fn list_deleted_assets(&self) -> Result<Vec<DeletedAsset>, String> {
        let v = self.load()?;
        Ok(self
            .inner
            .list_deleted_assets()?
            .into_iter()
            .filter(|e| !trims_differently(&e.path) && clean_request(&e.path).is_some_and(|c| v.visible(&c)))
            .collect())
    }

    /// Takes only an entry this client can list, and the folder it returns to
    /// must still be a real one.
    fn restore_deleted_asset(&self, deleted: &DeletedAsset) -> Result<AssetMeta, String> {
        self.need(Access::Write)?;
        let v = self.load()?;
        let entry = self.list_deleted_assets()?.into_iter().find(|d| d.undo_token == deleted.undo_token);
        let Some(entry) = entry else { return Err(deleted_asset_not_found(&deleted.undo_token)) };
        let clean = clean_request(&entry.path).unwrap_or_default();
        if !v.real_dirs(dirname(&clean)) {
            return Err(deleted_asset_not_found(&deleted.undo_token));
        }
        let restored = self.inner.restore_deleted_asset(&entry)?;
        // As with a copy: a number that landed out of view is undone.
        if !v.visible(&restored.path) {
            let _ = self.inner.delete_asset(&restored.path);
            return Err(deleted_asset_not_found(&deleted.undo_token));
        }
        Ok(restored)
    }

    /// A template can hold anything, so a client that sees only part of the
    /// vault gets none.
    fn list_templates(&self) -> Result<Vec<(String, String)>, String> {
        if is_restricted(self.scope) {
            return Err(NOT_OFFERED.into());
        }
        self.inner.list_templates()
    }

    fn read_note(&self, rel: &str) -> Result<NoteContent, String> {
        let meta = self.note_for(rel, Access::Read)?;
        self.inner.read_note(&meta.path)
    }

    fn write_note(&self, rel: &str, body: &str) -> Result<NoteMeta, String> {
        let meta = self.note_for(rel, Access::Write)?;
        self.inner.write_note(&meta.path, body)
    }

    fn create_note(&self, folder: &str, title: Option<&str>, subpath: &str, body: Option<&str>) -> Result<NoteMeta, String> {
        self.need(Access::Write)?;
        let v = self.load()?;
        let destination = v.destination(folder, subpath)?;
        v.refuse_hidden_landing(&join_rel(&v.bucket_dir(folder), &destination), &sanitize_title(title.unwrap_or("Untitled")), ".md")?;
        self.inner.create_note(folder, title, &destination, body)
    }

    fn append_to_note(&self, rel: &str, text: &str) -> Result<NoteMeta, String> {
        let meta = self.note_for(rel, Access::Write)?;
        self.inner.append_to_note(&meta.path, text)
    }

    fn prepend_to_note(&self, rel: &str, text: &str) -> Result<NoteMeta, String> {
        let meta = self.note_for(rel, Access::Write)?;
        self.inner.prepend_to_note(&meta.path, text)
    }

    fn insert_at_line(&self, rel: &str, line: f64, text: &str) -> Result<NoteMeta, String> {
        let meta = self.note_for(rel, Access::Write)?;
        self.inner.insert_at_line(&meta.path, line, text)
    }

    fn replace_in_note(&self, rel: &str, find: &str, replace: &str, all: bool) -> Result<ReplaceResult, String> {
        let meta = self.note_for(rel, Access::Write)?;
        self.inner.replace_in_note(&meta.path, find, replace, all)
    }

    /// The new name must be one the client can see. This engine's rename
    /// does not rewrite inbound links at all, so it never writes into a note
    /// the client cannot see.
    fn rename_note(&self, rel: &str, title: &str) -> Result<NoteMeta, String> {
        let meta = self.note_for(rel, Access::Write)?;
        self.keep_record_page(&meta)?;
        let v = self.load()?;
        let dir = dirname(&meta.path);
        let next = join_rel(if dir == "." { "" } else { dir }, &format!("{}.md", sanitize_title(title)));
        if !v.visible(&next) {
            return Err("This assistant cannot give the note that name.".into());
        }
        self.inner.rename_note(&meta.path, title)
    }

    fn move_note(&self, rel: &str, folder: &str, subpath: &str) -> Result<NoteMeta, String> {
        let meta = self.note_for(rel, Access::Write)?;
        self.keep_record_page(&meta)?;
        let v = self.load()?;
        let destination = v.destination(folder, subpath)?;
        let dir = join_rel(&v.bucket_dir(folder), &destination);
        let from = dirname(&meta.path);
        // A note moved to where it already is stays put, under its own name.
        if dir != if from == "." { "" } else { from } {
            let ext = extname(&meta.path);
            let name = meta.path.rsplit('/').next().unwrap_or(&meta.path);
            v.refuse_hidden_landing(&dir, &name[..name.len() - ext.len()], ext)?;
        }
        self.inner.move_note(&meta.path, folder, &destination)
    }

    fn duplicate_note(&self, rel: &str) -> Result<NoteMeta, String> {
        let meta = self.note_for(rel, Access::Write)?;
        self.keep_record_page(&meta)?;
        let v = self.load()?;
        let ext = extname(&meta.path);
        let dir = dirname(&meta.path);
        let base = meta.path.rsplit('/').next().unwrap_or(&meta.path);
        let name = format!("{}{ext}", copy_title(&base[..base.len() - ext.len()]));
        if !v.visible(&join_rel(if dir == "." { "" } else { dir }, &name)) {
            return Err(NO_COPY_NAME.into());
        }
        let copy = self.inner.duplicate_note(&meta.path)?;
        // A numbered name can land where the first name would not.
        if !v.visible(&copy.path) {
            let _ = self.inner.delete_note(&copy.path);
            return Err(NO_COPY_NAME.into());
        }
        Ok(copy)
    }

    // Archiving and trashing are lifecycle exits: the note may leave what the
    // client sees, and the user can undo either in the app.
    fn move_to_trash(&self, rel: &str) -> Result<NoteMeta, String> {
        let meta = self.note_for(rel, Access::Write)?;
        self.keep_record_page(&meta)?;
        self.refuse_linked_exit(&meta, "trash")?;
        self.inner.move_to_trash(&meta.path)
    }

    fn restore_from_trash(&self, rel: &str) -> Result<NoteMeta, String> {
        let meta = self.restore_target(rel)?;
        self.inner.restore_from_trash(&meta.path)
    }

    fn archive_note(&self, rel: &str) -> Result<NoteMeta, String> {
        let meta = self.note_for(rel, Access::Write)?;
        self.keep_record_page(&meta)?;
        self.refuse_linked_exit(&meta, "archive")?;
        self.inner.archive_note(&meta.path)
    }

    fn unarchive_note(&self, rel: &str) -> Result<NoteMeta, String> {
        let meta = self.restore_target(rel)?;
        self.inner.unarchive_note(&meta.path)
    }

    fn delete_note(&self, rel: &str) -> Result<(), String> {
        let meta = self.note_for(rel, Access::Write)?;
        self.keep_record_page(&meta)?;
        self.inner.delete_note(&meta.path)
    }

    fn empty_trash(&self) -> Result<(), String> {
        self.need(Access::Write)?;
        if is_restricted(self.scope) {
            return Err(NOT_OFFERED.into());
        }
        self.inner.empty_trash()
    }

    fn create_folder(&self, folder: &str, subpath: &str) -> Result<(), String> {
        self.need(Access::Write)?;
        let v = self.load()?;
        let sub = v.destination(folder, subpath)?;
        if sub.is_empty() {
            return Err("Folder name is required".into());
        }
        self.inner.create_folder(folder, &sub)
    }

    fn rename_folder(&self, folder: &str, old: &str, new: &str) -> Result<String, String> {
        let (v, old_sub) = self.whole_folder(folder, old, "rename")?;
        let new_sub = v.destination(folder, new)?;
        if new_sub.is_empty() {
            return Err("Both old and new folder paths are required".into());
        }
        self.inner.rename_folder(folder, &old_sub, &new_sub)
    }

    fn delete_folder(&self, folder: &str, subpath: &str) -> Result<(), String> {
        let (_, sub) = self.whole_folder(folder, subpath, "delete")?;
        self.inner.delete_folder(folder, &sub)
    }

    /// Filters before it limits: the inner walk stops at its limit, so
    /// filtering its first page would let hidden matches crowd out visible
    /// ones, and a short page would say how many hidden notes match.
    fn search_text(&self, query: &str, limit: f64) -> Result<Vec<TextMatch>, String> {
        let v = self.load()?;
        let cap = if limit <= 0.0 { 80.0 } else { limit };
        let mut out = Vec::new();
        for found in self.inner.search_text(query, 9007199254740991.0)? {
            if out.len() as f64 >= cap {
                break;
            }
            if v.by_path.get(&found.path).is_some_and(|n| v.visible(&n.path)) {
                out.push(found);
            }
        }
        Ok(out)
    }

    fn backlinks(&self, rel: &str) -> Result<Vec<NoteMeta>, String> {
        let meta = self.note_for(rel, Access::Read)?;
        let v = self.load()?;
        Ok(self.inner.backlinks(&meta.path)?.into_iter().filter(|n| v.by_path.contains_key(&n.path) && v.visible(&n.path)).collect())
    }

    fn scan_all_tasks(&self, include_excluded: bool) -> Result<Vec<VaultTask>, String> {
        let v = self.load()?;
        Ok(self
            .inner
            .scan_all_tasks(include_excluded)?
            .into_iter()
            .filter(|t| v.by_path.contains_key(&t.source_path) && v.visible(&t.source_path))
            .collect())
    }

    fn scan_tasks_for_path(&self, rel: &str, include_excluded: bool) -> Result<Vec<VaultTask>, String> {
        let meta = self.note_for(rel, Access::Read)?;
        self.inner.scan_tasks_for_path(&meta.path, include_excluded)
    }

    fn toggle_task(&self, id: &str) -> Result<Option<VaultTask>, String> {
        let hash = match id.rfind('#') {
            Some(i) if i > 0 => i,
            _ => return Err(format!("Task not found: {id}")),
        };
        let meta = self.note_for(&id[..hash], Access::Write)?;
        self.inner.toggle_task(&format!("{}{}", meta.path, &id[hash..]))
    }

    fn list_comments(&self, rel: &str) -> Result<Vec<NoteComment>, String> {
        let meta = self.note_for(rel, Access::Read)?;
        self.inner.list_comments(&meta.path)
    }

    fn write_comments(&self, rel: &str, list: &[Value]) -> Result<Vec<NoteComment>, String> {
        let meta = self.note_for(rel, Access::Comment)?;
        self.inner.write_comments(&meta.path, list)
    }
}

/// Keeps the vault's absolute location out of a scoped client's errors: an
/// MCP-only client has no use for it, and it names the user's home folder.
/// What follows the root is a vault path, written with forward slashes, up to
/// the next quote or the end of the line.
pub fn scrub_vault_root(message: &str, root: &str) -> String {
    if root.is_empty() {
        return message.to_string();
    }
    let sep = std::path::MAIN_SEPARATOR;
    let prefix = format!("{root}{sep}");
    let mut out = String::new();
    let mut rest = message;
    while let Some(at) = rest.find(&prefix) {
        out.push_str(&rest[..at]);
        rest = &rest[at + prefix.len()..];
        let end = rest.find(['\'', '"', '\n']).unwrap_or(rest.len());
        if sep == '/' {
            out.push_str(&rest[..end]);
        } else {
            out.push_str(&rest[..end].replace(sep, "/"));
        }
        rest = &rest[end..];
    }
    out.push_str(rest);
    out.replace(root, "the vault")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spelling_prefers_case_over_accents() {
        let known = vec!["Café".to_string(), "Cafe".to_string(), "Projects".to_string()];
        assert_eq!(spell("projects", &known), Some("Projects".into()));
        assert_eq!(spell("CAFE\u{301}", &known), Some("Café".into()));
        assert_eq!(spell("cafe", &known), Some("Cafe".into()));
        assert_eq!(spell("nothing", &known), None);
        assert!(accents_agree("cafe", "Café"));
        assert!(!accents_agree("Café", "Cafe"));
    }

    #[test]
    fn subpaths_and_scrubbing() {
        assert_eq!(folder_subpath_of("archive/Work/x.md", "archive", false), "Work");
        assert_eq!(folder_subpath_of("Work/x.md", "inbox", true), "Work");
        assert_eq!(folder_subpath_of("x.md", "inbox", true), "");
        assert_eq!(
            scrub_vault_root("ENOENT: no such file or directory, open '/home/u/v/Projects/a.md'", "/home/u/v"),
            "ENOENT: no such file or directory, open 'Projects/a.md'"
        );
        assert_eq!(scrub_vault_root("the vault /home/u/v is gone", "/home/u/v"), "the vault the vault is gone");
    }
}
