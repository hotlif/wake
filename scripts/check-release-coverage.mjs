import { existsSync, readFileSync, readdirSync } from 'node:fs'
import { resolve } from 'node:path'

import {
  PLATFORM_CONTRACTS,
  expectedPlatformFiles,
} from './native-package-contract.mjs'
import { readSystemBrowserConformanceManifest } from './check-system-browser-conformance.mjs'

const root = resolve(import.meta.dirname, '..')
const npmRoot = resolve(root, 'npm')
const workflowPath = resolve(root, '.github/workflows/release-npm.yml')
const workflow = readFileSync(workflowPath, 'utf8')
readSystemBrowserConformanceManifest()
const vscodeWorkflow = readFileSync(
  resolve(root, '.github/workflows/vscode-css.yml'),
  'utf8',
)

const releaseJobs = [...workflow.matchAll(/^  ([a-zA-Z0-9_-]+):\r?$/gm)]
function releaseJob(name) {
  const matches = releaseJobs.filter((match) => match[1] === name)
  if (matches.length !== 1) {
    throw new Error(`release-npm.yml must define exactly one ${name} job; found ${matches.length}`)
  }
  const match = matches[0]
  const index = releaseJobs.indexOf(match)
  const end = releaseJobs[index + 1]?.index ?? workflow.length
  return { index: match.index, source: workflow.slice(match.index, end) }
}

function requireJobMarkers(name, source, markers) {
  for (const marker of markers) {
    if (!source.includes(marker)) {
      throw new Error(`release-npm.yml ${name} job is missing contract marker ${marker}`)
    }
  }
}

function requireOrderedJobMarkers(name, source, markers) {
  let previous = -1
  for (const marker of markers) {
    const index = source.indexOf(marker)
    if (index <= previous) {
      throw new Error(`release-npm.yml ${name} job must order ${marker} after its preceding gate`)
    }
    previous = index
  }
}

const packages = readdirSync(npmRoot, { withFileTypes: true })
  .filter((entry) => entry.isDirectory())
  .map((entry) => {
    const manifestPath = resolve(npmRoot, entry.name, 'package.json')
    if (!existsSync(manifestPath)) return undefined
    const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'))
    return { directory: `npm/${entry.name}`, directoryName: entry.name, manifest }
  })
  .filter((entry) => entry && entry.manifest.private !== true)
  .sort((left, right) => left.manifest.name.localeCompare(right.manifest.name))

if (packages.length === 0) {
  throw new Error('No publishable npm packages were discovered under npm/*')
}
for (const required of [
  "- 'v*'",
  "- '!vscode-css-v*'",
  "if: github.ref_type == 'tag' && github.ref_name == format('v{0}', needs.verify.outputs.version)",
  'node scripts/stage-test-host.mjs',
  'node scripts/prepare-rusty-v8.mjs',
  'package/test-host/wake-test-host',
  'package/native-manifest.json',
  'package/sbom.spdx.json',
  'package/THIRD_PARTY_LICENSES.txt',
  'node scripts/verify-native-package.mjs',
  'npm run browser:conformance:test',
  'check-system-browser-conformance.mjs',
  '67108864',
  '58720256',
  '201326592',
  'React browser and screenshot smoke',
  'visual.browser.test.mjs',
  "toMatchScreenshot('published-ready')",
  'npm publish',
]) {
  if (!workflow.includes(required)) {
    throw new Error(`release-npm.yml is missing release contract marker ${required}`)
  }
}

const verifyJob = releaseJob('verify')
const buildNativeJob = releaseJob('build-native')
const buildLinuxJob = releaseJob('build-linux')
const auditTarballsJob = releaseJob('audit-tarballs')
const prepublishSmokeJob = releaseJob('prepublish-smoke')
const publishJob = releaseJob('publish')
const registrySmokeJob = releaseJob('smoke')
if (!(auditTarballsJob.index < prepublishSmokeJob.index
  && prepublishSmokeJob.index < publishJob.index)) {
  throw new Error('local tarball smoke must run after audit-tarballs and before publish')
}
requireJobMarkers('verify', verifyJob.source, [
  'cargo fetch --locked --target x86_64-unknown-linux-gnu',
  'node scripts/prepare-rusty-v8.mjs --target x86_64-unknown-linux-gnu',
  'cargo test --workspace --locked --offline',
  'cargo clippy --workspace --all-targets --locked --offline -- -D warnings',
  'cargo build -p wake_test_host -p wake_cli --locked --offline',
  './target/debug/wake test npm/css/test/runtime.test.mjs --serial',
  'node --test npm/css/test/realm.node.mjs',
])
if (verifyJob.source.includes('npm run npm:test:css')) {
  throw new Error('release verify must use the freshly built Wake CLI and host for CSS tests')
}
for (const [name, job] of [
  ['build-native', buildNativeJob],
  ['build-linux', buildLinuxJob],
]) {
  requireOrderedJobMarkers(name, job.source, [
    'cargo fetch --locked --target ${{ matrix.target }}',
    'node scripts/prepare-rusty-v8.mjs --target ${{ matrix.target }}',
    'cargo build -p wake_test_host --release --locked --offline --target ${{ matrix.target }}',
    'npx --no-install napi build',
    '-- --locked --offline',
    'git diff --exit-code -- Cargo.lock',
    'node scripts/stage-test-host.mjs',
  ])
}
requireJobMarkers('prepublish-smoke', prepublishSmokeJob.source, [
  'needs: [verify, audit-tarballs]',
  'platform: [win32-x64-msvc, linux-x64-gnu, linux-arm64-gnu, darwin-x64, darwin-arm64]',
  "node: ['22.14.0', '24', '26']",
  'runner: windows-latest',
  'runner: ubuntu-24.04',
  'runner: ubuntu-24.04-arm',
  'runner: macos-15-intel',
  'runner: macos-15',
  'pattern: npm-*',
  'actions/checkout@v4',
  'node-version: ${{ matrix.node }}',
  "Clean install this build's local tarballs",
  '--ignore-scripts',
  '--omit=optional',
  'PLATFORM_ARCHIVE',
  "requested.startsWith('file:')",
  'Wake Test CLI, runTests and TestContext smoke',
  'npx --no-install wake test smoke.test.mjs --serial',
  'runTests(options)',
  'createTestContext(options)',
  'context.startWatch()',
  'context.stopWatch()',
  'await context.close()',
  'Select a system Chrome, Edge or Chromium',
  "if: matrix.node == '24'",
  'ubuntu-24.04-arm',
  'WAKE_RELEASE_BROWSER_PATH',
  'A compatible system Chrome, Edge or Chromium is required',
  'React browser and screenshot smoke from local tarballs',
  "toMatchScreenshot('local-ready')",
  '--browser-path "$WAKE_RELEASE_BROWSER_PATH"',
  '--reporter json',
  '--output browser-result.json',
  '--target "${{ matrix.platform }}"',
  '--result browser-result.json',
])
if (prepublishSmokeJob.source.includes('continue-on-error')) {
  throw new Error('prepublish local tarball smoke must never continue on error')
}
if (prepublishSmokeJob.source.includes('npm publish')) {
  throw new Error('prepublish local tarball smoke must not mutate the npm registry')
}
const node24GateCount = prepublishSmokeJob.source.split("if: matrix.node == '24'").length - 1
if (node24GateCount !== 2) {
  throw new Error(`prepublish smoke must have exactly two Node 24 browser gates; found ${node24GateCount}`)
}
const prepublishConditions = [...prepublishSmokeJob.source.matchAll(/^\s+if:\s*(.+)\r?$/gm)]
  .map((match) => match[1])
if (prepublishConditions.length !== 2
  || prepublishConditions.some((condition) => condition !== "matrix.node == '24'")) {
  throw new Error(
    `prepublish smoke must gate only the two Node 24 browser steps, found ${prepublishConditions.join(', ')}`,
  )
}
for (const [platform, runner] of [
  ['win32-x64-msvc', 'windows-latest'],
  ['linux-x64-gnu', 'ubuntu-24.04'],
  ['linux-arm64-gnu', 'ubuntu-24.04-arm'],
  ['darwin-x64', 'macos-15-intel'],
  ['darwin-arm64', 'macos-15'],
]) {
  const pair = new RegExp(`- platform: ${platform}\\r?\\n\\s+runner: ${runner}(?:\\r?\\n|$)`)
  if (!pair.test(prepublishSmokeJob.source)) {
    throw new Error(`prepublish smoke is missing the ${platform} -> ${runner} runner mapping`)
  }
}
requireJobMarkers('publish', publishJob.source, [
  'needs: [verify, audit-tarballs, prepublish-smoke]',
  'npm publish',
])
requireJobMarkers('smoke', registrySmokeJob.source, [
  'needs: publish',
  'registry-url: https://registry.npmjs.org',
  'Clean registry install without build tools',
])
if (workflow.includes('test-host/node')) {
  throw new Error('release-npm.yml must use the canonical wake-test-host name')
}
for (const [marker, expectedCount] of [
  ['cargo fetch --locked --target ${{ matrix.target }}', 2],
  ['cargo build -p wake_test_host --release --locked --offline --target ${{ matrix.target }}', 2],
  ['-- --locked --offline', 2],
  ['git diff --exit-code -- Cargo.lock', 2],
  ['CARGO_NET_OFFLINE: "true"', 5],
]) {
  const count = workflow.split(marker).length - 1
  if (count !== expectedCount) {
    throw new Error(
      `release-npm.yml must contain ${expectedCount} copies of ${marker}; found ${count}`,
    )
  }
}

const missing = []
for (const { directory, directoryName, manifest } of packages) {
  if (typeof manifest.name !== 'string' || typeof manifest.version !== 'string') {
    throw new Error(`${directory}/package.json must declare name and version`)
  }
  if (manifest.publishConfig?.access !== 'public') {
    throw new Error(`${manifest.name} must set publishConfig.access to public`)
  }
  if (manifest.publishConfig?.provenance !== true) {
    throw new Error(`${manifest.name} must enable publishConfig.provenance`)
  }

  const directoryCovered = workflow.includes(directory)
    || workflow.includes(`package_dir: ${directoryName}`)
  const nameCovered = workflow.includes(`'${manifest.name}'`)
    || workflow.includes(`"${manifest.name}"`)
  if (!directoryCovered || !nameCovered) {
    missing.push(
      `${manifest.name} (${directory}; build=${directoryCovered}; audit=${nameCovered})`,
    )
  }
}

if (missing.length > 0) {
  throw new Error(`npm packages missing automatic release coverage:\n${missing.join('\n')}`)
}

for (const { directory, manifest } of packages) {
  const contract = PLATFORM_CONTRACTS[manifest.name]
  if (!contract) continue
  if (directory !== contract.directory) {
    throw new Error(`${manifest.name} must be published from ${contract.directory}`)
  }
  const expectedFiles = expectedPlatformFiles(manifest, contract)
    .filter((path) => path !== 'package.json')
    .sort()
  const actualFiles = (manifest.files ?? []).slice().sort()
  if (JSON.stringify(actualFiles) !== JSON.stringify(expectedFiles)) {
    throw new Error(
      `${directory} must stage only the canonical binding, test host, provenance and license files`,
    )
  }
  for (const field of ['dependencies', 'devDependencies', 'optionalDependencies', 'scripts']) {
    if (Object.keys(manifest[field] ?? {}).length !== 0) {
      throw new Error(`${manifest.name} must not declare ${field}`)
    }
  }
}

const wakeLoader = readFileSync(resolve(root, 'npm/wake/loader.cjs'), 'utf8')
if (
  !wakeLoader.includes("'wake-test-host.exe'")
  || !wakeLoader.includes("'wake-test-host'")
) {
  throw new Error('npm loader must resolve the canonical wake-test-host basename')
}
if (wakeLoader.includes("'node.exe' : 'node'")) {
  throw new Error('npm loader must not use the retired node test-host basename')
}

const extensionManifest = JSON.parse(
  readFileSync(resolve(root, 'editors/vscode-css/package.json'), 'utf8'),
)
if (extensionManifest.private !== true) {
  throw new Error(
    'editors/vscode-css is distributed as a GitHub VSIX and must be private for npm',
  )
}

const vscodeTargets = [
  'win32-x64',
  'linux-x64',
  'linux-arm64',
  'darwin-x64',
  'darwin-arm64',
]
for (const target of vscodeTargets) {
  if (!vscodeWorkflow.includes(`vsce_target: ${target}`)) {
    throw new Error(`VS Code automatic release is missing target ${target}`)
  }
}
for (const required of [
  "- 'vscode-css-v*'",
  'github-release:',
  'contents: write',
  'Check out release source for provenance',
  'actions/attest-build-provenance@v3',
  'gh release',
  'needs.verify.outputs.version',
  'export WAKE_BIN="$PWD/${{ matrix.wake_binary }}"',
  '--binary "$PWD/${{ matrix.binary }}"',
  '--out "$PWD/artifacts"',
]) {
  if (!vscodeWorkflow.includes(required)) {
    throw new Error(`vscode-css.yml is missing release contract marker ${required}`)
  }
}
for (const forbidden of [
  'vsce publish',
  'VSCE_PAT',
  'vscode-marketplace-release',
  '--azure-credential',
  '--oidc',
]) {
  if (vscodeWorkflow.includes(forbidden)) {
    throw new Error(`vscode-css.yml must not publish to a marketplace: ${forbidden}`)
  }
}
const packageScript = readFileSync(
  resolve(root, 'editors/vscode-css/scripts/package-vsix.mjs'),
  'utf8',
)
if (!packageScript.includes('extensionManifest.version')) {
  throw new Error('VSIX archive names must use the extension manifest version')
}

console.log(
  `npm automatic release coverage: ${packages.length}/${packages.length} (${packages.map(({ manifest }) => manifest.name).join(', ')})`,
)
console.log(
  `GitHub VSIX automatic release coverage: ${vscodeTargets.length}/${vscodeTargets.length} (${vscodeTargets.join(', ')})`,
)
