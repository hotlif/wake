import assert from 'node:assert/strict'

import {
  evaluateStableBrowserReadiness,
  parseChromiumMajor,
  readSystemBrowserConformanceManifest,
  recordUnavailableBrowserEvidence,
  validateExperimentalBrowserIdentity,
  validateSystemBrowserConformanceManifest,
} from './check-system-browser-conformance.mjs'

const manifest = readSystemBrowserConformanceManifest()
assert.equal(validateSystemBrowserConformanceManifest(manifest), manifest)
assert.equal(parseChromiumMajor('Chrome/151.0.7922.172'), 151)
assert.equal(parseChromiumMajor('Microsoft Edge 150.0.4078.99'), 150)

for (const kind of ['chrome', 'edge', 'chromium']) {
  const checked = validateExperimentalBrowserIdentity({
    manifest,
    target: 'win32-x64-msvc',
    identity: {
      kind,
      executable: 'C:/browser/' + kind + '.exe',
      version: kind + '/151.0.1.99',
      headless: true,
    },
  })
  assert.equal(checked.browser.major, 151)
  assert.equal(checked.browser.kind, kind)
  assert.equal(checked.schemaVersion, 'wake.browser.evidence.v2')
  assert.equal(checked.mode, 'exact-major-conformance')
  assert.equal(checked.stableConformance, true)
}

for (const major of [151, 152]) {
  const linuxChromeEvidence = validateExperimentalBrowserIdentity({
    manifest,
    target: 'linux-x64-gnu',
    identity: {
      kind: 'chrome',
      executable: '/usr/bin/google-chrome',
      version: `Google Chrome ${major}.0.0.1`,
      headless: true,
    },
  })
  assert.equal(linuxChromeEvidence.stableConformance, false)
  assert.equal(linuxChromeEvidence.mode, 'reviewed-major-conformance')
  assert.equal(linuxChromeEvidence.browser.kind, 'chrome')
  assert.equal(linuxChromeEvidence.browser.major, major)
  assert.equal(linuxChromeEvidence.browser.executable, '/usr/bin/google-chrome')
}

for (const major of [150, 151]) {
  const checked = validateExperimentalBrowserIdentity({
    manifest,
    target: 'darwin-x64',
    identity: {
      kind: 'chrome',
      version: `Chrome/${major}.0.0.1`,
      headless: true,
    },
  })
  assert.equal(checked.browser.major, major)
  assert.equal(checked.mode, 'reviewed-major-smoke')
  assert.equal(checked.stableConformance, false)
}

for (const major of [150, 152]) {
  const result = {
    schemaVersion: 'wake.test.v1',
    success: true,
    environment: {
      kind: 'browser',
      browser: {
        name: 'chrome',
        version: `Chrome/${major}.9.8.7`,
        headless: true,
      },
    },
  }
  const macEvidence = validateExperimentalBrowserIdentity({
    manifest,
    target: 'darwin-arm64',
    result,
  })
  assert.equal(macEvidence.browser.version, `Chrome/${major}.9.8.7`)
  assert.equal(macEvidence.mode, 'reviewed-major-smoke')
  assert.equal(macEvidence.stableConformance, false)
}

const armEvidence = recordUnavailableBrowserEvidence({
  manifest,
  target: 'linux-arm64-gnu',
})
assert.equal(armEvidence.status, 'unavailable')
assert.equal(armEvidence.browser, null)
assert.equal(armEvidence.schemaVersion, 'wake.browser.evidence.v2')
assert.equal(armEvidence.reviewedRunnerEvidence[0].browserVersions.chrome, undefined)

const readiness = evaluateStableBrowserReadiness(manifest)
assert.equal(readiness.ready, false)
assert.deepEqual(
  new Set(readiness.blockers.map(({ target }) => target)),
  new Set(['linux-x64-gnu', 'linux-arm64-gnu', 'darwin-x64', 'darwin-arm64']),
)
assert(readiness.blockers.some(({ target, code }) =>
  target === 'linux-arm64-gnu' && code === 'browser-unavailable'))
assert(readiness.blockers.some(({ target, code }) =>
  target === 'darwin-x64' && code === 'not-conformance-evidence'))

for (const [description, action, pattern] of [
  [
    'unreviewed rolling major',
    () => validateExperimentalBrowserIdentity({
      manifest,
      target: 'darwin-x64',
      identity: { kind: 'chrome', version: 'Chrome/149.0.0.1', headless: true },
    }),
    /permits reviewed Chromium-family majors 150, 151/,
  ],
  [
    'unreviewed Linux rolling major',
    () => validateExperimentalBrowserIdentity({
      manifest,
      target: 'linux-x64-gnu',
      identity: { kind: 'chrome', version: 'Chrome/150.0.0.1', headless: true },
    }),
    /permits reviewed Chromium-family majors 151, 152/,
  ],
  [
    'unknown browser kind',
    () => validateExperimentalBrowserIdentity({
      manifest,
      target: 'linux-x64-gnu',
      identity: { kind: 'unknown', version: 'Chrome/151.0.0.1', headless: true },
    }),
    /requires chrome\/edge\/chromium/,
  ],
  [
    'non-headless result',
    () => validateExperimentalBrowserIdentity({
      manifest,
      target: 'darwin-x64',
      identity: { kind: 'chrome', version: 'Chrome/151.0.0.1', headless: false },
    }),
    /requires headless=true/,
  ],
  [
    'non-CDP version',
    () => parseChromiumMajor('unknown (verified after CDP launch)'),
    /not a full Chromium version/,
  ],
  [
    'failed Wake result',
    () => validateExperimentalBrowserIdentity({
      manifest,
      target: 'darwin-x64',
      result: {
        schemaVersion: 'wake.test.v1',
        success: false,
        environment: {},
      },
    }),
    /successful wake\.test\.v1 result/,
  ],
  [
    'unavailable target rejects browser identity',
    () => validateExperimentalBrowserIdentity({
      manifest,
      target: 'linux-arm64-gnu',
      identity: { kind: 'chrome', version: 'Chrome/151.0.0.1', headless: true },
    }),
    /reviewed as unavailable/,
  ],
  [
    'available target rejects unavailable evidence',
    () => recordUnavailableBrowserEvidence({ manifest, target: 'linux-x64-gnu' }),
    /not reviewed as browser-unavailable/,
  ],
]) {
  assert.throws(action, pattern, description)
}

const missingTarget = structuredClone(manifest)
delete missingTarget.targets['linux-arm64-gnu']
assert.throws(
  () => validateSystemBrowserConformanceManifest(missingTarget),
  /must contain exactly/,
)
const extraField = { ...manifest, fallbackMajor: 150 }
assert.throws(
  () => validateSystemBrowserConformanceManifest(extraField),
  /must contain exactly/,
)
const inventedArmBrowser = structuredClone(manifest)
inventedArmBrowser.targets['linux-arm64-gnu'].reviewedRunnerEvidence[0].browserVersions.chrome =
  '151.0.0.1'
assert.throws(
  () => validateSystemBrowserConformanceManifest(inventedArmBrowser),
  /unavailable evidence cannot list a browser version/,
)
const unpinnedEvidence = structuredClone(manifest)
unpinnedEvidence.targets['darwin-x64'].reviewedRunnerEvidence[0].source =
  'https://github.com/actions/runner-images/blob/main/images/macos/macos-15-Readme.md'
assert.throws(
  () => validateSystemBrowserConformanceManifest(unpinnedEvidence),
  /immutable official runner-images inventory/,
)
const wrongRunner = structuredClone(manifest)
wrongRunner.targets['darwin-arm64'].runner = 'macos-15-intel'
assert.throws(
  () => validateSystemBrowserConformanceManifest(wrongRunner),
  /darwin-arm64 runner must be macos-15/,
)
const wrongInventory = structuredClone(manifest)
wrongInventory.targets['darwin-x64'].reviewedRunnerEvidence[0].source =
  manifest.targets['darwin-arm64'].reviewedRunnerEvidence[0].source
assert.throws(
  () => validateSystemBrowserConformanceManifest(wrongInventory),
  /darwin-x64 runner evidence must use its immutable official runner-images inventory/,
)

const missingRollingMajorEvidence = structuredClone(manifest)
missingRollingMajorEvidence.targets['darwin-x64'].reviewedRunnerEvidence.pop()
assert.throws(
  () => validateSystemBrowserConformanceManifest(missingRollingMajorEvidence),
  /must cover every reviewed experimental major/,
)

console.log('System browser conformance checker tests passed: schema v3 and release split')
