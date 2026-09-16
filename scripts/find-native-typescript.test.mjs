import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import test from 'node:test'

const script = fileURLToPath(new URL('./find-native-typescript.mjs', import.meta.url))

test('finds the runner native TypeScript compiler and can export it for GitHub Actions', () => {
  const compiler = execFileSync(process.execPath, [script, '--print'], { encoding: 'utf8' }).trim()
  assert.ok(compiler.length > 0)
  assert.match(compiler.replaceAll('\\', '/'), /@typescript\/typescript-(?:win32|linux|darwin)-[^/]+\/lib\/tsc(?:\.exe)?$/)

  const directory = mkdtempSync(join(tmpdir(), 'wake-native-typescript-test-'))
  const githubEnv = join(directory, 'github-env')
  try {
    execFileSync(process.execPath, [script, '--github-env'], {
      encoding: 'utf8',
      env: { ...process.env, GITHUB_ENV: githubEnv },
    })
    assert.equal(readFileSync(githubEnv, 'utf8'), `WAKE_LINT_TYPESCRIPT_EXE=${compiler}\n`)
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
})

test('rejects GitHub environment export without GITHUB_ENV', () => {
  assert.throws(
    () => execFileSync(process.execPath, [script, '--github-env'], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'pipe'],
      env: { ...process.env, GITHUB_ENV: undefined },
    }),
    /--github-env requires GITHUB_ENV/,
  )
})
