const FEDERATION_RUNTIME_ABI = 'wake.federation.v1'
const FEDERATION_MANIFEST_SCHEMA = 'wake.federation.manifest.v1'
const FEDERATION_DEV_UPDATE_SCHEMA = 'wake.federation.dev-update.v1'
const FEDERATION_DEV_LEASE_SCHEMA = 'wake.federation.dev-lease.v1'
const FEDERATION_DEV_MAX_BUILD_LEASES = 8
const FEDERATION_ISOLATED_REMOUNT_EVENT = 'wake:federation:isolated-remount'
const DEFAULT_SCOPE = 'default'
const DEFAULT_TIMEOUT_MS = 15_000
const DEFAULT_MAX_MANIFEST_SIZE = 1024 * 1024
const DEFAULT_MAX_ASSET_SIZE = 64 * 1024 * 1024
const MAX_TIMEOUT_MS = 5 * 60 * 1000
const MAX_MANIFEST_SIZE = 16 * 1024 * 1024
const MAX_ASSET_SIZE = 512 * 1024 * 1024
const DEFAULT_DEV_RECONNECT_MS = 250
const MAX_DEV_RECONNECT_MS = 5_000
const REACT_COHERENCE_MEMBERS = Object.freeze([
  'react',
  'react/jsx-runtime',
  'react/jsx-dev-runtime',
  'react-dom',
  'react-dom/client',
])
const RUNTIME_SYMBOL = Symbol.for('wake.federation.v1')
const ASSET_CONTEXTS_SYMBOL = Symbol.for('wake.federation.asset-contexts.v1')

const FEDERATION_ERROR_CODES = Object.freeze({
  INVALID_SPECIFIER: 'FED_INVALID_SPECIFIER',
  CONFIG_INVALID: 'FED_CONFIG_INVALID',
  LOCK_REQUIRED: 'FED_LOCK_REQUIRED',
  LOCK_INVALID: 'FED_LOCK_INVALID',
  LOCK_MISMATCH: 'FED_LOCK_MISMATCH',
  UNKNOWN_REMOTE: 'FED_UNKNOWN_REMOTE',
  MANIFEST_FETCH: 'FED_MANIFEST_FETCH',
  MANIFEST_SCHEMA: 'FED_MANIFEST_SCHEMA',
  RUNTIME_ABI: 'FED_RUNTIME_ABI',
  ORIGIN_DENIED: 'FED_ORIGIN_DENIED',
  MANIFEST_INTEGRITY: 'FED_MANIFEST_INTEGRITY',
  ASSET_INTEGRITY: 'FED_ASSET_INTEGRITY',
  ASSET_MIME: 'FED_ASSET_MIME',
  ASSET_SIZE: 'FED_ASSET_SIZE',
  UNKNOWN_EXPOSE: 'FED_UNKNOWN_EXPOSE',
  CONTAINER_INIT: 'FED_CONTAINER_INIT',
  CONTAINER_GET: 'FED_CONTAINER_GET',
  SHARE_UNSATISFIABLE: 'FED_SHARE_UNSATISFIABLE',
  SHARE_SINGLETON_CONFLICT: 'FED_SHARE_SINGLETON_CONFLICT',
  COHERENCE_CONFLICT: 'FED_COHERENCE_CONFLICT',
  TYPE_BUILD_MISMATCH: 'FED_TYPE_BUILD_MISMATCH',
  TYPES_INVALID: 'FED_TYPES_INVALID',
  TIMEOUT: 'FED_TIMEOUT',
  NETWORK: 'FED_NETWORK',
  STATIC_REMOTE_UNSUPPORTED: 'FED_STATIC_REMOTE_UNSUPPORTED',
  REMOTE_CYCLE: 'FED_REMOTE_CYCLE',
  REMOTE_CONFLICT: 'FED_REMOTE_CONFLICT',
  UNSUPPORTED_ENVIRONMENT: 'FED_UNSUPPORTED_ENVIRONMENT',
  CONTAINER_REGISTRATION: 'FED_CONTAINER_REGISTRATION',
  BRIDGE_LIFECYCLE: 'FED_BRIDGE_LIFECYCLE',
  BRIDGE_PROPS: 'FED_BRIDGE_PROPS',
  STYLE_LOAD: 'FED_STYLE_LOAD',
})

const NON_RETRYABLE_CODES = new Set([
  FEDERATION_ERROR_CODES.MANIFEST_SCHEMA,
  FEDERATION_ERROR_CODES.CONFIG_INVALID,
  FEDERATION_ERROR_CODES.LOCK_REQUIRED,
  FEDERATION_ERROR_CODES.LOCK_INVALID,
  FEDERATION_ERROR_CODES.LOCK_MISMATCH,
  FEDERATION_ERROR_CODES.RUNTIME_ABI,
  FEDERATION_ERROR_CODES.ORIGIN_DENIED,
  FEDERATION_ERROR_CODES.MANIFEST_INTEGRITY,
  FEDERATION_ERROR_CODES.ASSET_INTEGRITY,
  FEDERATION_ERROR_CODES.ASSET_MIME,
  FEDERATION_ERROR_CODES.ASSET_SIZE,
  FEDERATION_ERROR_CODES.UNKNOWN_EXPOSE,
  FEDERATION_ERROR_CODES.SHARE_UNSATISFIABLE,
  FEDERATION_ERROR_CODES.SHARE_SINGLETON_CONFLICT,
  FEDERATION_ERROR_CODES.COHERENCE_CONFLICT,
  FEDERATION_ERROR_CODES.TYPE_BUILD_MISMATCH,
  FEDERATION_ERROR_CODES.TYPES_INVALID,
  FEDERATION_ERROR_CODES.REMOTE_CONFLICT,
])

const FATAL_REMOTE_CODES = new Set([
  FEDERATION_ERROR_CODES.MANIFEST_SCHEMA,
  FEDERATION_ERROR_CODES.LOCK_MISMATCH,
  FEDERATION_ERROR_CODES.RUNTIME_ABI,
  FEDERATION_ERROR_CODES.ORIGIN_DENIED,
  FEDERATION_ERROR_CODES.MANIFEST_INTEGRITY,
  FEDERATION_ERROR_CODES.ASSET_INTEGRITY,
  FEDERATION_ERROR_CODES.ASSET_MIME,
  FEDERATION_ERROR_CODES.ASSET_SIZE,
  FEDERATION_ERROR_CODES.TYPE_BUILD_MISMATCH,
])

const JAVASCRIPT_MIMES = new Set([
  'application/javascript',
  'text/javascript',
])
const SOURCE_MAP_MIMES = new Set([
  'application/json',
  'application/source-map+json',
])

const DEV_UPDATE_ACTIONS = new Set(['types-only', 'isolated-remount', 'full-reload'])
const DEV_UPDATE_FIELDS = new Set([
  'schemaVersion',
  'remote',
  'oldBuildId',
  'newBuildId',
  'changedExposes',
  'typesHash',
  'generation',
  'action',
])
const DEV_LEASE_RELOAD_REASONS = new Set(['build-gone', 'invalid-lease', 'lease-limit', 'update-lagged'])
const DEV_LEASE_FIELDS = Object.freeze({
  lease: new Set(['type', 'schemaVersion', 'remote', 'buildIds']),
  'lease-ack': new Set(['type', 'schemaVersion', 'remote', 'buildIds', 'currentBuildId', 'generation']),
  'full-reload': new Set(['type', 'schemaVersion', 'remote', 'currentBuildId', 'generation', 'expiredBuildId', 'reason']),
})
const DEV_CONTROL_HEADERS = Object.freeze({
  schema: 'wake-federation-control',
  action: 'wake-federation-action',
  remote: 'wake-federation-remote',
  currentBuildId: 'wake-federation-current-build-id',
  generation: 'wake-federation-generation',
  expiredBuildId: 'wake-federation-expired-build-id',
  reason: 'wake-federation-reason',
})

export class FederationError extends Error {
  constructor(code, message, options = {}) {
    super(`[${code}] ${message}`, options.cause === undefined ? undefined : { cause: options.cause })
    this.name = 'FederationError'
    this.code = code
    this.phase = options.phase ?? 'runtime'
    this.retryable = options.retryable ?? !NON_RETRYABLE_CODES.has(code)
    this.details = Object.freeze({ ...(options.details ?? {}) })
  }

  toJSON() {
    return {
      name: this.name,
      code: this.code,
      message: this.message,
      phase: this.phase,
      retryable: this.retryable,
      details: this.details,
    }
  }
}

function fail(code, message, options) {
  throw new FederationError(code, message, options)
}

function boundedPositiveInteger(value, field, maximum, phase) {
  if (!Number.isSafeInteger(value) || value <= 0 || value > maximum) {
    fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, `${field} must be a positive safe integer no greater than ${maximum}`, {
      phase,
      retryable: false,
      details: { field, actual: value, minimum: 1, maximum },
    })
  }
  return value
}

function federationMode(value, phase) {
  if (value !== 'development' && value !== 'production') {
    fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Federation mode must be development or production', {
      phase,
      retryable: false,
      details: { field: 'mode', actual: value, allowed: ['development', 'production'] },
    })
  }
  return value
}

function isRecord(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

function hasOwn(value, key) {
  return Object.prototype.hasOwnProperty.call(value, key)
}

function isValidContainerName(value) {
  return typeof value === 'string' && value.length <= 64 && /^[A-Za-z][A-Za-z0-9_-]*$/u.test(value)
}

function isValidIdentityToken(value) {
  return typeof value === 'string' && value.length > 0 && value.length <= 256 &&
    /^[!-~]+$/u.test(value) && !value.includes('\\')
}

function isValidExposeKey(value) {
  if (typeof value !== 'string' || !value.startsWith('./') || value.length > 256) return false
  const path = value.slice(2)
  return path.length > 0 && /^[A-Za-z0-9/@_.-]+$/u.test(path) &&
    path.split('/').every((segment) => segment.length > 0 && segment !== '.' && segment !== '..')
}

function isValidShareScope(value) {
  return typeof value === 'string' && value.length > 0 && value.length <= 128 &&
    /^[A-Za-z0-9_.@/-]+$/u.test(value) &&
    value.split('/').every((segment) => segment.length > 0 && segment !== '.' && segment !== '..')
}

function normalizeDevUpdate(value) {
  const invalid = (message, details = {}) => fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, message, {
    phase: 'dev-update',
    retryable: false,
    details,
  })
  if (!isRecord(value)) invalid('Federation dev update must be an object')
  const unknownFields = Object.keys(value).filter((field) => !DEV_UPDATE_FIELDS.has(field)).sort()
  if (unknownFields.length > 0) invalid('Federation dev update contains unknown fields', { unknownFields })
  if (value.schemaVersion !== FEDERATION_DEV_UPDATE_SCHEMA) {
    invalid('Unsupported federation dev update schema', {
      expected: FEDERATION_DEV_UPDATE_SCHEMA,
      actual: value.schemaVersion,
    })
  }
  if (!isValidContainerName(value.remote)) invalid('Federation dev update has an invalid remote name', { remote: value.remote })
  if (value.oldBuildId !== undefined && value.oldBuildId !== null && !isValidIdentityToken(value.oldBuildId)) {
    invalid('Federation dev update has an invalid oldBuildId', { oldBuildId: value.oldBuildId })
  }
  if (!isValidIdentityToken(value.newBuildId)) invalid('Federation dev update has an invalid newBuildId', { newBuildId: value.newBuildId })
  if (!Array.isArray(value.changedExposes) || !value.changedExposes.every(isValidExposeKey)) {
    invalid('Federation dev update has invalid changedExposes', { changedExposes: value.changedExposes })
  }
  if (value.typesHash !== undefined && value.typesHash !== null && !isValidIdentityToken(value.typesHash)) {
    invalid('Federation dev update has an invalid typesHash', { typesHash: value.typesHash })
  }
  if (!Number.isSafeInteger(value.generation) || value.generation < 0) {
    invalid('Federation dev update generation must be a non-negative safe integer', { generation: value.generation })
  }
  if (!DEV_UPDATE_ACTIONS.has(value.action)) invalid('Federation dev update has an invalid action', { action: value.action })
  return Object.freeze({
    schemaVersion: FEDERATION_DEV_UPDATE_SCHEMA,
    remote: value.remote,
    oldBuildId: value.oldBuildId ?? null,
    newBuildId: value.newBuildId,
    changedExposes: Object.freeze([...new Set(value.changedExposes)].sort()),
    typesHash: value.typesHash ?? null,
    generation: value.generation,
    action: value.action,
  })
}

function normalizeDevLeaseMessage(value) {
  const invalid = (message, details = {}) => fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, message, {
    phase: 'dev-lease',
    retryable: false,
    details,
  })
  if (!isRecord(value)) invalid('Federation dev lease message must be an object')
  const fields = DEV_LEASE_FIELDS[value.type]
  if (fields === undefined) invalid('Federation dev lease message has an invalid type', { type: value.type })
  const unknownFields = Object.keys(value).filter((field) => !fields.has(field)).sort()
  if (unknownFields.length > 0) invalid('Federation dev lease message contains unknown fields', { unknownFields })
  if (value.schemaVersion !== FEDERATION_DEV_LEASE_SCHEMA) {
    invalid('Unsupported federation dev lease schema', {
      expected: FEDERATION_DEV_LEASE_SCHEMA,
      actual: value.schemaVersion,
    })
  }
  if (!isValidContainerName(value.remote)) invalid('Federation dev lease message has an invalid remote', { remote: value.remote })
  if (value.type === 'lease' || value.type === 'lease-ack') {
    if (!Array.isArray(value.buildIds) || value.buildIds.length === 0 ||
        value.buildIds.length > FEDERATION_DEV_MAX_BUILD_LEASES ||
        !value.buildIds.every(isValidIdentityToken) ||
        value.buildIds.some((buildId, index) => index > 0 && value.buildIds[index - 1] >= buildId)) {
      invalid('Federation dev lease buildIds must be a sorted unique bounded set', {
        buildIds: value.buildIds,
        maximum: FEDERATION_DEV_MAX_BUILD_LEASES,
      })
    }
    if (value.type === 'lease') {
      return Object.freeze({
        type: 'lease',
        schemaVersion: FEDERATION_DEV_LEASE_SCHEMA,
        remote: value.remote,
        buildIds: Object.freeze([...value.buildIds]),
      })
    }
  }
  if (!isValidIdentityToken(value.currentBuildId)) {
    invalid('Federation dev lease message has an invalid currentBuildId', { currentBuildId: value.currentBuildId })
  }
  if (!Number.isSafeInteger(value.generation) || value.generation < 0) {
    invalid('Federation dev lease generation must be a non-negative safe integer', { generation: value.generation })
  }
  if (value.type === 'lease-ack') {
    return Object.freeze({
      type: 'lease-ack',
      schemaVersion: FEDERATION_DEV_LEASE_SCHEMA,
      remote: value.remote,
      buildIds: Object.freeze([...value.buildIds]),
      currentBuildId: value.currentBuildId,
      generation: value.generation,
    })
  }
  if (value.expiredBuildId !== null && !isValidIdentityToken(value.expiredBuildId)) {
    invalid('Federation dev full-reload has an invalid expiredBuildId', { expiredBuildId: value.expiredBuildId })
  }
  if (!DEV_LEASE_RELOAD_REASONS.has(value.reason)) {
    invalid('Federation dev full-reload has an invalid reason', { reason: value.reason })
  }
  return Object.freeze({
    type: 'full-reload',
    schemaVersion: FEDERATION_DEV_LEASE_SCHEMA,
    remote: value.remote,
    currentBuildId: value.currentBuildId,
    generation: value.generation,
    expiredBuildId: value.expiredBuildId,
    reason: value.reason,
  })
}

function devFullReloadFromHeaders(response) {
  if (response?.status !== 410 || typeof response.headers?.get !== 'function') return null
  const generation = response.headers.get(DEV_CONTROL_HEADERS.generation)
  const value = {
    type: response.headers.get(DEV_CONTROL_HEADERS.action),
    schemaVersion: response.headers.get(DEV_CONTROL_HEADERS.schema),
    remote: response.headers.get(DEV_CONTROL_HEADERS.remote),
    currentBuildId: response.headers.get(DEV_CONTROL_HEADERS.currentBuildId),
    generation: typeof generation === 'string' && /^(0|[1-9][0-9]*)$/u.test(generation)
      ? Number(generation)
      : Number.NaN,
    expiredBuildId: response.headers.get(DEV_CONTROL_HEADERS.expiredBuildId),
    reason: response.headers.get(DEV_CONTROL_HEADERS.reason),
  }
  try {
    return normalizeDevLeaseMessage(value)
  } catch {
    return null
  }
}

async function devFullReloadFromBody(response) {
  if (response?.status !== 410) return null
  let value
  try {
    if (typeof response.json === 'function') value = await response.json()
    else if (typeof response.text === 'function') value = JSON.parse(await response.text())
    else return null
    return normalizeDevLeaseMessage(value)
  } catch {
    return null
  }
}

function matchesDevelopmentAssetReload(control, context) {
  const identity = context.assetContext
  return identity?.development === true && control?.type === 'full-reload' &&
    control.reason === 'build-gone' && control.remote === identity.name &&
    control.expiredBuildId === identity.buildId && control.currentBuildId !== identity.buildId &&
    control.generation > identity.generation
}

function reloadForDevelopmentAsset(control, context) {
  if (!matchesDevelopmentAssetReload(control, context)) return false
  context.global.location?.reload?.()
  return true
}

function runtimeGeneration(remote, manifest) {
  return remote.mode === 'development' ? manifest.development?.generation ?? 0 : 0
}

function runtimeAssetContext(remote, manifest, expose) {
  return Object.freeze({
    name: manifest.name,
    buildId: manifest.buildId,
    generation: runtimeGeneration(remote, manifest),
    development: remote.mode === 'development',
    ...(expose === undefined ? {} : { expose }),
  })
}

function acceptedDevelopmentCursor(remote) {
  if (remote.mode !== 'development') return null
  const currentBuildId = remote.devBuildId ?? remote.manifest?.buildId ?? null
  const generation = Math.max(
    remote.devGeneration,
    remote.manifest?.development?.generation ?? -1,
  )
  if (!isValidIdentityToken(currentBuildId) || !Number.isSafeInteger(generation) || generation < 0) return null
  return Object.freeze({ currentBuildId, generation })
}

function goneAssetError(asset, response, control) {
  return new FederationError(FEDERATION_ERROR_CODES.NETWORK, `Asset request failed with HTTP ${response.status}`, {
    phase: 'asset-fetch',
    retryable: false,
    details: {
      url: asset.url,
      status: response.status,
      ...(control === null ? {} : {
        remote: control.remote,
        currentBuildId: control.currentBuildId,
        expiredBuildId: control.expiredBuildId,
        generation: control.generation,
        reason: control.reason,
      }),
    },
  })
}

function normalizeFederatedFileName(value) {
  if (typeof value !== 'string' || value.length === 0 || value.length > 1024 ||
      value.includes('\\') || value.includes('?') || value.includes('#')) {
    fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Federated asset fileName must be a relative URL path', {
      phase: 'asset-resolve', retryable: false, details: { fileName: value },
    })
  }
  const normalized = value.replace(/^\.\//u, '').replace(/^\/+/, '')
  if (normalized.length === 0 || /^[A-Za-z][A-Za-z0-9+.-]*:/u.test(normalized) ||
      normalized.split('/').some((segment) => segment.length === 0 || segment === '.' || segment === '..')) {
    fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Federated asset fileName must not be absolute or traverse directories', {
      phase: 'asset-resolve', retryable: false, details: { fileName: value },
    })
  }
  return normalized
}

function manifestAssetRecords(manifest) {
  const records = new Map()
  const add = (asset, expose) => {
    if (asset === undefined) return
    const existing = records.get(asset.url)
    if (existing === undefined) {
      records.set(asset.url, { asset, exposes: new Set(expose === undefined ? [] : [expose]) })
    } else if (expose !== undefined) {
      existing.exposes.add(expose)
    }
  }
  add(manifest.remoteEntry)
  add(manifest.remoteEntrySourceMap)
  for (const [exposeKey, expose] of Object.entries(manifest.exposes)) {
    add(expose.entry, exposeKey)
    add(expose.sourceMap, exposeKey)
    for (const asset of expose.css) add(asset, exposeKey)
    for (const asset of expose.synchronousAssets) add(asset)
    for (const asset of expose.asynchronousAssets) add(asset, exposeKey)
  }
  for (const offer of manifest.shared.offers) add(offer.asset)
  for (const requirement of manifest.shared.requirements) add(requirement.fallback)
  if (manifest.types !== undefined) {
    add(Object.freeze({ ...manifest.types, kind: 'other', mime: 'application/json' }))
  }
  return [...records.values()]
}

function immutableManifestSignature(manifest) {
  return JSON.stringify({
    schemaVersion: manifest.schemaVersion,
    runtimeAbi: manifest.runtimeAbi,
    name: manifest.name,
    buildId: manifest.buildId,
    browserTarget: manifest.browserTarget,
    remoteEntry: manifest.remoteEntry,
    remoteEntrySourceMap: manifest.remoteEntrySourceMap,
    exposes: Object.fromEntries(
      Object.entries(manifest.exposes).sort(([left], [right]) => left.localeCompare(right)),
    ),
    shared: manifest.shared,
    types: manifest.types,
  })
}

function normalizeMime(value) {
  return String(value ?? '').split(';', 1)[0].trim().toLowerCase()
}

function assetMimeMatches(kind, expectedValue, actualValue) {
  const expected = normalizeMime(expectedValue)
  const actual = normalizeMime(actualValue)
  if (kind === 'javascript') return JAVASCRIPT_MIMES.has(expected) && JAVASCRIPT_MIMES.has(actual)
  if (kind === 'source-map') return SOURCE_MAP_MIMES.has(expected) && SOURCE_MAP_MIMES.has(actual)
  if (kind === 'css') return expected === 'text/css' && actual === 'text/css'
  return expected === actual
}

function toBytes(value) {
  if (value instanceof Uint8Array) return value
  if (value instanceof ArrayBuffer) return new Uint8Array(value)
  if (ArrayBuffer.isView(value)) return new Uint8Array(value.buffer, value.byteOffset, value.byteLength)
  if (typeof value === 'string') return new TextEncoder().encode(value)
  return null
}

function responseContentLength(response, context) {
  const raw = response.headers.get('content-length')
  if (raw === null) {
    if (context.requireContentLength === true) {
      fail(FEDERATION_ERROR_CODES.ASSET_SIZE, 'Response Content-Length is required', {
        phase: context.phase,
        retryable: false,
        details: { url: context.url, maximum: context.maximumBytes },
      })
    }
    return null
  }
  const trimmed = raw.trim()
  const length = /^\d+$/u.test(trimmed) ? Number(trimmed) : Number.NaN
  if (!Number.isSafeInteger(length) || length < 0) {
    fail(FEDERATION_ERROR_CODES.ASSET_SIZE, 'Response Content-Length is invalid', {
      phase: context.phase,
      retryable: false,
      details: { url: context.url, actual: raw, maximum: context.maximumBytes },
    })
  }
  const contentEncoding = String(response.headers.get('content-encoding') ?? '').trim().toLowerCase()
  const identityEncoding = contentEncoding === '' || contentEncoding === 'identity'
  if (length > context.maximumBytes ||
      (identityEncoding && context.expectedBytes !== undefined && length !== context.expectedBytes)) {
    fail(FEDERATION_ERROR_CODES.ASSET_SIZE, 'Response Content-Length does not match the allowed size', {
      phase: context.phase,
      retryable: false,
      details: {
        url: context.url,
        ...(context.expectedBytes === undefined ? {} : { expected: context.expectedBytes }),
        actual: length,
        maximum: context.maximumBytes,
        ...(contentEncoding === '' ? {} : { contentEncoding }),
      },
    })
  }
  return length
}

async function readBoundedResponseBytes(response, context) {
  responseContentLength(response, context)
  const body = response.body
  if (body === null) {
    if (context.expectedBytes !== undefined && context.expectedBytes !== 0) {
      fail(FEDERATION_ERROR_CODES.ASSET_SIZE, 'Response body size does not match the manifest', {
        phase: context.phase,
        retryable: false,
        details: { url: context.url, expected: context.expectedBytes, actual: 0 },
      })
    }
    return new Uint8Array()
  }
  if (typeof body?.getReader !== 'function') {
    fail(FEDERATION_ERROR_CODES.UNSUPPORTED_ENVIRONMENT, 'Streaming response bodies are required for bounded federation reads', {
      phase: context.phase,
      retryable: false,
      details: { url: context.url },
    })
  }

  const reader = body.getReader()
  const chunks = []
  let total = 0
  try {
    while (true) {
      const { done, value } = await reader.read()
      if (done) break
      const bytes = toBytes(value)
      if (bytes === null) {
        try { await reader.cancel('invalid federation response chunk') } catch {}
        fail(FEDERATION_ERROR_CODES.UNSUPPORTED_ENVIRONMENT, 'Federation response stream yielded a non-byte chunk', {
          phase: context.phase,
          retryable: false,
          details: { url: context.url },
        })
      }
      total += bytes.byteLength
      if (total > context.maximumBytes ||
          (context.expectedBytes !== undefined && total > context.expectedBytes)) {
        try { await reader.cancel('federation response exceeded its size limit') } catch {}
        fail(FEDERATION_ERROR_CODES.ASSET_SIZE, 'Federation response exceeded its allowed size while streaming', {
          phase: context.phase,
          retryable: false,
          details: {
            url: context.url,
            ...(context.expectedBytes === undefined ? {} : { expected: context.expectedBytes }),
            actual: total,
            maximum: context.maximumBytes,
          },
        })
      }
      if (bytes.byteLength > 0) chunks.push(bytes)
    }
  } finally {
    reader.releaseLock?.()
  }

  if (context.expectedBytes !== undefined && total !== context.expectedBytes) {
    fail(FEDERATION_ERROR_CODES.ASSET_SIZE, 'Response body size does not match the manifest', {
      phase: context.phase,
      retryable: false,
      details: { url: context.url, expected: context.expectedBytes, actual: total },
    })
  }
  const result = new Uint8Array(total)
  let offset = 0
  for (const chunk of chunks) {
    result.set(chunk, offset)
    offset += chunk.byteLength
  }
  return result
}

function bytesToBase64(bytes, targetGlobal) {
  if (typeof targetGlobal.btoa === 'function') {
    let binary = ''
    const chunkSize = 0x8000
    for (let offset = 0; offset < bytes.length; offset += chunkSize) {
      binary += String.fromCharCode(...bytes.subarray(offset, offset + chunkSize))
    }
    return targetGlobal.btoa(binary)
  }
  if (typeof Buffer !== 'undefined') return Buffer.from(bytes).toString('base64')
  fail(FEDERATION_ERROR_CODES.ASSET_INTEGRITY, 'No base64 encoder is available', {
    phase: 'integrity',
    retryable: false,
  })
}

function sha384Digest(integrity) {
  return typeof integrity === 'string' && /^sha384-[A-Za-z0-9+/]{64}$/u.test(integrity)
    ? integrity
    : null
}

async function verifyIntegrity(bytes, integrity, targetGlobal, code, details) {
  const expected = sha384Digest(integrity)
  if (expected === null) {
    fail(code, 'A SHA-384 integrity value is required', {
      phase: 'integrity',
      retryable: false,
      details,
    })
  }
  const subtle = targetGlobal.crypto?.subtle ?? globalThis.crypto?.subtle
  if (subtle === undefined) {
    fail(code, 'Web Crypto is required to verify integrity', {
      phase: 'integrity',
      retryable: false,
      details,
    })
  }
  const digest = new Uint8Array(await subtle.digest('SHA-384', bytes))
  const actual = `sha384-${bytesToBase64(digest, targetGlobal)}`
  if (actual !== expected) {
    fail(code, 'SHA-384 integrity verification failed', {
      phase: 'integrity',
      retryable: false,
      details: { ...details, expected, actual },
    })
  }
}

function freezeDecision(value) {
  if (Array.isArray(value)) return Object.freeze(value.map(freezeDecision))
  if (!isRecord(value)) return value
  const clone = {}
  for (const [key, child] of Object.entries(value)) clone[key] = freezeDecision(child)
  return Object.freeze(clone)
}

function parseVersion(value) {
  const input = String(value ?? '').trim()
  const match = /^(?:v)?(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/u.exec(input)
  if (match === null) return null
  return {
    major: Number(match[1]),
    minor: Number(match[2]),
    patch: Number(match[3]),
    prerelease: match[4] === undefined ? [] : match[4].split('.'),
    raw: input,
  }
}

function compareIdentifiers(left, right) {
  const leftNumeric = /^\d+$/u.test(left)
  const rightNumeric = /^\d+$/u.test(right)
  if (leftNumeric && rightNumeric) return Number(left) - Number(right)
  if (leftNumeric) return -1
  if (rightNumeric) return 1
  return left < right ? -1 : left > right ? 1 : 0
}

function compareParsedVersions(left, right) {
  for (const field of ['major', 'minor', 'patch']) {
    if (left[field] !== right[field]) return left[field] - right[field]
  }
  if (left.prerelease.length === 0 && right.prerelease.length === 0) return 0
  if (left.prerelease.length === 0) return 1
  if (right.prerelease.length === 0) return -1
  const count = Math.max(left.prerelease.length, right.prerelease.length)
  for (let index = 0; index < count; index += 1) {
    if (left.prerelease[index] === undefined) return -1
    if (right.prerelease[index] === undefined) return 1
    const compared = compareIdentifiers(left.prerelease[index], right.prerelease[index])
    if (compared !== 0) return compared
  }
  return 0
}

function partialVersion(value) {
  const input = String(value).trim().replace(/^v/u, '')
  const match = /^(\d+|[xX*])(?:\.(\d+|[xX*]))?(?:\.(\d+|[xX*]))?(?:-([0-9A-Za-z.-]+))?$/u.exec(input)
  if (match === null) return null
  const parts = match.slice(1, 4)
  const wildcardIndex = parts.findIndex((part) => part === undefined || /^[xX*]$/u.test(part))
  if (wildcardIndex >= 0 && parts.slice(wildcardIndex + 1).some((part) => part !== undefined && !/^[xX*]$/u.test(part))) {
    return null
  }
  const precision = wildcardIndex === -1 ? 3 : wildcardIndex
  if (precision < 3 && match[4] !== undefined) return null
  return {
    major: precision >= 1 ? Number(parts[0]) : 0,
    minor: precision >= 2 ? Number(parts[1]) : 0,
    patch: precision >= 3 ? Number(parts[2]) : 0,
    prerelease: match[4] === undefined ? [] : match[4].split('.'),
    precision,
  }
}

function upperForPartial(version) {
  if (version.precision <= 1) return { major: version.major + 1, minor: 0, patch: 0, prerelease: [] }
  return { major: version.major, minor: version.minor + 1, patch: 0, prerelease: [] }
}

function comparator(operator, version) {
  return { operator, version }
}

function expandRangeToken(token) {
  if (token === '' || token === '*' || /^x$/iu.test(token)) return []
  const operatorMatch = /^(\^|~|>=|<=|>|<|=)?(.+)$/u.exec(token)
  if (operatorMatch === null) return null
  const operator = operatorMatch[1] ?? ''
  const version = partialVersion(operatorMatch[2])
  if (version === null) return null
  const lower = { ...version }

  if (operator === '^') {
    let upper
    if (version.major > 0 || version.precision === 1) {
      upper = { major: version.major + 1, minor: 0, patch: 0, prerelease: [] }
    } else if (version.minor > 0 || version.precision === 2) {
      upper = { major: 0, minor: version.minor + 1, patch: 0, prerelease: [] }
    } else {
      upper = { major: 0, minor: 0, patch: version.patch + 1, prerelease: [] }
    }
    return [comparator('>=', lower), comparator('<', upper)]
  }
  if (operator === '~') {
    return [comparator('>=', lower), comparator('<', upperForPartial(version))]
  }
  if (operator !== '' && version.precision < 3) {
    if (operator === '>') return [comparator('>=', upperForPartial(version))]
    if (operator === '<=') return [comparator('<', upperForPartial(version))]
    return [comparator(operator, lower)]
  }
  if (operator === '' && version.precision < 3) {
    return [comparator('>=', lower), comparator('<', upperForPartial(version))]
  }
  return [comparator(operator === '' ? '=' : operator, lower)]
}

function parseComparatorSet(value) {
  const input = value.trim()
  if (input === '' || input === '*') return []
  const hyphen = /^(\S+)\s+-\s+(\S+)$/u.exec(input)
  if (hyphen !== null) {
    const lower = partialVersion(hyphen[1])
    const upper = partialVersion(hyphen[2])
    if (lower === null || upper === null) return null
    const upperComparator = upper.precision < 3
      ? comparator('<', upperForPartial(upper))
      : comparator('<=', upper)
    return [comparator('>=', lower), upperComparator]
  }
  const comparators = []
  for (const token of input.split(/\s+/u)) {
    const expanded = expandRangeToken(token)
    if (expanded === null) return null
    comparators.push(...expanded)
  }
  return comparators
}

function testComparator(version, entry) {
  const compared = compareParsedVersions(version, entry.version)
  if (entry.operator === '=') return compared === 0
  if (entry.operator === '>') return compared > 0
  if (entry.operator === '>=') return compared >= 0
  if (entry.operator === '<') return compared < 0
  if (entry.operator === '<=') return compared <= 0
  return false
}

function satisfiesRange(versionValue, rangeValue) {
  const version = parseVersion(versionValue)
  if (version === null) return false
  const range = String(rangeValue ?? '*').trim()
  for (const alternative of range.split('||')) {
    if (alternative.trim() === '') continue
    const comparators = parseComparatorSet(alternative)
    if (comparators === null) continue
    if (version.prerelease.length > 0) {
      const permitsPrerelease = comparators.some((entry) =>
        entry.version.prerelease.length > 0 &&
        entry.version.major === version.major &&
        entry.version.minor === version.minor &&
        entry.version.patch === version.patch)
      if (!permitsPrerelease) continue
    }
    if (comparators.every((entry) => testComparator(version, entry))) return true
  }
  return false
}

function normalizeExpose(specifier) {
  return specifier.startsWith('./') ? specifier : `./${specifier}`
}

function parseRemoteSpecifier(specifier) {
  if (typeof specifier !== 'string') {
    fail(FEDERATION_ERROR_CODES.INVALID_SPECIFIER, 'A remote specifier must be a string', {
      phase: 'resolve',
      retryable: false,
      details: { specifierType: typeof specifier },
    })
  }
  const separator = specifier.indexOf('/')
  if (separator <= 0 || separator === specifier.length - 1 || specifier.startsWith('./')) {
    fail(FEDERATION_ERROR_CODES.INVALID_SPECIFIER, `Invalid remote specifier ${JSON.stringify(specifier)}`, {
      phase: 'resolve',
      retryable: false,
      details: { specifier },
    })
  }
  return { remote: specifier.slice(0, separator), expose: normalizeExpose(specifier.slice(separator + 1)) }
}

function containerKey(name, buildId) {
  return `${name}\0${buildId}`
}

function shareBucketKey(scope, shareKey) {
  return `${scope}\0${shareKey}`
}

function coherenceBucketKey(scope, coherenceGroup) {
  return `${scope}\0${coherenceGroup}`
}

function providerIdentity(provider) {
  return [
    provider.ownerKey,
    provider.shareKey,
    provider.version,
    provider.packageContext,
    provider.buildVariant,
  ].join('\0')
}

function sharedProviderPriority(provider, request, context) {
  if (provider.host) return 0
  if (provider.loaded) return 1
  if ((provider.ownerKey === context.currentRemoteKey ||
       request.owner === provider.ownerName || request.owner === provider.ownerKey) &&
      provider.fallback && request.fallback) return 2
  return 3
}

function sharedCandidateDiagnostics(providers, request, context, coherence) {
  const sorted = [...providers].sort((left, right) => {
    const version = compareParsedVersions(right.parsedVersion, left.parsedVersion)
    if (version !== 0) return version
    const identity = providerIdentity(left).localeCompare(providerIdentity(right))
    return identity !== 0 ? identity : left.sequence - right.sequence
  })
  return Object.freeze(sorted.map((provider) => {
    const rejections = []
    const reject = (code, details = {}) => rejections.push(Object.freeze({ code, ...details }))
    if (request.packageContext !== undefined && request.packageContext !== provider.packageContext) {
      reject('package-context-mismatch', { expected: request.packageContext, actual: provider.packageContext })
    }
    if (request.buildVariant !== undefined && request.buildVariant !== provider.buildVariant) {
      reject('build-variant-mismatch', { expected: request.buildVariant, actual: provider.buildVariant })
    }
    if (request.owner !== undefined && request.owner !== provider.ownerName && request.owner !== provider.ownerKey) {
      reject('owner-mismatch', { expected: request.owner, actual: provider.ownerKey })
    }
    if (context.forcedOwner !== undefined && context.forcedOwner !== provider.ownerKey) {
      reject('coherence-plan-owner-mismatch', { expected: context.forcedOwner, actual: provider.ownerKey })
    }
    if (!satisfiesRange(provider.version, request.requiredVersion)) {
      reject('version-mismatch', { expected: request.requiredVersion, actual: provider.version })
    }
    if (coherence !== undefined && coherence.ownerKey !== provider.ownerKey) {
      reject('coherence-lock-owner-mismatch', { expected: coherence.ownerKey, actual: provider.ownerKey })
    }
    const priority = sharedProviderPriority(provider, request, context)
    if (priority === 3) {
      if (!provider.fallback) reject('provider-fallback-disabled')
      else if (!request.fallback) reject('request-fallback-disabled')
      else reject('provider-not-loaded-or-owned', { owner: provider.ownerKey })
    }
    return Object.freeze({
      version: provider.version,
      owner: provider.ownerKey,
      packageContext: provider.packageContext,
      buildVariant: provider.buildVariant,
      source: priority === 0 ? 'host' : priority === 1 ? 'loaded' : priority === 2 ? 'fallback' : 'unavailable',
      eligible: rejections.length === 0,
      rejections: Object.freeze(rejections),
    })
  }))
}

function assertBrowserGlobal(targetGlobal) {
  if (targetGlobal?.window !== targetGlobal || targetGlobal.document === undefined) {
    fail(FEDERATION_ERROR_CODES.UNSUPPORTED_ENVIRONMENT, 'Wake Federation is available only in a browser Window', {
      phase: 'environment',
      retryable: false,
    })
  }
}

async function preflightAsset(asset, context) {
  // This HEAD request is a metadata gate, not proof of the native GET's final origin. Successful
  // identity transfers require the exact decoded size; encoded transfers have a distinct wire
  // length and are bounded instead. The browser's SHA-384 SRI check binds the executed content,
  // while deployment CSP/CORS must independently constrain the executable origins.
  const response = await context.global.fetch(asset.url, {
    cache: 'default',
    credentials: 'omit',
    method: 'HEAD',
    mode: 'cors',
    redirect: 'error',
    signal: context.signal,
  })
  if (!response.ok) {
    if (response.status === 410) {
      const control = devFullReloadFromHeaders(response)
      const matched = reloadForDevelopmentAsset(control, context)
      throw goneAssetError(asset, response, matched ? control : null)
    }
    fail(FEDERATION_ERROR_CODES.NETWORK, `Asset request failed with HTTP ${response.status}`, {
      phase: 'asset-fetch',
      retryable: response.status >= 500 || response.status === 408 || response.status === 429,
      details: { url: asset.url, status: response.status },
    })
  }
  const contentType = normalizeMime(response.headers.get('content-type'))
  if (!assetMimeMatches(asset.kind, asset.mime, contentType)) {
    fail(FEDERATION_ERROR_CODES.ASSET_MIME, 'Asset response MIME does not match the manifest', {
      phase: 'asset-fetch',
      retryable: false,
      details: { url: asset.url, expected: asset.mime, actual: contentType },
    })
  }
  responseContentLength(response, {
    phase: 'asset-fetch',
    url: asset.url,
    maximumBytes: context.maxAssetSize,
    expectedBytes: asset.size,
    requireContentLength: true,
  })
}

async function diagnoseNativeAssetFailure(asset, context, fallbackError) {
  let response
  try {
    response = await context.global.fetch(asset.url, {
      cache: 'no-store',
      credentials: 'omit',
      method: 'GET',
      mode: 'cors',
      redirect: 'error',
      signal: context.signal,
    })
  } catch {
    throw fallbackError
  }
  if (!response.ok) {
    if (response.status === 410) {
      const control = await devFullReloadFromBody(response)
      const matched = reloadForDevelopmentAsset(control, context)
      throw goneAssetError(asset, response, matched ? control : null)
    }
    throw fallbackError
  }
  const contentType = normalizeMime(response.headers.get('content-type'))
  if (!assetMimeMatches(asset.kind, asset.mime, contentType)) {
    fail(FEDERATION_ERROR_CODES.ASSET_MIME, 'Failed native asset response MIME does not match the manifest', {
      phase: 'asset-diagnose',
      retryable: false,
      details: { url: asset.url, expected: asset.mime, actual: contentType },
    })
  }
  let bytes
  try {
    bytes = await readBoundedResponseBytes(response, {
      phase: 'asset-diagnose',
      url: asset.url,
      maximumBytes: Math.min(asset.size, context.maxAssetSize),
      expectedBytes: asset.size,
    })
  } catch (error) {
    if (error instanceof FederationError) throw error
    throw fallbackError
  }
  await verifyIntegrity(bytes, asset.integrity, context.global, FEDERATION_ERROR_CODES.ASSET_INTEGRITY, {
    url: asset.url,
    nativeLoadFailed: true,
  })
  throw fallbackError
}

function installAssetExecutionContext(targetGlobal, assetUrl, value) {
  let contexts = targetGlobal[ASSET_CONTEXTS_SYMBOL]
  if (contexts === undefined) {
    contexts = new Map()
    Object.defineProperty(targetGlobal, ASSET_CONTEXTS_SYMBOL, {
      configurable: false,
      enumerable: false,
      writable: false,
      value: contexts,
    })
  }
  if (!(contexts instanceof Map)) {
    fail(FEDERATION_ERROR_CODES.RUNTIME_ABI, 'Federation asset context registry has an incompatible owner', {
      phase: 'asset-load', retryable: false, details: { url: assetUrl },
    })
  }
  const executionContext = Object.freeze({
    name: value.name,
    buildId: value.buildId,
    generation: value.generation,
    ...(value.expose === undefined ? {} : { expose: value.expose }),
  })
  const existing = contexts.get(assetUrl)
  if (existing !== undefined &&
      (existing.name !== executionContext.name || existing.buildId !== executionContext.buildId ||
       existing.generation !== executionContext.generation || existing.expose !== executionContext.expose)) {
    fail(FEDERATION_ERROR_CODES.REMOTE_CONFLICT, 'Federation asset URL is already owned by another execution context', {
      phase: 'asset-load', retryable: false,
      details: { url: assetUrl, existing, requested: executionContext },
    })
  }
  if (existing === undefined) contexts.set(assetUrl, executionContext)
  return existing ?? executionContext
}

function createBrowserTransport(targetGlobal, options) {
  return Object.freeze({
    async fetchManifest(url, context) {
      const response = await targetGlobal.fetch(url, {
        cache: context.mode === 'production' ? 'no-cache' : 'no-store',
        credentials: 'omit',
        mode: 'cors',
        redirect: 'error',
        signal: context.signal,
      })
      if (!response.ok) {
        fail(FEDERATION_ERROR_CODES.MANIFEST_FETCH, `Manifest request failed with HTTP ${response.status}`, {
          phase: 'manifest-fetch',
          retryable: response.status >= 500 || response.status === 408 || response.status === 429,
          details: { url, status: response.status },
        })
      }
      const contentType = normalizeMime(response.headers.get('content-type'))
      if (contentType !== 'application/json' && contentType !== 'application/federation+json') {
        fail(FEDERATION_ERROR_CODES.ASSET_MIME, 'Federation manifest must be served as JSON', {
          phase: 'manifest-fetch',
          retryable: false,
          details: { url, actual: contentType },
        })
      }
      const rawBytes = await readBoundedResponseBytes(response, {
        phase: 'manifest-fetch',
        url,
        maximumBytes: context.maxManifestSize,
      })
      let manifest
      try {
        manifest = JSON.parse(new TextDecoder().decode(rawBytes))
      } catch (cause) {
        fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'Federation manifest is not valid JSON', {
          phase: 'manifest-validate',
          retryable: false,
          cause,
          details: { url },
        })
      }
      return { manifest, rawBytes, contentType }
    },

    async loadScript(asset, context) {
      await preflightAsset(asset, {
        global: targetGlobal,
        signal: context.signal,
        maxAssetSize: context.maxAssetSize,
        assetContext: context.assetContext,
      })
      installAssetExecutionContext(targetGlobal, asset.url, context.assetContext)
      await new Promise((resolve, reject) => {
        const script = targetGlobal.document.createElement('script')
        script.type = 'module'
        script.async = true
        script.src = asset.url
        script.integrity = asset.integrity
        script.crossOrigin = 'anonymous'
        if (options.nonce !== undefined) script.nonce = options.nonce
        const cleanup = () => {
          script.onload = null
          script.onerror = null
          context.signal?.removeEventListener('abort', onAbort)
        }
        const onAbort = () => {
          cleanup()
          script.remove()
          reject(new FederationError(FEDERATION_ERROR_CODES.TIMEOUT, 'Remote entry loading was aborted', {
            phase: 'entry-load',
            retryable: true,
            details: { url: asset.url },
          }))
        }
        script.onload = () => {
          cleanup()
          resolve()
        }
        script.onerror = (cause) => {
          cleanup()
          script.remove()
          const fallbackError = new FederationError(FEDERATION_ERROR_CODES.NETWORK, 'Remote entry script failed to load', {
            phase: 'entry-load',
            retryable: true,
            cause,
            details: { url: asset.url },
          })
          void diagnoseNativeAssetFailure(asset, {
            global: targetGlobal,
            signal: context.signal,
            maxAssetSize: context.maxAssetSize,
            assetContext: context.assetContext,
          }, fallbackError).catch(reject)
        }
        context.signal?.addEventListener('abort', onAbort, { once: true })
        targetGlobal.document.head.append(script)
      })
    },

    async loadStyle(asset, context) {
      await preflightAsset(asset, {
        global: targetGlobal,
        signal: context.signal,
        maxAssetSize: context.maxAssetSize,
        assetContext: context.assetContext,
      })
      const target = context.styleTarget ?? targetGlobal.document.head
      if (target === null || typeof target?.append !== 'function') {
        fail(FEDERATION_ERROR_CODES.STYLE_LOAD, 'Remote stylesheet target is unavailable', {
          phase: 'style-load', retryable: false, details: { url: asset.url },
        })
      }
      return new Promise((resolve, reject) => {
        const link = targetGlobal.document.createElement('link')
        link.rel = 'stylesheet'
        link.href = asset.url
        link.integrity = asset.integrity
        link.crossOrigin = 'anonymous'
        if (options.nonce !== undefined) link.nonce = options.nonce
        const cleanup = () => {
          link.onload = null
          link.onerror = null
          context.signal?.removeEventListener('abort', onAbort)
        }
        const onAbort = () => {
          cleanup()
          link.remove()
          reject(new FederationError(FEDERATION_ERROR_CODES.TIMEOUT, 'Remote stylesheet loading was aborted', {
            phase: 'style-load', retryable: true, details: { url: asset.url },
          }))
        }
        link.onload = () => {
          cleanup()
          resolve(link)
        }
        link.onerror = (cause) => {
          cleanup()
          link.remove()
          const fallbackError = new FederationError(FEDERATION_ERROR_CODES.STYLE_LOAD, 'Remote stylesheet failed to load', {
            phase: 'style-load',
            retryable: true,
            cause,
            details: { url: asset.url },
          })
          void diagnoseNativeAssetFailure(asset, {
            global: targetGlobal,
            signal: context.signal,
            maxAssetSize: context.maxAssetSize,
            assetContext: context.assetContext,
          }, fallbackError).catch(reject)
        }
        context.signal?.addEventListener('abort', onAbort, { once: true })
        target.append(link)
      })
    },
  })
}

function normalizeRemoteRegistration(nameOrDefinition, definition) {
  let value
  if (typeof nameOrDefinition === 'string') {
    value = typeof definition === 'string'
      ? { name: nameOrDefinition, manifestUrl: definition }
      : { ...(definition ?? {}), name: nameOrDefinition }
  } else {
    value = { ...(nameOrDefinition ?? {}) }
  }
  if (!isValidContainerName(value.name)) {
    fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Remote name must be a non-empty URL-safe identifier', {
      phase: 'remote-register', retryable: false, details: { name: value.name },
    })
  }
  if (typeof value.manifestUrl !== 'string' || value.manifestUrl.length === 0) {
    fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Remote manifestUrl is required', {
      phase: 'remote-register', retryable: false, details: { name: value.name },
    })
  }
  return value
}

function normalizeLockAssets(lock, manifestUrl) {
  const values = lock?.allowedAssets ?? lock?.assets ?? lock?.assetClosure
  if (values === undefined) return null
  const result = new Map()
  const entries = Array.isArray(values)
    ? values.map((asset) => [asset.url, asset.integrity])
    : Object.entries(values)
  for (const [url, integrity] of entries) result.set(new URL(url, manifestUrl).href, integrity)
  return result
}

function assertNonEmptyString(value, path) {
  if (typeof value !== 'string' || value.length === 0) {
    fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `${path} must be a non-empty string`, {
      phase: 'manifest-validate', retryable: false, details: { path },
    })
  }
  return value
}

function assertKnownFields(value, path, fields) {
  const allowed = new Set(fields)
  const unknownFields = Object.keys(value).filter((field) => !allowed.has(field)).sort()
  if (unknownFields.length > 0) {
    fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `${path} contains unknown fields`, {
      phase: 'manifest-validate', retryable: false, details: { path, unknownFields },
    })
  }
}

function validateShareEntries(value, path) {
  if (!Array.isArray(value)) {
    fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `${path} must be an array`, {
      phase: 'manifest-validate', retryable: false, details: { path },
    })
  }
  return value.map((entry, index) => {
    if (!isRecord(entry)) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `${path}[${index}] must be an object`, {
        phase: 'manifest-validate', retryable: false, details: { path: `${path}[${index}]` },
      })
    }
    return Object.freeze({ ...entry })
  })
}

function normalizeManifest(rawManifest, remote) {
  if (!isRecord(rawManifest)) {
    fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'Federation manifest must be an object', {
      phase: 'manifest-validate', retryable: false,
    })
  }
  assertKnownFields(rawManifest, 'manifest', [
    'schemaVersion', 'runtimeAbi', 'name', 'buildId', 'browserTarget', 'remoteEntry',
    'remoteEntrySourceMap', 'exposes', 'shared', 'types', 'development',
  ])
  if (rawManifest.schemaVersion !== FEDERATION_MANIFEST_SCHEMA) {
    fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'Unsupported federation manifest schema', {
      phase: 'manifest-validate', retryable: false,
      details: { expected: FEDERATION_MANIFEST_SCHEMA, actual: rawManifest.schemaVersion },
    })
  }
  if (rawManifest.runtimeAbi !== FEDERATION_RUNTIME_ABI) {
    fail(FEDERATION_ERROR_CODES.RUNTIME_ABI, 'Remote runtime ABI is incompatible', {
      phase: 'manifest-validate', retryable: false,
      details: { expected: FEDERATION_RUNTIME_ABI, actual: rawManifest.runtimeAbi },
    })
  }
  if (rawManifest.name !== remote.name) {
    fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'Manifest name does not match the registered remote', {
      phase: 'manifest-validate', retryable: false,
      details: { expected: remote.name, actual: rawManifest.name },
    })
  }
  const buildId = assertNonEmptyString(rawManifest.buildId, 'buildId')
  if (!isValidIdentityToken(buildId)) {
    fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'buildId must be a stable identity token', {
      phase: 'manifest-validate', retryable: false, details: { path: 'buildId' },
    })
  }
  assertNonEmptyString(rawManifest.browserTarget, 'browserTarget')
  if (remote.expectedBuildId !== undefined && buildId !== remote.expectedBuildId) {
    fail(FEDERATION_ERROR_CODES.LOCK_MISMATCH, 'Manifest buildId does not match the production lock', {
      phase: 'manifest-validate', retryable: false,
      details: { expected: remote.expectedBuildId, actual: buildId },
    })
  }

  const seenAssets = new Map()
  const normalizeAsset = (rawAsset, path, expectedKind) => {
    if (!isRecord(rawAsset)) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `${path} must be an asset object`, {
        phase: 'manifest-validate', retryable: false, details: { path },
      })
    }
    assertKnownFields(rawAsset, path, ['kind', 'url', 'contentHash', 'integrity', 'mime', 'size'])
    const kind = assertNonEmptyString(rawAsset.kind, `${path}.kind`)
    if (expectedKind !== undefined && kind !== expectedKind) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `${path}.kind must be ${expectedKind}`, {
        phase: 'manifest-validate', retryable: false, details: { path, expected: expectedKind, actual: kind },
      })
    }
    let url
    try {
      url = new URL(assertNonEmptyString(rawAsset.url, `${path}.url`), remote.manifestUrl).href
    } catch (cause) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `${path}.url is invalid`, {
        phase: 'manifest-validate', retryable: false, cause, details: { path },
      })
    }
    remote.assertUrl(url, path)
    const contentHash = assertNonEmptyString(rawAsset.contentHash, `${path}.contentHash`)
    const integrity = assertNonEmptyString(rawAsset.integrity, `${path}.integrity`)
    if (sha384Digest(integrity) === null) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `${path}.integrity must contain SHA-384`, {
        phase: 'manifest-validate', retryable: false, details: { path },
      })
    }
    if (!Number.isSafeInteger(rawAsset.size) || rawAsset.size < 0 || rawAsset.size > remote.maxAssetSize) {
      fail(FEDERATION_ERROR_CODES.ASSET_SIZE, `${path}.size exceeds the allowed range`, {
        phase: 'manifest-validate', retryable: false,
        details: { path, actual: rawAsset.size, maximum: remote.maxAssetSize },
      })
    }
    const mime = normalizeMime(assertNonEmptyString(rawAsset.mime, `${path}.mime`))
    if (!['javascript', 'css', 'source-map', 'other'].includes(kind)) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `${path}.kind is not supported`, {
        phase: 'manifest-validate', retryable: false, details: { path, kind },
      })
    }
    const mimeMatches = kind === 'javascript'
      ? JAVASCRIPT_MIMES.has(mime)
      : kind === 'css'
        ? mime === 'text/css'
        : kind === 'source-map'
          ? mime === 'application/json' || mime === 'application/source-map+json'
          : mime.length > 0
    if (!mimeMatches) {
      fail(FEDERATION_ERROR_CODES.ASSET_MIME, `${path}.mime is incompatible with ${kind}`, {
        phase: 'manifest-validate', retryable: false, details: { path, kind, mime },
      })
    }
    if (remote.lockAssets !== null) {
      const lockedIntegrity = remote.lockAssets.get(url)
      if (lockedIntegrity !== integrity) {
        fail(FEDERATION_ERROR_CODES.ASSET_INTEGRITY, 'Manifest asset is outside the production lock closure', {
          phase: 'manifest-validate', retryable: false,
          details: { path, url, expected: lockedIntegrity, actual: integrity },
        })
      }
    }
    const identity = JSON.stringify({ kind, contentHash, integrity, size: rawAsset.size, mime })
    const previousIdentity = seenAssets.get(url)
    if (previousIdentity !== undefined && previousIdentity !== identity) {
      fail(FEDERATION_ERROR_CODES.ASSET_INTEGRITY, 'One asset URL has conflicting manifest metadata', {
        phase: 'manifest-validate', retryable: false, details: { path, url },
      })
    }
    seenAssets.set(url, identity)
    return Object.freeze({ kind, url, contentHash, integrity, size: rawAsset.size, mime })
  }

  const remoteEntry = normalizeAsset(rawManifest.remoteEntry, 'remoteEntry', 'javascript')
  const remoteEntrySourceMap = rawManifest.remoteEntrySourceMap === undefined || rawManifest.remoteEntrySourceMap === null
    ? undefined
    : normalizeAsset(rawManifest.remoteEntrySourceMap, 'remoteEntrySourceMap', 'source-map')
  if (!isRecord(rawManifest.exposes)) {
    fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'exposes must be an object', {
      phase: 'manifest-validate', retryable: false, details: { path: 'exposes' },
    })
  }
  const exposes = {}
  for (const [key, rawExpose] of Object.entries(rawManifest.exposes)) {
    if (!isValidExposeKey(key) || !isRecord(rawExpose)) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'Expose keys must use the ./Name form', {
        phase: 'manifest-validate', retryable: false, details: { expose: key },
      })
    }
    assertKnownFields(rawExpose, `exposes.${key}`, [
      'mode', 'scope', 'shadow', 'entry', 'css', 'sourceMap',
      'synchronousAssets', 'asynchronousAssets',
    ])
    if (!['generic', 'host-rendered', 'isolated'].includes(rawExpose.mode)) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `Expose ${key} has an invalid mode`, {
        phase: 'manifest-validate', retryable: false, details: { expose: key, mode: rawExpose.mode },
      })
    }
    const scope = rawExpose.scope
    if (!isValidShareScope(scope)) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `Expose ${key} has an invalid share scope`, {
        phase: 'manifest-validate', retryable: false,
        details: { path: `exposes.${key}.scope`, scope },
      })
    }
    const expectedShadow = rawExpose.mode === 'isolated' ? 'open' : 'none'
    if (rawExpose.shadow !== expectedShadow) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `Expose ${key} must use shadow=${expectedShadow}`, {
        phase: 'manifest-validate', retryable: false,
        details: { expose: key, mode: rawExpose.mode, expected: expectedShadow, actual: rawExpose.shadow },
      })
    }
    if (rawExpose.mode === 'isolated' && scope === DEFAULT_SCOPE) {
      fail(FEDERATION_ERROR_CODES.COHERENCE_CONFLICT, `Expose ${key} requires a non-default share scope`, {
        phase: 'manifest-validate', retryable: false,
        details: { path: `exposes.${key}.scope`, expose: key, scope },
      })
    }
    const mapAssets = (value, path, kind) => {
      if (!Array.isArray(value)) {
        fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `${path} must be an array`, {
          phase: 'manifest-validate', retryable: false, details: { path },
        })
      }
      return Object.freeze(value.map((asset, index) => normalizeAsset(asset, `${path}[${index}]`, kind)))
    }
    exposes[key] = Object.freeze({
      mode: rawExpose.mode,
      scope,
      shadow: rawExpose.shadow,
      entry: normalizeAsset(rawExpose.entry, `exposes.${key}.entry`, 'javascript'),
      css: mapAssets(rawExpose.css, `exposes.${key}.css`, 'css'),
      sourceMap: rawExpose.sourceMap === undefined || rawExpose.sourceMap === null
        ? undefined
        : normalizeAsset(rawExpose.sourceMap, `exposes.${key}.sourceMap`, 'source-map'),
      synchronousAssets: mapAssets(rawExpose.synchronousAssets, `exposes.${key}.synchronousAssets`),
      asynchronousAssets: mapAssets(rawExpose.asynchronousAssets, `exposes.${key}.asynchronousAssets`),
    })
  }
  const hasExposes = Object.keys(exposes).length > 0
  if (remote.expectedHasExposes !== undefined && remote.expectedHasExposes !== hasExposes) {
    fail(FEDERATION_ERROR_CODES.LOCK_MISMATCH, 'Manifest expose presence does not match the production lock', {
      phase: 'manifest-validate', retryable: false,
      details: { expected: remote.expectedHasExposes, actual: hasExposes },
    })
  }
  if (!isRecord(rawManifest.shared)) {
    fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'shared must be an object', {
      phase: 'manifest-validate', retryable: false, details: { path: 'shared' },
    })
  }
  const shared = rawManifest.shared
  assertKnownFields(shared, 'shared', ['offers', 'requirements'])
  const validIdentity = (value, maximum = 512) =>
    typeof value === 'string' && value.length > 0 && value.length <= maximum &&
    /^[\x21-\x5b\x5d-\x7e]+$/u.test(value)
  const validBareSpecifier = (value) => validIdentity(value) && !value.startsWith('.') && !value.startsWith('/')
  const normalizePolicy = (rawPolicy, path) => {
    if (!isRecord(rawPolicy) || !isValidShareScope(rawPolicy.scope) ||
        typeof rawPolicy.singleton !== 'boolean' || typeof rawPolicy.strict !== 'boolean' ||
        typeof rawPolicy.fallback !== 'boolean') {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `${path} is invalid`, {
        phase: 'manifest-validate', retryable: false, details: { path },
      })
    }
    assertKnownFields(rawPolicy, path, [
      'scope', 'singleton', 'strict', 'fallback', 'coherenceGroup', 'owner',
    ])
    const coherenceGroup = rawPolicy.coherenceGroup ?? undefined
    const owner = rawPolicy.owner ?? undefined
    if (coherenceGroup !== undefined && (!validIdentity(coherenceGroup, 128) || !rawPolicy.singleton)) {
      fail(FEDERATION_ERROR_CODES.COHERENCE_CONFLICT, `${path}.coherenceGroup requires a singleton stable token`, {
        phase: 'manifest-validate', retryable: false, details: { path },
      })
    }
    if (owner !== undefined && (!isValidContainerName(owner) || !rawPolicy.singleton)) {
      fail(FEDERATION_ERROR_CODES.SHARE_SINGLETON_CONFLICT, `${path}.owner requires a singleton container name`, {
        phase: 'manifest-validate', retryable: false, details: { path },
      })
    }
    if (remote.mode === 'production' && rawPolicy.singleton && owner === undefined) {
      fail(FEDERATION_ERROR_CODES.SHARE_SINGLETON_CONFLICT, `${path}.owner is required for production singletons`, {
        phase: 'manifest-validate', retryable: false, details: { path },
      })
    }
    return Object.freeze({
      scope: rawPolicy.scope,
      singleton: rawPolicy.singleton,
      strict: rawPolicy.strict,
      fallback: rawPolicy.fallback,
      coherenceGroup,
      owner,
    })
  }
  const offers = validateShareEntries(shared.offers, 'shared.offers').map((offer, index) => {
    const path = `shared.offers[${index}]`
    assertKnownFields(offer, path, ['shareKey', 'package', 'provider', 'policy', 'asset'])
    if (!validBareSpecifier(offer.shareKey) || !isRecord(offer.package) ||
        !validBareSpecifier(offer.package.name) || parseVersion(offer.package.version) === null ||
        !validIdentity(offer.package.packageContext) || !validIdentity(offer.package.buildVariant, 256) ||
        offer.provider !== rawManifest.name) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `${path} is invalid`, {
        phase: 'manifest-validate', retryable: false, details: { path },
      })
    }
    assertKnownFields(offer.package, `${path}.package`, [
      'name', 'version', 'packageContext', 'buildVariant',
    ])
    return Object.freeze({
      shareKey: offer.shareKey,
      package: Object.freeze({ ...offer.package }),
      provider: offer.provider,
      policy: normalizePolicy(offer.policy, `${path}.policy`),
      asset: offer.asset === undefined || offer.asset === null
        ? undefined
        : normalizeAsset(offer.asset, `${path}.asset`, 'javascript'),
    })
  })
  const requirements = validateShareEntries(shared.requirements, 'shared.requirements').map((requirement, index) => {
    const path = `shared.requirements[${index}]`
    assertKnownFields(requirement, path, [
      'shareKey', 'requiredVersion', 'packageContext', 'buildVariant', 'policy', 'fallback',
    ])
    const rangeValid = typeof requirement.requiredVersion === 'string' && requirement.requiredVersion.length > 0 &&
      requirement.requiredVersion.split('||').every((alternative) =>
        alternative.trim() !== '' && parseComparatorSet(alternative) !== null)
    if (!validBareSpecifier(requirement.shareKey) || !rangeValid ||
        !validIdentity(requirement.packageContext) || !validIdentity(requirement.buildVariant, 256)) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, `${path} is invalid`, {
        phase: 'manifest-validate', retryable: false, details: { path },
      })
    }
    return Object.freeze({
      shareKey: requirement.shareKey,
      requiredVersion: requirement.requiredVersion,
      packageContext: requirement.packageContext,
      buildVariant: requirement.buildVariant,
      policy: normalizePolicy(requirement.policy, `${path}.policy`),
      fallback: requirement.fallback === undefined || requirement.fallback === null
        ? undefined
        : normalizeAsset(requirement.fallback, `${path}.fallback`, 'javascript'),
    })
  })
  const normalizedShared = Object.freeze({
    offers: Object.freeze(offers),
    requirements: Object.freeze(requirements),
  })
  for (const [exposeKey, exposed] of Object.entries(exposes).sort(([left], [right]) => left.localeCompare(right))) {
    if (exposed.mode !== 'host-rendered') continue
    const reactRequirements = requirements.filter((requirement) =>
      requirement.policy.scope === exposed.scope && REACT_COHERENCE_MEMBERS.includes(requirement.shareKey))
    for (const shareKey of REACT_COHERENCE_MEMBERS) {
      if (!reactRequirements.some((requirement) => requirement.shareKey === shareKey)) {
        fail(FEDERATION_ERROR_CODES.COHERENCE_CONFLICT, 'Host-rendered React scope is incomplete', {
          phase: 'manifest-validate', retryable: false,
          details: { expose: exposeKey, scope: exposed.scope, missing: shareKey },
        })
      }
    }
    for (const requirement of reactRequirements) {
      if (!requirement.policy.singleton || requirement.policy.coherenceGroup === undefined) {
        fail(FEDERATION_ERROR_CODES.COHERENCE_CONFLICT, 'Host-rendered React dependencies require one singleton coherence group', {
          phase: 'manifest-validate', retryable: false,
          details: { expose: exposeKey, scope: exposed.scope, shareKey: requirement.shareKey },
        })
      }
    }
    const coherenceGroups = new Set(reactRequirements.map((requirement) => requirement.policy.coherenceGroup))
    const owners = new Set(reactRequirements.map((requirement) => requirement.policy.owner))
    if (coherenceGroups.size !== 1 || owners.size !== 1) {
      fail(FEDERATION_ERROR_CODES.COHERENCE_CONFLICT, 'Host-rendered React dependencies must use the same coherence group and owner', {
        phase: 'manifest-validate', retryable: false,
        details: { expose: exposeKey, scope: exposed.scope },
      })
    }
  }
  let types
  if (rawManifest.types !== undefined && rawManifest.types !== null) {
    if (!isRecord(rawManifest.types)) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'types must be an object', {
        phase: 'manifest-validate', retryable: false, details: { path: 'types' },
      })
    }
    assertKnownFields(rawManifest.types, 'types', [
      'buildId', 'url', 'contentHash', 'integrity', 'size', 'format',
    ])
    let url
    try {
      url = new URL(assertNonEmptyString(rawManifest.types.url, 'types.url'), remote.manifestUrl).href
    } catch (cause) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'types.url is invalid', {
        phase: 'manifest-validate', retryable: false, cause, details: { path: 'types.url' },
      })
    }
    remote.assertUrl(url, 'types.url')
    types = Object.freeze({
      buildId: assertNonEmptyString(rawManifest.types.buildId, 'types.buildId'),
      url,
      contentHash: assertNonEmptyString(rawManifest.types.contentHash, 'types.contentHash'),
      integrity: assertNonEmptyString(rawManifest.types.integrity, 'types.integrity'),
      size: rawManifest.types.size,
      format: rawManifest.types.format,
    })
    if (types.format !== 'declaration-bundle' || !Number.isSafeInteger(types.size) || types.size < 0 || types.size > remote.maxAssetSize) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'types artifact is invalid', {
        phase: 'manifest-validate', retryable: false, details: { path: 'types' },
      })
    }
    if (sha384Digest(types.integrity) === null) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'types.integrity must contain SHA-384', {
        phase: 'manifest-validate', retryable: false, details: { path: 'types.integrity' },
      })
    }
    if (remote.lockAssets !== null) {
      const lockedIntegrity = remote.lockAssets.get(url)
      if (lockedIntegrity !== types.integrity) {
        fail(FEDERATION_ERROR_CODES.ASSET_INTEGRITY, 'Type artifact is outside the production lock closure', {
          phase: 'manifest-validate', retryable: false,
          details: { path: 'types.url', url, expected: lockedIntegrity, actual: types.integrity },
        })
      }
    }
    const identity = JSON.stringify({
      kind: 'other',
      contentHash: types.contentHash,
      integrity: types.integrity,
      size: types.size,
      mime: 'application/json',
    })
    const previousIdentity = seenAssets.get(url)
    if (previousIdentity !== undefined && previousIdentity !== identity) {
      fail(FEDERATION_ERROR_CODES.ASSET_INTEGRITY, 'Type URL conflicts with another manifest asset', {
        phase: 'manifest-validate', retryable: false, details: { path: 'types.url', url },
      })
    }
    seenAssets.set(url, identity)
    if (types.buildId !== buildId) {
      fail(FEDERATION_ERROR_CODES.TYPE_BUILD_MISMATCH, 'Type artifact buildId does not match JavaScript buildId', {
        phase: 'manifest-validate', retryable: false,
        details: { expected: buildId, actual: types.buildId },
      })
    }
    if (remote.expectedTypesHash !== undefined && types.contentHash !== remote.expectedTypesHash) {
      fail(FEDERATION_ERROR_CODES.TYPE_BUILD_MISMATCH, 'Type artifact does not match the production lock', {
        phase: 'manifest-validate', retryable: false,
        details: { expected: remote.expectedTypesHash, actual: types.contentHash },
      })
    }
    if (remote.expectedTypesIntegrity !== undefined && types.integrity !== remote.expectedTypesIntegrity) {
      fail(FEDERATION_ERROR_CODES.TYPE_BUILD_MISMATCH, 'Type artifact integrity does not match the production lock', {
        phase: 'manifest-validate', retryable: false,
        details: { expected: remote.expectedTypesIntegrity, actual: types.integrity },
      })
    }
  }
  if (remote.expectedHasExposes === true && types === undefined) {
    fail(FEDERATION_ERROR_CODES.TYPE_BUILD_MISMATCH, 'Production remotes with exposes require build-bound types', {
      phase: 'manifest-validate', retryable: false,
      details: { buildId },
    })
  }
  if (remote.lockAssets !== null && seenAssets.size !== remote.lockAssets.size) {
    fail(FEDERATION_ERROR_CODES.ASSET_INTEGRITY, 'Manifest asset closure does not match the production lock', {
      phase: 'manifest-validate', retryable: false,
      details: { expectedAssets: remote.lockAssets.size, actualAssets: seenAssets.size },
    })
  }
  const generation = Number.isSafeInteger(rawManifest.development?.generation)
    ? rawManifest.development.generation
    : 0
  let development
  if (rawManifest.development !== undefined && rawManifest.development !== null) {
    if (!isRecord(rawManifest.development) ||
        typeof rawManifest.development.updatesUrl !== 'string' || rawManifest.development.updatesUrl.length === 0 ||
        !Number.isSafeInteger(rawManifest.development.generation) || rawManifest.development.generation < 0) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'development metadata is invalid', {
        phase: 'manifest-validate', retryable: false, details: { path: 'development' },
      })
    }
    assertKnownFields(rawManifest.development, 'development', ['updatesUrl', 'generation'])
    let updatesUrl
    try {
      updatesUrl = new URL(rawManifest.development.updatesUrl, remote.manifestUrl)
    } catch (cause) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'development.updatesUrl is invalid', {
        phase: 'manifest-validate', retryable: false, cause, details: { path: 'development.updatesUrl' },
      })
    }
    if (!['ws:', 'wss:'].includes(updatesUrl.protocol)) {
      fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'development.updatesUrl must use ws or wss', {
        phase: 'manifest-validate', retryable: false,
        details: { path: 'development.updatesUrl', protocol: updatesUrl.protocol },
      })
    }
    const originUrl = new URL(updatesUrl.href)
    originUrl.protocol = updatesUrl.protocol === 'wss:' ? 'https:' : 'http:'
    if (!remote.allowedOrigins.has(originUrl.origin) ||
        (new URL(remote.manifestUrl).protocol === 'https:' && updatesUrl.protocol !== 'wss:')) {
      fail(FEDERATION_ERROR_CODES.ORIGIN_DENIED, 'development.updatesUrl is outside the remote origin allowlist', {
        phase: 'manifest-validate', retryable: false,
        details: { path: 'development.updatesUrl', url: updatesUrl.href },
      })
    }
    development = Object.freeze({ updatesUrl: updatesUrl.href, generation })
  }
  return Object.freeze({
    schemaVersion: FEDERATION_MANIFEST_SCHEMA,
    runtimeAbi: FEDERATION_RUNTIME_ABI,
    name: remote.name,
    buildId,
    browserTarget: rawManifest.browserTarget,
    remoteEntry,
    remoteEntrySourceMap,
    exposes: Object.freeze(exposes),
    shared: normalizedShared,
    types,
    development,
  })
}

function normalizeSharedRequest(request, overrides = {}) {
  const raw = typeof request === 'string' ? { shareKey: request } : request
  if (!isRecord(raw)) {
    fail(FEDERATION_ERROR_CODES.SHARE_UNSATISFIABLE, 'Shared dependency request must be an object', {
      phase: 'share-resolve', retryable: false,
    })
  }
  const shareKey = raw.shareKey ?? raw.name ?? raw.package?.name
  if (typeof shareKey !== 'string' || shareKey.length === 0) {
    fail(FEDERATION_ERROR_CODES.SHARE_UNSATISFIABLE, 'Shared dependency request is missing shareKey', {
      phase: 'share-resolve', retryable: false,
    })
  }
  return Object.freeze({
    shareKey,
    requiredVersion: raw.requiredVersion ?? raw.versionRange ?? raw.range ?? '*',
    packageContext: raw.packageContext ?? raw.package?.packageContext ?? raw.package?.peerContext,
    buildVariant: raw.buildVariant ?? raw.package?.buildVariant,
    coherenceGroup: raw.coherenceGroup ?? raw.policy?.coherenceGroup,
    fallback: (raw.fallback ?? raw.policy?.fallback) !== false,
    owner: raw.owner ?? raw.policy?.owner,
    scope: raw.scope ?? raw.policy?.scope ?? DEFAULT_SCOPE,
    singleton: Boolean(raw.singleton ?? raw.policy?.singleton),
    strict: (raw.strict ?? raw.policy?.strict) !== false,
    ...overrides,
  })
}

function normalizeProvider(raw, defaults) {
  const shareKey = raw.shareKey ?? raw.name ?? raw.package?.name
  const version = raw.version ?? raw.package?.version
  if (typeof shareKey !== 'string' || shareKey.length === 0 || parseVersion(version) === null) {
    fail(FEDERATION_ERROR_CODES.SHARE_UNSATISFIABLE, 'Shared provider requires shareKey and an exact semantic version', {
      phase: 'share-register', retryable: false, details: { shareKey, version },
    })
  }
  let get
  let loaded = false
  let moduleValue
  if (hasOwn(raw, 'module')) {
    moduleValue = raw.module
    get = async () => moduleValue
    loaded = true
  } else if (typeof raw.get === 'function') {
    get = raw.get
  } else {
    get = defaults.get
  }
  if (typeof get !== 'function') {
    fail(FEDERATION_ERROR_CODES.SHARE_UNSATISFIABLE, 'Shared provider requires module or get()', {
      phase: 'share-register', retryable: false, details: { shareKey, version },
    })
  }
  return {
    shareKey,
    version: String(version),
    parsedVersion: parseVersion(version),
    scope: raw.scope ?? raw.policy?.scope ?? DEFAULT_SCOPE,
    singleton: Boolean(raw.singleton ?? raw.policy?.singleton),
    strict: (raw.strict ?? raw.policy?.strict) !== false,
    packageContext: raw.packageContext ?? raw.package?.packageContext ?? raw.package?.peerContext ?? '',
    buildVariant: raw.buildVariant ?? raw.package?.buildVariant ?? '',
    coherenceGroup: raw.coherenceGroup ?? raw.policy?.coherenceGroup,
    fallback: (raw.fallback ?? raw.policy?.fallback) !== false,
    host: defaults.host,
    ownerKey: defaults.ownerKey,
    ownerName: defaults.ownerName,
    get,
    loaded,
    module: moduleValue,
    promise: null,
    fatalError: null,
    sequence: defaults.sequence,
  }
}

export class FederationRuntime {
  constructor(options = {}) {
    const mode = federationMode(options.mode === undefined ? 'development' : options.mode, 'runtime-config')
    if (options.transport === undefined) assertBrowserGlobal(options.global ?? globalThis)
    this.runtimeAbi = FEDERATION_RUNTIME_ABI
    this.global = options.global ?? globalThis
    this.mode = mode
    this.timeoutMs = options.timeoutMs === undefined
      ? DEFAULT_TIMEOUT_MS
      : boundedPositiveInteger(options.timeoutMs, 'timeoutMs', MAX_TIMEOUT_MS, 'runtime-config')
    this.maxManifestSize = options.maxManifestSize === undefined
      ? DEFAULT_MAX_MANIFEST_SIZE
      : boundedPositiveInteger(options.maxManifestSize, 'maxManifestSize', MAX_MANIFEST_SIZE, 'runtime-config')
    this.maxAssetSize = options.maxAssetSize === undefined
      ? DEFAULT_MAX_ASSET_SIZE
      : boundedPositiveInteger(options.maxAssetSize, 'maxAssetSize', MAX_ASSET_SIZE, 'runtime-config')
    this.devReconnectMs = options.devReconnectMs === undefined
      ? DEFAULT_DEV_RECONNECT_MS
      : boundedPositiveInteger(options.devReconnectMs, 'devReconnectMs', MAX_DEV_RECONNECT_MS, 'runtime-config')
    this.transport = options.transport ?? createBrowserTransport(this.global, options)
    this.remotes = new Map()
    this.containers = new Map()
    this.providers = new Map()
    this.singletonLocks = new Map()
    this.coherenceLocks = new Map()
    this.singletonFlights = new Map()
    this.containerFlights = new Map()
    this.moduleFlights = new Map()
    this.moduleValues = new Map()
    this.scriptFlights = new Map()
    this.styleFlights = new Map()
    this.isolatedStyleBuckets = new Map()
    this.sharedDecisions = []
    this.evaluationStack = []
    this.activeEvaluations = new Set()
    this.activeContainerEvaluations = new Map()
    this.evaluationEdges = new Map()
    this.devUpdateConnections = new Map()
    this.sequence = 0
  }

  registerRemote(nameOrDefinition, definition) {
    const config = normalizeRemoteRegistration(nameOrDefinition, definition)
    let manifestUrl
    try {
      manifestUrl = new URL(config.manifestUrl, this.global.location?.href).href
    } catch (cause) {
      fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Remote manifestUrl must be an absolute URL', {
        phase: 'remote-register', retryable: false, cause, details: { name: config.name },
      })
    }
    const mode = federationMode(config.mode === undefined ? this.mode : config.mode, 'remote-register')
    const manifestOrigin = new URL(manifestUrl).origin
    let allowedOrigins
    try {
      allowedOrigins = new Set((config.allowedOrigins ?? [manifestOrigin]).map((origin) => new URL(origin, manifestUrl).origin))
    } catch (cause) {
      fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'allowedOrigins contains an invalid URL', {
        phase: 'remote-register', retryable: false, cause, details: { name: config.name },
      })
    }
    if (!allowedOrigins.has(manifestOrigin)) {
      fail(FEDERATION_ERROR_CODES.ORIGIN_DENIED, 'Manifest origin is not in allowedOrigins', {
        phase: 'remote-register', retryable: false, details: { name: config.name, origin: manifestOrigin },
      })
    }
    if (mode === 'production' && new URL(manifestUrl).protocol !== 'https:') {
      fail(FEDERATION_ERROR_CODES.ORIGIN_DENIED, 'Production federation manifests require HTTPS', {
        phase: 'remote-register', retryable: false, details: { name: config.name, manifestUrl },
      })
    }
    const lock = config.lock ?? {}
    if (mode === 'production' && typeof lock.hasExposes !== 'boolean') {
      fail(FEDERATION_ERROR_CODES.LOCK_INVALID, 'Production remotes require lock.hasExposes', {
        phase: 'remote-register', retryable: false, details: { name: config.name },
      })
    }
    const expectedBuildId = config.expectedBuildId ?? config.buildId ?? lock.buildId
    if (expectedBuildId !== undefined && !isValidIdentityToken(expectedBuildId)) {
      fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Remote expected buildId must be a stable identity token', {
        phase: 'remote-register', retryable: false, details: { name: config.name, buildId: expectedBuildId },
      })
    }
    const lockedManifestIntegrity = config.manifestIntegrity ?? lock.manifestIntegrity
    const lockedTypesIntegrity = lock.typesIntegrity ?? undefined
    const typesIntegrity = config.typesIntegrity ?? lockedTypesIntegrity
    if (mode === 'production' &&
        (expectedBuildId === undefined ||
         typeof lockedManifestIntegrity !== 'string' || sha384Digest(lockedManifestIntegrity) === null)) {
      fail(FEDERATION_ERROR_CODES.LOCK_INVALID, 'Production remotes require a locked buildId and manifest SHA-384', {
        phase: 'remote-register', retryable: false, details: { name: config.name },
      })
    }
    let lockAssets
    try {
      lockAssets = normalizeLockAssets(lock, manifestUrl)
    } catch (cause) {
      fail(FEDERATION_ERROR_CODES.LOCK_INVALID, 'Production lock asset closure contains an invalid entry', {
        phase: 'remote-register', retryable: false, cause, details: { name: config.name },
      })
    }
    if (lock.manifestUrl !== undefined) {
      let lockedManifestUrl
      try {
        lockedManifestUrl = new URL(lock.manifestUrl, manifestUrl).href
      } catch (cause) {
        fail(FEDERATION_ERROR_CODES.LOCK_INVALID, 'Production lock manifestUrl is invalid', {
          phase: 'remote-register', retryable: false, cause, details: { name: config.name },
        })
      }
      if (lockedManifestUrl !== manifestUrl) {
        fail(FEDERATION_ERROR_CODES.LOCK_MISMATCH, 'Remote manifestUrl does not match the production lock', {
          phase: 'remote-register', retryable: false,
          details: { name: config.name, expected: lockedManifestUrl, actual: manifestUrl },
        })
      }
    }
    if (mode === 'production' && lockAssets === null) {
      fail(FEDERATION_ERROR_CODES.LOCK_INVALID, 'Production remotes require an allowedAssets closure', {
        phase: 'remote-register', retryable: false, details: { name: config.name },
      })
    }
    if (mode === 'production') {
      for (const [url, assetIntegrity] of lockAssets) {
        const parsed = new URL(url)
        if (parsed.protocol !== 'https:' || !allowedOrigins.has(parsed.origin)) {
          fail(FEDERATION_ERROR_CODES.ORIGIN_DENIED, 'Production lock contains an asset outside allowedOrigins or HTTPS', {
            phase: 'remote-register', retryable: false, details: { name: config.name, url },
          })
        }
        if (sha384Digest(assetIntegrity) === null) {
          fail(FEDERATION_ERROR_CODES.LOCK_INVALID, 'Production lock assets require SHA-384 integrity', {
            phase: 'remote-register', retryable: false, details: { name: config.name, url },
          })
        }
      }
      if (lock.hasExposes === true &&
          (typeof lockedTypesIntegrity !== 'string' || sha384Digest(lockedTypesIntegrity) === null)) {
        fail(FEDERATION_ERROR_CODES.LOCK_INVALID, 'Production remotes with exposes require locked type integrity', {
          phase: 'remote-register', retryable: false, details: { name: config.name },
        })
      }
      if (typesIntegrity !== undefined && sha384Digest(typesIntegrity) === null) {
        fail(FEDERATION_ERROR_CODES.LOCK_INVALID, 'Production lock type integrity must use SHA-384', {
          phase: 'remote-register', retryable: false, details: { name: config.name },
        })
      }
    }
    const timeoutMs = config.timeoutMs === undefined
      ? this.timeoutMs
      : boundedPositiveInteger(config.timeoutMs, 'timeoutMs', MAX_TIMEOUT_MS, 'remote-register')
    const maxAssetSize = config.maxAssetSize === undefined
      ? this.maxAssetSize
      : boundedPositiveInteger(config.maxAssetSize, 'maxAssetSize', MAX_ASSET_SIZE, 'remote-register')
    const maxManifestSize = config.maxManifestSize === undefined
      ? this.maxManifestSize
      : boundedPositiveInteger(config.maxManifestSize, 'maxManifestSize', MAX_MANIFEST_SIZE, 'remote-register')
    const registrationSignature = JSON.stringify({
      manifestUrl,
      mode,
      allowedOrigins: [...allowedOrigins].sort(),
      expectedBuildId,
      manifestIntegrity: lockedManifestIntegrity,
      expectedTypesHash: config.typesHash ?? lock.typesHash,
      expectedTypesIntegrity: typesIntegrity,
      expectedHasExposes: mode === 'production' ? lock.hasExposes : undefined,
      timeoutMs,
      maxManifestSize,
      maxAssetSize,
      lockAssets: lockAssets === null ? null : [...lockAssets].sort(([left], [right]) => left.localeCompare(right)),
    })
    const existing = this.remotes.get(config.name)
    if (existing !== undefined) {
      if (existing.registrationSignature === registrationSignature) return this
      fail(FEDERATION_ERROR_CODES.REMOTE_CONFLICT, 'Remote name is already registered with different configuration', {
        phase: 'remote-register', retryable: false,
        details: { name: config.name, existingManifestUrl: existing.manifestUrl, manifestUrl },
      })
    }
    const state = {
      name: config.name,
      manifestUrl,
      mode,
      registrationSignature,
      allowedOrigins,
      expectedBuildId,
      manifestIntegrity: lockedManifestIntegrity,
      expectedTypesHash: config.typesHash ?? lock.typesHash,
      expectedTypesIntegrity: typesIntegrity,
      expectedHasExposes: mode === 'production' ? lock.hasExposes : undefined,
      timeoutMs,
      maxManifestSize,
      maxAssetSize,
      lockAssets,
      manifest: null,
      manifestFlight: null,
      acceptedManifests: new Map(),
      acceptedManifestSignatures: new Map(),
      fatalError: null,
      revision: 0,
      devGeneration: -1,
      devBuildId: null,
      appliedDevGeneration: -1,
      appliedDevBuildId: null,
      lastDevUpdate: null,
      lastDevUpdateSignature: null,
      lastDispatchedDevUpdate: null,
      activeDevBuildIds: new Set(),
      devReloadRequested: false,
      exposeErrors: new Map(),
      trace: [],
      assertUrl: (url, path) => {
        const parsed = new URL(url)
        if (!allowedOrigins.has(parsed.origin) || (mode === 'production' && parsed.protocol !== 'https:')) {
          fail(FEDERATION_ERROR_CODES.ORIGIN_DENIED, 'Federation asset origin is not allowed', {
            phase: 'manifest-validate', retryable: false, details: { name: config.name, path, url },
          })
        }
      },
    }
    this.remotes.set(config.name, state)
    this.#trace(state, 'remote-register', 'registered', { manifestUrl, mode })
    return this
  }

  applyDevUpdate(value) {
    const update = normalizeDevUpdate(value)
    const remote = this.#remote(update.remote)
    if (remote.mode !== 'development') {
      fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Federation dev updates can only target development remotes', {
        phase: 'dev-update', retryable: false,
        details: { remote: update.remote, mode: remote.mode },
      })
    }
    const currentBuildId = remote.devBuildId ?? remote.manifest?.buildId ?? null
    const currentGeneration = Math.max(
      remote.devGeneration,
      remote.manifest?.development?.generation ?? -1,
    )
    const updateSignature = JSON.stringify(update)
    if (update.generation === currentGeneration) {
      if (remote.lastDevUpdateSignature === updateSignature && remote.lastDevUpdate !== null) {
        return remote.lastDevUpdate
      }
      const continuousCatchUp = update.oldBuildId === remote.appliedDevBuildId
      if (update.oldBuildId !== update.newBuildId && continuousCatchUp &&
          remote.appliedDevGeneration < currentGeneration && update.newBuildId === currentBuildId) {
        remote.appliedDevGeneration = update.generation
        remote.appliedDevBuildId = update.newBuildId
        remote.lastDevUpdate = update
        remote.lastDevUpdateSignature = updateSignature
        this.#syncDevLease(remote)
        this.#trace(remote, 'dev-update', 'control-caught-up', {
          oldBuildId: update.oldBuildId,
          newBuildId: update.newBuildId,
          generation: update.generation,
          action: update.action,
        })
        return update
      }
      fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Federation dev update conflicts with the accepted generation', {
        phase: 'dev-update', retryable: false,
        details: {
          remote: update.remote,
          currentGeneration,
          currentBuildId,
          appliedGeneration: remote.appliedDevGeneration,
          appliedBuildId: remote.appliedDevBuildId,
          updateGeneration: update.generation,
          updateBuildId: update.newBuildId,
        },
      })
    }
    if (update.generation < currentGeneration) {
      fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Federation dev update generation is not monotonic', {
        phase: 'dev-update', retryable: false,
        details: { remote: update.remote, currentGeneration, updateGeneration: update.generation },
      })
    }
    if (currentBuildId !== null && update.oldBuildId !== currentBuildId) {
      fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Federation dev update oldBuildId does not match the active build', {
        phase: 'dev-update', retryable: false,
        details: { remote: update.remote, expected: currentBuildId, actual: update.oldBuildId },
      })
    }
    if (update.oldBuildId === update.newBuildId || currentBuildId === update.newBuildId) {
      fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Federation dev update must advance to a new buildId', {
        phase: 'dev-update', retryable: false,
        details: { remote: update.remote, currentBuildId, newBuildId: update.newBuildId },
      })
    }

    remote.devGeneration = update.generation
    remote.devBuildId = update.newBuildId
    remote.appliedDevGeneration = update.generation
    remote.appliedDevBuildId = update.newBuildId
    remote.lastDevUpdate = update
    remote.lastDevUpdateSignature = updateSignature
    if (update.action === 'types-only') {
      this.#syncDevLease(remote)
      this.#trace(remote, 'dev-update', 'control-advanced', {
        oldBuildId: update.oldBuildId,
        newBuildId: update.newBuildId,
        generation: update.generation,
        action: update.action,
      })
      return update
    }

    this.#syncDevLease(remote)
    remote.revision += 1
    remote.manifest = null
    remote.manifestFlight = null
    remote.fatalError = null
    remote.exposeErrors.clear()
    this.#trace(remote, 'dev-update', 'invalidated', {
      oldBuildId: update.oldBuildId,
      newBuildId: update.newBuildId,
      generation: update.generation,
      action: update.action,
    })
    return update
  }

  async attachIsolatedStyleTarget(specifier, root) {
    if (!isRecord(root) || root.mode !== 'open' || typeof root.append !== 'function' ||
        !isRecord(root.host) || root.host.shadowRoot !== root) {
      fail(FEDERATION_ERROR_CODES.STYLE_LOAD, 'Isolated styles require an open ShadowRoot', {
        phase: 'style-target', retryable: false, details: { specifier },
      })
    }
    const parsed = parseRemoteSpecifier(specifier)
    const remote = this.#remote(parsed.remote)
    const revision = remote.revision
    const manifest = await this.#manifest(remote)
    this.#assertRevision(remote, revision)
    const expose = manifest.exposes[parsed.expose]
    if (expose === undefined) this.#unknownExpose(remote, parsed.expose)
    if (expose.mode !== 'isolated' || expose.shadow !== 'open') {
      fail(FEDERATION_ERROR_CODES.STYLE_LOAD, 'Style targets can only attach to isolated exposes', {
        phase: 'style-target', retryable: false,
        details: { specifier, mode: expose.mode, shadow: expose.shadow },
      })
    }
    const bucket = this.#isolatedStyleBucket(manifest, parsed.expose, remote)
    this.#rememberIsolatedStyles(bucket, [
      ...expose.synchronousAssets.filter((asset) => asset.kind === 'css'),
      ...expose.css,
    ])
    let target = bucket.targets.get(root)
    if (target === undefined) {
      target = {
        root,
        references: 0,
        active: true,
        tail: Promise.resolve(),
        flights: new Map(),
        nodes: new Map(),
      }
      bucket.targets.set(root, target)
    }
    target.references += 1
    let released = false
    try {
      await this.#hydrateIsolatedStyleTarget(bucket, target)
      this.#assertRevision(remote, revision)
    } catch (error) {
      this.#releaseIsolatedStyleTarget(bucket, target)
      released = true
      throw error
    }
    return () => {
      if (released) return
      released = true
      this.#releaseIsolatedStyleTarget(bucket, target)
    }
  }

  async loadFederatedAsset(request) {
    if (!isRecord(request) || !isValidContainerName(request.name) ||
        !isValidIdentityToken(request.buildId) || !['javascript', 'css'].includes(request.kind) ||
        (request.expose !== undefined && !isValidExposeKey(request.expose))) {
      fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Federated asset request is invalid', {
        phase: 'asset-resolve', retryable: false, details: { request },
      })
    }
    const fileName = normalizeFederatedFileName(request.fileName)
    const remote = this.#remote(request.name)
    const manifest = await this.#manifestForBuild(remote, request.buildId)
    const suffix = `/${fileName}`
    const candidates = manifestAssetRecords(manifest).filter(({ asset, exposes }) => {
      if (asset.kind !== request.kind) return false
      if (request.expose !== undefined && !exposes.has(request.expose)) return false
      const pathname = new URL(asset.url).pathname
      return pathname === suffix || pathname.endsWith(suffix)
    })
    if (candidates.length !== 1) {
      fail(FEDERATION_ERROR_CODES.ASSET_INTEGRITY, candidates.length === 0
        ? 'Federated asset is not present in the accepted manifest closure'
        : 'Federated asset fileName is ambiguous in the accepted manifest closure', {
        phase: 'asset-resolve', retryable: false,
        details: { name: request.name, buildId: request.buildId, fileName, kind: request.kind, matches: candidates.map(({ asset }) => asset.url) },
      })
    }
    const [{ asset, exposes }] = candidates
    const expose = request.expose ?? (exposes.size === 1 ? exposes.values().next().value : undefined)
    if (request.kind === 'javascript') {
      await this.#script(asset, remote, manifest, expose)
    } else {
      const isolatedOwners = [...exposes].filter((owner) => manifest.exposes[owner]?.mode === 'isolated')
      if (expose === undefined && isolatedOwners.length > 0) {
        fail(FEDERATION_ERROR_CODES.STYLE_LOAD, 'Isolated stylesheet ownership is ambiguous', {
          phase: 'style-target', retryable: false,
          details: { name: manifest.name, buildId: manifest.buildId, fileName, exposes: isolatedOwners },
        })
      }
      if (expose !== undefined && manifest.exposes[expose]?.mode === 'isolated') {
        const bucket = this.#isolatedStyleBucket(manifest, expose, remote)
        await this.#loadIsolatedStyles(bucket, [asset])
      } else {
        await this.#styles([asset], remote, manifest)
      }
    }
  }

  registerContainer(registration, maybeContainer) {
    const value = typeof registration === 'string'
      ? { name: registration, buildId: maybeContainer?.buildId, container: maybeContainer?.container ?? maybeContainer }
      : registration
    if (!isRecord(value)) {
      fail(FEDERATION_ERROR_CODES.CONTAINER_REGISTRATION, 'Container registration must be an object', {
        phase: 'container-register', retryable: false,
      })
    }
    const name = value.name
    const buildId = value.buildId
    const container = value.container
    if (!isValidContainerName(name) || !isValidIdentityToken(buildId) ||
        !isRecord(container) || typeof container.init !== 'function' || typeof container.get !== 'function') {
      fail(FEDERATION_ERROR_CODES.CONTAINER_REGISTRATION, 'Container must provide name, buildId, init(), and get()', {
        phase: 'container-register', retryable: false, details: { name, buildId },
      })
    }
    const remote = this.remotes.get(name)
    if (remote?.mode === 'development' && !remote.activeDevBuildIds.has(buildId)) {
      fail(FEDERATION_ERROR_CODES.CONTAINER_REGISTRATION, 'An unleased development container cannot be registered', {
        phase: 'container-register', retryable: false, details: { name, buildId },
      })
    }
    const key = containerKey(name, buildId)
    const existing = this.containers.get(key)
    if (existing !== undefined) {
      if (existing.container !== container) {
        fail(FEDERATION_ERROR_CODES.CONTAINER_REGISTRATION, 'A different container already owns this name and buildId', {
          phase: 'container-register', retryable: false, details: { name, buildId },
        })
      }
      return this
    }
    this.containers.set(key, {
      name,
      buildId,
      container,
      initialized: false,
      initFlight: null,
      offersRegistered: false,
    })
    return this
  }

  registerHostShared(sharedOrKey, maybeProvider) {
    let entries
    if (typeof sharedOrKey === 'string') {
      entries = [{ ...(maybeProvider ?? {}), shareKey: sharedOrKey }]
    } else if (Array.isArray(sharedOrKey)) {
      entries = sharedOrKey
    } else if (isRecord(sharedOrKey) && (sharedOrKey.shareKey !== undefined || sharedOrKey.name !== undefined)) {
      entries = [sharedOrKey]
    } else if (isRecord(sharedOrKey)) {
      entries = Object.entries(sharedOrKey).map(([shareKey, provider]) => ({ ...provider, shareKey }))
    } else {
      entries = []
    }
    for (const raw of entries) {
      const ownerKey = raw.owner ?? '$host'
      const provider = normalizeProvider(raw, {
        host: true,
        ownerKey,
        ownerName: raw.owner ?? '$host',
        sequence: this.sequence++,
      })
      this.#addProvider(provider)
    }
    return this
  }

  async resolveShared(request, context = {}) {
    const normalized = normalizeSharedRequest(request)
    const key = shareBucketKey(normalized.scope, normalized.shareKey)
    const candidates = this.providers.get(key) ?? []
    const needsSerialization = normalized.singleton || candidates.some((candidate) => candidate.singleton) || this.singletonLocks.has(key)
    if (!needsSerialization) return this.#resolveShared(normalized, context)
    const currentFlight = this.singletonFlights.get(key)
    if (currentFlight !== undefined) {
      try {
        await currentFlight
      } catch {
        // The waiting request performs its own deterministic resolution below.
      }
      return this.#resolveShared(normalized, context)
    }
    const flight = this.#resolveShared(normalized, context)
    this.singletonFlights.set(key, flight)
    try {
      return await flight
    } finally {
      if (this.singletonFlights.get(key) === flight) this.singletonFlights.delete(key)
    }
  }

  async preloadRemote(specifier) {
    const parsed = parseRemoteSpecifier(specifier)
    const remote = this.#remote(parsed.remote)
    const revision = remote.revision
    try {
      const manifest = await this.#manifest(remote)
      this.#assertRevision(remote, revision)
      if (manifest.exposes[parsed.expose] === undefined) this.#unknownExpose(remote, parsed.expose)
      await this.#container(remote, manifest)
      this.#assertRevision(remote, revision)
      await this.#exposeAssets(parsed.expose, manifest.exposes[parsed.expose], remote, manifest)
      this.#assertRevision(remote, revision)
      this.#trace(remote, 'preload', 'ready', { expose: parsed.expose, buildId: manifest.buildId })
    } catch (error) {
      this.#rememberFatal(remote, error, revision)
      this.#traceError(remote, 'preload', error, { expose: parsed.expose })
      throw error
    }
  }

  async prepareRemote(name) {
    if (!isValidContainerName(name)) {
      fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'prepareRemote requires a valid remote name', {
        phase: 'remote-prepare', retryable: false, details: { name },
      })
    }
    const remote = this.#remote(name)
    const revision = remote.revision
    try {
      const manifest = await this.#manifest(remote)
      this.#assertRevision(remote, revision)
      await this.#container(remote, manifest)
      this.#assertRevision(remote, revision)
      this.#trace(remote, 'remote-prepare', 'ready', { buildId: manifest.buildId })
      return Object.freeze({
        name: manifest.name,
        buildId: manifest.buildId,
        generation: runtimeGeneration(remote, manifest),
      })
    } catch (error) {
      this.#rememberFatal(remote, error, revision)
      this.#traceError(remote, 'remote-prepare', error)
      throw error
    }
  }

  async describeRemote(specifier) {
    const parsed = parseRemoteSpecifier(specifier)
    const remote = this.#remote(parsed.remote)
    const revision = remote.revision
    try {
      const manifest = await this.#manifest(remote)
      this.#assertRevision(remote, revision)
      const expose = manifest.exposes[parsed.expose]
      if (expose === undefined) this.#unknownExpose(remote, parsed.expose)
      const css = [
        ...expose.synchronousAssets.filter((asset) => asset.kind === 'css'),
        ...expose.css,
      ]
      return Object.freeze({
        specifier,
        name: manifest.name,
        buildId: manifest.buildId,
        generation: runtimeGeneration(remote, manifest),
        development: remote.mode === 'development',
        expose: parsed.expose,
        mode: expose.mode,
        scope: expose.scope,
        shadow: expose.shadow,
        css: Object.freeze(css),
      })
    } catch (error) {
      this.#rememberFatal(remote, error, revision)
      this.#traceError(remote, 'remote-describe', error, { expose: parsed.expose })
      throw error
    }
  }

  async connectDevUpdates(name) {
    if (!isValidContainerName(name)) {
      fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'connectDevUpdates requires a valid remote name', {
        phase: 'dev-updates-connect', retryable: false, details: { name },
      })
    }
    const remote = this.#remote(name)
    if (remote.mode !== 'development') {
      fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Dev update sockets can only target development remotes', {
        phase: 'dev-updates-connect', retryable: false, details: { name, mode: remote.mode },
      })
    }
    const manifest = await this.#manifest(remote)
    return this.#connectDevUpdateSocket(remote, manifest)
  }

  async loadRemote(specifier, requesterContext) {
    const parsed = parseRemoteSpecifier(specifier)
    const evaluationKey = `${parsed.remote}/${parsed.expose.slice(2)}`
    const requester = this.#requesterEvaluationKey(requesterContext) ?? this.evaluationStack.at(-1)
    if (requester !== undefined) this.#recordEvaluationEdge(requester, evaluationKey)
    const remote = this.#remote(parsed.remote)
    const revision = remote.revision
    let manifest
    try {
      manifest = await this.#manifest(remote)
      this.#assertRevision(remote, revision)
      const expose = manifest.exposes[parsed.expose]
      if (expose === undefined) this.#unknownExpose(remote, parsed.expose)
      const state = await this.#container(remote, manifest)
      this.#assertRevision(remote, revision)
      await this.#exposeAssets(parsed.expose, expose, remote, manifest)
      this.#assertRevision(remote, revision)
      const generation = runtimeGeneration(remote, manifest)
      const key = `${containerKey(manifest.name, manifest.buildId)}\0${parsed.expose}\0${generation}`
      if (this.moduleValues.has(key)) {
        remote.exposeErrors.delete(parsed.expose)
        return this.moduleValues.get(key)
      }
      const existing = this.moduleFlights.get(key)
      if (existing !== undefined) {
        const namespace = await existing
        this.#assertRevision(remote, revision)
        remote.exposeErrors.delete(parsed.expose)
        return namespace
      }
      const flight = (async () => {
        let factory
        try {
          factory = await state.container.get(parsed.expose)
        } catch (cause) {
          if (cause instanceof FederationError) throw cause
          throw new FederationError(FEDERATION_ERROR_CODES.CONTAINER_GET, 'Remote container get() failed', {
            phase: 'container-get', retryable: false, cause,
            details: { name: manifest.name, buildId: manifest.buildId, expose: parsed.expose },
          })
        }
        if (typeof factory !== 'function') {
          fail(FEDERATION_ERROR_CODES.CONTAINER_GET, 'Remote container get() did not return a module factory', {
            phase: 'container-get', retryable: false,
            details: { name: manifest.name, buildId: manifest.buildId, expose: parsed.expose },
          })
        }
        try {
          this.activeEvaluations.add(evaluationKey)
          this.#activateContainerEvaluation(manifest.name, manifest.buildId, evaluationKey)
          this.evaluationStack.push(evaluationKey)
          let evaluation
          try {
            evaluation = factory()
          } finally {
            const popped = this.evaluationStack.pop()
            if (popped !== evaluationKey) {
              fail(FEDERATION_ERROR_CODES.REMOTE_CYCLE, 'Remote evaluation stack was corrupted', {
                phase: 'module-evaluate', retryable: false, details: { expected: evaluationKey, actual: popped },
              })
            }
          }
          const namespace = await evaluation
          this.#assertRevision(remote, revision)
          this.moduleValues.set(key, namespace)
          remote.exposeErrors.delete(parsed.expose)
          this.#trace(remote, 'module-evaluate', 'loaded', { expose: parsed.expose, buildId: manifest.buildId })
          return namespace
        } catch (cause) {
          if (cause instanceof FederationError) throw cause
          throw new FederationError(FEDERATION_ERROR_CODES.CONTAINER_GET, 'Remote module factory failed', {
            phase: 'module-evaluate', retryable: false, cause,
            details: { name: manifest.name, buildId: manifest.buildId, expose: parsed.expose },
          })
        } finally {
          this.activeEvaluations.delete(evaluationKey)
          this.#deactivateContainerEvaluation(manifest.name, manifest.buildId, evaluationKey)
          this.evaluationEdges.delete(evaluationKey)
        }
      })()
      this.moduleFlights.set(key, flight)
      try {
        return await flight
      } finally {
        if (this.moduleFlights.get(key) === flight) this.moduleFlights.delete(key)
      }
    } catch (error) {
      this.#rememberFatal(remote, error, revision)
      if (remote.revision === revision && error instanceof FederationError) {
        remote.exposeErrors.set(parsed.expose, error)
      }
      this.#traceError(remote, 'load', error, { expose: parsed.expose, buildId: manifest?.buildId })
      throw error
    }
  }

  explain(specifier) {
    let parsed
    try {
      parsed = parseRemoteSpecifier(specifier)
    } catch (error) {
      return freezeDecision({
        specifier,
        status: 'error',
        error: error instanceof FederationError ? error.toJSON() : { message: String(error) },
        trace: [],
      })
    }
    const remote = this.remotes.get(parsed.remote)
    if (remote === undefined) {
      return freezeDecision({ specifier, remote: parsed.remote, expose: parsed.expose, status: 'unregistered', trace: [] })
    }
    const manifest = remote.manifest
    const buildId = manifest?.buildId
    const keyPrefix = buildId === undefined ? null : `${containerKey(parsed.remote, buildId)}\0${parsed.expose}\0`
    const moduleLoaded = keyPrefix !== null && [...this.moduleValues.keys()].some((key) => key.startsWith(keyPrefix))
    const container = buildId === undefined ? undefined : this.containers.get(containerKey(parsed.remote, buildId))
    const diagnosticError = remote.fatalError ?? remote.exposeErrors.get(parsed.expose)
    const status = diagnosticError !== undefined && diagnosticError !== null
      ? 'error'
      : moduleLoaded
        ? 'loaded'
        : container?.initialized
          ? 'ready'
          : manifest === null
            ? 'registered'
            : 'manifest-loaded'
    return freezeDecision({
      specifier,
      remote: parsed.remote,
      expose: parsed.expose,
      status,
      manifestUrl: remote.manifestUrl,
      container: manifest === null ? undefined : {
        name: manifest.name,
        buildId: manifest.buildId,
        generation: runtimeGeneration(remote, manifest),
      },
      cache: {
        manifest: manifest !== null,
        container: container !== undefined,
        module: moduleLoaded,
      },
      shared: this.sharedDecisions.filter((decision) => decision.requester === containerKey(parsed.remote, buildId ?? '')),
      error: diagnosticError?.toJSON(),
      trace: remote.trace,
    })
  }

  #recordEvaluationEdge(requester, target) {
    const targets = this.evaluationEdges.get(requester) ?? new Set()
    targets.add(target)
    this.evaluationEdges.set(requester, targets)
    if (!this.activeEvaluations.has(target)) return
    const pending = [[target, [target]]]
    const visited = new Set()
    while (pending.length > 0) {
      const [current, path] = pending.pop()
      if (current === requester) {
        const chain = [...path, target]
        fail(FEDERATION_ERROR_CODES.REMOTE_CYCLE, 'Cross-container remote evaluation cycle is unsupported', {
          phase: 'module-evaluate', retryable: false, details: { chain },
        })
      }
      if (visited.has(current)) continue
      visited.add(current)
      for (const next of this.evaluationEdges.get(current) ?? []) pending.push([next, [...path, next]])
    }
  }

  #requesterEvaluationKey(context) {
    if (context === undefined || context === null) return undefined
    if (!isRecord(context)) {
      fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Remote requester context must be an object', {
        phase: 'module-evaluate', retryable: false,
      })
    }
    const nested = isRecord(context.container) ? context.container : null
    const name = context.name ?? nested?.name ?? (typeof context.container === 'string' ? context.container : undefined)
    const buildId = context.buildId ?? nested?.buildId
    const expose = context.expose ?? nested?.expose
    if (!isValidContainerName(name) || !isValidIdentityToken(buildId) ||
        (expose !== undefined && !isValidExposeKey(expose))) {
      fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Remote requester context has an invalid container identity', {
        phase: 'module-evaluate', retryable: false, details: { name, buildId, expose },
      })
    }
    const active = this.activeContainerEvaluations.get(containerKey(name, buildId))
    if (active === undefined || active.size === 0) return undefined
    if (expose !== undefined) {
      const key = `${name}/${expose.slice(2)}`
      if (active.has(key)) return key
    }
    if (active.size === 1) return active.values().next().value
    fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Remote requester context is ambiguous between active exposes', {
      phase: 'module-evaluate', retryable: false,
      details: { name, buildId, active: [...active].sort() },
    })
  }

  #activateContainerEvaluation(name, buildId, evaluationKey) {
    const key = containerKey(name, buildId)
    const active = this.activeContainerEvaluations.get(key) ?? new Set()
    active.add(evaluationKey)
    this.activeContainerEvaluations.set(key, active)
  }

  #deactivateContainerEvaluation(name, buildId, evaluationKey) {
    const key = containerKey(name, buildId)
    const active = this.activeContainerEvaluations.get(key)
    if (active === undefined) return
    active.delete(evaluationKey)
    if (active.size === 0) this.activeContainerEvaluations.delete(key)
  }

  #remote(name) {
    const remote = this.remotes.get(name)
    if (remote === undefined) {
      fail(FEDERATION_ERROR_CODES.UNKNOWN_REMOTE, `Remote ${JSON.stringify(name)} is not registered`, {
        phase: 'resolve', retryable: false, details: { remote: name },
      })
    }
    return remote
  }

  #assertRevision(remote, revision) {
    if (remote.revision !== revision) {
      fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Remote loading was superseded by a newer development build', {
        phase: 'dev-update', retryable: true,
        details: { remote: remote.name, expectedRevision: revision, actualRevision: remote.revision },
      })
    }
  }

  #connectDevUpdateSocket(remote, manifest) {
    if (remote.mode !== 'development' || manifest.development?.updatesUrl === undefined) return false
    const WebSocketConstructor = this.global.WebSocket
    if (typeof WebSocketConstructor !== 'function') return false
    const url = manifest.development.updatesUrl
    const existing = this.devUpdateConnections.get(remote.name)
    if (existing?.url === url && !existing.fatal) return true
    if (existing !== undefined) {
      existing.fatal = true
      const clearTimer = this.global.clearTimeout ?? globalThis.clearTimeout
      if (existing.retryTimer !== null) clearTimer(existing.retryTimer)
      try { existing.socket?.close(1000, 'manifest update endpoint changed') } catch {}
    }
    const state = {
      url,
      socket: null,
      open: false,
      retryAttempt: 0,
      retryTimer: null,
      fatal: false,
      lastSentLeaseSignature: null,
      pendingLeaseSignatures: [],
      acknowledgedBuildId: null,
      acknowledgedGeneration: -1,
    }
    this.devUpdateConnections.set(remote.name, state)
    const scheduleReconnect = () => {
      if (state.fatal || this.devUpdateConnections.get(remote.name) !== state || state.retryTimer !== null) return
      const delay = Math.min(this.devReconnectMs * (2 ** state.retryAttempt), MAX_DEV_RECONNECT_MS)
      state.retryAttempt += 1
      const setTimer = this.global.setTimeout ?? globalThis.setTimeout
      state.retryTimer = setTimer(() => {
        state.retryTimer = null
        open()
      }, delay)
    }
    const open = () => {
      if (state.fatal || this.devUpdateConnections.get(remote.name) !== state) return
      let socket
      try {
        socket = new WebSocketConstructor(url)
      } catch (error) {
        this.global.console?.error?.('[Wake Federation] failed to connect remote update socket', error)
        scheduleReconnect()
        return
      }
      state.socket = socket
      socket.onopen = () => {
        if (state.socket !== socket) return
        state.open = true
        state.retryAttempt = 0
        state.lastSentLeaseSignature = null
        state.pendingLeaseSignatures.length = 0
        state.acknowledgedBuildId = null
        state.acknowledgedGeneration = -1
        this.#syncDevLease(remote)
      }
      socket.onmessage = (event) => {
        if (state.socket !== socket || state.fatal) return
        try {
          if (typeof event?.data !== 'string') throw new TypeError('Federation update frame must be JSON text')
          const message = JSON.parse(event.data)
          if (isRecord(message) && message.schemaVersion === FEDERATION_DEV_LEASE_SCHEMA) {
            const control = normalizeDevLeaseMessage(message)
            if (control.remote !== remote.name || control.type === 'lease') {
              throw new TypeError('Federation dev lease control targets the wrong remote or direction')
            }
            if (control.type === 'lease-ack') {
              if (JSON.stringify(control.buildIds) !== state.pendingLeaseSignatures.shift()) {
                throw new TypeError('Federation dev lease ack does not match the last replacement')
              }
              const accepted = acceptedDevelopmentCursor(remote)
              if (accepted === null || control.currentBuildId !== accepted.currentBuildId ||
                  control.generation !== accepted.generation) {
                state.fatal = true
                this.#requestDevFullReload(remote, 'update-lagged')
                try { socket.close(1000, 'federation development cursor diverged') } catch {}
                return
              }
              remote.devBuildId = control.currentBuildId
              remote.devGeneration = control.generation
              state.acknowledgedBuildId = control.currentBuildId
              state.acknowledgedGeneration = control.generation
              return
            }
            state.fatal = true
            this.#requestDevFullReload(remote, control.reason)
            try { socket.close(1000, 'federation snapshot lease expired') } catch {}
            return
          }
          const update = this.applyDevUpdate(message)
          this.#dispatchDevUpdateAction(update)
        } catch (error) {
          state.fatal = true
          this.global.console?.error?.('[Wake Federation] rejected remote update frame', error)
          try { socket.close(1008, 'invalid federation update') } catch {}
        }
      }
      socket.onerror = () => {
        try { socket.close() } catch {}
      }
      socket.onclose = () => {
        if (state.socket === socket) {
          state.socket = null
          state.open = false
        }
        scheduleReconnect()
      }
    }
    open()
    return true
  }

  #syncDevLease(remote) {
    if (remote.mode !== 'development') return false
    const buildIds = [...remote.activeDevBuildIds].sort()
    if (buildIds.length === 0) return false
    if (buildIds.length > FEDERATION_DEV_MAX_BUILD_LEASES) {
      this.#requestDevFullReload(remote, 'lease-limit')
      return false
    }
    const cursor = acceptedDevelopmentCursor(remote)
    if (cursor === null) return false
    const state = this.devUpdateConnections.get(remote.name)
    if (state === undefined || state.fatal || !state.open || typeof state.socket?.send !== 'function') return false
    const lease = normalizeDevLeaseMessage({
      type: 'lease',
      schemaVersion: FEDERATION_DEV_LEASE_SCHEMA,
      remote: remote.name,
      buildIds,
    })
    const buildIdsSignature = JSON.stringify(lease.buildIds)
    const signature = JSON.stringify([lease.buildIds, cursor.currentBuildId, cursor.generation])
    if (signature === state.lastSentLeaseSignature) return true
    try {
      state.socket.send(JSON.stringify(lease))
      state.lastSentLeaseSignature = signature
      state.pendingLeaseSignatures.push(buildIdsSignature)
      return true
    } catch (error) {
      this.global.console?.error?.('[Wake Federation] failed to replace remote snapshot lease', error)
      try { state.socket.close() } catch {}
      return false
    }
  }

  #requestDevFullReload(remote, reason) {
    if (remote.devReloadRequested) return
    remote.devReloadRequested = true
    this.#trace(remote, 'dev-lease', 'full-reload', { reason })
    this.global.location?.reload?.()
  }

  #dispatchDevUpdateAction(update) {
    const remote = this.#remote(update.remote)
    if (remote.lastDispatchedDevUpdate === update) return
    remote.lastDispatchedDevUpdate = update
    if (update.action === 'types-only') return
    if (update.action === 'full-reload') {
      this.global.location?.reload?.()
      return
    }
    const EventConstructor = this.global.CustomEvent ?? globalThis.CustomEvent
    if (typeof EventConstructor === 'function' && typeof this.global.dispatchEvent === 'function') {
      this.global.dispatchEvent(new EventConstructor(FEDERATION_ISOLATED_REMOUNT_EVENT, { detail: update }))
    } else {
      this.global.location?.reload?.()
    }
  }

  async #withTimeout(operation, milliseconds, phase, details) {
    const Controller = this.global.AbortController ?? globalThis.AbortController
    const controller = Controller === undefined ? null : new Controller()
    let timer
    try {
      return await Promise.race([
        operation(controller?.signal),
        new Promise((_, reject) => {
          timer = setTimeout(() => {
            controller?.abort()
            reject(new FederationError(FEDERATION_ERROR_CODES.TIMEOUT, `${phase} timed out`, {
              phase, retryable: true, details: { ...details, timeoutMs: milliseconds },
            }))
          }, milliseconds)
        }),
      ])
    } finally {
      clearTimeout(timer)
    }
  }

  async #manifest(remote) {
    if (remote.fatalError !== null) throw remote.fatalError
    if (remote.manifest !== null) return remote.manifest
    if (remote.manifestFlight !== null) return remote.manifestFlight
    const revision = remote.revision
    const controlAtStart = Object.freeze({
      generation: remote.devGeneration,
      buildId: remote.devBuildId,
    })
    const flight = (async () => {
      this.#trace(remote, 'manifest-fetch', 'started')
      let result
      try {
        result = await this.#withTimeout(
          (signal) => this.transport.fetchManifest(remote.manifestUrl, {
            signal,
            mode: remote.mode,
            maxManifestSize: remote.maxManifestSize,
          }),
          remote.timeoutMs,
          'manifest-fetch',
          { name: remote.name, url: remote.manifestUrl },
        )
      } catch (cause) {
        if (cause instanceof FederationError) throw cause
        throw new FederationError(FEDERATION_ERROR_CODES.MANIFEST_FETCH, 'Federation manifest request failed', {
          phase: 'manifest-fetch', retryable: true, cause,
          details: { name: remote.name, url: remote.manifestUrl },
        })
      }
      const wrapped = isRecord(result) && hasOwn(result, 'manifest')
      const rawManifest = wrapped ? result.manifest : result
      const rawBytes = wrapped ? toBytes(result.rawBytes) : null
      this.#assertRevision(remote, revision)
      if (remote.manifestIntegrity !== undefined) {
        if (rawBytes === null && result?.verifiedIntegrity !== true) {
          fail(FEDERATION_ERROR_CODES.MANIFEST_INTEGRITY, 'Transport did not provide bytes for manifest integrity verification', {
            phase: 'manifest-validate', retryable: false, details: { name: remote.name },
          })
        }
        if (rawBytes !== null) {
          await verifyIntegrity(rawBytes, remote.manifestIntegrity, this.global, FEDERATION_ERROR_CODES.MANIFEST_INTEGRITY, {
            name: remote.name, url: remote.manifestUrl,
          })
        }
      }
      this.#assertRevision(remote, revision)
      const manifest = normalizeManifest(rawManifest, remote)
      this.#assertRevision(remote, revision)
      this.#acceptDevelopmentManifest(remote, manifest, controlAtStart)
      this.#rememberAcceptedManifest(remote, manifest)
      remote.manifest = manifest
      this.#connectDevUpdateSocket(remote, manifest)
      this.#trace(remote, 'manifest-validate', 'accepted', { buildId: manifest.buildId })
      return manifest
    })()
    remote.manifestFlight = flight
    try {
      return await flight
    } catch (error) {
      const superseded = remote.revision !== revision
      const normalized = superseded
        ? new FederationError(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Remote manifest loading was superseded by a newer development build', {
          phase: 'dev-update', retryable: true, cause: error,
          details: { remote: remote.name, expectedRevision: revision, actualRevision: remote.revision },
        })
        : error instanceof FederationError
          ? error
          : new FederationError(FEDERATION_ERROR_CODES.MANIFEST_FETCH, 'Federation manifest loading failed', {
            phase: 'manifest-fetch', retryable: true, cause: error, details: { name: remote.name },
          })
      if (!superseded && !normalized.retryable) remote.fatalError = normalized
      this.#traceError(remote, 'manifest', normalized)
      throw normalized
    } finally {
      if (remote.manifestFlight === flight) remote.manifestFlight = null
    }
  }

  async #manifestForBuild(remote, buildId) {
    const accepted = remote.acceptedManifests.get(buildId)
    if (accepted !== undefined) return accepted
    const manifest = await this.#manifest(remote)
    if (manifest.buildId === buildId) return manifest
    fail(FEDERATION_ERROR_CODES.ASSET_INTEGRITY, 'Federated asset request buildId does not match an accepted manifest', {
      phase: 'asset-resolve', retryable: false,
      details: {
        name: remote.name,
        buildId,
        currentBuildId: manifest.buildId,
        acceptedBuildIds: [...remote.acceptedManifests.keys()].sort(),
      },
    })
  }

  #rememberAcceptedManifest(remote, manifest) {
    const signature = immutableManifestSignature(manifest)
    const acceptedSignature = remote.acceptedManifestSignatures.get(manifest.buildId)
    if (acceptedSignature !== undefined && acceptedSignature !== signature) {
      fail(FEDERATION_ERROR_CODES.ASSET_INTEGRITY, 'A buildId resolved to conflicting immutable manifest metadata', {
        phase: 'manifest-validate', retryable: false,
        details: { name: remote.name, buildId: manifest.buildId },
      })
    }
    if (remote.mode === 'development' && !remote.activeDevBuildIds.has(manifest.buildId)) {
      if (remote.activeDevBuildIds.size >= FEDERATION_DEV_MAX_BUILD_LEASES) {
        this.#requestDevFullReload(remote, 'lease-limit')
        fail(FEDERATION_ERROR_CODES.CONFIG_INVALID, 'Development page exceeded its bounded active build lease set', {
          phase: 'dev-lease', retryable: false,
          details: { remote: remote.name, maximum: FEDERATION_DEV_MAX_BUILD_LEASES },
        })
      }
      remote.activeDevBuildIds.add(manifest.buildId)
    }
    if (acceptedSignature === undefined) {
      remote.acceptedManifestSignatures.set(manifest.buildId, signature)
      remote.acceptedManifests.set(manifest.buildId, manifest)
    }
    this.#syncDevLease(remote)
  }

  #acceptDevelopmentManifest(remote, manifest, controlAtStart) {
    if (remote.mode !== 'development') return
    const generation = manifest.development?.generation
    if (controlAtStart.generation >= 0) {
      if (generation === undefined || generation < controlAtStart.generation) {
        fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'Development manifest generation is older than the accepted update', {
          phase: 'manifest-validate', retryable: false,
          details: { remote: remote.name, minimum: controlAtStart.generation, actual: generation },
        })
      }
      if (generation === controlAtStart.generation && controlAtStart.buildId !== null && manifest.buildId !== controlAtStart.buildId) {
        fail(FEDERATION_ERROR_CODES.MANIFEST_SCHEMA, 'Development manifest buildId does not match its generation', {
          phase: 'manifest-validate', retryable: false,
          details: { remote: remote.name, generation, expected: controlAtStart.buildId, actual: manifest.buildId },
        })
      }
    }
    if (generation !== undefined && generation > remote.devGeneration) {
      remote.devGeneration = generation
      remote.devBuildId = manifest.buildId
    } else if (generation !== undefined && generation === remote.devGeneration && remote.devBuildId === null) {
      remote.devBuildId = manifest.buildId
    } else if (generation === undefined && remote.devGeneration < 0 && remote.devBuildId === null) {
      remote.devBuildId = manifest.buildId
    }
  }

  async #container(remote, manifest) {
    const key = containerKey(manifest.name, manifest.buildId)
    const ready = this.containers.get(key)
    if (ready?.initialized) return ready
    const existingFlight = this.containerFlights.get(key)
    if (existingFlight !== undefined) return existingFlight
    const flight = this.#initializeContainer(remote, manifest)
    this.containerFlights.set(key, flight)
    try {
      return await flight
    } finally {
      if (this.containerFlights.get(key) === flight) this.containerFlights.delete(key)
    }
  }

  async #initializeContainer(remote, manifest) {
    const key = containerKey(manifest.name, manifest.buildId)
    let state = this.containers.get(key)
    if (state?.initialized) return state
    if (state?.initFlight !== null && state?.initFlight !== undefined) return state.initFlight
    if (state === undefined) {
      this.#trace(remote, 'entry-load', 'started', { buildId: manifest.buildId })
      let returned
      try {
        returned = await this.#withTimeout(
          (signal) => this.transport.loadScript(manifest.remoteEntry, {
            signal,
            runtime: this,
            manifest,
            remote,
            maxAssetSize: remote.maxAssetSize,
            assetContext: runtimeAssetContext(remote, manifest),
          }),
          remote.timeoutMs,
          'entry-load',
          { name: remote.name, buildId: manifest.buildId, url: manifest.remoteEntry.url },
        )
      } catch (cause) {
        if (cause instanceof FederationError) throw cause
        throw new FederationError(FEDERATION_ERROR_CODES.NETWORK, 'Remote entry failed to load', {
          phase: 'entry-load', retryable: true, cause,
          details: { name: remote.name, buildId: manifest.buildId },
        })
      }
      if (returned !== undefined) {
        const container = isRecord(returned) && hasOwn(returned, 'container') ? returned.container : returned
        this.registerContainer({ name: manifest.name, buildId: manifest.buildId, container })
      }
      state = this.containers.get(key)
      if (state === undefined) {
        fail(FEDERATION_ERROR_CODES.CONTAINER_REGISTRATION, 'Remote entry did not register its container', {
          phase: 'entry-load', retryable: false,
          details: { name: manifest.name, buildId: manifest.buildId },
        })
      }
      this.#trace(remote, 'entry-load', 'registered', { buildId: manifest.buildId })
    }
    if (state.initFlight !== null) return state.initFlight
    const initFlight = (async () => {
      this.#registerRemoteOffers(state, manifest, remote)
      const requester = containerKey(manifest.name, manifest.buildId)
      const resolved = {}
      const requests = manifest.shared.requirements.map((rawRequest) => normalizeSharedRequest(rawRequest))
      const coherenceOwners = this.#planCoherenceOwners(requests, { requester, currentRemoteKey: requester })
      for (const request of requests) {
        const forcedOwner = request.coherenceGroup === undefined
          ? undefined
          : coherenceOwners.get(coherenceBucketKey(request.scope, request.coherenceGroup))
        resolved[`${request.scope}:${request.shareKey}`] = await this.resolveShared(request, {
          requester,
          currentRemoteKey: requester,
          forcedOwner,
        })
      }
      const context = Object.freeze({
        runtimeAbi: FEDERATION_RUNTIME_ABI,
        container: Object.freeze({ name: manifest.name, buildId: manifest.buildId }),
        resolved: Object.freeze(resolved),
        resolve: (request) => this.resolveShared(request, { requester, currentRemoteKey: requester }),
        getSync: (shareKey, scope = DEFAULT_SCOPE) => {
          const resolvedKey = `${scope}:${shareKey}`
          if (!hasOwn(resolved, resolvedKey)) {
            fail(FEDERATION_ERROR_CODES.SHARE_UNSATISFIABLE, 'Shared dependency was not resolved during container initialization', {
              phase: 'share-get-sync', retryable: false,
              details: { shareKey, scope, name: manifest.name, buildId: manifest.buildId },
            })
          }
          return resolved[resolvedKey]
        },
      })
      try {
        await state.container.init(context)
      } catch (cause) {
        if (cause instanceof FederationError) throw cause
        throw new FederationError(FEDERATION_ERROR_CODES.CONTAINER_INIT, 'Remote container init() failed', {
          phase: 'container-init', retryable: false, cause,
          details: { name: manifest.name, buildId: manifest.buildId },
        })
      }
      state.initialized = true
      this.#trace(remote, 'container-init', 'ready', { buildId: manifest.buildId })
      return state
    })()
    state.initFlight = initFlight
    try {
      return await initFlight
    } catch (error) {
      // A transient shared fallback/network failure must not poison this container forever.
      // Non-retryable init failures remain frozen for the lifetime of the page.
      if (error?.retryable && state.initFlight === initFlight) state.initFlight = null
      throw error
    }
  }

  #registerRemoteOffers(state, manifest, remote) {
    if (state.offersRegistered) return
    const ownerKey = containerKey(manifest.name, manifest.buildId)
    for (const offer of manifest.shared.offers) {
      const get = async () => {
        if (offer.asset !== undefined) await this.#script(offer.asset, remote, manifest)
        if (typeof state.container.getShared === 'function') return state.container.getShared(offer.shareKey ?? offer.name)
        const expose = offer.expose ?? `./__wake_shared__/${offer.shareKey ?? offer.name}`
        const factory = await state.container.get(expose)
        if (typeof factory !== 'function') {
          fail(FEDERATION_ERROR_CODES.CONTAINER_GET, 'Shared fallback did not return a module factory', {
            phase: 'share-load', retryable: false, details: { owner: ownerKey, expose },
          })
        }
        return factory()
      }
      const provider = normalizeProvider(offer, {
        host: false,
        ownerKey,
        ownerName: offer.provider ?? manifest.name,
        sequence: this.sequence++,
        get,
      })
      this.#addProvider(provider)
    }
    state.offersRegistered = true
  }

  #addProvider(provider) {
    const key = shareBucketKey(provider.scope, provider.shareKey)
    const entries = this.providers.get(key) ?? []
    const identity = providerIdentity(provider)
    if (!entries.some((entry) => providerIdentity(entry) === identity)) entries.push(provider)
    this.providers.set(key, entries)
  }

  #planCoherenceOwners(requests, context) {
    const groups = new Map()
    for (const request of requests) {
      if (request.coherenceGroup === undefined) continue
      const key = coherenceBucketKey(request.scope, request.coherenceGroup)
      const entries = groups.get(key) ?? []
      entries.push(request)
      groups.set(key, entries)
    }
    const result = new Map()
    for (const [groupKey, groupRequests] of groups) {
      const existing = this.coherenceLocks.get(groupKey)
      let commonOwners = null
      const ownerScores = new Map()
      for (const request of groupRequests) {
        const singleton = this.singletonLocks.get(shareBucketKey(request.scope, request.shareKey))
        let candidates = singleton === undefined
          ? (this.providers.get(shareBucketKey(request.scope, request.shareKey)) ?? [])
          : [singleton]
        candidates = candidates.filter((provider) =>
          (request.packageContext === undefined || request.packageContext === provider.packageContext) &&
          (request.buildVariant === undefined || request.buildVariant === provider.buildVariant) &&
          (request.owner === undefined || request.owner === provider.ownerName || request.owner === provider.ownerKey))
        const compatible = candidates.filter((provider) => satisfiesRange(provider.version, request.requiredVersion))
        candidates = compatible
        const bucket = (provider) => sharedProviderPriority(provider, request, context)
        candidates = candidates.filter((provider) => bucket(provider) < 3)
        if (existing !== undefined) candidates = candidates.filter((provider) => provider.ownerKey === existing.ownerKey)
        const owners = new Set(candidates.map((provider) => provider.ownerKey))
        commonOwners = commonOwners === null
          ? owners
          : new Set([...commonOwners].filter((owner) => owners.has(owner)))
        for (const owner of owners) {
          const best = Math.min(...candidates.filter((provider) => provider.ownerKey === owner).map(bucket))
          const score = ownerScores.get(owner) ?? { maximum: 0, total: 0 }
          score.maximum = Math.max(score.maximum, best)
          score.total += best
          ownerScores.set(owner, score)
        }
      }
      const selectedOwner = [...(commonOwners ?? [])].sort((left, right) => {
        const leftScore = ownerScores.get(left)
        const rightScore = ownerScores.get(right)
        return leftScore.maximum - rightScore.maximum || leftScore.total - rightScore.total || left.localeCompare(right)
      })[0]
      if (selectedOwner === undefined) {
        fail(FEDERATION_ERROR_CODES.COHERENCE_CONFLICT, 'No provider owner can satisfy the complete coherence group', {
          phase: 'share-resolve', retryable: false,
          details: {
            group: groupRequests[0].coherenceGroup,
            scope: groupRequests[0].scope,
            shares: groupRequests.map((request) => request.shareKey),
          },
        })
      }
      result.set(groupKey, selectedOwner)
    }
    return result
  }

  async #resolveShared(request, context) {
    const key = shareBucketKey(request.scope, request.shareKey)
    const locked = this.singletonLocks.get(key)
    if (locked !== undefined) {
      if (request.owner !== undefined && request.owner !== locked.ownerName && request.owner !== locked.ownerKey) {
        fail(FEDERATION_ERROR_CODES.SHARE_SINGLETON_CONFLICT, 'Frozen singleton does not match the required owner', {
          phase: 'share-resolve', retryable: false,
          details: { shareKey: request.shareKey, scope: request.scope, expectedOwner: request.owner, actualOwner: locked.ownerKey },
        })
      }
      if ((request.packageContext !== undefined && request.packageContext !== locked.packageContext) ||
          (request.buildVariant !== undefined && request.buildVariant !== locked.buildVariant)) {
        fail(FEDERATION_ERROR_CODES.SHARE_SINGLETON_CONFLICT, 'Frozen singleton has an incompatible package context or build variant', {
          phase: 'share-resolve', retryable: false,
          details: {
            shareKey: request.shareKey,
            expectedPackageContext: request.packageContext,
            actualPackageContext: locked.packageContext,
            expectedBuildVariant: request.buildVariant,
            actualBuildVariant: locked.buildVariant,
          },
        })
      }
      if (context.forcedOwner !== undefined && context.forcedOwner !== locked.ownerKey) {
        fail(FEDERATION_ERROR_CODES.COHERENCE_CONFLICT, 'Frozen singleton conflicts with the planned coherence owner', {
          phase: 'share-resolve', retryable: false,
          details: { shareKey: request.shareKey, expectedOwner: context.forcedOwner, actualOwner: locked.ownerKey },
        })
      }
      if (!satisfiesRange(locked.version, request.requiredVersion)) {
        fail(FEDERATION_ERROR_CODES.SHARE_SINGLETON_CONFLICT, 'Frozen singleton does not satisfy the requested version', {
          phase: 'share-resolve', retryable: false,
          details: { shareKey: request.shareKey, scope: request.scope, requested: request.requiredVersion, locked: locked.version, owner: locked.ownerKey },
        })
      }
      if (request.coherenceGroup !== undefined) this.#assertCoherence(request, locked)
      return this.#loadProvider(locked, request, context)
    }
    const allCandidates = this.providers.get(key) ?? []
    let candidates = allCandidates.filter((provider) =>
      (request.packageContext === undefined || request.packageContext === provider.packageContext) &&
      (request.buildVariant === undefined || request.buildVariant === provider.buildVariant) &&
      (request.owner === undefined || request.owner === provider.ownerName || request.owner === provider.ownerKey))
    if (context.forcedOwner !== undefined) candidates = candidates.filter((provider) => provider.ownerKey === context.forcedOwner)
    const compatible = candidates.filter((provider) => satisfiesRange(provider.version, request.requiredVersion))
    candidates = compatible
    const coherenceKey = request.coherenceGroup === undefined
      ? undefined
      : coherenceBucketKey(request.scope, request.coherenceGroup)
    const coherence = coherenceKey === undefined ? undefined : this.coherenceLocks.get(coherenceKey)
    if (coherence !== undefined) candidates = candidates.filter((provider) => provider.ownerKey === coherence.ownerKey)
    const currentRemoteKey = context.currentRemoteKey
    const bucket = (provider) => sharedProviderPriority(provider, request, context)
    candidates = candidates.filter((provider) => bucket(provider) < 3)
    candidates.sort((left, right) => {
      const priority = bucket(left) - bucket(right)
      if (priority !== 0) return priority
      const version = compareParsedVersions(right.parsedVersion, left.parsedVersion)
      if (version !== 0) return version
      const identity = providerIdentity(left).localeCompare(providerIdentity(right))
      return identity !== 0 ? identity : left.sequence - right.sequence
    })
    const selected = candidates[0]
    if (selected === undefined) {
      const code = coherence === undefined
        ? FEDERATION_ERROR_CODES.SHARE_UNSATISFIABLE
        : FEDERATION_ERROR_CODES.COHERENCE_CONFLICT
      fail(code, 'No compatible shared dependency provider is available', {
        phase: 'share-resolve', retryable: false,
        details: {
          shareKey: request.shareKey,
          scope: request.scope,
          requested: request.requiredVersion,
          requestedPackageContext: request.packageContext,
          requestedBuildVariant: request.buildVariant,
          requestedOwner: request.owner,
          currentRemote: currentRemoteKey,
          coherenceGroup: request.coherenceGroup,
          candidates: sharedCandidateDiagnostics(allCandidates, request, context, coherence),
        },
      })
    }
    const value = await this.#loadProvider(selected, request, context)
    if (request.singleton || selected.singleton) this.singletonLocks.set(key, selected)
    if (request.coherenceGroup !== undefined) {
      const coherenceKey = coherenceBucketKey(request.scope, request.coherenceGroup)
      const existing = this.coherenceLocks.get(coherenceKey)
      if (existing !== undefined && existing.ownerKey !== selected.ownerKey) {
        fail(FEDERATION_ERROR_CODES.COHERENCE_CONFLICT, 'Coherence group is already frozen to another provider owner', {
          phase: 'share-resolve', retryable: false,
          details: { group: request.coherenceGroup, expectedOwner: existing.ownerKey, actualOwner: selected.ownerKey },
        })
      }
      this.coherenceLocks.set(coherenceKey, { ownerKey: selected.ownerKey })
    }
    return value
  }

  #assertCoherence(request, provider) {
    const lock = this.coherenceLocks.get(coherenceBucketKey(request.scope, request.coherenceGroup))
    if (lock !== undefined && lock.ownerKey !== provider.ownerKey) {
      fail(FEDERATION_ERROR_CODES.COHERENCE_CONFLICT, 'Frozen singleton conflicts with the coherence group owner', {
        phase: 'share-resolve', retryable: false,
        details: { group: request.coherenceGroup, expectedOwner: lock.ownerKey, actualOwner: provider.ownerKey },
      })
    }
  }

  async #loadProvider(provider, request, context) {
    if (provider.fatalError !== null) throw provider.fatalError
    if (provider.loaded && provider.promise === null) {
      this.#recordShareDecision(provider, request, context, true)
      return provider.module
    }
    if (provider.promise === null) {
      provider.promise = Promise.resolve().then(() => provider.get()).then((moduleValue) => {
        provider.module = moduleValue
        provider.loaded = true
        return moduleValue
      })
    }
    try {
      const value = await provider.promise
      this.#recordShareDecision(provider, request, context, false)
      return value
    } catch (cause) {
      const error = new FederationError(FEDERATION_ERROR_CODES.SHARE_UNSATISFIABLE, 'Shared dependency provider failed to load', {
        phase: 'share-load', retryable: cause instanceof FederationError ? cause.retryable : true,
        cause,
        details: { shareKey: request.shareKey, version: provider.version, owner: provider.ownerKey },
      })
      if (error.retryable) provider.promise = null
      else provider.fatalError = error
      throw error
    }
  }

  #recordShareDecision(provider, request, context, cacheHit) {
    this.sharedDecisions.push(Object.freeze({
      requester: context.requester,
      shareKey: request.shareKey,
      scope: request.scope,
      requested: request.requiredVersion,
      selected: provider.version,
      owner: provider.ownerKey,
      source: provider.host ? 'host' : provider.loaded && cacheHit ? 'loaded' : 'fallback',
      cacheHit,
    }))
  }

  #isolatedStyleBucket(manifest, expose, remote) {
    const generation = runtimeGeneration(remote, manifest)
    const key = [manifest.name, manifest.buildId, expose, generation].join('\0')
    let bucket = this.isolatedStyleBuckets.get(key)
    if (bucket === undefined) {
      bucket = {
        name: manifest.name,
        buildId: manifest.buildId,
        expose,
        generation,
        remote,
        assets: [],
        assetKeys: new Set(),
        targets: new Map(),
      }
      this.isolatedStyleBuckets.set(key, bucket)
    }
    return bucket
  }

  #rememberIsolatedStyles(bucket, assets) {
    for (const asset of assets) {
      const key = `${asset.url}\0${asset.integrity}`
      if (bucket.assetKeys.has(key)) continue
      bucket.assetKeys.add(key)
      bucket.assets.push(asset)
    }
  }

  async #loadIsolatedStyleForTarget(bucket, target, asset) {
    const key = `${asset.url}\0${asset.integrity}`
    let flight = target.flights.get(key)
    if (flight === undefined) {
      flight = target.tail.then(async () => {
        if (typeof this.transport.loadStyle !== 'function') {
          fail(FEDERATION_ERROR_CODES.STYLE_LOAD, 'The federation transport cannot load isolated styles', {
            phase: 'style-target', retryable: false,
            details: { name: bucket.name, buildId: bucket.buildId, expose: bucket.expose, url: asset.url },
          })
        }
        const loaded = await this.#withTimeout(
          (signal) => this.transport.loadStyle(asset, {
            signal,
            maxAssetSize: bucket.remote.maxAssetSize,
            styleTarget: target.root,
            assetContext: Object.freeze({
              name: bucket.name,
              buildId: bucket.buildId,
              generation: bucket.generation,
              development: bucket.remote.mode === 'development',
              expose: bucket.expose,
            }),
          }),
          bucket.remote.timeoutMs,
          'style-load',
          { name: bucket.name, buildId: bucket.buildId, expose: bucket.expose, url: asset.url },
        )
        if (loaded !== undefined && typeof loaded?.remove === 'function') {
          if (target.active && target.references > 0) target.nodes.set(key, loaded)
          else loaded.remove()
        }
      })
      target.flights.set(key, flight)
      target.tail = flight.catch(() => {})
    }
    try {
      await flight
    } catch (error) {
      if (error?.retryable && target.flights.get(key) === flight) target.flights.delete(key)
      throw error
    }
  }

  async #hydrateIsolatedStyleTarget(bucket, target) {
    for (let index = 0; index < bucket.assets.length; index += 1) {
      await this.#loadIsolatedStyleForTarget(bucket, target, bucket.assets[index])
    }
  }

  async #loadIsolatedStyles(bucket, assets) {
    const targets = [...bucket.targets.values()].filter((target) => target.active && target.references > 0)
    if (targets.length === 0) {
      fail(FEDERATION_ERROR_CODES.STYLE_LOAD, 'Isolated stylesheet requested without an active ShadowRoot', {
        phase: 'style-target', retryable: false,
        details: { name: bucket.name, buildId: bucket.buildId, expose: bucket.expose },
      })
    }
    this.#rememberIsolatedStyles(bucket, assets)
    await Promise.all(targets.map((target) => this.#hydrateIsolatedStyleTarget(bucket, target)))
  }

  #releaseIsolatedStyleTarget(bucket, target) {
    if (target.references > 0) target.references -= 1
    if (target.references > 0) return
    target.active = false
    if (bucket.targets.get(target.root) === target) bucket.targets.delete(target.root)
    for (const node of [...target.nodes.values()].reverse()) node.remove()
    target.nodes.clear()
  }

  async #styles(assets, remote, manifest) {
    if (typeof this.transport.loadStyle !== 'function') return
    for (const asset of assets) {
      const key = `${asset.url}\0${asset.integrity}`
      let flight = this.styleFlights.get(key)
      if (flight === undefined) {
        flight = this.#withTimeout(
          (signal) => this.transport.loadStyle(asset, {
            signal,
            maxAssetSize: remote.maxAssetSize,
            assetContext: runtimeAssetContext(remote, manifest),
          }),
          remote.timeoutMs,
          'style-load',
          { url: asset.url },
        )
        this.styleFlights.set(key, flight)
      }
      try {
        await flight
      } catch (error) {
        if (error?.retryable && this.styleFlights.get(key) === flight) this.styleFlights.delete(key)
        throw error
      }
    }
  }

  async #exposeAssets(exposeKey, expose, remote, manifest) {
    for (const asset of expose.synchronousAssets) {
      if (asset.kind === 'javascript') await this.#script(asset, remote, manifest)
    }
    await this.#script(expose.entry, remote, manifest, exposeKey)
    if (expose.mode !== 'isolated') {
      const synchronousStyles = expose.synchronousAssets.filter((asset) => asset.kind === 'css')
      await this.#styles([...synchronousStyles, ...expose.css], remote, manifest)
    }
  }

  async #script(asset, remote, manifest, expose) {
    const generation = runtimeGeneration(remote, manifest)
    const key = [asset.url, asset.integrity, manifest.name, manifest.buildId, generation, expose ?? ''].join('\0')
    let flight = this.scriptFlights.get(key)
    if (flight === undefined) {
      flight = this.#withTimeout(
        (signal) => this.transport.loadScript(asset, {
          signal,
          runtime: this,
          manifest,
          remote,
          maxAssetSize: remote.maxAssetSize,
          assetContext: runtimeAssetContext(remote, manifest, expose),
        }),
        remote.timeoutMs,
        'asset-load',
        { url: asset.url },
      )
      this.scriptFlights.set(key, flight)
    }
    try {
      await flight
    } catch (error) {
      if (error?.retryable && this.scriptFlights.get(key) === flight) this.scriptFlights.delete(key)
      throw error
    }
  }

  #unknownExpose(remote, expose) {
    fail(FEDERATION_ERROR_CODES.UNKNOWN_EXPOSE, `Remote ${JSON.stringify(remote.name)} does not expose ${JSON.stringify(expose)}`, {
      phase: 'resolve', retryable: false, details: { remote: remote.name, expose },
    })
  }

  #trace(remote, phase, outcome, details = {}) {
    remote.trace.push(Object.freeze({ phase, outcome, details: Object.freeze({ ...details }) }))
  }

  #traceError(remote, phase, error, details = {}) {
    this.#trace(remote, phase, 'error', {
      ...details,
      code: error instanceof FederationError ? error.code : undefined,
      retryable: error instanceof FederationError ? error.retryable : undefined,
    })
  }

  #rememberFatal(remote, error, revision) {
    if (revision !== undefined && remote.revision !== revision) return
    if (!(error instanceof FederationError) || error.retryable) return
    if (FATAL_REMOTE_CODES.has(error.code)) remote.fatalError = error
  }
}

export function createFederationRuntime(options = {}) {
  return new FederationRuntime(options)
}

export const createFederationBroker = createFederationRuntime

export function getFederationRuntime(options = {}) {
  const targetGlobal = options.global ?? globalThis
  assertBrowserGlobal(targetGlobal)
  const existing = targetGlobal[RUNTIME_SYMBOL]
  if (existing !== undefined) {
    if (existing.runtimeAbi !== FEDERATION_RUNTIME_ABI ||
        typeof existing.registerRemote !== 'function' ||
        typeof existing.loadRemote !== 'function' ||
        typeof existing.loadFederatedAsset !== 'function' ||
        typeof existing.attachIsolatedStyleTarget !== 'function' ||
        typeof existing.describeRemote !== 'function' ||
        typeof existing.prepareRemote !== 'function' ||
        typeof existing.applyDevUpdate !== 'function') {
      fail(FEDERATION_ERROR_CODES.RUNTIME_ABI, 'The Window already contains an incompatible federation broker', {
        phase: 'environment', retryable: false,
        details: { expected: FEDERATION_RUNTIME_ABI, actual: existing.runtimeAbi },
      })
    }
    return existing
  }
  const runtime = createFederationRuntime({ ...options, global: targetGlobal })
  Object.defineProperty(targetGlobal, RUNTIME_SYMBOL, {
    configurable: false,
    enumerable: false,
    writable: false,
    value: runtime,
  })
  return runtime
}

export const getFederationBroker = getFederationRuntime

export {
  diagnoseNativeAssetFailure as __diagnoseFederatedAssetFailure,
  FEDERATION_DEV_LEASE_SCHEMA,
  FEDERATION_DEV_MAX_BUILD_LEASES,
  FEDERATION_DEV_UPDATE_SCHEMA,
  FEDERATION_ERROR_CODES,
  FEDERATION_ISOLATED_REMOUNT_EVENT,
  FEDERATION_MANIFEST_SCHEMA,
  FEDERATION_RUNTIME_ABI,
  preflightAsset as __preflightFederatedAsset,
}
