import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'

const root = resolve(import.meta.dirname, '..')
const cargo = readFileSync(resolve(root, 'Cargo.toml'), 'utf8')
const cargoVersion = cargo.match(/^version = "([^"]+)"$/m)?.[1]
if (!cargoVersion) throw new Error('Unable to read workspace Cargo version')

const publishedDirectories = [
  'npm/wake',
  'npm/wake-win32-x64-msvc',
  'npm/wake-linux-x64-gnu',
  'npm/wake-linux-arm64-gnu',
  'npm/wake-darwin-x64',
  'npm/wake-darwin-arm64',
]
const manifests = new Map()

for (const directory of ['.', ...publishedDirectories]) {
  const manifest = JSON.parse(
    readFileSync(resolve(root, directory, 'package.json'), 'utf8'),
  )
  manifests.set(manifest.name, manifest)
  if (manifest.version !== cargoVersion) {
    throw new Error(
      `${manifest.name}@${manifest.version} does not match Cargo ${cargoVersion}`,
    )
  }
}

const mainManifest = manifests.get('@crab-dev/wake')
const platformNames = [...manifests.keys()]
  .filter((name) => name.startsWith('@crab-dev/wake-'))
  .sort()
const optionalNames = Object.keys(mainManifest.optionalDependencies ?? {}).sort()
if (JSON.stringify(optionalNames) !== JSON.stringify(platformNames)) {
  throw new Error(
    `${mainManifest.name} optional platform packages must be exactly: ${platformNames.join(', ')}`,
  )
}
for (const platformName of platformNames) {
  const pinnedVersion = mainManifest.optionalDependencies[platformName]
  if (pinnedVersion !== cargoVersion) {
    throw new Error(
      `${mainManifest.name} must pin ${platformName} to ${cargoVersion}; found ${pinnedVersion ?? 'missing'}`,
    )
  }
}

if (
  process.env.GITHUB_REF_TYPE === 'tag' &&
  process.env.GITHUB_REF_NAME !== `v${cargoVersion}`
) {
  throw new Error(
    `Git tag ${process.env.GITHUB_REF_NAME} does not match v${cargoVersion}`,
  )
}

console.log(`Wake versions are aligned at ${cargoVersion}`)
