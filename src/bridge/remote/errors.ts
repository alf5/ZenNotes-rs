/**
 * The two ways a request to a ZenNotes server fails, as upstream's
 * apps/desktop/src/main/remote/server-client.ts tells them apart. Rust sends
 * them over as `{ remoteError, status?, message }` and `ipc.ts` turns them back
 * into these, so `instanceof` and `status` work as they do upstream.
 */

/** The server never answered: DNS, refused, timeout. The workspace may be
 *  fine; the network is not. Callers must never read this as "absent". */
export class RemoteConnectionError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'RemoteConnectionError'
  }
}

/** The server answered with a non-2xx status: it is alive and made a
 *  decision. `status` lets callers separate a 404 from an auth failure. */
export class RemoteRequestError extends Error {
  constructor(
    message: string,
    readonly status: number
  ) {
    super(message)
    this.name = 'RemoteRequestError'
  }
}

interface RemoteErrorShape {
  remoteError: 'connection' | 'request' | 'other'
  status?: number
  message: string
}

function isRemoteErrorShape(value: unknown): value is RemoteErrorShape {
  if (!value || typeof value !== 'object') return false
  const shape = value as Partial<RemoteErrorShape>
  return typeof shape.remoteError === 'string' && typeof shape.message === 'string'
}

/** The Error a structured Rust rejection stands for, or null for any other. */
export function remoteErrorFrom(reason: unknown): Error | null {
  if (!isRemoteErrorShape(reason)) return null
  switch (reason.remoteError) {
    case 'connection':
      return new RemoteConnectionError(reason.message)
    case 'request':
      return new RemoteRequestError(reason.message, reason.status ?? 0)
    default:
      return new Error(reason.message)
  }
}
