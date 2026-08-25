import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { pathToFileURL } from 'node:url'

const root = resolve(import.meta.dirname, '..')
export const systemBrowserConformanceManifestPath = resolve(
  root,
  'engineering/system-browser-conformance.json',
)

const targetIds = [
  'win32-x64-msvc',
  'linux-x64-gnu',
  'linux-arm64-gnu',
  'darwin-x64',
  'darwin-arm64',
]
const browserKinds = new Set(['chrome', 'edge', 'chromium'])

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

export function validateSystemBrowserConformanceManifest(value) {
  const manifest = record(value, 'system browser conformance manifest')
  exactKeys(manifest, [
    'schemaVersion',
    'contract',
    'scope',
    'versionPolicy',
    'versionSource',
    'requiredHeadless',
    'targets',
  ], 'system browser conformance manifest')
  if (
    manifest.schemaVersion !== 1 ||
    manifest.contract !== 'ADR-0020' ||
    manifest.scope !== 'ci-release-conformance-only' ||
    manifest.versionPolicy !== 'exact-major' ||
    manifest.versionSource !== 'cdp-browser-get-version' ||
    manifest.requiredHeadless !== true
  ) {
    throw new Error(
      'system browser conformance manifest must be ADR-0020 schema v1 with an exact-major, post-CDP, headless CI/release policy',
    )
  }

  const targets = record(manifest.targets, 'system browser conformance targets')
  exactKeys(targets, targetIds, 'system browser conformance targets')
  let sharedMajor
  for (const target of targetIds) {
    const policy = record(targets[target], 'system browser policy for ' + target)
    exactKeys(policy, ['major', 'acceptedKinds'], 'system browser policy for ' + target)
    if (!Number.isSafeInteger(policy.major) || policy.major <= 0) {
      throw new Error(target + ' browser major must be a positive integer')
    }
    sharedMajor ??= policy.major
    if (policy.major !== sharedMajor) {
      throw new Error('all Wake browser conformance targets must pin one shared major')
    }
    if (
      !Array.isArray(policy.acceptedKinds) ||
      policy.acceptedKinds.length === 0 ||
      new Set(policy.acceptedKinds).size !== policy.acceptedKinds.length ||
      policy.acceptedKinds.some((kind) => !browserKinds.has(kind))
    ) {
      throw new Error(target + ' acceptedKinds must be unique Chromium-family names')
    }
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

function identityFromResult(value) {
  const result = record(value, 'Wake browser test result')
  if (result.schemaVersion !== 'wake.test.v1' || result.success !== true) {
    throw new Error('Wake browser conformance result must be a successful wake.test.v1 result')
  }
  const environment = record(result.environment, 'Wake browser test environment')
  if (environment.kind !== 'browser') {
    throw new Error('Wake browser conformance result must use the browser environment')
  }
  const browser = record(environment.browser, 'Wake browser identity')
  return {
    kind: browser.name,
    version: browser.version,
    headless: browser.headless,
    executable: undefined,
  }
}

export function validateSystemBrowserIdentity({ manifest, target, identity, result }) {
  const checkedManifest = validateSystemBrowserConformanceManifest(manifest)
  const policy = checkedManifest.targets[target]
  if (!policy) {
    throw new Error('unknown Wake browser conformance target: ' + target)
  }
  if ((identity === undefined) === (result === undefined)) {
    throw new Error('provide exactly one browser identity or Wake browser test result')
  }
  const candidate = identity === undefined
    ? identityFromResult(result)
    : record(identity, 'system browser identity')
  const kind = candidate.kind
  if (!policy.acceptedKinds.includes(kind)) {
    throw new Error(
      target + ' requires ' + policy.acceptedKinds.join('/') + ' major ' +
      policy.major + '; found ' + String(kind),
    )
  }
  if (candidate.headless !== checkedManifest.requiredHeadless) {
    throw new Error(
      target + ' browser conformance requires headless=' +
      checkedManifest.requiredHeadless + '; found ' + String(candidate.headless),
    )
  }
  const major = parseChromiumMajor(candidate.version)
  if (major !== policy.major) {
    throw new Error(
      target + ' pins Chromium-family major ' + policy.major + '; found ' + candidate.version,
    )
  }
  return {
    target,
    kind,
    major,
    version: candidate.version,
    executable: candidate.executable,
  }
}

function parseArguments(arguments_) {
  const options = new Map()
  for (let index = 0; index < arguments_.length; index += 2) {
    const name = arguments_[index]
    const value = arguments_[index + 1]
    if (!name?.startsWith('--') || value === undefined || value.startsWith('--')) {
      throw new Error(
        'Usage: node scripts/check-system-browser-conformance.mjs --manifest-only true | --target <target> (--identity <json-file> | --result <json-file>)',
      )
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
    console.log('System browser conformance manifest is valid')
    return
  }
  const target = options.get('--target')
  const identityPath = options.get('--identity')
  const resultPath = options.get('--result')
  if (
    !target ||
    (identityPath === undefined) === (resultPath === undefined) ||
    options.size !== 2
  ) {
    throw new Error(
      'Usage: node scripts/check-system-browser-conformance.mjs --manifest-only true | --target <target> (--identity <json-file> | --result <json-file>)',
    )
  }
  const validated = validateSystemBrowserIdentity({
    manifest,
    target,
    identity: identityPath ? readJson(identityPath, 'system browser identity') : undefined,
    result: resultPath ? readJson(resultPath, 'Wake browser test result') : undefined,
  })
  console.log(
    'System browser conformance passed: target=' + validated.target +
    ' kind=' + validated.kind +
    ' version=' + validated.version +
    ' executable=' + (validated.executable ?? '(reported by Wake/CDP)'),
  )
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main(process.argv.slice(2))
}
