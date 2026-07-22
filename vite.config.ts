import { createReadStream } from 'node:fs'
import { cp } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { dirname, resolve, sep } from 'node:path'
import { defineConfig, type Plugin } from 'vite'
import react from '@vitejs/plugin-react'

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

  if (id.includes('/react/') || id.includes('/react-dom/') || id.includes('/zustand/')) {
    return 'vendor-react'
  }
  if (id.includes('/@codemirror/language-data/')) {
    return 'vendor-editor-languages'
  }
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
  if (id.includes('/mermaid/') || id.includes('/cytoscape/') || id.includes('/dagre/')) {
    return 'vendor-mermaid'
  }
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
    dep.includes('wardley-') ||
    dep.includes('vendor-markdown') ||
    dep.includes('vendor-highlight') ||
    dep.includes('vendor-d3') ||
    dep.includes('vendor-mermaid') ||
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
  plugins: [react(), excalidrawFonts()],
  // Typst ships a WASM compiler loaded lazily via `?url` + dynamic import; keep
  // it out of the esbuild dep pre-bundler so the wasm glue stays intact.
  optimizeDeps: {
    exclude: [
      '@myriaddreamin/typst.ts',
      '@myriaddreamin/typst-ts-web-compiler',
      '@myriaddreamin/typst-ts-renderer'
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
