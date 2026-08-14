import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'
import { validateAdrs, validatePolicy } from './check-architecture.mjs'

const decision = 'engineering/decisions/0001-architecture-evolution-loop.md'
const activeAdrRecords = new Map([
  ['0001-architecture-evolution-loop.md', { status: 'accepted' }],
  ['0003-compiler-and-shell-boundaries.md', { status: 'proposed' }],
])

function policy(overrides = {}) {
  return {
    schemaVersion: 1,
    decision,
    crates: ['wake_common', 'wake_ecma_parser', 'wake_app'],
    groups: { compiler: ['wake_common', 'wake_ecma_parser'] },
    rules: [{
      id: 'compiler-no-app',
      description: 'compiler cannot depend on app',
      fromGroups: ['compiler'],
      deny: ['wake_app'],
      decision,
      suggestion: 'invert the dependency',
    }],
    ...overrides,
  }
}

test('rejects a forbidden compiler to app dependency', () => {
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set(['wake_common', 'wake_app'])],
    ['wake_app', new Set()],
  ])
  const errors = validatePolicy({ policy: policy(), packages, adrRecords: activeAdrRecords })
  assert(errors.some((error) => error.includes('[compiler-no-app] wake_ecma_parser -> wake_app')))
})

test('expands allow-only groups and rejects dependencies outside the declared layer', () => {
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set(['wake_common'])],
    ['wake_app', new Set(['wake_ecma_parser'])],
  ])
  const layered = policy({
    groups: { compiler: ['wake_common', 'wake_ecma_parser'] },
    rules: [{
      id: 'app-only-compiler',
      description: 'app test boundary',
      from: ['wake_app'],
      allowOnlyGroups: ['compiler'],
      decision,
      suggestion: 'use the compiler layer',
    }],
  })
  assert.deepEqual(validatePolicy({ policy: layered, packages, adrRecords: activeAdrRecords }), [])

  packages.get('wake_app').add('wake_app')
  const errors = validatePolicy({ policy: layered, packages, adrRecords: activeAdrRecords })
  assert(errors.some((error) => error.includes('[app-only-compiler] wake_app -> wake_app')))
})

test('repository policy rejects foundation, parser, and shell boundary regressions', () => {
  const repositoryPolicy = JSON.parse(readFileSync(new URL('../engineering/architecture-boundaries.json', import.meta.url), 'utf8'))
  const packages = new Map(repositoryPolicy.crates.map((name) => [name, new Set()]))
  packages.get('wake_common').add('wake_css')
  packages.get('wake_ecma_parser').add('wake_ecma_semantic')
  packages.get('wake_cli').add('wake_bundler')

  const errors = validatePolicy({
    policy: repositoryPolicy,
    packages,
    adrRecords: activeAdrRecords,
  })
  assert(errors.some((error) => error.includes('[common-is-workspace-foundation] wake_common -> wake_css')))
  assert(errors.some((error) => error.includes('[parser-does-not-own-semantic] wake_ecma_parser -> wake_ecma_semantic')))
  assert(errors.some((error) => error.includes('[shells-use-app-or-compiler] wake_cli -> wake_bundler')))
})

test('rejects an unregistered workspace crate', () => {
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set(['wake_common'])],
    ['wake_app', new Set()],
    ['wake_new', new Set()],
  ])
  const errors = validatePolicy({ policy: policy(), packages, adrRecords: activeAdrRecords })
  assert(errors.some((error) => error.includes('workspace crate wake_new is not registered')))
})

test('rejects boundary decisions that are not active', () => {
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set(['wake_common'])],
    ['wake_app', new Set()],
  ])
  const rejected = new Map([['0001-architecture-evolution-loop.md', { status: 'rejected' }]])
  const errors = validatePolicy({ policy: policy(), packages, adrRecords: rejected })
  assert(errors.some((error) => error.includes('must be proposed or accepted')))
})

test('rejects a boundary policy without an ADR', () => {
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set(['wake_common'])],
    ['wake_app', new Set()],
  ])
  const withoutDecision = policy({ decision: undefined, rules: [] })
  const errors = validatePolicy({ policy: withoutDecision, packages, adrRecords: activeAdrRecords })
  assert(errors.some((error) => error.includes('decision must reference an ADR')))
})

test('rejects invalid ADR status and a missing supersedes target', () => {
  const root = join(tmpdir(), `wake-architecture-${Date.now()}-${Math.random().toString(16).slice(2)}`)
  const decisionsDir = join(root, 'engineering', 'decisions')
  mkdirSync(decisionsDir, { recursive: true })
  writeFileSync(join(decisionsDir, '0001-first.md'), `# ADR 0001: First\n\n- Status: invalid\n\n${sections('None.')}`)
  writeFileSync(join(decisionsDir, '0002-second.md'), `# ADR 0002: Second\n\n- Status: proposed\n\n${sections('[ADR 0099](0099-missing.md)')}`)
  try {
    const result = validateAdrs({ repoRoot: root, decisionsDir })
    assert(result.errors.some((error) => error.includes('status must be proposed')))
    assert(result.errors.some((error) => error.includes('Supersedes target 0099-missing.md does not exist')))
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('rejects duplicate ADR numbers', () => {
  const root = join(tmpdir(), `wake-architecture-${Date.now()}-${Math.random().toString(16).slice(2)}`)
  const decisionsDir = join(root, 'engineering', 'decisions')
  mkdirSync(decisionsDir, { recursive: true })
  const body = `- Status: proposed\n\n${sections('None.')}`
  writeFileSync(join(decisionsDir, '0001-first.md'), `# ADR 0001: First\n\n${body}`)
  writeFileSync(join(decisionsDir, '0001-second.md'), `# ADR 0001: Second\n\n${body}`)
  try {
    const result = validateAdrs({ repoRoot: root, decisionsDir })
    assert(result.errors.some((error) => error.includes('ADR number 0001 duplicates')))
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

function sections(supersedes) {
  return [
    '## Context\n\nContext.',
    '## Decision\n\nDecision.',
    '## Invariants\n\nInvariant.',
    '## Evidence\n\nEvidence.',
    '## Consequences\n\nConsequences.',
    '## Validation\n\nValidation.',
    `## Supersedes\n\n${supersedes}`,
    '## Removal plan\n\nNone.',
  ].join('\n\n')
}
