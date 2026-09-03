import assert from 'node:assert/strict'
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, join } from 'node:path'
import test from 'node:test'

import {
  materializeRustyV8ArchiveForEnvironment,
  selectRustyV8ArchivePath,
} from './rusty-v8-archive-path.mjs'

test('local Rusty V8 preparation keeps the canonical archive path', () => {
  const canonical = join('target', 'rusty-v8-prebuilt', 'librusty_v8.a.gz')

  assert.equal(selectRustyV8ArchivePath(canonical, {}), canonical)
})

test('GitHub runs and attempts receive isolated Rusty V8 archive paths', () => {
  const canonical = join('target', 'rusty-v8-prebuilt', 'librusty_v8.a.gz')
  const runnerTemp = join('runner', 'temp')
  const first = selectRustyV8ArchivePath(canonical, {
    GITHUB_RUN_ID: '33726259522',
    GITHUB_RUN_ATTEMPT: '1',
    RUNNER_TEMP: runnerTemp,
  })
  const retried = selectRustyV8ArchivePath(canonical, {
    GITHUB_RUN_ID: '33726259522',
    GITHUB_RUN_ATTEMPT: '2',
    RUNNER_TEMP: runnerTemp,
  })
  const nextRun = selectRustyV8ArchivePath(canonical, {
    GITHUB_RUN_ID: '33730000000',
    GITHUB_RUN_ATTEMPT: '1',
    RUNNER_TEMP: runnerTemp,
  })

  assert.equal(
    first,
    join(
      runnerTemp,
      'wake-rusty-v8',
      'run-33726259522-attempt-1',
      basename(canonical),
    ),
  )
  assert.notEqual(first, retried)
  assert.notEqual(first, nextRun)
})

test('CI archive materialization is content-preserving and idempotent', (context) => {
  const directory = mkdtempSync(join(tmpdir(), 'wake-rusty-v8-'))
  context.after(() => rmSync(directory, { recursive: true, force: true }))
  const canonical = join(directory, 'librusty_v8.a.gz')
  const contents = Buffer.from('pinned-rusty-v8-archive')
  writeFileSync(canonical, contents)
  const environment = {
    GITHUB_RUN_ID: '33726259522',
    GITHUB_RUN_ATTEMPT: '1',
    RUNNER_TEMP: join(directory, 'runner-temp'),
  }

  const selected = materializeRustyV8ArchiveForEnvironment(canonical, environment)
  assert.notEqual(selected, canonical)
  assert.equal(existsSync(selected), true)
  assert.deepEqual(readFileSync(selected), contents)
  assert.equal(
    materializeRustyV8ArchiveForEnvironment(canonical, environment),
    selected,
  )
})

test('incomplete or malformed GitHub run identity fails closed', () => {
  const canonical = join('target', 'rusty-v8-prebuilt', 'librusty_v8.a.gz')

  for (const environment of [
    { GITHUB_RUN_ID: '33726259522' },
    { GITHUB_RUN_ATTEMPT: '1' },
    {
      GITHUB_RUN_ID: '../escape',
      GITHUB_RUN_ATTEMPT: '1',
      RUNNER_TEMP: 'runner-temp',
    },
    {
      GITHUB_RUN_ID: '33726259522',
      GITHUB_RUN_ATTEMPT: '0',
      RUNNER_TEMP: 'runner-temp',
    },
    { GITHUB_RUN_ID: '33726259522', GITHUB_RUN_ATTEMPT: '1' },
  ]) {
    assert.throws(
      () => selectRustyV8ArchivePath(canonical, environment),
      /GitHub run identity/,
    )
  }
})
