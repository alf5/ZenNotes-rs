//! Periodic notes for the MCP `daily_note` tool: where the daily, weekly or
//! monthly note for a date lives, which existing note is it, and what a new
//! one starts with. Pure logic only, no I/O. Ported from upstream:
//!
//! - apps/desktop/src/mcp/work-tools.ts: `resolveDate` and `isValidIsoDate`,
//!   `dailyNote` up to the point where it consults the backend, and
//!   `periodicBody` without the template listing.
//! - packages/app-core/src/lib/vault-layout.ts: the dailyNotes, weeklyNotes,
//!   monthlyNotes and primaryNotesLocation parts of `normalizeVaultSettings`
//!   (pattern history included), `noteTitleForDate`, `weeklyNoteTitle`,
//!   `monthlyNoteTitle`, `dailyNoteLocationForDate`,
//!   `weeklyNoteLocationForDate`, `monthlyNoteLocationForDate`,
//!   `vaultRelativeFolderPath` (folder `inbox` only), `notePathWithinFolder`,
//!   `noteFolderSubpath`, `classifyDateNote`, `findDailyNoteForDate`,
//!   `findWeeklyNoteForDate`, `findMonthlyNoteForDate`, and the pattern
//!   parse/format/match, candidate-date and ISO-week helpers under them.
//! - packages/app-core/src/lib/template-render.ts: `renderTemplate`,
//!   `formatDate`, `formatISODate`, `formatTime`, `getISOWeek`,
//!   `getISOWeekYear`, `mondayOfISOWeek`.
//! - packages/shared-domain/src/system-folder-paths.ts:
//!   `normalizeSystemFolderPaths`, `resolveFolderPath` (a remapped inbox moves
//!   the notes too).
//! - packages/shared-domain/src/builtin-templates.ts (`BUILTIN_TEMPLATES`) and
//!   packages/shared-domain/src/template-files.ts (`parseFrontmatter`,
//!   `parseCustomTemplate`, `mergeTemplates`, `findTemplate`).
//! - packages/bridge-contract/src/ipc.ts: the `DEFAULT_*` periodic constants.
//!
//! How JS semantics are kept:
//! - A JS `Date` here is local wall-clock time, so dates are `NaiveDate` /
//!   `NaiveDateTime` and the caller supplies local time. `new Date(y, m, d)`
//!   and `Date.UTC(y, m, d)` are both [`js_make_date`]: months and days
//!   overflow into the next unit, and a year in 0..=99 means 1900 + year, as
//!   in JS. That rule reaches results only for dates no caller produces, but
//!   it costs nothing to keep.
//! - `normalizeVaultSettings` is NOT idempotent (a directory of
//!   `inbox/inbox/x` in an Inbox vault loses one `inbox/` per pass, a lone
//!   `inbox` becomes '' and then the default), and upstream runs it again
//!   inside every helper it calls. [`SettingsRef`] and
//!   [`normalize_vault_settings`] replay exactly the same passes.
//! - `String.prototype.trim` is ECMAScript WhiteSpace plus LineTerminator (it
//!   has U+FEFF and lacks U+0085), so [`js_trim`], not `str::trim`. The one
//!   regex that matches note names (`matchDateNotePattern`) is a hand-written
//!   backtracking matcher over UTF-16 code units, explored in the order V8's
//!   engine explores `^…(\d{4})…[^/]+…$` (greedy, `\d` ASCII), so it captures
//!   what V8 captures. The other upstream regexes are simple enough to scan by
//!   hand; each scanner says which regex it replaces.
//!
//! Deviations from upstream, all documented where they live:
//! - Month and weekday names are English as V8 prints them for `en-US`
//!   ([`locale_name`]), whatever locale a pattern names. Upstream asks ICU
//!   for the pattern's locale, or the OS default for `system`; an `en-AU` or
//!   `en-GB` default prints `Sept` where `en-US` prints `Sep`.
//! - A settings value of the wrong JSON type where upstream calls `.trim()`
//!   on it (a number as `directory`, say) makes upstream throw a TypeError;
//!   here it counts as absent.
//! - `localeCompare` in `mergeTemplates` is approximated ([`locale_compare`]).
//!   It only breaks ties between templates that share an id, or that all
//!   claim to be the edited copy of one built-in.
//! - A local midnight that does not exist (a DST change at 00:00) is 01:00 in
//!   JS; here it stays 00:00. Only `{{time}}`/`HH` of an explicit date could
//!   tell.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use chrono::{Datelike, NaiveDate, NaiveDateTime, TimeDelta, Timelike};
use serde_json::Value;

/* ---------- Public API ------------------------------------------------- */

/// The error `daily_note` throws for a `kind` it does not know.
pub const KIND_ERROR: &str = "kind must be daily, weekly or monthly";
/// The error `resolveDate` throws for a `date` it cannot read.
pub const DATE_ERROR: &str = "date must be today, yesterday, tomorrow or YYYY-MM-DD";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeriodicKind {
    Daily,
    Weekly,
    Monthly,
}

impl PeriodicKind {
    const ALL: [PeriodicKind; 3] = [PeriodicKind::Daily, PeriodicKind::Weekly, PeriodicKind::Monthly];

    pub fn as_str(self) -> &'static str {
        match self {
            PeriodicKind::Daily => "daily",
            PeriodicKind::Weekly => "weekly",
            PeriodicKind::Monthly => "monthly",
        }
    }

    /// The tool's `kind` argument as the handler reads it: `''` (absent) is
    /// daily, and anything but the three names is [`KIND_ERROR`].
    pub fn from_arg(raw: &str) -> Option<Self> {
        match raw {
            "" | "daily" => Some(PeriodicKind::Daily),
            "weekly" => Some(PeriodicKind::Weekly),
            "monthly" => Some(PeriodicKind::Monthly),
            _ => None,
        }
    }

    fn defaults(self) -> KindDefaults {
        match self {
            PeriodicKind::Daily => KindDefaults {
                directory: "Daily Notes",
                title_pattern: "yyyy-MM-dd",
            },
            PeriodicKind::Weekly => KindDefaults {
                directory: "Weekly Notes",
                title_pattern: "yyyy-'W'ww",
            },
            PeriodicKind::Monthly => KindDefaults {
                directory: "Monthly Notes",
                title_pattern: "yyyy-MM",
            },
        }
    }
}

/// Everything dailyNote() computes before it looks at the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyNotePlan {
    /// `daily` | `weekly` | `monthly`.
    pub kind: &'static str,
    /// `noteTitleForDate(date)`.
    pub date: String,
    /// `!!plan.periodic.enabled`.
    pub enabled: bool,
    /// `location.subpath`: inside the inbox, for `createWith.subpath`.
    pub subpath: String,
    /// `location.title`.
    pub title: String,
    /// The vault-relative path the note has under the current pattern:
    /// `${dir}/${title}.md`, or `${title}.md` when `dir` is the vault root,
    /// with `dir = vaultRelativeFolderPath('inbox', subpath, settings)`.
    pub rel: String,
    /// The normalized `templateId` of this kind of note.
    pub template_id: Option<String>,
}

/// A note as the find functions read it (`listNotes()` metadata).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteRef<'a> {
    /// Vault-relative POSIX path.
    pub path: &'a str,
    /// File name without extension.
    pub title: &'a str,
    /// `inbox` | `quick` | `archive` | `trash`.
    pub folder: &'a str,
}

/// `dailyNote()`'s plan. `raw_settings` is `.zennotes/vault.json` as stored
/// (anything but a JSON object counts as `{}`); `primary` is the vault's
/// `primaryNotesLocation` (`inbox` | `root`), which replaces the stored one as
/// `{ ...raw, primaryNotesLocation }` does upstream.
pub fn plan_daily_note(
    kind: PeriodicKind,
    date: NaiveDate,
    raw_settings: &Value,
    primary: &str,
) -> DailyNotePlan {
    plan_internal(kind, date, raw_settings, primary).0
}

/// `dailyNote()`'s lookup: the note whose path is exactly `plan.rel`, else the
/// `find*NoteForDate` fallback, which also recognizes a note made under an
/// older naming pattern, with only this kind of note turned on (whether or
/// not the user has it on), as upstream's `lookup` does. Returns the index
/// into `notes`.
pub fn find_daily_note(
    kind: PeriodicKind,
    date: NaiveDate,
    raw_settings: &Value,
    primary: &str,
    notes: &[NoteRef],
) -> Option<usize> {
    let (plan, settings) = plan_internal(kind, date, raw_settings, primary);
    if let Some(index) = notes.iter().position(|note| note.path == plan.rel) {
        return Some(index);
    }
    // `{ ...settings, ...lookup }`
    let mut lookup = settings;
    lookup.daily.enabled = kind == PeriodicKind::Daily;
    lookup.weekly.enabled = kind == PeriodicKind::Weekly;
    lookup.monthly.enabled = kind == PeriodicKind::Monthly;
    find_note_for_date(kind, notes, SettingsRef::Normalized(&lookup), date)
}

/// `periodicBody()` without the I/O: the body a new periodic note starts
/// with. `custom` is the vault's own templates as `(sourcePath, raw)` pairs in
/// the order the backend lists them, or `None` when they could not be listed,
/// which leaves the heading (built-ins included: upstream's `try` covers the
/// listing). Returns the template's rendered body, or `# {title}\n\n` when
/// there is no template id or no template has it.
pub fn periodic_body(
    template_id: Option<&str>,
    custom: Option<&[(String, String)]>,
    title: &str,
    now: NaiveDateTime,
) -> String {
    if let (Some(id), Some(files)) = (template_id.filter(|id| !id.is_empty()), custom) {
        let custom: Vec<Template> = files
            .iter()
            .map(|(source_path, raw)| parse_custom_template(raw, source_path))
            .collect();
        let merged = merge_templates(builtin_templates(), custom);
        if let Some(template) = find_template(&merged, Some(id)) {
            return render_template(&template.body, title, now);
        }
    }
    format!("# {title}\n\n")
}

/// `resolveDate()`: `today`, `yesterday`, `tomorrow` (any case, trimmed) or
/// an exact `YYYY-MM-DD` naming a real day, against `now` in local time. An
/// explicit date is that day's midnight; a shifted one keeps `now`'s time.
pub fn resolve_date(raw: &str, now: NaiveDateTime) -> Result<NaiveDateTime, String> {
    match js_trim(raw).to_lowercase().as_str() {
        "" | "today" => return Ok(now),
        "yesterday" => return shift_days(now, -1),
        "tomorrow" => return shift_days(now, 1),
        _ => {}
    }
    let Some((year, month, day)) = valid_iso_date(raw) else {
        return Err(DATE_ERROR.to_string());
    };
    js_make_date(year, month - 1, day)
        .and_then(|date| date.and_hms_opt(0, 0, 0))
        .ok_or_else(|| DATE_ERROR.to_string())
}

/// `date.setDate(date.getDate() + days)`.
fn shift_days(now: NaiveDateTime, days: i64) -> Result<NaiveDateTime, String> {
    TimeDelta::try_days(days)
        .and_then(|delta| now.checked_add_signed(delta))
        .ok_or_else(|| DATE_ERROR.to_string())
}

/// `isValidIsoDate()`: `/^\d{4}-\d{2}-\d{2}$/` (ASCII digits, and `$` is the
/// very end in JS) naming a real day by `Date.UTC`, whose two-digit-year rule
/// rejects years 0000-0099. Returns the numbers.
fn valid_iso_date(value: &str) -> Option<(i64, i64, i64)> {
    let b = value.as_bytes();
    let shape = b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit());
    if !shape {
        return None;
    }
    let year: i64 = value[0..4].parse().ok()?;
    let month: i64 = value[5..7].parse().ok()?;
    let day: i64 = value[8..10].parse().ok()?;
    let date = js_make_date(year, month - 1, day)?;
    (i64::from(date.year()) == year
        && i64::from(date.month0()) == month - 1
        && i64::from(date.day()) == day)
        .then_some((year, month, day))
}

fn plan_internal(
    kind: PeriodicKind,
    date: NaiveDate,
    raw_settings: &Value,
    primary: &str,
) -> (DailyNotePlan, VaultSettings) {
    let settings = normalize_vault_settings(SettingsRef::Json {
        value: raw_settings,
        primary: Some(primary),
    });
    let location = location_for_date(kind, date, SettingsRef::Normalized(&settings));
    let dir = vault_relative_inbox_path(&location.subpath, &settings);
    let rel = if dir.is_empty() {
        format!("{}.md", location.title)
    } else {
        format!("{dir}/{}.md", location.title)
    };
    let periodic = settings.periodic(kind);
    let plan = DailyNotePlan {
        kind: kind.as_str(),
        date: note_title_for_date(date),
        enabled: periodic.enabled,
        subpath: location.subpath,
        title: location.title,
        rel,
        template_id: periodic.template_id.clone(),
    };
    (plan, settings)
}

/* ---------- JS helpers ------------------------------------------------- */

/// ECMAScript WhiteSpace + LineTerminator, the set `trim` and `\s` use.
fn is_js_ws(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n'
            | '\u{0B}'
            | '\u{0C}'
            | '\r'
            | ' '
            | '\u{A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200A}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
            | '\u{FEFF}'
    )
}

/// `String.prototype.trim`.
fn js_trim(s: &str) -> &str {
    s.trim_matches(is_js_ws)
}

/// `String(n).padStart(2, '0')`.
fn pad2(n: i64) -> String {
    let s = n.to_string();
    if s.len() < 2 {
        format!("0{s}")
    } else {
        s
    }
}

/// `s.replace(/^\/+|\/+$/g, '')`.
fn strip_slashes(s: &str) -> &str {
    s.trim_start_matches('/').trim_end_matches('/')
}

/// `s.replace(/[\\/]+/g, '-')`.
fn replace_slash_runs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_run = false;
    for c in s.chars() {
        if c == '/' || c == '\\' {
            if !in_run {
                out.push('-');
            }
            in_run = true;
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `s.slice(n)` with `n` in UTF-16 code units. A cut through a surrogate pair
/// would start the JS string with a lone low surrogate, which a Rust string
/// cannot hold: it becomes U+FFFD (only a case mapping that changes length
/// can get there).
fn utf16_slice_from(s: &str, n: usize) -> Cow<'_, str> {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        if units == n {
            return Cow::Borrowed(&s[i..]);
        }
        units += c.len_utf16();
        if units > n {
            return Cow::Owned(format!("\u{FFFD}{}", &s[i + c.len_utf8()..]));
        }
    }
    Cow::Borrowed("")
}

/// `!!value` for a property read off parsed JSON (`undefined` is `None`).
fn js_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        // -0.0 == 0.0, so -0 is falsy too.
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_)) | Some(Value::Object(_)) => true,
    }
}

/// A property upstream passes to `(value ?? '').trim()`. `null`/absent are
/// `None` as in JS. Any other non-string makes upstream throw a TypeError;
/// it is `None` here (documented deviation).
fn js_str(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str)
}

/* ---------- JS Date arithmetic ----------------------------------------- */

/// `new Date(year, month0, day)` (local) or `Date.UTC(year, month0, day)` as
/// a calendar day: a year in 0..=99 means 1900 + year, and months and days
/// overflow (`day` 0 is the last day of the previous month). `None` only
/// outside chrono's range of years (about ±262 000).
fn js_make_date(year: i64, month0: i64, day: i64) -> Option<NaiveDate> {
    let year = if (0..=99).contains(&year) { 1900 + year } else { year };
    let year = year.checked_add(month0.div_euclid(12))?;
    let month = u32::try_from(month0.rem_euclid(12)).ok()? + 1;
    let first = NaiveDate::from_ymd_opt(i32::try_from(year).ok()?, month, 1)?;
    add_days(first, day.checked_sub(1)?)
}

fn add_days(date: NaiveDate, days: i64) -> Option<NaiveDate> {
    date.checked_add_signed(TimeDelta::try_days(days)?)
}

/// `isoWeekParts()`: `(week, weekYear)`, computed as upstream does: shift to
/// the Thursday of the week, count weeks from Jan 1 of its year.
fn iso_week_parts(date: NaiveDate) -> (i64, i64) {
    js_iso_week_parts(date).unwrap_or_else(|| {
        let week = date.iso_week();
        (i64::from(week.week()), i64::from(week.year()))
    })
}

fn js_iso_week_parts(date: NaiveDate) -> Option<(i64, i64)> {
    let start = js_make_date(
        i64::from(date.year()),
        i64::from(date.month0()),
        i64::from(date.day()),
    )?;
    let day_num = i64::from(start.weekday().number_from_monday()); // getUTCDay() || 7
    let thursday = add_days(start, 4 - day_num)?;
    let year_start = js_make_date(i64::from(thursday.year()), 0, 1)?;
    let days = thursday.signed_duration_since(year_start).num_days();
    // Math.ceil((days + 1) / 7)
    Some(((days + 1 + 6).div_euclid(7), i64::from(thursday.year())))
}

/// `getISOWeek()`.
fn iso_week(date: NaiveDate) -> i64 {
    iso_week_parts(date).0
}

/// `mondayOfISOWeek()`: the Monday of `week` in ISO week-year `week_year`.
fn monday_of_iso_week(week_year: i64, week: i64) -> Option<NaiveDate> {
    let jan4 = js_make_date(week_year, 0, 4)?;
    let day_num = i64::from(jan4.weekday().number_from_monday());
    let monday = add_days(jan4, -(day_num - 1) + (week - 1) * 7)?;
    js_make_date(
        i64::from(monday.year()),
        i64::from(monday.month0()),
        i64::from(monday.day()),
    )
}

/// `noteTitleForDate()` (and template-render's `formatISODate()`).
fn note_title_for_date(date: NaiveDate) -> String {
    format!(
        "{}-{}-{}",
        date.year(),
        pad2(i64::from(date.month())),
        pad2(i64::from(date.day()))
    )
}

/// `weeklyNoteTitle()`.
fn weekly_note_title(date: NaiveDate) -> String {
    let (week, year) = iso_week_parts(date);
    format!("{year}-W{}", pad2(week))
}

/// `monthlyNoteTitle()`.
fn monthly_note_title(date: NaiveDate) -> String {
    format!("{}-{}", date.year(), pad2(i64::from(date.month())))
}

fn date_key(kind: PeriodicKind, date: NaiveDate) -> String {
    match kind {
        PeriodicKind::Daily => note_title_for_date(date),
        PeriodicKind::Weekly => weekly_note_title(date),
        PeriodicKind::Monthly => monthly_note_title(date),
    }
}

/* ---------- Month and weekday names ------------------------------------ */

const MONTHS_LONG: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September",
    "October", "November", "December",
];
const MONTHS_SHORT: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];
/// From Sunday, as `getDay()` counts.
const WEEKDAYS_LONG: [&str; 7] = [
    "Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday",
];
const WEEKDAYS_SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

#[derive(Debug, Clone, Copy)]
enum NamePart {
    MonthLong,
    MonthShort,
    WeekdayLong,
    WeekdayShort,
}

/// `date.toLocaleDateString(locale, { month | weekday: 'long' | 'short' })`.
///
/// Always English, as V8 (ICU) prints it for `en-US`. Upstream formats in
/// `locale` (`undefined` = the OS default; an unknown tag falls back to the
/// default), so a pattern locale like `fr-FR`, or an OS default other than
/// `en-US`, prints other names there: `en-AU` and `en-GB` print `Sept` for
/// September's short name. That is an accepted, documented deviation.
fn locale_name(date: NaiveDate, _locale: Option<&str>, part: NamePart) -> &'static str {
    let month = date.month0() as usize;
    let weekday = date.weekday().num_days_from_sunday() as usize;
    match part {
        NamePart::MonthLong => MONTHS_LONG[month],
        NamePart::MonthShort => MONTHS_SHORT[month],
        NamePart::WeekdayLong => WEEKDAYS_LONG[weekday],
        NamePart::WeekdayShort => WEEKDAYS_SHORT[weekday],
    }
}

/// Every periodic locale defaults to `system`.
const DEFAULT_LOCALE: &str = "system";

/// `localeArg()`: `system` means the default locale.
fn locale_arg(locale: &str) -> Option<&str> {
    (locale != DEFAULT_LOCALE).then_some(locale)
}

/* ---------- Vault settings --------------------------------------------- */

struct KindDefaults {
    directory: &'static str,
    title_pattern: &'static str,
}

/// `DateNotePatternSettings`, normalized (all three fields always set).
#[derive(Debug, Clone, PartialEq, Eq)]
struct DatePattern {
    directory: String,
    title_pattern: String,
    locale: String,
}

impl DatePattern {
    /// `dateNotePatternKey()`.
    fn key(&self) -> String {
        format!("{}\0{}\0{}", self.directory, self.title_pattern, self.locale)
    }
}

/// `dailyNotes` / `weeklyNotes` / `monthlyNotes` after normalization. The
/// daily-only task flags do not reach anything ported here.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PeriodicSettings {
    enabled: bool,
    directory: String,
    title_pattern: String,
    locale: String,
    legacy_patterns: Vec<DatePattern>,
    template_id: Option<String>,
}

/// The part of a normalized `VaultSettings` this module reads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct VaultSettings {
    /// `primaryNotesLocation === 'root'`.
    primary_root: bool,
    daily: PeriodicSettings,
    weekly: PeriodicSettings,
    monthly: PeriodicSettings,
    /// `resolveFolderPath('inbox', systemFolderPaths)`. Normalizing the
    /// folder map is idempotent, so the resolved name is carried as is.
    inbox_dir: String,
}

impl VaultSettings {
    fn periodic(&self, kind: PeriodicKind) -> &PeriodicSettings {
        match kind {
            PeriodicKind::Daily => &self.daily,
            PeriodicKind::Weekly => &self.weekly,
            PeriodicKind::Monthly => &self.monthly,
        }
    }
}

/// What a TS function's `settings` argument holds when it calls
/// `normalizeVaultSettings(settings)`: stored JSON on the first pass, an
/// already normalized object on every later one.
#[derive(Clone, Copy)]
enum SettingsRef<'a> {
    /// Stored JSON; `primary`, when set, replaces its `primaryNotesLocation`.
    Json {
        value: &'a Value,
        primary: Option<&'a str>,
    },
    Normalized(&'a VaultSettings),
}

/// `(directory, titlePattern, locale)` of a stored pattern.
type PatternFields<'a> = (Option<&'a str>, Option<&'a str>, Option<&'a str>);

/// The stored fields of one periodic block, as `normalizeVaultSettings` reads
/// them.
struct PeriodicInput<'a> {
    enabled: bool,
    directory: Option<&'a str>,
    title_pattern: Option<&'a str>,
    locale: Option<&'a str>,
    /// `None` when not an array.
    legacy_patterns: Option<Vec<PatternFields<'a>>>,
    template_id: Option<&'a str>,
}

impl<'a> PeriodicInput<'a> {
    /// `settings?.dailyNotes?.…`: a block that is not an object has no fields.
    fn from_json(block: Option<&'a Value>) -> Self {
        let object = block.and_then(Value::as_object);
        let get = |key: &str| object.and_then(|o| o.get(key));
        let legacy_patterns = get("legacyPatterns").and_then(Value::as_array).map(|patterns| {
            patterns
                .iter()
                .map(|pattern| {
                    let fields = pattern.as_object();
                    let field = |key: &str| js_str(fields.and_then(|o| o.get(key)));
                    (field("directory"), field("titlePattern"), field("locale"))
                })
                .collect()
        });
        PeriodicInput {
            enabled: js_truthy(get("enabled")),
            directory: js_str(get("directory")),
            title_pattern: js_str(get("titlePattern")),
            locale: js_str(get("locale")),
            legacy_patterns,
            template_id: js_str(get("templateId")),
        }
    }

    fn from_settings(settings: &'a PeriodicSettings) -> Self {
        PeriodicInput {
            enabled: settings.enabled,
            directory: Some(&settings.directory),
            title_pattern: Some(&settings.title_pattern),
            locale: Some(&settings.locale),
            legacy_patterns: Some(
                settings
                    .legacy_patterns
                    .iter()
                    .map(|p| {
                        (
                            Some(p.directory.as_str()),
                            Some(p.title_pattern.as_str()),
                            Some(p.locale.as_str()),
                        )
                    })
                    .collect(),
            ),
            template_id: settings.template_id.as_deref(),
        }
    }
}

/// `normalizeVaultSettings()`, for the fields this module reads.
fn normalize_vault_settings(settings: SettingsRef) -> VaultSettings {
    match settings {
        SettingsRef::Json { value, primary } => {
            let object = value.as_object();
            let get = |key: &str| object.and_then(|o| o.get(key));
            let primary_root = match primary {
                Some(primary) => primary == "root",
                None => get("primaryNotesLocation").and_then(Value::as_str) == Some("root"),
            };
            let periodic = |kind: PeriodicKind, key: &str| {
                normalize_periodic(kind, &PeriodicInput::from_json(get(key)), primary_root)
            };
            let folders = normalize_system_folder_paths(get("systemFolderPaths"));
            VaultSettings {
                primary_root,
                daily: periodic(PeriodicKind::Daily, "dailyNotes"),
                weekly: periodic(PeriodicKind::Weekly, "weeklyNotes"),
                monthly: periodic(PeriodicKind::Monthly, "monthlyNotes"),
                inbox_dir: resolve_folder_path(0, &folders).to_string(),
            }
        }
        SettingsRef::Normalized(s) => {
            let periodic = |kind: PeriodicKind| {
                normalize_periodic(
                    kind,
                    &PeriodicInput::from_settings(s.periodic(kind)),
                    s.primary_root,
                )
            };
            VaultSettings {
                primary_root: s.primary_root,
                daily: periodic(PeriodicKind::Daily),
                weekly: periodic(PeriodicKind::Weekly),
                monthly: periodic(PeriodicKind::Monthly),
                inbox_dir: s.inbox_dir.clone(),
            }
        }
    }
}

fn normalize_periodic(
    kind: PeriodicKind,
    input: &PeriodicInput,
    primary_root: bool,
) -> PeriodicSettings {
    PeriodicSettings {
        enabled: input.enabled,
        directory: normalize_primary_relative_subpath(
            normalize_directory(kind, input.directory),
            primary_root,
        ),
        title_pattern: normalize_title_pattern(kind, input.title_pattern),
        locale: normalize_locale(input.locale),
        legacy_patterns: normalize_pattern_history(
            kind,
            input.legacy_patterns.as_deref(),
            primary_root,
        ),
        template_id: normalize_template_id(input.template_id),
    }
}

/// `normalizeDailyNotesDirectory()` and its weekly/monthly twins.
fn normalize_directory(kind: PeriodicKind, directory: Option<&str>) -> String {
    let trimmed = strip_slashes(js_trim(directory.unwrap_or("")));
    if trimmed.is_empty() {
        kind.defaults().directory.to_string()
    } else {
        trimmed.to_string()
    }
}

/// `normalizeDailyNoteTitlePattern()` and its twins.
fn normalize_title_pattern(kind: PeriodicKind, value: Option<&str>) -> String {
    let trimmed = replace_slash_runs(js_trim(value.unwrap_or("")));
    if trimmed.is_empty() {
        kind.defaults().title_pattern.to_string()
    } else {
        trimmed
    }
}

/// `normalizeDailyNoteLocale()` and its twins (one default for all three).
fn normalize_locale(value: Option<&str>) -> String {
    let trimmed = js_trim(value.unwrap_or(""));
    if trimmed.is_empty() {
        DEFAULT_LOCALE.to_string()
    } else {
        trimmed.to_string()
    }
}

/// `normalizePrimaryRelativeSubpath()`: in an Inbox vault, a directory that
/// names the inbox itself is relative to it.
fn normalize_primary_relative_subpath(subpath: String, primary_root: bool) -> String {
    if primary_root {
        return subpath;
    }
    if subpath == "inbox" {
        return String::new();
    }
    match subpath.strip_prefix("inbox/") {
        Some(rest) => rest.to_string(),
        None => subpath,
    }
}

/// `normalizeTemplateId()`.
fn normalize_template_id(value: Option<&str>) -> Option<String> {
    let trimmed = js_trim(value.unwrap_or(""));
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// `normalizeDailyNotePatternHistory()` and its twins.
fn normalize_pattern_history(
    kind: PeriodicKind,
    patterns: Option<&[PatternFields]>,
    primary_root: bool,
) -> Vec<DatePattern> {
    let Some(patterns) = patterns else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for &(directory, title_pattern, locale) in patterns {
        let next = DatePattern {
            directory: normalize_primary_relative_subpath(
                normalize_directory(kind, directory),
                primary_root,
            ),
            title_pattern: normalize_title_pattern(kind, title_pattern),
            locale: normalize_locale(locale),
        };
        if seen.insert(next.key()) {
            out.push(next);
        }
    }
    out
}

/// `currentDailyNotePattern()` and its twins.
fn current_pattern(settings: &VaultSettings, kind: PeriodicKind) -> DatePattern {
    let periodic = settings.periodic(kind);
    DatePattern {
        directory: periodic.directory.clone(),
        title_pattern: periodic.title_pattern.clone(),
        locale: periodic.locale.clone(),
    }
}

/// `dailyNotePatterns()` and its twins: the current pattern, then the
/// earlier ones it does not repeat.
fn note_patterns(settings: &VaultSettings, kind: PeriodicKind) -> Vec<DatePattern> {
    let current = current_pattern(settings, kind);
    let mut seen = HashSet::from([current.key()]);
    let mut out = vec![current];
    for pattern in &settings.periodic(kind).legacy_patterns {
        if seen.insert(pattern.key()) {
            out.push(pattern.clone());
        }
    }
    out
}

/* ---------- System folder paths ---------------------------------------- */

/// `FOLDER_IDS`; each folder's default directory is its own id.
const FOLDER_IDS: [&str; 4] = ["inbox", "quick", "archive", "trash"];
const RESERVED_ROOT_NAMES: [&str; 6] = [
    "assets",
    ".zennotes",
    "attachements",
    "_assets",
    "deleted-assets",
    "comments",
];
const MAX_FOLDER_PATH_LENGTH: usize = 128;

type FolderPaths = [Option<String>; 4];

/// `normalizeSystemFolderPath()`.
fn normalize_system_folder_path(value: Option<&Value>) -> Option<String> {
    let trimmed = js_trim(value?.as_str()?);
    if trimmed.is_empty()
        || utf16_len(trimmed) > MAX_FOLDER_PATH_LENGTH
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.starts_with('.')
        // INVALID_CHARS_RE = /[\\:*?"<>|#^\[\]<>]/
        || trimmed.chars().any(|c| "\\:*?\"<>|#^[]".contains(c))
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// `normalizeSystemFolderPaths()`.
fn normalize_system_folder_paths(value: Option<&Value>) -> FolderPaths {
    let mut next: FolderPaths = Default::default();
    // `!value || typeof value !== 'object'`; an array has no folder keys.
    let Some(raw) = value.and_then(Value::as_object) else {
        return next;
    };
    for (i, key) in FOLDER_IDS.iter().enumerate() {
        let Some(path) = normalize_system_folder_path(raw.get(*key)) else {
            continue;
        };
        if path == *key {
            continue;
        }
        let lower = path.to_lowercase();
        if FOLDER_IDS
            .iter()
            .enumerate()
            .any(|(j, other)| j != i && lower == *other)
        {
            continue;
        }
        next[i] = Some(path);
    }
    let mut changed = true;
    while changed {
        changed = false;
        for i in 0..FOLDER_IDS.len() {
            if next[i].is_none() {
                continue;
            }
            let lower = resolve_folder_path(i, &next).to_lowercase();
            if RESERVED_ROOT_NAMES.contains(&lower.as_str()) {
                next[i] = None;
                changed = true;
                continue;
            }
            for j in 0..FOLDER_IDS.len() {
                if j != i && resolve_folder_path(j, &next).to_lowercase() == lower {
                    next[i] = None;
                    changed = true;
                    break;
                }
            }
        }
    }
    next
}

/// `resolveFolderPath()` by index into [`FOLDER_IDS`].
fn resolve_folder_path(folder: usize, overrides: &FolderPaths) -> &str {
    overrides[folder].as_deref().unwrap_or(FOLDER_IDS[folder])
}

/// `vaultRelativeFolderPath('inbox', subpath, settings)`.
fn vault_relative_inbox_path(subpath: &str, settings: &VaultSettings) -> String {
    let sub = strip_slashes(subpath);
    if settings.primary_root {
        return sub.to_string();
    }
    if sub.is_empty() {
        settings.inbox_dir.clone()
    } else {
        format!("{}/{sub}", settings.inbox_dir)
    }
}

/// `notePathWithinFolder(path, 'inbox', settings)`: the inbox directory is
/// matched case-insensitively (`toLowerCase` on both sides), then cut by its
/// own UTF-16 length.
fn note_path_within_inbox<'p>(path: &'p str, settings: &VaultSettings) -> Cow<'p, str> {
    if settings.primary_root {
        return Cow::Borrowed(path);
    }
    let prefix = format!("{}/", settings.inbox_dir);
    if path.to_lowercase().starts_with(&prefix.to_lowercase()) {
        utf16_slice_from(path, utf16_len(&prefix))
    } else {
        Cow::Borrowed(path)
    }
}

/// `noteFolderSubpath()` for an inbox note.
fn note_folder_subpath(path: &str, settings: &VaultSettings) -> String {
    let within = note_path_within_inbox(path, settings);
    let parts: Vec<&str> = within.split('/').filter(|part| !part.is_empty()).collect();
    if parts.len() > 1 {
        parts[..parts.len() - 1].join("/")
    } else {
        String::new()
    }
}

/* ---------- Date note patterns ----------------------------------------- */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Token {
    Yyyy,
    Yy,
    MonthLong,
    MonthShort,
    Mm,
    M,
    Dd,
    D,
    WeekdayLong,
    WeekdayShort,
    Ww,
    W,
}

/// `DATE_NOTE_PATTERN_TOKENS`, longest first where one prefixes another.
const PATTERN_TOKENS: [(&str, Token); 12] = [
    ("yyyy", Token::Yyyy),
    ("yy", Token::Yy),
    ("MMMM", Token::MonthLong),
    ("MMM", Token::MonthShort),
    ("MM", Token::Mm),
    ("M", Token::M),
    ("dd", Token::Dd),
    ("d", Token::D),
    ("EEEE", Token::WeekdayLong),
    ("EEE", Token::WeekdayShort),
    ("ww", Token::Ww),
    ("w", Token::W),
];

#[derive(Debug, Clone, PartialEq, Eq)]
enum PatternPart {
    Literal(String),
    Token(Token),
}

/// `parseDateNotePattern()`: tokens outside quotes, `'…'` quotes a literal,
/// `''` is a quote. Tokens and quotes are ASCII, so walking chars instead of
/// UTF-16 units splits nothing differently.
fn parse_date_note_pattern(pattern: &str) -> Vec<PatternPart> {
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut quoted = false;
    let mut rest = pattern;
    while let Some(ch) = rest.chars().next() {
        if ch == '\'' {
            if rest[1..].starts_with('\'') {
                literal.push('\'');
                rest = &rest[2..];
                continue;
            }
            quoted = !quoted;
            rest = &rest[1..];
            continue;
        }
        if !quoted {
            if let Some((text, token)) = PATTERN_TOKENS.iter().find(|(text, _)| rest.starts_with(text)) {
                if !literal.is_empty() {
                    parts.push(PatternPart::Literal(std::mem::take(&mut literal)));
                }
                parts.push(PatternPart::Token(*token));
                rest = &rest[text.len()..];
                continue;
            }
        }
        literal.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    if !literal.is_empty() {
        parts.push(PatternPart::Literal(literal));
    }
    parts
}

/// The `parts` argument of `formatDateNotePattern()`: a weekly note formats
/// its ISO week-year and week, not the Monday's own.
#[derive(Debug, Clone, Copy, Default)]
struct PatternParts {
    year: Option<i64>,
    week: Option<i64>,
}

/// `formatDateNotePattern()`.
fn format_date_note_pattern(
    date: NaiveDate,
    pattern: &str,
    locale: &str,
    parts: PatternParts,
) -> String {
    let year = parts.year.unwrap_or(i64::from(date.year()));
    let week = parts.week.unwrap_or_else(|| iso_week(date));
    let month = i64::from(date.month());
    let day = i64::from(date.day());
    let locale = locale_arg(locale);
    let mut out = String::new();
    for part in parse_date_note_pattern(pattern) {
        match part {
            PatternPart::Literal(text) => out.push_str(&text),
            PatternPart::Token(token) => match token {
                Token::Yyyy => out.push_str(&year.to_string()),
                Token::Yy => out.push_str(&pad2(year % 100)),
                Token::MonthLong => out.push_str(locale_name(date, locale, NamePart::MonthLong)),
                Token::MonthShort => out.push_str(locale_name(date, locale, NamePart::MonthShort)),
                Token::Mm => out.push_str(&pad2(month)),
                Token::M => out.push_str(&month.to_string()),
                Token::Dd => out.push_str(&pad2(day)),
                Token::D => out.push_str(&day.to_string()),
                Token::WeekdayLong => {
                    out.push_str(locale_name(date, locale, NamePart::WeekdayLong))
                }
                Token::WeekdayShort => {
                    out.push_str(locale_name(date, locale, NamePart::WeekdayShort))
                }
                Token::Ww => out.push_str(&pad2(week)),
                Token::W => out.push_str(&week.to_string()),
            },
        }
    }
    out
}

/// `DateNotePatternMatch`: the numbers a directory or title gives away.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct PatternMatch {
    year: Option<i64>,
    month: Option<i64>,
    day: Option<i64>,
    week: Option<i64>,
}

/// `mergeCapture()`: false when the slot already holds another value.
fn merge_capture(slot: &mut Option<i64>, value: i64) -> bool {
    if slot.is_some_and(|current| current != value) {
        return false;
    }
    *slot = Some(value);
    true
}

/// `mergePatternMatch()`.
fn merge_pattern_match(target: &mut PatternMatch, source: &PatternMatch) -> bool {
    [
        (&mut target.year, source.year),
        (&mut target.month, source.month),
        (&mut target.day, source.day),
        (&mut target.week, source.week),
    ]
    .into_iter()
    .all(|(slot, value)| match value {
        Some(value) => merge_capture(slot, value),
        None => true,
    })
}

/// `readTwoDigitYear()`.
fn read_two_digit_year(n: i64) -> i64 {
    if n >= 70 {
        1900 + n
    } else {
        2000 + n
    }
}

/// One piece of the regex `matchDateNotePattern()` builds.
enum MatchElem {
    /// `escapeRegex(literal)`, as UTF-16 units.
    Literal(Vec<u16>),
    /// `(\d{4})`, `(\d{2})` or `(\d{1,2})`, with the token it captures.
    Digits { min: usize, max: usize, token: Token },
    /// `[^/]+` (a name; not captured).
    NotSlash,
}

/// Backtracking match of `^elems$` from `pos`, trying the longest run of each
/// quantifier first as V8 does, so the captures are the ones V8 returns.
fn match_elems(
    elems: &[MatchElem],
    text: &[u16],
    pos: usize,
    captures: &mut Vec<(Token, usize, usize)>,
) -> bool {
    let Some((elem, rest)) = elems.split_first() else {
        return pos == text.len();
    };
    match elem {
        MatchElem::Literal(units) => {
            text[pos..].starts_with(units) && match_elems(rest, text, pos + units.len(), captures)
        }
        MatchElem::Digits { min, max, token } => {
            let available = text[pos..]
                .iter()
                .take(*max)
                .take_while(|&&unit| (u16::from(b'0')..=u16::from(b'9')).contains(&unit))
                .count();
            for len in (*min..=available).rev() {
                captures.push((*token, pos, pos + len));
                if match_elems(rest, text, pos + len, captures) {
                    return true;
                }
                captures.pop();
            }
            false
        }
        MatchElem::NotSlash => {
            let available = text[pos..]
                .iter()
                .take_while(|&&unit| unit != u16::from(b'/'))
                .count();
            (1..=available)
                .rev()
                .any(|len| match_elems(rest, text, pos + len, captures))
        }
    }
}

/// `matchDateNotePattern()`.
fn match_date_note_pattern(pattern: &str, text: &str) -> Option<PatternMatch> {
    let elems: Vec<MatchElem> = parse_date_note_pattern(pattern)
        .into_iter()
        .map(|part| match part {
            PatternPart::Literal(literal) => MatchElem::Literal(literal.encode_utf16().collect()),
            PatternPart::Token(token) => match token {
                Token::Yyyy => MatchElem::Digits { min: 4, max: 4, token },
                Token::Yy => MatchElem::Digits { min: 2, max: 2, token },
                Token::Mm | Token::M | Token::Dd | Token::D | Token::Ww | Token::W => {
                    MatchElem::Digits { min: 1, max: 2, token }
                }
                Token::MonthLong | Token::MonthShort | Token::WeekdayLong | Token::WeekdayShort => {
                    MatchElem::NotSlash
                }
            },
        })
        .collect();
    let text: Vec<u16> = text.encode_utf16().collect();
    let mut captures = Vec::new();
    if !match_elems(&elems, &text, 0, &mut captures) {
        return None;
    }
    let mut out = PatternMatch::default();
    for (token, start, end) in captures {
        let value: i64 = String::from_utf16_lossy(&text[start..end]).parse().ok()?;
        let merged = match token {
            Token::Yyyy => merge_capture(&mut out.year, value),
            Token::Yy => merge_capture(&mut out.year, read_two_digit_year(value)),
            Token::Mm | Token::M => merge_capture(&mut out.month, value),
            Token::Dd | Token::D => merge_capture(&mut out.day, value),
            Token::Ww | Token::W => merge_capture(&mut out.week, value),
            _ => true,
        };
        if !merged {
            return None;
        }
    }
    Some(out)
}

/// `shouldFormatDirectoryPattern()`: a plain folder name such as `daily` or
/// `Monthly Notes` stays literal even though it holds token letters.
fn should_format_directory_pattern(pattern: &str) -> bool {
    let tokens: Vec<Token> = parse_date_note_pattern(pattern)
        .into_iter()
        .filter_map(|part| match part {
            PatternPart::Token(token) => Some(token),
            PatternPart::Literal(_) => None,
        })
        .collect();
    pattern.contains('\'')
        || tokens.len() >= 2
        || tokens
            .iter()
            .any(|token| matches!(token, Token::Yyyy | Token::Yy | Token::Ww))
}

/// `formatDirectoryPattern()`.
fn format_directory_pattern(
    date: NaiveDate,
    pattern: &str,
    locale: &str,
    parts: PatternParts,
) -> String {
    if !should_format_directory_pattern(pattern) {
        return pattern.to_string();
    }
    format_date_note_pattern(date, pattern, locale, parts)
}

/// `matchDirectoryPattern()`.
fn match_directory_pattern(pattern: &str, text: &str) -> Option<PatternMatch> {
    if !should_format_directory_pattern(pattern) {
        return (pattern == text).then(PatternMatch::default);
    }
    match_date_note_pattern(pattern, text)
}

/* ---------- Locations -------------------------------------------------- */

/// `DailyNoteLocation` and its twins.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Location {
    title: String,
    subpath: String,
}

/// `dailyNoteLocationForPattern()`, `weeklyNoteLocationForPattern()` and
/// `monthlyNoteLocationForPattern()`. A weekly note is formatted from the
/// Monday of its ISO week, a monthly one from the first of its month.
fn location_for_pattern(
    kind: PeriodicKind,
    date: NaiveDate,
    settings: &VaultSettings,
    pattern: &DatePattern,
) -> Location {
    let (anchor, parts) = match kind {
        PeriodicKind::Daily => (date, PatternParts::default()),
        PeriodicKind::Weekly => {
            let (week, week_year) = iso_week_parts(date);
            // Only out of chrono's range could there be no Monday.
            let monday = monday_of_iso_week(week_year, week).unwrap_or(date);
            let parts = PatternParts {
                year: Some(week_year),
                week: Some(week),
            };
            (monday, parts)
        }
        PeriodicKind::Monthly => {
            let first = js_make_date(i64::from(date.year()), i64::from(date.month0()), 1);
            (first.unwrap_or(date), PatternParts::default())
        }
    };
    let directory = format_directory_pattern(anchor, &pattern.directory, &pattern.locale, parts);
    let subpath = normalize_primary_relative_subpath(
        normalize_directory(kind, Some(&directory)),
        settings.primary_root,
    );
    let title = normalize_title_pattern(
        kind,
        Some(&format_date_note_pattern(anchor, &pattern.title_pattern, &pattern.locale, parts)),
    );
    Location { title, subpath }
}

/// `dailyNoteLocationForDate()`, `weeklyNoteLocationForDate()` and
/// `monthlyNoteLocationForDate()`: each normalizes what it is given.
fn location_for_date(kind: PeriodicKind, date: NaiveDate, settings: SettingsRef) -> Location {
    let normalized = normalize_vault_settings(settings);
    location_for_pattern(kind, date, &normalized, &current_pattern(&normalized, kind))
}

/* ---------- Recognizing date notes ------------------------------------- */

/// `patternNumerics()`.
fn pattern_numerics(pattern: &DatePattern, subpath: &str, title: &str) -> Option<PatternMatch> {
    let directory = match_directory_pattern(&pattern.directory, subpath);
    let title = match_date_note_pattern(&pattern.title_pattern, title);
    let (directory, title) = (directory?, title?);
    let mut merged = PatternMatch::default();
    (merge_pattern_match(&mut merged, &directory) && merge_pattern_match(&mut merged, &title))
        .then_some(merged)
}

/// `dailyCandidateDates()`.
fn daily_candidate_dates(n: &PatternMatch) -> Vec<NaiveDate> {
    let Some(year) = n.year else {
        return Vec::new();
    };
    if let (Some(month), Some(day)) = (n.month, n.day) {
        let valid = js_make_date(year, month - 1, day).filter(|date| {
            i64::from(date.year()) == year
                && i64::from(date.month0()) == month - 1
                && i64::from(date.day()) == day
        });
        return valid.into_iter().collect();
    }
    let first_month = n.month.map_or(0, |month| month - 1);
    let last_month = n.month.map_or(11, |month| month - 1);
    let mut out = Vec::new();
    for month in first_month..=last_month {
        let Some(last_day) = js_make_date(year, month + 1, 0) else {
            continue;
        };
        for day in 1..=i64::from(last_day.day()) {
            if n.day.is_some_and(|wanted| wanted != day) {
                continue;
            }
            let Some(date) = js_make_date(year, month, day) else {
                continue;
            };
            if n.week.is_some_and(|week| iso_week(date) != week) {
                continue;
            }
            out.push(date);
        }
    }
    out
}

/// `weeklyCandidateMondays()`.
fn weekly_candidate_mondays(n: &PatternMatch) -> Vec<NaiveDate> {
    let Some(year) = n.year else {
        return Vec::new();
    };
    let weeks = match n.week {
        Some(week) => week..=week,
        None => 1..=53,
    };
    weeks
        .filter_map(|week| monday_of_iso_week(year, week))
        .filter(|monday| iso_week_parts(*monday).1 == year)
        .collect()
}

/// `monthlyCandidateDates()`.
fn monthly_candidate_dates(n: &PatternMatch) -> Vec<NaiveDate> {
    let Some(year) = n.year else {
        return Vec::new();
    };
    let first_month = n.month.map_or(0, |month| month - 1);
    let last_month = n.month.map_or(11, |month| month - 1);
    (first_month..=last_month)
        .filter_map(|month| js_make_date(year, month, 1))
        .collect()
}

/// `matchDailyPattern()`, `matchWeeklyPattern()`, `matchMonthlyPattern()`:
/// a candidate counts only when formatting it reproduces the very folder and
/// title.
fn match_pattern(
    kind: PeriodicKind,
    subpath: &str,
    title: &str,
    settings: &VaultSettings,
    pattern: &DatePattern,
) -> Option<NaiveDate> {
    let numerics = pattern_numerics(pattern, subpath, title)?;
    let candidates = match kind {
        PeriodicKind::Daily => daily_candidate_dates(&numerics),
        PeriodicKind::Weekly => weekly_candidate_mondays(&numerics),
        PeriodicKind::Monthly => monthly_candidate_dates(&numerics),
    };
    candidates.into_iter().find(|&candidate| {
        let expected = location_for_pattern(kind, candidate, settings, pattern);
        expected.subpath == subpath && expected.title == title
    })
}

/// `classifyDateNote(note, settings)` with the normalization and the pattern
/// lists done once instead of once per note (same result: upstream's are
/// pure).
struct Classifier {
    settings: VaultSettings,
    patterns: [Vec<DatePattern>; 3],
}

impl Classifier {
    fn new(settings: SettingsRef) -> Self {
        let settings = normalize_vault_settings(settings);
        let patterns = PeriodicKind::ALL.map(|kind| note_patterns(&settings, kind));
        Classifier { settings, patterns }
    }

    /// `DateNoteInfo`: the kind and the date it stands for (the Monday of a
    /// week, the first of a month).
    fn classify(&self, note: &NoteRef) -> Option<(PeriodicKind, NaiveDate)> {
        if note.folder != "inbox" {
            return None;
        }
        let subpath = note_folder_subpath(note.path, &self.settings);
        for (kind, patterns) in PeriodicKind::ALL.into_iter().zip(&self.patterns) {
            if !self.settings.periodic(kind).enabled {
                continue;
            }
            for pattern in patterns {
                if let Some(date) = match_pattern(kind, &subpath, note.title, &self.settings, pattern)
                {
                    return Some((kind, date));
                }
            }
        }
        None
    }
}

/// `findDailyNoteForDate()`, `findWeeklyNoteForDate()`,
/// `findMonthlyNoteForDate()`: the note where the current pattern puts it,
/// else any note of this kind standing for the same day, week or month.
fn find_note_for_date(
    kind: PeriodicKind,
    notes: &[NoteRef],
    settings: SettingsRef,
    date: NaiveDate,
) -> Option<usize> {
    let expected = location_for_date(kind, date, settings);
    let normalized = normalize_vault_settings(settings);
    // `classifyDateNote(note, normalized)` normalizes once more.
    let classifier = Classifier::new(SettingsRef::Normalized(&normalized));
    let key = date_key(kind, date);
    notes
        .iter()
        .position(|note| {
            note.folder == "inbox"
                && note.title == expected.title
                && note_folder_subpath(note.path, &normalized) == expected.subpath
                && classifier.classify(note).map(|(found, _)| found) == Some(kind)
        })
        .or_else(|| {
            notes.iter().position(|note| {
                matches!(classifier.classify(note),
                    Some((found, info_date)) if found == kind && date_key(kind, info_date) == key)
            })
        })
}

/* ---------- Template rendering ----------------------------------------- */

/// Every `{{cursor}}` variant is normalized to this, then all are removed.
const CURSOR_TOKEN: &str = "{{cursor}}";

/// `renderTemplate(body, { title, now }).body`.
fn render_template(body: &str, title: &str, now: NaiveDateTime) -> String {
    substitute_tokens(body, title, now).replace(CURSOR_TOKEN, "")
}

/// `substituteTokens()`: `input.replace(/\{\{\s*([^}]+?)\s*\}\}/g, …)`.
///
/// Nothing in a token can be `}`, so a token opened at `{{` closes at the
/// first `}` after it, and only if a second `}` follows and something lies in
/// between; the trimmed token is that in-between, trimmed. Where no token
/// starts, the regex retries one position later, which is what lets
/// `{{{title}}` read as the unknown token `{title`.
fn substitute_tokens(input: &str, title: &str, now: NaiveDateTime) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut done = 0;
    let mut search = 0;
    while let Some(offset) = input[search..].find("{{") {
        let start = search + offset;
        let content_start = start + 2;
        match input[content_start..].find('}') {
            Some(len) if len > 0 && bytes.get(content_start + len + 1) == Some(&b'}') => {
                let end = content_start + len;
                out.push_str(&input[done..start]);
                let full = &input[start..end + 2];
                let token = js_trim(&input[content_start..end]);
                match expand_token(token, title, now) {
                    Some(expanded) => out.push_str(&expanded),
                    None => out.push_str(full),
                }
                done = end + 2;
                search = done;
            }
            _ => search = start + 1,
        }
    }
    out.push_str(&input[done..]);
    out
}

/// One known token, or `None` to leave it as written.
fn expand_token(token: &str, title: &str, now: NaiveDateTime) -> Option<String> {
    Some(match token {
        "cursor" => CURSOR_TOKEN.to_string(),
        "title" => title.to_string(),
        "date" => note_title_for_date(now.date()),
        "time" => format!(
            "{}:{}",
            pad2(i64::from(now.hour())),
            pad2(i64::from(now.minute()))
        ),
        "week" => pad2(iso_week(now.date())),
        _ => format_date(now, token.strip_prefix("date:")?),
    })
}

/// `DATE_FORMAT_RE`'s token alternatives, in its order: the first that
/// matches wins, not the longest.
const DATE_FORMAT_TOKENS: [&str; 21] = [
    "YYYY", "YY", "yyyy", "yy", "MMMM", "MMM", "MM", "M", "EEEE", "EEE", "dddd", "ddd", "dd", "d",
    "DD", "D", "ww", "w", "HH", "mm", "ss",
];

/// `formatDate()`: `format.replace(/\[([^\]]*)\]|YYYY|…|ss/g, …)`. A `[`
/// with a `]` somewhere after it emits what lies between; a `[` without one
/// is plain text.
fn format_date(now: NaiveDateTime, format: &str) -> String {
    let date = now.date();
    let mut out = String::with_capacity(format.len());
    let mut rest = format;
    while let Some(ch) = rest.chars().next() {
        if ch == '[' {
            if let Some(close) = rest[1..].find(']') {
                out.push_str(&rest[1..1 + close]);
                rest = &rest[close + 2..];
                continue;
            }
        }
        if let Some(token) = DATE_FORMAT_TOKENS.iter().find(|token| rest.starts_with(*token)) {
            let year = i64::from(date.year());
            match *token {
                "YYYY" | "yyyy" => out.push_str(&year.to_string()),
                "YY" | "yy" => out.push_str(&pad2(year % 100)),
                "MMMM" => out.push_str(locale_name(date, None, NamePart::MonthLong)),
                "MMM" => out.push_str(locale_name(date, None, NamePart::MonthShort)),
                "MM" => out.push_str(&pad2(i64::from(date.month()))),
                "M" => out.push_str(&date.month().to_string()),
                "DD" | "dd" => out.push_str(&pad2(i64::from(date.day()))),
                "D" | "d" => out.push_str(&date.day().to_string()),
                "dddd" | "EEEE" => out.push_str(locale_name(date, None, NamePart::WeekdayLong)),
                "ddd" | "EEE" => out.push_str(locale_name(date, None, NamePart::WeekdayShort)),
                "ww" => out.push_str(&pad2(iso_week(date))),
                "w" => out.push_str(&iso_week(date).to_string()),
                "HH" => out.push_str(&pad2(i64::from(now.hour()))),
                "mm" => out.push_str(&pad2(i64::from(now.minute()))),
                _ => out.push_str(&pad2(i64::from(now.second()))), // ss
            }
            rest = &rest[token.len()..];
            continue;
        }
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

/* ---------- Templates -------------------------------------------------- */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Engineering,
    Personal,
    Custom,
}

impl Category {
    /// `normalizeCategory()`.
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("Engineering") => Category::Engineering,
            Some("Personal") => Category::Personal,
            _ => Category::Custom,
        }
    }

    /// Position in `CATEGORY_ORDER`.
    fn rank(self) -> u8 {
        match self {
            Category::Engineering => 0,
            Category::Personal => 1,
            Category::Custom => 2,
        }
    }
}

/// The `NoteTemplate` fields finding and rendering one reads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Template {
    id: String,
    name: String,
    category: Category,
    body: String,
    builtin: bool,
    source_path: Option<String>,
    builtin_id: Option<String>,
}

struct Builtin {
    id: &'static str,
    name: &'static str,
    category: Category,
    body: &'static str,
}

/// `BUILTIN_TEMPLATES`, generated from the upstream module so every body is
/// byte for byte the same.
const BUILTIN_TEMPLATES: [Builtin; 12] = [
    Builtin {
        id: "builtin.adr",
        name: "ADR",
        category: Category::Engineering,
        body: "# {{title}}\n\n- **Status:** Proposed\n- **Date:** {{date}}\n- **Deciders:**\n\n## Context\n\n{{cursor}}\n\n## Decision\n\n## Consequences\n\n### Positive\n\n### Negative\n\n## Related\n\n",
    },
    Builtin {
        id: "builtin.rfc",
        name: "RFC / Design Doc",
        category: Category::Engineering,
        body: "# {{title}}\n\n- **Author:**\n- **Status:** Draft\n- **Date:** {{date}}\n\n## Summary\n\n{{cursor}}\n\n## Motivation\n\n## Proposal\n\n## Alternatives considered\n\n## Rollout & risks\n\n## Related\n\n",
    },
    Builtin {
        id: "builtin.bug",
        name: "Bug Report",
        category: Category::Engineering,
        body: "# {{title}}\n\n- **Date:** {{date}}\n- **Severity:**\n- **Environment:**\n\n## Steps to reproduce\n\n1. {{cursor}}\n\n## Expected\n\n## Actual\n\n## Notes\n\n",
    },
    Builtin {
        id: "builtin.postmortem",
        name: "Postmortem",
        category: Category::Engineering,
        body: "# {{title}}\n\n- **Date:** {{date}}\n- **Authors:**\n- **Impact:**\n\n## Summary\n\n{{cursor}}\n\n## Timeline\n\n## Root cause\n\n## Resolution\n\n## Action items\n\n- [ ]\n\n## Related\n\n",
    },
    Builtin {
        id: "builtin.meeting",
        name: "Meeting Notes",
        category: Category::Engineering,
        body: "# {{title}}\n\n- **Date:** {{date}}\n- **Attendees:**\n\n## Agenda\n\n- {{cursor}}\n\n## Notes\n\n## Decisions\n\n## Action items\n\n- [ ]\n\n",
    },
    Builtin {
        id: "builtin.oneonone",
        name: "1:1",
        category: Category::Engineering,
        body: "# {{title}}\n\n- **Date:** {{date}}\n\n## Wins\n\n{{cursor}}\n\n## Challenges & blockers\n\n## Growth & feedback\n\n## Follow-ups\n\n- [ ]\n\n",
    },
    Builtin {
        id: "builtin.daily",
        name: "Daily Note",
        category: Category::Personal,
        body: "# {{date:dddd, MMMM D, YYYY}}\n\n## Focus\n\n- {{cursor}}\n\n## Schedule\n\n## Notes\n\n## Tasks\n\n- [ ]\n\n## Log\n\n",
    },
    Builtin {
        id: "builtin.weekly",
        name: "Weekly Review",
        category: Category::Personal,
        body: "# Week {{week}}, {{date:YYYY}}\n\n## Last week: review\n\n- {{cursor}}\n\n## Wins\n\n## This week: plan\n\n- [ ]\n\n## Carry-overs\n\n## Notes\n\n## Related\n\n",
    },
    Builtin {
        id: "builtin.reading",
        name: "Reading Notes",
        category: Category::Personal,
        body: "# {{title}}\n\n- **Author:**\n- **Started:** {{date}}\n- **Status:** Reading\n\n## Key ideas\n\n- {{cursor}}\n\n## Quotes\n\n>\n\n## Takeaways\n\n## Related\n\n",
    },
    Builtin {
        id: "builtin.journal",
        name: "Journal",
        category: Category::Personal,
        body: "# {{date:dddd, MMMM D, YYYY}}\n\n{{cursor}}\n\n",
    },
    Builtin {
        id: "builtin.kickoff",
        name: "Project Kickoff",
        category: Category::Personal,
        body: "# {{title}}\n\n- **Date:** {{date}}\n- **Owner:**\n\n## Goals\n\n- {{cursor}}\n\n## Scope\n\n### In scope\n\n### Out of scope\n\n## Milestones\n\n## Stakeholders\n\n## Risks\n\n## Related\n\n",
    },
    Builtin {
        id: "builtin.todo",
        name: "To-do",
        category: Category::Personal,
        body: "# {{title}}\n\n- [ ] {{cursor}}\n- [ ]\n- [ ]\n\n",
    },
];

fn builtin_templates() -> Vec<Template> {
    BUILTIN_TEMPLATES
        .iter()
        .map(|b| Template {
            id: b.id.to_string(),
            name: b.name.to_string(),
            category: b.category,
            body: b.body.to_string(),
            builtin: true,
            source_path: None,
            builtin_id: None,
        })
        .collect()
}

/// `parseFrontmatter()`: flat `key: value` scalars from a leading fence, and
/// the body after it. Values lose one pair of matching quotes.
fn parse_frontmatter(raw: &str) -> (HashMap<String, String>, &str) {
    let Some((inner, end)) = frontmatter_span(raw) else {
        return (HashMap::new(), raw);
    };
    let mut data = HashMap::new();
    for line in split_crlf_lines(inner) {
        let Some(idx) = line.find(':') else {
            continue;
        };
        let key = js_trim(&line[..idx]);
        if key.is_empty() {
            continue;
        }
        let mut value = js_trim(&line[idx + 1..]);
        if (value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\''))
        {
            // value.slice(1, -1); a lone quote leaves ''.
            value = if value.len() >= 2 { &value[1..value.len() - 1] } else { "" };
        }
        data.insert(key.to_string(), value.to_string());
    }
    (data, &raw[end..])
}

/// `FRONTMATTER_RE = /^---\r?\n([\s\S]*?)\r?\n---\r?\n?/`: the fence's
/// contents and where the match ends. The lazy group stops at the first
/// `\r?\n---`, whatever follows the dashes.
fn frontmatter_span(raw: &str) -> Option<(&str, usize)> {
    let start = if raw.starts_with("---\n") {
        4
    } else if raw.starts_with("---\r\n") {
        5
    } else {
        return None;
    };
    let mut k = start;
    loop {
        let rest = &raw[k..];
        let close = if rest.starts_with("\r\n---") {
            Some(k + 5)
        } else if rest.starts_with("\n---") {
            Some(k + 4)
        } else {
            None
        };
        if let Some(mut end) = close {
            if raw[end..].starts_with('\r') {
                end += 1;
            }
            if raw[end..].starts_with('\n') {
                end += 1;
            }
            return Some((&raw[start..k], end));
        }
        k += rest.chars().next()?.len_utf8();
    }
}

/// `s.split(/\r?\n/)`.
fn split_crlf_lines(s: &str) -> Vec<&str> {
    let pieces: Vec<&str> = s.split('\n').collect();
    let last = pieces.len() - 1;
    pieces
        .into_iter()
        .enumerate()
        .map(|(i, piece)| {
            if i < last {
                piece.strip_suffix('\r').unwrap_or(piece)
            } else {
                piece
            }
        })
        .collect()
}

/// `filenameStem()`: the last path segment without a `.md` of any case.
fn filename_stem(source_path: &str) -> &str {
    let file = source_path.rsplit('/').next().unwrap_or(source_path);
    let b = file.as_bytes();
    if b.len() >= 3 && b[b.len() - 3..].eq_ignore_ascii_case(b".md") {
        &file[..file.len() - 3]
    } else {
        file
    }
}

/// `parseCustomTemplate()`, the fields [`Template`] keeps.
fn parse_custom_template(raw: &str, source_path: &str) -> Template {
    let (data, body) = parse_frontmatter(raw);
    let stem = filename_stem(source_path);
    let field = |key: &str| {
        data.get(key)
            .map(|value| js_trim(value))
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Template {
        id: format!("custom:{stem}"),
        name: field("name").unwrap_or_else(|| stem.to_string()),
        category: Category::parse(data.get("category").map(String::as_str)),
        body: body.to_string(),
        builtin: false,
        source_path: Some(source_path.to_string()),
        builtin_id: field("builtinId"),
    }
}

/// `mergeTemplates()`: a custom template carrying `builtinId` replaces that
/// built-in; then category, built-ins (and edited copies) first, name, source
/// and path. `sort_by` is stable, as V8's sort is.
fn merge_templates(builtins: Vec<Template>, custom: Vec<Template>) -> Vec<Template> {
    let overridden: HashSet<String> = custom.iter().filter_map(|c| c.builtin_id.clone()).collect();
    let mut all: Vec<Template> = builtins
        .into_iter()
        .filter(|b| !overridden.contains(&b.id))
        .collect();
    all.extend(custom);
    all.sort_by(compare_templates);
    all
}

fn compare_templates(a: &Template, b: &Template) -> Ordering {
    if a.category != b.category {
        return a.category.rank().cmp(&b.category.rank());
    }
    let primary = |t: &Template| t.builtin || t.builtin_id.is_some();
    if primary(a) != primary(b) {
        return if primary(a) { Ordering::Less } else { Ordering::Greater };
    }
    // SOURCE_ORDER: parseCustomTemplate sets no `source`, so `builtin` decides.
    let source = |t: &Template| u8::from(!t.builtin);
    locale_compare(&a.name, &b.name)
        .then_with(|| source(a).cmp(&source(b)))
        .then_with(|| {
            locale_compare(
                a.source_path.as_deref().unwrap_or(""),
                b.source_path.as_deref().unwrap_or(""),
            )
        })
}

/// `findTemplate()`: the template with the id, else the edited copy of the
/// built-in it names.
fn find_template<'t>(templates: &'t [Template], id: Option<&str>) -> Option<&'t Template> {
    let id = id.filter(|id| !id.is_empty())?;
    templates
        .iter()
        .find(|t| t.id == id)
        .or_else(|| templates.iter().find(|t| t.builtin_id.as_deref() == Some(id)))
}

/// ICU root collation's order of ASCII punctuation and symbols (what
/// `localeCompare` sorts them by), after whitespace and before digits.
const COLLATION_PUNCTUATION: &str = "_-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$";

/// Primary weight under [`locale_compare`]; `None` is ignorable.
fn collation_primary(c: char) -> Option<u32> {
    Some(match c {
        '\t' => 1,
        '\n' => 2,
        '\u{0B}' => 3,
        '\u{0C}' => 4,
        '\r' => 5,
        '\u{85}' => 6,
        '\u{2028}' => 7,
        '\u{2029}' => 8,
        ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}'
        | '\u{3000}' => 9,
        '\u{0}'..='\u{1F}' | '\u{7F}'..='\u{9F}' => return None,
        '0'..='9' => 200 + (c as u32 - '0' as u32),
        'a'..='z' => 300 + (c as u32 - 'a' as u32),
        'A'..='Z' => 300 + (c as u32 - 'A' as u32),
        _ => match COLLATION_PUNCTUATION.find(c) {
            Some(index) => 100 + index as u32,
            None => 1000 + c.to_lowercase().next().map_or(c as u32, |l| l as u32),
        },
    })
}

/// `a.localeCompare(b)`, approximated: ICU root order for ASCII (whitespace,
/// punctuation and symbols, digits, then letters, case ignored), lowercase
/// before uppercase when nothing else differs, control characters ignored.
/// Other characters sort by code point after the letters, where ICU would
/// put an accented letter beside its base letter. Only ties between
/// templates of one id reach this (documented deviation).
fn locale_compare(a: &str, b: &str) -> Ordering {
    let key = |s: &str| -> Vec<(u32, bool)> {
        s.chars()
            .filter_map(|c| collation_primary(c).map(|weight| (weight, c.is_uppercase())))
            .collect()
    };
    let (ka, kb) = (key(a), key(b));
    ka.iter()
        .map(|k| k.0)
        .cmp(kb.iter().map(|k| k.0))
        .then_with(|| ka.iter().map(|k| k.1).cmp(kb.iter().map(|k| k.1)))
}

#[cfg(test)]
mod tests {
    //! Expected values come from the upstream tests named in each section,
    //! or were printed by running the upstream modules under bun.

    use super::*;
    use serde_json::json;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn dt(y: i32, m: u32, day: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
        d(y, m, day).and_hms_opt(h, mi, s).unwrap()
    }

    fn inbox_note<'a>(path: &'a str, title: &'a str) -> NoteRef<'a> {
        NoteRef { path, title, folder: "inbox" }
    }

    /// `xNoteLocationForDate(date, settings)` with stored settings, as the
    /// upstream tests call it: `(title, subpath)`.
    fn loc(kind: PeriodicKind, date: NaiveDate, settings: &Value) -> (String, String) {
        let location =
            location_for_date(kind, date, SettingsRef::Json { value: settings, primary: None });
        (location.title, location.subpath)
    }

    /// `classifyDateNote(note, settings)` with stored settings.
    fn classify(note: NoteRef, settings: &Value) -> Option<(PeriodicKind, NaiveDate)> {
        Classifier::new(SettingsRef::Json { value: settings, primary: None }).classify(&note)
    }

    fn owned(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    /* ----- dailyNote(): the plan ----- */

    #[test]
    fn plans_default_daily_note_in_an_inbox_vault() {
        let plan = plan_daily_note(PeriodicKind::Daily, d(2026, 11, 14), &json!({}), "inbox");
        assert_eq!(
            plan,
            DailyNotePlan {
                kind: "daily",
                date: "2026-11-14".into(),
                enabled: false,
                subpath: "Daily Notes".into(),
                title: "2026-11-14".into(),
                rel: "inbox/Daily Notes/2026-11-14.md".into(),
                template_id: None,
            }
        );
    }

    #[test]
    fn plans_at_the_vault_root_in_a_root_vault() {
        let plan = plan_daily_note(PeriodicKind::Daily, d(2026, 11, 14), &json!({}), "root");
        assert_eq!(plan.rel, "Daily Notes/2026-11-14.md");
        // The caller's primary replaces the stored one.
        let stored = json!({ "primaryNotesLocation": "root" });
        let plan = plan_daily_note(PeriodicKind::Daily, d(2026, 11, 14), &stored, "inbox");
        assert_eq!(plan.rel, "inbox/Daily Notes/2026-11-14.md");
        // A directory that names the inbox is only stripped in an Inbox vault.
        let s = json!({ "dailyNotes": { "directory": "inbox/Journal" } });
        assert_eq!(
            plan_daily_note(PeriodicKind::Daily, d(2026, 11, 14), &s, "inbox").rel,
            "inbox/Journal/2026-11-14.md"
        );
        assert_eq!(
            plan_daily_note(PeriodicKind::Daily, d(2026, 11, 14), &s, "root").rel,
            "inbox/Journal/2026-11-14.md"
        );
    }

    #[test]
    fn plans_weekly_notes_by_iso_week_across_year_boundaries() {
        let weekly = |date| plan_daily_note(PeriodicKind::Weekly, date, &json!({}), "inbox");
        let plan = weekly(d(2026, 1, 1));
        assert_eq!(plan.kind, "weekly");
        assert_eq!(plan.date, "2026-01-01");
        assert_eq!(plan.title, "2026-W01");
        assert_eq!(plan.rel, "inbox/Weekly Notes/2026-W01.md");
        assert_eq!(weekly(d(2027, 1, 1)).title, "2026-W53");
        assert_eq!(weekly(d(2024, 12, 30)).title, "2025-W01");
        assert_eq!(weekly(d(2021, 1, 1)).title, "2020-W53");
        // Date tokens of a weekly pattern format its Monday (2026-12-28), and
        // `yy` its ISO week-year.
        let s = json!({ "weeklyNotes": { "directory": "yyyy/MMMM", "titlePattern": "yy-'W'ww EEE MMM d" } });
        let plan = plan_daily_note(PeriodicKind::Weekly, d(2027, 1, 3), &s, "inbox");
        assert_eq!(plan.rel, "inbox/2026/December/26-W53 Mon Dec 28.md");
    }

    #[test]
    fn plans_monthly_notes_from_the_first_of_the_month() {
        let plan = plan_daily_note(PeriodicKind::Monthly, d(2026, 7, 21), &json!({}), "inbox");
        assert_eq!(plan.kind, "monthly");
        assert_eq!(plan.date, "2026-07-21");
        assert_eq!(plan.rel, "inbox/Monthly Notes/2026-07.md");
        // Two tokens make a directory a pattern, however unlikely.
        let s = json!({ "monthlyNotes": { "directory": "Monthly diary" } });
        let plan = plan_daily_note(PeriodicKind::Monthly, d(2026, 7, 21), &s, "inbox");
        assert_eq!(plan.subpath, "7onthly 1iary");
    }

    #[test]
    fn plans_custom_patterns() {
        let plan = |s: Value| plan_daily_note(PeriodicKind::Daily, d(2026, 9, 9), &s, "inbox");
        let p = plan(json!({ "dailyNotes": { "directory": " /Journal/ ", "titlePattern": "dd.MM.yy" } }));
        assert_eq!((p.subpath.as_str(), p.title.as_str()), ("Journal", "09.09.26"));
        let p = plan(json!({ "dailyNotes": { "directory": "'Log'/yyyy/MMMM", "titlePattern": "EEEE, MMMM d, yyyy" } }));
        assert_eq!(p.rel, "inbox/Log/2026/September/Wednesday, September 9, 2026.md");
        let p = plan(json!({ "dailyNotes": { "directory": "'It''s'", "titlePattern": "''yyyy''" } }));
        assert_eq!(p.rel, "inbox/It's/'2026'.md");
        // Slashes in a title pattern become dashes.
        let p = plan(json!({ "dailyNotes": { "titlePattern": "yyyy/MM\\\\dd" } }));
        assert_eq!(p.title, "2026-09-09");
        let p = plan(json!({ "dailyNotes": { "titlePattern": "'Week' w, yyyy" } }));
        assert_eq!(p.title, "Week 37, 2026");
    }

    #[test]
    fn replays_every_normalization_pass() {
        let plan = |dir: &str, primary| {
            let s = json!({ "dailyNotes": { "directory": dir } });
            plan_daily_note(PeriodicKind::Daily, d(2026, 11, 14), &s, primary).subpath
        };
        // Pass 1 leaves `inbox`, pass 2 makes it '', the location the default.
        assert_eq!(plan("inbox/inbox", "inbox"), "Daily Notes");
        assert_eq!(plan("inbox/inbox/x", "inbox"), "x");
        assert_eq!(plan("inbox/inbox", "root"), "inbox/inbox");
        assert_eq!(plan("inbox", "inbox"), "Daily Notes");
    }

    #[test]
    fn follows_a_remapped_inbox() {
        let plan = |paths: Value| {
            let s = json!({ "systemFolderPaths": paths });
            plan_daily_note(PeriodicKind::Daily, d(2026, 11, 14), &s, "inbox").rel
        };
        assert_eq!(plan(json!({ "inbox": " 01 - Entry " })), "01 - Entry/Daily Notes/2026-11-14.md");
        for rejected in [
            json!({ "inbox": "Notes", "quick": "notes" }),
            json!({ "inbox": "Archive" }),
            json!({ "inbox": "Assets" }),
            json!({ "inbox": "a/b" }),
            json!({ "inbox": ".hidden" }),
            json!(["x"]),
        ] {
            assert_eq!(plan(rejected), "inbox/Daily Notes/2026-11-14.md");
        }
        let s = json!({ "systemFolderPaths": { "inbox": "01 - Entry" } });
        assert_eq!(
            plan_daily_note(PeriodicKind::Daily, d(2026, 11, 14), &s, "root").rel,
            "Daily Notes/2026-11-14.md"
        );
    }

    #[test]
    fn reads_enabled_and_template_id_as_js_does() {
        let s = json!({
            "dailyNotes": { "enabled": "yes", "templateId": " builtin.daily " },
            "weeklyNotes": { "enabled": 0, "templateId": "  " },
            "monthlyNotes": "not an object"
        });
        let daily = plan_daily_note(PeriodicKind::Daily, d(2026, 11, 14), &s, "inbox");
        assert!(daily.enabled);
        assert_eq!(daily.template_id.as_deref(), Some("builtin.daily"));
        let weekly = plan_daily_note(PeriodicKind::Weekly, d(2026, 11, 14), &s, "inbox");
        assert!(!weekly.enabled);
        assert_eq!(weekly.template_id, None);
        let monthly = plan_daily_note(PeriodicKind::Monthly, d(2026, 11, 14), &s, "inbox");
        assert_eq!(monthly.rel, "inbox/Monthly Notes/2026-11.md");
        // Settings that are not an object read as `{}`.
        let plan = plan_daily_note(PeriodicKind::Daily, d(2026, 11, 14), &json!([1]), "inbox");
        assert_eq!(plan.rel, "inbox/Daily Notes/2026-11-14.md");
    }

    #[test]
    fn parses_kind_arguments() {
        assert_eq!(PeriodicKind::from_arg(""), Some(PeriodicKind::Daily));
        assert_eq!(PeriodicKind::from_arg("weekly"), Some(PeriodicKind::Weekly));
        assert_eq!(PeriodicKind::from_arg("monthly").map(PeriodicKind::as_str), Some("monthly"));
        assert_eq!(PeriodicKind::from_arg("Daily"), None);
    }

    /* ----- dailyNote(): the lookup ----- */

    #[test]
    fn finds_the_note_at_the_exact_path_first() {
        let notes = [
            inbox_note("inbox/Daily Notes/2026-11-13.md", "2026-11-13"),
            inbox_note("inbox/Daily Notes/2026-11-14.md", "2026-11-14"),
        ];
        let found = find_daily_note(PeriodicKind::Daily, d(2026, 11, 14), &json!({}), "inbox", &notes);
        assert_eq!(found, Some(1));
        let found = find_daily_note(PeriodicKind::Daily, d(2026, 11, 12), &json!({}), "inbox", &notes);
        assert_eq!(found, None);
    }

    #[test]
    fn falls_back_to_an_older_pattern_even_when_the_kind_is_off() {
        let s = json!({ "dailyNotes": {
            "enabled": false,
            "directory": "yyyy/MM-MMM",
            "titlePattern": "yyyy-MM-dd-EEE",
            "legacyPatterns": [{ "directory": "Daily Notes", "titlePattern": "yyyy-MM-dd" }]
        } });
        let plan = plan_daily_note(PeriodicKind::Daily, d(2026, 6, 12), &s, "inbox");
        assert_eq!(plan.rel, "inbox/2026/06-Jun/2026-06-12-Fri.md");
        let notes = [
            inbox_note("inbox/Daily Notes/2026-06-11.md", "2026-06-11"),
            inbox_note("inbox/Daily Notes/2026-06-12.md", "2026-06-12"),
        ];
        let found = find_daily_note(PeriodicKind::Daily, d(2026, 6, 12), &s, "inbox", &notes);
        assert_eq!(found, Some(1));
        // Outside the inbox it is not a daily note.
        let archived = [NoteRef { folder: "archive", ..notes[1] }];
        assert_eq!(find_daily_note(PeriodicKind::Daily, d(2026, 6, 12), &s, "inbox", &archived), None);
        // A weekly lookup does not take a daily note.
        let weekly = [inbox_note("inbox/Weekly Notes/2026-06-12.md", "2026-06-12")];
        assert_eq!(find_daily_note(PeriodicKind::Weekly, d(2026, 6, 12), &s, "inbox", &weekly), None);
    }

    #[test]
    fn prefers_the_current_location_over_an_older_one() {
        let s = json!({ "dailyNotes": {
            "enabled": true,
            "legacyPatterns": [{ "directory": "Journal", "titlePattern": "dd.MM.yyyy" }]
        } });
        // `Inbox/` misses the exact path, but the inbox prefix matches in any
        // case, so the second note is where the current pattern puts it.
        let notes = [
            inbox_note("inbox/Journal/14.11.2026.md", "14.11.2026"),
            inbox_note("Inbox/Daily Notes/2026-11-14.md", "2026-11-14"),
        ];
        let found = find_daily_note(PeriodicKind::Daily, d(2026, 11, 14), &s, "inbox", &notes);
        assert_eq!(found, Some(1));
        assert_eq!(find_daily_note(PeriodicKind::Daily, d(2026, 11, 14), &s, "inbox", &notes[..1]), Some(0));
    }

    #[test]
    fn finds_weekly_and_monthly_notes_by_their_period() {
        let s = json!({ "weeklyNotes": { "legacyPatterns": [{ "directory": "Weeks", "titlePattern": "yyyy-ww" }] } });
        let notes = [inbox_note("inbox/Weeks/2026-53.md", "2026-53")];
        // 2027-01-01 is in ISO week 53 of 2026.
        assert_eq!(find_daily_note(PeriodicKind::Weekly, d(2027, 1, 1), &s, "inbox", &notes), Some(0));
        assert_eq!(find_daily_note(PeriodicKind::Weekly, d(2027, 1, 4), &s, "inbox", &notes), None);
        let s = json!({ "monthlyNotes": { "directory": "yyyy", "titlePattern": "MMMM" } });
        let notes = [inbox_note("2026/July.md", "July")];
        assert_eq!(find_daily_note(PeriodicKind::Monthly, d(2026, 7, 21), &s, "root", &notes), Some(0));
        assert_eq!(find_daily_note(PeriodicKind::Monthly, d(2026, 8, 1), &s, "root", &notes), None);
    }

    /* ----- periodicBody() ----- */

    #[test]
    fn renders_the_builtin_templates() {
        let at = dt(2026, 11, 14, 9, 5, 7);
        assert_eq!(
            periodic_body(Some("builtin.daily"), Some(&[]), "2026-11-14", at),
            "# Saturday, November 14, 2026\n\n## Focus\n\n- \n\n## Schedule\n\n## Notes\n\n## Tasks\n\n- [ ]\n\n## Log\n\n"
        );
        assert_eq!(
            periodic_body(Some("builtin.weekly"), Some(&[]), "2026-W46", at),
            "# Week 46, 2026\n\n## Last week: review\n\n- \n\n## Wins\n\n## This week: plan\n\n- [ ]\n\n## Carry-overs\n\n## Notes\n\n## Related\n\n"
        );
    }

    #[test]
    fn renders_a_custom_template_with_placeholders() {
        let custom = owned(&[(
            ".zennotes/templates/mine.md",
            "---\nname: Mine\n---\n# {{ title }}\n{{date}} {{time}} week {{week}} {{date:ddd D MMM YY [at] HH:mm:ss}} {{unknown}} {{{title}}}\n{{cursor}}done\n",
        )]);
        assert_eq!(
            periodic_body(Some("custom:mine"), Some(&custom), "Hello", dt(2026, 11, 14, 9, 5, 7)),
            "# Hello\n2026-11-14 09:05 week 46 Sat 14 Nov 26 at 09:05:07 {{unknown}} {{{title}}}\ndone\n"
        );
    }

    #[test]
    fn reaches_an_edited_builtin_through_its_id() {
        let at = dt(2026, 11, 14, 9, 5, 7);
        let custom = owned(&[(
            ".zennotes/templates/daily-note.md",
            "---\nname: Daily Note\ncategory: Personal\nbuiltinId: builtin.daily\n---\n# {{date:EEEE}}\n\n## Today\n",
        )]);
        assert_eq!(periodic_body(Some("builtin.daily"), Some(&custom), "x", at), "# Saturday\n\n## Today\n");
        // Two edited copies of one built-in: the sort order decides.
        let custom = owned(&[
            (".zennotes/templates/b.md", "---\nname: Zed\ncategory: Personal\nbuiltinId: builtin.daily\n---\nZed\n"),
            (".zennotes/templates/a.md", "---\nname: alpha\ncategory: Personal\nbuiltinId: builtin.daily\n---\nalpha\n"),
        ]);
        assert_eq!(periodic_body(Some("builtin.daily"), Some(&custom), "x", at), "alpha\n");
    }

    #[test]
    fn falls_back_to_a_heading() {
        let at = dt(2026, 11, 14, 9, 5, 7);
        assert_eq!(periodic_body(Some("custom:gone"), Some(&[]), "T", at), "# T\n\n");
        assert_eq!(periodic_body(None, Some(&[]), "T", at), "# T\n\n");
        assert_eq!(periodic_body(Some(""), Some(&[]), "T", at), "# T\n\n");
        // Templates that could not be listed leave the heading, built-ins too.
        assert_eq!(periodic_body(Some("builtin.daily"), None, "T", at), "# T\n\n");
    }

    /* ----- resolveDate() ----- */

    #[test]
    fn resolves_dates_like_the_handler() {
        let now = dt(2026, 11, 14, 9, 30, 5);
        let ok = |raw: &str| resolve_date(raw, now).unwrap();
        assert_eq!(ok(""), now);
        assert_eq!(ok(" TODAY "), now);
        assert_eq!(ok("Yesterday"), dt(2026, 11, 13, 9, 30, 5));
        assert_eq!(ok("tomorrow\n"), dt(2026, 11, 15, 9, 30, 5));
        assert_eq!(ok("2024-02-29"), dt(2024, 2, 29, 0, 0, 0));
        assert_eq!(ok("0100-03-01"), dt(100, 3, 1, 0, 0, 0));
        for bad in ["2026-02-29", "0050-01-01", "2026-11-14 ", "2026-1-14", "2026-13-01", "２０２６-11-14", "soon"] {
            assert_eq!(resolve_date(bad, now), Err(DATE_ERROR.to_string()), "{bad:?}");
        }
    }

    /* ----- vault-layout.test.ts: classifyDateNote and locations ----- */

    fn two_kinds(daily_directory: &str, weekly_directory: &str) -> Value {
        json!({
            "primaryNotesLocation": "inbox",
            "dailyNotes": { "enabled": true, "directory": daily_directory },
            "weeklyNotes": { "enabled": true, "directory": weekly_directory },
            "monthlyNotes": { "enabled": false, "directory": "Monthly Notes" }
        })
    }

    fn one_kind(key: &str, block: Value) -> Value {
        let mut s = json!({
            "primaryNotesLocation": "inbox",
            "dailyNotes": { "enabled": false, "directory": "Daily Notes" },
            "weeklyNotes": { "enabled": false, "directory": "Weekly Notes" },
            "monthlyNotes": { "enabled": false, "directory": "Monthly Notes" }
        });
        s[key] = block;
        s
    }

    const DAILY: PeriodicKind = PeriodicKind::Daily;
    const WEEKLY: PeriodicKind = PeriodicKind::Weekly;
    const MONTHLY: PeriodicKind = PeriodicKind::Monthly;

    #[test]
    fn recognizes_notes_under_a_directory_with_the_inbox_prefix() {
        let s = two_kinds("inbox/Journal", "Weekly Notes");
        let note = inbox_note("inbox/Journal/2026-06-12.md", "2026-06-12");
        assert_eq!(classify(note, &s), Some((DAILY, d(2026, 6, 12))));
        let s = two_kinds("Daily Notes", "inbox/Weeks");
        let note = inbox_note("inbox/Weeks/2026-W24.md", "2026-W24");
        assert_eq!(classify(note, &s), Some((WEEKLY, d(2026, 6, 8))));
    }

    #[test]
    fn recognizes_notes_from_date_based_patterns() {
        let s = one_kind("weeklyNotes", json!({ "enabled": true, "directory": "yyyy/MM-MMM", "titlePattern": "yyyy-'W'ww-EEE", "locale": "en-US" }));
        let note = inbox_note("inbox/2026/06-Jun/2026-W24-Mon.md", "2026-W24-Mon");
        assert_eq!(classify(note, &s), Some((WEEKLY, d(2026, 6, 8))));
        let s = one_kind("dailyNotes", json!({ "enabled": true, "directory": "yyyy/MM-MMM", "titlePattern": "yyyy-MM-dd-EEE", "locale": "en-US" }));
        let note = inbox_note("inbox/2026/06-Jun/2026-06-09-Tue.md", "2026-06-09-Tue");
        assert_eq!(classify(note, &s), Some((DAILY, d(2026, 6, 9))));
        assert_eq!(loc(DAILY, d(2026, 6, 9), &s), ("2026-06-09-Tue".into(), "2026/06-Jun".into()));
    }

    #[test]
    fn keeps_lowercase_directories_literal() {
        let s = two_kinds("daily", "Weekly Notes");
        assert_eq!(loc(DAILY, d(2026, 6, 9), &s), ("2026-06-09".into(), "daily".into()));
        let note = inbox_note("inbox/daily/2026-06-09.md", "2026-06-09");
        assert_eq!(classify(note, &s), Some((DAILY, d(2026, 6, 9))));
        let s = one_kind("weeklyNotes", json!({ "enabled": true, "directory": "week", "titlePattern": "yyyy-'W'ww", "locale": "en-US" }));
        assert_eq!(loc(WEEKLY, d(2026, 6, 9), &s), ("2026-W24".into(), "week".into()));
        let note = inbox_note("inbox/week/2026-W24.md", "2026-W24");
        assert_eq!(classify(note, &s), Some((WEEKLY, d(2026, 6, 8))));
    }

    #[test]
    fn supports_month_only_daily_directories() {
        let s = one_kind("dailyNotes", json!({ "enabled": true, "directory": "MM-MMM", "titlePattern": "yyyy-MM-dd", "locale": "en-US" }));
        assert_eq!(loc(DAILY, d(2026, 6, 9), &s), ("2026-06-09".into(), "06-Jun".into()));
        let note = inbox_note("inbox/06-Jun/2026-06-09.md", "2026-06-09");
        assert_eq!(classify(note, &s), Some((DAILY, d(2026, 6, 9))));
    }

    #[test]
    fn renders_quoted_literals_in_directories() {
        let s = one_kind("dailyNotes", json!({ "enabled": true, "directory": "'Daily Notes'/yyyy/MM-MMM", "titlePattern": "yyyy-MM-dd-EEE", "locale": "en-US" }));
        assert_eq!(loc(DAILY, d(2026, 6, 9), &s), ("2026-06-09-Tue".into(), "Daily Notes/2026/06-Jun".into()));
        let s = one_kind("weeklyNotes", json!({ "enabled": true, "directory": "'Weekly Notes'/yyyy/MM-MMM", "titlePattern": "yyyy-'W'ww-EEE", "locale": "en-US" }));
        assert_eq!(loc(WEEKLY, d(2026, 6, 9), &s), ("2026-W24-Mon".into(), "Weekly Notes/2026/06-Jun".into()));
    }

    #[test]
    fn renders_monthly_locations() {
        let s = one_kind("monthlyNotes", json!({ "enabled": true, "directory": "Monthly Notes", "titlePattern": "yyyy-MM", "locale": "en-US" }));
        assert_eq!(loc(MONTHLY, d(2026, 7, 21), &s), ("2026-07".into(), "Monthly Notes".into()));
        let s = one_kind("monthlyNotes", json!({ "enabled": true, "directory": "yyyy/MM-MMM", "titlePattern": "yyyy-'M'MM", "locale": "en-US" }));
        assert_eq!(loc(MONTHLY, d(2026, 7, 21), &s), ("2026-M07".into(), "2026/07-Jul".into()));
    }

    #[test]
    fn classifies_a_monthly_note_only_inside_its_folder() {
        let s = one_kind("monthlyNotes", json!({ "enabled": true, "directory": "Monthly Notes", "titlePattern": "yyyy-MM", "locale": "system" }));
        let note = inbox_note("inbox/Monthly Notes/2026-07.md", "2026-07");
        assert_eq!(classify(note, &s), Some((MONTHLY, d(2026, 7, 1))));
        assert_eq!(classify(inbox_note("inbox/Random/2026-07.md", "2026-07"), &s), None);
    }

    #[test]
    fn uses_the_iso_week_year_and_week_only_directories() {
        let s = one_kind("weeklyNotes", json!({ "enabled": true, "directory": "yyyy", "titlePattern": "yyyy-'W'ww", "locale": "en-US" }));
        assert_eq!(loc(WEEKLY, d(2021, 1, 1), &s), ("2020-W53".into(), "2020".into()));
        let s = one_kind("weeklyNotes", json!({ "enabled": true, "directory": "ww", "titlePattern": "yyyy-'W'ww", "locale": "en-US" }));
        assert_eq!(loc(WEEKLY, d(2026, 6, 9), &s), ("2026-W24".into(), "24".into()));
    }

    #[test]
    fn recognizes_a_day_from_iso_week_and_weekday() {
        let s = one_kind("dailyNotes", json!({ "enabled": true, "directory": "yyyy/MM-MMM", "titlePattern": "yyyy-'W'ww-EEE", "locale": "en-US" }));
        let note = inbox_note("inbox/2026/06-Jun/2026-W24-Fri.md", "2026-W24-Fri");
        assert_eq!(classify(note, &s), Some((DAILY, d(2026, 6, 12))));
        // ISO week 24 falls entirely in June, so a July folder cannot round-trip.
        let note = inbox_note("inbox/2026/07-Jul/2026-W24-Fri.md", "2026-W24-Fri");
        assert_eq!(classify(note, &s), None);
    }

    #[test]
    fn recognizes_a_daily_note_whose_year_is_in_the_directory() {
        let s = one_kind("dailyNotes", json!({ "enabled": true, "directory": "yyyy", "titlePattern": "MM-dd", "locale": "en-US" }));
        assert_eq!(classify(inbox_note("inbox/2026/06-09.md", "06-09"), &s), Some((DAILY, d(2026, 6, 9))));
    }

    #[test]
    fn recognizes_notes_from_legacy_patterns() {
        let s = one_kind("dailyNotes", json!({
            "enabled": true, "directory": "yyyy/MM-MMM", "titlePattern": "yyyy-MM-dd-EEE", "locale": "en-US",
            "legacyPatterns": [{ "directory": "Daily Notes", "titlePattern": "yyyy-MM-dd", "locale": "en-US" }]
        }));
        let note = inbox_note("inbox/Daily Notes/2026-06-12.md", "2026-06-12");
        assert_eq!(classify(note, &s), Some((DAILY, d(2026, 6, 12))));
        let s = one_kind("weeklyNotes", json!({
            "enabled": true, "directory": "yyyy/MM-MMM", "titlePattern": "yyyy-'W'ww-EEE", "locale": "en-US",
            "legacyPatterns": [{ "directory": "Weekly Notes", "titlePattern": "yyyy-'W'ww", "locale": "en-US" }]
        }));
        let note = inbox_note("inbox/Weekly Notes/2026-W24.md", "2026-W24");
        assert_eq!(classify(note, &s), Some((WEEKLY, d(2026, 6, 8))));
    }

    #[test]
    fn rejects_captures_that_disagree() {
        assert_eq!(match_date_note_pattern("yyyy-MM-dd yyyy", "2026-06-09 2025"), None);
        assert_eq!(
            match_date_note_pattern("yy-M-d", "26-6-9"),
            Some(PatternMatch { year: Some(2026), month: Some(6), day: Some(9), week: None })
        );
        assert_eq!(match_date_note_pattern("yy", "70").and_then(|m| m.year), Some(1970));
        // `\d` is ASCII only.
        assert_eq!(match_date_note_pattern("yyyy", "２０２６"), None);
    }

    /* ----- template-render.test.ts ----- */

    /// Friday, 2026-05-29, 14:07:09 local time.
    fn reference() -> NaiveDateTime {
        dt(2026, 5, 29, 14, 7, 9)
    }

    #[test]
    fn substitutes_title_date_time_and_week() {
        let body = render_template("# {{title}}\n{{date}} (week {{week}}) at {{time}}", "My Note", reference());
        assert_eq!(body, "# My Note\n2026-05-29 (week 22) at 14:07");
        assert_eq!(render_template("{{ title }} - {{  date  }}", "X", reference()), "X - 2026-05-29");
        assert_eq!(
            render_template("keep {{unknown}} and {{foo:bar}}", "X", reference()),
            "keep {{unknown}} and {{foo:bar}}"
        );
    }

    #[test]
    fn strips_every_cursor_marker() {
        assert_eq!(render_template("# {{title}}\n\n{{cursor}}", "Hi", reference()), "# Hi\n\n");
        assert_eq!(render_template("a{{cursor}}b{{ cursor }}c", "X", reference()), "abc");
        assert_eq!(render_template("{{date}}{{cursor}}!", "X", reference()), "2026-05-29!");
        // A cursor arriving through the title is stripped as well.
        assert_eq!(render_template("[{{title}}]", "a{{cursor}}b", reference()), "[ab]");
    }

    #[test]
    fn leaves_what_the_token_regex_does_not_match() {
        let now = reference();
        assert_eq!(substitute_tokens("{{}} {{ }} {{{title}}", "T", now), "{{}} {{ }} {{{title}}");
        assert_eq!(substitute_tokens("{{a} {{title}}", "T", now), "{{a} T");
        // A `[` without a `]` is text, and the `d` of `unclosed` a day.
        assert_eq!(substitute_tokens("{{date:[unclosed}}", "T", now), "[unclose29");
        assert_eq!(substitute_tokens("{{date: yyyy }}", "T", now), " 2026");
        assert_eq!(substitute_tokens("{{\u{FEFF}title\u{FEFF}}}", "T", now), "T");
    }

    #[test]
    fn formats_the_full_token_table() {
        let r = reference();
        assert_eq!(format_date(r, "YYYY-MM-DD"), "2026-05-29");
        assert_eq!(format_date(r, "YY/M/D"), "26/5/29");
        assert_eq!(format_date(r, "dddd, MMMM D, YYYY"), "Friday, May 29, 2026");
        assert_eq!(format_date(r, "ddd MMM"), "Fri May");
        assert_eq!(format_date(r, "HH:mm:ss"), "14:07:09");
        assert_eq!(format_date(r, "[Week of] MMMM D"), "Week of May 29");
        assert_eq!(format_date(r, "yyyy-MM-dd"), "2026-05-29");
        assert_eq!(format_date(r, "yy/M/d"), "26/5/29");
        assert_eq!(format_date(r, "EEEE, MMMM d"), "Friday, May 29");
        assert_eq!(format_date(r, "EEE"), "Fri");
        assert_eq!(format_date(r, "yyyy-[W]ww"), "2026-W22");
        assert_eq!(format_date(r, "w"), "22");
        assert_eq!(format_date(r, "YYYY-MM-DD dddd"), "2026-05-29 Friday");
        // First alternative wins: `YYY` is `YY` then a plain `Y`.
        assert_eq!(format_date(r, "YYY EE"), "26Y EE");
    }

    #[test]
    fn computes_iso_weeks() {
        assert_eq!(iso_week_parts(d(2026, 1, 1)), (1, 2026));
        assert_eq!(iso_week_parts(d(2026, 12, 31)), (53, 2026));
        assert_eq!(iso_week_parts(d(2027, 1, 1)), (53, 2026));
        assert_eq!(iso_week_parts(d(2024, 1, 1)), (1, 2024));
        assert_eq!(iso_week_parts(d(2023, 1, 1)), (52, 2022));
        assert_eq!(monday_of_iso_week(2026, 53), Some(d(2026, 12, 28)));
        assert_eq!(monday_of_iso_week(2020, 53), Some(d(2020, 12, 28)));
        // Every day agrees with chrono's ISO week, across a few decades.
        let mut day = d(1990, 1, 1);
        while day < d(2040, 1, 1) {
            let week = day.iso_week();
            assert_eq!(iso_week_parts(day), (i64::from(week.week()), i64::from(week.year())), "{day}");
            day = day.succ_opt().unwrap();
        }
    }

    #[test]
    fn makes_dates_the_way_the_date_constructor_does() {
        assert_eq!(js_make_date(2026, 1, 0), Some(d(2026, 1, 31)));
        assert_eq!(js_make_date(2026, -1, 1), Some(d(2025, 12, 1)));
        assert_eq!(js_make_date(2026, 12, 1), Some(d(2027, 1, 1)));
        assert_eq!(js_make_date(2026, 1, 30), Some(d(2026, 3, 2)));
        assert_eq!(js_make_date(50, 0, 1), Some(d(1950, 1, 1)));
        assert_eq!(js_make_date(100, 0, 1), Some(d(100, 1, 1)));
    }

    /* ----- template-files.test.ts ----- */

    #[test]
    fn parses_frontmatter() {
        let (data, body) = parse_frontmatter("---\nname: ADR\ncategory: Engineering\n---\n# Body\n");
        assert_eq!(data.len(), 2);
        assert_eq!((data["name"].as_str(), data["category"].as_str()), ("ADR", "Engineering"));
        assert_eq!(body, "# Body\n");
        let (data, _) = parse_frontmatter("---\ntitleTemplate: \"{{date:YYYY-MM-DD}} — \"\n---\nx");
        assert_eq!(data["titleTemplate"], "{{date:YYYY-MM-DD}} — ");
        let (data, body) = parse_frontmatter("# Just a body\n");
        assert!(data.is_empty());
        assert_eq!(body, "# Just a body\n");
        let raw = "---\nname: Broken\n# no closing fence\n";
        assert_eq!(parse_frontmatter(raw).1, raw);
        // CRLF fences; the fence closes at the first `---` line prefix.
        let (data, body) = parse_frontmatter("---\r\nname: X\r\n---\r\nbody");
        assert_eq!((data["name"].as_str(), body), ("X", "body"));
        assert_eq!(parse_frontmatter("---\nname: Dash\n----\nbody").1, "-\nbody");
        let (data, _) = parse_frontmatter("---\nq: \"\nr: ' x '\n: none\n---\n");
        assert_eq!((data["q"].as_str(), data["r"].as_str(), data.len()), ("", " x ", 2));
    }

    #[test]
    fn parses_custom_templates() {
        let raw = "---\nname: My ADR\ndescription: A record\ncategory: Engineering\n---\n# {{title}}\n";
        let t = parse_custom_template(raw, ".zennotes/templates/my-adr.md");
        assert_eq!(t.id, "custom:my-adr");
        assert_eq!(t.name, "My ADR");
        assert_eq!(t.category, Category::Engineering);
        assert!(!t.builtin);
        assert_eq!(t.source_path.as_deref(), Some(".zennotes/templates/my-adr.md"));
        assert_eq!(t.body, "# {{title}}\n");
        assert_eq!(parse_custom_template("# body", ".zennotes/templates/weekly-review.md").name, "weekly-review");
        let t = parse_custom_template("---\nname: X\ncategory: Nonsense\ntargetFolder: trash\n---\nbody", ".zennotes/templates/x.md");
        assert_eq!(t.category, Category::Custom);
        assert_eq!(parse_custom_template("", "a/B.Md").id, "custom:B");
        let t = parse_custom_template("---\nname: \" Spaced \"\nbuiltinId: '  '\n---\n", "x.md");
        assert_eq!((t.name.as_str(), t.builtin_id.as_deref()), ("Spaced", None));
    }

    fn test_template(id: &str, name: &str, category: Category, builtin: bool) -> Template {
        Template {
            id: id.into(),
            name: name.into(),
            category,
            body: String::new(),
            builtin,
            source_path: (!builtin).then(|| format!(".zennotes/templates/{name}.md")),
            builtin_id: None,
        }
    }

    #[test]
    fn merges_templates() {
        let merged = merge_templates(
            vec![
                test_template("builtin.rfc", "RFC", Category::Engineering, true),
                test_template("builtin.adr", "ADR", Category::Engineering, true),
            ],
            vec![
                test_template("custom:Zeta", "Zeta", Category::Custom, false),
                test_template("custom:Recipe", "Recipe", Category::Personal, false),
            ],
        );
        let names: Vec<&str> = merged.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["ADR", "RFC", "Recipe", "Zeta"]);
        let mut edited = test_template("custom:ADR", "ADR", Category::Engineering, false);
        edited.builtin_id = Some("builtin.adr".into());
        let merged = merge_templates(
            vec![
                test_template("builtin.adr", "ADR", Category::Engineering, true),
                test_template("builtin.rfc", "RFC", Category::Engineering, true),
            ],
            vec![edited],
        );
        let names: Vec<&str> = merged.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["ADR", "RFC"]);
        assert!(!merged[0].builtin);
        assert_eq!(merged[0].builtin_id.as_deref(), Some("builtin.adr"));
    }

    #[test]
    fn finds_templates() {
        // The Daily Note built-in edited in Settings, as composeTemplateFile writes it.
        let edited = parse_custom_template(
            "---\nname: Daily Note\ncategory: Personal\ntitleTemplate: \"{{date:YYYY-MM-DD}}\"\nbuiltinId: builtin.daily\n---\n# {{date:dddd, MMMM D, YYYY}}\n\n## Today\n\n- {{cursor}}\n",
            ".zennotes/templates/daily-note.md",
        );
        let merged = merge_templates(builtin_templates(), vec![edited.clone()]);
        assert!(!merged.iter().any(|t| t.id == "builtin.daily"));
        let found = find_template(&merged, Some("builtin.daily")).unwrap();
        assert_eq!(found.id, edited.id);
        assert!(found.body.contains("## Today"));
        assert_eq!(find_template(&merged, Some(&edited.id)), Some(&edited));
        assert!(find_template(&merged, Some("builtin.weekly")).unwrap().builtin);
        // A template with the id itself beats an edited copy pointing at it.
        let mut other = edited.clone();
        other.id = "custom:other".into();
        other.builtin_id = Some("builtin.adr".into());
        let adr = builtin_templates().into_iter().find(|t| t.id == "builtin.adr").unwrap();
        assert!(find_template(&[other, adr], Some("builtin.adr")).unwrap().builtin);
        assert_eq!(find_template(&merged, None), None);
        assert_eq!(find_template(&merged, Some("custom:deleted")), None);
    }

    #[test]
    fn approximates_icu_collation_for_ascii() {
        // The order Node's `localeCompare` sorts these in.
        let expected = "\t _-,;:!?.'\"()[]{}@*/\\&#%`^+<=>|~$0123456789aAbBzZ";
        let mut chars: Vec<String> = expected.chars().rev().map(String::from).collect();
        chars.sort_by(|a, b| locale_compare(a, b));
        assert_eq!(chars.concat(), expected);
        assert_eq!(locale_compare("alpha", "Zed"), Ordering::Less);
        assert_eq!(locale_compare("a\u{1}b", "ab"), Ordering::Equal);
        assert_eq!(locale_compare("ab", "Ab"), Ordering::Less);
        assert_eq!(locale_compare("Ab", "ac"), Ordering::Less);
    }

    #[test]
    fn trims_as_js_does() {
        assert_eq!(js_trim("\u{FEFF}\u{A0} x \u{3000}"), "x");
        assert_eq!(js_trim("\u{85}x"), "\u{85}x");
    }
}
