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
const ciWorkflow = readFileSync(resolve(root, '.github/workflows/ci.yml'), 'utf8')
const browserManifest = readSystemBrowserConformanceManifest()
const vscodeWorkflow = readFileSync(
  resolve(root, '.github/workflows/vscode-css.yml'),
  'utf8',
)

const releaseJobs = [...workflow.matchAll(/^  ([a-zA-Z0-9_-]+):\r?$/gm)]
const ciJobs = [...ciWorkflow.matchAll(/^  ([a-zA-Z0-9_-]+):\r?$/gm)]
const vscodeJobs = [...vscodeWorkflow.matchAll(/^  ([a-zA-Z0-9_-]+):\r?$/gm)]
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

function ciJob(name) {
  const matches = ciJobs.filter((match) => match[1] === name)
  if (matches.length !== 1) {
    throw new Error(`ci.yml must define exactly one ${name} job; found ${matches.length}`)
  }
  const match = matches[0]
  const index = ciJobs.indexOf(match)
  const end = ciJobs[index + 1]?.index ?? ciWorkflow.length
  return { index: match.index, source: ciWorkflow.slice(match.index, end) }
}

function vscodeJob(name) {
  const matches = vscodeJobs.filter((match) => match[1] === name)
  if (matches.length !== 1) {
    throw new Error(`vscode-css.yml must define exactly one ${name} job; found ${matches.length}`)
  }
  const match = matches[0]
  const index = vscodeJobs.indexOf(match)
  const end = vscodeJobs[index + 1]?.index ?? vscodeWorkflow.length
  return { index: match.index, source: vscodeWorkflow.slice(match.index, end) }
}

function requireOrderedVscodeJobMarkers(name, source, markers) {
  let previous = -1
  for (const marker of markers) {
    const index = source.indexOf(marker)
    if (index <= previous) {
      throw new Error(`vscode-css.yml ${name} job must order ${marker} after its preceding gate`)
    }
    previous = index
  }
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

function requireUnconditionalShellStep(name, source, stepName) {
  const pattern = new RegExp(
    `- name: ${escapeRegExp(stepName)}\\r?\\n\\s+shell:`,
  )
  if (!pattern.test(source)) {
    throw new Error(`${name} must run ${stepName} unconditionally for every matrix cell`)
  }
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

function requireBrowserMatrix(name, source, { targetKey, runnerKey, rustTargetKey }) {
  for (const [target, targetPolicy] of Object.entries(browserManifest.targets)) {
    const policy = targetPolicy.experimental
    const lines = [
      `- ${targetKey}: ${target}`,
      `${runnerKey}: ${targetPolicy.runner}`,
    ]
    if (rustTargetKey) lines.push(`${rustTargetKey}: ${targetPolicy.rustTarget}`)
    lines.push(`browser_evidence: ${policy.mode}`)
    if (policy.mode !== 'unavailable') lines.push(`browser_major: ${policy.major}`)
    const row = lines
      .map((line, index) => index === 0
        ? escapeRegExp(line)
        : `\\s+${escapeRegExp(line)}`)
      .join('\\r?\\n')
    if (!new RegExp(row).test(source)) {
      throw new Error(`${name} is missing the manifest-backed ${target} browser evidence row`)
    }
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
const ciNpmLockJob = ciJob('npm-lock')
const ciBrowserJobMatch = ciWorkflow.match(
  /^  browser-conformance:\r?\n[\s\S]*?(?=^  [a-zA-Z0-9_-]+:\r?$)/m,
)
if (!ciBrowserJobMatch) {
  throw new Error('ci.yml must define the browser-conformance job')
}
const ciBrowserJob = ciBrowserJobMatch[0]
const ciNodeJobMatch = ciWorkflow.match(/\r?\n  node:\r?\n/)
if (!ciNodeJobMatch) {
  throw new Error('ci.yml must define the node job')
}
const ciNodeJob = ciWorkflow.slice(ciNodeJobMatch.index)
if (!(auditTarballsJob.index < prepublishSmokeJob.index
  && prepublishSmokeJob.index < publishJob.index)) {
  throw new Error('local tarball smoke must run after audit-tarballs and before publish')
}
let npmLockMarkerIndex = -1
for (const marker of [
  'actions/checkout@v4',
  'actions/setup-node@v6',
  'node-version: 24',
  'npm run npm:lock:check',
]) {
  const index = ciNpmLockJob.source.indexOf(marker)
  if (index <= npmLockMarkerIndex) {
    throw new Error(`ci.yml npm-lock job must order ${marker} after its preceding gate`)
  }
  npmLockMarkerIndex = index
}
if (ciNpmLockJob.source.includes('npm ci')) {
  throw new Error('ci.yml npm-lock job must validate the lock before clean install')
}
for (const name of [
  'architecture',
  'clippy',
  'test',
  'test262-es2024',
  'browser-conformance',
  'typescript-7',
  'bench-smoke',
  'docs',
  'css',
  'node',
]) {
  if (!ciJob(name).source.includes('needs: npm-lock')) {
    throw new Error(`ci.yml ${name} job must depend on npm-lock before clean install`)
  }
}
requireOrderedJobMarkers('verify', verifyJob.source, [
  'actions/setup-node@v6',
  'npm run npm:lock:check',
  'npm ci --ignore-scripts',
])
requireOrderedJobMarkers('audit-tarballs', auditTarballsJob.source, [
  'actions/setup-node@v6',
  'node-version: 24',
  'npm ci --ignore-scripts',
  'node scripts/verify-native-package.mjs',
])
requireJobMarkers('verify', verifyJob.source, [
  'cargo fetch --locked',
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
for (const [name, job, lockMarkers] of [
  ['build-native', buildNativeJob, [
    'git diff --exit-code -- Cargo.lock',
  ]],
  ['build-linux', buildLinuxJob, [
    'cp Cargo.lock "$RUNNER_TEMP/Cargo.lock.before"',
    'cmp Cargo.lock "$RUNNER_TEMP/Cargo.lock.before"',
  ]],
]) {
  requireOrderedJobMarkers(name, job.source, [
    'cargo fetch --locked',
    'node scripts/prepare-rusty-v8.mjs --target ${{ matrix.target }}',
    ...lockMarkers.slice(0, -1),
    'cargo build -p wake_test_host --release --locked --offline --target ${{ matrix.target }}',
    'npx --no-install napi build',
    '-- --locked --offline',
    lockMarkers.at(-1),
    'node scripts/stage-test-host.mjs',
  ])
  if (job.source.includes('cargo fetch --locked --target')) {
    throw new Error(`${name} must fetch the complete locked graph before offline napi metadata`)
  }
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
  'await watchComplete',
  'TestContext watch smoke failed',
  "event.watching ? 'watchRunStart' : 'runStart'",
  'watchLifecycleValid',
  'context.stopWatch()',
  'await context.close()',
  'Select a system Chrome, Edge or Chromium',
  "matrix.node == '24' && matrix.browser_evidence != 'unavailable'",
  "matrix.node == '24' && matrix.browser_evidence == 'unavailable'",
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
  '--unavailable true',
  '--stable-readiness blocked',
  'browser-evidence-prepublish-${{ matrix.platform }}',
  '${{ runner.temp }}/wake-browser-evidence.json',
  '${{ runner.temp }}/wake-browser-stable-readiness.json',
])
if (prepublishSmokeJob.source.includes('continue-on-error')) {
  throw new Error('prepublish local tarball smoke must never continue on error')
}
if (prepublishSmokeJob.source.includes('npm publish')) {
  throw new Error('prepublish local tarball smoke must not mutate the npm registry')
}
requireUnconditionalShellStep(
  'prepublish-smoke',
  prepublishSmokeJob.source,
  "Clean install this build's local tarballs",
)
requireUnconditionalShellStep(
  'prepublish-smoke',
  prepublishSmokeJob.source,
  'Wake Test CLI, runTests and TestContext smoke',
)
requireBrowserMatrix('ci.yml browser-conformance', ciBrowserJob, {
  targetKey: 'target',
  runnerKey: 'os',
  rustTargetKey: 'rust_target',
})
requireBrowserMatrix('release-npm.yml prepublish-smoke', prepublishSmokeJob.source, {
  targetKey: 'platform',
  runnerKey: 'runner',
})
requireBrowserMatrix('release-npm.yml smoke', registrySmokeJob.source, {
  targetKey: 'platform',
  runnerKey: 'runner',
})
requireJobMarkers('ci browser-conformance', ciBrowserJob, [
  "matrix.browser_evidence == 'unavailable'",
  "matrix.browser_evidence != 'unavailable'",
  'cargo test --locked --offline -p wake_test_browser --lib -- --ignored --nocapture --test-threads=1',
  'cargo test --locked --offline -p wake_test --lib -- --ignored --nocapture --test-threads=1',
  '--identity "$identity" > "$RUNNER_TEMP/wake-browser-evidence.json"',
  '--unavailable true > "$RUNNER_TEMP/wake-browser-evidence.json"',
  'browser-evidence-ci-${{ matrix.target }}',
])
if (ciBrowserJob.includes('continue-on-error')) {
  throw new Error('CI browser evidence must never continue on error')
}
requireOrderedJobMarkers('ci node', ciNodeJob, [
  'npm ci --ignore-scripts',
  'cargo fetch --locked',
  'node scripts/prepare-rusty-v8.mjs --target x86_64-pc-windows-msvc',
  'npm run native:build',
  'git diff --exit-code -- Cargo.lock',
  'node scripts/stage-test-host.mjs',
])
if (ciNodeJob.includes('cargo fetch --locked --target')) {
  throw new Error('CI node must fetch the complete locked graph before offline napi metadata')
}
for (const marker of [
  'browser-stable-readiness:',
  'needs: browser-conformance',
  '--stable-readiness blocked > "$RUNNER_TEMP/wake-browser-stable-readiness.json"',
  'browser-stable-readiness-ci',
]) {
  if (!ciWorkflow.includes(marker)) {
    throw new Error(`ci.yml is missing the explicit blocked stable-browser marker ${marker}`)
  }
}
requireJobMarkers('publish', publishJob.source, [
  'needs: [verify, audit-tarballs, prepublish-smoke]',
  'npm publish',
])
requireJobMarkers('smoke', registrySmokeJob.source, [
  'needs: publish',
  'platform: [win32-x64-msvc, linux-x64-gnu, linux-arm64-gnu, darwin-x64, darwin-arm64]',
  'node: [24, 26]',
  'registry-url: https://registry.npmjs.org',
  'Clean registry install without build tools',
  "matrix.node == 24 && matrix.browser_evidence != 'unavailable'",
  "matrix.node == 24 && matrix.browser_evidence == 'unavailable'",
  '--browser-path "$browser"',
  '--output browser-result.json',
  '--result browser-result.json',
  '--unavailable true',
  '--stable-readiness blocked',
  'browser-evidence-postpublish-${{ matrix.platform }}',
])
if (registrySmokeJob.source.includes('continue-on-error')) {
  throw new Error('postpublish registry smoke must never continue on error')
}
requireUnconditionalShellStep(
  'smoke',
  registrySmokeJob.source,
  'Clean registry install without build tools',
)
for (const source of [workflow, ciWorkflow]) {
  if (/playwright|puppeteer|chrome-for-testing|setup-chrome|browser-actions/i.test(source)) {
    throw new Error('browser evidence workflows must not install a third-party browser')
  }
}
if (workflow.includes('test-host/node')) {
  throw new Error('release-npm.yml must use the canonical wake-test-host name')
}
for (const [marker, expectedCount] of [
  ['cargo build -p wake_test_host --release --locked --offline --target ${{ matrix.target }}', 2],
  ['-- --locked --offline', 2],
  ['git diff --exit-code -- Cargo.lock', 1],
  ['cp Cargo.lock "$RUNNER_TEMP/Cargo.lock.before"', 1],
  ['cmp Cargo.lock "$RUNNER_TEMP/Cargo.lock.before"', 1],
  ['archive_details="$(tar -tvzf "$archive")"', 1],
  ['<<<"$archive_details"', 2],
  ['CARGO_NET_OFFLINE: "true"', 5],
]) {
  const count = workflow.split(marker).length - 1
  if (count !== expectedCount) {
    throw new Error(
      `release-npm.yml must contain ${expectedCount} copies of ${marker}; found ${count}`,
    )
  }
}
if (workflow.includes('tar -tvzf "$archive" |')) {
  throw new Error('release tarball audit must consume the complete archive listing before matching')
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

const vscodeVerifyJob = vscodeJob('verify')
const vscodeVerifyMarkers = [
  'npm run npm:lock:check',
  'npm ci --ignore-scripts',
  'npm ci --ignore-scripts --prefix editors/vscode-css',
  'cargo fetch --locked',
  'node scripts/prepare-rusty-v8.mjs --target x86_64-unknown-linux-gnu',
  'cargo build --release -p wake_test_host -p wake_cli --locked --offline',
  'npm run release:check',
  'npm run vscode:css:check',
  'WAKE_BIN: ${{ github.workspace }}/target/release/wake',
]
requireOrderedVscodeJobMarkers('verify', vscodeVerifyJob.source, vscodeVerifyMarkers)
for (const marker of [
  'cache-dependency-path: |',
  'package-lock.json',
  'editors/vscode-css/package-lock.json',
  'CARGO_NET_OFFLINE: "true"',
]) {
  if (!vscodeVerifyJob.source.includes(marker)) {
    throw new Error(`vscode-css.yml verify job is missing contract marker ${marker}`)
  }
}
if (!vscodeWorkflow.includes("'scripts/check-npm-lock.mjs'")) {
  throw new Error('vscode-css.yml must run when the npm lock preflight changes')
}
for (const forbidden of ['npm run native:build', 'napi build', 'stage-test-host']) {
  if (vscodeVerifyJob.source.includes(forbidden)) {
    throw new Error(`vscode-css.yml verify job must not stage the retired Node test path: ${forbidden}`)
  }
}
if (vscodeVerifyJob.source.includes('cargo fetch --locked --target')) {
  throw new Error('vscode-css.yml verify job must fetch the complete lock graph for architecture:check')
}

const vscodeBuildJobs = [
  {
    name: 'extension-host',
    job: vscodeJob('extension-host'),
    fetch: 'cargo fetch --locked --target x86_64-unknown-linux-gnu',
    build: 'cargo build --release -p wake_css_lsp -p wake_cli --locked --offline',
  },
  {
    name: 'package-native',
    job: vscodeJob('package-native'),
    fetch: 'cargo fetch --locked --target ${{ matrix.rust_target }}',
    build: 'cargo build --release -p wake_css_lsp -p wake_cli --target ${{ matrix.rust_target }} --locked --offline',
  },
  {
    name: 'package-linux',
    job: vscodeJob('package-linux'),
    fetch: 'cargo fetch --locked --target ${{ matrix.rust_target }}',
    build: 'cargo build --release -p wake_css_lsp -p wake_cli --target ${{ matrix.rust_target }} --locked --offline',
  },
]
for (const { name, job, fetch, build } of vscodeBuildJobs) {
  requireOrderedVscodeJobMarkers(name, job.source, [
    'npm ci --ignore-scripts --prefix editors/vscode-css',
    fetch,
    build,
    'CARGO_NET_OFFLINE: "true"',
  ])
  const cargoBuildLines = job.source
    .split(/\r?\n/)
    .filter((line) => line.includes('- run: cargo build'))
  if (cargoBuildLines.length !== 1 || !cargoBuildLines[0].includes('--locked --offline')) {
    throw new Error(`vscode-css.yml ${name} must contain one locked/offline Cargo build`)
  }
  if (job.source.includes('prepare-rusty-v8.mjs')) {
    throw new Error(`vscode-css.yml ${name} must not prepare V8 for CLI-only packaging`)
  }
}

const extensionBuild = readFileSync(
  resolve(root, 'editors/vscode-css/scripts/build.mjs'),
  'utf8',
)
const extensionTestLauncher = readFileSync(
  resolve(root, 'editors/vscode-css/scripts/run-wake-tests.mjs'),
  'utf8',
)
const extensionWakeBinary = readFileSync(
  resolve(root, 'editors/vscode-css/scripts/wake-binary.mjs'),
  'utf8',
)
if (
  extensionManifest.scripts?.check
  !== 'npm run compile && node scripts/run-wake-tests.mjs test/manifest.test.mjs'
) {
  throw new Error('Crab CSS package check must use the first-party Wake test launcher')
}
for (const marker of ['process.env.WAKE_BIN', 'isAbsolute(wakeBinary)', 'statSync(wakeBinary)']) {
  if (!extensionWakeBinary.includes(marker)) {
    throw new Error(`Crab CSS WAKE_BIN resolver is missing contract marker ${marker}`)
  }
}
if (!extensionBuild.includes('spawnSync(wakeBinary, args')) {
  throw new Error('Crab CSS editor build must consume only the explicit WAKE_BIN executable')
}
if (!extensionTestLauncher.includes("spawnSync(wakeBinary, ['test', ...testFiles, '--serial']")) {
  throw new Error('Crab CSS editor tests must execute the explicit Wake CLI in serial mode')
}
for (const [name, source] of [
  ['package check', extensionManifest.scripts?.check ?? ''],
  ['editor build', extensionBuild],
  ['editor test launcher', extensionTestLauncher],
  ['WAKE_BIN resolver', extensionWakeBinary],
]) {
  for (const forbidden of [
    'npm/wake/bin/wake.mjs',
    'node_modules',
    'releaseBinary',
    "'cargo'",
    'http://',
    'https://',
  ]) {
    if (source.includes(forbidden)) {
      throw new Error(`Crab CSS ${name} must not retain a fallback path: ${forbidden}`)
    }
  }
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
