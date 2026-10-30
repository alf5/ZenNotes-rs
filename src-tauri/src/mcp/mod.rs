//! The ZenNotes MCP server (`zennotes-rs mcp`), ported from upstream
//! apps/desktop/src/mcp. MCP clients (Claude Code, Claude Desktop, Codex,
//! OpenCode) spawn it over stdio; it works on the vault folder on disk
//! directly, so the desktop app does not have to be running. Which vault:
//! `--vault <path or name>`, else `$ZENNOTES_VAULT`, else the vault the app
//! last had open, asked again before every tool call.
//!
//! Only local vaults: this build has no remote workspaces, so upstream's
//! `--server` / `--token` and the server-token settings have nothing to
//! reach.

pub mod agent_edits;
pub mod comments;
pub mod integrations;
pub mod links;
pub mod ops;
pub mod server;
pub mod tasks;

use std::path::{Path, PathBuf};

use crate::vault::config::{basename, load_config, resolve_path};

/// The app's machine-local config folder, where Tauri keeps
/// `zennotes-rs.config.json` (`app.path().app_config_dir()`). Worked out here
/// without Tauri because the MCP process never starts it.
pub fn app_config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_default().join("com.zennotes-rs.app")
}

/// A boolean from the portable config.toml (`[section] key`), or None when
/// the file, the section or the key is missing.
pub fn portable_pref(section: &str, key: &str) -> Option<bool> {
    let raw = std::fs::read_to_string(crate::app_config::config_file()).ok()?;
    let parsed: toml::Table = raw.parse().ok()?;
    parsed.get(section)?.get(key)?.as_bool()
}

/* ---------- Instructions --------------------------------------------- */

/// The instructions upstream's server ships, captured by
/// tooling/mcp-golden/generate.ts.
pub const DEFAULT_INSTRUCTIONS: &str = include_str!("instructions.md");
const INSTRUCTIONS_FILENAME: &str = "zennotes.mcp-instructions.md";

/// Settings → MCP → Instructions saves an override here as plain Markdown,
/// so it is easy to diff, edit outside the app, or keep in dotfiles.
pub fn instructions_file_path() -> PathBuf {
    app_config_dir().join(INSTRUCTIONS_FILENAME)
}

pub fn custom_instructions() -> Option<String> {
    let raw = std::fs::read_to_string(instructions_file_path()).ok()?;
    (!raw.trim().is_empty()).then_some(raw)
}

pub fn resolve_instructions() -> String {
    custom_instructions().unwrap_or_else(|| DEFAULT_INSTRUCTIONS.to_string())
}

/// Save an override; `None` or blank text clears it and the default applies.
pub fn write_custom_instructions(next: Option<&str>) -> Result<(), String> {
    let path = instructions_file_path();
    match next.filter(|t| !t.trim().is_empty()) {
        None => {
            let _ = std::fs::remove_file(path);
            Ok(())
        }
        Some(text) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
            }
            let text = if text.ends_with('\n') { text.to_string() } else { format!("{text}\n") };
            std::fs::write(path, text).map_err(|e| format!("write failed: {e}"))
        }
    }
}

/* ---------- Vault resolution ----------------------------------------- */

fn expand_home(target: &str) -> String {
    let home = || dirs::home_dir().unwrap_or_default();
    if target == "~" {
        return home().to_string_lossy().into_owned();
    }
    match target.strip_prefix("~/").or_else(|| target.strip_prefix(&format!("~{}", std::path::MAIN_SEPARATOR))) {
        Some(rest) => home().join(rest).to_string_lossy().into_owned(),
        None => target.to_string(),
    }
}

/// A `--vault` selector: a vault the app knows by name (the name it shows,
/// then the folder's own), else a directory path. Errors name the known
/// vaults so a typo corrects itself.
fn resolve_vault_selector(selector: &str) -> Result<PathBuf, String> {
    let trimmed = selector.trim();
    let wanted = trimmed.to_lowercase();
    let known = load_config(&app_config_dir()).local_vaults;
    let by_display: Vec<_> = known.iter().filter(|v| v.name.to_lowercase() == wanted).collect();
    let by_name = if by_display.is_empty() {
        known.iter().filter(|v| basename(&v.root).to_lowercase() == wanted).collect()
    } else {
        by_display
    };
    match by_name.as_slice() {
        [one] if Path::new(&one.root).is_dir() => return Ok(PathBuf::from(&one.root)),
        [one] => {
            return Err(format!(
                "Vault \"{}\" points to {}, which is missing. Open it in ZenNotes again or pass a path.",
                one.name, one.root
            ))
        }
        [] => {}
        many => {
            let roots: Vec<&str> = many.iter().map(|v| v.root.as_str()).collect();
            return Err(format!("Multiple vaults are named \"{trimmed}\" ({}). Pass the path instead.", roots.join(", ")));
        }
    }
    let abs = PathBuf::from(resolve_path(&expand_home(trimmed)));
    if abs.is_dir() {
        return Ok(abs);
    }
    let names: Vec<&str> = known.iter().map(|v| v.name.as_str()).collect();
    Err(if names.is_empty() {
        format!("No vault named \"{trimmed}\" and no such directory. Pass a vault directory path.")
    } else {
        format!("No vault named \"{trimmed}\". Known vaults: {}. You can also pass a directory path.", names.join(", "))
    })
}

/// The vault for one tool call: the `--vault` selector, `$ZENNOTES_VAULT`,
/// or the vault the desktop app has open.
pub fn resolve_vault(selector: Option<&str>) -> Result<PathBuf, String> {
    if let Some(selector) = selector.filter(|s| !s.trim().is_empty()) {
        return resolve_vault_selector(selector);
    }
    if let Some(env) = std::env::var("ZENNOTES_VAULT").ok().filter(|v| !v.trim().is_empty()) {
        return Ok(PathBuf::from(resolve_path(env.trim())));
    }
    if let Some(root) = load_config(&app_config_dir()).vault_root.filter(|r| !r.trim().is_empty()) {
        return Ok(PathBuf::from(resolve_path(&root)));
    }
    Err("No ZenNotes vault is configured. Open ZenNotes-rs once and pick a vault, or set the ZENNOTES_VAULT environment variable.".into())
}

/* ---------- Entry ------------------------------------------------------ */

const USAGE: &str = "usage: zennotes-rs mcp [--vault <path or name>]

Runs the ZenNotes MCP server on stdin/stdout for an MCP client (Claude Code,
Claude Desktop, Codex, OpenCode). Without --vault it uses $ZENNOTES_VAULT, or
else the vault ZenNotes-rs has open. Settings -> MCP installs it into a client.";

/// `zennotes-rs mcp …`: serve until the client closes stdin. Returns the
/// process exit code.
pub fn run(args: &[String]) -> i32 {
    let mut vault: Option<String> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return 0;
            }
            "--vault" => vault = iter.next().cloned(),
            other if other.starts_with("--vault=") => vault = Some(other["--vault=".len()..].to_string()),
            // stdout is the protocol channel: everything else goes to stderr,
            // which MCP clients keep in their server logs.
            "--server" | "--token" => {
                iter.next();
                eprintln!("[zennotes-mcp] {arg} is not supported: this build serves local vaults only.");
            }
            other => eprintln!("[zennotes-mcp] ignoring unknown argument {other}"),
        }
    }

    // Try at startup and say so when it fails: waiting for the first tool
    // call hid a `--vault` typo in a client config until the assistant
    // tripped over it. The server runs anyway; every call retries.
    if let Err(message) = resolve_vault(vault.as_deref()) {
        eprintln!("[zennotes-mcp] {message} The MCP server is running anyway; every tool call returns this error until a vault resolves.");
    }
    let session = server::VaultSession::new(move || resolve_vault(vault.as_deref()));
    let handler = server::ZenNotesServer::new(session, resolve_instructions());

    let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("[zennotes-mcp] fatal: {e}");
            return 1;
        }
    };
    runtime.block_on(async move {
        use rmcp::ServiceExt;
        let service = match handler.serve(rmcp::transport::stdio()).await {
            Ok(service) => service,
            Err(e) => {
                eprintln!("[zennotes-mcp] fatal: {e}");
                return 1;
            }
        };
        match service.waiting().await {
            Ok(_) => 0,
            Err(e) => {
                eprintln!("[zennotes-mcp] fatal: {e}");
                1
            }
        }
    })
}
