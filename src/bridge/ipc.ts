/**
 * The one way the bridge reaches Rust. A Rust command that returns `Err(String)`
 * rejects with that bare string, where Electron's IPC rejects with an `Error`,
 * and app-core reads `(err as Error).message` and `err instanceof Error` all
 * over: a string there showed "undefined" or lost the reason for a generic
 * fallback. So every rejection becomes an `Error` here.
 */
import {
  invoke as tauriInvoke,
  type InvokeArgs,
  type InvokeOptions
} from '@tauri-apps/api/core'

export function toError(reason: unknown): Error {
  if (reason instanceof Error) return reason
  if (typeof reason === 'string') return new Error(reason)
  const message = (reason as { message?: unknown } | null | undefined)?.message
  if (typeof message === 'string') return new Error(message)
  try {
    return new Error(JSON.stringify(reason) ?? String(reason))
  } catch {
    return new Error(String(reason))
  }
}

export async function invoke<T>(
  command: string,
  args?: InvokeArgs,
  options?: InvokeOptions
): Promise<T> {
  try {
    return await tauriInvoke<T>(command, args, options)
  } catch (reason) {
    throw toError(reason)
  }
}
