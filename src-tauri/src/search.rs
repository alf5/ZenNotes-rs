//! Full-text vault search — port of the search pipeline in
//! apps/desktop/src/main/vault.ts (`searchVaultText*`, `scoreMatch`,
//! `firstMatchColumn`, builtin/ripgrep/fzf candidate collection + ranking).
//!
//! Offsets are computed in UTF-16 code units to match JS string indices, so
//! the match offset lines up with CodeMirror positions in the editor.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::ipc::types::VaultTextSearchMatch;
use crate::vault::config::resolve_path;
use crate::vault::listing::list_notes;
use crate::vault::notes::{folder_for_relative_path, normalize_vault_relative_path};

const SEARCHABLE_FOLDERS: [&str; 3] = ["inbox", "quick", "archive"];
const SEARCH_LIMIT: usize = 80;

#[derive(Clone)]
struct Candidate {
    path: String,
    title: String,
    folder: String,
    line_number: i64,
    line_text: String,
    /// UTF-16 offset of the line start, when known (builtin path).
    offset: Option<usize>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Backend {
    Builtin,
    Ripgrep,
    Fzf,
}

pub struct ToolPaths {
    pub ripgrep: Option<String>,
    pub fzf: Option<String>,
}

fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn collapse_search_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut prev_space = false;
    for c in line.chars() {
        if c.is_whitespace() {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out.trim().to_string()
}

fn truncate_utf16_220(s: &str) -> String {
    // Mirror JS `.slice(0, 220)` over the collapsed line (UTF-16 units).
    if utf16_len(s) <= 220 {
        return s.to_string();
    }
    let mut out = String::new();
    let mut count = 0usize;
    for c in s.chars() {
        let w = c.encode_utf16(&mut [0u16; 2]).len();
        if count + w > 220 {
            break;
        }
        out.push(c);
        count += w;
    }
    out
}

fn is_boundary_char(c: char) -> bool {
    c.is_whitespace() || matches!(c, '·' | ':' | '_' | '-' | '/')
}

/// Port of `scoreMatch`.
fn score_match(query: &str, text: &str) -> f64 {
    if query.is_empty() {
        return 1.0;
    }
    if text.is_empty() {
        return 0.0;
    }
    let q = query.to_lowercase();
    let t = text.to_lowercase();
    let t_len = utf16_len(&t) as f64;
    if t == q {
        return 1000.0;
    }
    if t.starts_with(&q) {
        return 900.0 - t_len * 0.5;
    }
    // word-boundary: q preceded by start or a boundary char.
    if let Some(pos) = find_with_boundary(&t, &q) {
        let _ = pos;
        return 700.0 - t_len * 0.5;
    }
    if t.contains(&q) {
        return 500.0 - t_len * 0.5;
    }
    // subsequence
    let q_chars: Vec<char> = q.chars().collect();
    let mut i = 0usize;
    let mut gaps = 0i64;
    let mut prev: i64 = -1;
    for (j, ch) in t.chars().enumerate() {
        if i >= q_chars.len() {
            break;
        }
        if ch == q_chars[i] {
            if prev == -1 {
                gaps += j as i64;
            } else {
                gaps += j as i64 - prev - 1;
            }
            prev = j as i64;
            i += 1;
        }
    }
    if i == q_chars.len() {
        return (200.0 - gaps as f64 * 3.0 - t_len * 0.2).max(1.0);
    }
    0.0
}

/// True when `q` occurs at start or after a boundary char in `t`.
fn find_with_boundary(t: &str, q: &str) -> Option<usize> {
    let tc: Vec<char> = t.chars().collect();
    let qc: Vec<char> = q.chars().collect();
    if qc.is_empty() || qc.len() > tc.len() {
        return None;
    }
    for start in 0..=(tc.len() - qc.len()) {
        if tc[start..start + qc.len()] == qc[..] {
            let boundary = start == 0 || is_boundary_char(tc[start - 1]);
            if boundary {
                return Some(start);
            }
        }
    }
    None
}

/// Port of `firstMatchColumn` — UTF-16 index of the (fuzzy) match start.
fn first_match_column(query: &str, text: &str) -> usize {
    let q: String = query.trim().to_lowercase();
    let t = text.to_lowercase();
    if let Some(byte_idx) = t.find(&q) {
        // Convert the byte index to a UTF-16 offset.
        return utf16_len(&t[..byte_idx]);
    }
    let q_chars: Vec<char> = q.chars().collect();
    let mut qi = 0usize;
    let mut start: i64 = -1;
    let mut utf16_pos = 0usize;
    let mut start_utf16 = 0usize;
    for ch in t.chars() {
        if qi < q_chars.len() && ch == q_chars[qi] {
            if start == -1 {
                start = 0;
                start_utf16 = utf16_pos;
            }
            qi += 1;
        }
        utf16_pos += ch.encode_utf16(&mut [0u16; 2]).len();
    }
    if start >= 0 {
        start_utf16
    } else {
        0
    }
}

fn collect_builtin(root: &Path) -> Vec<Candidate> {
    let mut out = Vec::new();
    for meta in list_notes(root) {
        if !SEARCHABLE_FOLDERS.contains(&meta.folder.as_str()) {
            continue;
        }
        let abs = root.join(meta.path.replace('/', std::path::MAIN_SEPARATOR_STR));
        let Ok(body) = std::fs::read_to_string(&abs) else {
            continue;
        };
        let mut line_offset = 0usize;
        for (index, line) in body.split('\n').enumerate() {
            out.push(Candidate {
                path: meta.path.clone(),
                title: meta.title.clone(),
                folder: meta.folder.clone(),
                line_number: index as i64 + 1,
                line_text: truncate_utf16_220(&collapse_search_line(line)),
                offset: Some(line_offset),
            });
            line_offset += utf16_len(line) + 1;
        }
    }
    out
}

fn collect_ripgrep(root: &Path, rg: &str) -> Vec<Candidate> {
    let search_roots: Vec<String> = SEARCHABLE_FOLDERS
        .iter()
        .map(|f| {
            let dir = crate::vault::layout::folder_root(root, f);
            let root_abs = resolve_path(&root.to_string_lossy());
            let dir_abs = resolve_path(&dir.to_string_lossy());
            let rel = dir_abs
                .strip_prefix(&format!("{root_abs}{}", std::path::MAIN_SEPARATOR))
                .unwrap_or(&dir_abs);
            let n = normalize_vault_relative_path(rel);
            if n.is_empty() {
                ".".to_string()
            } else {
                n
            }
        })
        .collect();
    let roots: Vec<String> = if search_roots.iter().any(|r| r == ".") {
        vec![".".to_string()]
    } else {
        search_roots
    };

    let mut args = vec![
        "--json".to_string(),
        "--line-number".to_string(),
        "--with-filename".to_string(),
        "--no-heading".to_string(),
        "--color=never".to_string(),
        "-g".to_string(),
        "*.md".to_string(),
        "^".to_string(),
    ];
    args.extend(roots);

    let Ok(output) = Command::new(rg).args(&args).current_dir(root).output() else {
        return Vec::new();
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut out = Vec::new();
    for line in stdout.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) != Some("match") {
            continue;
        }
        let data = event.get("data");
        let rel = data
            .and_then(|d| d.get("path"))
            .and_then(|p| p.get("text"))
            .and_then(Value::as_str)
            .map(normalize_vault_relative_path);
        let raw_line = data
            .and_then(|d| d.get("lines"))
            .and_then(|l| l.get("text"))
            .and_then(Value::as_str)
            .map(|s| s.trim_end_matches(['\r', '\n']).to_string());
        let line_number = data.and_then(|d| d.get("line_number")).and_then(Value::as_i64);
        let (Some(rel), Some(raw_line), Some(line_number)) = (rel, raw_line, line_number) else {
            continue;
        };
        let Some(folder) = folder_for_relative_path(&rel) else { continue };
        if !SEARCHABLE_FOLDERS.contains(&folder.as_str()) {
            continue;
        }
        let title = crate::vault::notes::title_from_path(Path::new(&rel));
        out.push(Candidate {
            path: rel,
            title,
            folder,
            line_number,
            line_text: truncate_utf16_220(&collapse_search_line(&raw_line)),
            offset: None,
        });
    }
    out
}

fn rank(query: &str, candidates: &[Candidate]) -> Vec<Candidate> {
    let mut scored: Vec<(f64, &Candidate)> = Vec::new();
    for c in candidates {
        let body = score_match(query, &c.line_text);
        if body <= 0.0 {
            continue;
        }
        let total = body + score_match(query, &c.title) * 0.18 + score_match(query, &c.path) * 0.1;
        scored.push((total, c));
    }
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored.into_iter().take(SEARCH_LIMIT).map(|(_, c)| c.clone()).collect()
}

fn run_fzf(query: &str, fzf: &str, candidates: &[Candidate]) -> Option<Vec<Candidate>> {
    let mut child = Command::new(fzf)
        .args(["--filter", query, "--delimiter=\t", "--nth=1,2,5", "--tiebreak=index"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    {
        let mut stdin = child.stdin.take()?;
        for c in candidates {
            let row = format!(
                "{}\t{}\t{}\t{}\t{}\n",
                c.path.replace('\t', " "),
                c.title.replace('\t', " "),
                c.folder,
                c.line_number,
                c.line_text.replace('\t', " ")
            );
            if stdin.write_all(row.as_bytes()).is_err() {
                break;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() && output.status.code() != Some(1) {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let matches: Vec<Candidate> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .take(SEARCH_LIMIT)
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 5 {
                return None;
            }
            let folder = if parts[2] == "quick" || parts[2] == "archive" { parts[2] } else { "inbox" };
            Some(Candidate {
                path: parts[0].to_string(),
                title: parts[1].to_string(),
                folder: folder.to_string(),
                line_number: parts[3].parse().unwrap_or(0),
                line_text: parts[4].to_string(),
                offset: None,
            })
        })
        .collect();
    Some(matches)
}

fn hydrate(root: &Path, query: &str, candidates: Vec<Candidate>) -> Vec<VaultTextSearchMatch> {
    use std::collections::HashMap;
    let mut body_cache: HashMap<String, String> = HashMap::new();
    candidates
        .into_iter()
        .map(|c| {
            let body = body_cache.entry(c.path.clone()).or_insert_with(|| {
                let abs = root.join(c.path.replace('/', std::path::MAIN_SEPARATOR_STR));
                std::fs::read_to_string(&abs).unwrap_or_default()
            });
            let raw_line = body
                .split('\n')
                .nth((c.line_number - 1).max(0) as usize)
                .unwrap_or("");
            let line_len = utf16_len(raw_line);
            let column = first_match_column(query, raw_line).min(line_len);
            let line_start = match c.offset {
                Some(o) => o,
                None => {
                    // ripgrep/fzf: compute the line start offset (UTF-16) from the body.
                    let mut acc = 0usize;
                    for (i, line) in body.split('\n').enumerate() {
                        if i as i64 == c.line_number - 1 {
                            break;
                        }
                        acc += utf16_len(line) + 1;
                    }
                    acc
                }
            };
            VaultTextSearchMatch {
                path: c.path,
                title: c.title,
                folder: c.folder,
                line_number: c.line_number,
                offset: (line_start + column) as i64,
                line_text: c.line_text,
            }
        })
        .collect()
}

fn normalize_tool_path(value: &Option<String>) -> Option<String> {
    let trimmed = value.as_deref().map(str::trim).filter(|s| !s.is_empty())?;
    if trimmed == "~" {
        return std::env::var("HOME").ok();
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return Some(format!("{home}/{rest}"));
        }
    }
    Some(trimmed.to_string())
}

fn search_executable(kind: Backend, paths: &ToolPaths) -> Option<String> {
    let (custom, default_name) = match kind {
        Backend::Ripgrep => (normalize_tool_path(&paths.ripgrep), "rg"),
        Backend::Fzf => (normalize_tool_path(&paths.fzf), "fzf"),
        Backend::Builtin => return None,
    };
    Some(custom.unwrap_or_else(|| default_name.to_string()))
}

fn command_available(cmd: &str) -> bool {
    Command::new(cmd)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `vault:text-search-capabilities` — which external search tools are usable.
pub fn capabilities(paths: &ToolPaths) -> (bool, bool) {
    let rg = search_executable(Backend::Ripgrep, paths).map(|c| command_available(&c)).unwrap_or(false);
    let fzf = search_executable(Backend::Fzf, paths).map(|c| command_available(&c)).unwrap_or(false);
    (rg, fzf)
}

fn resolve_backend(preferred: &str, rg: bool, fzf: bool) -> Backend {
    match preferred {
        "builtin" => Backend::Builtin,
        "ripgrep" => {
            if rg {
                Backend::Ripgrep
            } else {
                Backend::Builtin
            }
        }
        "fzf" => {
            if fzf {
                Backend::Fzf
            } else {
                Backend::Builtin
            }
        }
        _ => {
            if fzf {
                Backend::Fzf
            } else if rg {
                Backend::Ripgrep
            } else {
                Backend::Builtin
            }
        }
    }
}

/// `vault:search-text` — full-text search over inbox/quick/archive.
pub fn search_vault_text(
    root: &Path,
    query: &str,
    preferred: &str,
    paths: &ToolPaths,
) -> Vec<VaultTextSearchMatch> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }
    let (rg_ok, fzf_ok) = capabilities(paths);
    let backend = resolve_backend(preferred, rg_ok, fzf_ok);

    let ranked: Vec<Candidate> = match backend {
        Backend::Builtin => rank(trimmed, &collect_builtin(root)),
        Backend::Ripgrep => {
            let rg = search_executable(Backend::Ripgrep, paths).unwrap();
            rank(trimmed, &collect_ripgrep(root, &rg))
        }
        Backend::Fzf => {
            // fzf filters over rg (preferred) or builtin candidates.
            let candidates = if rg_ok {
                let rg = search_executable(Backend::Ripgrep, paths).unwrap();
                collect_ripgrep(root, &rg)
            } else {
                collect_builtin(root)
            };
            let fzf = search_executable(Backend::Fzf, paths).unwrap();
            match run_fzf(trimmed, &fzf, &candidates) {
                Some(m) if !m.is_empty() => m,
                _ => rank(trimmed, &candidates),
            }
        }
    };
    hydrate(root, trimmed, ranked)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::{crud, layout};

    fn vault() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        layout::ensure_vault_layout(dir.path()).unwrap();
        dir
    }

    #[test]
    fn score_ordering() {
        assert!(score_match("foo", "foo") > score_match("foo", "foobar"));
        assert!(score_match("foo", "a foo b") > score_match("foo", "xfoox"));
        assert_eq!(score_match("zzz", "abc"), 0.0);
    }

    #[test]
    fn builtin_search_finds_lines_with_offsets() {
        let v = vault();
        crud::write_note(v.path(), "inbox/A.md", "first line\nhello needle here\nlast").unwrap();
        crud::write_note(v.path(), "trash/T.md", "needle in trash").unwrap();
        let paths = ToolPaths { ripgrep: None, fzf: None };
        let results = search_vault_text(v.path(), "needle", "builtin", &paths);
        assert!(results.iter().any(|m| m.path == "inbox/A.md" && m.line_number == 2));
        // Trash is excluded from search.
        assert!(!results.iter().any(|m| m.path.starts_with("trash/")));
        let hit = results.iter().find(|m| m.path == "inbox/A.md").unwrap();
        // "hello " is 6 UTF-16 units; line 2 starts after "first line\n" (11).
        assert_eq!(hit.offset, 11 + 6);
    }

    #[test]
    fn first_match_column_handles_substring_and_subsequence() {
        assert_eq!(first_match_column("ndl", "a needle"), 2);
        assert_eq!(first_match_column("needle", "a needle"), 2);
    }
}
