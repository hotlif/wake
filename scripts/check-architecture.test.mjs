import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { test } from '@crab-dev/wake/test'
import {
  parseCargoTreePackages,
  parseCargoLock,
  validateAdrs,
  validateCargoManifestSources,
  validateCargoProvenance,
  validateCargoTreeRules,
  validateYarnProvenance,
  validatePolicy,
  validateRepositorySources,
} from './check-architecture.mjs'

const decision = 'engineering/decisions/0001-architecture-evolution-loop.md'
const activeAdrRecords = new Map([
  ['0001-architecture-evolution-loop.md', { status: 'accepted' }],
  ['0003-compiler-and-shell-boundaries.md', { status: 'proposed' }],
  ['0010-shared-css-syntax-tree.md', { status: 'accepted' }],
  ['0020-react-browser-test-runtime.md', { status: 'proposed' }],
  ['0021-local-platform-package-links.md', { status: 'superseded' }],
  ['0022-yarn-pnp-ownership.md', { status: 'accepted' }],
])

const publicTestContractFiles = [
  '.github/workflows/release-npm.yml',
  'crates/wake_test/Cargo.toml',
  'docs/reference/cli/test.mdx',
  'docs/reference/compatibility.mdx',
  'docs/reference/configuration/test.mdx',
  'docs/reference/errors.mdx',
  'docs/reference/node-api/test.mdx',
  'npm/wake/CHANGELOG.md',
  'npm/wake/bin/wake.mjs',
  'npm/wake/index.cjs',
  'npm/wake/index.d.ts',
  'npm/wake/index.mjs',
  'npm/wake/package.json',
  'npm/wake/test.cjs',
  'npm/wake/test.d.ts',
  'npm/wake/test.mjs',
  'npm/wake/test-react.cjs',
  'npm/wake/test-react.d.ts',
  'npm/wake/test-react.mjs',
]

const removedTestContracts = [
  ['Jest compatibility surface', /\bJest\b/i],
  ['Boa engine promise', /\bBoa(?:_engine|_gc)?\b/i],
  ['jsdom environment promise', /\bjsdom\b/i],
  ['test config initializer', /\binitTestConfig\b/],
  ['old runtime filename', /jest-runtime\.js/i],
  ['camelCase name flag', /\btestNamePattern\b/],
  ['run-in-band compatibility field', /\brunInBand\b/],
  ['singular snapshot update field', /\bupdateSnapshot\b/],
  ['legacy no-tests field', /\bpassWithNoTests\b/],
  ['legacy watch flag', /\bwatchAll\b/],
  ['legacy randomization field', /\brandomize\b/],
  ['legacy init flag', /--init\b/],
  ['legacy JSON flag', /--json\b/],
  ['legacy dashed name flag', /--test-name-pattern\b/],
  ['legacy dashed serial flag', /--run-in-band\b/],
  ['legacy dashed snapshot flag', /--update-snapshot\b/],
  ['legacy dashed no-tests flag', /--pass-with-no-tests\b/],
  ['legacy dashed watch flag', /--watch-all\b/],
  ['legacy flattened failure field', /\bfailureMessages\b/],
  ['legacy flattened result count', /\bnumPassedTestSuites\b/],
  ['inline source snapshot matcher', /\btoMatchInlineSnapshot\b/],
  ['intra-DOM concurrent test API', /readonly\s+concurrent\s*:/],
  ['legacy fake timer promise', /legacy\s+(?:fake\s+)?timer/i],
  ['Babel coverage promise', /Babel\s+coverage/i],
]

test('public test surfaces contain only the ADR 0020 Wake-native contract', () => {
  for (const path of publicTestContractFiles) {
    const source = readFileSync(new URL(`../${path}`, import.meta.url), 'utf8')
    for (const [contract, pattern] of removedTestContracts) {
      assert.doesNotMatch(source, pattern, `${path} still exposes ${contract}`)
    }
  }
})

test('active test kernel contains one versioned Wake result wire', () => {
  for (const path of [
    'crates/wake_test/src/lib.rs',
    'crates/wake_test/runtime/wake-test-runtime.js',
  ]) {
    const source = readFileSync(new URL(`../${path}`, import.meta.url), 'utf8')
    assert.match(source, /wake\.test\.runtime\.v1/, `${path} is missing the private result schema`)
    for (const [contract, pattern] of removedTestContracts) {
      assert.doesNotMatch(source, pattern, `${path} retains ${contract}`)
    }
    assert.doesNotMatch(source, /#\[cfg\(any\(\)\)\]/, `${path} retains a disabled compatibility path`)
    assert.doesNotMatch(source, /\b(?:ancestorTitles|numPassingAsserts)\b/, `${path} retains an old result field`)
  }
})

test('embedded V8 conformance uses one immutable selected Test262 ES2024 manifest', () => {
  const manifest = JSON.parse(
    readFileSync(new URL('../engineering/test262-es2024.json', import.meta.url), 'utf8'),
  )
  assert.equal(manifest.contract, 'ADR-0020')
  assert.equal(manifest.target, 'ES2024')
  assert.match(manifest.commit, /^[0-9a-f]{40}$/)
  assert.match(manifest.sha256, /^[0-9a-f]{64}$/)
  assert.ok(manifest.selectedRoots.length > 0)
  assert.equal(new Set(manifest.selectedRoots).size, manifest.selectedRoots.length)
  assert.deepEqual(
    new Set(manifest.excludedTests),
    new Set(Object.keys(manifest.exclusionReasons)),
  )

  const runner = readFileSync(new URL('../scripts/run-test262.mjs', import.meta.url), 'utf8')
  assert.match(runner, /createHash\('sha256'\)/)
  assert.match(runner, /wake_ecma_vm/)
  assert.match(runner, /'--locked'/)
  assert.match(runner, /'--offline'/)
  const ci = readFileSync(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8')
  assert.match(ci, /corepack yarn test262:es2024/)
  assert.match(ci, /prepare-rusty-v8\.mjs --target x86_64-unknown-linux-gnu/)
  assert.match(ci, /browser-conformance:/)
  for (const platform of [
    'windows-latest',
    'ubuntu-24.04',
    'ubuntu-24.04-arm',
    'macos-15',
    'macos-15-intel',
  ]) {
    assert.match(ci, new RegExp(platform.replaceAll('.', '\\.')))
  }
  assert.match(ci, /cargo test --locked --offline -p wake_test_browser --lib -- --ignored/)
  assert.match(ci, /cargo test --locked --offline -p wake_test --lib -- --ignored/)
})

test('system browser evidence separates experimental publication from stable readiness', () => {
  const manifest = JSON.parse(
    readFileSync(
      new URL('../engineering/system-browser-conformance.json', import.meta.url),
      'utf8',
    ),
  )
  assert.equal(manifest.schemaVersion, 3)
  assert.equal(manifest.contract, 'ADR-0020')
  assert.equal(manifest.scope, 'ci-release-browser-evidence')
  assert.equal(manifest.versionSource, 'cdp-browser-get-version')
  assert.equal(manifest.requiredHeadless, true)
  assert.equal(manifest.browserBinaryPolicy, 'system-only-no-download')
  assert.deepEqual(manifest.acceptedKinds, ['chrome', 'edge', 'chromium'])
  assert.equal(manifest.stableReadiness.policy, 'shared-exact-major')
  assert.equal(manifest.stableReadiness.major, 151)
  assert.deepEqual(
    Object.keys(manifest.targets).sort(),
    [
      'darwin-arm64',
      'darwin-x64',
      'linux-arm64-gnu',
      'linux-x64-gnu',
      'win32-x64-msvc',
    ],
  )
  assert.equal(manifest.targets['win32-x64-msvc'].experimental.mode, 'exact-major-conformance')
  assert.equal(manifest.targets['win32-x64-msvc'].experimental.major, 151)
  assert.equal(manifest.targets['linux-x64-gnu'].experimental.mode, 'exact-major-conformance')
  assert.equal(manifest.targets['linux-x64-gnu'].experimental.major, 151)
  assert.equal(manifest.targets['linux-arm64-gnu'].experimental.mode, 'unavailable')
  assert.deepEqual(
    manifest.targets['linux-arm64-gnu'].reviewedRunnerEvidence[0].browserVersions,
    {},
  )
  assert.equal(manifest.targets['darwin-x64'].experimental.mode, 'reviewed-major-smoke')
  assert.deepEqual(manifest.targets['darwin-x64'].experimental.majors, [150, 151])
  assert.equal(manifest.targets['darwin-arm64'].experimental.mode, 'exact-major-smoke')
  assert.equal(manifest.targets['darwin-arm64'].experimental.major, 150)
  for (const policy of Object.values(manifest.targets)) {
    assert(Array.isArray(policy.reviewedRunnerEvidence))
    for (const evidence of policy.reviewedRunnerEvidence) {
      assert.match(
        evidence.source,
        /^https:\/\/github\.com\/actions\/runner-images\/blob\/[0-9a-f]{40}\//,
      )
      assert.match(evidence.imageVersion, /^\d+\.\d+\.\d+$/)
    }
  }

  const checker = readFileSync(
    new URL('./check-system-browser-conformance.mjs', import.meta.url),
    'utf8',
  )
  const identityExample = readFileSync(
    new URL(
      '../crates/wake_test_browser/examples/system_browser_identity.rs',
      import.meta.url,
    ),
    'utf8',
  )
  const ci = readFileSync(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8')
  const release = readFileSync(
    new URL('../.github/workflows/release-npm.yml', import.meta.url),
    'utf8',
  )
  assert.match(identityExample, /BrowserDriver::launch/)
  assert.match(identityExample, /driver\.installation/)
  assert.match(ci, /check-system-browser-conformance\.mjs/)
  assert.match(ci, /--identity/)
  assert.match(ci, /--unavailable true/)
  assert.match(ci, /--stable-readiness blocked/)
  assert.match(ci, /browser-stable-readiness:/)
  assert.match(release, /--reporter json/)
  assert.match(release, /--result browser-result\.json/)
  assert.match(release, /--unavailable true/)
  assert.match(release, /--stable-readiness blocked/)
  assert.ok(
    release.indexOf('--result browser-result.json') <
      release.indexOf('  publish:'),
    'the pinned browser result must be checked before publish',
  )
  assert.doesNotMatch(checker, /from ['"]node:(?:http|https|net)['"]|\bfetch\s*\(/)
  for (const source of [ci, release]) {
    assert.doesNotMatch(
      source,
      /playwright|puppeteer|chrome-for-testing|setup-chrome|browser-actions/i,
    )
  }
})

test('architecture CI fetches the complete lock graph before its offline target-all check', () => {
  const ci = readFileSync(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8')
  const start = ci.indexOf('  architecture:')
  const end = ci.indexOf('\n  fmt:', start)
  assert.notEqual(start, -1)
  assert.notEqual(end, -1)
  const job = ci.slice(start, end)
  const markers = [
    'corepack yarn install --immutable --check-cache',
    'cargo fetch --locked',
    'node scripts/prepare-rusty-v8.mjs --target x86_64-unknown-linux-gnu',
    'cargo build -p wake_test_host -p wake_cli --locked --offline',
    'corepack yarn release:check',
    './target/debug/wake test scripts/check-architecture.test.mjs --serial',
    'corepack yarn architecture:check',
  ]
  let previous = -1
  for (const marker of markers) {
    const index = job.indexOf(marker)
    assert.ok(index > previous, `${marker} must follow the preceding clean-cache gate`)
    previous = index
  }
  assert.doesNotMatch(job, /cargo fetch --locked --target/)
  assert.match(job, /cargo tree --target all --offline/)

  const checker = readFileSync(
    new URL('./check-architecture.mjs', import.meta.url),
    'utf8',
  )
  assert.match(checker, /'--offline',[\s\S]*?'--target',[\s\S]*?'all'/)
})

test('Node CI stages the complete local platform package before testing and packing', () => {
  const ci = readFileSync(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8')
  const manifest = JSON.parse(readFileSync(new URL('../package.json', import.meta.url), 'utf8'))
  const startupCheck = readFileSync(new URL('./check-startup.mjs', import.meta.url), 'utf8')
  const nodeJobStart = ci.search(/\r?\n  node:\r?\n/)
  assert.notEqual(nodeJobStart, -1)
  const nodeJob = ci.slice(nodeJobStart)
  const markers = [
    'cargo fetch --locked',
    'node scripts/prepare-rusty-v8.mjs --target x86_64-pc-windows-msvc',
    'corepack yarn native:build',
    'git diff --exit-code -- Cargo.lock',
    'node scripts/stage-test-host.mjs --package-dir npm/wake-win32-x64-msvc',
    'corepack yarn npm:test:wake',
    'corepack yarn npm:pack:check',
  ]
  let previous = -1
  for (const marker of markers) {
    const index = nodeJob.indexOf(marker)
    assert(index > previous, `${marker} must follow the preceding Node CI stage`)
    previous = index
  }
  assert.doesNotMatch(nodeJob, /cargo fetch --locked --target/)
  assert.doesNotMatch(nodeJob, /Copy-Item .*\.node/)
  assert.equal(
    manifest.scripts['npm:test:wake'],
    'wake test npm/wake/test/cli.test.mjs npm/wake/test/components-state.test.mjs npm/wake/test/console.test.mjs npm/wake/test/terminal.test.mjs && yarn npm:test:wake:addon',
  )
  for (const marker of ['.pnp.cjs', '.pnp.loader.mjs', 'pathToFileURL', "'--experimental-loader'"]) {
    assert.match(startupCheck, new RegExp(marker.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))
  }
})

test('npm consumers are built from local tarballs and tested outside the PnP source tree', () => {
  const ci = readFileSync(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8')
  const release = readFileSync(
    new URL('../.github/workflows/release-npm.yml', import.meta.url),
    'utf8',
  )
  const consumer = readFileSync(new URL('./check-npm-consumer.mjs', import.meta.url), 'utf8')
  const artifactStart = ci.search(/\r?\n  npm-package-artifacts:\r?\n/)
  const consumerStart = ci.search(/\r?\n  npm-consumer:\r?\n/)
  const consumerEnd = ci.search(/\r?\n  architecture:\r?\n/)
  assert.notEqual(artifactStart, -1)
  assert(consumerStart > artifactStart)
  assert(consumerEnd > consumerStart)
  const artifactJob = ci.slice(artifactStart, consumerStart)
  const consumerJob = ci.slice(consumerStart, consumerEnd)

  for (const marker of [
    'platform: win32-x64-msvc',
    'platform: linux-x64-gnu',
    'corepack yarn install --immutable --check-cache',
    'corepack yarn native:build',
    'npm pack ./npm/wake --ignore-scripts --pack-destination artifacts',
    'npm pack ./npm/css --ignore-scripts --pack-destination artifacts',
    'npm pack ./npm/${{ matrix.package_dir }} --ignore-scripts --pack-destination artifacts',
    'node scripts/pack-npm-lock-platforms.mjs --artifacts artifacts --exclude ${{ matrix.platform }}',
  ]) assert.match(artifactJob, new RegExp(marker.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')))

  assert.match(consumerJob, /needs: npm-package-artifacts/)
  assert.match(consumerJob, /node: '22\.14\.0'/)
  assert.match(consumerJob, /node: '26'/)
  assert.match(consumerJob, /node scripts\/check-npm-consumer\.mjs/)
  assert.match(consumerJob, /WAKE_NPM_PROJECT: \$\{\{ runner\.temp \}\}\/wake-npm-consumer/)
  assert.doesNotMatch(consumerJob, /corepack|yarn install|cargo |npm pack|WAKE_NATIVE_PATH/)

  assert.match(consumer, /\['install', '--package-lock-only'/)
  assert.match(consumer, /\['ci', \.\.\.ciArguments\]/)
  assert.match(consumer, /optionalPlatformArchives/)
  assert.match(consumer, /assertNoPnpAncestor\(project\)/)
  assert.match(consumer, /WAKE_NPM_WORKSPACE_CLASSIC/)
  assert.match(consumer, /node_modules\/wake-npm-consumer-shared/)

  const prepublishStart = release.search(/\r?\n  prepublish-smoke:\r?\n/)
  const publishStart = release.search(/\r?\n  publish:\r?\n/)
  const prepublish = release.slice(prepublishStart, publishStart)
  assert.match(prepublish, /Select external npm consumer project/)
  assert.match(prepublish, /WAKE_NPM_PROJECT=%s\\n/)
  assert.match(prepublish, /\$RUNNER_TEMP\/wake-npm-consumer/)
  assert.match(prepublish, /node scripts\/check-npm-consumer\.mjs/)
  assert.doesNotMatch(prepublish, /--package-lock=false|mkdir local-smoke|cd local-smoke/)
})

test('Crab CSS editor tests use only the freshly built Wake CLI and sibling host', () => {
  const workflow = readFileSync(new URL('../.github/workflows/vscode-css.yml', import.meta.url), 'utf8')
  const yarnConfig = readFileSync(new URL('../.yarnrc.yml', import.meta.url), 'utf8')
  const architecturePolicy = JSON.parse(
    readFileSync(new URL('../engineering/architecture-boundaries.json', import.meta.url), 'utf8'),
  )
  const manifest = JSON.parse(
    readFileSync(new URL('../editors/vscode-css/package.json', import.meta.url), 'utf8'),
  )
  const build = readFileSync(
    new URL('../editors/vscode-css/scripts/build.mjs', import.meta.url),
    'utf8',
  )
  const launcher = readFileSync(
    new URL('../editors/vscode-css/scripts/run-wake-tests.mjs', import.meta.url),
    'utf8',
  )
  const binaryResolver = readFileSync(
    new URL('../editors/vscode-css/scripts/wake-binary.mjs', import.meta.url),
    'utf8',
  )
  const packageScript = readFileSync(
    new URL('../editors/vscode-css/scripts/package-vsix.mjs', import.meta.url),
    'utf8',
  )
  assert.equal(
    manifest.scripts.check,
    'yarn compile && node scripts/run-wake-tests.mjs test/manifest.test.mjs',
  )
  assert.deepEqual(
    architecturePolicy.dependencyProvenance.networkFreeBuild.offlineCargoBuildFiles,
    ['.github/workflows/release-npm.yml', '.github/workflows/vscode-css.yml'],
  )
  assert.match(binaryResolver, /process\.env\.WAKE_BIN/)
  assert.match(binaryResolver, /isAbsolute\(wakeBinary\)/)
  assert.match(build, /spawnSync\(wakeBinary, args/)
  assert.match(launcher, /spawnSync\(wakeBinary, \['test', \.\.\.testFiles, '--serial'\]/)
  assert.match(packageScript, /'--no-dependencies'/)
  assert.match(yarnConfig, /"@secretlint\/resolver@10\.2\.2":/)
  assert.match(yarnConfig, /"@secretlint\/secretlint-formatter-sarif": "10\.2\.2"/)
  assert.match(yarnConfig, /"@secretlint\/secretlint-rule-no-dotenv": "10\.2\.2"/)
  assert.match(yarnConfig, /"@secretlint\/secretlint-rule-preset-recommend": "10\.2\.2"/)
  for (const source of [manifest.scripts.check, build, launcher, binaryResolver]) {
    assert.doesNotMatch(source, /npm\/wake\/bin\/wake\.mjs|node_modules|releaseBinary|['"]cargo['"]|https?:\/\//)
  }

  const verifyStart = workflow.search(/\r?\n  verify:\r?\n/)
  const verifyEnd = workflow.search(/\r?\n  extension-host:\r?\n/)
  assert.notEqual(verifyStart, -1)
  assert(verifyEnd > verifyStart)
  const verify = workflow.slice(verifyStart, verifyEnd)
  const markers = [
    'corepack yarn install --immutable --check-cache',
    'cargo fetch --locked',
    'node scripts/prepare-rusty-v8.mjs --target x86_64-unknown-linux-gnu',
    'cargo build --release -p wake_test_host -p wake_cli --locked --offline',
    'corepack yarn release:check',
    'corepack yarn vscode:css:check',
    'WAKE_BIN: ${{ github.workspace }}/target/release/wake',
  ]
  let previous = -1
  for (const marker of markers) {
    const index = verify.indexOf(marker)
    assert(index > previous, `${marker} must follow the preceding VSIX verify stage`)
    previous = index
  }
  assert.match(verify, /CARGO_NET_OFFLINE: "true"/)
  assert.doesNotMatch(verify, /npm run native:build|napi build|stage-test-host/)
  assert.doesNotMatch(verify, /cargo fetch --locked --target/)

  const jobSource = (name, nextName) => {
    const start = workflow.search(new RegExp(`\\r?\\n  ${name}:\\r?\\n`))
    const end = workflow.search(new RegExp(`\\r?\\n  ${nextName}:\\r?\\n`))
    assert.notEqual(start, -1)
    assert(end > start)
    return workflow.slice(start, end)
  }
  const buildJobs = [
    {
      name: 'extension-host',
      source: jobSource('extension-host', 'package-native'),
      fetch: 'cargo fetch --locked --target x86_64-unknown-linux-gnu',
      build: 'cargo build --release -p wake_css_lsp -p wake_cli --locked --offline',
    },
    {
      name: 'package-native',
      source: jobSource('package-native', 'package-linux'),
      fetch: 'cargo fetch --locked --target ${{ matrix.rust_target }}',
      build: 'cargo build --release -p wake_css_lsp -p wake_cli --target ${{ matrix.rust_target }} --locked --offline',
    },
    {
      name: 'package-linux',
      source: jobSource('package-linux', 'github-release'),
      fetch: 'cargo fetch --locked --target ${{ matrix.rust_target }}',
      build: 'cargo build --release -p wake_css_lsp -p wake_cli --target ${{ matrix.rust_target }} --locked --offline',
    },
  ]
  for (const { name, source, fetch, build: buildMarker } of buildJobs) {
    const fetchIndex = source.indexOf(fetch)
    const buildIndex = source.indexOf(buildMarker)
    const offlineIndex = source.indexOf('CARGO_NET_OFFLINE: "true"')
    assert(fetchIndex >= 0, `${name} is missing its target-scoped Cargo fetch`)
    assert(buildIndex > fetchIndex, `${name} must build only after its Cargo fetch`)
    assert(offlineIndex > buildIndex, `${name} must force Cargo offline during its build`)
    assert.equal(
      source.split(/\r?\n/).filter((line) => line.includes('- run: cargo build')).length,
      1,
      `${name} must contain one Cargo build`,
    )
    assert.doesNotMatch(source, /prepare-rusty-v8\.mjs/)
  }
})

function policy(overrides = {}) {
  return {
    schemaVersion: 3,
    decision,
    dependencyProvenance: {
      decision,
      forbiddenTrackedPaths: ['vendor/**'],
      forbiddenTrackedBinaryExtensions: [],
      cargo: {
        allowedRegistrySources: ['registry+https://github.com/rust-lang/crates.io-index'],
        pathDependencies: 'workspace-members-only',
      },
      yarn: {
        decision: 'engineering/decisions/0022-yarn-pnp-ownership.md',
        packageManager: 'yarn@4.16.0',
        allowedResolutionProtocols: ['npm:', 'workspace:', 'patch:'],
        workspaceLocators: 'declared-workspaces-only',
        internalWorkspacePackages: {
          '@crab-dev/wake-win32-x64-msvc': 'npm/wake-win32-x64-msvc',
        },
      },
    },
    crates: ['wake_common', 'wake_ecma_parser', 'wake_app'],
    groups: { compiler: ['wake_common', 'wake_ecma_parser'] },
    cargoTreeRules: [{
      id: 'app-no-engine',
      description: 'app closure cannot contain the engine',
      from: ['wake_app'],
      denyPackages: ['deno_core'],
      decision,
      suggestion: 'spawn the isolated host',
    }],
    rules: [{
      id: 'compiler-no-app',
      description: 'compiler cannot depend on app',
      fromGroups: ['compiler'],
      deny: ['wake_app'],
      decision,
      suggestion: 'invert the dependency',
    }],
    ...overrides,
  }
}

test('rejects a forbidden compiler to app dependency', () => {
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set(['wake_common', 'wake_app'])],
    ['wake_app', new Set()],
  ])
  const errors = validatePolicy({ policy: policy(), packages, adrRecords: activeAdrRecords })
  assert(errors.some((error) => error.includes('[compiler-no-app] wake_ecma_parser -> wake_app')))
})

test('expands allow-only groups and rejects dependencies outside the declared layer', () => {
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set(['wake_common'])],
    ['wake_app', new Set(['wake_ecma_parser'])],
  ])
  const layered = policy({
    groups: { compiler: ['wake_common', 'wake_ecma_parser'] },
    rules: [{
      id: 'app-only-compiler',
      description: 'app test boundary',
      from: ['wake_app'],
      allowOnlyGroups: ['compiler'],
      decision,
      suggestion: 'use the compiler layer',
    }],
  })
  assert.deepEqual(validatePolicy({ policy: layered, packages, adrRecords: activeAdrRecords }), [])

  packages.get('wake_app').add('wake_app')
  const errors = validatePolicy({ policy: layered, packages, adrRecords: activeAdrRecords })
  assert(errors.some((error) => error.includes('[app-only-compiler] wake_app -> wake_app')))
})

test('repository policy rejects foundation, parser, and shell boundary regressions', () => {
  const repositoryPolicy = JSON.parse(readFileSync(new URL('../engineering/architecture-boundaries.json', import.meta.url), 'utf8'))
  const packages = new Map(repositoryPolicy.crates.map((name) => [name, new Set()]))
  packages.get('wake_common').add('wake_css')
  packages.get('wake_ecma_parser').add('wake_ecma_semantic')
  packages.get('wake_cli').add('wake_bundler')

  const errors = validatePolicy({
    policy: repositoryPolicy,
    packages,
    adrRecords: activeAdrRecords,
  })
  assert(errors.some((error) => error.includes('[common-is-workspace-foundation] wake_common -> wake_css')))
  assert(errors.some((error) => error.includes('[parser-does-not-own-semantic] wake_ecma_parser -> wake_ecma_semantic')))
  assert(errors.some((error) => error.includes('[shells-use-app-or-compiler] wake_cli -> wake_bundler')))
})

test('repository policy keeps browser policy above the driver', () => {
  const repositoryPolicy = JSON.parse(readFileSync(new URL('../engineering/architecture-boundaries.json', import.meta.url), 'utf8'))
  const packages = new Map(repositoryPolicy.crates.map((name) => [name, new Set()]))
  packages.get('wake_test').add('wake_test_browser')
  packages.get('wake_test').add('wake_test_contract')
  packages.get('wake_test_host').add('wake_test_contract')
  packages.get('wake_test_host').add('wake_test')
  packages.get('wake_app').add('wake_test_contract')

  assert.deepEqual(validatePolicy({
    policy: repositoryPolicy,
    packages,
    adrRecords: activeAdrRecords,
  }), [])

  packages.get('wake_test_browser').add('wake_test')
  const errors = validatePolicy({
    policy: repositoryPolicy,
    packages,
    adrRecords: activeAdrRecords,
  })
  assert(errors.some((error) => error.includes('[browser-driver-does-not-own-tests] wake_test_browser -> wake_test')))
})

test('repository policy separates the test contract, runner, host, and app', () => {
  const repositoryPolicy = JSON.parse(readFileSync(new URL('../engineering/architecture-boundaries.json', import.meta.url), 'utf8'))
  const packages = new Map(repositoryPolicy.crates.map((name) => [name, new Set()]))
  packages.get('wake_test').add('wake_test_contract')
  packages.get('wake_test_host').add('wake_test_contract')
  packages.get('wake_test_host').add('wake_test')
  packages.get('wake_app').add('wake_test_contract')

  assert.deepEqual(validatePolicy({
    policy: repositoryPolicy,
    packages,
    adrRecords: activeAdrRecords,
  }), [])

  packages.get('wake_app').add('wake_test')
  packages.get('wake_test_contract').add('wake_common')
  packages.get('wake_test_host').delete('wake_test_contract')
  const errors = validatePolicy({
    policy: repositoryPolicy,
    packages,
    adrRecords: activeAdrRecords,
  })
  assert(errors.some((error) => error.includes('[app-uses-test-contract-not-runner] wake_app -> wake_test')))
  assert(errors.some((error) => error.includes('[test-contract-is-data-only] wake_test_contract -> wake_common')))
  assert(errors.some((error) => error.includes('[test-host-owns-session-isolation-only] wake_test_host must directly depend on wake_test_contract')))
})

test('parses prefix-free Cargo tree output without depending on platform paths', () => {
  const packages = parseCargoTreePackages([
    'wake_cli v0.1.21 (C:\\repo\\crates\\wake_cli)',
    'wake_app v0.1.21 (/repo/crates/wake_app)',
    'deno_core v0.410.0',
    'v8 v150.4.0 (*)',
    '[build-dependencies]',
    '',
  ].join('\n'))
  assert.deepEqual(packages, new Set(['wake_cli', 'wake_app', 'deno_core', 'v8']))
})

test('Cargo tree rules reject engine leakage and require the authoritative host path', () => {
  const treePolicy = {
    groups: { shells: ['wake_cli', 'wake_node'] },
    cargoTreeRules: [
      {
        id: 'shells-no-engine',
        description: 'shell closure is engine-free',
        fromGroups: ['shells'],
        denyPackages: ['wake_test', 'deno_core', 'v8'],
        suggestion: 'spawn the host',
      },
      {
        id: 'host-has-runner',
        description: 'host owns execution',
        from: ['wake_test_host'],
        requirePackages: ['wake_test_contract', 'wake_test', 'deno_core', 'v8'],
        suggestion: 'link the authoritative runner',
      },
    ],
  }
  const packageTrees = new Map([
    ['wake_cli', new Set(['wake_cli', 'wake_app', 'wake_test_contract'])],
    ['wake_node', new Set(['wake_node', 'wake_app', 'wake_test_contract'])],
    ['wake_test_host', new Set(['wake_test_host', 'wake_test_contract', 'wake_test', 'deno_core', 'v8'])],
  ])
  assert.deepEqual(validateCargoTreeRules({ policy: treePolicy, packageTrees }), [])

  packageTrees.get('wake_cli').add('deno_core')
  packageTrees.get('wake_test_host').delete('wake_test_contract')
  const errors = validateCargoTreeRules({ policy: treePolicy, packageTrees })
  assert(errors.some((error) => error.includes('[shells-no-engine] wake_cli transitive cargo tree contains forbidden package deno_core')))
  assert(errors.some((error) => error.includes('[host-has-runner] wake_test_host transitive cargo tree is missing required package wake_test_contract')))
})

test('rejects malformed Cargo tree rules before invoking Cargo', () => {
  const malformed = policy({
    cargoTreeRules: [{
      id: 'bad-tree-rule',
      description: 'invalid tree rule',
      from: ['missing_crate'],
      denyPackages: ['v8', 'v8'],
      requirePackages: ['v8'],
      decision,
      suggestion: 'fix the rule',
    }],
  })
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set()],
    ['wake_app', new Set()],
  ])
  const errors = validatePolicy({ policy: malformed, packages, adrRecords: activeAdrRecords })
  assert(errors.some((error) => error.includes('references unknown crate missing_crate')))
  assert(errors.some((error) => error.includes('denyPackages contains duplicates')))
  assert(errors.some((error) => error.includes('package v8 cannot be both denied and required')))
})

const cratesIo = 'registry+https://github.com/rust-lang/crates.io-index'
const npmIntegrity = 'sha512-PWaYA1L/q9u2u7xYQi+Y3L3Yfnie7XyLeaJICV1MGD6LprsBxcAqGjYyr0eY3p+QdsA+x/Irkt4Qif8D63+Sbw=='

function cargoFixture(root) {
  const commonId = 'wake_common 0.1.0'
  const vmId = 'wake_ecma_vm 0.1.0'
  return {
    metadata: {
      workspace_members: [commonId, vmId],
      packages: [
        {
          id: commonId,
          name: 'wake_common',
          version: '0.1.0',
          manifest_path: join(root, 'crates', 'wake_common', 'Cargo.toml'),
          dependencies: [],
        },
        {
          id: vmId,
          name: 'wake_ecma_vm',
          version: '0.1.0',
          manifest_path: join(root, 'crates', 'wake_ecma_vm', 'Cargo.toml'),
          dependencies: [
            { name: 'wake_common', source: null, req: '*', path: join(root, 'crates', 'wake_common') },
            { name: 'deno_core', source: cratesIo, req: '=0.410.0', path: null },
          ],
        },
      ],
    },
    lockText: `# generated\nversion = 4\n\n[[package]]\nname = "wake_common"\nversion = "0.1.0"\n\n[[package]]\nname = "wake_ecma_vm"\nversion = "0.1.0"\n\n[[package]]\nname = "deno_core"\nversion = "0.410.0"\nsource = "${cratesIo}"\nchecksum = "${'a'.repeat(64)}"\n`,
    policy: {
      lockfileVersion: 4,
      allowedRegistrySources: [cratesIo],
      exactPackages: { deno_core: '0.410.0' },
      exclusiveOwners: { deno_core: ['wake_ecma_vm'] },
    },
  }
}

function clone(value) {
  return JSON.parse(JSON.stringify(value))
}

test('Cargo provenance accepts crates.io locks and first-party workspace paths', () => {
  const root = join(tmpdir(), 'wake-provenance-cargo')
  const fixture = cargoFixture(root)
  assert.equal(parseCargoLock(fixture.lockText).packages.length, 3)
  assert.deepEqual(validateCargoProvenance({ ...fixture, repoRoot: root }), [])
  assert.deepEqual(validateCargoManifestSources({
    repoRoot: root,
    workspacePaths: [join(root, 'crates', 'wake_common')],
    manifests: new Map([[
      join(root, 'fuzz', 'Cargo.toml'),
      '[dependencies]\nwake_common = { path = "../crates/wake_common" }\n',
    ]]),
  }), [])
})

test('Cargo provenance rejects external paths, git sources, missing checksums, and wrong owners', () => {
  const root = join(tmpdir(), 'wake-provenance-cargo-invalid')
  const base = cargoFixture(root)

  const externalPath = clone(base.metadata)
  externalPath.packages[0].dependencies.push({
    name: 'third_party',
    source: null,
    req: '*',
    path: join(root, 'vendor', 'third_party'),
  })
  assert(validateCargoProvenance({ ...base, metadata: externalPath, repoRoot: root })
    .some((error) => error.includes('cargo-path')))

  const wrongOwner = clone(base.metadata)
  wrongOwner.packages[0].dependencies.push({ name: 'deno_core', source: cratesIo, req: '=0.410.0', path: null })
  assert(validateCargoProvenance({ ...base, metadata: wrongOwner, repoRoot: root })
    .some((error) => error.includes('cargo-owner')))

  const gitSource = clone(base.metadata)
  gitSource.packages[1].dependencies[1].source = 'git+https://example.invalid/deno_core'
  assert(validateCargoProvenance({ ...base, metadata: gitSource, repoRoot: root })
    .some((error) => error.includes('cargo-source')))

  const missingChecksum = base.lockText.replace(`checksum = "${'a'.repeat(64)}"`, '')
  assert(validateCargoProvenance({ ...base, lockText: missingChecksum, repoRoot: root })
    .some((error) => error.includes('SHA-256 checksum')))

  const sourceFreeThirdParty = base.lockText
    .replace(`source = "${cratesIo}"\n`, '')
    .replace(`checksum = "${'a'.repeat(64)}"\n`, '')
  assert(validateCargoProvenance({ ...base, lockText: sourceFreeThirdParty, repoRoot: root })
    .some((error) => error.includes('is not a workspace member')))

  const manifestErrors = validateCargoManifestSources({
    repoRoot: root,
    workspacePaths: [join(root, 'crates', 'wake_common')],
    manifests: new Map([[
      join(root, 'fuzz', 'Cargo.toml'),
      '[dependencies]\nthird_party = { path = "../vendor/third_party" }\nremote = { git = "https://example.invalid/repo" }\n',
    ]]),
  })
  assert(manifestErrors.some((error) => error.includes('cargo-path')))
  assert(manifestErrors.some((error) => error.includes('cargo-source')))
})

function yarnFixture() {
  const checksum = `10c0/${'a'.repeat(128)}`
  const platformName = '@crab-dev/wake-win32-x64-msvc'
  const platformPath = 'npm/wake-win32-x64-msvc'
  return {
    rootManifest: {
      name: 'wake-workspace',
      version: '0.1.0',
      packageManager: 'yarn@4.16.0',
      workspaces: ['npm/*'],
      dependencies: { react: '19.2.8', other: '^1.0.0' },
    },
    workspaceManifests: new Map([
      ['npm/wake', {
        name: '@crab-dev/wake',
        version: '0.1.0',
        peerDependencies: { react: '>=19.2.0 <20' },
        optionalDependencies: { [platformName]: '0.1.0' },
      }],
      [platformPath, {
        name: platformName,
        version: '0.1.0',
        os: ['win32'],
        cpu: ['x64'],
      }],
    ]),
    internalManifests: new Map([[platformPath, {
      name: platformName,
      version: '0.1.0',
      os: ['win32'],
      cpu: ['x64'],
    }]]),
    lock: {
      __metadata: { version: '10', cacheKey: '10c0' },
      '@crab-dev/wake@workspace:npm/wake': {
        version: '0.0.0-use.local',
        resolution: '@crab-dev/wake@workspace:npm/wake',
        linkType: 'soft',
      },
      [`${platformName}@workspace:${platformPath}`]: {
        version: '0.0.0-use.local',
        resolution: `${platformName}@workspace:${platformPath}`,
        linkType: 'soft',
      },
      'react@npm:19.2.8': {
        version: '19.2.8',
        resolution: 'react@npm:19.2.8',
        checksum,
      },
      'other@npm:^1.0.0': {
        version: '1.2.3',
        resolution: 'other@npm:1.2.3',
        checksum,
      },
    },
    policy: {
      lockfileVersion: 10,
      packageManager: 'yarn@4.16.0',
      allowedResolutionProtocols: ['npm:', 'workspace:', 'patch:'],
      internalWorkspacePackages: { [platformName]: platformPath },
      exactPackages: { react: '19.2.8' },
    },
  }
}

test('Yarn provenance allows manifest ranges while the lock owns exact npm artifacts', () => {
  assert.deepEqual(validateYarnProvenance(yarnFixture()), [])
})

test('Yarn provenance owns platform packages through workspace locators', () => {
  const mismatchedPin = yarnFixture()
  mismatchedPin.workspaceManifests.get('npm/wake').optionalDependencies[
    '@crab-dev/wake-win32-x64-msvc'
  ] = '0.1.1'
  assert(validateYarnProvenance(mismatchedPin).some((error) => error.includes('must equal internal')))

  const retiredRootBridge = yarnFixture()
  retiredRootBridge.rootManifest.optionalDependencies = {}
  retiredRootBridge.rootManifest.optionalDependencies[
    '@crab-dev/wake-win32-x64-msvc'
  ] = 'file:npm/wake-win32-x64-msvc'
  assert(validateYarnProvenance(retiredRootBridge)
    .some((error) => error.includes('retired file: bridge')))

  const missingManifest = yarnFixture()
  missingManifest.internalManifests.clear()
  assert(validateYarnProvenance(missingManifest).some((error) => error.includes('must define')))
})

test('Yarn provenance rejects non-registry locators, corrupt locks, and false workspace locators', () => {
  const invalidLocator = yarnFixture()
  invalidLocator.rootManifest.dependencies.other = 'file:../other'
  assert(validateYarnProvenance(invalidLocator).some((error) => error.includes('yarn-source')))

  const badResolved = yarnFixture()
  badResolved.lock['react@npm:19.2.8'].resolution = 'react@git:https://example.invalid/react'
  assert(validateYarnProvenance(badResolved).some((error) => error.includes('yarn-resolution')))

  const badChecksum = yarnFixture()
  badChecksum.lock['react@npm:19.2.8'].checksum = 'sha1-deadbeef'
  assert(validateYarnProvenance(badChecksum).some((error) => error.includes('yarn-checksum')))

  const rangedLock = yarnFixture()
  rangedLock.lock['react@npm:19.2.8'].version = '^19.2.8'
  assert(validateYarnProvenance(rangedLock).some((error) => error.includes('exact SemVer')))

  const falseLocator = yarnFixture()
  falseLocator.lock['@crab-dev/wake@workspace:npm/wake'].linkType = 'hard'
  assert(validateYarnProvenance(falseLocator).some((error) => error.includes('yarn-workspace')))

  const missingExactPin = yarnFixture()
  missingExactPin.rootManifest.dependencies.react = '^19.2.8'
  assert(validateYarnProvenance(missingExactPin).some((error) => error.includes('yarn-pin')))
})

test('repository provenance rejects vendor trees, checked-in binaries, and networked build hooks', () => {
  const policy = {
    forbiddenTrackedPaths: ['vendor/**', 'crates/**/vendor/**'],
    forbiddenTrackedBinaryExtensions: ['.node'],
    networkFreeBuild: {
      forbiddenRustBuildScriptTokens: ['https://', 'reqwest::'],
      forbiddenNpmLifecycleScripts: ['preinstall', 'install', 'postinstall'],
      offlineCargoBuildFiles: ['.github/workflows/release-npm.yml'],
    },
  }
  const validFiles = [
    'crates/wake_node/build.rs',
    'package.json',
    '.github/workflows/release-npm.yml',
  ]
  const validSources = new Map([
    ['crates/wake_node/build.rs', 'fn main() { napi_build::setup(); }'],
    ['package.json', '{"scripts":{"build":"node build.mjs"}}'],
    [
      '.github/workflows/release-npm.yml',
      [
        '- run: cargo build --locked --offline',
        '- run: cargo test --locked --offline',
        '- run: cargo clippy --locked --offline',
        '  env:',
        '    CARGO_NET_OFFLINE: "true"',
      ].join('\n'),
    ],
  ])
  assert.deepEqual(validateRepositorySources({ files: validFiles, sources: validSources, policy }), [])

  const files = [
    ...validFiles,
    'vendor/deno_core-0.410.0/lib.rs',
    'crates/wake_js_runtime/vendor/happy-dom-20.11.6/index.js',
    'npm/wake/native.node',
  ]
  const sources = new Map(validSources)
  sources.set('crates/wake_node/build.rs', 'const URL: &str = "https://example.invalid/archive";')
  sources.set('package.json', '{"scripts":{"install":"node download.mjs"}}')
  sources.set(
    '.github/workflows/release-npm.yml',
    [
      '- run: cargo build --release',
      '- run: cargo test --workspace',
      '- run: cargo clippy --workspace',
    ].join('\n'),
  )
  const errors = validateRepositorySources({ files, sources, policy })
  assert(errors.some((error) => error.includes('vendor/deno_core')))
  assert(errors.some((error) => error.includes('happy-dom')))
  assert(errors.some((error) => error.includes('native.node')))
  assert(errors.some((error) => error.includes('cargo build must include --locked --offline')))
  assert(errors.some((error) => error.includes('cargo test must include --locked --offline')))
  assert(errors.some((error) => error.includes('cargo clippy must include --locked --offline')))
})

test('rejects an unregistered workspace crate', () => {
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set(['wake_common'])],
    ['wake_app', new Set()],
    ['wake_new', new Set()],
  ])
  const errors = validatePolicy({ policy: policy(), packages, adrRecords: activeAdrRecords })
  assert(errors.some((error) => error.includes('workspace crate wake_new is not registered')))
})

test('rejects boundary decisions that are not active', () => {
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set(['wake_common'])],
    ['wake_app', new Set()],
  ])
  const rejected = new Map([['0001-architecture-evolution-loop.md', { status: 'rejected' }]])
  const errors = validatePolicy({ policy: policy(), packages, adrRecords: rejected })
  assert(errors.some((error) => error.includes('must be proposed or accepted')))
})

test('rejects a boundary policy without an ADR', () => {
  const packages = new Map([
    ['wake_common', new Set()],
    ['wake_ecma_parser', new Set(['wake_common'])],
    ['wake_app', new Set()],
  ])
  const withoutDecision = policy({ decision: undefined, rules: [] })
  const errors = validatePolicy({ policy: withoutDecision, packages, adrRecords: activeAdrRecords })
  assert(errors.some((error) => error.includes('decision must reference an ADR')))
})

test('rejects invalid ADR status and a missing supersedes target', () => {
  const root = join(tmpdir(), `wake-architecture-${Date.now()}-${Math.random().toString(16).slice(2)}`)
  const decisionsDir = join(root, 'engineering', 'decisions')
  mkdirSync(decisionsDir, { recursive: true })
  writeFileSync(join(decisionsDir, '0001-first.md'), `# ADR 0001: First\n\n- Status: invalid\n\n${sections('None.')}`)
  writeFileSync(join(decisionsDir, '0002-second.md'), `# ADR 0002: Second\n\n- Status: proposed\n\n${sections('[ADR 0099](0099-missing.md)')}`)
  try {
    const result = validateAdrs({ repoRoot: root, decisionsDir })
    assert(result.errors.some((error) => error.includes('status must be proposed')))
    assert(result.errors.some((error) => error.includes('Supersedes target 0099-missing.md does not exist')))
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

test('rejects duplicate ADR numbers', () => {
  const root = join(tmpdir(), `wake-architecture-${Date.now()}-${Math.random().toString(16).slice(2)}`)
  const decisionsDir = join(root, 'engineering', 'decisions')
  mkdirSync(decisionsDir, { recursive: true })
  const body = `- Status: proposed\n\n${sections('None.')}`
  writeFileSync(join(decisionsDir, '0001-first.md'), `# ADR 0001: First\n\n${body}`)
  writeFileSync(join(decisionsDir, '0001-second.md'), `# ADR 0001: Second\n\n${body}`)
  try {
    const result = validateAdrs({ repoRoot: root, decisionsDir })
    assert(result.errors.some((error) => error.includes('ADR number 0001 duplicates')))
  } finally {
    rmSync(root, { recursive: true, force: true })
  }
})

function sections(supersedes) {
  return [
    '## Context\n\nContext.',
    '## Decision\n\nDecision.',
    '## Invariants\n\nInvariant.',
    '## Evidence\n\nEvidence.',
    '## Consequences\n\nConsequences.',
    '## Validation\n\nValidation.',
    `## Supersedes\n\n${supersedes}`,
    '## Removal plan\n\nNone.',
  ].join('\n\n')
}
