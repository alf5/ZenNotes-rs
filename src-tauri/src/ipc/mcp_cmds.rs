//! Settings → MCP: the server runtime, client installs and the instructions
//! editor (upstream's `mcp:*` IPC handlers). The server itself is this binary
//! run as `zennotes-rs mcp`; see `crate::mcp`.

use serde::Serialize;

use crate::mcp::{self, integrations};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpInstructionsPayload {
    default_value: String,
    current: String,
    is_custom: bool,
    file_path: String,
}

fn instructions_payload() -> McpInstructionsPayload {
    let custom = mcp::custom_instructions();
    McpInstructionsPayload {
        default_value: mcp::DEFAULT_INSTRUCTIONS.to_string(),
        current: custom.clone().unwrap_or_else(|| mcp::DEFAULT_INSTRUCTIONS.to_string()),
        is_custom: custom.is_some(),
        file_path: mcp::instructions_file_path().to_string_lossy().into_owned(),
    }
}

#[tauri::command]
pub fn mcp_get_runtime() -> integrations::Runtime {
    integrations::runtime()
}

#[tauri::command]
pub async fn mcp_get_statuses() -> Vec<integrations::ClientStatus> {
    integrations::statuses()
}

#[tauri::command]
pub async fn mcp_install(id: String) -> Result<integrations::ClientStatus, String> {
    integrations::install(&id)
}

#[tauri::command]
pub async fn mcp_uninstall(id: String) -> Result<integrations::ClientStatus, String> {
    integrations::uninstall(&id)
}

#[tauri::command]
pub fn mcp_get_instructions() -> McpInstructionsPayload {
    instructions_payload()
}

#[tauri::command]
pub fn mcp_set_instructions(next: Option<String>) -> Result<McpInstructionsPayload, String> {
    mcp::write_custom_instructions(next.as_deref())?;
    Ok(instructions_payload())
}
