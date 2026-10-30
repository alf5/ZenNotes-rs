//! MCP client integrations (Settings → MCP), ported from upstream
//! apps/desktop/src/main/mcp-integrations.ts: read and write the config files
//! Claude Code, Claude Desktop, Codex and OpenCode discover MCP servers in, so
//! the user can connect ZenNotes-rs without leaving the app.
//!
//! The server installed is this very binary run as `zennotes-rs mcp`. It is
//! registered under its own key, `zennotes-rs`, not upstream's `zennotes`:
//! with both apps installed, a shared key would have each overwrite the
//! other's entry and report it out of date forever.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Map, Value};

use crate::vault::atomic::write_file_atomic;

pub const SERVER_KEY: &str = "zennotes-rs";
const MARK_START: &str = "# >>> zennotes-rs mcp begin (managed) >>>";
const MARK_END: &str = "# <<< zennotes-rs mcp end (managed) <<<";

/// The ids `@shared/mcp-clients` lists, in its order.
pub const CLIENT_IDS: [&str; 4] = ["claude-code", "claude-desktop", "codex", "opencode"];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Runtime {
    pub command: String,
    pub args: Vec<String>,
    pub env: Map<String, Value>,
    /// The binary the clients will run, when it exists.
    pub entry_path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientStatus {
    pub id: String,
    pub config_path: String,
    pub installed: bool,
    pub up_to_date: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// How clients start the server. An AppImage runs from a fresh mount point
/// each launch, so the file to register is the AppImage itself, which the
/// runtime names in `$APPIMAGE`.
pub fn runtime() -> Runtime {
    let command = std::env::var("APPIMAGE")
        .ok()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_exe().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let entry_path = (!command.is_empty() && Path::new(&command).exists()).then(|| command.clone());
    Runtime { command, args: vec!["mcp".into()], env: Map::new(), entry_path }
}

/* ---------- Paths -------------------------------------------------------- */

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_default()
}

fn xdg_config_home() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home().join(".config"))
}

pub fn client_config_path(id: &str) -> Option<PathBuf> {
    Some(match id {
        // User-scope MCP servers live in ~/.claude.json under `mcpServers`.
        "claude-code" => home().join(".claude.json"),
        "claude-desktop" => {
            if cfg!(target_os = "macos") {
                home().join("Library/Application Support/Claude/claude_desktop_config.json")
            } else if cfg!(windows) {
                std::env::var_os("APPDATA")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home().join("AppData").join("Roaming"))
                    .join("Claude")
                    .join("claude_desktop_config.json")
            } else {
                xdg_config_home().join("Claude").join("claude_desktop_config.json")
            }
        }
        "codex" => home().join(".codex").join("config.toml"),
        "opencode" => xdg_config_home().join("opencode").join("opencode.json"),
        _ => return None,
    })
}

/* ---------- File helpers ------------------------------------------------- */

fn read_text(path: &Path) -> Result<String, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// A JSON config as an object; a missing or non-object file reads as `{}`.
/// Unparseable JSON is an error for Claude's files: rewriting one we could
/// not read would drop everything in it.
fn read_json(path: &Path, lenient: bool) -> Result<Map<String, Value>, String> {
    let raw = read_text(path)?;
    if raw.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Ok(Map::new()),
        Err(_) if lenient => Ok(Map::new()),
        Err(e) => Err(format!("{} is not valid JSON ({e}); fix or move it first", path.display())),
    }
}

fn write_json(path: &Path, data: &Map<String, Value>) -> Result<(), String> {
    let text = serde_json::to_string_pretty(data).map_err(|e| e.to_string())?;
    write_file_atomic(path, &format!("{text}\n"))
}

/* ---------- Entry shapes -------------------------------------------------- */

fn json_entry(rt: &Runtime) -> Value {
    json!({ "command": rt.command, "args": rt.args, "env": rt.env })
}

fn json_entry_matches(entry: &Value, rt: &Runtime) -> bool {
    let env = entry.get("env").and_then(Value::as_object).cloned().unwrap_or_default();
    entry.get("command").and_then(Value::as_str) == Some(rt.command.as_str())
        && entry.get("args").cloned().unwrap_or(json!([])) == json!(rt.args)
        && env == rt.env
}

fn opencode_entry(rt: &Runtime) -> Value {
    let mut command = vec![rt.command.clone()];
    command.extend(rt.args.iter().cloned());
    let mut entry = json!({ "type": "local", "command": command });
    if !rt.env.is_empty() {
        entry["environment"] = Value::Object(rt.env.clone());
    }
    entry["enabled"] = json!(true);
    entry
}

fn toml_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

fn render_toml_block(rt: &Runtime) -> String {
    let args: Vec<String> = rt.args.iter().map(|a| format!("\"{}\"", toml_escape(a))).collect();
    let mut lines = vec![
        MARK_START.to_string(),
        format!("[mcp_servers.{SERVER_KEY}]"),
        format!("command = \"{}\"", toml_escape(&rt.command)),
        format!("args = [{}]", args.join(", ")),
    ];
    if !rt.env.is_empty() {
        let pairs: Vec<String> = rt
            .env
            .iter()
            .map(|(k, v)| format!("{k} = \"{}\"", toml_escape(v.as_str().unwrap_or(""))))
            .collect();
        lines.push(format!("env = {{ {} }}", pairs.join(", ")));
    }
    lines.push(MARK_END.into());
    lines.join("\n")
}

fn managed_block(contents: &str) -> Option<(usize, usize)> {
    let start = contents.find(MARK_START)?;
    let end = contents[start..].find(MARK_END)? + start + MARK_END.len();
    Some((start, end))
}

fn strip_managed_block(contents: &str) -> String {
    let Some((start, end)) = managed_block(contents) else { return contents.to_string() };
    let before = contents[..start].trim_end_matches('\n');
    let after = contents[end..].trim_start_matches('\n');
    match (before.is_empty(), after.is_empty()) {
        (false, false) => format!("{before}\n\n{after}"),
        (false, true) => format!("{before}\n"),
        _ => after.to_string(),
    }
}

/* ---------- Public API --------------------------------------------------- */

fn status_of(id: &str, rt: &Runtime) -> ClientStatus {
    let path = client_config_path(id).unwrap_or_default();
    let mut status = ClientStatus {
        id: id.to_string(),
        config_path: path.to_string_lossy().into_owned(),
        installed: false,
        up_to_date: false,
        note: rt.entry_path.is_none().then(|| "The ZenNotes-rs binary could not be located.".to_string()),
    };
    let result = (|| -> Result<(), String> {
        match id {
            "claude-code" | "claude-desktop" => {
                let cfg = read_json(&path, false)?;
                if let Some(entry) = cfg.get("mcpServers").and_then(|s| s.get(SERVER_KEY)) {
                    status.installed = true;
                    status.up_to_date = json_entry_matches(entry, rt);
                }
            }
            "opencode" => {
                let cfg = read_json(&path, true)?;
                if let Some(entry) = cfg.get("mcp").and_then(|s| s.get(SERVER_KEY)) {
                    let expected = opencode_entry(rt);
                    status.installed = true;
                    status.up_to_date = entry.get("type") == Some(&json!("local"))
                        && entry.get("command") == expected.get("command")
                        && entry.get("environment").cloned().unwrap_or(json!({}))
                            == expected.get("environment").cloned().unwrap_or(json!({}));
                }
            }
            _ => {
                let raw = read_text(&path)?;
                if let Some((start, end)) = managed_block(&raw) {
                    status.installed = true;
                    status.up_to_date = raw[start..end].trim() == render_toml_block(rt).trim();
                }
            }
        }
        Ok(())
    })();
    if let Err(message) = result {
        status.note = Some(message);
    }
    status
}

pub fn statuses() -> Vec<ClientStatus> {
    let rt = runtime();
    CLIENT_IDS.iter().map(|id| status_of(id, &rt)).collect()
}

fn known(id: &str) -> Result<PathBuf, String> {
    client_config_path(id).ok_or_else(|| format!("Unknown MCP client: {id}"))
}

pub fn install(id: &str) -> Result<ClientStatus, String> {
    let path = known(id)?;
    let rt = runtime();
    if rt.entry_path.is_none() {
        return Err("The ZenNotes-rs binary could not be located, so there is nothing to register.".into());
    }
    match id {
        "claude-code" | "claude-desktop" | "opencode" => {
            let (section, entry) =
                if id == "opencode" { ("mcp", opencode_entry(&rt)) } else { ("mcpServers", json_entry(&rt)) };
            let mut cfg = read_json(&path, id == "opencode")?;
            let servers = cfg.entry(section).or_insert_with(|| json!({}));
            if !servers.is_object() {
                *servers = json!({});
            }
            servers[SERVER_KEY] = entry;
            write_json(&path, &cfg)?;
        }
        _ => {
            let stripped = strip_managed_block(&read_text(&path)?);
            let block = render_toml_block(&rt);
            let joined = if stripped.is_empty() {
                format!("{block}\n")
            } else {
                format!("{}\n\n{block}\n", stripped.trim_end_matches('\n'))
            };
            write_file_atomic(&path, &joined)?;
        }
    }
    Ok(status_of(id, &rt))
}

pub fn uninstall(id: &str) -> Result<ClientStatus, String> {
    let path = known(id)?;
    let rt = runtime();
    match id {
        "claude-code" | "claude-desktop" | "opencode" => {
            let section = if id == "opencode" { "mcp" } else { "mcpServers" };
            let mut cfg = read_json(&path, id == "opencode")?;
            let removed = cfg.get_mut(section).and_then(Value::as_object_mut).and_then(|s| s.shift_remove(SERVER_KEY));
            if removed.is_some() {
                if cfg.get(section).and_then(Value::as_object).is_some_and(Map::is_empty) {
                    cfg.shift_remove(section);
                }
                write_json(&path, &cfg)?;
            }
        }
        _ => {
            let raw = read_text(&path)?;
            if managed_block(&raw).is_some() {
                let stripped = strip_managed_block(&raw);
                let text = if stripped.ends_with('\n') || stripped.is_empty() { stripped } else { format!("{stripped}\n") };
                write_file_atomic(&path, &text)?;
            }
        }
    }
    Ok(status_of(id, &rt))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rt() -> Runtime {
        Runtime { command: "/opt/zn \"x\"".into(), args: vec!["mcp".into()], env: Map::new(), entry_path: Some("/opt/zn".into()) }
    }

    #[test]
    fn toml_block_round_trips_and_keeps_the_rest() {
        let block = render_toml_block(&rt());
        assert_eq!(
            block,
            "# >>> zennotes-rs mcp begin (managed) >>>\n[mcp_servers.zennotes-rs]\ncommand = \"/opt/zn \\\"x\\\"\"\nargs = [\"mcp\"]\n# <<< zennotes-rs mcp end (managed) <<<"
        );
        let parsed: toml::Table = block.parse().unwrap();
        assert_eq!(parsed["mcp_servers"]["zennotes-rs"]["command"].as_str(), Some("/opt/zn \"x\""));
        let file = format!("model = \"o3\"\n\n{block}\n\n[other]\nx = 1\n");
        assert_eq!(strip_managed_block(&file), "model = \"o3\"\n\n[other]\nx = 1\n");
        assert_eq!(strip_managed_block(&format!("{block}\n")), "");
    }

    #[test]
    fn json_entries_compare_like_upstream() {
        let entry = json_entry(&rt());
        assert!(json_entry_matches(&entry, &rt()));
        assert!(json_entry_matches(&json!({ "command": "/opt/zn \"x\"", "args": ["mcp"] }), &rt()));
        assert!(!json_entry_matches(&json!({ "command": "/old/zn", "args": ["mcp"] }), &rt()));
        assert_eq!(opencode_entry(&rt()), json!({ "type": "local", "command": ["/opt/zn \"x\"", "mcp"], "enabled": true }));
    }
}

/// Writes real client configs under a throwaway HOME, so it is opt-in:
/// `cargo test install_round_trip -- --ignored --test-threads=1`.
#[cfg(test)]
mod home_tests {
    use super::*;

    #[test]
    #[ignore]
    fn install_round_trip() {
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", home.path());
        std::env::set_var("XDG_CONFIG_HOME", home.path().join(".config"));
        let claude = home.path().join(".claude.json");
        fs::write(&claude, "{\n  \"zeta\": 1,\n  \"alpha\": {\"keep\": true}\n}\n").unwrap();
        fs::create_dir_all(home.path().join(".codex")).unwrap();
        fs::write(home.path().join(".codex/config.toml"), "model = \"o3\"\n").unwrap();
        for id in CLIENT_IDS {
            let status = install(id).unwrap();
            assert!(status.installed && status.up_to_date, "{id}: {status:?}");
        }
        // The user's own keys survive, in their order.
        let text = fs::read_to_string(&claude).unwrap();
        assert!(text.find("\"zeta\"").unwrap() < text.find("\"alpha\"").unwrap(), "{text}");
        let codex: toml::Table = fs::read_to_string(home.path().join(".codex/config.toml")).unwrap().parse().unwrap();
        assert_eq!(codex["model"].as_str(), Some("o3"));
        assert_eq!(codex["mcp_servers"]["zennotes-rs"]["args"][0].as_str(), Some("mcp"));
        for id in CLIENT_IDS {
            assert!(!uninstall(id).unwrap().installed, "{id}");
        }
        assert_eq!(fs::read_to_string(&claude).unwrap(), "{\n  \"zeta\": 1,\n  \"alpha\": {\n    \"keep\": true\n  }\n}\n");
        assert_eq!(fs::read_to_string(home.path().join(".codex/config.toml")).unwrap(), "model = \"o3\"\n");
    }
}
