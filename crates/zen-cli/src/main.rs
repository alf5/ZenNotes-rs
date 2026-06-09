//! `syncnotes` — the SynNotes command-line client.
//!
//! A small, dependency-light binary that shares all vault logic with the
//! desktop app via `synnotes-core` (no Tauri/WebKit), so it builds as a fully
//! static `x86_64-unknown-linux-musl` binary that runs on any Linux distro.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use synnotes_core::search::{self, ToolPaths};
use synnotes_core::types::NoteMeta;
use synnotes_core::vault::{config, crud, listing, notes, settings, tasks};

const APP_IDENTIFIER: &str = "com.synnotes.app";

#[derive(Parser)]
#[command(name = "syncnotes", version, about = "SynNotes command-line client", long_about = None)]
struct Cli {
    /// Vault path (overrides the vault configured in the SynNotes app).
    #[arg(long, global = true)]
    vault: Option<String>,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Vault information / known vaults.
    Vault {
        #[command(subcommand)]
        action: VaultCmd,
    },
    /// List notes.
    List {
        #[arg(long)]
        folder: Option<String>,
        #[arg(long)]
        tag: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        #[arg(long)]
        json: bool,
    },
    /// Print a note's body.
    Read { path: String },
    /// Create a new note.
    Create {
        #[arg(long, default_value = "inbox")]
        folder: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, default_value = "")]
        subpath: String,
    },
    /// Write a note's body (from --content, else stdin).
    Write {
        path: String,
        #[arg(long)]
        content: Option<String>,
    },
    /// Append (or --prepend) text to a note (from --content, else stdin).
    Append {
        path: String,
        #[arg(long)]
        content: Option<String>,
        #[arg(long)]
        prepend: bool,
    },
    /// Rename a note.
    Rename { path: String, title: String },
    /// Move a note to a folder/subpath.
    Move {
        path: String,
        folder: String,
        #[arg(default_value = "")]
        subpath: String,
    },
    /// Move a note to the archive.
    Archive { path: String },
    /// Restore an archived note to the inbox.
    Unarchive { path: String },
    /// Move a note to the trash.
    Trash { path: String },
    /// Restore a note from the trash.
    Restore { path: String },
    /// Permanently delete a note.
    Delete { path: String },
    /// Duplicate a note.
    Duplicate { path: String },
    /// Full-text search across the vault.
    Search {
        query: String,
        #[arg(long)]
        json: bool,
    },
    /// List all tasks across the vault.
    Tasks {
        #[arg(long)]
        json: bool,
    },
    /// List folders.
    Folders {
        #[arg(long)]
        json: bool,
    },
    /// List tags with note counts.
    Tags {
        #[arg(long)]
        json: bool,
    },
    /// Quick-capture a note (from --content, else stdin).
    Capture {
        #[arg(long, default_value = "inbox")]
        folder: String,
        #[arg(long)]
        content: Option<String>,
    },
}

#[derive(Subcommand)]
enum VaultCmd {
    /// Show the active vault.
    Info,
    /// List vaults remembered by the app.
    List,
}

fn config_dir() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|d| d.join(APP_IDENTIFIER))
        .ok_or_else(|| "Could not resolve the OS config directory".to_string())
}

/// Resolve the vault root from --vault, else the app's configured vault.
fn vault_root(cli_vault: &Option<String>) -> Result<PathBuf, String> {
    if let Some(v) = cli_vault {
        return Ok(PathBuf::from(config::resolve_path(v)));
    }
    let cfg = config::load_config(&config_dir()?);
    cfg.vault_root
        .map(PathBuf::from)
        .ok_or_else(|| "No vault configured. Open one in SynNotes, or pass --vault <path>.".to_string())
}

fn content_or_stdin(content: Option<String>) -> String {
    match content {
        Some(s) => s,
        None => {
            let mut s = String::new();
            let _ = std::io::stdin().read_to_string(&mut s);
            s
        }
    }
}

fn print_note_line(m: &NoteMeta) {
    println!("{:<8} {}", m.folder, m.path);
}

fn run() -> Result<(), String> {
    let cli = Cli::parse();
    let v = &cli.vault;

    match cli.command {
        Cmd::Vault { action } => match action {
            VaultCmd::Info => {
                let root = vault_root(v)?;
                let s = settings::get_vault_settings(&root);
                let count = listing::list_notes(&root).iter().filter(|m| m.folder != "trash").count();
                println!("root:               {}", root.display());
                println!("name:               {}", root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
                println!("primaryNotesLocation: {}", s.primary_notes_location);
                println!("notes:              {count}");
            }
            VaultCmd::List => {
                let cfg = config::load_config(&config_dir()?);
                if cfg.local_vaults.is_empty() {
                    println!("(no vaults remembered — open one in SynNotes)");
                }
                for lv in &cfg.local_vaults {
                    println!("{}\t{}", lv.name, lv.root);
                }
            }
        },
        Cmd::List { folder, tag, limit, json } => {
            let root = vault_root(v)?;
            let mut notes: Vec<NoteMeta> = listing::list_notes(&root)
                .into_iter()
                .filter(|m| folder.as_deref().map(|f| m.folder == f).unwrap_or(true))
                .filter(|m| tag.as_deref().map(|t| m.tags.iter().any(|x| x == t)).unwrap_or(true))
                .collect();
            notes.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            if let Some(n) = limit {
                notes.truncate(n);
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&notes).map_err(|e| e.to_string())?);
            } else {
                for m in &notes {
                    print_note_line(m);
                }
            }
        }
        Cmd::Read { path } => {
            let root = vault_root(v)?;
            print!("{}", notes::read_note(&root, &path)?.body);
        }
        Cmd::Create { folder, title, subpath } => {
            let root = vault_root(v)?;
            let meta = crud::create_note(&root, &folder, title.as_deref(), &subpath)?;
            println!("{}", meta.path);
        }
        Cmd::Write { path, content } => {
            let root = vault_root(v)?;
            let meta = crud::write_note(&root, &path, &content_or_stdin(content))?;
            println!("{}", meta.path);
        }
        Cmd::Append { path, content, prepend } => {
            let root = vault_root(v)?;
            let pos = if prepend { "start" } else { "end" };
            let meta = crud::append_to_note(&root, &path, &content_or_stdin(content), pos)?;
            println!("{}", meta.path);
        }
        Cmd::Rename { path, title } => {
            let root = vault_root(v)?;
            println!("{}", crud::rename_note(&root, &path, &title)?.path);
        }
        Cmd::Move { path, folder, subpath } => {
            let root = vault_root(v)?;
            println!("{}", crud::move_note(&root, &path, &folder, &subpath)?.path);
        }
        Cmd::Archive { path } => {
            let root = vault_root(v)?;
            println!("{}", crud::archive_note(&root, &path)?.path);
        }
        Cmd::Unarchive { path } => {
            let root = vault_root(v)?;
            println!("{}", crud::unarchive_note(&root, &path)?.path);
        }
        Cmd::Trash { path } => {
            let root = vault_root(v)?;
            println!("{}", crud::move_to_trash(&root, &path)?.path);
        }
        Cmd::Restore { path } => {
            let root = vault_root(v)?;
            println!("{}", crud::restore_from_trash(&root, &path)?.path);
        }
        Cmd::Delete { path } => {
            let root = vault_root(v)?;
            crud::delete_note(&root, &path)?;
            println!("deleted {path}");
        }
        Cmd::Duplicate { path } => {
            let root = vault_root(v)?;
            println!("{}", crud::duplicate_note(&root, &path)?.path);
        }
        Cmd::Search { query, json } => {
            let root = vault_root(v)?;
            let results = search::search_vault_text(&root, &query, "auto", &ToolPaths { ripgrep: None, fzf: None });
            if json {
                println!("{}", serde_json::to_string_pretty(&results).map_err(|e| e.to_string())?);
            } else {
                for r in &results {
                    println!("{}:{}: {}", r.path, r.line_number, r.line_text);
                }
            }
        }
        Cmd::Tasks { json } => {
            let root = vault_root(v)?;
            let all = tasks::scan_all_tasks(&root);
            if json {
                println!("{}", serde_json::to_string_pretty(&all).map_err(|e| e.to_string())?);
            } else {
                for t in &all {
                    let mark = if t.checked { "x" } else { " " };
                    println!("[{}] {}  ({})", mark, t.content, t.source_path);
                }
            }
        }
        Cmd::Folders { json } => {
            let root = vault_root(v)?;
            let f = listing::list_folders(&root);
            if json {
                println!("{}", serde_json::to_string_pretty(&f).map_err(|e| e.to_string())?);
            } else {
                for entry in &f {
                    println!("{}/{}", entry.folder, entry.subpath);
                }
            }
        }
        Cmd::Tags { json } => {
            let root = vault_root(v)?;
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for m in listing::list_notes(&root) {
                for tag in m.tags {
                    *counts.entry(tag).or_insert(0) += 1;
                }
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&counts).map_err(|e| e.to_string())?);
            } else {
                for (tag, n) in &counts {
                    println!("{n:>5}  #{tag}");
                }
            }
        }
        Cmd::Capture { folder, content } => {
            let root = vault_root(v)?;
            let title = chrono::Local::now().format("%Y-%m-%d %H%M%S").to_string();
            let meta = crud::create_note(&root, &folder, Some(&title), "")?;
            let body = content_or_stdin(content);
            if !body.trim().is_empty() {
                crud::write_note(&root, &meta.path, &body)?;
            }
            println!("{}", meta.path);
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("syncnotes: {e}");
            ExitCode::FAILURE
        }
    }
}
