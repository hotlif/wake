import { spawnSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const extensionRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const repositoryRoot = resolve(extensionRoot, '..', '..')
const releaseBinary = resolve(
  repositoryRoot,
  'target',
  'release',
  process.platform === 'win32' ? 'wake.exe' : 'wake',
)

function runWake(args) {
  const configured = process.env.WAKE_BIN
  const releaseSupportsBundle = !configured
    && existsSync(releaseBinary)
    && spawnSync(releaseBinary, ['bundle', '--help'], { stdio: 'ignore', shell: false }).status === 0
  const executable = configured || (releaseSupportsBundle ? releaseBinary : undefined)
  const command = executable || 'cargo'
  const commandArgs = executable
    ? args
    : [
        'run',
        '--quiet',
        '--release',
        '-p',
        'wake_cli',
        '--manifest-path',
        resolve(repositoryRoot, 'Cargo.toml'),
        '--',
        ...args,
      ]
  const result = spawnSync(command, commandArgs, {
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
