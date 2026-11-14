//! Task reminders (#1036, upstream v2.81): port of
//! apps/desktop/src/main/task-reminders.ts plus its wiring in index.ts
//! (`startTaskReminders`, `openTaskReminder`, `TASK_REMINDERS_SET`).
//!
//! The renderer owns WHAT to remind about (it has the task index and the
//! daily-note dates) and hands over the whole schedule each time; this module
//! owns WHEN, because it is the part of the app alive with the window hidden
//! in the tray. It never touches the vault: a clicked reminder hands its path
//! back to the renderer.
//!
//! The rules, as upstream's:
//!   - A reminder fires once. A ledger of fired keys survives relaunches, so a
//!     renderer re-sending the schedule or a restart never repeats one.
//!   - "Watched until" is the last moment the app was checking. Anything that
//!     came due between it and now, at most 24 hours back, fires on the next
//!     check: what the app missed while quit, asleep or busy.
//!   - A reminder set for a time already past is behind "watched until" and
//!     never fires.
//!   - Upstream arms one timer for the earliest reminder, capped at an hour,
//!     and checks again on wake and unlock. Here one thread looks at the wall
//!     clock at least every 30 s, which covers sleep and clock changes without
//!     a power-event API.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

pub const MAX_REMINDERS: usize = 500;
const CATCH_UP_MS: f64 = 24.0 * 3_600_000.0;
const LATE_MS: f64 = 2.0 * 60_000.0;
const FIRED_TTL_MS: f64 = 7.0 * 86_400_000.0;
const BOOT_GRACE_MS: f64 = 2.0 * 60_000.0;
const FOLD_OVER: usize = 3;
/// The longest the scheduler sleeps before looking at the clock again.
const LOOK_EVERY: Duration = Duration::from_secs(30);
const STATE_FILE: &str = "task-reminders.json";
const REMINDER_OPEN_EVENT: &str = "tasks://reminder-open";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskReminder {
    pub key: String,
    pub fire_at: f64,
    pub title: String,
    pub body: String,
    pub path: String,
    pub task_index: u64,
}

/// What a clicked reminder asks the app to open; None (a folded notification)
/// opens the Tasks view.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderTarget {
    pub path: String,
    pub title: String,
    pub task_index: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ReminderNotice {
    pub title: String,
    pub body: String,
    pub target: Option<ReminderTarget>,
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

fn is_safe_rel_path(path: &str) -> bool {
    if path.is_empty() || utf16_len(path) > 1024 || path.contains('\0') {
        return false;
    }
    let bytes = path.as_bytes();
    if path.starts_with('/') || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':') {
        return false;
    }
    !path.split(['\\', '/']).any(|segment| segment == "..")
}

fn bounded_string(value: Option<&Value>, max: usize) -> Option<String> {
    let text = value?.as_str()?;
    (utf16_len(text) <= max && !text.contains('\0')).then(|| text.to_string())
}

/// Port of `sanitizeReminders`: the reminders a renderer may hand over,
/// well-formed, bounded and pointing inside the vault. The rest is dropped.
pub fn sanitize_reminders(raw: &Value) -> Vec<TaskReminder> {
    let Some(items) = raw.as_array() else { return Vec::new() };
    let mut out = Vec::new();
    for item in items.iter().take(MAX_REMINDERS) {
        let Some(r) = item.as_object() else { continue };
        let (Some(key), Some(title), Some(body)) =
            (bounded_string(r.get("key"), 2048), bounded_string(r.get("title"), 500), bounded_string(r.get("body"), 500))
        else {
            continue;
        };
        let Some(fire_at) = r.get("fireAt").and_then(Value::as_f64).filter(|n| n.is_finite()) else { continue };
        let Some(path) = r.get("path").and_then(Value::as_str).filter(|p| is_safe_rel_path(p)) else { continue };
        // A JSON integer, zero or more (`Number.isInteger`, so 3.0 counts).
        let Some(task_index) = r
            .get("taskIndex")
            .and_then(Value::as_f64)
            .filter(|n| n.is_finite() && *n >= 0.0 && n.fract() == 0.0)
        else {
            continue;
        };
        out.push(TaskReminder { key, fire_at, title, body, path: path.to_string(), task_index: task_index as u64 });
    }
    out
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PersistedState<'a> {
    version: u32,
    watched_until: f64,
    fired: &'a HashMap<String, f64>,
    items: Vec<TaskReminder>,
}

struct LoadedState {
    watched_until: f64,
    fired: HashMap<String, f64>,
    items: Vec<TaskReminder>,
}

fn parse_state(text: Option<&str>) -> Option<LoadedState> {
    let data: Value = serde_json::from_str(text?).ok()?;
    if data.get("version").and_then(Value::as_f64) != Some(1.0) {
        return None;
    }
    let watched_until = data.get("watchedUntil").and_then(Value::as_f64)?;
    let mut fired = HashMap::new();
    if let Some(map) = data.get("fired").and_then(Value::as_object) {
        for (key, at) in map {
            if let Some(at) = at.as_f64().filter(|n| n.is_finite()) {
                fired.insert(key.clone(), at);
            }
        }
    }
    let items = sanitize_reminders(data.get("items").unwrap_or(&Value::Null));
    Some(LoadedState { watched_until, fired, items })
}

/// The scheduler's rules, with no clock or side effect of its own: every
/// call takes `now` and returns what to show and what to save.
pub struct Schedule {
    schedules: HashMap<String, Vec<TaskReminder>>,
    /// The schedule from the last run stands in until a renderer sends a fresh
    /// one, so a launch with no window (tray only) still fires what is due.
    persisted: Vec<TaskReminder>,
    fired: HashMap<String, f64>,
    watched_until: f64,
    started: bool,
    boot_until: Option<f64>,
}

/// What one step decided: notices to show and the state text to save.
#[derive(Default)]
pub struct Step {
    pub notices: Vec<ReminderNotice>,
    pub save: Option<String>,
}

impl Schedule {
    pub fn new(now: f64) -> Self {
        Schedule {
            schedules: HashMap::new(),
            persisted: Vec::new(),
            fired: HashMap::new(),
            watched_until: now,
            started: false,
            boot_until: None,
        }
    }

    fn merged(&self) -> Vec<TaskReminder> {
        let all: Vec<&TaskReminder> = if self.schedules.is_empty() {
            self.persisted.iter().collect()
        } else {
            self.schedules.values().flatten().collect()
        };
        let mut seen = std::collections::HashSet::new();
        let mut out: Vec<TaskReminder> = all.into_iter().filter(|r| seen.insert(r.key.clone())).cloned().collect();
        out.sort_by(|a, b| a.fire_at.partial_cmp(&b.fire_at).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    fn state_text(&self) -> String {
        let state = PersistedState { version: 1, watched_until: self.watched_until, fired: &self.fired, items: self.merged() };
        serde_json::to_string(&state).unwrap_or_default()
    }

    /// Port of `start`: load the ledger and the last schedule.
    pub fn start(&mut self, saved: Option<&str>, now: f64) -> Step {
        if let Some(state) = parse_state(saved) {
            self.watched_until = state.watched_until.min(now);
            self.fired = state.fired;
            self.persisted = state.items;
        }
        self.started = true;
        if !self.schedules.is_empty() {
            return self.check(now);
        }
        // Give the window a moment to send a fresh schedule before catching
        // up on the stale one; a fresh schedule checks at once.
        self.boot_until = Some(now + BOOT_GRACE_MS);
        Step::default()
    }

    /// Port of `setSchedule`: one window's whole schedule.
    pub fn set_schedule(&mut self, window: &str, raw: &Value, now: f64) -> Step {
        let items = sanitize_reminders(raw);
        let empty = items.is_empty();
        self.schedules.insert(window.to_string(), items);
        // An empty schedule during startup may be a window whose tasks are not
        // indexed yet; checking on it would mark the stale schedule's missed
        // reminders as watched. The boot grace catches up instead.
        if empty && self.boot_until.is_some() {
            self.schedules.remove(window);
            return Step::default();
        }
        self.boot_until = None;
        self.check(now)
    }

    /// When the scheduler next has something to do, if ever.
    pub fn next_due(&self) -> Option<f64> {
        let next = self
            .merged()
            .into_iter()
            .find(|r| r.fire_at > self.watched_until && !self.fired.contains_key(&r.key))
            .map(|r| r.fire_at);
        match (next, self.boot_until) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The clock moved: run the boot catch-up or a check if one is due.
    pub fn tick(&mut self, now: f64) -> Step {
        if let Some(until) = self.boot_until {
            if now >= until {
                self.boot_until = None;
                return self.check(now);
            }
        }
        if self.boot_until.is_none() && self.next_due().is_some_and(|at| at <= now) {
            return self.check(now);
        }
        Step::default()
    }

    /// Port of `check`: fire whatever came due.
    pub fn check(&mut self, now: f64) -> Step {
        if !self.started {
            return Step::default();
        }
        let from = self.watched_until.max(now - CATCH_UP_MS);
        let due: Vec<TaskReminder> = self
            .merged()
            .into_iter()
            .filter(|r| r.fire_at > from && r.fire_at <= now && !self.fired.contains_key(&r.key))
            .collect();
        self.watched_until = now;
        for r in &due {
            self.fired.insert(r.key.clone(), now);
        }
        self.fired.retain(|_, at| now - *at <= FIRED_TTL_MS);
        Step { notices: notices_for(&due, now), save: Some(self.state_text()) }
    }
}

/// Port of `notifyAll`: late ones fold into one, and more than three on time
/// fold into one too.
fn notices_for(due: &[TaskReminder], now: f64) -> Vec<ReminderNotice> {
    let (late, on_time): (Vec<&TaskReminder>, Vec<&TaskReminder>) = due.iter().partition(|r| now - r.fire_at > LATE_MS);
    let one = |r: &TaskReminder| ReminderNotice {
        title: r.title.clone(),
        body: r.body.clone(),
        target: Some(ReminderTarget { path: r.path.clone(), title: r.title.clone(), task_index: r.task_index }),
    };
    let folded = |list: &[&TaskReminder], title: String| {
        let names: Vec<&str> = list.iter().take(2).map(|r| r.title.as_str()).collect();
        let more = list.len() - names.len();
        let body = if more > 0 { format!("{} and {more} more", names.join(", ")) } else { names.join(", ") };
        ReminderNotice { title, body, target: None }
    };
    let mut out = Vec::new();
    match late.len() {
        0 => {}
        1 => out.push(one(late[0])),
        n => out.push(folded(&late, format!("{n} missed reminders"))),
    }
    if on_time.len() > FOLD_OVER {
        out.push(folded(&on_time, format!("{} task reminders", on_time.len())));
    } else {
        out.extend(on_time.into_iter().map(one));
    }
    out
}

fn now_ms() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as f64).unwrap_or(0.0)
}

// ---- the host: a thread, a state file, notifications ----------------------

pub struct TaskReminders {
    schedule: Mutex<Schedule>,
    wake: Condvar,
}

pub struct TaskRemindersState(pub Arc<TaskReminders>);

fn state_path(app: &AppHandle) -> Option<PathBuf> {
    app.path().app_data_dir().ok().map(|dir| dir.join(STATE_FILE))
}

fn apply(app: &AppHandle, step: Step) {
    if let (Some(text), Some(path)) = (step.save, state_path(app)) {
        if let Err(error) = crate::vault::atomic::write_file_atomic(&path, &text) {
            eprintln!("task reminders: could not save the ledger: {error}");
        }
    }
    for notice in step.notices {
        show(app, notice);
    }
}

/// Show one reminder. A click opens its task, where the platform reports
/// clicks to the app: the freedesktop notification server always does; macOS
/// and Windows are wired the same way but unverified (builds come from CI).
fn show(app: &AppHandle, notice: ReminderNotice) {
    let app = app.clone();
    std::thread::spawn(move || {
        let mut notification = notify_rust::Notification::new();
        notification.summary(&notice.title).body(&notice.body).appname("ZenNotes-rs").action("default", "Open");
        #[cfg(target_os = "windows")]
        notification.app_id(&app.config().identifier);
        #[cfg(target_os = "macos")]
        let _ = notify_rust::set_application(&app.config().identifier);
        match notification.show() {
            Ok(handle) => handle.wait_for_action(|action| {
                if action == "default" {
                    open_task_reminder(&app, notice.target.clone());
                }
            }),
            Err(error) => eprintln!("task reminders: could not show a notification: {error}"),
        }
    });
}

/// Port of `openTaskReminder`: bring the main window up and open the task (a
/// folded notification opens the Tasks view). Without a renderer ready for
/// it, the task's note opens the way a deep link would.
fn open_task_reminder(app: &AppHandle, target: Option<ReminderTarget>) {
    let app_for_main = app.clone();
    let _ = app.run_on_main_thread(move || {
        let app = app_for_main;
        let state = app.state::<crate::state::AppState>();
        if let Some(main) = app.get_webview_window("main") {
            crate::tray::forget_hidden(&app, "main");
            let _ = main.show();
            let _ = main.unminimize();
            let _ = main.set_focus();
            if state.is_renderer_ready() {
                let _ = main.emit(REMINDER_OPEN_EVENT, target);
                return;
            }
        }
        match target {
            Some(target) => state.queue_open_note(target.path),
            None => crate::tray::reveal_tray_hidden_windows(&app),
        }
    });
}

/// Port of `startTaskReminders`: load the ledger and keep the clock.
pub fn start(app: &AppHandle) -> TaskRemindersState {
    let reminders = Arc::new(TaskReminders { schedule: Mutex::new(Schedule::new(now_ms())), wake: Condvar::new() });
    let saved = state_path(app).and_then(|path| std::fs::read_to_string(path).ok());
    let step = reminders.schedule.lock().unwrap().start(saved.as_deref(), now_ms());
    apply(app, step);
    let thread_app = app.clone();
    let thread_reminders = reminders.clone();
    std::thread::spawn(move || {
        let mut schedule = thread_reminders.schedule.lock().unwrap();
        loop {
            let now = now_ms();
            let wait = schedule
                .next_due()
                .map(|at| Duration::from_millis((at - now).clamp(1000.0, LOOK_EVERY.as_millis() as f64) as u64))
                .unwrap_or(LOOK_EVERY);
            schedule = thread_reminders.wake.wait_timeout(schedule, wait).unwrap().0;
            let step = schedule.tick(now_ms());
            if step.save.is_some() || !step.notices.is_empty() {
                drop(schedule);
                apply(&thread_app, step);
                schedule = thread_reminders.schedule.lock().unwrap();
            }
        }
    });
    TaskRemindersState(reminders)
}

/// `tasks:set-reminders`: one window's whole schedule. Untrusted like any
/// renderer payload: only well-formed, vault-relative items are kept, and only
/// their text is ever shown and their path handed back.
#[tauri::command]
pub fn task_reminders_set(app: AppHandle, window: tauri::Window, state: tauri::State<TaskRemindersState>, reminders: Value) {
    let step = state.0.schedule.lock().unwrap().set_schedule(window.label(), &reminders, now_ms());
    state.0.wake.notify_all();
    apply(&app, step);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const HOUR: f64 = 3_600_000.0;
    const T0: f64 = 1_800_000_000_000.0;

    fn reminder(key: &str, fire_at: f64) -> Value {
        json!({ "key": key, "fireAt": fire_at, "title": key, "body": "due", "path": "inbox/Tasks.md", "taskIndex": 0 })
    }

    fn started(now: f64) -> Schedule {
        let mut s = Schedule::new(now);
        s.start(None, now);
        s
    }

    #[test]
    fn drops_what_a_renderer_may_not_hand_over() {
        let raw = json!([
            reminder("ok", T0),
            { "key": "abs", "fireAt": T0, "title": "t", "body": "b", "path": "/etc/passwd", "taskIndex": 0 },
            { "key": "up", "fireAt": T0, "title": "t", "body": "b", "path": "inbox/../../x.md", "taskIndex": 0 },
            { "key": "drive", "fireAt": T0, "title": "t", "body": "b", "path": "C:\\x.md", "taskIndex": 0 },
            { "key": "frac", "fireAt": T0, "title": "t", "body": "b", "path": "a.md", "taskIndex": 1.5 },
            { "key": "neg", "fireAt": T0, "title": "t", "body": "b", "path": "a.md", "taskIndex": -1 },
            { "key": "long", "fireAt": T0, "title": "x".repeat(501), "body": "b", "path": "a.md", "taskIndex": 0 },
            "not an object"
        ]);
        let kept: Vec<String> = sanitize_reminders(&raw).into_iter().map(|r| r.key).collect();
        assert_eq!(kept, vec!["ok"]);
    }

    #[test]
    fn fires_once_when_due() {
        let mut s = started(T0);
        let step = s.set_schedule("main", &json!([reminder("a", T0 + HOUR)]), T0);
        assert!(step.notices.is_empty());
        assert_eq!(s.next_due(), Some(T0 + HOUR));
        let step = s.tick(T0 + HOUR);
        assert_eq!(step.notices.len(), 1);
        assert_eq!(step.notices[0].target.as_ref().unwrap().path, "inbox/Tasks.md");
        // A renderer re-sending the schedule never repeats it.
        let step = s.set_schedule("main", &json!([reminder("a", T0 + HOUR)]), T0 + HOUR + 1000.0);
        assert!(step.notices.is_empty());
    }

    #[test]
    fn a_time_already_past_never_fires() {
        let mut s = started(T0);
        let step = s.set_schedule("main", &json!([reminder("past", T0 - 1000.0)]), T0);
        assert!(step.notices.is_empty());
    }

    #[test]
    fn what_came_due_while_quit_fires_once_on_launch_and_folds() {
        let mut first = started(T0);
        let step = first.set_schedule("main", &json!([reminder("a", T0 + HOUR), reminder("b", T0 + 2.0 * HOUR)]), T0);
        let saved = step.save.unwrap();
        // Relaunched three hours later: both missed, folded into one.
        let mut s = Schedule::new(T0 + 3.0 * HOUR);
        s.start(Some(&saved), T0 + 3.0 * HOUR);
        let step = s.tick(T0 + 3.0 * HOUR + BOOT_GRACE_MS);
        assert_eq!(step.notices.len(), 1);
        assert_eq!(step.notices[0].title, "2 missed reminders");
        assert_eq!(step.notices[0].body, "a, b");
        assert!(step.notices[0].target.is_none());
    }

    #[test]
    fn an_empty_schedule_during_startup_waits_for_the_grace() {
        let mut first = started(T0);
        let saved = first.set_schedule("main", &json!([reminder("a", T0 + HOUR)]), T0).save.unwrap();
        let mut s = Schedule::new(T0 + 2.0 * HOUR);
        s.start(Some(&saved), T0 + 2.0 * HOUR);
        assert!(s.set_schedule("main", &json!([]), T0 + 2.0 * HOUR).notices.is_empty());
        let step = s.tick(T0 + 2.0 * HOUR + BOOT_GRACE_MS);
        assert_eq!(step.notices.len(), 1);
        assert_eq!(step.notices[0].title, "a");
    }

    #[test]
    fn more_than_three_on_time_fold() {
        let mut s = started(T0);
        let items: Vec<Value> = (0..5).map(|i| reminder(&format!("r{i}"), T0 + HOUR)).collect();
        s.set_schedule("main", &Value::Array(items), T0);
        let step = s.tick(T0 + HOUR);
        assert_eq!(step.notices.len(), 1);
        assert_eq!(step.notices[0].title, "5 task reminders");
        assert_eq!(step.notices[0].body, "r0, r1 and 3 more");
    }

    #[test]
    fn nothing_older_than_a_day_catches_up() {
        let mut first = started(T0);
        let saved = first.set_schedule("main", &json!([reminder("old", T0 + HOUR)]), T0).save.unwrap();
        let later = T0 + 30.0 * HOUR;
        let mut s = Schedule::new(later);
        s.start(Some(&saved), later);
        assert!(s.tick(later + BOOT_GRACE_MS).notices.is_empty());
    }
}
