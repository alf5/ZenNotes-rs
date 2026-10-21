//! The details a bug report needs beside the version (#814): architecture,
//! operating system, what renders the UI and how this copy was installed.
//! Port of `describeInstall` (apps/desktop/src/main/updater.ts) and the arch /
//! engine fields the preload adds; read by `:version` and Settings > About.
//! Nothing here may fail: a field that cannot be answered is described as
//! unknown, never guessed.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct InstallInfo {
    pub arch: String,
    pub os: String,
    pub engine: String,
    pub install: String,
}

/// Node's `process.arch` spelling, which the renderer's report already uses.
fn node_arch() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "x86" => "ia32",
        "aarch64" => "arm64",
        "arm" => "arm",
        "powerpc64" => "ppc64",
        "s390x" => "s390x",
        "riscv64" => "riscv64",
        "loongarch64" => "loong64",
        other => other,
    }
    .to_string()
}

/// Port of `osReleasePrettyName`.
fn os_release_pretty_name(os_release: &str) -> Option<String> {
    for line in os_release.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix("PRETTY_NAME") else { continue };
        let Some(value) = rest.trim_start().strip_prefix('=') else { continue };
        let value = value.trim();
        let value = value.strip_prefix(['"', '\'']).unwrap_or(value);
        let value = value.strip_suffix(['"', '\'']).unwrap_or(value);
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

fn os_release_field(os_release: &str, key: &str) -> String {
    os_release
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{key}=")))
        .map(|v| v.trim().trim_matches(['"', '\'']).to_lowercase())
        .unwrap_or_default()
}

/// The package format a system install on this distro is, from os-release.
fn linux_format_from_os_release(os_release: &str) -> &'static str {
    let ids = format!("{} {}", os_release_field(os_release, "ID"), os_release_field(os_release, "ID_LIKE"));
    let has = |name: &str| ids.split_whitespace().any(|id| id == name);
    if has("arch") || has("archlinux") {
        "pacman package"
    } else if has("debian") || has("ubuntu") {
        "deb package"
    } else if has("fedora") || has("rhel") || has("centos") || has("suse") || has("opensuse") {
        "rpm package"
    } else {
        "Linux package (format unknown)"
    }
}

fn read_os_release() -> Option<String> {
    std::fs::read_to_string("/etc/os-release")
        .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
        .ok()
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program).args(args).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !text.is_empty()).then_some(text)
}

fn describe_os() -> String {
    if cfg!(target_os = "macos") {
        return format!("macOS {}", command_output("sw_vers", &["-productVersion"]).unwrap_or_default())
            .trim()
            .to_string();
    }
    if cfg!(target_os = "windows") {
        return "Windows".to_string();
    }
    if cfg!(target_os = "linux") {
        let pretty = read_os_release().as_deref().and_then(os_release_pretty_name);
        let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|k| k.trim().to_string())
            .ok()
            .filter(|k| !k.is_empty())
            .unwrap_or_else(|| "unknown".into());
        return format!("{} (kernel {kernel})", pretty.unwrap_or_else(|| "Linux".into()));
    }
    std::env::consts::OS.to_string()
}

fn describe_install() -> String {
    if cfg!(debug_assertions) {
        return "development build".into();
    }
    if cfg!(target_os = "macos") {
        return "macOS app bundle".into();
    }
    if cfg!(target_os = "windows") {
        return "Windows installer".into();
    }
    if std::env::var_os("APPIMAGE").is_some() {
        return "AppImage".into();
    }
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    if exe.contains("/target/release/") {
        return "built from source".into();
    }
    if exe.starts_with("/usr/") {
        return read_os_release()
            .map(|r| linux_format_from_os_release(&r))
            .unwrap_or("Linux package (format unknown)")
            .into();
    }
    "tarball or manual install".into()
}

/// What runs the renderer, e.g. `Tauri 2.9.5 (WebKitGTK 2.50.1)`.
fn describe_engine() -> String {
    let webview = match tauri::webview_version() {
        Ok(version) if cfg!(target_os = "linux") => format!(" (WebKitGTK {version})"),
        Ok(version) if cfg!(target_os = "windows") => format!(" (WebView2 {version})"),
        Ok(version) => format!(" (WebKit {version})"),
        Err(_) => String::new(),
    };
    format!("Tauri {}{webview}", tauri::VERSION)
}

#[tauri::command]
pub fn app_install_info() -> InstallInfo {
    InstallInfo { arch: node_arch(), os: describe_os(), engine: describe_engine(), install: describe_install() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_pretty_name_like_upstream() {
        assert_eq!(
            os_release_pretty_name("NAME=Arch\nPRETTY_NAME=\"Arch Linux\"\n").as_deref(),
            Some("Arch Linux")
        );
        assert_eq!(os_release_pretty_name("  PRETTY_NAME = 'Ubuntu 24.04.1 LTS'").as_deref(), Some("Ubuntu 24.04.1 LTS"));
        assert_eq!(os_release_pretty_name("PRETTY_NAME=\"\""), None);
        assert_eq!(os_release_pretty_name("NAME=x"), None);
    }

    #[test]
    fn maps_distros_to_package_formats() {
        assert_eq!(linux_format_from_os_release("ID=arch\n"), "pacman package");
        assert_eq!(linux_format_from_os_release("ID=endeavouros\nID_LIKE=arch\n"), "pacman package");
        assert_eq!(linux_format_from_os_release("ID=ubuntu\nID_LIKE=debian\n"), "deb package");
        assert_eq!(linux_format_from_os_release("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n"), "rpm package");
        assert_eq!(linux_format_from_os_release("ID=nixos\n"), "Linux package (format unknown)");
    }
}
