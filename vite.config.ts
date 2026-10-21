import { createReadStream, readFileSync } from 'node:fs'
import { cp } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { dirname, resolve, sep } from 'node:path'
import { defineConfig, type Plugin } from 'vite'
import react from '@vitejs/plugin-react'
import { harperWasmAsset } from './tooling/vite/harper-wasm-asset'

// Mirror of the ZenNotes web/desktop renderer build (apps/web/vite.config.ts).
// Same manual-chunk + module-preload strategy so the reused frontend bundles
// identically; aliases repointed to the vendored packages under ./packages.

// Excalidraw resolves its hand-drawn fonts from a base URL. With
// EXCALIDRAW_ASSET_PATH unset it falls back to the esm.sh CDN, which the
// webview CSP blocks. Upstream desktop serves the fonts via a custom
// `zen-excalidraw://` protocol; here we take the web build's approach instead:
// bundle the woff2 files into dist so Tauri serves them same-origin. The
// renderer points EXCALIDRAW_ASSET_PATH at `excalidraw-assets/`; Excalidraw
// appends `fonts/<Family>/<file>`.
const excalidrawFontsDir = resolve(
  dirname(createRequire(resolve(__dirname, 'package.json')).resolve('@excalidraw/excalidraw')),
  'fonts'
)
const EXCALIDRAW_FONTS_URL_PREFIX = '/excalidraw-assets/fonts/'

function onigurumaDataUrl(): Plugin {
  const virtualId = '\0zennotes:oniguruma-wasm-data-url'
  const wasmPath = createRequire(resolve(__dirname, 'package.json')).resolve(
    'vscode-oniguruma/release/onig.wasm'
  )
  return {
    name: 'zennotes-oniguruma-data-url',
    enforce: 'pre',
    resolveId(id) {
      if (id === 'vscode-oniguruma/release/onig.wasm?url') return virtualId
      return null
    },
    load(id) {
      if (id !== virtualId) return null
      const bytes = readFileSync(wasmPath)
      const url = `data:application/wasm;base64,${bytes.toString('base64')}`
      return `export default ${JSON.stringify(url)}`
    }
  }
}

function excalidrawFontMime(path: string): string {
  if (/\.woff2$/i.test(path)) return 'font/woff2'
  if (/\.woff$/i.test(path)) return 'font/woff'
  if (/\.otf$/i.test(path)) return 'font/otf'
  if (/\.ttf$/i.test(path)) return 'font/ttf'
  return 'application/octet-stream'
}

function excalidrawFonts(): Plugin {
  return {
    name: 'zennotes-excalidraw-fonts',
    // Dev: serve the fonts straight from node_modules so the same URLs resolve.
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        const url = req.url?.split('?')[0]
        if (!url || !url.startsWith(EXCALIDRAW_FONTS_URL_PREFIX)) return next()
        const rel = decodeURIComponent(url.slice(EXCALIDRAW_FONTS_URL_PREFIX.length))
        const abs = resolve(excalidrawFontsDir, rel)
        if (
          (abs !== excalidrawFontsDir && !abs.startsWith(excalidrawFontsDir + sep)) ||
          !/\.(woff2?|otf|ttf)$/i.test(abs)
        ) {
          res.statusCode = 404
          res.end()
          return
        }
        res.setHeader('Content-Type', excalidrawFontMime(abs))
        res.setHeader('Cache-Control', 'public, max-age=31536000, immutable')
        createReadStream(abs)
          .on('error', () => {
            res.statusCode = 404
            res.end()
          })
          .pipe(res)
      })
    },
    // Build: copy the fonts into the bundle so Tauri ships and serves them.
    async closeBundle() {
      await cp(excalidrawFontsDir, resolve(__dirname, 'dist/excalidraw-assets/fonts'), {
        recursive: true
      })
    }
  }
}

function rendererManualChunk(id: string): string | undefined {
  const normalizedId = id.split('\\').join('/')

  // The boot-order guard. `src/main.tsx` awaits `initPortableConfig()` before
  // anything else loads, because app-core's store reads `getConfigSync()` while
  // it builds its initial state and memoizes the result. A static import is
  // evaluated before the importing module's body, so if any module the entry
  // imports statically lands in the same chunk as that store, the store wins
  // the race and config.toml is ignored for the whole session.
  //
  // main.tsx keeps its static graph down to portable-config, and this rule
  // keeps portable-config (with its TOML parser) in a chunk of its own, so the
  // two cannot be merged by a future re-vendor that shifts the module graph
  // around. Guarded by src/__tests__/main-bootstrap-order.test.ts.
  //
  // Vite's `__vitePreload` helper rides along deliberately: the entry calls it
  // for every dynamic import, so wherever that helper lands the entry gains a
  // STATIC import of — and therefore evaluates — the whole chunk. Left alone it
  // was grouped with the store, which re-created the exact race this rule
  // exists to prevent even after main.tsx stopped importing the bridge.
  if (
    normalizedId.endsWith('/src/bridge/portable-config.ts') ||
    normalizedId.includes('/node_modules/smol-toml/') ||
    normalizedId.includes('vite/preload-helper')
  ) {
    return 'bootstrap-config'
  }

  // The other half of the same guard. `src/bridge/tauri-bridge.ts` is imported
  // dynamically, but a dynamic import still evaluates its chunk's STATIC
  // dependencies first — so when Rollup grouped the shared-domain helpers the
  // bridge uses into the `app-store` chunk, `await import('./bridge/tauri-bridge')`
  // evaluated app-core's store (and its memoizing `loadPrefs()`) BEFORE
  // `createTauriBridge()` had assigned `window.zen`. The store saw no bridge,
  // fell back to defaults, and config.toml was ignored for the session.
  //
  // shared-domain and bridge-contract are separate packages that app-core and
  // the bridge both consume, so giving each its own chunk is the honest split
  // as well as the one that keeps the store out of the bridge's static graph.
  if (normalizedId.includes('/packages/shared-domain/src/')) {
    return 'shared-domain'
  }
  if (normalizedId.includes('/packages/bridge-contract/src/')) {
    return 'bridge-contract'
  }
  if (normalizedId.endsWith('/packages/app-core/src/lib/wikilinks.ts')) {
    return 'app-wikilinks'
  }
  if (normalizedId.endsWith('/packages/app-core/src/lib/local-assets.ts')) {
    return 'app-local-assets'
  }
  if (normalizedId.endsWith('/packages/app-core/src/store.ts')) {
    return 'app-store'
  }

  if (!id.includes('node_modules')) return undefined

  // `@xyflow/react` (and the zustand 4 nested under it) must NOT ride this
  // rule: `/react/` matches it as a substring, and vendor-react is the one
  // chunk the entry both statically imports and modulepreloads, so React Flow
  // was fetched and evaluated on every launch for a feature that is off by
  // default. No named chunk for it either (see the mermaid comment below):
  // left alone, it lands in the lazily-imported WorkflowsView chunk and is
  // fetched the first time a Workflows tab renders.
  if (
    (id.includes('/react/') || id.includes('/react-dom/') || id.includes('/zustand/')) &&
    !id.includes('/@xyflow/')
  ) {
    return 'vendor-react'
  }
  if (id.includes('/@codemirror/language-data/')) {
    return 'vendor-editor-languages'
  }
  // The merge view only serves reviewing an assistant's edits, which
  // cm-agent-review.ts imports on demand. The rule below would pull it into
  // vendor-editor, onto every launch; left alone it stays in that lazy chunk.
  // (Upstream has this rule in apps/desktop/electron.vite.config.ts only; the
  // web build lacks it, and this renderer is the desktop one.)
  if (id.includes('/@codemirror/merge/')) return undefined
  if (
    id.includes('/@codemirror/') ||
    id.includes('/codemirror/') ||
    id.includes('/@lezer/') ||
    id.includes('/@replit/codemirror-vim/')
  ) {
    return 'vendor-editor'
  }
  if (
    id.includes('/remark-') ||
    id.includes('/rehype-') ||
    id.includes('/unified/') ||
    id.includes('/unist-util-visit/') ||
    id.includes('/gray-matter/') ||
    id.includes('/katex/')
  ) {
    return 'vendor-markdown'
  }
  if (id.includes('/highlight.js/')) {
    return 'vendor-highlight'
  }
  if (id.includes('/vscode-textmate/') || id.includes('/vscode-oniguruma/')) {
    return 'vendor-textmate'
  }
  // No manualChunks rule for the diagram stacks on purpose. Mermaid fences are
  // drawn by Pele, reached only through `import("./pele-runtime")` in
  // mermaid-render.ts; mermaid.js itself (with cytoscape / dagre) now arrives
  // only with Excalidraw's Mermaid-to-Excalidraw dialog, also behind a dynamic
  // import. Both stay async only while Rollup is left to chunk them. When
  // mermaid's modules were forced into a named chunk, Rollup hoisted that chunk
  // into the entry's STATIC graph:
  // `index-*.js` ended up with `import{m as gt}from"./vendor-mermaid-*.js"`,
  // where gt is mermaid itself, so every launch fetched and evaluated 2.5MB of
  // diagram code (and vendor-markdown with it, which vendor-mermaid imports)
  // before the user opened a note. Narrowing the rule to just '/mermaid/' does
  // not help; the grouping itself is what breaks the async boundary. Left to
  // Rollup, mermaid lands in its own async chunks (mermaid.core-*.js and
  // friends) and is fetched the first time a diagram actually renders. Total
  // bundle size is unchanged; only the boot path is.
  if (id.includes('/jsxgraph/')) {
    return 'vendor-jsxgraph'
  }
  if (id.includes('/function-plot/')) {
    return 'vendor-function-plot'
  }

  // Keep the tiny `?url` wasm-locator modules out of this chunk so it stays a
  // pure dynamic import, only fetched when a Typst formula is actually rendered.
  if (id.includes('/@myriaddreamin/') && !id.includes('.wasm')) {
    return 'vendor-typst'
  }

  if (id.includes('/d3')) {
    return 'vendor-d3'
  }
  return undefined
}

function resolveRendererModulePreloads(
  _filename: string,
  deps: string[],
  context: { hostType: 'html' | 'js' }
): string[] {
  if (context.hostType === 'html') {
    return deps.filter((dep) => dep.includes('vendor-react'))
  }
  return deps.filter((dep) => !isDeferredRendererPreload(dep))
}

function isDeferredRendererPreload(dep: string): boolean {
  return (
    dep.includes('NoteHoverPreview-') ||
    dep.includes('Preview-') ||
    dep.includes('WorkflowsView-') ||
    dep.includes('xyflow') ||
    dep.includes('wardley-') ||
    dep.includes('vendor-markdown') ||
    dep.includes('vendor-highlight') ||
    dep.includes('vendor-textmate') ||
    dep.includes('vendor-d3') ||
    // Pele, the mermaid-fence renderer: its runtime chunk, after which each
    // diagram type fetches a chunk of its own on first use.
    dep.includes('pele-runtime') ||
    // D2, the ```d2 renderer: one chunk of about 8 MB with its WebAssembly
    // compiler inside, fetched only when a note holds a d2 fence.
    dep.includes('d2-runtime') ||
    // mermaid.js (Excalidraw's Mermaid dialog only) splits into mermaid.core plus
    // a chunk per diagram type, with cytoscape/dagre alongside.
    dep.includes('mermaid') ||
    dep.includes('cytoscape') ||
    dep.includes('dagre') ||
    dep.includes('vendor-jsxgraph') ||
    dep.includes('vendor-function-plot') ||
    dep.includes('vendor-typst')
  )
}

const host = process.env.TAURI_DEV_HOST

export default defineConfig({
  root: __dirname,
  // Tauri serves the built bundle from a custom protocol; relative asset
  // paths keep the HTML portable across the dev server and the bundle.
  base: './',
  resolve: {
    alias: [
      { find: '@renderer', replacement: resolve(__dirname, './packages/app-core/src') },
      { find: '@shared', replacement: resolve(__dirname, './packages/shared-domain/src') },
      { find: '@bridge-contract', replacement: resolve(__dirname, './packages/bridge-contract/src') }
    ],
    // app-core is consumed as source, so its bare `react` / `react-dom`
    // imports can resolve to a second React instance — "Invalid hook call".
    // Pin every React import to the single hoisted copy (upstream parity).
    dedupe: ['react', 'react-dom']
  },
  // Tauri expects a fixed dev port; fail rather than silently pick another.
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: host || false,
    hmr: host ? { protocol: 'ws', host, port: 5174 } : undefined,
    watch: {
      ignored: ['**/src-tauri/**']
    }
  },
  plugins: [onigurumaDataUrl(), harperWasmAsset(), react(), excalidrawFonts()],
  // Typst ships a WASM compiler loaded lazily via `?url` + dynamic import; keep
  // it out of the esbuild dep pre-bundler so the wasm glue stays intact. The
  // same goes for Harper, whose worker is an inline blob the pre-bundler
  // would otherwise rewrite.
  optimizeDeps: {
    exclude: [
      '@myriaddreamin/typst.ts',
      '@myriaddreamin/typst-ts-web-compiler',
      '@myriaddreamin/typst-ts-renderer',
      'harper.js'
    ]
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    target: 'esnext',
    chunkSizeWarningLimit: 3500,
    modulePreload: {
      resolveDependencies: resolveRendererModulePreloads
    },
    sourcemap: false,
    rollupOptions: {
      output: {
        manualChunks: rendererManualChunk
      }
    }
  }
})
