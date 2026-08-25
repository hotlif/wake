import assert from 'node:assert/strict'

import {
  parseChromiumMajor,
  validateSystemBrowserConformanceManifest,
  validateSystemBrowserIdentity,
} from './check-system-browser-conformance.mjs'

const targets = Object.fromEntries([
  'win32-x64-msvc',
  'linux-x64-gnu',
  'linux-arm64-gnu',
  'darwin-x64',
  'darwin-arm64',
].map((target) => [target, {
  major: 151,
  acceptedKinds: ['chrome', 'edge', 'chromium'],
}]))
const manifest = {
  schemaVersion: 1,
  contract: 'ADR-0020',
  scope: 'ci-release-conformance-only',
  versionPolicy: 'exact-major',
  versionSource: 'cdp-browser-get-version',
  requiredHeadless: true,
  targets,
}

assert.equal(validateSystemBrowserConformanceManifest(manifest), manifest)
assert.equal(parseChromiumMajor('Chrome/151.0.7922.172'), 151)
assert.equal(parseChromiumMajor('Microsoft Edge 151.0.4129.101'), 151)

for (const kind of ['chrome', 'edge', 'chromium']) {
  const checked = validateSystemBrowserIdentity({
    manifest,
    target: 'win32-x64-msvc',
    identity: {
      kind,
      executable: 'C:/browser/' + kind + '.exe',
      version: kind + '/151.0.1.99',
      headless: true,
    },
  })
  assert.equal(checked.major, 151)
  assert.equal(checked.kind, kind)
}

const result = {
  schemaVersion: 'wake.test.v1',
  success: true,
  environment: {
    kind: 'browser',
    browser: {
      name: 'chrome',
      version: 'Chrome/151.9.8.7',
      headless: true,
    },
  },
}
assert.equal(validateSystemBrowserIdentity({
  manifest,
  target: 'darwin-arm64',
  result,
}).version, 'Chrome/151.9.8.7')

for (const [description, action, pattern] of [
  [
    'wrong major',
    () => validateSystemBrowserIdentity({
      manifest,
      target: 'linux-x64-gnu',
      identity: { kind: 'chrome', version: 'Chrome/152.0.0.1', headless: true },
    }),
    /pins Chromium-family major 151/,
  ],
  [
    'unknown browser kind',
    () => validateSystemBrowserIdentity({
      manifest,
      target: 'linux-x64-gnu',
      identity: { kind: 'unknown', version: 'Chrome/151.0.0.1', headless: true },
    }),
    /requires chrome\/edge\/chromium/,
  ],
  [
    'non-headless result',
    () => validateSystemBrowserIdentity({
      manifest,
      target: 'linux-x64-gnu',
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
    () => validateSystemBrowserIdentity({
      manifest,
      target: 'darwin-x64',
      result: { ...result, success: false },
    }),
    /successful wake\.test\.v1 result/,
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
const splitMajor = structuredClone(manifest)
splitMajor.targets['darwin-arm64'].major = 152
assert.throws(
  () => validateSystemBrowserConformanceManifest(splitMajor),
  /must pin one shared major/,
)

console.log('System browser conformance checker tests passed: 15 cases')
