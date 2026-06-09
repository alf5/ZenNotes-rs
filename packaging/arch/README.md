# Arch Linux packaging (`syncnotes`)

A `makepkg`/AUR setup that builds the **SynNotes** Tauri desktop app from source and
links against Arch's own system libraries (`webkit2gtk-4.1`, `gtk3`) — nothing is bundled.

The binary is installed as **`/usr/bin/syncnotes`**.

## Files
- `PKGBUILD` — builds the app (Rust + Vite frontend) and installs the binary, `.desktop`, icons, and license.
- `syncnotes.desktop` — application launcher entry.
- `.SRCINFO` — AUR metadata (regenerate with `makepkg --printsrcinfo > .SRCINFO` if you edit the PKGBUILD).

## Dependencies (Arch packages)
- **Runtime:** `webkit2gtk-4.1`, `gtk3`, `hicolor-icon-theme`
- **Build:** `rust` (provides cargo), `npm`, `nodejs`, `pkgconf`, `git`

## Build & install locally
```bash
cd packaging/arch
makepkg -si        # build + install (pulls deps via pacman)
```

## Publish to the AUR
The AUR repo for a package contains just the `PKGBUILD`, `.SRCINFO`, and any local
source files (here, `syncnotes.desktop`) at its root:
```bash
git clone ssh://aur@aur.archlinux.org/syncnotes.git
cp PKGBUILD .SRCINFO syncnotes.desktop syncnotes/
cd syncnotes && git add -A && git commit -m "syncnotes 2.1.0-1" && git push
```
Users then install with an AUR helper:
```bash
paru -S syncnotes      # or: yay -S syncnotes
```

## Notes
- The source is pinned to the **`v2.1.0` git tag** (`git+…#tag=v2.1.0`), so the hash is `SKIP`
  (git sources are verified by the tag/commit, not a tarball checksum).
- This is a **from-source** package — building compiles Rust + the frontend (a few minutes).
  WebKitGTK cannot be statically linked, so there is no fully self-contained single binary;
  the app uses Arch's system `webkit2gtk-4.1` at runtime (the standard Tauri-on-Linux model).
- For a no-compile install you could instead extract the release `.deb`/`.AppImage`, but this
  source PKGBUILD is the canonical way to use Arch's system packages for the dependencies.
