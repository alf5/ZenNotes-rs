import { describe, expect, it } from 'vitest'
import { NODE_DEFS, bindParams, nodeDef, sanitizeTagChars } from './nodes'
import type { NodeDef } from './nodes'
import { parseWorkflow } from './parse'
import { RAW_ARG } from './types'

/* -------------------------------------------------------------------------- */
/*  The documentation contract                                                */
/* -------------------------------------------------------------------------- */

// `description` and `example` are required by the type, so these tests are not
// there to catch a missing field in a checked build. They are there for the two
// ways one can still ship undocumented: a field present but empty, and an
// example that reads well but does not parse. Both would reach the completion
// popup, which is the only place most people will ever read this language.

/**
 * Wrap one example in the smallest file that could carry it.
 *
 * A source example is a whole statement (`books = tag #book`), because that is
 * how a pipeline opens and how the examples are written. Everything else is a
 * bare step, so it needs a wire in front of it: `notes = all` is the shortest
 * legal one and drags nothing else into the parse.
 */
function fileFor(def: NodeDef): string {
  const body = def.source ? def.example : `notes = all\nnotes | ${def.example}`
  return `---\nname: Example\n---\n\n${body}\n`
}

describe('every node in the registry is documented', () => {
  it.each(NODE_DEFS.map((def) => [def.kind, def] as const))(
    '`%s` says what it does and shows a line',
    (_kind, def) => {
      expect(def.description.trim()).not.toBe('')
      expect(def.example.trim()).not.toBe('')
    }
  )

  it('describes the step rather than repeating a parameter name', () => {
    for (const def of NODE_DEFS) {
      const words = def.description.trim().split(/\s+/)
      // A one-word "answer" is always the parameter name echoed back ("folder",
      // "tag"), which is the exact failure this documentation replaced.
      expect(words.length).toBeGreaterThan(4)
      for (const spec of def.params) expect(def.description.trim()).not.toBe(spec.name)
    }
  })

  it('writes a sentence, so the popup can end it where the author did', () => {
    for (const def of NODE_DEFS) {
      expect(def.description.trim().endsWith('.')).toBe(true)
      // House style: the long dash is never written anywhere in this project.
      // Escaped rather than typed, so this file cannot be the exception.
      expect(def.description).not.toContain('\u2014')
      expect(def.example).not.toContain('\u2014')
    }
  })

  it('names the verb in its own example', () => {
    for (const def of NODE_DEFS) {
      expect(def.example.split(/\s+/)).toContain(def.kind)
    }
  })
})

describe('every example is a line that actually parses', () => {
  it.each(NODE_DEFS.map((def) => [def.kind, def] as const))(
    '`%s` example parses with no errors',
    (_kind, def) => {
      const { workflow, diagnostics } = parseWorkflow(fileFor(def), 'example')
      expect(diagnostics.filter((d) => d.severity === 'error')).toEqual([])
      // Parsing "cleanly" is not enough on its own: an unknown verb still lands
      // as a step. The example has to have produced a step of THIS kind.
      const kinds = workflow.statements.flatMap((statement) =>
        statement.steps.map((step) => step.kind)
      )
      expect(kinds).toContain(def.kind)
    }
  )

  it('binds every required parameter, so no example is a half-written line', () => {
    for (const def of NODE_DEFS) {
      const { workflow } = parseWorkflow(fileFor(def), 'example')
      const step = workflow.statements
        .flatMap((statement) => statement.steps)
        .find((candidate) => candidate.kind === def.kind)
      expect(step).toBeDefined()
      for (const spec of def.params) {
        if (!spec.required) continue
        expect(step && Object.prototype.hasOwnProperty.call(step.args, spec.name)).toBe(true)
      }
    }
  })
})

describe('bindParams parks what it cannot bind', () => {
  const def = (kind: string): NodeDef => {
    const found = nodeDef(kind)
    if (!found) throw new Error(`no node def for ${kind}`)
    return found
  }

  it('stops at the failed token instead of letting later tokens slide left', () => {
    const bound = bindParams(def('where'), ['rating', '==', '4'], 1)
    expect(bound.args.field).toBe('rating')
    expect(bound.args.op).toBeUndefined()
    expect(bound.args.value).toBeUndefined()
    expect(bound.args[RAW_ARG]).toBe('== 4')
    expect(bound.diagnostics.some((d) => d.severity === 'error')).toBe(true)
  })

  it('keeps a failed quoted token with its quotes', () => {
    const bound = bindParams(def('since'), ['"7 x"'], 1)
    expect(bound.args[RAW_ARG]).toBe('"7 x"')
  })

  it('accepts the unicode tags the vault indexes and refuses digit-leading ones', () => {
    expect(bindParams(def('tag'), ['#café'], 1).args.tag).toBe('café')
    expect(bindParams(def('tag'), ['#日記'], 1).args.tag).toBe('日記')
    const digits = bindParams(def('tag'), ['#2024'], 1)
    expect(digits.args.tag).toBeUndefined()
    expect(digits.args[RAW_ARG]).toBe('#2024')
    expect(digits.diagnostics.some((d) => d.severity === 'error')).toBe(true)
  })
})

describe('tag lists bind as one comma-separated argument', () => {
  const def = (kind: string): NodeDef => {
    const found = nodeDef(kind)
    if (!found) throw new Error(`no node def for ${kind}`)
    return found
  }

  it('binds one tag exactly as before lists existed', () => {
    expect(bindParams(def('tag'), ['#book'], 1)).toEqual({ args: { tag: 'book' }, diagnostics: [] })
  })

  it('stores a list comma-joined without the hashes, spaces or not', () => {
    expect(bindParams(def('tagged'), ['#book,', '#article'], 1)).toEqual({
      args: { tag: 'book,article' },
      diagnostics: []
    })
    expect(bindParams(def('not-tagged'), ['#a,#b,', 'c'], 1).args.tag).toBe('a,b,c')
  })

  it('drops a repeated tag whatever its case, as tags match', () => {
    expect(bindParams(def('tagged'), ['#Book,', '#book,', '#art'], 1).args.tag).toBe('Book,art')
  })

  it('refuses tags separated only by spaces and shows the comma form', () => {
    const bound = bindParams(def('not-tagged'), ['#a', '#b'], 1)
    expect(bound.args.tag).toBeUndefined()
    expect(bound.args[RAW_ARG]).toBe('#a #b')
    expect(bound.diagnostics).toEqual([
      { severity: 'error', message: 'separate tags with commas: `not-tagged #a, #b`', line: 1 }
    ])
  })

  it('refuses an empty entry rather than guessing what was meant', () => {
    for (const tokens of [['#a,'], ['#a,,', '#b'], [',#a']]) {
      const bound = bindParams(def('tagged'), tokens, 1)
      expect(bound.args.tag).toBeUndefined()
      expect(bound.args[RAW_ARG]).toBe(tokens.join(' '))
      expect(bound.diagnostics[0]?.message).toBe('`tagged` has an empty entry in its tag list')
    }
  })

  it('names the entry that is not a tag', () => {
    const bound = bindParams(def('tagged'), ['#book,', '#2024'], 1)
    expect(bound.args[RAW_ARG]).toBe('#book, #2024')
    expect(bound.diagnostics[0]?.message).toBe('`#2024` is not a valid tag')
  })

  it('still needs at least one tag', () => {
    expect(bindParams(def('tagged'), [], 1).diagnostics).toEqual([
      { severity: 'error', message: '`tagged` needs tag', line: 1 }
    ])
  })

  it('`no-tags` takes no argument', () => {
    expect(bindParams(def('no-tags'), [], 1)).toEqual({ args: {}, diagnostics: [] })
  })
})

describe('sanitizeTagChars — what survives typing a tag (#532)', () => {
  // The canvas inspector runs every keystroke through this. It has to keep
  // exactly what `bindParams` would accept, in any script: an ASCII-only
  // alphabet here deletes Cyrillic as fast as it is typed while the file
  // format, the vault's own tag index, and the binder all take it happily.
  it('keeps letters from any script', () => {
    expect(sanitizeTagChars('кириллица-работает')).toBe('кириллица-работает')
    expect(sanitizeTagChars('café')).toBe('café')
    expect(sanitizeTagChars('ελληνικά')).toBe('ελληνικά')
    expect(sanitizeTagChars('日本語')).toBe('日本語')
  })

  it('keeps the punctuation a tag may contain, and drops the rest', () => {
    expect(sanitizeTagChars('project/sub_task-2024')).toBe('project/sub_task-2024')
    expect(sanitizeTagChars('two words')).toBe('twowords')
    expect(sanitizeTagChars('bad!chars?here')).toBe('badcharshere')
  })

  it('strips the leading # the stored argument never carries', () => {
    expect(sanitizeTagChars('#кириллица')).toBe('кириллица')
    expect(sanitizeTagChars('##double')).toBe('double')
  })

  it('agrees with the binder: whatever it produces, bindParams accepts', () => {
    const def = nodeDef('tagged')
    expect(def).not.toBeNull()
    for (const typed of ['#кириллица-работает', 'café', '日本語', 'project/sub']) {
      const clean = sanitizeTagChars(typed)
      const bound = bindParams(def!, [`#${clean}`], 1)
      expect(bound.diagnostics).toEqual([])
    }
  })
})
