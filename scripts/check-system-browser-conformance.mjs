import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

const root = resolve(import.meta.dirname, '..')
export const systemBrowserConformanceManifestPath = resolve(
  root,
  'engineering/system-browser-conformance.json',
)

export const systemBrowserTargetIds = [
  'win32-x64-msvc',
  'linux-x64-gnu',
  'linux-arm64-gnu',
  'darwin-x64',
  'darwin-arm64',
]

const targetDefinitions = {
  'win32-x64-msvc': {
    runner: 'windows-latest',
    rustTarget: 'x86_64-pc-windows-msvc',
    evidencePath: '/images/windows/Windows2025-Readme.md',
  },
  'linux-x64-gnu': {
    runner: 'ubuntu-24.04',
    rustTarget: 'x86_64-unknown-linux-gnu',
    evidencePath: '/images/ubuntu/Ubuntu2404-Readme.md',
  },
  'linux-arm64-gnu': {
    runner: 'ubuntu-24.04-arm',
    rustTarget: 'aarch64-unknown-linux-gnu',
    evidencePath: '/images/ubuntu/Ubuntu2404-Arm64-Readme.md',
  },
  'darwin-x64': {
    runner: 'macos-15-intel',
    rustTarget: 'x86_64-apple-darwin',
    evidencePath: '/images/macos/macos-15-Readme.md',
  },
  'darwin-arm64': {
    runner: 'macos-15',
    rustTarget: 'aarch64-apple-darwin',
    evidencePath: '/images/macos/macos-15-arm64-Readme.md',
  },
}

const browserKinds = new Set(['chrome', 'edge', 'chromium'])
const evidenceModes = new Set([
  'exact-major-conformance',
  'exact-major-smoke',
  'unavailable',
])

function record(value, description) {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(description + ' must be an object')
  }
  return value
}

function exactKeys(value, expected, description) {
  const actual = Object.keys(value).sort()
  const wanted = [...expected].sort()
  if (JSON.stringify(actual) !== JSON.stringify(wanted)) {
    throw new Error(description + ' must contain exactly: ' + wanted.join(', '))
  }
}

function uniqueExactStrings(value, expected, description) {
  if (
    !Array.isArray(value) ||
    value.length !== expected.length ||
    new Set(value).size !== value.length ||
    value.some((entry) => typeof entry !== 'string') ||
    expected.some((entry) => !value.includes(entry))
  ) {
    throw new Error(description + ' must contain exactly: ' + expected.join(', '))
  }
}

function positiveInteger(value, description) {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(description + ' must be a positive integer')
  }
}

export function parseChromiumMajor(version) {
  if (typeof version !== 'string' || version.trim() === '') {
    throw new Error('system browser CDP version must be a non-empty string')
  }
  const match = version.match(/(?:^|[\s/])(\d+)\.\d+\.\d+\.\d+(?:\D|$)/)
  if (!match) {
    throw new Error('system browser CDP version is not a full Chromium version: ' + version)
  }
  return Number(match[1])
}

function validateReviewedEvidence(value, target, policy, acceptedKinds, evidencePath) {
  const evidence = record(value, 'reviewed runner evidence for ' + target)
  exactKeys(
    evidence,
    ['source', 'imageVersion', 'browserVersions'],
    'reviewed runner evidence for ' + target,
  )
  if (
    typeof evidence.source !== 'string' ||
    !/^https:\/\/github\.com\/actions\/runner-images\/blob\/[0-9a-f]{40}\//.test(evidence.source) ||
    !evidence.source.endsWith(evidencePath)
  ) {
    throw new Error(
      target + ' runner evidence must use its immutable official runner-images inventory',
    )
  }
  if (typeof evidence.imageVersion !== 'string' || !/^\d+\.\d+\.\d+$/.test(evidence.imageVersion)) {
    throw new Error(target + ' runner evidence imageVersion must be an exact hosted image version')
  }
  const versions = record(evidence.browserVersions, 'reviewed browser versions for ' + target)
  for (const [kind, version] of Object.entries(versions)) {
    if (!acceptedKinds.includes(kind)) {
      throw new Error(target + ' runner evidence has unsupported browser kind ' + kind)
    }
    const major = parseChromiumMajor(version)
    if (policy.mode === 'unavailable') {
      throw new Error(target + ' unavailable evidence cannot list a browser version')
    }
    if (major !== policy.major) {
      throw new Error(target + ' reviewed browser version must match experimental major ' + policy.major)
    }
  }
  if (policy.mode === 'unavailable' && Object.keys(versions).length !== 0) {
    throw new Error(target + ' unavailable evidence must have no browser versions')
  }
  if (policy.mode !== 'unavailable' && Object.keys(versions).length === 0) {
    throw new Error(target + ' browser evidence must list at least one reviewed browser version')
  }
  return evidence
}

export function validateSystemBrowserConformanceManifest(value) {
  const manifest = record(value, 'system browser conformance manifest')
  exactKeys(manifest, [
    'schemaVersion',
    'contract',
    'scope',
    'versionSource',
    'requiredHeadless',
    'browserBinaryPolicy',
    'acceptedKinds',
    'stableReadiness',
    'targets',
  ], 'system browser conformance manifest')
  if (
    manifest.schemaVersion !== 2 ||
    manifest.contract !== 'ADR-0020' ||
    manifest.scope !== 'ci-release-browser-evidence' ||
    manifest.versionSource !== 'cdp-browser-get-version' ||
    manifest.requiredHeadless !== true ||
    manifest.browserBinaryPolicy !== 'system-only-no-download'
  ) {
    throw new Error(
      'system browser conformance manifest must be ADR-0020 schema v2 with post-CDP, headless, system-only evidence',
    )
  }
  uniqueExactStrings(
    manifest.acceptedKinds,
    [...browserKinds],
    'accepted Chromium-family browser kinds',
  )

  const stable = record(manifest.stableReadiness, 'stable browser readiness policy')
  exactKeys(stable, ['policy', 'major', 'requiredTargets'], 'stable browser readiness policy')
  if (stable.policy !== 'shared-exact-major') {
    throw new Error('stable browser readiness policy must remain shared-exact-major')
  }
  positiveInteger(stable.major, 'stable browser readiness major')
  uniqueExactStrings(stable.requiredTargets, systemBrowserTargetIds, 'stable browser targets')

  const targets = record(manifest.targets, 'system browser conformance targets')
  exactKeys(targets, systemBrowserTargetIds, 'system browser conformance targets')
  for (const target of systemBrowserTargetIds) {
    const targetPolicy = record(targets[target], 'system browser policy for ' + target)
    exactKeys(
      targetPolicy,
      ['runner', 'rustTarget', 'experimental', 'reviewedRunnerEvidence'],
      'system browser policy for ' + target,
    )
    const definition = targetDefinitions[target]
    if (targetPolicy.runner !== definition.runner) {
      throw new Error(target + ' runner must be ' + definition.runner)
    }
    if (targetPolicy.rustTarget !== definition.rustTarget) {
      throw new Error(target + ' rustTarget must be ' + definition.rustTarget)
    }
    const policy = record(targetPolicy.experimental, 'experimental browser policy for ' + target)
    if (!evidenceModes.has(policy.mode)) {
      throw new Error(target + ' has an unsupported experimental browser evidence mode')
    }
    if (policy.mode === 'unavailable') {
      exactKeys(policy, ['mode', 'reason'], 'experimental browser policy for ' + target)
      if (typeof policy.reason !== 'string' || policy.reason.trim() === '') {
        throw new Error(target + ' unavailable policy must include a reason')
      }
    } else {
      exactKeys(policy, ['mode', 'major'], 'experimental browser policy for ' + target)
      positiveInteger(policy.major, target + ' experimental browser major')
    }
    validateReviewedEvidence(
      targetPolicy.reviewedRunnerEvidence,
      target,
      policy,
      manifest.acceptedKinds,
      definition.evidencePath,
    )
  }
  return manifest
}

export function readSystemBrowserConformanceManifest(path = systemBrowserConformanceManifestPath) {
  let value
  try {
    value = JSON.parse(readFileSync(path, 'utf8'))
  } catch (error) {
    throw new Error('unable to read system browser conformance manifest at ' + path, {
      cause: error,
    })
  }
  return validateSystemBrowserConformanceManifest(value)
}

function identityFromResult(value) {
  const result = record(value, 'Wake browser test result')
  if (result.schemaVersion !== 'wake.test.v1' || result.success !== true) {
    throw new Error('Wake browser evidence result must be a successful wake.test.v1 result')
  }
  const environment = record(result.environment, 'Wake browser test environment')
  if (environment.kind !== 'browser') {
    throw new Error('Wake browser evidence result must use the browser environment')
  }
  const browser = record(environment.browser, 'Wake browser identity')
  return {
    kind: browser.name,
    version: browser.version,
    headless: browser.headless,
    executable: undefined,
  }
}

export function validateExperimentalBrowserIdentity({ manifest, target, identity, result }) {
  const checkedManifest = validateSystemBrowserConformanceManifest(manifest)
  const targetPolicy = checkedManifest.targets[target]
  if (!targetPolicy) {
    throw new Error('unknown Wake browser evidence target: ' + target)
  }
  const policy = targetPolicy.experimental
  if (policy.mode === 'unavailable') {
    throw new Error(target + ' is reviewed as unavailable and cannot accept browser evidence')
  }
  if ((identity === undefined) === (result === undefined)) {
    throw new Error('provide exactly one browser identity or Wake browser test result')
  }
  const candidate = identity === undefined
    ? identityFromResult(result)
    : record(identity, 'system browser identity')
  const kind = candidate.kind
  if (!checkedManifest.acceptedKinds.includes(kind)) {
    throw new Error(
      target + ' requires ' + checkedManifest.acceptedKinds.join('/') + ' major ' +
      policy.major + '; found ' + String(kind),
    )
  }
  if (candidate.headless !== checkedManifest.requiredHeadless) {
    throw new Error(
      target + ' browser evidence requires headless=' +
      checkedManifest.requiredHeadless + '; found ' + String(candidate.headless),
    )
  }
  const major = parseChromiumMajor(candidate.version)
  if (major !== policy.major) {
    throw new Error(
      target + ' pins experimental Chromium-family major ' + policy.major +
      '; found ' + candidate.version,
    )
  }
  return {
    schemaVersion: 'wake.browser.evidence.v1',
    contract: checkedManifest.contract,
    target,
    runner: targetPolicy.runner,
    mode: policy.mode,
    status: 'passed',
    stableConformance:
      policy.mode === 'exact-major-conformance' &&
      policy.major === checkedManifest.stableReadiness.major,
    browser: {
      kind,
      version: candidate.version,
      major,
      headless: candidate.headless,
      executable: candidate.executable ?? null,
    },
    reviewedRunnerEvidence: targetPolicy.reviewedRunnerEvidence,
  }
}

export function recordUnavailableBrowserEvidence({ manifest, target }) {
  const checkedManifest = validateSystemBrowserConformanceManifest(manifest)
  const targetPolicy = checkedManifest.targets[target]
  if (!targetPolicy) {
    throw new Error('unknown Wake browser evidence target: ' + target)
  }
  if (targetPolicy.experimental.mode !== 'unavailable') {
    throw new Error(target + ' is not reviewed as browser-unavailable')
  }
  return {
    schemaVersion: 'wake.browser.evidence.v1',
    contract: checkedManifest.contract,
    target,
    runner: targetPolicy.runner,
    mode: 'unavailable',
    status: 'unavailable',
    stableConformance: false,
    browser: null,
    reason: targetPolicy.experimental.reason,
    reviewedRunnerEvidence: targetPolicy.reviewedRunnerEvidence,
  }
}

export function evaluateStableBrowserReadiness(value) {
  const manifest = validateSystemBrowserConformanceManifest(value)
  const blockers = []
  for (const target of manifest.stableReadiness.requiredTargets) {
    const policy = manifest.targets[target].experimental
    if (policy.mode === 'unavailable') {
      blockers.push({
        target,
        code: 'browser-unavailable',
        detail: policy.reason,
      })
      continue
    }
    if (policy.mode !== 'exact-major-conformance') {
      blockers.push({
        target,
        code: 'not-conformance-evidence',
        detail: 'experimental evidence mode is ' + policy.mode,
      })
    }
    if (policy.major !== manifest.stableReadiness.major) {
      blockers.push({
        target,
        code: 'major-mismatch',
        detail: 'reviewed major ' + policy.major + ' does not match shared stable major ' +
          manifest.stableReadiness.major,
      })
    }
  }
  return {
    schemaVersion: 'wake.browser.stable-readiness.v1',
    contract: manifest.contract,
    policy: manifest.stableReadiness.policy,
    major: manifest.stableReadiness.major,
    requiredTargets: manifest.stableReadiness.requiredTargets,
    ready: blockers.length === 0,
    blockers,
  }
}

function usage() {
  return 'Usage: node scripts/check-system-browser-conformance.mjs ' +
    '--manifest-only true | --stable-readiness <ready|blocked> | ' +
    '--target <target> (--identity <json-file> | --result <json-file> | --unavailable true)'
}

function parseArguments(arguments_) {
  const options = new Map()
  for (let index = 0; index < arguments_.length; index += 2) {
    const name = arguments_[index]
    const value = arguments_[index + 1]
    if (!name?.startsWith('--') || value === undefined || value.startsWith('--')) {
      throw new Error(usage())
    }
    if (options.has(name)) throw new Error('duplicate option ' + name)
    options.set(name, value)
  }
  return options
}

function readJson(path, description) {
  try {
    return JSON.parse(readFileSync(resolve(path), 'utf8'))
  } catch (error) {
    throw new Error('unable to read ' + description + ' at ' + path, { cause: error })
  }
}

function main(arguments_) {
  const options = parseArguments(arguments_)
  const manifest = readSystemBrowserConformanceManifest()
  if (options.get('--manifest-only') === 'true' && options.size === 1) {
    console.log(JSON.stringify({
      schemaVersion: 'wake.browser.manifest-check.v1',
      contract: manifest.contract,
      valid: true,
    }))
    return
  }
  const expectedReadiness = options.get('--stable-readiness')
  if (expectedReadiness !== undefined && options.size === 1) {
    const readiness = evaluateStableBrowserReadiness(manifest)
    if (!['ready', 'blocked'].includes(expectedReadiness)) throw new Error(usage())
    if ((expectedReadiness === 'ready') !== readiness.ready) {
      throw new Error(
        'stable browser readiness is ' + (readiness.ready ? 'ready' : 'blocked') +
        ', not ' + expectedReadiness,
      )
    }
    console.log(JSON.stringify(readiness, null, 2))
    return
  }

  const target = options.get('--target')
  const identityPath = options.get('--identity')
  const resultPath = options.get('--result')
  const unavailable = options.get('--unavailable')
  const inputCount = [identityPath, resultPath, unavailable].filter((value) => value !== undefined).length
  if (!target || inputCount !== 1 || options.size !== 2 || (unavailable && unavailable !== 'true')) {
    throw new Error(usage())
  }
  const evidence = unavailable === 'true'
    ? recordUnavailableBrowserEvidence({ manifest, target })
    : validateExperimentalBrowserIdentity({
        manifest,
        target,
        identity: identityPath ? readJson(identityPath, 'system browser identity') : undefined,
        result: resultPath ? readJson(resultPath, 'Wake browser test result') : undefined,
      })
  console.log(JSON.stringify(evidence, null, 2))
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2))
}
