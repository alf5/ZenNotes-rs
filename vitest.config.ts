import { defineConfig } from 'vitest/config'
import { resolve } from 'node:path'

// Root-level tests cover the PORT's own code (src/), not the vendored packages
// — those have their own configs and are run from their own directories.
// `include` is scoped deliberately: without it vitest also collects the test
// files vendored inside `packaging/arch/src/bun-cache/**`, which are other
// projects' suites and fail on a globals config that was never theirs.
export default defineConfig({
  resolve: {
    alias: {
      '@renderer': resolve(__dirname, './packages/app-core/src'),
      '@shared': resolve(__dirname, './packages/shared-domain/src'),
      '@bridge-contract': resolve(__dirname, './packages/bridge-contract/src')
    }
  },
  test: {
    include: ['src/**/*.test.ts'],
    exclude: ['**/node_modules/**', 'dist/**', 'packaging/**', 'src-tauri/**']
  }
})
