import { spawnSync } from 'node:child_process'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { requireWakeBinary } from './wake-binary.mjs'

const extensionRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const wakeBinary = requireWakeBinary('Crab CSS editor build')

function runWake(args) {
  const result = spawnSync(wakeBinary, args, {
    cwd: extensionRoot,
    env: process.env,
    stdio: 'inherit',
    shell: false,
  })
  if (result.error) throw result.error
  if (result.status !== 0) {
    throw new Error(`Wake bundle failed with exit code ${result.status ?? 'unknown'}`)
  }
}

const common = [
  '--platform',
  'node',
  '--format',
  'cjs',
  '--target',
  'node20',
  '--external',
  'vscode',
  '--ui',
  'plain',
]

runWake([
  'bundle',
  resolve(extensionRoot, 'src/extension.ts'),
  '--outfile',
  resolve(extensionRoot, 'dist/extension.js'),
  '--minify',
  ...common,
])

runWake([
  'bundle',
  resolve(extensionRoot, 'test/suite/index.ts'),
  '--outfile',
  resolve(extensionRoot, '.test-dist/suite/index.js'),
  ...common,
])
