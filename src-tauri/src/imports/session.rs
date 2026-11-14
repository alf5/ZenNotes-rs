//! What the host remembers about each import: the folder the dialog picked
//! (upstream's module-level `source`), the run in progress (its `controller`)
//! and the last progress it reported (`lastProgress`).
//!
//! Upstream's run lives in the main process, so it outlives a window. Here it
//! runs in the webview that started it, so the run records which page that
//! is. A run whose window has closed, or whose window has since loaded a new
//! page (a reload), is over: the next status, start or stop reports it
//! stopped, which leaves a Notion import resumable as upstream's Stop does.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, LazyLock, Mutex};

use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Obsidian,
    Logseq,
    Notion,
}

impl Kind {
    pub fn parse(name: &str) -> Result<Kind, String> {
        match name {
            "obsidian" => Ok(Kind::Obsidian),
            "logseq" => Ok(Kind::Logseq),
            "notion" => Ok(Kind::Notion),
            _ => Err(format!("Unknown import: {name}")),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Obsidian => "obsidian",
            Kind::Logseq => "logseq",
            Kind::Notion => "notion",
        }
    }

    /// Upstream's staging folder under userData.
    pub fn staging_dir_name(self) -> &'static str {
        match self {
            Kind::Obsidian => "obsidian-import",
            Kind::Logseq => "logseq-import",
            Kind::Notion => "notion-import",
        }
    }

    /// Upstream's `*_IMPORT_ON_PROGRESS` channel.
    pub fn progress_event(self) -> &'static str {
        match self {
            Kind::Obsidian => "import://obsidian-progress",
            Kind::Logseq => "import://logseq-progress",
            Kind::Notion => "import://notion-progress",
        }
    }

    /// Upstream's wording when the picked folder and the vault overlap.
    pub fn overlap_message(self) -> &'static str {
        match self {
            Kind::Logseq => {
                "That folder holds this vault, or sits inside it. Choose a graph somewhere else."
            }
            _ => "That folder holds this vault, or sits inside it. Choose a vault somewhere else.",
        }
    }

    pub fn choose_first_message(self) -> &'static str {
        match self {
            Kind::Logseq => "Choose a graph first.",
            _ => "Choose a vault first.",
        }
    }
}

pub const STOP_EVENT: &str = "import://stop";

/// One import while it runs.
#[derive(Debug)]
pub struct Run {
    /// The vault it writes to, fixed for the whole run as upstream's is.
    pub root: PathBuf,
    pub staging: PathBuf,
    /// The picked folder it reads (Obsidian and Logseq).
    pub source: Option<PathBuf>,
    /// The window, and the page in it, that runs it.
    pub owner_label: String,
    pub owner_page: String,
    /// Folders this run moved into the vault: the only place it may append.
    pub moved_in: Vec<PathBuf>,
    /// Set by Stop; a download checks it between chunks.
    pub stop: Arc<AtomicBool>,
}

#[derive(Debug, Default)]
pub struct Session {
    /// The folder the last dialog picked. The webview scans it under
    /// `/<kind>/picked` before it becomes the source.
    pub picked: Option<PathBuf>,
    /// Upstream's `source`: "the vault that was picked".
    pub source: Option<PathBuf>,
    pub run: Option<Run>,
    pub progress: Option<Value>,
}

static SESSIONS: LazyLock<Mutex<HashMap<Kind, Session>>> = LazyLock::new(Default::default);

pub fn with_session<R>(kind: Kind, f: impl FnOnce(&mut Session) -> R) -> R {
    let mut sessions = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    f(sessions.entry(kind).or_default())
}

pub fn stopped_progress() -> Value {
    json!({ "phase": "stopped", "done": 0, "total": 0 })
}

/// The window and page a command came from.
pub struct Caller<'a> {
    pub label: &'a str,
    pub page: &'a str,
}

/// Ends a run whose page is gone, as if it had been stopped. Returns the
/// progress to tell every window, when it did.
pub fn reap_dead_run(
    session: &mut Session,
    caller: &Caller<'_>,
    window_exists: impl Fn(&str) -> bool,
) -> Option<Value> {
    let run = session.run.as_ref()?;
    let gone = !window_exists(&run.owner_label)
        || (caller.label == run.owner_label && caller.page != run.owner_page);
    if !gone {
        return None;
    }
    session.run = None;
    let progress = stopped_progress();
    session.progress = Some(progress.clone());
    Some(progress)
}

/// Upstream's `broadcast`, host side: keeps the progress, and a settled one
/// ends the run (`.finally(() => controller = null)`); a finished Obsidian or
/// Logseq import also forgets its folder (`source = null`). Only the page
/// that runs the import reports for it. Returns whether to tell the windows.
pub fn record_progress(kind: Kind, session: &mut Session, page: &str, progress: Value) -> bool {
    let Some(run) = session.run.as_ref() else { return false };
    if run.owner_page != page {
        return false;
    }
    let phase = progress.get("phase").and_then(Value::as_str).unwrap_or("").to_string();
    session.progress = Some(progress);
    if matches!(phase.as_str(), "done" | "stopped" | "failed") {
        session.run = None;
        if phase == "done" && kind != Kind::Notion {
            session.source = None;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(label: &str, page: &str) -> Run {
        Run {
            root: PathBuf::from("/vault"),
            staging: PathBuf::from("/staging"),
            source: None,
            owner_label: label.into(),
            owner_page: page.into(),
            moved_in: Vec::new(),
            stop: Arc::new(AtomicBool::new(false)),
        }
    }

    #[test]
    fn a_reloaded_or_closed_window_ends_its_run_as_stopped() {
        let mut session = Session { run: Some(run("main", "p1")), ..Default::default() };
        // Another window asking, while the owner is still there: still running.
        assert!(reap_dead_run(&mut session, &Caller { label: "note-1", page: "x" }, |_| true).is_none());
        // The owner's own page asking: still running.
        assert!(reap_dead_run(&mut session, &Caller { label: "main", page: "p1" }, |_| true).is_none());
        // The owner window has a new page: the run died with the old one.
        let told = reap_dead_run(&mut session, &Caller { label: "main", page: "p2" }, |_| true);
        assert_eq!(told.unwrap()["phase"], "stopped");
        assert!(session.run.is_none());
        assert_eq!(session.progress.as_ref().unwrap()["phase"], "stopped");

        let mut session = Session { run: Some(run("main", "p1")), ..Default::default() };
        assert!(reap_dead_run(&mut session, &Caller { label: "note-1", page: "x" }, |l| l != "main").is_some());
    }

    #[test]
    fn a_settled_report_ends_the_run_and_a_finished_one_forgets_the_folder() {
        let mut session = Session {
            source: Some(PathBuf::from("/picked")),
            run: Some(run("main", "p1")),
            ..Default::default()
        };
        // Someone else's page can't speak for the run.
        assert!(!record_progress(Kind::Obsidian, &mut session, "p9", json!({"phase": "writing"})));
        assert!(session.progress.is_none());
        assert!(record_progress(Kind::Obsidian, &mut session, "p1", json!({"phase": "writing", "done": 1})));
        assert!(session.run.is_some());
        assert!(record_progress(Kind::Obsidian, &mut session, "p1", json!({"phase": "done"})));
        assert!(session.run.is_none());
        assert!(session.source.is_none());
        // Nothing after the end.
        assert!(!record_progress(Kind::Obsidian, &mut session, "p1", json!({"phase": "writing"})));
        assert_eq!(session.progress.as_ref().unwrap()["phase"], "done");

        let mut session = Session {
            source: Some(PathBuf::from("/picked")),
            run: Some(run("main", "p1")),
            ..Default::default()
        };
        record_progress(Kind::Logseq, &mut session, "p1", json!({"phase": "failed", "error": "x"}));
        assert!(session.run.is_none());
        assert!(session.source.is_some(), "a failed import keeps its folder, to try again");
    }
}
