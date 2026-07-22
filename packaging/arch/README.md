# Arch Linux packaging (`zennotes-rs`)

A `makepkg`/AUR setup that builds the **ZenNotes-rs** Tauri desktop app from source and
links against Arch's own system libraries (`webkit2gtk-4.1`, `gtk3`) — nothing is bundled.

The binary is installed as **`/usr/bin/zennotes-rs`**.

## Files

- `PKGBUILD` — builds the app (Rust + Vite frontend) and installs the binary, `.desktop`, icons, and license.
- `zennotes-rs.desktop` — application launcher entry.
- `.SRCINFO` — AUR metadata (regenerate with `makepkg --printsrcinfo > .SRCINFO` if you edit the PKGBUILD).

## Dependencies (Arch packages)

- **Runtime:** `webkit2gtk-4.1`, `gtk3`, `hicolor-icon-theme`
- **Build:** `rust` (provides cargo), `bun`, `nodejs`, `pkgconf`, `git`

## Build & install locally

```bash
cd packaging/arch
makepkg -si        # build + install (pulls deps via pacman)
```

## Publish to the AUR

The AUR repo for a package contains just the `PKGBUILD`, `.SRCINFO`, and any local
source files (here, `zennotes-rs.desktop`) at its root:

```bash
git clone ssh://aur@aur.archlinux.org/zennotes-rs.git
cp PKGBUILD .SRCINFO zennotes-rs.desktop zennotes-rs/
cd zennotes-rs && git add -A && git commit -m "zennotes-rs 2.15.0-1" && git push
```

Users then install with an AUR helper:

```bash
paru -S zennotes-rs      # or: yay -S zennotes-rs
```

## Notes

- The source is pinned to the **`v2.15.0` git tag** (`git+…#tag=v2.15.0`), so the hash is `SKIP`
  (git sources are verified by the tag/commit, not a tarball checksum).
- This is a **from-source** package — building compiles Rust + the frontend (a few minutes).
  WebKitGTK cannot be statically linked, so there is no fully self-contained single binary;
  the app uses Arch's system `webkit2gtk-4.1` at runtime (the standard Tauri-on-Linux model).
- For a no-compile install you could instead extract the release `.deb`/`.AppImage`, but this
  source PKGBUILD is the canonical way to use Arch's system packages for the dependencies.
