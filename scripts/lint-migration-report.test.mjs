import assert from 'node:assert/strict'
import { mkdtemp, readFile, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { execFile } from 'node:child_process'
import { promisify } from 'node:util'
import test from 'node:test'

const run = promisify(execFile)

test('migration report maps native rules and retains unsupported rules', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-lint-migration-'))
  const config = join(root, '.eslintrc.json')
  await writeFile(config, JSON.stringify({
    extends: ['eslint:recommended', 'plugin:unknown/recommended'],
    parserOptions: { ecmaVersion: 2024 },
    rules: {
      'no-debugger': 'error',
      'react/jsx-key': 'warn',
      'react/no-danger': 'error',
      '@typescript-eslint/no-explicit-any': 'error',
      'import/no-unresolved': 'warn',
      quotes: 'warn',
      'plugin/no-debugger': 'warn',
      'no-unknown-rule': 'warn',
    },
  }))
  const { stdout } = await run(process.execPath, ['scripts/lint-migration-report.mjs', config], { cwd: join(import.meta.dirname, '..') })
  const report = JSON.parse(stdout)
  assert.equal(report.schema, 'wake.lint.migration.v1')
  assert.equal(report.migrated['js/no-debugger'], 'error')
  assert.equal(report.migrated['react/jsx-key'], 'warn')
  assert.equal(report.migrated['react/no-danger'], 'error')
  assert.equal(report.migrated['ts/no-explicit-any'], 'error')
  assert.equal(report.migrated['import/no-unresolved'], 'warn')
  assert.equal(report.migrated['style/quotes'], 'warn')
  assert.deepEqual(report.unsupported, ['no-unknown-rule', 'plugin/no-debugger'])
  assert.deepEqual(report.presets, [
    { source: 'eslint:recommended', native: 'recommended' },
    { source: 'plugin:unknown/recommended', native: null },
  ])
  assert.deepEqual(report.unsupportedConfig, ['parserOptions'])
  assert.match(await readFile(config, 'utf8'), /no-debugger/)
})

test('migration report covers every registered native rule', async () => {
  const root = join(import.meta.dirname, '..')
  const source = await readFile(join(root, 'crates/wake_lint_core/src/lib.rs'), 'utf8')
  const ids = [...source.matchAll(/^\s+id: "([^"]+)"/gm)].map((match) => match[1]).sort()
  const fixture = await mkdtemp(join(tmpdir(), 'wake-lint-migration-catalog-'))
  const config = join(fixture, '.eslintrc.json')
  await writeFile(config, JSON.stringify({ rules: Object.fromEntries(ids.map((id) => [id, 'error'])) }))
  const { stdout } = await run(process.execPath, ['scripts/lint-migration-report.mjs', config], { cwd: root })
  const report = JSON.parse(stdout)
  assert.deepEqual(Object.keys(report.migrated).sort(), ids)
  assert.deepEqual(report.unsupported, [])
})
