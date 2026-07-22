# Gap analysis: upstream ZenNotes v2.1.0 → v2.15.0

Status of the mirror as of 2026-07-22. Phase 1 (re-vendor + frontend parity) is
**done**; this document is the work plan for the Rust backend (phase 2+).

Upstream reference clone: `~/delme/zennotes` (tags `v2.1.0` … `v2.15.0`).
All `apps/desktop/...` references below are upstream files — the spec for each
Rust implementation, exactly like the original port.

## Phase 1 — completed

- `packages/{app-core,bridge-contract,shared-domain}` re-vendored **verbatim**
  from upstream `v2.15.0` (they were verbatim v2.1.0 before; keep it that way —
  never patch vendored code, port-specific code lives in `src/` + `src-tauri/`).
- Deps synced (`bun install`): new frontend-only heavyweights are
  `@excalidraw/excalidraw` and the `@myriaddreamin/typst*` WASM trio;
  `shared-domain` gained `lz-string`; `jsdom` added at the root for tests.
- `vite.config.ts` re-mirrored from upstream `apps/web/vite.config.ts@v2.15.0`:
  Excalidraw font self-hosting plugin (fonts copied into `dist/excalidraw-assets/`,
  `EXCALIDRAW_ASSET_PATH` set in `src/main.tsx` — upstream #324), `vendor-typst`
  chunk + `optimizeDeps` exclude, React `dedupe`.
- `tailwind.config.js` re-mirrored (danger/success/warning colors, `--z-radius-scale`
  borderRadius, `text-2xs`, z-index/dialog-width scales).
- `src/bridge/tauri-bridge.ts` implements the full v2.15 `ZenBridge` contract:
  the 34 new members are **stubs** that reproduce the web bridge's degradation
  values (see the "v2.15 contract surface (M17, stubs)" section in that file).
  `convertObsidianExcalidraw` (optional in the contract) is intentionally omitted.
- Verified: `tsc` clean, `vite build` clean, shared-domain tests 55/55,
  app-core tests 868/868.

Test-runner quirks on this machine (not code bugs):

```sh
# Node 26 ships an experimental localStorage global that shadows jsdom's,
# and resolves the default ICU locale as "und".
NODE_OPTIONS='--no-experimental-webstorage' LC_ALL=en_US.UTF-8 \
  ./node_modules/.bin/vitest run --root packages/app-core
```

## Phase 2, step 0 — contract-compat fixes in existing Rust code — ✅ DONE (2026-07-22)

| Fix | Status |
|---|---|
| `VaultSettings` / `PeriodicNotesSettings` widened fields | ✅ Solved structurally: both structs now carry a `#[serde(flatten)] extra` passthrough map, so monthlyNotes/folderColors/favorites/view/titlePattern/… — and any future upstream additions — survive get→set round-trips without further Rust changes. Covered by `v215_keys_survive_a_set_get_roundtrip` test. |
| `NoteMeta.assetEmbeds` | ✅ `extract_asset_embeds` in `metadata.rs` (mirrors upstream `vault.ts:1693`), populated in `notes.rs`. Test added. |
| `VaultChangeEvent.scope: 'folder'` | ✅ Watcher classifies directory add/unlink (notify `CreateKind::Folder`/`RemoveKind::Folder` or live `is_dir`) with scope `folder`. Known gap: rename-*from* of a directory is undetectable post-hoc; child file events still refresh the UI. `'database'` scope lands with the Databases cluster. |
| `openVaultWindow(root?)` | ✅ Plumbed through `window_open_vault`. Caveat: the backend is single-active-vault, so an explicit root switches the vault globally before opening the window; per-window vault sessions remain future work. |
| `DeletedAsset.deletedAt?`, `VaultInfo.temporary?` | ✅ Added. Bonus: `delete_asset` now writes the upstream `.zn-deleted.json` sidecar, pre-wiring the phase-A deleted-assets listing. |
| `.excalidraw` files are note-like | ⏸ Deferred to the Excalidraw cluster (phase A) — no drawings can exist before `createExcalidraw` lands. Spec: upstream `vault.ts:2524,2149,3249`. |

Also landed alongside step 0 (from the smoke test): Linux ships frameless
(`tauri.linux.conf.json` + `.decorations(false)` on secondary windows) with a
drag shim (`src/bridge/drag-region.ts`) replacing Electron's
`-webkit-app-region` handling; `BUILD.md` gained the
`WEBKIT_DISABLE_DMABUF_RENDERER=1` Wayland troubleshooting note.

## Phase 2 — new backend surface, by cluster

Difficulty labels are for the Rust implementation. "Spec" = upstream file.

### A. Quick wins (each ≤ ~1 day, high user value)

| Method | Difficulty | Spec | Notes |
|---|---|---|---|
| `readWorkspaceState` / `writeWorkspaceState` | trivial | `index.ts:2181-2203` | `<vault>/.zennotes/workspace.json`, raw JSON string, never parsed by the backend. Return `null` on ENOENT. Skip for ephemeral roots. Newest-wins merge vs localStorage is app-core's job. |
| `rootContentHiddenByInboxMode` | trivial | `vault.ts:1241` | `true` iff saved `primaryNotesLocation === 'inbox'` but a root scan (non-hidden, non-reserved dir or `.md`) would infer `'root'`. Reserved names: 4 system folders + `assets`/`attachements`/`_assets` + `.zennotes`. |
| `revealFilePath` | trivial | `index.ts:2599` | Absolute path → reveal in file manager (port already has reveal helpers in `os.rs`). |
| `toggleDevTools` | trivial | `index.ts:3003` | `open_devtools()`/`close_devtools()`; no-op in release builds is fine. |
| `createExcalidraw` | trivial | `vault.ts:3152` | Clone of `createNote` with `.excalidraw` extension and the fixed empty-scene JSON seed (`shared-domain/excalidraw.ts:32`). Called unconditionally by "New Drawing" — highest-priority stub to replace. |
| Deleted assets: `listDeletedAssets` / `purgeDeletedAsset` / `emptyDeletedAssets` | trivial | `vault.ts:3477-3517` | Store: `<vault>/.zennotes/deleted-assets/<uuid-token>/` + `.zn-deleted.json` sidecar `{path,name,deletedAt}`. Token regex `^[0-9a-f-]{36}$`. Separate mechanism from the notes `trash/` folder. Also add `deletedAt` when `deleteAsset` writes the sidecar. |
| `openExternalFile` | easy | `index.ts:2606` | Resolve `file://` / `~` / absolute → open with OS default app; return `{ok,error}` — never throw. The confirm dialog + link filtering live in app-core (free). Keep `'desktop-only'` sentinel semantics if stubbed. |

### B. Custom themes + overrides (moderate, one shared watcher pattern)

Spec: `apps/desktop/src/main/custom-themes.ts` (440 lines), `overrides.ts` (193
lines); pure helpers in `shared-domain/custom-themes.ts` are frontend-reusable.

- Layout: global config dir (see D) → `themes/<slug>/{manifest.json,theme.css,assets…}`
  and `overrides/*.css`. CSS is returned **inline as strings** in `CustomTheme[]` /
  `Override[]` — no asset protocol needed for the CSS itself.
- `list*`: dir scan + lenient `JSON.parse` of manifest; error entries get
  `{css:'', error}` instead of being dropped. Sort by name.
- `create/delete/reveal`: slug sanitization (`isSafeSlug`: no `/ \ ..`),
  resolved-parent check before any rm, first-run seeding (Soft Paper example +
  README) is optional polish. For `createCustomTheme`'s scaffolded CSS, call the
  shared-domain `scaffoldThemeCss` from the **frontend** and pass the string to a
  dumb Rust "write theme files" command — don't port the palette math.
- `on*Change`: `notify` watcher (themes: depth 1; overrides: flat), 200 ms
  debounce, re-scan, emit full fresh list as a Tauri event.
- Deferred: `zen-theme://`-equivalent asset protocol for fonts/images referenced
  *inside* theme CSS (sandboxed to the theme folder, mirror `resolveThemeAssetPath`).
  Themes without embedded assets work without it.

### C. CSV databases (the biggest cluster — go thin)

Spec: `apps/desktop/src/main/databases.ts` (399 lines). **Architecture
decision: thin backend.** All heavy logic (`parseCsv`/`serializeRows`/
`inferFields`/`buildDefaultViews`/`normalizeSidecar` + all view transforms) is
pure dependency-free TS in `shared-domain` that the frontend already bundles.
Prefer Rust commands that do raw file IO (read/write `data.csv` +
`schema.json` text, mkdir/rename/walk, atomic writes) with the (de)serialization
done in `src/bridge/` TS using the vendored shared-domain functions. That keeps
byte-level format parity (RFC-4180, LF, trailing newline, BOM handling) for free
and avoids reimplementing schema-inference heuristics.

- On-disk: `<Name>.base/` folder = `data.csv` + `schema.json` + record pages.
  Identity/cache key = the `data.csv` vault-relative path. Legacy loose
  `<Name>.csv` + `<Name>.csv.base.json` still read + migrated on open.
- Watcher: normalize any db-file event to the `data.csv` path and emit
  `scope:'database'` (`shared-domain/databases.ts` `databaseCsvPathFor` is the
  reference). Frontend already does 1500 ms echo suppression.
- Priorities: `openDatabase` + `createDatabase` + `writeDatabaseRows` +
  `writeDatabaseSchema` + `createRecordPage` are the working set.
  `renameDatabase` is trivial (folder rename). `listDatabases` is **dead code**
  upstream (nothing calls it) — keep the `[]` stub.

### D. Portable config (`config.toml`) — the subtle one

Spec: `apps/desktop/src/main/app-config.ts` (746 lines); schema in
`shared-domain/app-config.ts`.

- **Config dir must be `~/.config/zennotes` on macOS too** (deliberate upstream
  choice, #203 — dotfile-syncable), honoring `$ZENNOTES_CONFIG_DIR` and
  `$XDG_CONFIG_HOME`. Do not use Tauri's default app-config dir.
- **`getConfigSync` is genuinely synchronous at first paint** (Zustand reads it
  during module evaluation). Electron uses `sendSync`. Tauri options:
  gate the React mount on one `await invoke` in `src/main.tsx` stashing
  `window.__ZEN_CONFIG__`, or inject the snapshot via the window's
  `initialization_script` from Rust. Contract: `null` = no config support
  (current stub → pure localStorage, v2.1.0 behavior); `{}` = file absent →
  app-core seeds it from localStorage (this IS the migration — free);
  populated = file wins for portable keys.
- `setConfig`: merge partial → serialize → atomic write behind a serial queue;
  skip if text unchanged; remember own-writes so the watcher ignores them.
  The TOML emitter is **hand-rolled and comment-annotated** upstream (ordered
  sections, `[keymaps]` reference block, nullables persisted as `""`).
  Reproduce with string building, not `toml::to_string`.
- Watcher: single file, 150 ms debounce, own-write loop-guard (last-16 texts),
  `unlink` ignored, parse errors keep last-good config.
- `getConfigPath`/`revealConfigFile`: trivial (`ensureConfigFile` then reveal).

### E. Remaining misc

| Method | Difficulty | Notes |
|---|---|---|
| `fetchLinkMetadata` | moderate | `main/link-metadata.ts` is the spec: HTTPS-only, hostname-based private-range blocklist checked **pre- and post-redirect**, 6 s timeout, 512 KB streamed cap, regex OpenGraph/Twitter/title/favicon extraction, `{ok:false}` never throws. Rust: `reqwest` + regex; optionally harden with DNS-resolution SSRF checks. No persistence (frontend has an in-memory cache). |
| `openFolderTemporary` | hard | Ephemeral vault sessions: new window on a folder with **no** `.zennotes` writes (registry of ephemeral roots gates settings/workspace/meta writes), `folderHasMarkdown` precheck (bounded BFS, 4000 entries), `VaultInfo.temporary` banner. Do last, with the multi-window work. |
| `convertObsidianExcalidraw` | moderate | Optional — currently omitted; app-core shows a graceful "desktop only" message. Needs LZString base64 decompression (`lz-str` crate) + fenced-block extraction (`shared-domain/excalidraw.ts:76` is pure TS — thin-backend option applies here too: read file in Rust, parse in frontend, write via a dumb command). |

## Suggested order

1. **Step 0 compat fixes** (VaultSettings et al.) — restores full settings persistence.
2. **Cluster A quick wins** — workspace sync, drawings creation, deleted-assets UI, reveal/open/devtools.
3. **D config** — biggest UX payoff (synced prefs) + unlocks the `getConfigSync` bootstrap pattern.
4. **B themes/overrides** — reuses D's dir + watcher pattern.
5. **C databases** — thin-backend refactor, feature-complete tables/boards.
6. **E stragglers** — link metadata, Obsidian import, ephemeral vaults.

## Out of scope (upstream features the port intentionally drops)

Web/server/CLI (`zn`), MCP install flows, auto-updater, Raycast, TikZ. Upstream
UI touching these already degrades via the existing capability flags and
"unavailable" stubs in `tauri-bridge.ts`.
