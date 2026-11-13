# Remote workspaces: plan

**Goal:** ZenNotes-rs opens a vault on a server that speaks upstream's `self-hosted-http-v1`
protocol: upstream's `znserver` or our zenserver at zen.sys4.au. It should behave as upstream's
desktop app does in remote mode, with the same UI, the same refusals and the same live updates.

**Status (2026-11-13):** phases 0–4 built, for 2.79.3; phase 5 (MCP) waits for a
later release. What landed, and where it differs from upstream, is in GAP-ANALYSIS.md
under "Remote workspaces". Two things changed from this plan while building it: asset
rename and move keep upstream's missing capability check (they predate the 2.24 asset
family, so checking would break older servers), and the renderer's own URL
normalization needed a build-time swap for the `https://` default.

Upstream references are to `~/delme/zennotes` at v2.79.0+3. They use these abbreviations:

| Abbreviation | File |
|---|---|
| IDX | `apps/desktop/src/main/index.ts` |
| SC | `apps/desktop/src/main/remote/server-client.ts` |
| CONN | `apps/desktop/src/main/remote/connection.ts` |
| STORE | `packages/app-core/src/store.ts` |

---

## 1. What upstream does

- **Auth is a bearer token and nothing else.** Every JSON request carries
  `Authorization: Bearer <token>` (SC:629-633), and so do the upload, the raw asset fetch and the
  WebSocket upgrade. The desktop app has no login step: `getServerSession` and
  `loginServerSession` are stubs that report success.
- **Connecting** happens in `setRemoteWorkspace` (IDX:2561-2620):
  1. `GET /api/capabilities`
  2. `GET /api/vault`
  3. If the profile names a different vault, `POST /api/vault/select {path}`, then fetch
     `/capabilities` again
  4. Start the change feed
  5. Save `workspaceMode: 'remote'` in the config
- **At boot**, the app reconnects from the saved config with `persist: false`. On failure it
  keeps the error as `bootError`, and the renderer shows the "Can't reach your ZenNotes server"
  screen. Retrying is manual only, through the "Try again" button (IDX:3245-3306).
- **There is no dispatch layer.** Each IPC handler has its own
  `if (isRemoteWorkspaceActive()) return client.x(...)` before its local branch, about 60 in all.
  Databases are the exception: they are built by the shared `createDatabaseOps` over note and
  folder endpoints, with `createAbsenceAwareReader` (IDX:4435-4484). Workflows are planned
  client-side with `prepareWorkflowRun`, then applied on the server with `POST /workflows/apply`.
- **Live updates** come over a WebSocket at `/api/watch`. Each text frame is one
  `VaultChangeEvent`.
  - On close, it reconnects after `min(30 s, 1 s·2^n)`. The counter resets after 15 s of a
    stable connection.
  - While the socket is down, it polls with a synthetic `resync` every 30 s.
  - It also sends `resync` after a reconnect and after a 404 on a write ("stale path").
  - Events go to the renderer on the **same channel as the local watcher**.
- **TLS** trusts the system CA store as well as Node's bundled roots (`system-trust.ts`).
- **Refused in remote mode, by throwing:**
  - reveal in file manager, for notes, folders and the assets folder
  - Word export
  - workflow export and import
  - converting Obsidian Excalidraw files
  - Cloud sync
- **Quietly empty in remote mode:**
  - `workspaceState` reads and writes
  - agent edits
  - `rootContentHidden`
- **Gated on server capabilities** (`supportsAssetOps`, `supportsCustomTemplates`,
  `supportsWorkflows`): asset management, templates and workflows.
- **Concurrent edits are last-write-wins.** No If-Match, revisions or `409` handling exist
  anywhere.

The full route table is SC:87-627. Each method is one `GET` or `POST` under `/api`.

## 2. Design for the port

### Shape: route table in TypeScript, transport in Rust

- **TypeScript side:** `src/bridge/remote/client.ts` is a method-for-method port of SC, with the
  same names, paths and error semantics. Its transport is a single Rust command.
- **Rust side:** Rust owns the session, the token, TLS, the change feed and the byte streams
  (asset reads and uploads). **The token never reaches the webview.**

Why not dispatch in Rust, as upstream's main process does:

- **The logic around the routes is already vendored TypeScript.** That covers
  `prepareWorkflowRun`, `createDatabaseOps`, `createAbsenceAwareReader`, `stalePathMessage` and
  `parseServerErrorBody` in shared-domain. In TypeScript they run unchanged. In Rust they would
  have to be ported, and then they would go stale on every re-vendor. This is the same
  thin-backend choice already made for tasks, databases and workflows.
- **A re-vendor can diff one file against one file:** `server-client.ts` against `client.ts`.

Why not vendor upstream's browser bridge (`apps/web/src/bridge/http-bridge.ts`):

- It assumes the server hosted the page. It takes the WebSocket URL from `window.location` and
  depends on same-origin cookie sessions, `window.prompt` and page reloads.
- It is also outside the three vendored packages.

### Rust

- **Session state.** Add `remote: Mutex<Option<Arc<RemoteSession>>>` to `AppState`.
  `RemoteSession` holds:
  - the normalized base URL
  - the token
  - a `ureq::Agent`
  - the last `ServerCapabilities`
  - the profile id
  - the change-feed handle

  **Remote mode is app-wide**, because every window already follows the one open vault (2.79.1).
  That is simpler than upstream's per-window registry.
- **The local vault is fenced off while remote mode is on.** `AppState::current_root()` returns
  `None`, so every local `vault_*`, `db_*` and `workspace_state_*` command refuses.
  - **This is a safety requirement, not just tidiness.** The remote `VaultInfo.root` is a server
    path such as `/My vaults/Research`. A call that missed routing would otherwise resolve that
    path on the local disk.
  - The local watcher stops on connect and restarts on disconnect.
- **`remote_request` (async, `spawn_blocking`)** takes `{method, path, query?, body?}` and
  returns the JSON body. It fails with a structured error, `{kind: 'connection' | 'request',
  status?, message}`, which `src/bridge/ipc.ts` turns into `RemoteConnectionError` or
  `RemoteRequestError`, using the wording from CONN:40-68.
  - **Allowed calls:** `GET` and `POST` only, on paths that start with `/api/` and contain no
    `..`, scheme or host. The base URL is always the session's own.
  - **Timeouts:** 15 s to connect and 300 s to read, roughly Node's undici defaults. Upstream sets
    none explicitly.
- **Connect, disconnect and boot** run in Rust, because they need the token. These follow
  upstream's sequence:
  - `workspace_connect_remote`
  - `workspace_connect_remote_profile`
  - `vault_select_path`
  - `workspace_browse_server_directories`
  - `workspace_disconnect_remote`
  - the remote branch of `vault_get_current` and `retryWorkspaceBoot`

  The config keys match upstream's: `workspaceMode`, `remoteWorkspace {baseUrl}` and
  `remoteWorkspaceProfileId`, kept in `zennotes-rs.config.json`. After each of these calls,
  Rust emits `workspace://changed` so every window refreshes its cached `RemoteWorkspaceInfo`.
  Asset URLs are resolved synchronously, so the cache has to be current, as in upstream's
  preload.
- **Change feed:** a dedicated thread running blocking `tungstenite` over rustls. It follows the
  same rules as SC:496-627, for backoff, stable reset, the polling fallback, `resync` on reconnect
  and a stop flag. It emits `vault://change`, the event the local watcher already uses. The
  socket URL keeps any base path such as `/notes`; upstream drops it (SC:500).
- **Assets:**
  - **Display:** `syn-asset` gets a remote branch that fetches `/api/assets/raw?path=` and passes
    the bytes through. It has to become `register_asynchronous_uri_scheme_protocol` so network
    reads don't block the main thread.
  - **Uploads:** `remote_upload_files(paths, notePath)` serves drag-drop, where Linux gives native
    paths. `remote_upload_bytes(name, base64, notePath)` serves pasted images. Both stream a
    hand-built multipart body; ureq 2 has no multipart builder.
  - **Open externally:** download into a fresh temp directory, then hand the file to the OS opener
    (IDX:4821).
- **TLS:** turn on the OS trust store for the remote agent and the socket, for parity with
  `system-trust.ts`. A homelab server behind a private CA must work.
- **Undo history:** in remote mode the key is `remote:<profileId>:<root>` (IDX:3999-4004).

### TypeScript

- **`src/bridge/remote/client.ts`:** the SC port. It includes the stale-path rule: a 404 on a
  request whose body has a `path` throws `stalePathMessage` and asks Rust for a `resync`. A 404 on
  a GET read stays a plain 404, because databases treat that as "absent".
- **`src/bridge/remote/vault.ts`:** the remote branch of every bridge member in the IDX:3942-5112
  table.
  - **Databases:** use `createDatabaseOps` over the note and folder endpoints, as upstream does.
    The local path keeps `src/bridge/databases.ts`.
  - **Tasks** come from `GET /tasks`.
  - **Workflows** are planned with `prepareWorkflowRun`, then applied with `POST /workflows/apply`.
  - **Agent edits** come back empty.
  - **Reveal, workflow export/import and Excalidraw conversion** throw upstream's own messages.
- **`tauri-bridge.ts`:** each affected member becomes `remoteActive() ? remote.x(...) : invoke(...)`.
  The workspace members come off their stubs:
  - `getRemoteWorkspaceInfo`
  - `connectRemoteWorkspace`
  - `connectRemoteWorkspaceProfile`
  - `disconnectRemoteWorkspace`
  - `getServerCapabilities`
  - `selectVaultPath`
  - `browseServerDirectories`
  - `retryWorkspaceBoot`
- **`capabilities.ts`:** `supportsRemoteWorkspace` turns on in development builds first, with
  `import.meta.env.DEV`. It turns on everywhere in phase 4.

### Deliberate deviations from upstream

| Upstream | Port | Why |
|---|---|---|
| A URL with no scheme gets `http://` (CONN:24-28) | `https://` | Otherwise a public server receives the bearer token in cleartext before any redirect. Proposed default; see §6. |
| The WebSocket URL drops the base path (SC:500) | It keeps the base path | Bug |
| Disconnecting without a local vault leaves the feed running (IDX:2634-2642) | It stops the feed | Bug |
| No request timeouts | 15 s connect, 300 s read | A server that hangs shouldn't hang the app |

## 3. Phases

Each phase leaves master releasable, because the capability stays off outside development
builds until phase 4.

| # | Work | Done when |
|---|---|---|
| 0 | **Groundwork.** Secrets fallback per §6. TLS: OS roots for ureq; add `tungstenite`. The `AppState.remote` slot, plus the guard that refuses local commands while remote mode is on (with tests). | `cargo test` covers the guard. The fallback store follows the decision in §6. |
| 1 | **Connect.** `remote_request`, `RemoteSession`, connect / profile connect / disconnect / boot / retry, browse and select, `workspace://changed`, structured errors through `ipc.ts`. TS: `client.ts` for workspace routes, plus the 8 workspace bridge members. | A development build connects to a local `znserver` and to a local zenserver, picks a vault in the server directory picker, lists notes, survives a restart (boot reconnect), and shows the reconnect screen when the server is down. |
| 2 | **Vault operations.** The rest of `client.ts` and `remote/vault.ts`: notes, trash, archive, folders, search, comments, tasks, templates, settings, demo tour, databases, workflows, deleted assets, plus the refusals and quietly empty members. | Every bridge member in the IDX:3942-5112 table has a remote branch or a refusal, and the parity test (§4) passes. |
| 3 | **Bytes and live updates.** The change feed, async remote `syn-asset`, uploads (drag-drop and paste), open externally, the undo-history key. | An edit in the browser at the server shows up in the desktop window within a second. Killing the server shows polling, and restarting it resyncs. Images and PDFs display, and a pasted image uploads. |
| 4 | **Turn it on.** `supportsRemoteWorkspace: true`. Unhide the Help card "Connect the desktop app to a self-hosted server" and the Settings row "Saved Remote Workspaces" in `tooling/vite/vendored-copy.ts`. Reread both texts for Electron- or `zn`-specific wording and add swaps where needed. Decide whether "Run the self-hosted web version with Docker" stays hidden. Update GAP-ANALYSIS.md and README. | Release workflow green on all 5 jobs. Live smoke test against zen.sys4.au passes (§4). Tagged as the next patch release. |
| 5 | **MCP remote mode** (optional, separate release). `zennotes-rs mcp --server/--token`, `ZENNOTES_SERVER`, `ZENNOTES_REMOTE_TOKEN`, `credentials.toml` shared with `zn`, Settings → MCP → Server token (remove the swap at `vendored-copy.ts:58`). Needs a small Rust client for the MCP routes (upstream's `CliRemoteClient`). | The golden harness's remote cases pass against upstream's TypeScript under bun. |

## 4. Testing

- **TypeScript client (vitest, mocked `invoke`):** the TS-side cases from upstream's
  `server-client.test.ts`:
  - error typing
  - a stale-path 404 on a write versus a plain 404 on a read
  - template capability gating and the four template routes

  Also `store.test.ts`-style checks: `resync` re-pulls, and workspace info is re-read after boot.
- **Parity test:** a table in the test lists every SC method with its HTTP method and path. The
  test checks that `client.ts` has each one with the same verb and path.
  - Upstream isn't vendored here, so on a re-vendor the table is refreshed by hand from SC.
  - Add `src/bridge/remote/client.ts ← server-client.ts` and the change feed `← watchVaultChanges`
    to the "port-owned copies that rot" list.
- **Rust transport (local test servers):**
  - the bearer header is on every call, the upload and the socket upgrade
  - the path allowlist refuses absolute URLs, `..` and non-`/api/` paths
  - error mapping (connection versus status)
  - timeouts
  - the base path is kept for JSON requests and the socket
  - the change-feed cases from SC's tests: resubscribe after a restart, backoff when the upgrade
    is accepted then dropped, reset after a stable connection, stop prevents reconnects, a refused
    upgrade falls back to polling, an unreachable server doesn't panic
- **Guard:** every local `vault_*` / `db_*` command refuses while remote mode is on. This is a
  table test over `generate_handler!`, so a new command can't slip past it.
- **Live smoke** (the private-compositor + WebKit inspector setup):
  1. Against upstream's `znserver` release binary, the one the zenserver golden harness uses.
  2. Against zenserver-multi from its Compose setup, with two users and a shared vault, checking
     that roles are respected (a viewer's writes are refused with the server's message).
  3. Against zen.sys4.au **with a token minted for this device only**. On zenserver a token holds
     one current vault, so a shared token would switch the browser's vault too (DESIGN.md §2).
- **Not testable here:** macOS and Windows, beyond CI compiles; the macOS Keychain prompt.

## 5. Housekeeping when it ships

- GAP-ANALYSIS.md: rewrite the places that say this build has no remote workspaces. They are the
  MCP section, the not-ported table and the `retryWorkspaceBoot` note.
- zenserver-multi-rs DESIGN.md §10: replace "ZenNotes-rs can't connect to any server yet" with the
  release that can.
- Memory: the re-vendor rot list (§4) and the zenserver note.

## 6. Decisions

**Decided 2026-11-13: all four recommendations below.** Conflict detection goes on the server.
Tokens are never saved to a file. `https://` is the default scheme. MCP remote mode comes in a
later release.

1. **Concurrent edits.**
   - **Recommended:** keep this client protocol-identical to upstream and build conflict
     detection on the server, from the candidate in zenserver's DESIGN.md §5. That protects the
     browser and upstream's clients as well.
   - **Alternative:** this client sends a base-hash header that only zenserver understands.
2. **Token storage when no OS keychain is available.**
   - Today the fallback writes base64 tokens to `remote-workspace-secrets.json`. That is
     effectively plaintext, written without restricted permissions.
   - **Recommended:** match upstream. Don't save the token without a real keychain; connect for
     this session only, and say so.
   - **Alternative:** keep the file, written atomically with mode 0600.
   - This machine runs a Secret Service (`ksecretd`), so either choice works here.
3. **Default scheme.** `https://` when the URL has none (recommended), or `http://` as upstream
   does.
4. **MCP remote mode.** A later release (recommended, phase 5), or part of this one.

## 7. Risks

- **Asset display under WebKitGTK.** Large assets and video (range requests) through an
  asynchronous custom scheme are untried in this port.
- **Proxy limits at zen.sys4.au.** rathole hides client addresses, so every client counts as one
  address: 64 connections and 40 change-feed sockets per address. Uploads still depend on
  Traefik's raised 600 s read timeout.
- **Floating windows.** These are multi-window, and every window has to pick up
  `workspace://changed` before its next bridge call. Otherwise a window that missed it routes
  locally and hits the guard: an error, not data loss.
- **Last-write-wins.** Until decision 1 is built, two people editing one shared note lose the
  earlier save without warning, exactly as with the browser today.
