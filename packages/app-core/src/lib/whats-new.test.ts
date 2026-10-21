import { describe, expect, it } from 'vitest'
import {
  CLOUD_ANNOUNCEMENT,
  compareVersions,
  RELEASE_NOTES_URL,
  WHATS_NEW,
  whatsNewAtLaunch,
  whatsNewBetween,
  whatsNewOnDemand,
  type WhatsNewEntry
} from './whats-new'

const entries: WhatsNewEntry[] = [
  { version: '2.68.0', highlights: [{ title: 'a', body: 'a' }] },
  { version: '2.70.0', highlights: [{ title: 'c', body: 'c' }] },
  { version: '2.69.0', highlights: [{ title: 'b', body: 'b' }] }
]
const versions = (list: WhatsNewEntry[]): string[] => list.map((e) => e.version)

describe('compareVersions', () => {
  it('orders dotted versions numerically, a prerelease below its release', () => {
    expect(compareVersions('2.68.0', '2.9.0')).toBe(1)
    expect(compareVersions('v2.68.0', '2.68.0')).toBe(0)
    expect(compareVersions('2.68.0-beta.1', '2.68.0')).toBe(-1)
  })
})

describe('whatsNewAtLaunch', () => {
  it('shows a fresh install nothing, but records the version', () => {
    expect(whatsNewAtLaunch({ lastSeen: null, current: '2.69.0', returningUser: false, entries })).toEqual({
      updated: false,
      entries: [],
      record: true,
      count: 'install'
    })
  })

  it('opens for someone coming from a version without the window, with everything up to now', () => {
    const result = whatsNewAtLaunch({ lastSeen: null, current: '2.69.0', returningUser: true, entries })
    expect(result.updated).toBe(true)
    expect(versions(result.entries)).toEqual(['2.69.0', '2.68.0'])
    expect(result.record).toBe(true)
    expect(result.count).toBe('update')
  })

  it('opens with the entries since the version last seen, newest first', () => {
    const result = whatsNewAtLaunch({ lastSeen: '2.68.0', current: '2.70.0', returningUser: true, entries })
    expect(result.updated).toBe(true)
    expect(versions(result.entries)).toEqual(['2.70.0', '2.69.0'])
  })

  it('opens after every update, a version with no entry included', () => {
    expect(whatsNewAtLaunch({ lastSeen: '2.70.0', current: '2.70.1', returningUser: true, entries })).toEqual({
      updated: true,
      entries: [],
      record: true,
      count: 'update'
    })
  })

  it('does nothing on a relaunch of the same version', () => {
    expect(whatsNewAtLaunch({ lastSeen: '2.69.0', current: '2.69.0', returningUser: true, entries })).toEqual({
      updated: false,
      entries: [],
      record: false,
      count: null
    })
  })

  it('records a downgrade without opening or counting', () => {
    expect(whatsNewAtLaunch({ lastSeen: '2.70.0', current: '2.69.0', returningUser: true, entries })).toEqual({
      updated: false,
      entries: [],
      record: true,
      count: null
    })
  })
})

describe('whatsNewOnDemand', () => {
  it("opens this version's entry, or the newest below it", () => {
    expect(versions(whatsNewOnDemand('2.69.0', entries))).toEqual(['2.69.0'])
    expect(versions(whatsNewOnDemand('2.69.4', entries))).toEqual(['2.69.0'])
  })

  it('opens the newest entry for a development build ahead of its notes', () => {
    expect(versions(whatsNewOnDemand('2.67.0', entries))).toEqual(['2.70.0'])
  })

  it('reads the entries between two versions', () => {
    expect(versions(whatsNewBetween('2.68.0', '2.69.0', entries))).toEqual(['2.69.0'])
  })
})

describe('the bundled entries', () => {
  it('are unique versions, each with something to say', () => {
    const seen = new Set<string>()
    for (const entry of WHATS_NEW) {
      expect(entry.version).toMatch(/^\d+\.\d+\.\d+$/)
      expect(seen.has(entry.version)).toBe(false)
      seen.add(entry.version)
      expect(entry.highlights.length).toBeGreaterThan(0)
    }
  })

  it('follow the house style: no em dash, links over https', () => {
    for (const copy of [...WHATS_NEW, CLOUD_ANNOUNCEMENT].map((x) => JSON.stringify(x))) {
      expect(copy).not.toContain('\u2014')
    }
    expect(CLOUD_ANNOUNCEMENT.link.url).toMatch(/^https:\/\//)
  })

  it('send Full release notes to the release on the website', () => {
    expect(RELEASE_NOTES_URL('2.68.0')).toBe('https://zennotes.org/releases#v2.68.0')
  })
})
