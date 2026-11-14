/**
 * config.toml is shared with zn, ZenNotes' terminal client, which keeps its
 * own tables in it (`[terminal]`). Upstream 2.83 carries whatever the app does
 * not write through every rewrite (429defec); the serializer that does it is
 * spliced from upstream, but the read-before-write that feeds it is the port's
 * own IO layer, so this holds that layer to it.
 */
import { beforeEach, describe, expect, it, vi } from 'vitest'

let fileText: string | null = null
const writes: string[] = []

vi.mock('@tauri-apps/api/core', () => ({
  invoke: async (command: string, args: Record<string, unknown> = {}) => {
    switch (command) {
      case 'config_file_path':
        return '/home/test/.config/zennotes/config.toml'
      case 'config_file_read':
        return fileText
      case 'config_file_write':
        fileText = args.text as string
        writes.push(fileText)
        return undefined
      default:
        throw new Error(`unexpected ${command}`)
    }
  }
}))

vi.mock('@tauri-apps/api/event', () => ({
  listen: async () => () => {}
}))

const ZN_TABLE = '[terminal]\nfont = "Iosevka"\n'

async function freshModule(): Promise<typeof import('../bridge/portable-config')> {
  vi.resetModules()
  return await import('../bridge/portable-config')
}

describe('config.toml keeps what zn wrote', () => {
  beforeEach(() => {
    fileText = null
    writes.length = 0
  })

  it('keeps a zn table when the app normalizes the file at start', async () => {
    fileText = `config_version = 1\n\n${ZN_TABLE}`
    const config = await freshModule()
    await config.initPortableConfig()
    expect(writes).toHaveLength(1)
    expect(fileText).toMatch(/^\[terminal\]$/m)
    expect(fileText).toContain('font = "Iosevka"')
    // The app's own options were filled in around it.
    expect(fileText).toContain('[vim]')
  })

  it('keeps what zn saved after the app last read the file', async () => {
    fileText = `config_version = 1\n\n${ZN_TABLE}`
    const config = await freshModule()
    await config.initPortableConfig()
    // zn rewrites its table while the app runs.
    fileText = fileText!.replace('font = "Iosevka"', 'font = "Berkeley Mono"')
    await config.setPortableConfig({ vimMode: false })
    expect(fileText).toContain('font = "Berkeley Mono"')
    expect(fileText).not.toContain('Iosevka')
    expect(config.deserializeConfig(fileText!).portable.vimMode).toBe(false)
  })

  it('writes a file that never existed without inventing anything', async () => {
    const config = await freshModule()
    await config.initPortableConfig()
    expect(writes).toHaveLength(0)
    await config.setPortableConfig({ vimMode: true })
    expect(fileText).not.toMatch(/^\[terminal\]$/m)
    expect(config.deserializeConfig(fileText!).portable.vimMode).toBe(true)
  })
})
