import { describe, expect, it } from 'vitest'
import { humanIpcError } from './ipc-error'

describe('humanIpcError', () => {
  it('keeps only the sentence a person should read', () => {
    const wrapped = (inner: string) =>
      new Error(`Error invoking remote method 'cloud:backups:create': ${inner}`)
    expect(humanIpcError(wrapped('Error: The cloud service is unavailable.'), 'x')).toBe(
      'The cloud service is unavailable.'
    )
    expect(
      humanIpcError(wrapped('CloudServiceRequestError: This backup would exceed your plan limits.'), 'x')
    ).toBe('This backup would exceed your plan limits.')
    expect(
      humanIpcError(wrapped('CloudE2eeError: That passphrase does not unlock your encryption.'), 'x')
    ).toBe('That passphrase does not unlock your encryption.')
    expect(humanIpcError(new TypeError('fetch failed'), 'x')).toBe('fetch failed')
    expect(humanIpcError('not an error', 'Fallback.')).toBe('Fallback.')
  })
})
