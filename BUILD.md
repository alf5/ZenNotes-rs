# Building ZenNotes-rs

```bash
bun install
bun tauri:build
```

This produces, under `src-tauri/target/release/`:

- the `zennotes-rs` binary
- `bundle/deb/ZenNotes-rs_*.deb`
- `bundle/rpm/ZenNotes-rs-*.rpm`
- `bundle/appimage/ZenNotes-rs_*.AppImage`

The Rust build and the **deb/rpm** bundles work out of the box on any Linux
machine with the usual Tauri prerequisites. The **AppImage** target needs two
extra things on rolling-release / bleeding-edge distros (Arch, and anything with
a very recent `binutils` + `gdk-pixbuf`). Both are worked around below.

---

## Is this an Arch-only problem?

Mostly, yes — it's a "your system libraries are newer than `linuxdeploy`"
problem, and Arch is the common place to hit it. Tauri's AppImage bundler
downloads a prebuilt `linuxdeploy` (and its GTK plugin) into `~/.cache/tauri/`.
That tool was built against older system libraries, so on a modern Arch box two
mismatches surface:

1. **`strip` can't read `.relr.dyn`.** Recent `binutils`/`glibc` emit compact
   relative relocations (`SHT_RELR`, ELF section type `0x13`). The `strip`
   bundled inside `linuxdeploy` is too old to parse them, so it fails on every
   system `.so` it tries to strip:

   ```
   strip: ... unknown type [0x13] section `.relr.dyn'
   strip: Unable to recognise the format of the input file ...
   ```

2. **gdk-pixbuf no longer ships on-disk loaders.** `gdk-pixbuf2` >= ~2.42
   (Arch currently ships 2.44.x) compiles its image loaders *into*
   `libgdk_pixbuf`, so `/usr/lib/gdk-pixbuf-2.0/2.10.0/` does not exist. But
   `pkg-config` still advertises that path, and `linuxdeploy-plugin-gtk` blindly
   `cp -r`'s it, then aborts under `set -e`:

   ```
   [gtk/stderr] cp: cannot stat '/usr/lib/gdk-pixbuf-2.0/2.10.0'
   ERROR: Failed to run plugin: gtk (exit code: 1)
   ```

Distros that ship older toolchains (e.g. Ubuntu LTS, Debian stable — what most
CI runners use) generally **do not** hit either issue, because their libraries
are close in age to what `linuxdeploy` was built against. So if you build the
AppImage in CI on Ubuntu, you likely don't need any of the workarounds below.

---

## Workaround 1 — `NO_STRIP` (already in the repo)

`linuxdeploy` honours the `NO_STRIP` environment variable. The `tauri:build`
script in `package.json` sets it:

```json
"tauri:build": "NO_STRIP=true tauri build"
```

This skips the broken strip pass entirely. The libraries are already release
builds, so the only cost is a slightly larger AppImage. Nothing to do — it's
baked in.

## Workaround 2 — create the empty gdk-pixbuf loader dir (one-time, per machine)

Give `linuxdeploy-plugin-gtk` the directory it expects. It's empty (the loaders
live inside `libgdk_pixbuf`), so this is harmless — it just lets the `cp`
succeed:

```bash
sudo install -d /usr/lib/gdk-pixbuf-2.0/2.10.0/loaders
```

Notes:

- This is a **per-machine, one-time** step, not a repo change. It can't live in
  the repo because the failure is in a tool Tauri downloads to `~/.cache/tauri/`
  at build time, not in our source.
- The `gdk-pixbuf2` package does **not** own this path (`pacman -Ql gdk-pixbuf2`
  lists nothing under `2.10.0/`), so nothing will remove the directory on
  upgrade. It survives clearing `~/.cache/tauri/`.

With both workarounds in place, `bun tauri:build` produces all three bundles
using the stock, unmodified `linuxdeploy` plugin.

---

## If you only need deb/rpm

The AppImage is optional. To skip it (and avoid the above entirely), set the
bundle targets in `src-tauri/tauri.conf.json`:

```jsonc
"bundle": {
  "targets": ["deb", "rpm"]   // was: "all"
}
```

Arch users are served by the AUR package in `packaging/arch/` regardless.

## Blank window / instant crash on Wayland (Hyprland, NVIDIA)

If `bun run tauri:dev` dies immediately with:

```
Gdk-Message: Error 71 (Protocol error) dispatching to Wayland display.
```

that's the well-known WebKitGTK DMA-BUF renderer issue on some Wayland
compositor/GPU combinations (observed here on Hyprland). Launch with:

```sh
WEBKIT_DISABLE_DMABUF_RENDERER=1 bun run tauri:dev
```

The same variable works for the packaged binary if it hits the same crash.
