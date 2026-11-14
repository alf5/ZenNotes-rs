//! The shared scoped-access contract (ZenNotes/tui#21), run against this
//! engine the way upstream's scope-contract.test.ts runs it against its own:
//! a real vault on disk per case, every call through `server::run_tool`.
//! Upstream's bundled server and the Go `zn mcp` answer the same fixture,
//! packages/bridge-contract/fixtures/mcp-scope.json, with the same bytes.
//!
//! The cases on the `remapped` vault are skipped: this port does not honour
//! remapped system folders (vault.json `systemFolderPaths`) anywhere, the MCP
//! server included (see ops.rs).

use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use super::scope::{compare_paths, new_scope, ScopeFlags};
use super::server::{offered_tool_annotations, run_tool, ToolOutput};
use super::assets::AssetPart;

const FIXTURE: &str = include_str!("../../../packages/bridge-contract/fixtures/mcp-scope.json");
const SKIPPED_VAULTS: [&str; 1] = ["remapped"];

fn seed(base: &Path, vault: &Value) -> std::path::PathBuf {
    let root = base.join("vault");
    let write = |dir: &Path, files: Option<&Map<String, Value>>| {
        for (rel, body) in files.into_iter().flatten() {
            let abs = dir.join(rel);
            fs::create_dir_all(abs.parent().unwrap()).unwrap();
            fs::write(abs, body.as_str().unwrap()).unwrap();
        }
    };
    write(&root, vault["files"].as_object());
    write(&base.join("outside"), vault["outside"].as_object());
    for (rel, target) in vault["symlinks"].as_object().into_iter().flatten() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target.as_str().unwrap(), root.join(rel)).unwrap();
        #[cfg(not(unix))]
        let _ = (rel, target);
    }
    root
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().map(|a| a.iter().map(|x| x.as_str().unwrap().to_string()).collect()).unwrap_or_default()
}

fn sorted(mut values: Vec<String>) -> Vec<String> {
    values.sort_by(|a, b| compare_paths(a, b));
    values
}

/// Every key the fixture names matches; arrays match element for element.
fn expect_match(actual: &Value, expected: &Value, at: &str) {
    match expected {
        Value::Array(items) => {
            let list = actual.as_array().unwrap_or_else(|| panic!("{at}: not an array: {actual}"));
            assert_eq!(list.len(), items.len(), "{at}: length");
            for (i, item) in items.iter().enumerate() {
                expect_match(&list[i], item, &format!("{at}[{i}]"));
            }
        }
        Value::Object(map) => {
            assert!(actual.is_object(), "{at}: not an object: {actual}");
            for (key, value) in map {
                expect_match(actual.get(key).unwrap_or(&Value::Null), value, &format!("{at}.{key}"));
            }
        }
        _ => assert_eq!(actual, expected, "{at}"),
    }
}

fn flags_of(v: &Value) -> Option<ScopeFlags> {
    if v.is_null() {
        return None;
    }
    Some(ScopeFlags {
        allow: strings(&v["allow"]),
        deny: strings(&v["deny"]),
        access: v["access"].as_str().map(str::to_string),
        read_only: v["readOnly"].as_bool().unwrap_or(false),
    })
}

/// What a client reads from a call: every text part, and whether it failed.
fn call(name: &str, args: &Map<String, Value>, root: &Path, scope: Option<&super::scope::Scope>) -> (String, bool) {
    match run_tool(name, args, root, scope, "Scope Contract", None) {
        Ok(ToolOutput::Value(Value::String(text))) => (text, false),
        Ok(ToolOutput::Value(value)) => (serde_json::to_string_pretty(&value).unwrap(), false),
        Ok(ToolOutput::Parts(parts)) => (
            parts
                .iter()
                .map(|p| match p {
                    AssetPart::Text(t) => t.as_str(),
                    _ => "",
                })
                .collect(),
            false,
        ),
        Err(message) if message.starts_with("Unknown tool: ") => (message, true),
        Err(message) => (format!("Error: {message}"), true),
    }
}

#[test]
fn shared_fixture_tool_cases() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let mut ran = 0;
    for case in fixture["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        let vault_name = case["vault"].as_str().unwrap();
        if SKIPPED_VAULTS.contains(&vault_name) {
            continue;
        }
        ran += 1;
        let base = tempfile::tempdir().unwrap();
        let root = seed(base.path(), &fixture["vaults"][vault_name]);
        let scope = flags_of(&case["flags"]).map(|f| new_scope(&f).unwrap());
        let active = scope.as_ref().filter(|s| super::scope::is_active(Some(s)));

        if let Some(tools) = case.get("tools") {
            let listed = offered_tool_annotations(active);
            let names = sorted(listed.iter().map(|(n, _)| n.clone()).collect());
            if let Some(count) = tools["count"].as_u64() {
                assert_eq!(names.len() as u64, count, "{id}: tool count");
            }
            if !tools["exactly"].is_null() {
                assert_eq!(names, sorted(strings(&tools["exactly"])), "{id}: tools");
            }
            for name in strings(&tools["include"]) {
                assert!(names.contains(&name), "{id}: lists {name}");
            }
            for name in strings(&tools["absent"]) {
                assert!(!names.contains(&name), "{id}: does not list {name}");
            }
            for (name, annotations) in tools["annotations"].as_object().into_iter().flatten() {
                let got = listed.iter().find(|(n, _)| n == name).map(|(_, a)| a.clone());
                assert_eq!(got.as_ref(), Some(annotations), "{id}: annotations of {name}");
            }
        }

        let root_text = super::ops::abs_root(&root).to_string_lossy().into_owned();
        for c in case["calls"].as_array().unwrap() {
            let tool = c["tool"].as_str().unwrap();
            let args = c["args"].as_object().cloned().unwrap_or_default();
            let at = format!("{id}: {tool} {}", Value::Object(args.clone()));
            let (text, failed) = call(tool, &args, &root, active);
            let e = &c["expect"];
            if e["notOffered"].as_bool() == Some(true) {
                assert!(failed, "{at}: offered");
                continue;
            }
            if let Some(error) = e["error"].as_str() {
                assert!(failed, "{at}: succeeded with {text}");
                assert_eq!(text, format!("Error: {error}"), "{at}");
            }
            let needs_success = e["ok"].as_bool() == Some(true)
                || !e["paths"].is_null()
                || !e["folders"].is_null()
                || e.get("match").is_some()
                || !e["absentKeys"].is_null();
            if needs_success {
                assert!(!failed, "{at}: {text}");
            }
            if !e["paths"].is_null() {
                let notes: Value = serde_json::from_str(&text).unwrap();
                let paths: Vec<String> =
                    notes.as_array().unwrap().iter().map(|n| n["path"].as_str().unwrap().to_string()).collect();
                assert_eq!(sorted(paths), sorted(strings(&e["paths"])), "{at}");
            }
            if !e["folders"].is_null() {
                let folders: Value = serde_json::from_str(&text).unwrap();
                let got: Vec<String> = folders
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| format!("{}:{}", f["folder"].as_str().unwrap(), f["subpath"].as_str().unwrap()))
                    .collect();
                assert_eq!(sorted(got), sorted(strings(&e["folders"])), "{at}");
            }
            if let Some(expected) = e.get("match") {
                expect_match(&serde_json::from_str::<Value>(&text).unwrap(), expected, &at);
            }
            for key in strings(&e["absentKeys"]) {
                let object: Value = serde_json::from_str(&text).unwrap();
                assert!(object.get(&key).is_none(), "{at}: has {key}");
            }
            let spellings = |needle: &str| -> Vec<String> {
                if needle.contains("$VAULT") {
                    let escaped = serde_json::to_string(&root_text).unwrap();
                    let escaped = escaped[1..escaped.len() - 1].to_string();
                    let mut out = vec![needle.replace("$VAULT", &root_text)];
                    if escaped != root_text {
                        out.push(needle.replace("$VAULT", &escaped));
                    }
                    out
                } else {
                    vec![needle.to_string()]
                }
            };
            for needle in strings(&e["contains"]) {
                assert!(spellings(&needle).iter().any(|s| text.contains(s)), "{at}: contains {needle} in {text}");
            }
            for needle in strings(&e["excludes"]) {
                for s in spellings(&needle) {
                    assert!(!text.contains(&s), "{at}: leaks {s} in {text}");
                }
            }
        }

        let disk = &case["disk"];
        for rel in strings(&disk["exists"]) {
            assert!(fs::symlink_metadata(root.join(&rel)).is_ok(), "{id}: {rel} exists");
        }
        for rel in strings(&disk["absent"]) {
            assert!(fs::symlink_metadata(root.join(&rel)).is_err(), "{id}: {rel} is absent");
        }
        for (rel, body) in disk["equals"].as_object().into_iter().flatten() {
            assert_eq!(fs::read_to_string(root.join(rel)).unwrap(), body.as_str().unwrap(), "{id}: {rel}");
        }
    }
    assert!(ran > 20, "only {ran} cases ran");
}
