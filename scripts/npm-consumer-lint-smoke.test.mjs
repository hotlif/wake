import assert from 'node:assert/strict'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from 'node:test'

import { assertUnixTestHostMode, checkInstalledTestHost, runInstalledLintSmoke } from './npm-consumer-lint-smoke.mjs'

function makeProject() {
  const project = mkdtempSync(join(tmpdir(), 'wake-npm-lint-smoke-'))
  const packageRoot = join(project, 'node_modules', '@crab-dev', 'wake')
  mkdirSync(join(packageRoot, 'bin'), { recursive: true })
  writeFileSync(join(packageRoot, 'package.json'), JSON.stringify({
    name: '@crab-dev/wake', type: 'module',
    exports: { '.': { require: './index.cjs', import: './index.mjs' } },
  }))
  const result = JSON.stringify({
    exitCode: 1, errorCount: 1,
    files: [{ diagnostics: [{ code: 'js/no-debugger' }] }],
  })
  writeFileSync(join(packageRoot, 'index.cjs'), `exports.lint = async () => (${result});\n`)
  writeFileSync(join(packageRoot, 'index.mjs'), `export async function lint() { return (${result}); }\n`)
  writeFileSync(join(packageRoot, 'bin', 'wake.mjs'), `
if (!process.argv.includes('lint') || !process.argv.includes('--format')
  || !process.argv.includes('json') || !process.argv.includes('--rule')) process.exit(2)
console.log(${JSON.stringify(result)})
process.exitCode = 1
`)
  return { project, packageRoot, wakeCli: join(packageRoot, 'bin', 'wake.mjs') }
}

test('tarball consumer lint smoke checks CLI and both Node module formats', () => {
  const fixture = makeProject()
  try {
    runInstalledLintSmoke(fixture.project, fixture.wakeCli)
    assert.equal(readFileSync(join(fixture.project, 'lint-smoke.js'), 'utf8'), 'debugger;\n')
    writeFileSync(join(fixture.packageRoot, 'index.cjs'),
      'exports.lint = async () => ({exitCode:0,errorCount:0,files:[]});\n')
    assert.throws(() => runInstalledLintSmoke(fixture.project, fixture.wakeCli), /CommonJS|lint smoke/i)
  } finally {
    rmSync(fixture.project, { recursive: true, force: true })
  }
})

test('tarball consumer requires all Unix test host executable bits', () => {
  const fixture = makeProject()
  const host = join(fixture.project, 'node_modules', '@crab-dev', 'wake-linux-x64-gnu', 'test-host', 'wake-test-host')
  try {
    mkdirSync(join(host, '..'), { recursive: true })
    writeFileSync(host, '')
    assert.throws(() => assertUnixTestHostMode(0o100644), /executable/i)
    assert.throws(() => assertUnixTestHostMode(0o100754), /executable/i)
    assertUnixTestHostMode(0o100755)
    assert.throws(() => checkInstalledTestHost(fixture.project, '@crab-dev/wake-linux-x64-gnu', 'win32'), /wake-test-host\.exe/)
  } finally {
    rmSync(fixture.project, { recursive: true, force: true })
  }
})
