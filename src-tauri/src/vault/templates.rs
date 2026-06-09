//! Custom-template file I/O — port of apps/desktop/src/main/templates.ts.
//! Templates are plain `.md` files under `.zennotes/templates/`. Parse-free:
//! the renderer owns frontmatter parsing; this only moves raw bytes.

use std::fs;
use std::path::{Path, PathBuf};

use crate::ipc::types::{CustomTemplateFile, WriteTemplateInput};

const TEMPLATES_REL_DIR: &str = ".zennotes/templates";

fn templates_dir(root: &Path) -> PathBuf {
    root.join(".zennotes").join("templates")
}

fn source_path_for_name(name: &str) -> String {
    format!("{TEMPLATES_REL_DIR}/{name}")
}

fn filename_stem(source_path: &str) -> String {
    let file = source_path.rsplit('/').next().unwrap_or(source_path);
    file.strip_suffix(".md")
        .or_else(|| file.strip_suffix(".MD"))
        .unwrap_or(file)
        .to_string()
}

fn safe_slug(slug: &str) -> String {
    let lowered = slug.to_lowercase();
    // Replace runs of non [a-z0-9-] with a single dash, trim dashes.
    let mut out = String::new();
    let mut prev_dash = false;
    for c in lowered.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' {
            out.push(c);
            prev_dash = c == '-';
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let cleaned = out.trim_matches('-').to_string();
    if cleaned.is_empty() {
        "template".to_string()
    } else {
        cleaned
    }
}

/// Resolve a vault-relative template sourcePath to an absolute path, rejecting
/// anything outside the flat templates dir (no traversal, no subdirs).
fn resolve_template_path(root: &Path, source_path: &str) -> Result<PathBuf, String> {
    let dir = templates_dir(root);
    let abs = PathBuf::from(crate::vault::config::resolve_path(
        &root.join(source_path).to_string_lossy(),
    ));
    let dir_abs = crate::vault::config::resolve_path(&dir.to_string_lossy());
    let abs_str = abs.to_string_lossy().to_string();
    let sep = std::path::MAIN_SEPARATOR;
    // Must be a direct child of the templates dir.
    let rel = abs_str.strip_prefix(&format!("{dir_abs}{sep}"));
    match rel {
        Some(r) if !r.contains(sep) => {}
        _ => return Err(format!("Refusing template path outside templates dir: {source_path}")),
    }
    if !abs_str.to_lowercase().ends_with(".md") {
        return Err(format!("Template path must be a .md file: {source_path}"));
    }
    Ok(abs)
}

fn unique_slug(dir: &Path, base: &str, previous_source_path: Option<&str>) -> String {
    let prev_stem = previous_source_path.map(filename_stem);
    let mut candidate = base.to_string();
    let mut n = 2;
    loop {
        if Some(&candidate) == prev_stem.as_ref() {
            return candidate;
        }
        if !dir.join(format!("{candidate}.md")).exists() {
            return candidate;
        }
        candidate = format!("{base}-{n}");
        n += 1;
    }
}

pub fn list_custom_templates(root: &Path) -> Vec<CustomTemplateFile> {
    let dir = templates_dir(root);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<CustomTemplateFile> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || !name.to_lowercase().ends_with(".md") {
            continue;
        }
        if let Ok(raw) = fs::read_to_string(entry.path()) {
            files.push(CustomTemplateFile { source_path: source_path_for_name(&name), raw });
        }
    }
    files.sort_by(|a, b| a.source_path.cmp(&b.source_path));
    files
}

pub fn read_custom_template(root: &Path, source_path: &str) -> Result<String, String> {
    let abs = resolve_template_path(root, source_path)?;
    fs::read_to_string(abs).map_err(|e| format!("read failed: {e}"))
}

pub fn write_custom_template(
    root: &Path,
    input: &WriteTemplateInput,
) -> Result<CustomTemplateFile, String> {
    let dir = templates_dir(root);
    fs::create_dir_all(&dir).map_err(|e| format!("mkdir failed: {e}"))?;
    let slug = unique_slug(&dir, &safe_slug(&input.slug), input.previous_source_path.as_deref());
    let abs = dir.join(format!("{slug}.md"));
    fs::write(&abs, &input.raw).map_err(|e| format!("write failed: {e}"))?;
    if let Some(prev) = &input.previous_source_path {
        if let Ok(prev_abs) = resolve_template_path(root, prev) {
            if prev_abs != abs {
                let _ = fs::remove_file(prev_abs);
            }
        }
    }
    Ok(CustomTemplateFile {
        source_path: source_path_for_name(&format!("{slug}.md")),
        raw: input.raw.clone(),
    })
}

pub fn delete_custom_template(root: &Path, source_path: &str) -> Result<(), String> {
    let abs = resolve_template_path(root, source_path)?;
    let _ = fs::remove_file(abs);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::layout;

    fn vault() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        layout::ensure_vault_layout(dir.path()).unwrap();
        dir
    }

    #[test]
    fn write_list_read_delete() {
        let v = vault();
        let written = write_custom_template(
            v.path(),
            &WriteTemplateInput {
                slug: "My ADR!".into(),
                raw: "---\nname: ADR\n---\n# {{title}}".into(),
                previous_source_path: None,
            },
        )
        .unwrap();
        assert_eq!(written.source_path, ".zennotes/templates/my-adr.md");

        let list = list_custom_templates(v.path());
        assert_eq!(list.len(), 1);
        let raw = read_custom_template(v.path(), &written.source_path).unwrap();
        assert!(raw.contains("{{title}}"));

        delete_custom_template(v.path(), &written.source_path).unwrap();
        assert!(list_custom_templates(v.path()).is_empty());
    }

    #[test]
    fn dedupes_slug() {
        let v = vault();
        let a = write_custom_template(v.path(), &WriteTemplateInput { slug: "note".into(), raw: "a".into(), previous_source_path: None }).unwrap();
        let b = write_custom_template(v.path(), &WriteTemplateInput { slug: "note".into(), raw: "b".into(), previous_source_path: None }).unwrap();
        assert_eq!(a.source_path, ".zennotes/templates/note.md");
        assert_eq!(b.source_path, ".zennotes/templates/note-2.md");
    }

    #[test]
    fn rejects_traversal() {
        let v = vault();
        assert!(read_custom_template(v.path(), ".zennotes/templates/../../secret.md").is_err());
    }
}
