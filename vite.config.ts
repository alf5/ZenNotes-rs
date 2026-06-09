import { resolve } from 'node:path'
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// Mirror of the ZenNotes web/desktop renderer build (apps/web/vite.config.ts).
// Same manual-chunk + module-preload strategy so the reused frontend bundles
// identically; aliases repointed to the vendored packages under ./packages.

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
    dep.includes('vendor-function-plot')
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
    ]
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
  plugins: [react()],
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
