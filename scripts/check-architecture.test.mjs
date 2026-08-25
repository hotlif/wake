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
  validateNpmProvenance,
  validatePolicy,
  validateRepositorySources,
} from './check-architecture.mjs'

const decision = 'engineering/decisions/0001-architecture-evolution-loop.md'
const activeAdrRecords = new Map([
  ['0001-architecture-evolution-loop.md', { status: 'accepted' }],
  ['0003-compiler-and-shell-boundaries.md', { status: 'proposed' }],
  ['0010-shared-css-syntax-tree.md', { status: 'accepted' }],
  ['0020-react-browser-test-runtime.md', { status: 'proposed' }],
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
  const ci = readFileSync(new URL('../.github/workflows/ci.yml', import.meta.url), 'utf8')
  assert.match(ci, /npm run test262:es2024/)
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
  assert.match(ci, /cargo test -p wake_test_browser --lib -- --ignored/)
  assert.match(ci, /cargo test -p wake_test --lib -- --ignored/)
})

test('system browser conformance is exact-major pinned without a product download path', () => {
  const manifest = JSON.parse(
    readFileSync(
      new URL('../engineering/system-browser-conformance.json', import.meta.url),
      'utf8',
    ),
  )
  assert.equal(manifest.contract, 'ADR-0020')
  assert.equal(manifest.scope, 'ci-release-conformance-only')
  assert.equal(manifest.versionPolicy, 'exact-major')
  assert.equal(manifest.versionSource, 'cdp-browser-get-version')
  assert.equal(manifest.requiredHeadless, true)
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
  for (const policy of Object.values(manifest.targets)) {
    assert.equal(policy.major, 151)
    assert.deepEqual(policy.acceptedKinds, ['chrome', 'edge', 'chromium'])
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
  assert.match(release, /--reporter json/)
  assert.match(release, /--result browser-result\.json/)
  assert.ok(
    release.indexOf('--result browser-result.json') <
      release.indexOf('  publish:'),
    'the pinned browser result must be checked before publish',
  )
  assert.doesNotMatch(checker, /download|https?:\/\//i)
  for (const source of [ci, release]) {
    assert.doesNotMatch(
      source,
      /playwright|puppeteer|chrome-for-testing|setup-chrome|browser-actions/i,
    )
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
      npm: {
        allowedRegistryOrigins: ['https://registry.npmjs.org/'],
        workspaceLinks: 'declared-workspaces-only',
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

function npmFixture() {
  return {
    rootManifest: {
      name: 'wake-workspace',
      version: '0.1.0',
      workspaces: ['npm/wake'],
      dependencies: { react: '19.2.8', other: '^1.0.0' },
    },
    workspaceManifests: new Map([['npm/wake', {
      name: '@crab-dev/wake',
      version: '0.1.0',
      peerDependencies: { react: '>=19.2.0 <20' },
    }]]),
    lock: {
      lockfileVersion: 3,
      requires: true,
      packages: {
        '': { name: 'wake-workspace', version: '0.1.0' },
        'npm/wake': { name: '@crab-dev/wake', version: '0.1.0' },
        'node_modules/@crab-dev/wake': { resolved: 'npm/wake', link: true },
        'node_modules/react': {
          version: '19.2.8',
          resolved: 'https://registry.npmjs.org/react/-/react-19.2.8.tgz',
          integrity: npmIntegrity,
        },
        'node_modules/other': {
          version: '1.2.3',
          resolved: 'https://registry.npmjs.org/other/-/other-1.2.3.tgz',
          integrity: npmIntegrity,
        },
      },
    },
    policy: {
      lockfileVersion: 3,
      allowedRegistryOrigins: ['https://registry.npmjs.org/'],
      exactPackages: { react: '19.2.8' },
    },
  }
}

test('npm provenance allows manifest ranges while the lock owns exact registry artifacts', () => {
  assert.deepEqual(validateNpmProvenance(npmFixture()), [])
})

test('npm provenance rejects non-registry locators, corrupt locks, and false workspace links', () => {
  const invalidLocator = npmFixture()
  invalidLocator.rootManifest.dependencies.other = 'file:../other'
  assert(validateNpmProvenance(invalidLocator).some((error) => error.includes('npm-source')))

  const badResolved = npmFixture()
  badResolved.lock.packages['node_modules/react'].resolved = 'https://example.invalid/react.tgz'
  assert(validateNpmProvenance(badResolved).some((error) => error.includes('canonical npm registry tarball')))

  const badIntegrity = npmFixture()
  badIntegrity.lock.packages['node_modules/react'].integrity = 'sha1-deadbeef'
  assert(validateNpmProvenance(badIntegrity).some((error) => error.includes('SHA-512 integrity')))

  const rangedLock = npmFixture()
  rangedLock.lock.packages['node_modules/react'].version = '^19.2.8'
  assert(validateNpmProvenance(rangedLock).some((error) => error.includes('exact SemVer')))

  const falseLink = npmFixture()
  falseLink.lock.packages['node_modules/@crab-dev/wake'].resolved = '../outside'
  assert(validateNpmProvenance(falseLink).some((error) => error.includes('npm-link')))

  const missingExactPin = npmFixture()
  missingExactPin.rootManifest.dependencies.react = '^19.2.8'
  assert(validateNpmProvenance(missingExactPin).some((error) => error.includes('npm-pin')))
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
    ['.github/workflows/release-npm.yml', '- run: cargo build --locked --offline\n  env:\n    CARGO_NET_OFFLINE: "true"'],
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
  sources.set('.github/workflows/release-npm.yml', '- run: cargo build --release')
  const errors = validateRepositorySources({ files, sources, policy })
  assert(errors.some((error) => error.includes('vendor/deno_core')))
  assert(errors.some((error) => error.includes('happy-dom')))
  assert(errors.some((error) => error.includes('native.node')))
  assert(errors.some((error) => error.includes('build-network')))
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
