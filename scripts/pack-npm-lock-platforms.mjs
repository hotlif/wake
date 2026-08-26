import { execFileSync } from 'node:child_process'
import { existsSync, mkdirSync, readFileSync } from 'node:fs'
import { resolve } from 'node:path'

const repositoryRoot = resolve(import.meta.dirname, '..')
const npmCommand = process.platform === 'win32' ? 'npm.cmd' : 'npm'
const releasePlatforms = [
  'win32-x64-msvc',
  'linux-x64-gnu',
  'linux-arm64-gnu',
  'darwin-x64',
  'darwin-arm64',
]

function option(name) {
  const index = process.argv.indexOf(`--${name}`)
  if (index === -1 || !process.argv[index + 1] || process.argv[index + 1].startsWith('--')) {
    throw new Error(`--${name} requires a value`)
  }
  return process.argv[index + 1]
}

const artifacts = resolve(option('artifacts'))
const excluded = option('exclude')
if (!releasePlatforms.includes(excluded)) {
  throw new Error(`Unknown host platform ${excluded}`)
}
mkdirSync(artifacts, { recursive: true })

for (const platform of releasePlatforms) {
  if (platform === excluded) continue
  const packageDirectory = resolve(repositoryRoot, 'npm', `wake-${platform}`)
  const manifest = JSON.parse(readFileSync(resolve(packageDirectory, 'package.json'), 'utf8'))
  const archive = resolve(artifacts, `crab-dev-wake-${platform}-${manifest.version}.tgz`)
  if (existsSync(archive)) throw new Error(`Refusing to overwrite npm archive ${archive}`)
  execFileSync(
    npmCommand,
    ['pack', packageDirectory, '--ignore-scripts', '--pack-destination', artifacts],
    {
      cwd: repositoryRoot,
      stdio: 'inherit',
      shell: process.platform === 'win32',
    },
  )
  if (!existsSync(archive)) throw new Error(`npm pack did not create ${archive}`)
}
