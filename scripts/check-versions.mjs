import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'

const root = resolve(import.meta.dirname, '..')
const cargo = readFileSync(resolve(root, 'Cargo.toml'), 'utf8')
const cargoLock = readFileSync(resolve(root, 'Cargo.lock'), 'utf8')
const cargoVersion = cargo.match(/^version = "([^"]+)"$/m)?.[1]
if (!cargoVersion) throw new Error('Unable to read workspace Cargo version')

function cargoPackage(name) {
  const blocks = cargoLock.split(/\r?\n(?=\[\[package\]\])/)
  const block = blocks.find((candidate) => (
    candidate.match(/^name = "([^"]+)"$/m)?.[1] === name
  ))
  if (!block) throw new Error(`Cargo.lock is missing ${name}`)
  return {
    version: block.match(/^version = "([^"]+)"$/m)?.[1],
    source: block.match(/^source = "([^"]+)"$/m)?.[1],
    checksum: block.match(/^checksum = "([0-9a-f]+)"$/m)?.[1],
  }
}

const publishedDirectories = [
  'npm/css',
  'npm/wake',
  'npm/wake-win32-x64-msvc',
  'npm/wake-linux-x64-gnu',
  'npm/wake-linux-arm64-gnu',
  'npm/wake-darwin-x64',
  'npm/wake-darwin-arm64',
]
const manifests = new Map()
const dependencyFields = [
  'dependencies',
  'devDependencies',
  'peerDependencies',
  'optionalDependencies',
]
const retiredCssPackages = ['@linaria/core', '@wyw-in-js/core', '@wake/css']

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
  for (const field of dependencyFields) {
    for (const retired of retiredCssPackages) {
      if (Object.hasOwn(manifest[field] ?? {}, retired)) {
        throw new Error(
          `${manifest.name} must use @crab-dev/css exclusively; remove ${field}.${retired}`,
        )
      }
    }
  }
}

const lock = JSON.parse(readFileSync(resolve(root, 'package-lock.json'), 'utf8'))
for (const packagePath of Object.keys(lock.packages ?? {})) {
  if (
    packagePath.includes('node_modules/@linaria/') ||
    packagePath.includes('node_modules/@wyw-in-js/')
  ) {
    throw new Error(
      `package-lock.json contains retired CSS implementation ${packagePath}`,
    )
  }
}

const testRuntimeSources = JSON.parse(
  readFileSync(resolve(root, 'engineering/test-runtime-sources.json'), 'utf8'),
)
if (testRuntimeSources.schemaVersion !== 2 || testRuntimeSources.contract !== 'ADR-0020') {
  throw new Error('engineering/test-runtime-sources.json must use schema v2 and ADR-0020')
}
const requiredRustCrates = new Set(['deno_core', 'deno_v8', 'v8'])
for (const source of testRuntimeSources.rustCrates ?? []) {
  if (!requiredRustCrates.delete(source.name)) {
    throw new Error(`Unexpected or duplicate test runtime crate ${source.name}`)
  }
  if (
    source.registry !== 'https://crates.io' ||
    !/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(source.version) ||
    !/^[0-9a-f]{64}$/.test(source.checksum) ||
    source.license !== 'MIT'
  ) {
    throw new Error(`${source.name} must pin a crates.io version, checksum, and MIT license`)
  }
  const locked = cargoPackage(source.name)
  if (
    locked.version !== source.version ||
    locked.source !== 'registry+https://github.com/rust-lang/crates.io-index' ||
    locked.checksum !== source.checksum
  ) {
    throw new Error(`${source.name} Cargo.lock registry provenance does not match its manifest`)
  }
}
if (requiredRustCrates.size) {
  throw new Error(`Missing test runtime crates: ${[...requiredRustCrates].join(', ')}`)
}
if (!/^deno_core\s*=\s*\{[^\n]*version\s*=\s*"=0\.410\.0"[^\n]*\}/m.test(cargo)) {
  throw new Error('deno_core must be an exact crates.io workspace dependency at 0.410.0')
}
if (/^deno_core\s*=\s*\{[^\n]*(?:path|git)\s*=/m.test(cargo) || /^\[patch\.crates-io\]/m.test(cargo)) {
  throw new Error('test runtime crates cannot use path/git dependencies or crates.io patches')
}
const requiredTestSources = new Set([
  'happy-dom',
  'entities',
  'whatwg-mimetype',
  'buffer-image-size',
  'ws',
  'react',
  'react-dom',
  '@testing-library/react',
  '@testing-library/dom',
  '@testing-library/user-event',
  '@testing-library/jest-dom',
])
const embeddedDomPackages = new Set([
  'happy-dom',
  'entities',
  'whatwg-mimetype',
  'buffer-image-size',
])
for (const source of testRuntimeSources.sources ?? []) {
  if (!requiredTestSources.delete(source.name)) {
    throw new Error(`Unexpected or duplicate test runtime source ${source.name}`)
  }
  if (!/^sha512-[A-Za-z0-9+/]+=*$/.test(source.integrity)) {
    throw new Error(`${source.name} must pin an npm sha512 integrity`)
  }
  if (!new Set(['MIT', 'BSD-2-Clause']).has(source.license)) {
    throw new Error(`${source.name} must declare an audited SPDX license`)
  }
  if (embeddedDomPackages.has(source.name) !== (source.embedded === true)) {
    throw new Error(`${source.name} embedded DOM provenance does not match the build contract`)
  }
  if (source.gitHead !== undefined && !/^[0-9a-f]{40}$/.test(source.gitHead)) {
    throw new Error(`${source.name} gitHead must be a 40-character commit when present`)
  }
  const locked = lock.packages?.[`node_modules/${source.name}`]
  if (
    locked?.version !== source.version ||
    locked?.integrity !== source.integrity ||
    locked?.resolved !== source.tarball
  ) {
    throw new Error(`${source.name} lockfile version/integrity/tarball does not match provenance`)
  }
  if (source.name === 'happy-dom') {
    const adapters = source.wakeAdapters ?? []
    const outputReset = adapters.find(({ id }) => id === 'html-output-default-value-reset')
    if (
      adapters.length !== 1 ||
      outputReset?.standard !==
        'https://html.spec.whatwg.org/multipage/form-elements.html#the-output-element' ||
      outputReset?.owner !== 'Wake' ||
      outputReset?.root !== 'crates/wake_js_runtime/runtime/happy-dom'
    ) {
      throw new Error('Happy DOM compatibility must remain a Wake-owned runtime adapter')
    }
  }
  const requested =
    manifests.get('wake-workspace').dependencies?.[source.name] ??
    manifests.get('wake-workspace').devDependencies?.[source.name]
  const exactPackages = new Set([
    'happy-dom',
    'react',
    'react-dom',
    '@testing-library/react',
    '@testing-library/dom',
    '@testing-library/user-event',
    '@testing-library/jest-dom',
  ])
  if (exactPackages.has(source.name) && requested !== source.version) {
    throw new Error(`${source.name} must be exactly pinned to ${source.version}; found ${requested ?? 'missing'}`)
  }
}
if (requiredTestSources.size) {
  throw new Error(`Missing test runtime sources: ${[...requiredTestSources].join(', ')}`)
}

const test262 = JSON.parse(
  readFileSync(resolve(root, 'engineering/test262-es2024.json'), 'utf8'),
)
if (
  test262.schemaVersion !== 1 ||
  test262.contract !== 'ADR-0020' ||
  test262.suite !== 'test262' ||
  test262.target !== 'ES2024' ||
  test262.license !== 'BSD-3-Clause' ||
  !/^[0-9a-f]{40}$/.test(test262.commit) ||
  !/^[0-9a-f]{64}$/.test(test262.sha256) ||
  !test262.archiveUrl.includes(test262.commit) ||
  !Number.isSafeInteger(test262.expectedFiles) ||
  !Number.isSafeInteger(test262.expectedVariants) ||
  test262.expectedFiles <= 0 ||
  test262.expectedVariants < test262.expectedFiles ||
  !Array.isArray(test262.selectedRoots) ||
  test262.selectedRoots.length === 0 ||
  new Set(test262.selectedRoots).size !== test262.selectedRoots.length ||
  !Array.isArray(test262.excludedTests)
) {
  throw new Error('engineering/test262-es2024.json is not an immutable ADR-0020 manifest')
}
if (
  new Set(test262.excludedTests).size !== test262.excludedTests.length ||
  Object.keys(test262.exclusionReasons ?? {}).length !== test262.excludedTests.length ||
  test262.excludedTests.some((test) =>
    typeof test262.exclusionReasons[test] !== 'string' ||
    test262.exclusionReasons[test].trim() === ''
  )
) {
  throw new Error('every Test262 exclusion must be unique and carry an explicit reason')
}

const mainManifest = manifests.get('@crab-dev/wake')
const cssManifest = manifests.get('@crab-dev/css')
const workspaceManifest = manifests.get('wake-workspace')
for (const manifest of [workspaceManifest, mainManifest]) {
  const cssVersion = manifest.dependencies?.[cssManifest.name]
  if (cssVersion !== cargoVersion) {
    throw new Error(
      `${manifest.name} must pin ${cssManifest.name} to ${cargoVersion}; found ${cssVersion ?? 'missing'}`,
    )
  }
}
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

console.log(`Published package versions are aligned at ${cargoVersion}`)
