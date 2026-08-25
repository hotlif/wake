import { spawnSync } from 'node:child_process'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { requireWakeBinary } from './wake-binary.mjs'

const extensionRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const wakeBinary = requireWakeBinary('Crab CSS editor tests')
const testFiles = process.argv.slice(2)

if (testFiles.length === 0 || testFiles.some((testFile) => testFile.startsWith('-'))) {
  throw new Error('Crab CSS editor tests require one or more explicit test file paths')
}

const result = spawnSync(wakeBinary, ['test', ...testFiles, '--serial'], {
  cwd: extensionRoot,
  env: process.env,
  stdio: 'inherit',
  shell: false,
})
if (result.error) {
  throw new Error(
    `Crab CSS editor tests could not execute WAKE_BIN ${wakeBinary}: ${result.error.message}`,
    { cause: result.error },
  )
}
if (result.signal) {
  throw new Error(`Crab CSS editor tests were terminated by ${result.signal}`)
}
if (result.status === null) {
  throw new Error('Crab CSS editor tests ended without an exit status')
}
process.exitCode = result.status
