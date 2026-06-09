//! Demo-tour seeding — port of `generateDemoTour` / `removeDemoTour` from
//! apps/desktop/src/main/vault.ts. The note/asset content is the verbatim data
//! from the Electron build (apps/desktop/src/main/demo-tour-data.ts), extracted
//! to JSON and embedded at compile time.

use std::fs;
use std::path::Path;

use serde::Deserialize;
use std::sync::LazyLock;

use crate::types::VaultDemoTourResult;
use crate::vault::layout::ensure_vault_layout;
use crate::vault::notes::resolve_safe;

const DEMO_TOUR_DIR: &str = "inbox/demo";
const DEMO_DATA_JSON: &str = include_str!("../../demo-tour-data.json");

#[derive(Deserialize)]
struct DemoFile {
    path: String,
    body: String,
}

#[derive(Deserialize)]
struct DemoData {
    notes: Vec<DemoFile>,
    assets: Vec<DemoFile>,
}

static DEMO_DATA: LazyLock<DemoData> =
    LazyLock::new(|| serde_json::from_str(DEMO_DATA_JSON).expect("valid embedded demo data"));

pub fn generate_demo_tour(root: &Path) -> Result<VaultDemoTourResult, String> {
    ensure_vault_layout(root).map_err(|e| format!("layout failed: {e}"))?;
    for note in &DEMO_DATA.notes {
        let abs = resolve_safe(root, &note.path)?;
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
        }
        fs::write(&abs, &note.body).map_err(|e| format!("write failed: {e}"))?;
    }
    for asset in &DEMO_DATA.assets {
        let abs = resolve_safe(root, &asset.path)?;
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("mkdir failed: {e}"))?;
        }
        fs::write(&abs, &asset.body).map_err(|e| format!("write failed: {e}"))?;
    }
    Ok(VaultDemoTourResult {
        note_paths: DEMO_DATA.notes.iter().map(|n| n.path.clone()).collect(),
        asset_paths: DEMO_DATA.assets.iter().map(|a| a.path.clone()).collect(),
    })
}

pub fn remove_demo_tour(root: &Path) -> Result<VaultDemoTourResult, String> {
    for note in &DEMO_DATA.notes {
        if let Ok(abs) = resolve_safe(root, &note.path) {
            let _ = fs::remove_file(abs);
        }
    }
    for asset in &DEMO_DATA.assets {
        if let Ok(abs) = resolve_safe(root, &asset.path) {
            let _ = fs::remove_file(abs);
        }
    }
    // Remove the demo dir if empty.
    if let Ok(dir) = resolve_safe(root, DEMO_TOUR_DIR) {
        if let Ok(mut entries) = fs::read_dir(&dir) {
            if entries.next().is_none() {
                let _ = fs::remove_dir(&dir);
            }
        }
    }
    Ok(VaultDemoTourResult {
        note_paths: DEMO_DATA.notes.iter().map(|n| n.path.clone()).collect(),
        asset_paths: DEMO_DATA.assets.iter().map(|a| a.path.clone()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_then_remove() {
        let dir = tempfile::tempdir().unwrap();
        let result = generate_demo_tour(dir.path()).unwrap();
        assert!(!result.note_paths.is_empty());
        assert!(dir.path().join("inbox/demo/00 — Start Here.md").is_file());
        remove_demo_tour(dir.path()).unwrap();
        assert!(!dir.path().join("inbox/demo/00 — Start Here.md").exists());
    }
}
