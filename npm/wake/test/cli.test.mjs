import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { test } from '@crab-dev/wake/test'

const cli = fileURLToPath(new URL('../bin/wake.mjs', import.meta.url))
const fixture = fileURLToPath(new URL('../../../fixtures/hello-esm/src/index.js', import.meta.url))

function run(args) {
  return spawnSync(process.execPath, [cli, ...args], {
    encoding: 'utf8',
    env: process.env,
  })
}

test('lint globals override configuration by name and reject invalid assignments', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-cli-globals-'))
  try {
    await writeFile(join(root, 'wake.config.toml'), '[lint.globals]\nconsole="readonly"')
    const result = run(['lint', '--root', root, '--print-config', 'a.js', '--global', 'console=writable', '--global', 'Promise=off', '--global', 'console=off'])
    assert.equal(result.status, 0, result.stderr)
    const globals = JSON.parse(result.stdout).config.globals
    assert.deepEqual(globals.console, { mode: 'off', source: 'request' })
    assert.equal(globals.Promise.mode, 'off')
    for (const value of ['missing', '=readonly', 'x.y=off', 'x=true', 'x=readwrite', 'x=readonly=extra']) {
      assert.equal(run(['lint', '--root', root, '--print-config', 'a.js', '--global', value]).status, 2, value)
    }
    assert.equal(run(['lint', '--list-rules', '--global', 'console=readonly']).status, 2)
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('lint environments enable browser and Node globals from the CLI', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-cli-environments-'))
  try {
    await writeFile(join(root, 'a.js'), 'window; process;\n')
    const result = run([
      'lint', '--root', root, '--format', 'json', 'a.js',
      '--rule', 'js/no-undef=error', '--env', 'browser', '--env', 'node',
    ])
    assert.equal(result.status, 0, result.stderr)
    assert.equal(JSON.parse(result.stdout).errorCount, 0)
    assert.equal(run(['lint', '--root', root, '--env', 'deno', 'a.js']).status, 2)
    assert.equal(run(['lint', '--list-rules', '--env', 'browser']).status, 2)
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('lint baseline commands generate, check, prune and reject incompatible modes', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-cli-lint-baseline-'))
  try {
    await writeFile(join(root, 'a.js'), 'debugger;\n')
    for (const mode of ['--generate-baseline', '--baseline']) {
      const result = run(['lint', '--root', root, '--format', 'json', mode, 'lint-baseline.json'])
      assert.equal(result.status, 0, result.stderr)
      assert.equal(JSON.parse(result.stdout).baseline.suppressed, 1)
    }
    await writeFile(join(root, 'a.js'), 'run();\n')
    const pruned = run(['lint', '--root', root, '--format', 'json', '--prune-baseline', 'lint-baseline.json'])
    assert.equal(pruned.status, 0, pruned.stderr)
    assert.equal(JSON.parse(pruned.stdout).baseline.stale, 1)
    for (const args of [['--baseline', 'x.json', '--generate-baseline', 'x.json'], ['--generate-baseline', 'x.json', '--fix'], ['--prune-baseline', 'x.json', 'a.js']]) {
      assert.equal(run(['lint', '--root', root, ...args]).status, 2)
    }
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('lint reports project files with consistent exit codes', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-cli-lint-'))
  try {
    await writeFile(join(root, 'input.js'), 'debugger;')
    const file = run(['lint', '--root', root])
    assert.equal(file.status, 1, file.stderr)
    assert.equal(file.stderr, '')
    assert.equal(JSON.parse(file.stdout).files[0].diagnostics[0].code, 'js/no-debugger')
    await writeFile(join(root, 'warning.js'), 'if (a == b) run();')
    const warnings = run(['lint', '--root', root, 'warning.js', '--max-warnings', '0'])
    assert.equal(warnings.status, 1, warnings.stderr)
    assert.equal(JSON.parse(warnings.stdout).warningCount, 1)
    const human = run(['--no-color', 'lint', '--root', root, '--format', 'human'])
    assert.equal(human.status, 1, human.stderr)
    assert.match(human.stdout, /js\/no-debugger/)
    assert.equal(human.stderr, '')
    assert.doesNotMatch(human.stdout, /\x1b\[/)
    for (const args of [
      ['--max-warnings', '-1'], ['--stdin'], ['--stdin-filename', 'x.js'],
      ['--format', 'other'], ['--root'], ['--unknown'],
      ['--stdin', '--stdin-filename', 'x.js', 'input.js'],
    ]) {
      const invalid = run(['lint', '--root', root, ...args])
      assert.equal(invalid.status, 2, `${args.join(' ')}\n${invalid.stderr}`)
      assert.notEqual(invalid.stderr, '')
    }
    const missing = run(['lint', '--root', root, 'missing.js'])
    assert.equal(missing.status, 2, missing.stderr)
    assert.match(missing.stderr, /WAKE_LINT_IO/)
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})

test('lint explains effective rules without requiring source files', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-cli-lint-config-'))
  try {
    await writeFile(join(root, 'wake.config.toml'), '[lint]\npresets=["react@1"]')
    const result = run(['lint', '--root', root, '--print-config', 'src/missing.tsx', '--format', 'human',
      '--rule', 'js/eqeqeq=off', '--rule', 'js/eqeqeq={"level":"warn","options":{"allow_null":true}}'])
    assert.equal(result.status, 0, result.stderr)
    const value = JSON.parse(result.stdout)
    assert.equal(value.config.rules['js/eqeqeq'].options.allow_null, true)
    assert.equal(value.config.rules['js/eqeqeq'].source, 'request')
    for (const args of [['--fix'], ['--max-warnings', '0'], ['--rule', 'js/eqeqeq=oops']]) {
      assert.equal(run(['lint', '--root', root, '--print-config', 'x.js', ...args]).status, 2)
    }
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('lint rule catalog is always JSON and rejects analysis switches', () => {
  const result = run(['lint', '--list-rules', '--format', 'human'])
  assert.equal(result.status, 0, result.stderr)
  assert.equal(JSON.parse(result.stdout).catalog.schema, 'wake.lint.rules.v1')
  for (const args of [['--fix'], ['x.js'], ['--rule', 'js/no-debugger=off'], ['--print-config', 'x.js']]) {
    assert.equal(run(['lint', '--list-rules', ...args]).status, 2)
  }
})

test('lint cache reuses cold process diagnostics and respects bypass modes', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-lint-cache-cli-'))
  try {
    await writeFile(join(root, 'a.js'), 'debugger;')
    const cold = run(['lint', '--root', root, '--cache'])
    assert.equal(cold.status, 1, cold.stderr)
    const warm = run(['lint', '--root', root, '--cache'])
    assert.equal(JSON.parse(cold.stdout).cache.writes, 1)
    assert.equal(JSON.parse(warm.stdout).cache.hits, 1)
    assert.deepEqual(JSON.parse(warm.stdout).files, JSON.parse(cold.stdout).files)
    assert.equal(run(['lint', '--cache', '--list-rules']).status, 2)
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('lint fix preview keeps files untouched and write publishes the final source', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-cli-lint-fix-'))
  try {
    await writeFile(join(root, 'wake.config.toml'), "[lint.rules]\n'style/eol-last'='error'")
    await writeFile(join(root, 'a.js'), 'run();')
    const preview = run(['lint', '--root', root, '--fix-dry-run', '--format', 'human'])
    assert.equal(preview.status, 0, preview.stderr)
    assert.match(preview.stdout, /fixed preview/)
    assert.match(preview.stdout, /run\(\);/)
    assert.equal(await readFile(join(root, 'a.js'), 'utf8'), 'run();')
    const written = run(['lint', '--root', root, '--fix'])
    assert.equal(written.status, 0, written.stderr)
    assert.equal(JSON.parse(written.stdout).files[0].written, true)
    assert.equal(await readFile(join(root, 'a.js'), 'utf8'), 'run();\n')
    assert.equal(run(['lint', '--fix', '--fix-dry-run']).status, 2)
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})

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

test('test command exposes only the Wake dashed option contract', () => {
  const help = run(['test', '--help'])
  assert.equal(help.status, 0, help.stderr)
  assert.match(help.stdout, /--name-pattern/)
  for (const value of ['--environment', 'auto', 'dom', 'browser']) {
    assert.match(help.stdout, new RegExp(value))
  }
  for (const value of ['--reporter', 'pretty', 'json', 'junit']) {
    assert.match(help.stdout, new RegExp(value))
  }
  assert.doesNotMatch(help.stdout, /testNamePattern|runInBand|--json\b|--init\b/)

  for (const args of [
    ['test', '--testNamePattern', 'renders'],
    ['test', '--runInBand'],
    ['test', '--updateSnapshot'],
    ['test', '--passWithNoTests'],
    ['test', '--watchAll'],
    ['test', '--config', 'wake.config.toml'],
    ['test', '--init'],
    ['test', '--json'],
    ['test', '--randomize'],
    ['test', '--root'],
    ['test', '--workers', '0'],
    ['test', '--serial', '--workers', '2'],
    ['test', '--changed', '--related', 'src/button.tsx'],
  ]) {
    const result = run(args)
    assert.equal(result.status, 2, `${args.join(' ')}\n${result.stderr}`)
    assert.match(result.stderr, /WAKE_TEST_CONFIG/)
  }
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

test('federation init and lock use the shared native control-plane services', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'wake-cli-federation-'))
  await writeFile(
    join(cwd, 'wake.config.toml'),
    "[federation]\nenabled = true\nname = 'shell'\n",
  )
  try {
    const first = run(['--no-color', 'federation', 'init', cwd])
    assert.equal(first.status, 0, first.stderr)
    assert.match(first.stderr, /FEDERATION INIT/)
    assert.match(first.stderr, /Initialized federation types/)
    await readFile(join(cwd, 'wake-federation.d.ts'), 'utf8')

    const second = run(['--no-color', 'federation', 'init', cwd])
    assert.equal(second.status, 0, second.stderr)
    assert.match(second.stderr, /Already initialized/)

    const lock = run(['--no-color', 'federation', 'lock', cwd])
    assert.equal(lock.status, 1, lock.stderr)
    assert.match(lock.stderr, /FED_CONFIG_INVALID/)
  } finally {
    await rm(cwd, { recursive: true, force: true })
  }
})
