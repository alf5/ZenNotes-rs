# ZenNotes-rs 2.85.0: upstream v2.79.0 → v2.85.0

`packages/{app-core,bridge-contract,shared-domain}` are byte-identical to upstream v2.85.0
(214 upstream commits). Two upstream docs are vendored too, because a vendored test reads the
fixtures: `docs/web-clipper-link.md` and `docs/web-clipper-link.fixtures.json`.

**New in the bridge and the backend**

| Upstream | Here | Notes |
|---|---|---|
| config.toml keeps what only zn writes (429defec) | `src/bridge/portable-config.ts`: field tables and serializer spliced wholesale again; the write path reads the file just before writing, as upstream's `currentlyCarried` | `src/__tests__/portable-config-carried.test.ts` |
| Template folder (#1024) | `vault/settings.rs` normalizes `templates.folder` and moves it with a folder rename; `search.rs` and `src/bridge/tasks.ts` leave its notes out; `supportsTemplateFolder: true` | Tag surfaces are app-core's |
| Seeded inbox settle (v2.84) | `vault/seeded_inbox.rs`, run on open, after a burst of root-level changes (400 ms) and on a switch to root mode | The port's own Welcome texts are in the hash list beside upstream's |
| Watcher: handles run out, folders that cannot report (#1023, 9c27db1b) | `watcher.rs`: `VaultWatch` retries after exhaustion (30 s doubling to 10 min) with a resync; a folder notify cannot watch is re-read every 30 s and on window focus (`vault_fingerprint`) | On Linux this also covers one unreadable subfolder, which makes notify's recursive watch fail outright; chokidar only skips that folder. Theme and language-pack watchers fall back to one level deep |
| Open a vault in the file manager | `vault_open_in_file_manager`; the root must be one the app already knows | |
| Task reminders (#1036) | `src-tauri/src/task_reminders.rs` ports `task-reminders.ts`: the ledger in the app data dir, notifications through `notify-rust` | A thread looks at the wall clock at least every 30 s instead of power events. A click opens the task on Linux (freedesktop actions); macOS and Windows are wired the same way, untested |
| Clip links `zennotes://new`, `daily` (#620) | `zennotes-rs://new` and `zennotes-rs://daily`: `src-tauri/src/clip_links.rs` hands the link to the main window and checks the clipboard hash; `src/bridge/clip-links.ts` ports `clip-links.ts` | A clip naming another vault is refused with a sentence saying which (upstream writes it into a vault no window shows; this build keeps one vault open). The browser extension opens `zennotes://`, the original app's scheme, so its help card is hidden |
| Escaped wikilink pipe in tables (#1052) | `vault/metadata.rs`, `standalone_links.rs`; renames and asset moves through shared-domain | |
| Asset moves leave a link a note answers alone (v2.83) | `src/bridge/link-rewrites.ts` passes the notes | |
| Note creation dates survive a locked file (Windows) | `sidecars.rs` renames with retry | |
| (port bug, found by the live test) | `layout::read_primary_notes_location` now infers root mode when vault.json states none, as `getVaultSettings` does | Before, a vault showing its notes at the root (a flat Obsidian vault, a seeded one with folders copied in) got an `inbox/` folder on open and every new note went into it |

**MCP server (`zennotes-rs mcp`):** all 45 of upstream's tools (new: `read_asset`, `upload_asset`,
`rename_asset`, `move_asset`, `duplicate_asset`, `delete_asset`, `restore_asset`, `update_task`,
`daily_note`, `note_sections`, `edit_section`), and the scope flags `--allow-path`, `--deny-path`,
`--read-only`, `--access`, enforced on every tool (`src-tauri/src/mcp/scope.rs`, `scoped.rs`, every
tool behind `backend.rs`). `delete_asset` uses the app's own deleted-assets store, so the app's
Trash shows it. The golden harness now runs sessions with their own flags, setup files and
symlinks: 328 steps in 17 sessions, identical to upstream's server. `scope_contract.rs` runs
upstream's `fixtures/mcp-scope.json` through the real handlers. Not ported: remote mode
(`--server`, `--token`) and its scope branches (a later release); remapped system folders (4
contract cases skipped, as in the rest of the port). Month and weekday names in periodic notes
are en-US; upstream follows the locale. A lone UTF-16 surrogate in a tool argument is a parse error
here (upstream substitutes U+FFFD).

**Port-owned copies added this round (re-mirror on every re-vendor):** `src/bridge/clip-links.ts`
← `apps/desktop/src/main/clip-links.ts`; `src-tauri/src/task_reminders.rs` ←
`task-reminders.ts`; `src-tauri/src/vault/seeded_inbox.rs` ← `settleSeededInbox` and friends in
`vault.ts` (its hash list holds every Welcome the port and upstream have seeded); `vault_fingerprint`
in `watcher.rs` ← `vault-fingerprint.ts`.

**Not carried over:** `downloadVaultArchive` (the browser build's), Cloud's refused paths (no
Cloud here), the macOS dev-build deep-link fix (Electron's), the Excalidraw font fallback rewrite
(this webview runs without a CSP, so nothing is logged).

## Imports

Obsidian vault, Logseq graph and Notion workspace (integration token, or Sign in with Notion)
into the local vault (upstream v2.80–v2.85); `supportsImport: true`. The run, and the vendored
conversion it drives, stays in the webview (`src/bridge/imports/`); Rust (`src-tauri/src/imports/`)
keeps only what a webview can't do or shouldn't hold:

- the folder dialog and the folder it picked; the webview names files only by virtual path
  (`/<kind>/{picked,source,staging,vault}/…`) and Rust resolves each one (`paths.rs`, `files.rs`).
  The picked folder is read only and never read through a link that leaves it. In the vault an
  import only creates (`wx`), makes folders, moves its staged folder in under a free name, appends
  to its own report inside that folder and writes created-date sidecars: it never overwrites,
  deletes or renames anything that was there, and refuses to write through a link;
- requests to Notion, only to `https://api.notion.com/v1/…` (GET/POST) and
  `POST https://mcp.notion.com/mcp`, with the credential attached by Rust and no redirects
  (`notion_http.rs`); page file downloads go straight into staging, https only, never to a private
  address (the host name, every redirect hop and the address it resolves to are each checked);
- Sign in with Notion (`notion_auth.rs`): PKCE, dynamic client registration, 127.0.0.1 loopback
  redirect, refresh shared between concurrent calls. The token and session live only in the OS
  keychain (`notion:token`, `notion:session`); without one they are kept in memory for the run.
  Neither ever reaches the webview: its clients hold a stand-in and Rust swaps in the real one.

Status, progress and Stop are shared through Rust (`import_status`, `import_report`,
`import://<kind>-progress`, `import://stop`), so every window sees one run, a Stop from any
window reaches it, and a finished result survives a reload until dismissed. Remote mode refuses
start with upstream's "Imports write to a local vault."

**Port-owned copies (re-diff each against upstream on every re-vendor):**

| Here | Upstream (`apps/desktop/src/main/`) |
|---|---|
| `src/bridge/imports/obsidian-import-run.ts` | `obsidian-import-run.ts`: verbatim except the `fs`/`path` imports |
| `src/bridge/imports/logseq-import-run.ts` | `logseq-import-run.ts`: verbatim except the `fs`/`path` imports |
| `src/bridge/imports/notion-import-run.ts` | `notion-import-run.ts`: downloads go through the host (`FileFetch` writes into staging), `hashKey` is WebCrypto (async), `moveIntoVault` is one host call |
| `src/bridge/imports/{obsidian,logseq,notion}-import.ts` | `{obsidian,logseq,notion}-import.ts` (the controllers) |
| `src/bridge/imports/notion-host.ts` | the `net.fetch`/token wiring in `notion-import.ts` |
| `src/bridge/imports/index.ts` | the import members of `preload/index.ts` |
| `src-tauri/src/imports/commands.rs` | the `OBSIDIAN_IMPORT_*`, `LOGSEQ_IMPORT_*`, `NOTION_IMPORT_*` handlers in `index.ts` |
| `src-tauri/src/imports/notion_auth.rs` | `notion-sign-in.ts` and the Notion half of `secret-store.ts` |
| `src-tauri/src/imports/files.rs` (`move_into_vault`, `write_note_created_at`) | `moveIntoVault` (`notion-import-run.ts`), `writeNoteCreatedAt` (`note-creation-metadata.ts`) |
| `src/__tests__/{obsidian,logseq,notion}-import-run.test.ts`, tests in `notion_auth.rs` | the same-named `*.test.ts`, `notion-sign-in.test.ts` |

**Deviations:** a run lives in the page that started it, so reloading or closing that window
ends it as a Stop would (a Notion import stays resumable); upstream's survives in its main process.
Without a keychain a sign-in or token is kept for the run instead of refused (the port's standing
rule: never a file). Sign-in also refuses OAuth endpoints off mcp.notion.com's origin, and the
registration names the client "ZenNotes-rs". A folder in the vault that is a symlink, on the way
to the import folder, is refused rather than followed. The folder dialog has a title but no
button label or message (Tauri's dialog has neither). PKCE randomness comes from the OS through
`uuid` v4 rather than a new crate.

Upstream's `notifyIfAway` (a Notion import that ends while no window has focus says so) goes
through `notify-rust`, as task reminders do.

**Not ported:** nothing in the import feature; the stand-in overrides for a test run
(`ZENNOTES_NOTION_API_URL`, `ZENNOTES_NOTION_MCP_URL`) are honored as upstream honors them (a dev
build, or `ZEN_PERF=1`).

---

# ZenNotes-rs 2.79.3: remote workspaces

The app opens a vault on a ZenNotes server: upstream's `znserver`, or any
server speaking its `self-hosted-http-v1` protocol (zenserver-multi-rs is one).
Plan, decisions and deviations: [docs/remote-workspace-plan.md](docs/remote-workspace-plan.md).
Still mirrors upstream v2.79.0; `packages/` is untouched.

**Shape:** the route table is TypeScript, the transport is Rust.

| Piece | Where | Upstream |
|---|---|---|
| Server client, route for route | `src/bridge/remote/client.ts` | `apps/desktop/src/main/remote/server-client.ts` |
| Remote branch of each vault member, refusals and capability checks included | `src/bridge/remote/vault.ts`, routed by `routed()` in `tauri-bridge.ts` | the `if (isRemoteWorkspaceActive())` branches of the `VAULT_*` handlers in `index.ts` |
| Connect, boot, disconnect, vault selection, workspace info | `src-tauri/src/remote/session.rs` | `setRemoteWorkspace`, `loadCurrentVaultFromConfig`, `disconnectRemoteWorkspace`, `VAULT_SELECT_PATH` |
| Saved profiles | `src-tauri/src/remote/profiles.rs` | the profile half of `index.ts` |
| Transport: token, TLS, route allowlist, uploads, raw assets | `src-tauri/src/remote/http.rs`, `tls.rs` | `fetch` + `system-trust.ts` |
| Change feed | `src-tauri/src/remote/watch.rs` | `watchVaultChanges` |
| Remote assets | `syn-asset://…/remote:<path>` in `asset_protocol.rs` and `asset-url.ts` | `zen-asset://remote?path=` |

**Port-owned copies that rot on a re-vendor:** `client.ts` (diff `server-client.ts`;
`src/__tests__/remote-client.test.ts` holds the route table, update it first),
`vault.ts` (diff the `VAULT_*` handlers' remote branches), `watch.rs` (diff
`watchVaultChanges`), `profiles.rs` (diff `saveRemoteWorkspaceProfile` and
`deriveRemoteWorkspaceProfileName`).

**Safety:** the token never enters the webview (`getRemoteWorkspaceInfo` says only
whether there is one), and Rust sends a request only to the connected server and only
under `/api/`. While a session is on, no local vault is open in Rust, so every
local-disk command refuses; a test fails if a new place opens a local vault without
ending the session first, and another if a vault bridge member skips routing.

**Different from upstream, on purpose:**

- An address typed without a scheme gets `https://` (upstream: `http://`), except a
  loopback one. Swapped into `store.ts` and `RemoteWorkspaceProfileModal.tsx` at build
  time and matched in Rust.
- Tokens live in the OS keychain only; without one, a token lasts until the app quits
  (upstream: nothing saved). The pre-2.79.3 base64 file is read and emptied, never
  written.
- The feed keeps a base path such as `/notes` (upstream drops it), and requests time
  out (15 s to connect, 300 s to read).
- Remote mode is app-wide: the window that switches carries on, and every other
  window reloads onto the new workspace (upstream keeps a workspace per window).
- macOS: `src-tauri/Info.plist` declares `NSLocalNetworkUsageDescription`, as
  upstream's `extendInfo` does; without it macOS 15 never asks and a LAN server looks
  down. Unverified: macOS builds come from CI only.

**Not carried over:** the MCP server's remote mode (`--server`, `--token`,
`credentials.toml`, Settings → MCP → Server token) is phase 5 of the plan.
Upstream's two Electron-only quirks are not copied (a second `RemoteRequestError`
class; disconnect without a local vault leaving the feed running).

---

# ZenNotes-rs 2.79.1 and 2.79.2: what moving off Electron had left wrong

2.79.2 is 2.79.1 plus a Windows build fix: a Windows checkout has CRLF endings,
which multi-line wording swaps did not match. 2.79.1 has no Windows installers.

Still mirrors upstream v2.79.0; `packages/` is untouched. A review of every
place the port leans on something Electron gave for free.

| Electron gave | Here now | Checked |
|---|---|---|
| IPC rejects with an `Error` | `src/bridge/ipc.ts` turns Rust's bare strings into `Error`s; app-core read `.message` off them in 34 places | Live |
| `webUtils.getPathForFile` | `src/bridge/file-drops.ts`. Linux: Tauri's native drop, delivered as a synthetic DOM `drop` of stand-in `File`s. macOS/Windows: the native handler swallows in-app drags too (read in wry and Tauri's docs), so it is off and the page's drop is used: uri-list paths, else attachments staged to a temp file; a Markdown file or folder with no path is refused, not opened from a temp copy | Live on Linux from Tauri's drop event onward. **Not exercised: a real OS drag** (no way to drive one on this machine), nor anything on macOS or Windows |
| `GlobalShortcutsPortal` on Wayland | `global-hotkey` is X11-only, so `zennotes-rs --quick-capture` and `zennotes-rs://quick-capture` toggle the panel from a compositor keybind | Live for the triggers; the X11 hotkey itself not re-tested |
| `will-navigate` | Two halves, because WRY reports iframes to the guard without saying which frame asked: Rust (`external_urls::guard_navigation`) refuses `file:` and hands application schemes to the gate; `src/bridge/navigation-guard.ts` keeps the main frame in place | Live; PDF iframe still renders |
| `clipboard.writeText` | `tauri-plugin-clipboard-manager`, falling back to `navigator.clipboard` | Live on Wayland |
| `protocol.handle('zen-theme')` | `custom_css::handle_theme_asset`, sandboxed to the theme folder. WebView2 needs `http://zen-theme.localhost`, so `custom-css.ts` rewrites the CSS there | Live on Linux; the rewrite is unit level |
| Devtools in production | Cargo feature `devtools` | Live |
| Per-window vaults | Still one vault per run. `openVaultWindow(root)` now asks first | Dialog seen pending; choosing Switch not driven |
| `printToPDF`, Word export | Still absent. The commands now say so in a toast instead of doing nothing | Live |
| Bundled codecs | System GStreamer: `optdepends` and BUILD.md | Packaging only |

MCP: note links are `zennotes-rs://` (upstream's scheme opened upstream's app or
nothing), and the instructions no longer ask for TikZ. Both come from
`tooling/mcp-golden/port-overrides.ts`, applied on every re-record, which fails
if upstream rewords a passage. A second launch's deep links now run once: the
deep-link plugin and the single-instance callback both delivered them.

Wording swaps and hidden Help entries are in `tooling/vite/vendored-copy.ts`;
both fail the build when upstream rewords them, so they need attention on a
re-vendor. Help's CLI section is emptied whole.

---

# Gap analysis: upstream ZenNotes v2.73.0 → v2.79.0

Status of the mirror as of 2026-10-31, released as ZenNotes-rs 2.79.0.

Upstream reference clone: `~/delme/zennotes`, mirrored at tag `v2.79.0` (its
three later commits are packaging only). The gap crossed here was 173 commits,
almost all of it in the vendored packages: Timeline, tagged blocks, covers,
folder Gallery, database view columns and picked rows, a key for any command,
callout and code language menus, the Vim-off keyboard fixes. The bridge contract
gained six optional members (Unsplash) and one optional vault setting
(`sharesLocation`).

## Done

- `packages/{app-core,bridge-contract,shared-domain}` re-vendored **verbatim**
  (confirmed byte-identical to `v2.73.0` before replacing, and to `v2.79.0`
  after). `pele` moves to 1.12.0 through the workspace.
- `@types/node` follows upstream to 25: vendored `shared-domain/unsplash.ts` sets
  `RequestInit.cache`, which the package's own `tsc` rejects under 22.
- `src/bridge/portable-config.ts`: field tables and serializer re-spliced
  wholesale, byte-identical to upstream again. `[keymaps.commands]` (#971) has no
  table entry, so the parity test now names it, and a round trip is pinned.
- **Unsplash covers** (`src/bridge/unsplash.ts`, `src-tauri/src/unsplash.rs`):
  upstream builds the vendored `createUnsplashClient` in its main process. Here
  it runs in the renderer, so its rules cannot rot, over two things Rust keeps:
  the Access Key, only in the OS keychain with no file fallback, and the request,
  limited to api.unsplash.com's three endpoints with redirects off. The client
  holds a stand-in, never the key. A pasted key crosses once to be checked and
  saved. One difference: the 40-requests-a-minute cap counts per window.
- `vault/metadata.rs`: `stripCodeContent` is now upstream's line-based version,
  shared with `mcp/ops.rs`. The old regex missed fences in CRLF notes (their code
  leaked in as tags, upstream's fix in 2.77), and had also never handled indented
  or `~~~` fences (#293).
- Quick Capture opens on the display under the pointer (#976): `windows.rs` ports
  `quick-capture-placement.ts`. Unit tested only, not exercised live: this
  machine runs niri on Wayland, and the move is best effort where a compositor
  withholds the pointer position or window placement.
- `sharesLocation` round-trips through the existing `VaultSettings` passthrough.
- The MCP golden was re-recorded from upstream 2.79 with six new steps for CRLF
  notes (fenced tags and links, task toggles): 86 steps match.

## Not carried over

| Gap | Why |
|---|---|
| Share > ZenNotes (`share-inbox.ts`, `apps/share-extension`) | **Deferred until it can be tested on a Mac.** A macOS share extension, and macOS builds here are CI-only and unsigned, so macOS may not load it. First step when a Mac is at hand: embed upstream's extension in a Tauri build and see whether it is listed, before porting any filing code. Settings hides its row on Linux and Windows. On macOS, by the code, **Default shares location** still shows and is saved, but nothing files shares. |
| Trusting system certificates (`system-trust.ts`, #1009) | **Skipped.** It serves Cloud, updates and link previews. Cloud and updates are not in this port. Link previews use `ureq` with its own roots and already refuse private addresses, so it would only help a public address with a private certificate. |
| Update error copy (`update-error-copy.ts`, #1003) | **Closed, nothing to port.** It only explains auto-updater failures, and this port has no auto-updater. |
| Cloud sync changes | Cloud sync remains out of scope. |

Vendored text that is wrong here is swapped at build time, leaving `packages/`
byte-identical (`tooling/vite/vendored-copy.ts`): the Unsplash key row's Electron
`--password-store` hint, and the What's New entry announcing Share, which now
says this build does not have it. If upstream rewords either, the build and
`src/__tests__/vendored-copy.test.ts` fail, and the entry needs updating.

The open items in the v2.73 section below are unchanged.

---

# Gap analysis: upstream ZenNotes v2.45.0 → v2.73.0

Status of the mirror as of 2026-10-21, on branch `mirror-v2.73`.

Upstream reference clone: `~/delme/zennotes` (tags up to `v2.73.0`). The gap
crossed here was 451 upstream commits: ~72k changed lines across the three
vendored packages, ~18k in `apps/desktop/src`, and a bridge contract with one
new required member (`openExternalUrl`) and ~35 new optional ones.

## MCP server (ZenNotes-rs 2.73.1, after the v2.73 mirror)

`zennotes-rs mcp` is the port of upstream's MCP server
(`apps/desktop/src/mcp`), built on the `rmcp` crate and dispatched from
`main.rs` before Tauri starts, so it runs headless over stdio. It serves the
same 34 tools on the vault folder directly (the app need not be running):
`--vault <path or name>`, else `$ZENNOTES_VAULT`, else the vault the app last
had open, re-resolved before every call with upstream's "the vault changed,
call vault_info" guard. Edits through `write_note`, `append_to_note`,
`prepend_to_note`, `insert_at_line`, `replace_in_note` and `toggle_task` leave
agent edit records the app's review reads (`[mcp] review_agent_edits`).
Settings → MCP now installs it into Claude Code, Claude Desktop, Codex and
OpenCode, under the key `zennotes-rs` (not upstream's `zennotes`, so both apps
can be installed side by side without overwriting each other's entry), and
edits the instructions, kept in the app config dir as
`zennotes.mcp-instructions.md`.

**Parity is tested against upstream itself**: `tooling/mcp-golden/generate.ts
<upstream clone>` runs upstream's server from its TypeScript under bun,
replays `scenario.json` (80 calls over every tool and its error paths, on the
fixture vault beside it), and records `golden.json` plus the tool list
(`src-tauri/src/mcp/tools.json`, served verbatim) and the default instructions
(`src-tauri/src/mcp/instructions.md`). `bun run test:mcp` replays the scenario
against `target/debug/zennotes-rs` and fails on any difference in a reply or in
the files left on disk. **Re-run both on every re-vendor.**

Port-owned copies (Rust) of upstream logic, all covered by the golden test:

| File | Upstream |
|---|---|
| `src-tauri/src/mcp/server.rs` | `apps/desktop/src/mcp/server.ts` (handlers, argument checks, error wording, `VaultSession`) |
| `src-tauri/src/mcp/ops.rs` | `apps/desktop/src/mcp/vault-ops.ts` (its own vault logic, which differs from the main process: `.md`-only listings, frontmatter tags, ` 2` name counters, no inbound link rewrite on rename) |
| `src-tauri/src/mcp/tasks.rs` | the task parser in `vault-ops.ts` + `@shared/tasks` / `tasks-excluded-folders` helpers |
| `src-tauri/src/mcp/comments.rs` | `apps/desktop/src/mcp/comment-ops.ts` + `@shared/note-comments` threading |
| `src-tauri/src/mcp/agent_edits.rs` | `apps/desktop/src/mcp/agent-edit-recorder.ts` + the writing half of `@shared/agent-edits` |
| `src-tauri/src/mcp/integrations.rs` | `apps/desktop/src/main/mcp-integrations.ts` |

Not carried over: remote vaults (`--server`, `--token`, the Settings server
token, `ZENNOTES_SERVER`; the app connects to servers since 2.79.3, the MCP
server not yet: docs/remote-workspace-plan.md, phase 5), remapped
system folders (as everywhere in this port), and the legacy `out/main/mcp.js`
entry. (Settings' description of the server was reworded in 2.79.1.)

## Frontend

- `packages/{app-core,bridge-contract,shared-domain}` re-vendored **verbatim**
  from `v2.73.0` (confirmed byte-identical to `v2.45.0` before replacing, and to
  `v2.73.0` after). New deps come in through the workspaces: `pele` (replaces
  mermaid for ```` ```mermaid ```` fences), `@terrastruct/d2`, `@codemirror/merge`
  (agent edit review), `@lezer/common`, `@scure/bip39`, `hash-wasm`.
- `vite.config.ts`: the `pele-runtime` and `d2-runtime` chunks are deferred
  preloads (as upstream's web config), and `@codemirror/merge` is kept out of
  `vendor-editor` (as upstream's DESKTOP config; the web one lacks the rule).
  The boot-order guards hold: the entry still statically imports only
  `bootstrap-config`.
- `tailwind.config.js` now uses the vendored `packages/app-core/build/tailwind-preset.cjs`
  (identical theme to the old inline copy), so the next re-vendor updates it.
  `packages/app-core/build/vite.mjs` (a plugin for external hosts) is vendored
  but unused, like upstream's own apps.
- `docs/specs/cloud-e2ee-vectors.json` vendored at its monorepo path: a
  shared-domain test reads it as `../../../docs/specs/…` (same reason
  `tooling/vite/harper-wasm-asset.ts` lives where it does).

## Port-owned copies of upstream logic (these rot silently on a re-vendor)

| File | Upstream | Done |
|---|---|---|
| `src/bridge/portable-config.ts` | `apps/desktop/src/main/app-config.ts` | Field tables and serializer re-spliced wholesale; both sections are byte-identical to upstream again, which matters because the file is SHARED with upstream ZenNotes (`~/.config/zennotes/config.toml`): any difference makes the two apps rewrite it back and forth. 11 prefs had no mapping (the parity test caught them); the keymap reference now renders the same bytes on every OS (#901); the watcher's own-write guard only applies while writes settle (9aebb6f3). The parity test's last exemption (`keepViewModeAcrossNotes`) is gone: upstream maps it since v2.52. |
| `src/bridge/workflows.ts` | `apps/desktop/src/main/workflow-apply.ts` | Path ops now carry each note's comments, creation date and agent edit record (journalled, undone, drift-checked) (#839); a leftover discussion at the destination refuses the run; a symlinked note moves as the same link and undo moves it back, never planting one (the re-spell is checked in Rust); journal entries share one fsync; the journal key folds case on macOS/Windows. Covered by `src/__tests__/workflow-applier.test.ts`. |
| `src/bridge/databases.ts` | `apps/desktop/src/main/databases.ts` | `createDatabase` seeds (#832, table → database); rename goes through the folder rename so the comment tree follows. |
| `src/bridge/drag-region.ts` | the stylesheet's drag selectors | Honours `.zn-app-dragging`; utility windows' headers (inline `WebkitAppRegion`, which WebKitGTK drops) are matched structurally. With the title bar off (#754) the tab strip carries `.drag-region`. |
| task parsing | `apps/desktop/src/main/tasks.ts` | **Moved to TS** (`src/bridge/tasks.ts`) over vendored `@shared/tasks`. The Rust parser was a v2.1-era port that knew no `[>]`/`[-]`/`[/]` states, `@field:value` fields, `#task` files, or excluded/included Task folders (#458, #962). |
| demo tour | `shared-domain/demo-tour-data.ts` | **Moved to TS** (`src/bridge/demo-tour.ts`, imported on demand). The embedded JSON was the v2.1 tour; regenerating now also removes a tour seeded under the older note names. |

## Backend (Rust)

- **Note sidecars** (`src-tauri/src/vault/sidecars.rs`, port of
  `note-sidecars.ts`, `note-creation-metadata.ts`, `agent-edit-files.ts`):
  comments, `.zennotes/note-metadata/<note>.metadata.json` creation dates and
  agent edit records move with a note on rename/move, and leave with it on
  delete, empty-trash and folder delete (detached into a temp dir in one
  transaction, then erased). Folder renames move all three trees and rewrite
  folder icons, folder COLORS (missing before) and the Task-folder lists in the
  same transaction, rolling everything back on failure. Renaming onto another
  note's name is refused by file identity.
- **Atomic note saves** (`vault/atomic.rs`): `vault_write_note` was a plain
  truncating write, the data-loss window upstream closed in #585. Now temp file
  + rename through symlinks, with the creation date recorded first so a save
  no longer resets it.
- **Assets** aligned with upstream's layout, which the port had missed since
  v2.4 (#185, #377): drops and pastes land in `assets/`; `assets` is reserved;
  opening a vault migrates loose root attachments and `attachements/`/`_assets/`
  into it (idempotent; Obsidian vaults keep loose root files). Imported images,
  PDFs, audio and video become `![[vault/path]]` embeds; a pasted `image.png`
  gets the timestamped name (#775). Deliberate deviation: the migration leaves
  a root-level `.excalidraw` alone, since in a root-mode vault it is a note.
- Comments keep `author` and `parentId` (#738); the watcher emits the
  `agent-edits` and `templates` scopes; vault display names (#692) name the
  vault in `VaultInfo` and the remembered list, synced on every read.
- New commands: undo history (`undo_history.rs`, #793; app data dir, hashed
  names, pruned at launch), agent edit records, `app_open_external_url` (the
  application-scheme gate lives in Rust because the webview has no opener
  permission of its own), `app_install_info` (#814), the tray (#947, below),
  and the file primitives the applier needs.
- **Tray** (`tray.rs`): "Keep running in the tray" hides the last visible
  workspace window instead of closing it; the icon's menu opens windows, Quick
  Capture, or quits. The setting is machine-local. On Linux the appindicator
  library is probed first, because tray-icon panics without it (and the
  release profile aborts on panic); without it the setting stays off. Also
  ported: a hidden Quick Capture panel no longer keeps the app alive after the
  last window closes (last-window.ts).

## Bridge

`openExternalUrl`, undo history, agent edit review, tray, `getAppInfo`
(`hostKind`, `arch`, `os`, `engine`, `install`), `createDatabase(seed)`.
Inbound `[[wikilinks]]` now follow a note rename (upstream #111, v2.3, never
ported until now) and references follow an asset rename/move (#785), both in
`src/bridge/link-rewrites.ts` over the vendored rewriters. Capabilities:
`supportsUndoFile`, `supportsAgentEditReview` on.

Found by the smoke test and fixed: `window.open` of an outside URL did nothing
(WebKitGTK under Tauri opens no window and calls no browser), and app-core
opens every external link that way, so links in notes, Help and the release
notes never opened in this port. `createTauriBridge` now routes such calls
through `app_open_external_url`, the role Electron's `setWindowOpenHandler`
plays upstream.

Known and upstream's: the What's New window offers "Set up ZenNotes Cloud"
whenever Cloud is not connected, without asking `supportsCloudSync`, so it
shows here too. Its button opens Settings on the first page, because this
build has no Cloud page to open. Vendored code, so it is left as upstream has it.

## Not carried over

| Gap | Why |
|---|---|
| `sendUsageCount` | Upstream's anonymous install count goes to zennotes.org; sent from here it would count this port as upstream. Absent, so the Settings switch has no effect. |
| MCP server token, managed `zn` CLI updates | The MCP server serves local vaults only (remote vaults are phase 5 of docs/remote-workspace-plan.md), and no `zn` CLI ships in this build. |
| Cloud end-to-end encryption, backup paging | Cloud sync remains out of scope (see below). |
| `--new-window` (#815) | Single-vault backend; a second workspace window is "Open Vault in New Window". |
| macOS title-bar toggle | Upstream hides the traffic lights; this build is verified on Linux only, where windows are frameless and the app draws its own controls. |
| PDF export | Still the M16 stub (`exportNotePdf` resolves null); `src/export-window.tsx` is dormant and was not re-mirrored. |
| Remapped system folders (#115, v2.20) | Pre-existing: the Rust layout ignores `systemFolderPaths`, so a custom on-disk path for Inbox/Archive/Trash is not honoured. Large (listing, classification and every move). |

The earlier open items below (cloud sync, docx export, grammar vetting,
`onFrameEscape`, per-window workflow lock) are unchanged.

---

# Gap analysis: upstream ZenNotes v2.15.0 → v2.45.0 (historical)

Status of the mirror as of 2026-09-07. Phase 1 (re-vendor + frontend parity) is
**done**, and so is most of the new backend surface; the open items are listed
under "Not carried over" below.

Upstream reference clone: `~/delme/zennotes` (tags `v2.1.0` … `v2.45.0`).
All `apps/desktop/...` references below are upstream files — the spec for each
Rust implementation, exactly like the original port.

The gap crossed here was 400 upstream commits: ~84k changed lines across the
three vendored packages, a bridge contract 51 members wider, and five new
main-process subsystems.

## Phase 1 — completed

- `packages/{app-core,bridge-contract,shared-domain}` re-vendored **verbatim**
  from upstream `v2.45.0` (they were verbatim v2.15.0 before; keep it that way —
  never patch vendored code, port-specific code lives in `src/` + `src-tauri/`).
  Confirmed byte-identical before replacing, and `shared-ui` is still unused by
  app-core so it stays unvendored.
- New frontend deps: `@codemirror/lint`, `@xyflow/react` (the Workflows canvas),
  `harper.js` (offline grammar/spell check, ~15 MB wasm), `vscode-oniguruma` +
  `vscode-textmate` (user-installed code grammars). `remark-breaks` dropped.
  Vite 5 → 6, Vitest 2 → 3.
- `vite.config.ts` re-mirrored from upstream `apps/web/vite.config.ts@v2.45.0`
  by three-way merge, keeping every Tauri adaptation (relative `base`, dev-server
  block, `target: 'esnext'`, aliases into `./packages`). New from upstream: the
  oniguruma data-URL plugin, `harperWasmAsset()`, the `vendor-textmate` chunk,
  `@xyflow/react` excluded from `vendor-react`, and mermaid deliberately left
  unchunked so it stays off the boot path.
- `tooling/vite/harper-wasm-asset.ts` vendored verbatim (upstream keeps it
  outside the app, and `packages/app-core/vitest.config.ts` — which is vendored —
  imports it by the same `../../tooling/...` path, so the location is load-bearing).
- `tailwind.config.js` and the tsconfigs needed no changes (unchanged upstream).
- Verified: `tsc --noEmit` clean at the root and in all three packages;
  `vite build` clean; shared-domain 1556 passed; app-core 1889 passed, 1 skipped,
  1 failed (see below); `cargo test` 96 passed, 1 ignored; `tauri build` produces
  deb + rpm + AppImage at 2.45.0.

Harper and Typst both work without a privileged scheme here: Tauri serves the
renderer over `http://tauri.localhost`, so `bundledAssetUrl` passes the URL
through untouched. Electron needs `zen-harper://` only because its renderer is
`file://`. `supportsHarper` is therefore on.

### `portable-config.ts` rots silently across re-vendors (found by smoke test)

`src/bridge/portable-config.ts` is the PORT's own file — upstream's
`apps/desktop/src/main/app-config.ts` TOML layer, copied — so a re-vendor does
not touch it. Across v2.15 → v2.45 upstream added portable prefs that it never
learned: **21 of 79 had no TOML mapping**, `workflowsEnabled`, `harperEnabled`,
`harperDialect`, `atlasEnabled`, `autoPairs` and `textReplacements` among them.

The failure is quiet, which is what makes it dangerous: those settings work
normally in the app (localStorage) and simply never reach `config.toml`, so the
one promise that file makes — sync your settings across machines with git,
stow, chezmoi — was broken for them, and Workflows could not be switched on
from the file at all.

Fixed by mirroring upstream's `SCALAR_FIELDS`, `LIST_FIELDS` and the
map-table fields wholesale; the port's 52 pre-existing entries were already
byte-identical to upstream's, so this is a pure superset. `keepViewModeAcrossNotes`
stays unmapped, matching upstream, which does not map it either.

`src/__tests__/portable-config-parity.test.ts` now asserts the invariant against
shared-domain's `PORTABLE_PREF_KEYS` — which IS vendored, so it updates itself
on the next re-vendor and the test fails instead of the file rotting again.

### The renderer boot order is a chunking property (found by smoke test)

app-core's store calls `getConfigSync()` while building its initial state and
MEMOIZES the result, so `window.zen` must exist and `initPortableConfig()` must
have resolved before that module evaluates. `src/main.tsx` has always documented
this, and it was still broken in the first cut of this mirror: `config.toml` was
ignored on every launch, so hand-edited settings never applied and the first-run
wizard reappeared each time.

Two distinct causes, both invisible in the source:

1. `main.tsx` imported `./bridge/tauri-bridge` STATICALLY. Static imports are
   evaluated before the importing module's body, so the bridge's chunk — which
   Rollup had merged with the store — ran `loadPrefs()` before the top-level
   `await`.
2. Making that import dynamic was not enough. A dynamic import still evaluates
   its chunk's static dependencies first, and the `tauri-bridge` chunk still
   statically imported `app-store` because they shared the vendored
   shared-domain helpers. The store therefore still evaluated before
   `createTauriBridge()` had assigned `window.zen`.

The fix is both halves: `main.tsx` statically imports only `portable-config`,
and `rendererManualChunk` pins `bootstrap-config` (portable-config + smol-toml +
Vite's `__vitePreload` helper, which the entry pulls in for every dynamic
import), `shared-domain` and `bridge-contract` into chunks of their own.

Because none of that is visible in the source or the type system, it is pinned
by two tests: `src/__tests__/main-bootstrap-order.test.ts` (source shape) and
`src/__tests__/bundle-boot-order.test.ts` (the built chunk graph — the one that
actually catches cause 2). A root `vitest.config.ts` scopes `bun run test` to
`src/**`, since an unscoped run also collects the suites vendored under
`packaging/arch/src/bun-cache/`.

### Test-runner quirks on this machine (not code bugs)

```sh
# Node 26 ships an experimental localStorage global that shadows jsdom's,
# and resolves the default ICU locale as "und". app-core must run from its own
# directory so the vendored vitest.config.ts (harper + oniguruma plugins) applies
# — passing `--root` overrides it and 19 tests fail on an unresolvable wasm URL.
cd packages/app-core && NODE_OPTIONS='--no-experimental-webstorage' LC_ALL=en_US.UTF-8 \
  ../../node_modules/.bin/vitest run
```

`CloudSettings.test.ts > restore from date > falls back to the closest earlier
backup` fails at UTC+8 and passes at UTC and America/New_York. The file is
byte-identical to upstream v2.45.0, so this is an upstream timezone assumption,
not a porting defect. It is also in dead-for-this-port code (cloud sync).

## Phase 2 — the new backend surface

### Implemented

| Cluster | Where | Notes |
|---|---|---|
| **Workflows** (10 members) | `src-tauri/src/vault/workflows.rs` + `src/bridge/workflows.ts` | Thin-backend split, same as the CSV databases: Rust owns `.zennotes/workflows/*.md` CRUD, the native export/import dialogs and the file primitives; the applier runs in the webview over vendored `@shared/workflows/*`, so the rules a plan is applied by are the rules it was planned by. Ports upstream's guarantees intact: pre-run bytes journalled to a fsynced `.jsonl` before each write, whole-run rollback on any failure, undo from recorded state (never computed inverses), drift reporting, interrupted-run recovery, and ledger pruning by count and by total bytes. |
| **Custom code languages** (6) | `src-tauri/src/code_languages.rs` + `src/bridge/code-languages.ts` | Same split as custom themes: Rust owns `~/.config/zennotes/languages/<id>/{manifest.json,grammar.tmLanguage.json}`, the traversal guards and the watcher; parsing and validation run in the webview through vendored shared-domain, so reserved fence tags have one definition. |
| `followExternalFileLink` | `src-tauri/src/standalone_links.rs` | Full port of upstream's `standalone-links.ts` (#626), incl. the bounded wikilink search (depth 4, 5000 entries, skips `node_modules`/dotdirs) and the scheme/anchor rules. 11 tests. |
| `openFileDialog` | `ipc/window_cmds.rs` | Native picker into the existing vault-aware `open_markdown_file` (#449). |
| `openAssetExternally`, `readVaultAssetBase64` | `ipc/vault_cmds.rs` | Both go through `notes::resolve_safe`, so neither can address outside the vault. |
| `retryWorkspaceBoot` | `tauri-bridge.ts` | Was the local vault only. Since 2.79.3 it re-attempts a remote workspace that was unreachable at launch (`workspace_retry_boot`). |
| `AppUpdateState.installable` | `tauri-bridge.ts` | `false` — no in-app updater, and on Arch the AUR package owns the install, which is exactly the case the field names. |

New Rust file primitives backing the applier: `db_list_dir`, `db_file_size`,
`db_append_text` (fsyncs — the journal's whole job is to survive a kill),
`db_delete_file`, `db_write_through_links`, `db_remove_through_links`. The last
two exist because `db_write_text` is temp-file-plus-rename, and a rename over a
symlinked note would replace the link with a regular file and detach it from its
target for good.

### Not carried over

| Gap | Why | Cost to close |
|---|---|---|
| **Cloud sync** (33 bridge members) | Deliberate non-goal: a local-first port has no hosted companion service. Every member reproduces the web bridge's degradation exactly and `supportsCloudSync` is false, so app-core hides the feature. See `src/bridge/cloud-sync.ts`. | Large. Upstream spends ~90 KB across `apps/desktop/src/main/cloud-{auth,auth-loopback,sync-client,sync-filesystem,sync-service,sync-upload-source}.ts`, incl. an OAuth loopback flow and a conflict-resolving sync engine. Much of the engine is already portable TS in `shared-domain/cloud-sync-*`, so a port would mostly be the auth flow, the HTTP client and a filesystem adapter. |
| **`exportNoteDocx`** | No Rust equivalent of the `docx` npm package, which upstream uses in-process to embed local images. Rejects with the web bridge's message. | Medium. Spec: `apps/desktop/src/main/note-docx.ts` (19 KB). `docx-rs` covers the document model. |
| **Grammar vetting for code languages** | Upstream tokenizes a corpus in a `worker_threads` worker with a 2 s ceiling, so a catastrophically backtracking grammar cannot hang the editor. Not ported: no node worker here. A self-installed pathological grammar can therefore stall the editor. | Medium. Both `vscode-textmate` and `vscode-oniguruma` are already in the renderer bundle, so the natural shape is a Web Worker plus `terminate()` on timeout, mirroring `isGrammarFastEnough`. |
| **`onFrameEscape`** | Electron relays Escape out of a cross-origin subframe from the main process; WRY exposes no hook below the page. Optional in the contract, so it is omitted rather than registered as a listener that could never fire. | Blocked on upstream WRY support. |
| **Workflow run lock is per-window** | Upstream holds the per-vault lock in its single main process. Here the promise chain lives in the webview, so two windows on one vault serialize only within each. | Small: move the queue behind a Rust mutex keyed by vault root. |

Still open from the previous mirror, unchanged: theme-dir first-run seeding, a
`zen-theme://` asset protocol, per-window vault sessions, and `listDatabases`
(dead code upstream). The auto-updater cluster remains out of scope, as does
`supportsCliInstall`; remote workspaces arrived in 2.79.3.

---

# Gap analysis: upstream ZenNotes v2.1.0 → v2.15.0 (historical)

Kept for the per-method history of the first mirror. Upstream reference clone
and conventions are the same as above.

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

### A. Quick wins — ✅ DONE (2026-07-22)

All items below are implemented and wired (commands in `vault_cmds.rs` /
`os_cmds.rs`, helpers in the matching `vault/` modules; stubs in
`tauri-bridge.ts` replaced with invokes). `.excalidraw` files are now
note-like across listing/meta/rename/move/duplicate (extension preserved;
JSON body never parsed for metadata; task scans skip drawings). Verified
end-to-end in the running app: "New Drawing" opens a live Excalidraw canvas
and `.zennotes/workspace.json` persists the pane layout. `openExternalFile`
and `revealFilePath`/`toggleDevTools` are code-reviewed + typechecked only.

Remaining phase-2 clusters: B themes/overrides, C databases, D config,
E stragglers (link metadata, Obsidian import, ephemeral vaults).

| Method | Difficulty | Spec | Notes |
|---|---|---|---|
| `readWorkspaceState` / `writeWorkspaceState` | trivial | `index.ts:2181-2203` | `<vault>/.zennotes/workspace.json`, raw JSON string, never parsed by the backend. Return `null` on ENOENT. Skip for ephemeral roots. Newest-wins merge vs localStorage is app-core's job. |
| `rootContentHiddenByInboxMode` | trivial | `vault.ts:1241` | `true` iff saved `primaryNotesLocation === 'inbox'` but a root scan (non-hidden, non-reserved dir or `.md`) would infer `'root'`. Reserved names: 4 system folders + `assets`/`attachements`/`_assets` + `.zennotes`. |
| `revealFilePath` | trivial | `index.ts:2599` | Absolute path → reveal in file manager (port already has reveal helpers in `os.rs`). |
| `toggleDevTools` | trivial | `index.ts:3003` | `open_devtools()`/`close_devtools()`; no-op in release builds is fine. |
| `createExcalidraw` | trivial | `vault.ts:3152` | Clone of `createNote` with `.excalidraw` extension and the fixed empty-scene JSON seed (`shared-domain/excalidraw.ts:32`). Called unconditionally by "New Drawing" — highest-priority stub to replace. |
| Deleted assets: `listDeletedAssets` / `purgeDeletedAsset` / `emptyDeletedAssets` | trivial | `vault.ts:3477-3517` | Store: `<vault>/.zennotes/deleted-assets/<uuid-token>/` + `.zn-deleted.json` sidecar `{path,name,deletedAt}`. Token regex `^[0-9a-f-]{36}$`. Separate mechanism from the notes `trash/` folder. Also add `deletedAt` when `deleteAsset` writes the sidecar. |
| `openExternalFile` | easy | `index.ts:2606` | Resolve `file://` / `~` / absolute → open with OS default app; return `{ok,error}` — never throw. The confirm dialog + link filtering live in app-core (free). Keep `'desktop-only'` sentinel semantics if stubbed. |

### B. Custom themes + overrides — ✅ DONE (2026-07-22)

Implemented: `src-tauri/src/custom_css.rs` (scan/watch/delete/reveal/reserve
with traversal guards) + `src/bridge/custom-css.ts` (vendored
`parseThemeManifest` / `scaffoldThemeCss`; watcher pings fan out fresh
lists). Verified live: an external override file + config.toml enablement
restyled the running app. Deferred polish: first-run Soft Paper/README
seeding; `zen-theme://` protocol for assets referenced inside theme CSS.
Original notes below.

#### (original plan)

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

### C. CSV databases — ✅ DONE (2026-07-22)

Implemented with the thin-backend split as planned: `src/bridge/databases.ts`
(upstream orchestration + vendored shared-domain CSV/inference logic) over
generic vault file commands in `vault_cmds.rs` (`db_read_text` /
`db_write_text` atomic / `db_exists` / `db_mkdir` / `db_rename` /
`db_folder_root_rel` / `db_create_record_page`). The watcher normalizes
database files to the canonical `data.csv` path with scope `database`
(Rust port of `databaseCsvPathFor`). `listDatabases` stays a stub (dead
code upstream). Verified live: palette "New Database" produced the exact
upstream on-disk layout. Original notes below.

#### (original plan)

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

### D. Portable config (`config.toml`) — ✅ DONE (2026-07-22)

Implemented as a thin-backend split: `src/bridge/portable-config.ts` holds
upstream's annotated TOML serializer/deserializer nearly verbatim (byte-
compatible config files; own-write guard + write queue included), while
`src-tauri/src/app_config.rs` does XDG dir resolution, atomic writes, and the
debounced watcher (`config://file-changed` carries raw text). `getConfigSync`
stays synchronous because `main.tsx` awaits the config bootstrap and then
imports app-core **dynamically** — a static import would evaluate the store
before the top-level await resolves (the one Tauri-specific trap here).
Verified live: seed-from-localStorage on first boot, external hand-edits
apply without restart. Original notes kept below for reference.

#### (original plan)

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

### E. Remaining misc — ✅ DONE (2026-07-22)

All three implemented: `fetchLinkMetadata` (Rust `link_metadata.rs`, ureq +
upstream guard parity, live-verified), `convertObsidianExcalidraw`
(shared-domain extraction in the webview + `vault_write_drawing`), and
`openFolderTemporary` (ephemeral sessions with `.zennotes` write gates,
adapted to the single-active-vault backend — per-window vaults remain the
one architectural gap vs upstream). **The v2.15 bridge surface is now
complete**; the only intentional stub is `listDatabases` (dead code
upstream). Remaining nice-to-haves: theme-dir first-run seeding,
`zen-theme://` asset protocol, per-window vault sessions. Original notes
below.

#### (original plan)

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

Web/server/CLI (`zn`), auto-updater, Raycast, TikZ. (The MCP server and its
install flows were added after v2.73; see the top of this file.) Upstream
UI touching these already degrades via the existing capability flags and
"unavailable" stubs in `tauri-bridge.ts`.
