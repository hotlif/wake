import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
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

test('library token generates the configured TypeScript file', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'wake-cli-token-'))
  await writeFile(
    join(cwd, 'token.toml'),
    "[build]\noutput='./src/token.ts'\nprefix='demo'\n[token]\ncolor='red'\n",
  )
  const result = run(['--no-color', 'library', 'token', cwd])
  assert.equal(result.status, 0, result.stderr)
  assert.match(result.stderr, /LIBRARY TOKEN/)
  assert.match(await readFile(join(cwd, 'src/token.ts'), 'utf8'), /--demo-color/)
  await rm(cwd, { recursive: true, force: true })
})

test('library build emits ESM, CommonJS, and declarations', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'wake-cli-library-'))
  await mkdir(join(cwd, 'src'), { recursive: true })
  await writeFile(join(cwd, 'package.json'), '{"name":"@demo/button","type":"module"}')
  await writeFile(join(cwd, 'src', 'index.ts'), "import Button from './button.js';\nexport type { ButtonProps } from './button.js';\nexport default Button;\n")
  await writeFile(join(cwd, 'src', 'button.tsx'), "import type { FC } from 'react';\nexport interface ButtonProps { label: string; }\nconst Button: FC<ButtonProps> = (props) => <button>{props.label}</button>;\nexport default Button;\n")
  const result = run(['--no-color', 'library', 'build', cwd])
  assert.equal(result.status, 0, result.stderr)
  assert.match(result.stderr, /LIBRARY BUILD/)
  await readFile(join(cwd, 'esm/index.mjs'), 'utf8')
  await readFile(join(cwd, 'cjs/index.cjs'), 'utf8')
  await readFile(join(cwd, 'declarations/index.d.ts'), 'utf8')
  await rm(cwd, { recursive: true, force: true })
})

test('library docgen generates the deterministic react-docgen payload', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'wake-cli-docgen-'))
  await mkdir(join(cwd, 'src'), { recursive: true })
  await writeFile(join(cwd, 'package.json'), '{}')
  await writeFile(
    join(cwd, 'src', 'button.tsx'),
    'export default function Button(props: ButtonProps) { return null }\ninterface ButtonProps { label: string }\n',
  )
  const result = run(['--no-color', 'library', 'docgen', cwd, '--entry', 'src/button.tsx'])
  assert.equal(result.status, 0, result.stderr)
  assert.match(result.stderr, /LIBRARY DOCGEN/)
  const docgen = JSON.parse(await readFile(join(cwd, 'public/docgen.json'), 'utf8'))
  assert.equal(docgen['./src/button.tsx'][0].displayName, 'Button')
  await rm(cwd, { recursive: true, force: true })
})
