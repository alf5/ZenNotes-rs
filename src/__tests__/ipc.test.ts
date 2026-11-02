import { readdirSync, readFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it, vi } from 'vitest'

const tauriInvoke = vi.fn()
vi.mock('@tauri-apps/api/core', () => ({ invoke: tauriInvoke }))

const { invoke, toError } = await import('../bridge/ipc')

describe('invoke', () => {
  it('passes results and its arguments through', async () => {
    tauriInvoke.mockResolvedValueOnce(42)
    await expect(invoke('cmd', { a: 1 })).resolves.toBe(42)
    expect(tauriInvoke).toHaveBeenCalledWith('cmd', { a: 1 }, undefined)
  })

  it('rejects with an Error whose message is what Rust said', async () => {
    tauriInvoke.mockRejectedValueOnce('That folder already exists')
    const err = await invoke('cmd').catch((reason: unknown) => reason)
    expect(err).toBeInstanceOf(Error)
    expect((err as Error).message).toBe('That folder already exists')
  })
})

describe('toError', () => {
  it('keeps an Error, and reads any other shape', () => {
    const original = new Error('x')
    expect(toError(original)).toBe(original)
    expect(toError({ message: 'y' }).message).toBe('y')
    expect(toError({ code: 1 }).message).toBe('{"code":1}')
    expect(toError(undefined).message).toBe('undefined')
  })
})

describe('the bridge', () => {
  it('reaches Rust only through ipc.ts, so no rejection stays a string', () => {
    const dir = join(dirname(fileURLToPath(import.meta.url)), '..', 'bridge')
    const direct = readdirSync(dir)
      .filter((name) => name.endsWith('.ts') && name !== 'ipc.ts')
      .filter((name) =>
        /import\s*\{[^}]*\binvoke\b[^}]*\}\s*from\s*'@tauri-apps\/api\/core'/.test(
          readFileSync(resolve(dir, name), 'utf8')
        )
      )
    expect(direct).toEqual([])
  })
})
