import type { Plugin } from 'vite'
import type { HiddenHelp } from './help-hide'

/**
 * Text in the vendored packages that is wrong for this port. `packages/` stays
 * byte-identical to upstream, so the wording is swapped as the bundle is built,
 * and only here. Each `find` must occur exactly once in its file: when upstream
 * rewords one, the build fails instead of quietly showing the old text again,
 * and src/__tests__/vendored-copy.test.ts says the same without a build.
 */
const SETTINGS = 'packages/app-core/src/components/SettingsModal.tsx'
const HELP = 'packages/app-core/src/lib/help.ts'
const COMMANDS = 'packages/app-core/src/lib/commands.ts'
const REPO = 'https://github.com/alf5/ZenNotes-rs'

export const VENDORED_COPY: ReadonlyArray<{ file: string; find: string; replace: string }> = [
  {
    // An Electron flag, which does nothing here.
    file: 'packages/app-core/src/lib/unsplash-key.ts',
    find: ' If a Secret Service keyring is running, launch ZenNotes with --password-store=gnome-libsecret.',
    replace: ' Start a Secret Service keyring, such as KWallet or GNOME Keyring, then restart ZenNotes-rs.'
  },
  {
    // The macOS share extension is not ported (GAP-ANALYSIS.md).
    file: 'packages/app-core/src/lib/whats-new.ts',
    find: "title: 'Share to ZenNotes on macOS',",
    replace: "title: 'Share to ZenNotes: not in this build',"
  },
  {
    file: 'packages/app-core/src/lib/whats-new.ts',
    find: "body: 'Choose Share, ZenNotes in Safari or any app to save a page or the text you selected as a note. Each vault picks where shares land.'",
    replace: "body: 'The original ZenNotes can now save a page or selected text from the macOS Share menu. ZenNotes-rs does not have this yet.'"
  },

  // Settings → MCP describes upstream's server: Electron running a compiled script.
  {
    file: SETTINGS,
    find: 'It uses the packaged Electron binary in plain-Node mode, so no separate Node install is required.',
    replace: 'It is this app\'s own binary, run as zennotes-rs mcp, so there is nothing else to install.'
  },
  {
    file: SETTINGS,
    find: 'Run npm run build so installers have an entry script to register.',
    replace: 'The ZenNotes-rs binary could not be located.'
  },
  {
    file: SETTINGS,
    find: 'Entry script compiled. Install a client below to connect it.',
    replace: 'Ready. Install a client below to connect it.'
  },
  { file: SETTINGS, find: '? "Not built" :', replace: '? "Not found" :' },
  {
    file: 'packages/shared-domain/src/mcp-clients.ts',
    find: 'Appends a [mcp_servers.zennotes] entry',
    replace: 'Appends a [mcp_servers.zennotes-rs] entry'
  },
  // The MCP server token only serves remote vaults, which this build has none of.
  { file: SETTINGS, find: '<McpServerTokenSetting />', replace: '' },
  {
    file: SETTINGS,
    find: '"zn connect",\n            "share",\n            "assistant",\n          ],\n',
    replace: '"zn connect",\n            "share",\n            "assistant",\n          ],\n          available: false,\n'
  },

  // Engine is `Tauri x (WebKitGTK y)` here (src-tauri/src/app_info.rs).
  {
    file: HELP,
    find: 'Print the ZenNotes version, operating system and architecture, the Electron or browser engine,',
    replace: 'Print the ZenNotes-rs version, operating system and architecture, the Tauri version and system webview (WebKitGTK, WebKit or WebView2),'
  },
  {
    file: HELP,
    find: 'lists what a bug report needs: operating system and architecture, the Electron or browser engine,',
    replace: 'lists what a bug report needs: operating system and architecture, the Tauri version and system webview (WebKitGTK, WebKit or WebView2),'
  },
  {
    file: SETTINGS,
    find: '"bug report",\n            "electron",\n',
    replace: '"bug report",\n            "tauri",\n            "webview",\n            "webkit",\n'
  },

  // Nothing is sent from here, so a switch offering to send it says what is not so.
  { file: SETTINGS, find: '{appInfo.runtime === "desktop" && <UsageCountSetting />}', replace: '' },
  {
    file: SETTINGS,
    find: '"statistics",\n          ],\n          available: appInfo.runtime === "desktop",\n',
    replace: '"statistics",\n          ],\n          available: false,\n'
  },

  // A port bug belongs in the port's tracker, not upstream's.
  {
    file: COMMANDS,
    find: "const APP_REPOSITORY_URL = 'https://github.com/ZenNotes/zennotes'",
    replace: `const APP_REPOSITORY_URL = '${REPO}'`
  },
  {
    file: COMMANDS,
    find: "const APP_RELEASES_URL = 'https://github.com/ZenNotes/zennotes/releases/latest'",
    replace: `const APP_RELEASES_URL = '${REPO}/releases/latest'`
  },
  {
    file: COMMANDS,
    find: "const APP_ISSUES_URL = 'https://github.com/ZenNotes/zennotes/issues'",
    replace: `const APP_ISSUES_URL = '${REPO}/issues'`
  },
  {
    file: 'packages/app-core/src/components/ErrorDetails.tsx',
    find: 'const APP_ISSUES_URL = "https://github.com/ZenNotes/zennotes/issues"',
    replace: `const APP_ISSUES_URL = "${REPO}/issues"`
  },

  // No global hotkeys on Wayland: the crate is X11-only (deep_links.rs).
  {
    file: HELP,
    find: 'configurable under Settings → Editor) to drop a small always-on-top window',
    replace: 'configurable under Settings → Editor; on Wayland, bind a compositor key to `zennotes-rs --quick-capture` instead) to drop a small always-on-top window'
  },
  {
    file: SETTINGS,
    find: "Floating capture window for thoughts you want in the vault without leaving whatever you're doing.",
    replace: "Floating capture window for thoughts you want in the vault without leaving whatever you're doing. On Wayland the hotkey may not fire: bind a compositor key to zennotes-rs --quick-capture instead."
  },

  // Windows here keep their system title bar, except Linux's frameless ones.
  {
    file: HELP,
    find: 'to remove the main window title and controls on macOS, Windows, and Linux, including tiling desktops such as Hyprland.',
    replace: 'to remove the main window title and controls on Linux, including tiling desktops such as Hyprland. On macOS and Windows the system title bar stays.'
  },

  // TikZ and PDF export are not in this build; config.toml keeps the setting.
  { file: HELP, find: 'five fenced block languages', replace: 'four fenced block languages' },
  {
    file: HELP,
    find: ' `tikz` for LaTeX-native coordinate systems, commutative diagrams, and figure-quality plots (the TeX engine runs on-device so no network is required);',
    replace: ''
  },
  {
    file: SETTINGS,
    find: 'How notes look when you export them to PDF.',
    replace: 'PDF export is not available in ZenNotes-rs, so this has no effect here. It stays for a config.toml shared with the original ZenNotes.'
  }
]

/**
 * Help for what this build does not have. Each name must occur once in help.ts,
 * under the same rule as the swaps above. `HELP_CLI` (the whole CLI section) is
 * emptied by the generated code below.
 */
export const HELP_HIDDEN: HiddenHelp = {
  cardTitles: [
    'Save a page or text from Safari and other apps (Share → ZenNotes)',
    'Search from Raycast on macOS',
    'Check for updates and install them',
    'Run the self-hosted web version with Docker',
    'Connect the desktop app to a self-hosted server',
    'Encrypt a Cloud vault end to end',
    'When Cloud reports a problem',
    'The CLI is the bridge to launchers',
    'Updates are release-driven'
  ],
  settingsSections: ['CLI'],
  settingsRows: [
    'Count this copy anonymously',
    'Updates and releases',
    'Use theme for PDF export',
    'Shares location (macOS)',
    'Saved Remote Workspaces'
  ]
}

function occurrences(code: string, text: string): number {
  return code.split(text).length - 1
}

/** Names `HELP_HIDDEN` holds that help.ts no longer has exactly once. */
export function staleHelpEntries(code: string): string[] {
  return [
    ...HELP_HIDDEN.cardTitles.map((name) => [name, `title: '${name}'`] as const),
    ...HELP_HIDDEN.settingsSections.map((name) => [name, `title: '${name}'`] as const),
    ...HELP_HIDDEN.settingsRows.map((name) => [name, `label: '${name}'`] as const)
  ]
    .filter(([, source]) => occurrences(code, source) !== 1)
    .map(([name]) => name)
}

function withHelpHidden(code: string): string {
  const stale = staleHelpEntries(code)
  if (stale.length > 0) {
    throw new Error(
      `vendored-copy: help.ts no longer has: ${stale.join(', ')}\n` +
        'Upstream changed this text. Update or remove the entry in tooling/vite/vendored-copy.ts.'
    )
  }
  return (
    code +
    `
import { hideHelpEntries } from '../../../../tooling/vite/help-hide'
hideHelpEntries(
  {
    cards: [HELP_QUICK_START, HELP_HOW_TO_GUIDES, HELP_CORE_CONCEPTS],
    settings: HELP_SETTINGS,
    clearCards: [HELP_CLI]
  },
  ${JSON.stringify(HELP_HIDDEN)}
)
`
  )
}

/** `code` with every swap for `file` made. Throws when one no longer matches once. */
export function applyVendoredCopy(file: string, code: string): string {
  let next = code
  for (const swap of VENDORED_COPY) {
    if (swap.file !== file) continue
    const found = occurrences(next, swap.find)
    if (found !== 1) {
      throw new Error(
        `vendored-copy: expected one match in ${file}, found ${found}: ${swap.find}\n` +
          'Upstream changed this text. Update or remove the entry in tooling/vite/vendored-copy.ts.'
      )
    }
    next = next.replace(swap.find, () => swap.replace)
  }
  return file === HELP ? withHelpHidden(next) : next
}

export function vendoredCopy(): Plugin {
  const files = [...new Set(VENDORED_COPY.map((swap) => swap.file))]
  return {
    name: 'zennotes-vendored-copy',
    enforce: 'pre',
    transform(code, id) {
      const path = id.split('?')[0].replaceAll('\\', '/')
      const file = files.find((candidate) => path.endsWith(`/${candidate}`))
      return file ? { code: applyVendoredCopy(file, code), map: null } : null
    }
  }
}
