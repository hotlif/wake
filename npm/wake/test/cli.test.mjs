import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { test } from 'node:test'

const cli = fileURLToPath(new URL('../bin/wake.mjs', import.meta.url))
const fixture = fileURLToPath(new URL('../../../fixtures/hello-esm/src/index.js', import.meta.url))

function run(args) {
  return spawnSync(process.execPath, [cli, ...args], {
    encoding: 'utf8',
    env: process.env,
  })
}

test('parse auto emits clean JSON when stdout is piped', () => {
  const result = run(['parse', fixture])
  assert.equal(result.status, 0, result.stderr)
  const value = JSON.parse(result.stdout)
  assert.ok(value.statementCount > 0)
  assert.equal(result.stderr, '')
})

test('human compiler output keeps presentation on stderr and data on stdout', () => {
  const result = run(['--no-color', 'tokenize', fixture, '--format', 'human'])
  assert.equal(result.status, 0, result.stderr)
  assert.match(result.stdout, /START\.\.END/)
  assert.match(result.stderr, /WAKE \/ TOKENIZE/)
  assert.doesNotMatch(result.stdout + result.stderr, /\x1b\[/)
})

test('forced TUI is rejected for static commands without control sequences', () => {
  const result = run(['--ui', 'tui', 'parse', fixture])
  assert.equal(result.status, 1)
  assert.match(result.stderr, /only available/)
  assert.doesNotMatch(result.stderr, /\x1b\[/)
})

test('validates the docs mode before starting a build', () => {
  const result = run(['docs', 'build', '.', '--mode', 'storybook'])
  assert.equal(result.status, 1)
  assert.match(result.stderr, /--mode must be one of: site, components/)
  assert.doesNotMatch(result.stderr, /WAKE \/ DOCS BUILD/)
})

test('bundle parser errors use the Rust CLI usage exit code', () => {
  const missingOutfile = run(['bundle', fixture, '--platform', 'node'])
  assert.equal(missingOutfile.status, 2)
  assert.match(missingOutfile.stderr, /WAKE_CONFIG/)
  assert.match(missingOutfile.stderr, /--outfile/)

  const invalidPlatform = run([
    'bundle',
    fixture,
    '--outfile',
    'ignored.js',
    '--platform',
    'server',
  ])
  assert.equal(invalidPlatform.status, 2)
  assert.match(invalidPlatform.stderr, /WAKE_CONFIG/)
  assert.match(invalidPlatform.stderr, /browser, node/)
})
