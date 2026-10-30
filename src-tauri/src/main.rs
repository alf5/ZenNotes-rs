// Prevents an additional console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `zennotes-rs mcp`: the MCP server an assistant's client spawns over
    // stdio. Dispatched before anything starts Tauri, so it runs headless.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("mcp") {
        std::process::exit(zennotes_rs_lib::run_mcp(&args[1..]));
    }
    zennotes_rs_lib::run()
}
