import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { createHash, webcrypto } from 'node:crypto'
import { readFile } from 'node:fs/promises'
import { test } from 'node:test'

import {
  __diagnoseFederatedAssetFailure,
  __preflightFederatedAsset,
  FEDERATION_DEV_LEASE_SCHEMA,
  FEDERATION_DEV_MAX_BUILD_LEASES,
  FEDERATION_ERROR_CODES,
  FederationError,
  createFederationRuntime,
  getFederationRuntime,
} from '../federation.mjs'

const integrity = 'sha384-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA'
const rustSharedOnlyRemoteRefFixture = Object.freeze({
  manifestUrl: 'https://catalog.test/manifest.json',
  buildId: 'build-a',
  manifestIntegrity: integrity,
  hasExposes: false,
  allowedAssets: Object.freeze({}),
})

test('runtime and declarations expose exactly the Rust federation error codes', async () => {
  const [rustSource, declarationSource] = await Promise.all([
    readFile(
      new URL('../../../crates/wake_federation_contract/src/error.rs', import.meta.url),
      'utf8',
    ),
    readFile(new URL('../federation.d.mts', import.meta.url), 'utf8'),
  ])
  const rustCodes = [...rustSource.matchAll(/#\[serde\(rename = "(FED_[A-Z0-9_]+)"\)\]/g)]
    .map((match) => match[1])
    .sort()
  const runtimeCodes = Object.values(FEDERATION_ERROR_CODES).sort()
  const declarationCodes = [...new Set(
    [...declarationSource.matchAll(/'(FED_[A-Z0-9_]+)'/g)].map((match) => match[1]),
  )].sort()

  assert.ok(rustCodes.length > 0, 'the Rust ErrorCode authority must remain machine-readable')
  assert.deepEqual(runtimeCodes, rustCodes)
  assert.deepEqual(declarationCodes, rustCodes)
})

function asset(url, kind = 'javascript', mime = 'text/javascript') {
  return { kind, url, contentHash: `hash:${url}`, integrity, size: 1, mime }
}

function expose(url, mode = 'generic') {
  return {
    mode,
    scope: 'default',
    shadow: mode === 'isolated' ? 'open' : 'none',
    entry: asset(url),
    css: [],
    synchronousAssets: [],
    asynchronousAssets: [],
  }
}

function policy(overrides = {}) {
  return {
    scope: 'default',
    singleton: false,
    strict: true,
    fallback: true,
    coherenceGroup: null,
    owner: null,
    ...overrides,
  }
}

function offer(shareKey, version, provider, overrides = {}) {
  return {
    shareKey,
    package: { name: shareKey, version, packageContext: 'npm:root', buildVariant: 'browser' },
    provider,
    policy: policy(overrides),
  }
}

function requirement(shareKey, requiredVersion, overrides = {}) {
  return {
    shareKey,
    requiredVersion,
    packageContext: 'npm:root',
    buildVariant: 'browser',
    policy: policy(overrides),
  }
}

function reactRequirements(scope = 'default', owner = null) {
  return [
    'react',
    'react/jsx-runtime',
    'react/jsx-dev-runtime',
    'react-dom',
    'react-dom/client',
  ].map((shareKey) => requirement(shareKey, '^18.0.0', {
    scope,
    singleton: true,
    coherenceGroup: 'react18',
    owner,
  }))
}

function manifest(name, buildId, options = {}) {
  return {
    schemaVersion: 'wake.federation.manifest.v1',
    runtimeAbi: 'wake.federation.v1',
    name,
    buildId,
    browserTarget: 'chrome >= 120',
    remoteEntry: asset(`https://cdn.test/${name}/${buildId}/remote.mjs`),
    exposes: options.exposes ?? {
      './Button': expose(`https://cdn.test/${name}/${buildId}/button.mjs`),
    },
    shared: {
      offers: options.offers ?? [],
      requirements: options.requirements ?? [],
    },
    development: { updatesUrl: `wss://cdn.test/${name}/updates`, generation: options.generation ?? 0 },
  }
}

function devUpdate(remote, oldBuildId, newBuildId, generation, action = 'isolated-remount') {
  return {
    schemaVersion: 'wake.federation.dev-update.v1',
    remote,
    oldBuildId,
    newBuildId,
    changedExposes: ['./Button'],
    typesHash: `types-${generation}`,
    generation,
    action,
  }
}

function devLeaseAck(remote, buildIds, currentBuildId, generation) {
  return {
    type: 'lease-ack',
    schemaVersion: FEDERATION_DEV_LEASE_SCHEMA,
    remote,
    buildIds,
    currentBuildId,
    generation,
  }
}

function devLeaseReload(remote, currentBuildId, generation, expiredBuildId, reason = 'build-gone') {
  return {
    type: 'full-reload',
    schemaVersion: FEDERATION_DEV_LEASE_SCHEMA,
    remote,
    currentBuildId,
    generation,
    expiredBuildId,
    reason,
  }
}

function createHarness(manifests, containers, hooks = {}) {
  const calls = { fetch: 0, script: 0 }
  const runtime = createFederationRuntime({
    global: hooks.global ?? { location: { href: 'https://host.test/' } },
    transport: {
      async fetchManifest(url) {
        calls.fetch += 1
        if (hooks.fetchManifest !== undefined) return hooks.fetchManifest(url, calls.fetch)
        return manifests.get(url)
      },
      async loadScript(entry, { manifest: loadedManifest }) {
        calls.script += 1
        await hooks.beforeScript?.(entry, loadedManifest)
        return containers.get(`${loadedManifest.name}:${loadedManifest.buildId}`)
      },
      async loadStyle(style, context) {
        return hooks.loadStyle?.(style, context)
      },
    },
  })
  return { runtime, calls }
}

function openShadowRoot(name = 'root') {
  const host = { shadowRoot: null }
  const root = {
    name,
    mode: 'open',
    host,
    children: [],
    append(node) {
      node.parentNode = root
      root.children.push(node)
      queueMicrotask(() => node.onload?.())
    },
  }
  host.shadowRoot = root
  return root
}

function ownedStyleNode(root, url) {
  return {
    url,
    parentNode: null,
    remove() {
      if (this.parentNode === null) return
      const index = this.parentNode.children.indexOf(this)
      if (index >= 0) this.parentNode.children.splice(index, 1)
      this.parentNode = null
    },
  }
}

function sri(body) {
  return `sha384-${createHash('sha384').update(body).digest('base64')}`
}

function streamedResponse(chunks, options = {}) {
  const counters = options.counters ?? {}
  const encoded = chunks.map((chunk) => typeof chunk === 'string' ? new TextEncoder().encode(chunk) : chunk)
  let index = 0
  const reader = {
    async read() {
      counters.reads = (counters.reads ?? 0) + 1
      if (index >= encoded.length) return { done: true, value: undefined }
      return { done: false, value: encoded[index++] }
    },
    async cancel() {
      counters.cancels = (counters.cancels ?? 0) + 1
    },
    releaseLock() {
      counters.releases = (counters.releases ?? 0) + 1
    },
  }
  return {
    ok: options.status === undefined || (options.status >= 200 && options.status < 300),
    status: options.status ?? 200,
    headers: new Headers(options.headers),
    body: {
      getReader() {
        counters.readers = (counters.readers ?? 0) + 1
        return reader
      },
    },
  }
}

test('getFederationRuntime installs one broker at the canonical per-Window symbol', () => {
  const fakeWindow = { document: {}, location: { href: 'https://host.test/' } }
  fakeWindow.window = fakeWindow
  const first = getFederationRuntime({
    global: fakeWindow,
    transport: { fetchManifest() {}, loadScript() {} },
  })
  const second = getFederationRuntime({ global: fakeWindow })
  assert.equal(first, second)
  assert.equal(fakeWindow[Symbol.for('wake.federation.v1')], first)
  assert.equal(typeof first.registerRemote, 'function')
  assert.equal(typeof first.registerHostShared, 'function')
  assert.equal(typeof first.loadRemote, 'function')
  assert.equal(typeof first.attachIsolatedStyleTarget, 'function')
})

test('importing the runtime does not preempt CSP nonce configuration', () => {
  const moduleUrl = new URL('../federation.mjs?csp-no-auto-install', import.meta.url).href
  const script = [
    'globalThis.window = globalThis;',
    'globalThis.document = {};',
    `await import(${JSON.stringify(moduleUrl)});`,
    "if (globalThis[Symbol.for('wake.federation.v1')] !== undefined) throw new Error('runtime auto-installed');",
  ].join('\n')
  execFileSync(process.execPath, ['--input-type=module', '--eval', script], { stdio: 'pipe' })
})

test('remote registration is idempotent but rejects conflicting ownership', () => {
  const runtime = createFederationRuntime({ global: {}, transport: { fetchManifest() {}, loadScript() {} } })
  const registration = { name: 'catalog', manifestUrl: 'https://cdn.test/catalog/manifest.json' }
  assert.equal(runtime.registerRemote(registration), runtime)
  assert.equal(runtime.registerRemote({ ...registration }), runtime)
  assert.throws(
    () => runtime.registerRemote({ ...registration, manifestUrl: 'https://other.test/manifest.json' }),
    (error) => error instanceof FederationError && error.code === FEDERATION_ERROR_CODES.REMOTE_CONFLICT,
  )
})

test('runtime and remote modes fail closed on unknown values', () => {
  const transport = { fetchManifest() {}, loadScript() {} }
  for (const mode of ['prodution', null]) {
    assert.throws(
      () => createFederationRuntime({ global: {}, transport, mode }),
      (error) => error.code === FEDERATION_ERROR_CODES.CONFIG_INVALID &&
        error.phase === 'runtime-config' && error.retryable === false && error.details.field === 'mode',
    )
  }

  const runtime = createFederationRuntime({ global: {}, transport, mode: 'development' })
  for (const mode of ['prodution', null]) {
    assert.throws(
      () => runtime.registerRemote({
        name: 'catalog',
        manifestUrl: 'http://cdn.test/catalog/manifest.json',
        mode,
      }),
      (error) => error.code === FEDERATION_ERROR_CODES.CONFIG_INVALID &&
        error.phase === 'remote-register' && error.retryable === false && error.details.field === 'mode',
    )
  }
  assert.equal(runtime.explain('catalog/Button').status, 'unregistered')
  assert.equal(runtime.registerRemote({
    name: 'catalog',
    manifestUrl: 'http://cdn.test/catalog/manifest.json',
    mode: 'development',
  }), runtime)
  assert.equal(createFederationRuntime({ global: {}, transport, mode: 'production' }).mode, 'production')
})

test('runtime and remote resource limits require bounded positive safe integers', () => {
  const transport = { fetchManifest() {}, loadScript() {} }
  const constructorCases = [
    ['timeoutMs', 0],
    ['timeoutMs', 300_001],
    ['timeoutMs', Number.POSITIVE_INFINITY],
    ['maxManifestSize', -1],
    ['maxManifestSize', 16 * 1024 * 1024 + 1],
    ['maxManifestSize', 1.5],
    ['maxAssetSize', null],
    ['maxAssetSize', 512 * 1024 * 1024 + 1],
    ['maxAssetSize', Number.MAX_SAFE_INTEGER + 1],
    ['devReconnectMs', 0],
    ['devReconnectMs', 5_001],
    ['devReconnectMs', 1.5],
  ]
  for (const [field, value] of constructorCases) {
    assert.throws(
      () => createFederationRuntime({ global: {}, transport, [field]: value }),
      (error) => error.code === FEDERATION_ERROR_CODES.CONFIG_INVALID &&
        error.phase === 'runtime-config' && error.retryable === false && error.details.field === field,
      `${field}=${String(value)}`,
    )
  }

  const runtime = createFederationRuntime({
    global: {},
    transport,
    timeoutMs: 300_000,
    maxManifestSize: 16 * 1024 * 1024,
    maxAssetSize: 512 * 1024 * 1024,
    devReconnectMs: 5_000,
  })
  assert.equal(runtime.devReconnectMs, 5_000)
  for (const [field, value] of [
    ['timeoutMs', 0],
    ['timeoutMs', 300_001],
    ['maxManifestSize', 0],
    ['maxManifestSize', 16 * 1024 * 1024 + 1],
    ['maxAssetSize', 0],
    ['maxAssetSize', 512 * 1024 * 1024 + 1],
  ]) {
    assert.throws(
      () => runtime.registerRemote({
        name: 'catalog',
        manifestUrl: 'https://cdn.test/catalog/manifest.json',
        [field]: value,
      }),
      (error) => error.code === FEDERATION_ERROR_CODES.CONFIG_INVALID &&
        error.phase === 'remote-register' && error.retryable === false && error.details.field === field,
      `${field}=${String(value)}`,
    )
  }
  assert.equal(runtime.registerRemote({
    name: 'catalog',
    manifestUrl: 'https://cdn.test/catalog/manifest.json',
    timeoutMs: 300_000,
    maxManifestSize: 16 * 1024 * 1024,
    maxAssetSize: 512 * 1024 * 1024,
  }), runtime)
})

test('manifest reads reject oversized Content-Length before body access and cancel unbounded streams', async () => {
  const contentLengthUrl = 'https://cdn.test/content-length/manifest.json'
  let bodyAccesses = 0
  const contentLengthWindow = {
    document: {},
    location: { href: 'https://host.test/' },
    async fetch() {
      return {
        ok: true,
        status: 200,
        headers: new Headers({
          'content-type': 'application/json',
          'content-length': '9',
        }),
        get body() {
          bodyAccesses += 1
          throw new Error('oversized Content-Length must fail before body access')
        },
      }
    },
  }
  contentLengthWindow.window = contentLengthWindow
  const contentLengthRuntime = createFederationRuntime({
    global: contentLengthWindow,
    maxManifestSize: 16,
  })
  contentLengthRuntime.registerRemote({ name: 'length', manifestUrl: contentLengthUrl, maxManifestSize: 8 })
  await assert.rejects(
    contentLengthRuntime.prepareRemote('length'),
    (error) => error.code === FEDERATION_ERROR_CODES.ASSET_SIZE &&
      error.phase === 'manifest-fetch' && error.retryable === false,
  )
  assert.equal(bodyAccesses, 0)

  const streamUrl = 'https://cdn.test/stream/manifest.json'
  const counters = {}
  let fetches = 0
  const streamWindow = {
    document: {},
    location: { href: 'https://host.test/' },
    async fetch() {
      fetches += 1
      return streamedResponse(['12345', '67890'], {
        headers: { 'content-type': 'application/json' },
        counters,
      })
    },
  }
  streamWindow.window = streamWindow
  const streamRuntime = createFederationRuntime({ global: streamWindow, maxManifestSize: 8 })
  streamRuntime.registerRemote({ name: 'stream', manifestUrl: streamUrl })
  await assert.rejects(
    streamRuntime.prepareRemote('stream'),
    (error) => error.code === FEDERATION_ERROR_CODES.ASSET_SIZE &&
      error.phase === 'manifest-fetch' && error.retryable === false && error.details.actual === 10,
  )
  assert.deepEqual(counters, { readers: 1, reads: 2, cancels: 1, releases: 1 })
  await assert.rejects(streamRuntime.prepareRemote('stream'), (error) => error.code === FEDERATION_ERROR_CODES.ASSET_SIZE)
  assert.equal(fetches, 1, 'a fatal oversized manifest must remain cached for the page')
})

test('development updates invalidate only the named remote and preserve accepted build identity ownership', async () => {
  const catalogUrl = 'https://cdn.test/catalog/manifest.json'
  const accountUrl = 'https://cdn.test/account/manifest.json'
  const manifests = new Map([
    [catalogUrl, manifest('catalog', 'catalog-1', { generation: 1 })],
    [accountUrl, manifest('account', 'account-1', { generation: 1 })],
  ])
  const calls = { catalog1: 0, catalog2: 0, account: 0 }
  const makeContainer = (key, value) => ({
    init() {},
    get() {
      calls[key] += 1
      return () => ({ default: value })
    },
  })
  const containers = new Map([
    ['catalog:catalog-1', makeContainer('catalog1', 'catalog-old')],
    ['catalog:catalog-2', makeContainer('catalog2', 'catalog-new')],
    ['account:account-1', makeContainer('account', 'account')],
  ])
  const { runtime } = createHarness(manifests, containers)
  runtime.registerRemote({ name: 'catalog', manifestUrl: catalogUrl })
  runtime.registerRemote({ name: 'account', manifestUrl: accountUrl })

  assert.deepEqual(await runtime.loadRemote('catalog/Button'), { default: 'catalog-old' })
  const account = await runtime.loadRemote('account/Button')
  const accepted = runtime.applyDevUpdate({
    ...devUpdate('catalog', 'catalog-1', 'catalog-2', 2),
    changedExposes: ['./Button', './Button'],
  })

  assert.equal(Object.isFrozen(accepted), true)
  assert.deepEqual(accepted.changedExposes, ['./Button'])
  assert.equal(runtime.explain('catalog/Button').status, 'registered')
  assert.equal(runtime.explain('catalog/Button').cache.manifest, false)
  assert.equal(await runtime.loadRemote('account/Button'), account)
  assert.equal(calls.account, 1)

  manifests.set(catalogUrl, manifest('catalog', 'catalog-2', { generation: 2 }))
  assert.deepEqual(await runtime.loadRemote('catalog/Button'), { default: 'catalog-new' })
  assert.deepEqual(calls, { catalog1: 1, catalog2: 1, account: 1 })
  assert.throws(
    () => runtime.registerContainer({
      name: 'catalog',
      buildId: 'catalog-1',
      container: makeContainer('catalog1', 'conflict'),
    }),
    (error) => error.code === FEDERATION_ERROR_CODES.CONTAINER_REGISTRATION,
  )
})

test('an in-flight superseded development entry cannot commit after the control revision advances', async () => {
  const url = 'https://cdn.test/catalog/manifest.json'
  const manifests = new Map([[url, manifest('catalog', 'catalog-1', { generation: 1 })]])
  let releaseOldEntry
  let markOldEntryStarted
  const oldEntryStarted = new Promise((resolve) => { markOldEntryStarted = resolve })
  const oldEntryGate = new Promise((resolve) => { releaseOldEntry = resolve })
  let oldGets = 0
  const containers = new Map([
    ['catalog:catalog-1', {
      init() {},
      get() {
        oldGets += 1
        return () => ({ default: 'old' })
      },
    }],
    ['catalog:catalog-2', {
      init() {},
      get() { return () => ({ default: 'new' }) },
    }],
  ])
  const { runtime } = createHarness(manifests, containers, {
    async beforeScript(entry, loadedManifest) {
      if (loadedManifest.buildId === 'catalog-1' && entry.url === loadedManifest.remoteEntry.url) {
        markOldEntryStarted()
        await oldEntryGate
      }
    },
  })
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })

  const staleLoad = runtime.loadRemote('catalog/Button')
  await oldEntryStarted
  runtime.applyDevUpdate(devUpdate('catalog', 'catalog-1', 'catalog-2', 2))
  manifests.set(url, manifest('catalog', 'catalog-2', { generation: 2 }))
  releaseOldEntry()
  await assert.rejects(
    staleLoad,
    (error) => error.code === FEDERATION_ERROR_CODES.CONTAINER_REGISTRATION ||
      (error.code === FEDERATION_ERROR_CODES.CONFIG_INVALID && error.retryable),
  )
  assert.equal(oldGets, 0)
  assert.deepEqual(await runtime.loadRemote('catalog/Button'), { default: 'new' })
})

test('superseded manifest schema and integrity failures cannot poison the current revision', async () => {
  for (const failure of ['schema', 'integrity']) {
    const url = `https://cdn.test/${failure}/manifest.json`
    const staleManifest = manifest('catalog', 'catalog-1', { generation: 1 })
    const currentManifest = manifest('catalog', 'catalog-2', { generation: 2 })
    const currentBody = JSON.stringify(currentManifest)
    if (failure === 'schema') staleManifest.schemaVersion = 'wake.federation.manifest.invalid'
    const staleResult = failure === 'integrity'
      ? { manifest: staleManifest, rawBytes: new TextEncoder().encode('tampered') }
      : staleManifest
    const currentResult = failure === 'integrity'
      ? { manifest: currentManifest, rawBytes: new TextEncoder().encode(currentBody) }
      : currentManifest
    let releaseStaleManifest
    let markStaleManifestStarted
    const staleManifestStarted = new Promise((resolve) => { markStaleManifestStarted = resolve })
    const staleManifestGate = new Promise((resolve) => { releaseStaleManifest = resolve })
    let digests = 0
    const testCrypto = failure === 'integrity'
      ? {
          subtle: {
            async digest(algorithm, bytes) {
              digests += 1
              if (digests === 1) {
                markStaleManifestStarted()
                await staleManifestGate
              }
              return webcrypto.subtle.digest(algorithm, bytes)
            },
          },
        }
      : webcrypto
    let fetches = 0
    const { runtime } = createHarness(
      new Map(),
      new Map([['catalog:catalog-2', {
        init() {},
        get() { return () => ({ default: `${failure}-current` }) },
      }]]),
      {
        global: { crypto: testCrypto, location: { href: 'https://host.test/' } },
        async fetchManifest() {
          fetches += 1
          if (fetches === 1 && failure === 'schema') {
            markStaleManifestStarted()
            await staleManifestGate
          }
          return fetches === 1 ? staleResult : currentResult
        },
      },
    )
    runtime.registerRemote({
      name: 'catalog',
      manifestUrl: url,
      ...(failure === 'integrity' ? { manifestIntegrity: sri(currentBody) } : {}),
    })

    const staleLoad = runtime.loadRemote('catalog/Button')
    await staleManifestStarted
    runtime.applyDevUpdate(devUpdate('catalog', 'catalog-1', 'catalog-2', 2))
    releaseStaleManifest()

    await assert.rejects(
      staleLoad,
      (error) => error.code === FEDERATION_ERROR_CODES.CONFIG_INVALID &&
        error.phase === 'dev-update' && error.retryable === true,
    )
    assert.equal(runtime.explain('catalog/Button').status, 'registered')
    assert.deepEqual(await runtime.loadRemote('catalog/Button'), { default: `${failure}-current` })
    assert.equal(runtime.explain('catalog/Button').status, 'loaded')
    assert.equal(fetches, 2)
  }
})

test('types-only updates advance the control cursor without superseding in-flight or active code', async () => {
  const url = 'https://cdn.test/catalog/manifest.json'
  const manifests = new Map([[url, manifest('catalog', 'catalog-1', { generation: 1 })]])
  let releaseManifest
  let markManifestStarted
  const manifestStarted = new Promise((resolve) => { markManifestStarted = resolve })
  const manifestGate = new Promise((resolve) => { releaseManifest = resolve })
  const gets = { old: 0, fresh: 0 }
  const containers = new Map([
    ['catalog:catalog-1', {
      init() {},
      get() {
        gets.old += 1
        return () => ({ default: 'old-code' })
      },
    }],
    ['catalog:catalog-3', {
      init() {},
      get() {
        gets.fresh += 1
        return () => ({ default: 'fresh-code' })
      },
    }],
  ])
  const { runtime, calls } = createHarness(manifests, containers, {
    async fetchManifest(manifestUrl, call) {
      if (call === 1) {
        markManifestStarted()
        await manifestGate
      }
      return manifests.get(manifestUrl)
    },
  })
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })

  const initialLoad = runtime.loadRemote('catalog/Button')
  await manifestStarted
  runtime.applyDevUpdate(devUpdate('catalog', 'catalog-1', 'catalog-2', 2, 'types-only'))
  releaseManifest()

  const oldModule = await initialLoad
  assert.deepEqual(oldModule, { default: 'old-code' })
  assert.equal(runtime.explain('catalog/Button').status, 'loaded')
  assert.equal(await runtime.loadRemote('catalog/Button'), oldModule)
  assert.deepEqual(gets, { old: 1, fresh: 0 })
  assert.equal(calls.fetch, 1)

  runtime.applyDevUpdate(devUpdate('catalog', 'catalog-2', 'catalog-3', 3))
  manifests.set(url, manifest('catalog', 'catalog-3', { generation: 3 }))
  assert.deepEqual(await runtime.loadRemote('catalog/Button'), { default: 'fresh-code' })
  assert.deepEqual(gets, { old: 1, fresh: 1 })
  assert.equal(calls.fetch, 2)
})

test('code updates retain accepted old-build lazy JavaScript and isolated CSS closures', async () => {
  for (const action of ['isolated-remount', 'full-reload']) {
    const url = `https://cdn.test/${action}/manifest.json`
    const oldLazyScript = `https://cdn.test/${action}/build-1/chunks/lazy.mjs`
    const oldLazyStyle = `https://cdn.test/${action}/build-1/chunks/lazy.css`
    const oldExpose = expose(`https://cdn.test/${action}/build-1/button.mjs`, 'isolated')
    oldExpose.scope = 'react18'
    oldExpose.asynchronousAssets = [
      asset(oldLazyScript),
      asset(oldLazyStyle, 'css', 'text/css'),
    ]
    const freshExpose = expose(`https://cdn.test/${action}/build-2/button.mjs`, 'isolated')
    freshExpose.scope = 'react18'
    const manifests = new Map([[
      url,
      manifest('catalog', 'build-1', { generation: 1, exposes: { './Button': oldExpose } }),
    ]])
    const containers = new Map([
      ['catalog:build-1', { init() {}, get() { return () => ({ default: 'old' }) } }],
      ['catalog:build-2', { init() {}, get() { return () => ({ default: 'fresh' }) } }],
    ])
    const scripts = []
    const styles = []
    const { runtime, calls } = createHarness(manifests, containers, {
      beforeScript(entry, loadedManifest) {
        scripts.push({ buildId: loadedManifest.buildId, url: entry.url })
      },
      loadStyle(style, context) {
        const node = ownedStyleNode(context.styleTarget, style.url)
        context.styleTarget.append(node)
        styles.push({ root: context.styleTarget.name, url: style.url })
        return node
      },
    })
    runtime.registerRemote({ name: 'catalog', manifestUrl: url })
    const oldRoot = openShadowRoot(`${action}-old`)
    const detachOldRoot = await runtime.attachIsolatedStyleTarget('catalog/Button', oldRoot)
    assert.deepEqual(await runtime.loadRemote('catalog/Button'), { default: 'old' })

    runtime.applyDevUpdate(devUpdate('catalog', 'build-1', 'build-2', 2, action))
    manifests.set(url, manifest('catalog', 'build-2', {
      generation: 2,
      exposes: { './Button': freshExpose },
    }))
    await runtime.loadFederatedAsset({
      name: 'catalog', buildId: 'build-1', expose: './Button', fileName: 'chunks/lazy.mjs', kind: 'javascript',
    })
    await runtime.loadFederatedAsset({
      name: 'catalog', buildId: 'build-1', expose: './Button', fileName: 'chunks/lazy.css', kind: 'css',
    })

    assert.equal(calls.fetch, 1, `${action} historical assets must not fetch a replacement manifest`)
    assert.equal(scripts.some(({ buildId, url: scriptUrl }) => buildId === 'build-1' && scriptUrl === oldLazyScript), true)
    assert.deepEqual(styles, [{ root: `${action}-old`, url: oldLazyStyle }])
    assert.deepEqual(oldRoot.children.map(({ url: styleUrl }) => styleUrl), [oldLazyStyle])

    assert.deepEqual(await runtime.loadRemote('catalog/Button'), { default: 'fresh' })
    assert.equal(calls.fetch, 2)
    assert.equal(runtime.explain('catalog/Button').container.buildId, 'build-2')
    detachOldRoot()
  }
})

test('development update validation rejects production, malformed, mismatched, and regressive updates', async () => {
  const url = 'https://cdn.test/catalog/manifest.json'
  const { runtime } = createHarness(
    new Map([[url, manifest('catalog', 'catalog-1', { generation: 4 })]]),
    new Map([['catalog:catalog-1', { init() {}, get() { return () => ({}) } }]]),
  )
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })
  await runtime.loadRemote('catalog/Button')

  assert.throws(
    () => runtime.applyDevUpdate({ ...devUpdate('catalog', 'catalog-1', 'catalog-2', 5), unexpected: true }),
    (error) => error.code === FEDERATION_ERROR_CODES.CONFIG_INVALID && error.phase === 'dev-update',
  )
  assert.throws(
    () => runtime.applyDevUpdate(devUpdate('catalog', 'wrong', 'catalog-2', 5)),
    (error) => error.code === FEDERATION_ERROR_CODES.CONFIG_INVALID,
  )
  const accepted = runtime.applyDevUpdate({
    ...devUpdate('catalog', 'catalog-1', 'catalog-2', 5),
    changedExposes: ['./Button', './Button'],
  })
  const duplicate = runtime.applyDevUpdate(devUpdate('catalog', 'catalog-1', 'catalog-2', 5))
  assert.equal(duplicate, accepted, 'a canonical duplicate must return the original frozen update')
  assert.throws(
    () => runtime.applyDevUpdate(devUpdate('catalog', 'catalog-2', 'catalog-3', 5)),
    (error) => error.code === FEDERATION_ERROR_CODES.CONFIG_INVALID,
  )

  const production = createFederationRuntime({ global: {}, transport: { fetchManifest() {}, loadScript() {} } })
  production.registerRemote({
    name: 'locked',
    manifestUrl: 'https://cdn.test/locked/manifest.json',
    mode: 'production',
    lock: {
      buildId: 'locked-1',
      manifestIntegrity: integrity,
      hasExposes: false,
      allowedAssets: {},
    },
  })
  assert.throws(
    () => production.applyDevUpdate(devUpdate('locked', null, 'locked-2', 1)),
    (error) => error.code === FEDERATION_ERROR_CODES.CONFIG_INVALID,
  )
})

test('development manifests maintain one allowlisted remote update socket with action dispatch and backoff', async () => {
  const sockets = []
  const timers = []
  const events = []
  let reloads = 0
  class FakeWebSocket {
    constructor(url) {
      this.url = url
      this.closed = null
      this.sent = []
      sockets.push(this)
    }

    send(message) {
      this.sent.push(JSON.parse(message))
    }

    close(code, reason) {
      this.closed = { code, reason }
      this.onclose?.()
    }
  }
  class FakeCustomEvent {
    constructor(type, options) {
      this.type = type
      this.detail = options.detail
    }
  }
  const fakeGlobal = {
    WebSocket: FakeWebSocket,
    CustomEvent: FakeCustomEvent,
    location: {
      href: 'https://host.test/',
      reload() { reloads += 1 },
    },
    dispatchEvent(event) {
      events.push(event)
      return true
    },
    setTimeout(callback, delay) {
      const timer = { callback, delay, cleared: false }
      timers.push(timer)
      return timer
    },
    clearTimeout(timer) { timer.cleared = true },
    console: { error() {} },
  }
  const url = 'https://cdn.test/catalog/manifest.json'
  const manifests = new Map([[url, manifest('catalog', 'catalog-1', { generation: 1 })]])
  const containers = new Map([['catalog:catalog-1', { init() {}, get() { return () => ({}) } }]])
  const { runtime } = createHarness(manifests, containers, { global: fakeGlobal })
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })

  await runtime.prepareRemote('catalog')
  assert.equal(await runtime.connectDevUpdates('catalog'), true)
  assert.equal(sockets.length, 1)
  assert.equal(sockets[0].url, 'wss://cdn.test/catalog/updates')
  sockets[0].onopen?.()
  assert.deepEqual(sockets[0].sent, [{
    type: 'lease',
    schemaVersion: FEDERATION_DEV_LEASE_SCHEMA,
    remote: 'catalog',
    buildIds: ['catalog-1'],
  }])
  sockets[0].onmessage({
    data: JSON.stringify(devLeaseAck('catalog', ['catalog-1'], 'catalog-1', 1)),
  })
  assert.equal(sockets[0].closed, null)
  sockets[0].onmessage({ data: JSON.stringify(devUpdate('catalog', 'catalog-1', 'catalog-2', 2)) })
  assert.equal(events.length, 1)
  assert.equal(events[0].type, 'wake:federation:isolated-remount')
  assert.equal(Object.isFrozen(events[0].detail), true)
  assert.equal(runtime.explain('catalog/Button').status, 'registered')
  sockets[0].onmessage({
    data: JSON.stringify({
      ...devUpdate('catalog', 'catalog-1', 'catalog-2', 2),
      changedExposes: ['./Button', './Button'],
    }),
  })
  assert.equal(events.length, 1)
  assert.equal(sockets[0].closed, null)

  sockets[0].onmessage({ data: JSON.stringify(devUpdate('catalog', 'catalog-2', 'catalog-3', 3, 'types-only')) })
  sockets[0].onmessage({ data: JSON.stringify(devUpdate('catalog', 'catalog-2', 'catalog-3', 3, 'types-only')) })
  assert.equal(events.length, 1)
  assert.equal(reloads, 0)
  assert.equal(sockets[0].closed, null)
  sockets[0].onmessage({ data: JSON.stringify(devUpdate('catalog', 'catalog-3', 'catalog-4', 4, 'full-reload')) })
  sockets[0].onmessage({ data: JSON.stringify(devUpdate('catalog', 'catalog-3', 'catalog-4', 4, 'full-reload')) })
  assert.equal(reloads, 1)
  assert.equal(sockets[0].closed, null)

  sockets[0].onclose()
  assert.equal(timers.length, 1)
  assert.equal(timers[0].delay, 250)
  timers[0].callback()
  assert.equal(sockets.length, 2)
  sockets[1].onopen?.()
  assert.deepEqual(sockets[1].sent.at(-1)?.buildIds, ['catalog-1'])
  sockets[1].onmessage({
    data: JSON.stringify(devLeaseReload('catalog', 'catalog-4', 4, null, 'update-lagged')),
  })
  assert.equal(sockets[1].closed.code, 1000)
  assert.equal(reloads, 2)
  assert.equal(timers.length, 1)

  const deniedManifest = manifest('denied', 'd1')
  deniedManifest.development.updatesUrl = 'wss://evil.test/updates'
  const deniedUrl = 'https://cdn.test/denied/manifest.json'
  const denied = createHarness(
    new Map([[deniedUrl, deniedManifest]]),
    new Map(),
    { global: fakeGlobal },
  ).runtime
  denied.registerRemote({ name: 'denied', manifestUrl: deniedUrl })
  await assert.rejects(
    denied.prepareRemote('denied'),
    (error) => error.code === FEDERATION_ERROR_CODES.ORIGIN_DENIED,
  )
})

test('an HTTP-observed build accepts and dispatches its first same-generation control frame', async () => {
  const sockets = []
  const events = []
  class FakeWebSocket {
    constructor(url) {
      this.url = url
      this.closed = null
      sockets.push(this)
    }

    close(code, reason) {
      this.closed = { code, reason }
    }
  }
  class FakeCustomEvent {
    constructor(type, options) {
      this.type = type
      this.detail = options.detail
    }
  }
  const fakeGlobal = {
    WebSocket: FakeWebSocket,
    CustomEvent: FakeCustomEvent,
    location: { href: 'https://host.test/' },
    dispatchEvent(event) {
      events.push(event)
      return true
    },
    console: { error() {} },
  }
  const url = 'https://cdn.test/catalog/manifest.json'
  const manifests = new Map([[url, manifest('catalog', 'catalog-1', { generation: 1 })]])
  const gets = { old: 0, current: 0 }
  const moduleValue = { default: 'http-current' }
  const containers = new Map([
    ['catalog:catalog-1', {
      init() {},
      get() {
        gets.old += 1
        return () => ({ default: 'old' })
      },
    }],
    ['catalog:catalog-3', {
      init() {},
      get() {
        gets.current += 1
        return () => moduleValue
      },
    }],
  ])
  const { runtime, calls } = createHarness(manifests, containers, { global: fakeGlobal })
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })

  assert.deepEqual(await runtime.loadRemote('catalog/Button'), { default: 'old' })
  runtime.applyDevUpdate(devUpdate('catalog', 'catalog-1', 'catalog-2', 2))
  manifests.set(url, manifest('catalog', 'catalog-3', { generation: 3 }))
  assert.equal(await runtime.loadRemote('catalog/Button'), moduleValue)
  assert.equal(sockets.length, 1)
  assert.throws(
    () => runtime.applyDevUpdate(devUpdate('catalog', 'wrong-old', 'catalog-3', 3)),
    (error) => error.code === FEDERATION_ERROR_CODES.CONFIG_INVALID && error.retryable === false,
  )
  const catchUp = devUpdate('catalog', 'catalog-2', 'catalog-3', 3)
  sockets[0].onmessage({
    data: JSON.stringify({ ...catchUp, changedExposes: ['./Button', './Button'] }),
  })

  assert.equal(events.length, 1)
  assert.equal(events[0].type, 'wake:federation:isolated-remount')
  assert.equal(await runtime.loadRemote('catalog/Button'), moduleValue)
  assert.equal(calls.fetch, 2)
  assert.deepEqual(gets, { old: 1, current: 1 })
  assert.equal(
    runtime.explain('catalog/Button').trace.some(({ outcome }) => outcome === 'control-caught-up'),
    true,
  )

  sockets[0].onmessage({ data: JSON.stringify(catchUp) })
  assert.equal(events.length, 1, 'a canonical catch-up duplicate must not dispatch twice')
  assert.equal(sockets[0].closed, null)
  assert.throws(
    () => runtime.applyDevUpdate({ ...catchUp, typesHash: 'conflicting-types' }),
    (error) => error.code === FEDERATION_ERROR_CODES.CONFIG_INVALID && error.retryable === false,
  )
})

test('lease acknowledgements preserve a continuous cursor and reload a reconnect that missed generations', async () => {
  const sockets = []
  const timers = []
  const events = []
  let reloads = 0
  class FakeWebSocket {
    constructor(url) {
      this.url = url
      this.sent = []
      this.closed = null
      sockets.push(this)
    }

    send(message) {
      this.sent.push(JSON.parse(message))
    }

    close(code, reason) {
      this.closed = { code, reason }
      this.onclose?.()
    }
  }
  class FakeCustomEvent {
    constructor(type, options) {
      this.type = type
      this.detail = options.detail
    }
  }
  const fakeGlobal = {
    WebSocket: FakeWebSocket,
    CustomEvent: FakeCustomEvent,
    location: {
      href: 'https://host.test/',
      reload() { reloads += 1 },
    },
    dispatchEvent(event) {
      events.push(event)
      return true
    },
    setTimeout(callback, delay) {
      const timer = { callback, delay }
      timers.push(timer)
      return timer
    },
    clearTimeout() {},
    console: { error() {} },
  }
  const url = 'https://cdn.test/catalog/manifest.json'
  const remoteManifest = manifest('catalog', 'catalog-1', { generation: 1 })
  const { runtime } = createHarness(new Map([[url, remoteManifest]]), new Map(), { global: fakeGlobal })
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })

  await runtime.describeRemote('catalog/Button')
  sockets[0].onopen?.()
  sockets[0].onmessage({ data: JSON.stringify(devLeaseAck('catalog', ['catalog-1'], 'catalog-1', 1)) })
  assert.equal(reloads, 0)

  sockets[0].onclose?.()
  timers[0].callback()
  sockets[1].onopen?.()
  sockets[1].onmessage({ data: JSON.stringify(devLeaseAck('catalog', ['catalog-1'], 'catalog-1', 1)) })
  sockets[1].onmessage({ data: JSON.stringify(devUpdate('catalog', 'catalog-1', 'catalog-2', 2)) })
  assert.equal(events.length, 1, 'a valid reconnect ack must preserve subsequent update continuity')
  assert.equal(sockets[1].sent.length, 2, 'a cursor advance must renew even an unchanged build lease set')
  assert.deepEqual(sockets[1].sent[1].buildIds, ['catalog-1'])
  sockets[1].onmessage({ data: JSON.stringify(devLeaseAck('catalog', ['catalog-1'], 'catalog-2', 2)) })
  assert.equal(reloads, 0)

  sockets[1].onclose?.()
  timers[1].callback()
  sockets[2].onopen?.()
  sockets[2].onmessage({ data: JSON.stringify(devLeaseAck('catalog', ['catalog-1'], 'catalog-3', 3)) })
  assert.equal(reloads, 1, 'a reconnect ack ahead of the accepted cursor must refresh only this page')
  assert.deepEqual(sockets[2].closed, { code: 1000, reason: 'federation development cursor diverged' })
  sockets[2].onmessage({ data: JSON.stringify(devUpdate('catalog', 'catalog-2', 'catalog-3', 3)) })
  assert.equal(events.length, 1, 'a fatal gap must not dispatch later frames into stale state')
  assert.equal(timers.length, 2, 'a cursor gap is fatal and must not reconnect the stale page')
})

test('development lease replacements track accepted runtimes and reload instead of silently evicting the ninth build', async () => {
  const sockets = []
  let reloads = 0
  class FakeWebSocket {
    constructor(url) {
      this.url = url
      this.sent = []
      sockets.push(this)
    }

    send(message) {
      this.sent.push(JSON.parse(message))
    }

    close() {}
  }
  const fakeGlobal = {
    WebSocket: FakeWebSocket,
    location: {
      href: 'https://host.test/',
      reload() { reloads += 1 },
    },
    console: { error() {} },
  }
  const url = 'https://cdn.test/catalog/manifest.json'
  const manifests = new Map([[url, manifest('catalog', 'catalog-1', { generation: 1 })]])
  const containers = new Map()
  for (let generation = 1; generation <= FEDERATION_DEV_MAX_BUILD_LEASES + 1; generation += 1) {
    containers.set(`catalog:catalog-${generation}`, {
      init() {},
      get() { return () => ({ generation }) },
    })
  }
  const { runtime } = createHarness(manifests, containers, { global: fakeGlobal })
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })
  await runtime.loadRemote('catalog/Button')
  assert.equal(sockets.length, 1)
  sockets[0].onopen?.()
  assert.deepEqual(sockets[0].sent.at(-1)?.buildIds, ['catalog-1'])

  for (let generation = 2; generation <= FEDERATION_DEV_MAX_BUILD_LEASES; generation += 1) {
    runtime.applyDevUpdate(devUpdate(
      'catalog',
      `catalog-${generation - 1}`,
      `catalog-${generation}`,
      generation,
    ))
    manifests.set(url, manifest('catalog', `catalog-${generation}`, { generation }))
    await runtime.loadRemote('catalog/Button')
    assert.deepEqual(
      sockets[0].sent.at(-1)?.buildIds,
      Array.from({ length: generation }, (_, index) => `catalog-${index + 1}`),
    )
  }
  assert.equal(reloads, 0)

  const overflowGeneration = FEDERATION_DEV_MAX_BUILD_LEASES + 1
  runtime.applyDevUpdate(devUpdate(
    'catalog',
    `catalog-${overflowGeneration - 1}`,
    `catalog-${overflowGeneration}`,
    overflowGeneration,
  ))
  manifests.set(url, manifest('catalog', `catalog-${overflowGeneration}`, {
    generation: overflowGeneration,
  }))
  await assert.rejects(
    runtime.loadRemote('catalog/Button'),
    (error) => error.code === FEDERATION_ERROR_CODES.CONFIG_INVALID &&
      error.phase === 'dev-lease' && error.retryable === false,
  )
  assert.equal(reloads, 1)
  assert.equal(sockets[0].sent.at(-1).buildIds.length, FEDERATION_DEV_MAX_BUILD_LEASES)
  assert.equal(sockets[0].sent.at(-1).buildIds.includes(`catalog-${overflowGeneration}`), false)
})

test('production registration requires the canonical immutable lock fields', () => {
  const runtime = createFederationRuntime({ global: {}, transport: { fetchManifest() {}, loadScript() {} } })
  assert.throws(
    () => runtime.registerRemote({
      name: 'catalog',
      manifestUrl: 'https://cdn.test/catalog/manifest.json',
      mode: 'production',
      buildId: 'b1',
      manifestIntegrity: integrity,
    }),
    (error) => error.code === FEDERATION_ERROR_CODES.LOCK_INVALID,
  )
  assert.throws(
    () => runtime.registerRemote({
      name: 'typed',
      manifestUrl: 'https://cdn.test/typed/manifest.json',
      mode: 'production',
      lock: {
        buildId: 'b1',
        manifestIntegrity: integrity,
        hasExposes: true,
        allowedAssets: {},
      },
    }),
    (error) => error.code === FEDERATION_ERROR_CODES.LOCK_INVALID,
  )
  assert.equal(runtime.registerRemote({
    name: 'catalog',
    manifestUrl: 'https://cdn.test/catalog/manifest.json',
    mode: 'production',
    lock: {
      manifestUrl: 'https://cdn.test/catalog/manifest.json',
      buildId: 'b1',
      manifestIntegrity: integrity,
      hasExposes: false,
      allowedAssets: {
        'https://cdn.test/catalog/b1/remote.mjs': integrity,
        'https://cdn.test/catalog/b1/button.mjs': integrity,
      },
    },
  }), runtime)
})

test('production registration accepts canonical and legacy-null shared-only Rust lock entries', () => {
  const createRuntime = () => createFederationRuntime({
    global: {},
    transport: { fetchManifest() {}, loadScript() {} },
  })
  const definition = {
    name: 'catalog',
    manifestUrl: rustSharedOnlyRemoteRefFixture.manifestUrl,
    mode: 'production',
  }

  const canonicalRuntime = createRuntime()
  assert.equal(canonicalRuntime.registerRemote({
    ...definition,
    lock: rustSharedOnlyRemoteRefFixture,
  }), canonicalRuntime)

  const legacyRuntime = createRuntime()
  assert.equal(legacyRuntime.registerRemote({
    ...definition,
    typesIntegrity: null,
    lock: { ...rustSharedOnlyRemoteRefFixture, typesIntegrity: null },
  }), legacyRuntime)
  assert.equal(
    legacyRuntime.registerRemote({ ...definition, lock: rustSharedOnlyRemoteRefFixture }),
    legacyRuntime,
    'legacy null and canonical omission must normalize to one registration identity',
  )

  for (const [label, lock] of [
    ['missing', { ...rustSharedOnlyRemoteRefFixture, hasExposes: true }],
    ['null', { ...rustSharedOnlyRemoteRefFixture, hasExposes: true, typesIntegrity: null }],
  ]) {
    const exposedRuntime = createRuntime()
    assert.throws(
      () => exposedRuntime.registerRemote({ ...definition, lock }),
      (error) => error.code === FEDERATION_ERROR_CODES.LOCK_INVALID,
      `an exposed remote with ${label} typesIntegrity must fail closed`,
    )
  }
})

test('production manifests match locked expose/type presence and declare every singleton owner', async () => {
  const createProductionRuntime = (remoteManifest, lock) => {
    const manifestUrl = `https://cdn.test/${remoteManifest.name}/manifest.json`
    const runtime = createFederationRuntime({
      global: { location: { href: 'https://host.test/' } },
      transport: {
        async fetchManifest() {
          return { manifest: remoteManifest, verifiedIntegrity: true }
        },
        async loadScript() {},
      },
    })
    runtime.registerRemote({
      name: remoteManifest.name,
      manifestUrl,
      mode: 'production',
      lock: { manifestUrl, buildId: remoteManifest.buildId, manifestIntegrity: integrity, ...lock },
    })
    return runtime
  }

  const presence = manifest('presence', 'b1')
  const presenceRuntime = createProductionRuntime(presence, {
    hasExposes: false,
    allowedAssets: {
      [presence.remoteEntry.url]: integrity,
      [presence.exposes['./Button'].entry.url]: integrity,
    },
  })
  await assert.rejects(
    presenceRuntime.describeRemote('presence/Button'),
    (error) => error.code === FEDERATION_ERROR_CODES.LOCK_MISMATCH && error.retryable === false,
  )

  const missingTypes = manifest('missingtypes', 'b1')
  const missingTypesRuntime = createProductionRuntime(missingTypes, {
    hasExposes: true,
    typesIntegrity: integrity,
    allowedAssets: {
      [missingTypes.remoteEntry.url]: integrity,
      [missingTypes.exposes['./Button'].entry.url]: integrity,
    },
  })
  await assert.rejects(
    missingTypesRuntime.describeRemote('missingtypes/Button'),
    (error) => error.code === FEDERATION_ERROR_CODES.TYPE_BUILD_MISMATCH && error.retryable === false,
  )

  const ownerless = manifest('ownerless', 'b1', {
    exposes: {},
    requirements: [requirement('react', '^18.0.0', { singleton: true })],
  })
  const ownerlessRuntime = createProductionRuntime(ownerless, {
    hasExposes: false,
    allowedAssets: { [ownerless.remoteEntry.url]: integrity },
  })
  await assert.rejects(
    ownerlessRuntime.prepareRemote('ownerless'),
    (error) => error.code === FEDERATION_ERROR_CODES.SHARE_SINGLETON_CONFLICT && error.retryable === false,
  )

  const accepted = manifest('accepted', 'b1')
  const typesUrl = 'https://cdn.test/accepted/b1/types.json'
  accepted.types = {
    buildId: 'b1',
    url: typesUrl,
    contentHash: 'types-b1',
    integrity,
    size: 1,
    format: 'declaration-bundle',
  }
  const acceptedRuntime = createProductionRuntime(accepted, {
    hasExposes: true,
    typesIntegrity: integrity,
    allowedAssets: {
      [accepted.remoteEntry.url]: integrity,
      [accepted.exposes['./Button'].entry.url]: integrity,
      [typesUrl]: integrity,
    },
  })
  assert.equal((await acceptedRuntime.describeRemote('accepted/Button')).buildId, 'b1')
})

test('production mode keeps development metadata inert in every asset execution context', async () => {
  const isolated = expose('https://cdn.test/prodmeta/b1/widget.mjs', 'isolated')
  isolated.scope = 'prodmeta-isolated'
  isolated.css = [asset('https://cdn.test/prodmeta/b1/widget.css', 'css', 'text/css')]
  const remoteManifest = manifest('prodmeta', 'b1', { exposes: { './Widget': isolated } })
  const typesUrl = 'https://cdn.test/prodmeta/b1/types.json'
  remoteManifest.types = {
    buildId: 'b1',
    url: typesUrl,
    contentHash: 'types-b1',
    integrity,
    size: 1,
    format: 'declaration-bundle',
  }
  const manifestUrl = 'https://cdn.test/prodmeta/manifest.json'
  const scriptContexts = []
  const styleContexts = []
  let reloads = 0
  const runtime = createFederationRuntime({
    global: { location: { href: 'https://host.test/' } },
    transport: {
      async fetchManifest() {
        return { manifest: remoteManifest, verifiedIntegrity: true }
      },
      async loadScript(_asset, context) {
        scriptContexts.push(context.assetContext)
        return { init() {}, get() { return () => ({}) } }
      },
      async loadStyle(style, context) {
        styleContexts.push(context.assetContext)
        return ownedStyleNode(context.styleTarget, style.url)
      },
    },
  })
  runtime.registerRemote({
    name: 'prodmeta',
    manifestUrl,
    mode: 'production',
    lock: {
      manifestUrl,
      buildId: 'b1',
      manifestIntegrity: integrity,
      hasExposes: true,
      typesIntegrity: integrity,
      allowedAssets: {
        [remoteManifest.remoteEntry.url]: integrity,
        [isolated.entry.url]: integrity,
        [isolated.css[0].url]: integrity,
        [typesUrl]: integrity,
      },
    },
  })

  const prepared = await runtime.prepareRemote('prodmeta')
  assert.deepEqual(prepared, { name: 'prodmeta', buildId: 'b1', generation: 0 })
  assert.deepEqual(scriptContexts, [{
    name: 'prodmeta',
    buildId: 'b1',
    generation: 0,
    development: false,
  }])
  const detach = await runtime.attachIsolatedStyleTarget('prodmeta/Widget', openShadowRoot('production'))
  assert.deepEqual(styleContexts, [{
    name: 'prodmeta',
    buildId: 'b1',
    generation: 0,
    development: false,
    expose: './Widget',
  }])

  const headers = {
    'wake-federation-control': FEDERATION_DEV_LEASE_SCHEMA,
    'wake-federation-action': 'full-reload',
    'wake-federation-remote': 'prodmeta',
    'wake-federation-current-build-id': 'b2',
    'wake-federation-generation': '1',
    'wake-federation-expired-build-id': 'b1',
    'wake-federation-reason': 'build-gone',
  }
  await assert.rejects(
    __preflightFederatedAsset(remoteManifest.remoteEntry, {
      global: {
        location: { reload() { reloads += 1 } },
        async fetch() { return new Response(null, { status: 410, headers }) },
      },
      maxAssetSize: 1024,
      assetContext: scriptContexts[0],
    }),
    (error) => error.code === FEDERATION_ERROR_CODES.NETWORK && error.details.reason === undefined,
  )
  assert.equal(reloads, 0)
  detach()
})

test('manifest validation rejects conflicting metadata for one asset URL', async () => {
  const url = 'https://cdn.test/catalog/manifest.json'
  const remoteManifest = manifest('catalog', 'build-a')
  remoteManifest.exposes['./Button'].entry = {
    ...remoteManifest.remoteEntry,
    contentHash: 'different-content',
  }
  const { runtime } = createHarness(new Map([[url, remoteManifest]]), new Map())
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })

  await assert.rejects(
    runtime.prepareRemote('catalog'),
    (error) => error.code === FEDERATION_ERROR_CODES.ASSET_INTEGRITY && error.retryable === false,
  )
  await assert.rejects(
    runtime.prepareRemote('catalog'),
    (error) => error.code === FEDERATION_ERROR_CODES.ASSET_INTEGRITY,
  )
  assert.equal(runtime.explain('catalog/Button').error.code, FEDERATION_ERROR_CODES.ASSET_INTEGRITY)
})

test('browser manifest validation rejects fields outside the versioned schema', async () => {
  const url = 'https://cdn.test/catalog/manifest.json'
  const remoteManifest = { ...manifest('catalog', 'build-a'), unexpectedControl: true }
  const { runtime } = createHarness(new Map([[url, remoteManifest]]), new Map())
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })

  await assert.rejects(
    runtime.describeRemote('catalog/Button'),
    (error) => error.code === FEDERATION_ERROR_CODES.MANIFEST_SCHEMA &&
      error.details.unknownFields.includes('unexpectedControl') && error.retryable === false,
  )
})

test('browser manifest validation accepts Rust Option fields serialized as null', async () => {
  const url = 'https://cdn.test/catalog/manifest.json'
  const remoteManifest = manifest('catalog', 'build-a', {
    offers: [offer('react', '18.2.0', 'catalog')],
    requirements: [requirement('react', '^18.0.0')],
  })
  remoteManifest.remoteEntrySourceMap = null
  remoteManifest.exposes['./Button'].sourceMap = null
  remoteManifest.shared.offers[0].asset = null
  remoteManifest.shared.requirements[0].fallback = null
  remoteManifest.types = null
  remoteManifest.development = null
  const { runtime } = createHarness(new Map([[url, remoteManifest]]), new Map())
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })

  const description = await runtime.describeRemote('catalog/Button')
  assert.equal(description.buildId, 'build-a')
  assert.equal(description.development, true, 'runtime mode, not nullable metadata, owns development behavior')
  assert.equal(description.generation, 0)
})

test('browser manifest validation enforces Rust-compatible expose scopes', async () => {
  for (const { name, exposed, code } of [
    {
      name: 'invalidscope',
      exposed: { ...expose('https://cdn.test/invalidscope/b1/button.mjs'), scope: 'react//18' },
      code: FEDERATION_ERROR_CODES.MANIFEST_SCHEMA,
    },
    {
      name: 'defaultisolated',
      exposed: expose('https://cdn.test/defaultisolated/b1/button.mjs', 'isolated'),
      code: FEDERATION_ERROR_CODES.COHERENCE_CONFLICT,
    },
  ]) {
    const url = `https://cdn.test/${name}/manifest.json`
    const remoteManifest = manifest(name, 'b1', { exposes: { './Button': exposed } })
    const { runtime } = createHarness(new Map([[url, remoteManifest]]), new Map())
    runtime.registerRemote({ name, manifestUrl: url })
    await assert.rejects(
      runtime.describeRemote(`${name}/Button`),
      (error) => error.code === code && error.retryable === false &&
        error.details.path === 'exposes../Button.scope',
    )
  }

  const url = 'https://cdn.test/scopes/manifest.json'
  const react17 = { ...expose('https://cdn.test/scopes/b1/react17.mjs', 'isolated'), scope: 'react17' }
  const react18 = { ...expose('https://cdn.test/scopes/b1/react18.mjs', 'isolated'), scope: 'react18' }
  const remoteManifest = manifest('scopes', 'b1', {
    exposes: { './React17': react17, './React18': react18 },
  })
  const { runtime } = createHarness(new Map([[url, remoteManifest]]), new Map())
  runtime.registerRemote({ name: 'scopes', manifestUrl: url })
  assert.equal((await runtime.describeRemote('scopes/React17')).scope, 'react17')
  assert.equal((await runtime.describeRemote('scopes/React18')).scope, 'react18')
})

test('loadRemote is single-flight and host shared dependencies win over remote fallback', async () => {
  const url = 'https://cdn.test/catalog/manifest.json'
  const manifests = new Map([[url, manifest('catalog', 'build-a', {
    offers: [offer('react', '18.2.0', 'catalog', { singleton: true, coherenceGroup: 'react18' })],
    requirements: [requirement('react', '^18.0.0', { singleton: true, coherenceGroup: 'react18' })],
  })]])
  let initCount = 0
  let getCount = 0
  let factoryCount = 0
  let remoteSharedCount = 0
  let initializedContext
  const container = {
    async init(context) {
      initCount += 1
      initializedContext = context
    },
    async get(exposeKey) {
      assert.equal(exposeKey, './Button')
      getCount += 1
      return async () => {
        factoryCount += 1
        return Object.freeze({ default: 'button' })
      }
    },
    async getShared() {
      remoteSharedCount += 1
      return { source: 'remote' }
    },
  }
  const { runtime, calls } = createHarness(manifests, new Map([['catalog:build-a', container]]))
  const hostReact = Object.freeze({ source: 'host' })
  runtime.registerHostShared({
    shareKey: 'react',
    version: '18.3.0',
    singleton: true,
    coherenceGroup: 'react18',
    packageContext: 'npm:root',
    buildVariant: 'browser',
    module: hostReact,
  })
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })

  const [left, right] = await Promise.all([
    runtime.loadRemote('catalog/Button'),
    runtime.loadRemote('catalog/Button'),
  ])

  assert.equal(left, right)
  assert.deepEqual(left, { default: 'button' })
  assert.equal(calls.fetch, 1)
  // The immutable container entry and expose entry each execute once.
  assert.equal(calls.script, 2)
  assert.equal(initCount, 1)
  assert.equal(getCount, 1)
  assert.equal(factoryCount, 1)
  assert.equal(remoteSharedCount, 0)
  assert.equal(initializedContext.resolved['default:react'], hostReact)
  assert.equal(initializedContext.getSync('react'), hostReact)
  assert.throws(
    () => initializedContext.getSync('undeclared'),
    (error) => error.code === FEDERATION_ERROR_CODES.SHARE_UNSATISFIABLE,
  )
  assert.equal(runtime.explain('catalog/Button').status, 'loaded')
})

test('host-rendered CSS starts and resolves strictly in manifest order', async () => {
  const url = 'https://cdn.test/catalog/manifest.json'
  const firstUrl = 'https://cdn.test/catalog/build-a/first.css'
  const secondUrl = 'https://cdn.test/catalog/build-a/second.css'
  const remoteExpose = expose('https://cdn.test/catalog/build-a/button.mjs', 'host-rendered')
  remoteExpose.css = [
    asset(firstUrl, 'css', 'text/css'),
    asset(secondUrl, 'css', 'text/css'),
  ]
  const started = []
  const completed = []
  let releaseFirst
  let markFirstStarted
  const firstStarted = new Promise((resolve) => { markFirstStarted = resolve })
  const firstGate = new Promise((resolve) => { releaseFirst = resolve })
  const { runtime } = createHarness(
    new Map([[url, manifest('catalog', 'build-a', {
      exposes: { './Button': remoteExpose },
      requirements: reactRequirements(),
    })]]),
    new Map([['catalog:build-a', { init() {}, get() { return () => ({}) } }]]),
    {
      async loadStyle(style) {
        started.push(style.url)
        if (style.url === firstUrl) {
          markFirstStarted()
          await firstGate
        }
        completed.push(style.url)
      },
    },
  )
  for (const shareKey of ['react', 'react/jsx-runtime', 'react/jsx-dev-runtime', 'react-dom', 'react-dom/client']) {
    runtime.registerHostShared({
      shareKey,
      version: '18.2.0',
      scope: 'default',
      singleton: true,
      coherenceGroup: 'react18',
      packageContext: 'npm:root',
      buildVariant: 'browser',
      module: { shareKey },
    })
  }
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })

  const loading = runtime.loadRemote('catalog/Button')
  await firstStarted
  assert.deepEqual(started, [firstUrl])
  assert.deepEqual(completed, [])
  releaseFirst()
  await loading
  assert.deepEqual(started, [firstUrl, secondUrl])
  assert.deepEqual(completed, [firstUrl, secondUrl])
})

test('host-rendered manifests fail closed unless all five React identities form one singleton group', async () => {
  const url = 'https://cdn.test/catalog/manifest.json'
  const remoteExpose = expose('https://cdn.test/catalog/build-a/button.mjs', 'host-rendered')
  const incomplete = reactRequirements()
  incomplete.pop()
  const { runtime } = createHarness(
    new Map([[url, manifest('catalog', 'build-a', {
      exposes: { './Button': remoteExpose },
      requirements: incomplete,
    })]]),
    new Map(),
  )
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })

  await assert.rejects(
    runtime.describeRemote('catalog/Button'),
    (error) => error.code === FEDERATION_ERROR_CODES.COHERENCE_CONFLICT &&
      error.details.missing === 'react-dom/client' && error.retryable === false,
  )
})

test('selected remote fallback asset is integrity-loaded before getShared reads its namespace', async () => {
  const url = 'https://cdn.test/catalog/manifest.json'
  const fallbackAsset = asset('https://cdn.test/catalog/build-a/shared.mjs')
  const fallbackOffer = offer('react', '18.2.0', 'catalog', { singleton: true })
  fallbackOffer.asset = fallbackAsset
  const manifests = new Map([[url, manifest('catalog', 'build-a', {
    offers: [fallbackOffer],
    requirements: [requirement('react', '18.2.0', { singleton: true })],
  })]])
  let fallbackLoaded = false
  let selected
  const container = {
    async init(context) { selected = context.getSync('react') },
    async get() { return () => ({ default: 'button' }) },
    async getShared() {
      assert.equal(fallbackLoaded, true)
      return { source: 'remote-fallback' }
    },
  }
  const { runtime, calls } = createHarness(
    manifests,
    new Map([['catalog:build-a', container]]),
    { beforeScript(entry) { if (entry.url === fallbackAsset.url) fallbackLoaded = true } },
  )
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })

  await runtime.loadRemote('catalog/Button')

  assert.deepEqual(selected, { source: 'remote-fallback' })
  assert.equal(calls.script, 3)
})

test('a transient shared fallback failure can explicitly retry container initialization', async () => {
  const url = 'https://cdn.test/catalog/manifest.json'
  const fallbackAsset = asset('https://cdn.test/catalog/build-a/shared.mjs')
  const fallbackOffer = offer('react', '18.2.0', 'catalog', { singleton: true })
  fallbackOffer.asset = fallbackAsset
  const manifests = new Map([[url, manifest('catalog', 'build-a', {
    offers: [fallbackOffer],
    requirements: [requirement('react', '18.2.0', { singleton: true })],
  })]])
  let fallbackAttempts = 0
  let initCount = 0
  const container = {
    async init() { initCount += 1 },
    async get() { return () => ({ default: 'button' }) },
    async getShared() { return { source: 'remote-fallback' } },
  }
  const { runtime } = createHarness(
    manifests,
    new Map([['catalog:build-a', container]]),
    {
      beforeScript(entry) {
        if (entry.url !== fallbackAsset.url) return
        fallbackAttempts += 1
        if (fallbackAttempts === 1) {
          throw new FederationError(FEDERATION_ERROR_CODES.NETWORK, 'temporary fallback outage', {
            phase: 'asset-load',
            retryable: true,
          })
        }
      },
    },
  )
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })

  await assert.rejects(
    runtime.loadRemote('catalog/Button'),
    (error) => error.code === FEDERATION_ERROR_CODES.SHARE_UNSATISFIABLE && error.retryable,
  )
  assert.deepEqual(await runtime.loadRemote('catalog/Button'), { default: 'button' })
  assert.equal(fallbackAttempts, 2)
  assert.equal(initCount, 1)
})

test('prepareRemote makes an explicitly-owned remote singleton available without evaluating an expose', async () => {
  const url = 'https://cdn.test/catalog/manifest.json'
  const manifests = new Map([[url, manifest('catalog', 'build-a', {
    offers: [offer('react', '18.3.1', 'catalog', { singleton: true, owner: 'catalog' })],
  })]])
  let exposeGets = 0
  let sharedGets = 0
  const remoteReact = Object.freeze({ source: 'catalog' })
  const container = {
    async init() {},
    async get() {
      exposeGets += 1
      return () => ({})
    },
    async getShared() {
      sharedGets += 1
      return remoteReact
    },
  }
  const { runtime } = createHarness(manifests, new Map([['catalog:build-a', container]]))
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })
  const request = {
    shareKey: 'react',
    requiredVersion: '^18.0.0',
    singleton: true,
    owner: 'catalog',
  }

  await assert.rejects(
    runtime.resolveShared(request),
    (error) => error.code === FEDERATION_ERROR_CODES.SHARE_UNSATISFIABLE,
  )
  assert.deepEqual(await runtime.prepareRemote('catalog'), {
    name: 'catalog',
    buildId: 'build-a',
    generation: 0,
  })
  assert.equal(await runtime.resolveShared(request), remoteReact)
  assert.equal(exposeGets, 0)
  assert.equal(sharedGets, 1)
  await assert.rejects(
    runtime.resolveShared({ ...request, owner: 'other' }),
    (error) => error.code === FEDERATION_ERROR_CODES.SHARE_SINGLETON_CONFLICT,
  )
})

test('describeRemote returns ordered isolated CSS metadata without loading document styles or expose code', async () => {
  const url = 'https://cdn.test/catalog/manifest.json'
  const isolated = expose('https://cdn.test/catalog/build-a/button.mjs', 'isolated')
  isolated.scope = 'catalog-isolated'
  isolated.synchronousAssets = [asset('https://cdn.test/catalog/build-a/reset.css', 'css', 'text/css')]
  isolated.css = [asset('https://cdn.test/catalog/build-a/button.css', 'css', 'text/css')]
  const manifests = new Map([[url, manifest('catalog', 'build-a', { exposes: { './Button': isolated } })]])
  let styleLoads = 0
  const { runtime, calls } = createHarness(manifests, new Map(), {
    loadStyle() { styleLoads += 1 },
  })
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })

  const descriptor = await runtime.describeRemote('catalog/Button')
  assert.equal(descriptor.mode, 'isolated')
  assert.equal(descriptor.scope, 'catalog-isolated')
  assert.equal(descriptor.shadow, 'open')
  assert.equal(descriptor.development, true)
  assert.deepEqual(descriptor.css.map((asset) => asset.url), [
    'https://cdn.test/catalog/build-a/reset.css',
    'https://cdn.test/catalog/build-a/button.css',
  ])
  assert.equal(Object.isFrozen(descriptor.css), true)
  assert.deepEqual(calls, { fetch: 1, script: 0 })
  assert.equal(styleLoads, 0)
})

test('isolated styles stay root-owned across initial, lazy, multi-root, late attach, and detach lifecycles', async () => {
  const url = 'https://cdn.test/catalog/manifest.json'
  const remoteExpose = expose('https://cdn.test/catalog/build-a/button.mjs', 'isolated')
  remoteExpose.scope = 'react18'
  remoteExpose.synchronousAssets = [asset('https://cdn.test/catalog/build-a/base.css', 'css', 'text/css')]
  remoteExpose.css = [asset('https://cdn.test/catalog/build-a/button.css', 'css', 'text/css')]
  remoteExpose.asynchronousAssets = [
    asset('https://cdn.test/catalog/build-a/chunks/lazy.css', 'css', 'text/css'),
    asset('https://cdn.test/catalog/build-a/chunks/later.css', 'css', 'text/css'),
    asset('https://cdn.test/catalog/build-a/chunks/detached.css', 'css', 'text/css'),
  ]
  const loads = []
  const { runtime } = createHarness(
    new Map([[url, manifest('catalog', 'build-a', { exposes: { './Button': remoteExpose } })]]),
    new Map(),
    {
      loadStyle(style, context) {
        assert.notEqual(context.styleTarget, undefined, 'isolated CSS must never use the head-style path')
        const node = ownedStyleNode(context.styleTarget, style.url)
        context.styleTarget.append(node)
        loads.push(`${context.styleTarget.name}:${style.url.split('/').at(-1)}`)
        return node
      },
    },
  )
  runtime.registerRemote({ name: 'catalog', manifestUrl: url })
  const firstRoot = openShadowRoot('first')
  const secondRoot = openShadowRoot('second')

  const detachFirstReference = await runtime.attachIsolatedStyleTarget('catalog/Button', firstRoot)
  const detachSecondReference = await runtime.attachIsolatedStyleTarget('catalog/Button', firstRoot)
  assert.deepEqual(firstRoot.children.map(({ url: styleUrl }) => styleUrl.split('/').at(-1)), ['base.css', 'button.css'])

  const lazyRequest = {
    name: 'catalog',
    buildId: 'build-a',
    expose: './Button',
    fileName: 'chunks/lazy.css',
    kind: 'css',
  }
  await Promise.all([runtime.loadFederatedAsset(lazyRequest), runtime.loadFederatedAsset(lazyRequest)])
  const detachSecondRoot = await runtime.attachIsolatedStyleTarget('catalog/Button', secondRoot)
  await runtime.loadFederatedAsset({ ...lazyRequest, fileName: 'chunks/later.css' })

  assert.deepEqual(firstRoot.children.map(({ url: styleUrl }) => styleUrl.split('/').at(-1)), [
    'base.css', 'button.css', 'lazy.css', 'later.css',
  ])
  assert.deepEqual(secondRoot.children.map(({ url: styleUrl }) => styleUrl.split('/').at(-1)), [
    'base.css', 'button.css', 'lazy.css', 'later.css',
  ])
  assert.equal(loads.filter((entry) => entry === 'first:lazy.css').length, 1)

  detachFirstReference()
  assert.equal(firstRoot.children.length, 4, 'one reference must keep the shared root target alive')
  detachSecondReference()
  assert.deepEqual(firstRoot.children, [])
  detachSecondRoot()
  assert.deepEqual(secondRoot.children, [])

  const loadCount = loads.length
  await assert.rejects(
    runtime.loadFederatedAsset({ ...lazyRequest, fileName: 'chunks/detached.css' }),
    (error) => error.code === FEDERATION_ERROR_CODES.STYLE_LOAD && error.retryable === false,
  )
  assert.equal(loads.length, loadCount)
})

test('default browser transport appends isolated links to the ShadowRoot and removes them on detach', async () => {
  const manifestUrl = 'https://cdn.test/catalog/manifest.json'
  const initialUrl = 'https://cdn.test/catalog/b1/button.css'
  const lazyUrl = 'https://cdn.test/catalog/b1/chunks/lazy.css'
  const remoteExpose = expose('https://cdn.test/catalog/b1/button.mjs', 'isolated')
  remoteExpose.scope = 'react18'
  remoteExpose.css = [asset(initialUrl, 'css', 'text/css')]
  remoteExpose.asynchronousAssets = [asset(lazyUrl, 'css', 'text/css')]
  const remoteManifest = manifest('catalog', 'b1', { exposes: { './Button': remoteExpose } })
  const head = {
    children: [],
    append(node) {
      node.parentNode = head
      head.children.push(node)
      queueMicrotask(() => node.onload?.())
    },
  }
  const document = {
    head,
    createElement(tagName) {
      return {
        tagName,
        parentNode: null,
        remove() {
          if (this.parentNode === null) return
          const index = this.parentNode.children.indexOf(this)
          if (index >= 0) this.parentNode.children.splice(index, 1)
          this.parentNode = null
        },
      }
    },
  }
  const fakeWindow = {
    crypto: webcrypto,
    document,
    location: { href: 'https://host.test/' },
    async fetch(url, init = {}) {
      if (url === manifestUrl) {
        return new Response(JSON.stringify(remoteManifest), { headers: { 'content-type': 'application/json' } })
      }
      assert.equal(init.method, 'HEAD')
      assert.equal([initialUrl, lazyUrl].includes(url), true)
      return {
        ok: true,
        status: 200,
        headers: new Headers({ 'content-type': 'text/css', 'content-length': '1' }),
      }
    },
  }
  fakeWindow.window = fakeWindow
  const runtime = createFederationRuntime({ global: fakeWindow, nonce: 'shadow-nonce' })
  runtime.registerRemote({ name: 'catalog', manifestUrl })
  const root = openShadowRoot('browser')

  const detach = await runtime.attachIsolatedStyleTarget('catalog/Button', root)
  await runtime.loadFederatedAsset({
    name: 'catalog',
    buildId: 'b1',
    expose: './Button',
    fileName: 'chunks/lazy.css',
    kind: 'css',
  })

  assert.deepEqual(head.children, [])
  assert.deepEqual(root.children.map(({ href }) => href), [initialUrl, lazyUrl])
  assert.equal(root.children.every(({ nonce }) => nonce === 'shadow-nonce'), true)
  detach()
  assert.deepEqual(root.children, [])
})

test('cross-container remote evaluation cycles fail with a complete logical chain', async () => {
  const alphaUrl = 'https://cdn.test/alpha/manifest.json'
  const betaUrl = 'https://cdn.test/beta/manifest.json'
  const manifests = new Map([
    [alphaUrl, manifest('alpha', 'a1')],
    [betaUrl, manifest('beta', 'b1')],
  ])
  let runtime
  const containers = new Map([
    ['alpha:a1', {
      async init() {},
      async get() { return async () => runtime.loadRemote('beta/Button') },
    }],
    ['beta:b1', {
      async init() {},
      async get() { return async () => runtime.loadRemote('alpha/Button') },
    }],
  ])
  ;({ runtime } = createHarness(manifests, containers))
  runtime.registerRemote({ name: 'alpha', manifestUrl: alphaUrl })
  runtime.registerRemote({ name: 'beta', manifestUrl: betaUrl })

  await assert.rejects(
    runtime.loadRemote('alpha/Button'),
    (error) => error.code === FEDERATION_ERROR_CODES.REMOTE_CYCLE &&
      JSON.stringify(error.details.chain) === JSON.stringify(['alpha/Button', 'beta/Button', 'alpha/Button']),
  )
})

test('explicit requester identity detects delayed async and TLA-style remote cycles before flight re-entry', async () => {
  const alphaUrl = 'https://cdn.test/alpha/manifest.json'
  const betaUrl = 'https://cdn.test/beta/manifest.json'
  const manifests = new Map([
    [alphaUrl, manifest('alpha', 'a1')],
    [betaUrl, manifest('beta', 'b1')],
  ])
  let runtime
  const containers = new Map([
    ['alpha:a1', {
      async init() {},
      async get() {
        return async () => {
          await Promise.resolve()
          return runtime.loadRemote('beta/Button', { name: 'alpha', buildId: 'a1', expose: './Button' })
        }
      },
    }],
    ['beta:b1', {
      async init() {},
      async get() {
        return async () => {
          await new Promise((resolve) => setImmediate(resolve))
          return runtime.loadRemote('alpha/Button', { container: 'beta', buildId: 'b1' })
        }
      },
    }],
  ])
  ;({ runtime } = createHarness(manifests, containers))
  runtime.registerRemote({ name: 'alpha', manifestUrl: alphaUrl })
  runtime.registerRemote({ name: 'beta', manifestUrl: betaUrl })

  await assert.rejects(
    runtime.loadRemote('alpha/Button'),
    (error) => error.code === FEDERATION_ERROR_CODES.REMOTE_CYCLE &&
      JSON.stringify(error.details.chain) === JSON.stringify(['alpha/Button', 'beta/Button', 'alpha/Button']),
  )
})

test('an already executed child provider wins over the current remote fallback', async () => {
  const alphaUrl = 'https://cdn.test/alpha/manifest.json'
  const betaUrl = 'https://cdn.test/beta/manifest.json'
  const manifests = new Map([
    [alphaUrl, manifest('alpha', 'a1', {
      offers: [offer('library', '17.2.0', 'alpha')],
      requirements: [requirement('library', '^17.0.0')],
    })],
    [betaUrl, manifest('beta', 'b1', {
      offers: [offer('library', '17.3.0', 'beta')],
      requirements: [requirement('library', '^17.0.0')],
    })],
  ])
  const loads = { alpha: 0, beta: 0 }
  const resolved = new Map()
  const makeContainer = (name) => ({
    async init(context) {
      resolved.set(name, context.resolved['default:library'])
    },
    async get() {
      return () => ({ default: name })
    },
    async getShared() {
      loads[name] += 1
      return Object.freeze({ source: name })
    },
  })
  const { runtime } = createHarness(manifests, new Map([
    ['alpha:a1', makeContainer('alpha')],
    ['beta:b1', makeContainer('beta')],
  ]))
  runtime.registerRemote({ name: 'alpha', manifestUrl: alphaUrl })
  runtime.registerRemote({ name: 'beta', manifestUrl: betaUrl })

  await runtime.loadRemote('alpha/Button')
  await runtime.loadRemote('beta/Button')

  assert.deepEqual(resolved.get('alpha'), { source: 'alpha' })
  assert.equal(resolved.get('beta'), resolved.get('alpha'))
  assert.deepEqual(loads, { alpha: 1, beta: 0 })
})

test('ordinary shared providers keep compatible major versions side by side in one scope', async () => {
  const runtime = createFederationRuntime({ global: {}, transport: { fetchManifest() {}, loadScript() {} } })
  const version1 = { version: '1.9.4' }
  const version2 = { version: '2.3.1' }
  runtime.registerHostShared([
    { shareKey: 'library', version: '1.9.4', module: version1 },
    { shareKey: 'library', version: '2.3.1', module: version2 },
  ])

  assert.equal(await runtime.resolveShared({ shareKey: 'library', requiredVersion: '^1.0.0' }), version1)
  assert.equal(await runtime.resolveShared({ shareKey: 'library', requiredVersion: '^2.0.0' }), version2)
  assert.equal(await runtime.resolveShared({ shareKey: 'library', requiredVersion: '^1.0.0' }), version1)
})

test('package context and build variant keep same-version virtual instances distinct', async () => {
  const runtime = createFederationRuntime({ global: {}, transport: { fetchManifest() {}, loadScript() {} } })
  const peerA = { identity: 'peer-a/browser' }
  const peerB = { identity: 'peer-b/browser' }
  const worker = { identity: 'peer-a/worker' }
  runtime.registerHostShared([
    {
      shareKey: 'library', version: '1.4.0', packageContext: 'pnp:peer-a', buildVariant: 'browser', module: peerA,
    },
    {
      shareKey: 'library', version: '1.4.0', packageContext: 'pnp:peer-b', buildVariant: 'browser', module: peerB,
    },
    {
      shareKey: 'library', version: '1.4.0', packageContext: 'pnp:peer-a', buildVariant: 'worker', module: worker,
    },
  ])

  assert.equal(await runtime.resolveShared({
    shareKey: 'library', requiredVersion: '^1.0.0', packageContext: 'pnp:peer-a', buildVariant: 'browser',
  }), peerA)
  assert.equal(await runtime.resolveShared({
    shareKey: 'library', requiredVersion: '^1.0.0', packageContext: 'pnp:peer-b', buildVariant: 'browser',
  }), peerB)
  assert.equal(await runtime.resolveShared({
    shareKey: 'library', requiredVersion: '^1.0.0', packageContext: 'pnp:peer-a', buildVariant: 'worker',
  }), worker)
})

test('unsatisfied shared errors and explain list every candidate rejection reason', async () => {
  const url = 'https://cdn.test/diagnostics/manifest.json'
  const remoteManifest = manifest('diagnostics', 'd1', {
    requirements: [requirement('library', '^3.0.0')],
  })
  const container = { init() {}, get() { return () => ({}) } }
  const { runtime } = createHarness(
    new Map([[url, remoteManifest]]),
    new Map([['diagnostics:d1', container]]),
  )
  runtime.registerHostShared([
    {
      shareKey: 'library', version: '2.5.0', packageContext: 'npm:root', buildVariant: 'browser', module: {},
    },
    {
      shareKey: 'library', version: '3.1.0', packageContext: 'npm:peer-b', buildVariant: 'browser', module: {},
    },
    {
      shareKey: 'library', version: '3.2.0', packageContext: 'npm:root', buildVariant: 'worker', module: {},
    },
  ])
  runtime.registerRemote({ name: 'diagnostics', manifestUrl: url })

  let rejected
  await assert.rejects(
    runtime.loadRemote('diagnostics/Button'),
    (error) => {
      rejected = error
      return error.code === FEDERATION_ERROR_CODES.SHARE_UNSATISFIABLE && error.retryable === false
    },
  )
  const candidates = rejected.details.candidates
  assert.deepEqual(candidates.map(({ version }) => version), ['3.2.0', '3.1.0', '2.5.0'])
  assert.deepEqual(candidates.map(({ rejections }) => rejections.map(({ code }) => code)), [
    ['build-variant-mismatch'],
    ['package-context-mismatch'],
    ['version-mismatch'],
  ])
  assert.equal(candidates.every(({ eligible, source }) => !eligible && source === 'host'), true)

  const decision = runtime.explain('diagnostics/Button')
  assert.equal(decision.status, 'error')
  assert.deepEqual(decision.error.details.candidates, candidates)
  assert.equal(Object.isFrozen(decision.error.details.candidates), true)
})

test('singleton consumption freezes the provider and reports incompatible later ranges', async () => {
  const runtime = createFederationRuntime({ global: {}, transport: { fetchManifest() {}, loadScript() {} } })
  runtime.registerHostShared([
    { shareKey: 'react', version: '17.0.2', singleton: true, module: { version: 17 } },
    { shareKey: 'react', version: '18.3.1', singleton: true, module: { version: 18 } },
  ])
  assert.deepEqual(await runtime.resolveShared({ shareKey: 'react', requiredVersion: '^18.0.0', singleton: true }), { version: 18 })
  await assert.rejects(
    runtime.resolveShared({ shareKey: 'react', requiredVersion: '^17.0.0', singleton: true }),
    (error) => error.code === FEDERATION_ERROR_CODES.SHARE_SINGLETON_CONFLICT && error.retryable === false,
  )
  await assert.rejects(
    runtime.resolveShared({ shareKey: 'react', requiredVersion: '^18.0.0', singleton: true, owner: 'other' }),
    (error) => error.code === FEDERATION_ERROR_CODES.SHARE_SINGLETON_CONFLICT &&
      error.details.expectedOwner === 'other',
  )
  await assert.rejects(
    runtime.resolveShared({ shareKey: 'react', requiredVersion: '^18.0.0', singleton: true, buildVariant: 'worker' }),
    (error) => error.code === FEDERATION_ERROR_CODES.SHARE_SINGLETON_CONFLICT,
  )
})

test('non-strict shared requests still reject every semver-incompatible provider', async () => {
  const runtime = createFederationRuntime({ global: {}, transport: { fetchManifest() {}, loadScript() {} } })
  runtime.registerHostShared({ shareKey: 'library', version: '1.9.0', module: { version: 1 } })
  await assert.rejects(
    runtime.resolveShared({ shareKey: 'library', requiredVersion: '^2.0.0', strict: false }),
    (error) => error.code === FEDERATION_ERROR_CODES.SHARE_UNSATISFIABLE,
  )
})

test('React 17 and React 18 singletons coexist in independent share scopes', async () => {
  const runtime = createFederationRuntime({ global: {}, transport: { fetchManifest() {}, loadScript() {} } })
  const react17 = { version: 17 }
  const react18 = { version: 18 }
  runtime.registerHostShared([
    { shareKey: 'react', version: '17.0.2', scope: 'react17', singleton: true, coherenceGroup: 'react', module: react17 },
    { shareKey: 'react', version: '18.3.1', scope: 'react18', singleton: true, coherenceGroup: 'react', module: react18 },
  ])
  assert.equal(await runtime.resolveShared({
    shareKey: 'react', requiredVersion: '^17.0.0', scope: 'react17', singleton: true, coherenceGroup: 'react',
  }), react17)
  assert.equal(await runtime.resolveShared({
    shareKey: 'react', requiredVersion: '^18.0.0', scope: 'react18', singleton: true, coherenceGroup: 'react',
  }), react18)
})

test('coherence groups reject mixed owners before executing any shared factory', async () => {
  const url = 'https://cdn.test/mixed/manifest.json'
  const manifests = new Map([[url, manifest('mixed', 'm1', {
    offers: [offer('react-dom', '18.3.1', 'mixed', { singleton: true, coherenceGroup: 'react18' })],
    requirements: [
      requirement('react', '^18.0.0', { singleton: true, coherenceGroup: 'react18' }),
      requirement('react-dom', '^18.0.0', { singleton: true, coherenceGroup: 'react18' }),
    ],
  })]])
  let fallbackLoads = 0
  const container = {
    init() {},
    get() { return () => ({}) },
    getShared() {
      fallbackLoads += 1
      return {}
    },
  }
  const { runtime } = createHarness(manifests, new Map([['mixed:m1', container]]))
  runtime.registerHostShared({
    shareKey: 'react',
    version: '18.3.1',
    scope: 'default',
    singleton: true,
    coherenceGroup: 'react18',
    packageContext: 'npm:root',
    module: {},
  })
  runtime.registerRemote({ name: 'mixed', manifestUrl: url })
  await assert.rejects(
    runtime.loadRemote('mixed/Button'),
    (error) => error.code === FEDERATION_ERROR_CODES.COHERENCE_CONFLICT,
  )
  assert.equal(fallbackLoads, 0)
})

test('prerelease providers require a prerelease-aware range', async () => {
  const runtime = createFederationRuntime({ global: {}, transport: { fetchManifest() {}, loadScript() {} } })
  const candidate = { prerelease: true }
  runtime.registerHostShared({ shareKey: 'lib', version: '1.0.0-beta.2', module: candidate })
  await assert.rejects(
    runtime.resolveShared({ shareKey: 'lib', requiredVersion: '^1.0.0' }),
    (error) => error.code === FEDERATION_ERROR_CODES.SHARE_UNSATISFIABLE,
  )
  assert.equal(await runtime.resolveShared({ shareKey: 'lib', requiredVersion: '^1.0.0-beta.1' }), candidate)
})

test('non-retryable manifest errors are cached while transient fetch failures can retry', async () => {
  const badUrl = 'https://cdn.test/bad/manifest.json'
  const badManifest = { ...manifest('bad', 'bad-1'), runtimeAbi: 'other.abi' }
  const badHarness = createHarness(new Map(), new Map(), {
    fetchManifest() {
      return badManifest
    },
  })
  badHarness.runtime.registerRemote({ name: 'bad', manifestUrl: badUrl })
  await assert.rejects(badHarness.runtime.loadRemote('bad/Button'), (error) => error.code === FEDERATION_ERROR_CODES.RUNTIME_ABI)
  await assert.rejects(badHarness.runtime.loadRemote('bad/Button'), (error) => error.code === FEDERATION_ERROR_CODES.RUNTIME_ABI)
  assert.equal(badHarness.calls.fetch, 1)

  const retryUrl = 'https://cdn.test/retry/manifest.json'
  const retryContainer = { init() {}, get() { return () => ({ ok: true }) } }
  const retryHarness = createHarness(new Map(), new Map([['retry:r1', retryContainer]]), {
    fetchManifest(_url, count) {
      if (count === 1) throw new Error('temporary outage')
      return manifest('retry', 'r1')
    },
  })
  retryHarness.runtime.registerRemote({ name: 'retry', manifestUrl: retryUrl })
  await assert.rejects(retryHarness.runtime.loadRemote('retry/Button'), (error) => error.code === FEDERATION_ERROR_CODES.MANIFEST_FETCH && error.retryable)
  assert.deepEqual(await retryHarness.runtime.loadRemote('retry/Button'), { ok: true })
  assert.equal(retryHarness.calls.fetch, 2)
})

test('containers with the same local module identifiers remain isolated by name and buildId', async () => {
  const firstUrl = 'https://cdn.test/first/manifest.json'
  const secondUrl = 'https://cdn.test/second/manifest.json'
  const makeContainer = (value) => ({ init() {}, get() { return () => ({ moduleId: 0, value }) } })
  const { runtime } = createHarness(
    new Map([[firstUrl, manifest('first', 'same-build')], [secondUrl, manifest('second', 'same-build')]]),
    new Map([['first:same-build', makeContainer('first')], ['second:same-build', makeContainer('second')]]),
  )
  runtime.registerRemote({ name: 'first', manifestUrl: firstUrl })
  runtime.registerRemote({ name: 'second', manifestUrl: secondUrl })
  assert.deepEqual(await runtime.loadRemote('first/Button'), { moduleId: 0, value: 'first' })
  assert.deepEqual(await runtime.loadRemote('second/Button'), { moduleId: 0, value: 'second' })
})

test('cross-origin HEAD 410 reload control is strict and affects only the requesting development page', async () => {
  const requestedAsset = asset('http://127.0.0.1:4174/@wake/federation/builds/old/lazy.mjs')
  let reloads = 0
  let responseHeaders
  const targetGlobal = {
    location: { reload() { reloads += 1 } },
    async fetch(_url, init) {
      assert.equal(init.method, 'HEAD')
      return { ok: false, status: 410, headers: new Headers(responseHeaders) }
    },
  }
  const context = {
    global: targetGlobal,
    maxAssetSize: 1024,
    assetContext: {
      name: 'catalog',
      buildId: 'old',
      generation: 2,
      development: true,
    },
  }
  const validHeaders = {
    'wake-federation-control': FEDERATION_DEV_LEASE_SCHEMA,
    'wake-federation-action': 'full-reload',
    'wake-federation-remote': 'catalog',
    'wake-federation-current-build-id': 'current',
    'wake-federation-generation': '5',
    'wake-federation-expired-build-id': 'old',
    'wake-federation-reason': 'build-gone',
    'access-control-allow-origin': '*',
    'access-control-expose-headers': 'Wake-Federation-Control, Wake-Federation-Action, Wake-Federation-Remote, Wake-Federation-Current-Build-Id, Wake-Federation-Generation, Wake-Federation-Expired-Build-Id, Wake-Federation-Reason',
  }

  responseHeaders = validHeaders
  await assert.rejects(
    __preflightFederatedAsset(requestedAsset, context),
    (error) => error.code === FEDERATION_ERROR_CODES.NETWORK &&
      error.retryable === false && error.details.reason === 'build-gone',
  )
  assert.equal(reloads, 1)

  responseHeaders = { ...validHeaders, 'wake-federation-generation': '2' }
  await assert.rejects(
    __preflightFederatedAsset(requestedAsset, context),
    (error) => error.code === FEDERATION_ERROR_CODES.NETWORK && error.details.reason === undefined,
  )
  assert.equal(reloads, 1, 'an equal-generation HEAD control cannot refresh a different build')

  responseHeaders = { ...validHeaders, 'wake-federation-remote': 'checkout' }
  await assert.rejects(
    __preflightFederatedAsset(requestedAsset, context),
    (error) => error.code === FEDERATION_ERROR_CODES.NETWORK && error.details.reason === undefined,
  )
  responseHeaders = { ...validHeaders }
  delete responseHeaders['wake-federation-generation']
  await assert.rejects(
    __preflightFederatedAsset(requestedAsset, context),
    (error) => error.code === FEDERATION_ERROR_CODES.NETWORK && error.details.reason === undefined,
  )
  responseHeaders = validHeaders
  await assert.rejects(
    __preflightFederatedAsset(requestedAsset, {
      ...context,
      assetContext: { ...context.assetContext, development: false },
    }),
    (error) => error.code === FEDERATION_ERROR_CODES.NETWORK && error.details.reason === undefined,
  )
  assert.equal(reloads, 1, 'wrong-remote, malformed, and production controls must not refresh the page')
})

test('native GET diagnostics recognize the typed 410 JSON control without trusting malformed bodies', async () => {
  const requestedAsset = asset('http://127.0.0.1:4174/@wake/federation/builds/old/lazy.mjs')
  let reloads = 0
  let bodyText = JSON.stringify(devLeaseReload('catalog', 'current', 5, 'old'))
  let responseHeaders = { 'content-type': 'application/json' }
  const targetGlobal = {
    location: { reload() { reloads += 1 } },
    async fetch(_url, init) {
      assert.equal(init.method, 'GET')
      return new Response(bodyText, {
        status: 410,
        headers: responseHeaders,
      })
    },
  }
  const fallback = new FederationError(FEDERATION_ERROR_CODES.NETWORK, 'native load failed')
  const context = {
    global: targetGlobal,
    maxAssetSize: 1024,
    assetContext: {
      name: 'catalog',
      buildId: 'old',
      generation: 2,
      development: true,
    },
  }
  await assert.rejects(
    __diagnoseFederatedAssetFailure(requestedAsset, context, fallback),
    (error) => error.code === FEDERATION_ERROR_CODES.NETWORK && error.details.reason === 'build-gone',
  )
  assert.equal(reloads, 1)
  bodyText = JSON.stringify(devLeaseReload('catalog', 'current', 2, 'old'))
  await assert.rejects(
    __diagnoseFederatedAssetFailure(requestedAsset, context, fallback),
    (error) => error.code === FEDERATION_ERROR_CODES.NETWORK && error.details.reason === undefined,
  )
  assert.equal(reloads, 1, 'an equal-generation GET control cannot refresh a different build')
  bodyText = 'not-json'
  responseHeaders = {
    'content-type': 'application/json',
    'wake-federation-control': FEDERATION_DEV_LEASE_SCHEMA,
    'wake-federation-action': 'full-reload',
    'wake-federation-remote': 'catalog',
    'wake-federation-current-build-id': 'current',
    'wake-federation-generation': '5',
    'wake-federation-expired-build-id': 'old',
    'wake-federation-reason': 'build-gone',
  }
  await assert.rejects(
    __diagnoseFederatedAssetFailure(requestedAsset, context, fallback),
    (error) => error.code === FEDERATION_ERROR_CODES.NETWORK && error.details.reason === undefined,
  )
  assert.equal(reloads, 1, 'valid headers cannot substitute for a malformed native GET body')
  bodyText = JSON.stringify(devLeaseReload('other', 'current', 5, 'old'))
  responseHeaders = { 'content-type': 'application/json' }
  await assert.rejects(
    __diagnoseFederatedAssetFailure(requestedAsset, context, fallback),
    (error) => error.code === FEDERATION_ERROR_CODES.NETWORK && error.details.reason === undefined,
  )
  assert.equal(reloads, 1)
})

test('native failure diagnostics stream with the manifest size bound and cache an overflow', async () => {
  const manifestUrl = 'https://cdn.test/diagnostic/manifest.json'
  const lazyUrl = 'https://cdn.test/diagnostic/b1/lazy.mjs'
  const remoteManifest = manifest('diagnostic', 'b1')
  remoteManifest.exposes['./Button'].asynchronousAssets = [{
    ...asset(lazyUrl),
    integrity: sri('X'),
  }]
  const manifestBody = JSON.stringify(remoteManifest)
  const counters = {}
  const requests = []
  const scripts = []
  const document = {
    createElement(tagName) {
      return { tagName, remove() {} }
    },
    head: {
      append(element) {
        scripts.push(element)
        queueMicrotask(() => element.onerror?.(new Error('native load rejected')))
      },
    },
  }
  const fakeWindow = {
    crypto: webcrypto,
    document,
    location: { href: 'https://host.test/' },
    async fetch(url, init = {}) {
      requests.push({ url, ...init })
      if (url === manifestUrl) {
        return new Response(manifestBody, { headers: { 'content-type': 'application/json' } })
      }
      if (url === lazyUrl && init.method === 'HEAD') {
        return {
          ok: true,
          status: 200,
          headers: new Headers({ 'content-type': 'text/javascript', 'content-length': '1' }),
        }
      }
      if (url === lazyUrl && init.method === 'GET') {
        return streamedResponse(['X', 'Y'], {
          headers: { 'content-type': 'text/javascript' },
          counters,
        })
      }
      throw new Error(`unexpected request ${init.method ?? 'GET'} ${url}`)
    },
  }
  fakeWindow.window = fakeWindow
  const runtime = createFederationRuntime({ global: fakeWindow })
  runtime.registerRemote({ name: 'diagnostic', manifestUrl })

  const request = {
    name: 'diagnostic',
    buildId: 'b1',
    fileName: 'lazy.mjs',
    kind: 'javascript',
  }
  await assert.rejects(
    runtime.loadFederatedAsset(request),
    (error) => error.code === FEDERATION_ERROR_CODES.ASSET_SIZE &&
      error.phase === 'asset-diagnose' && error.retryable === false && error.details.actual === 2,
  )
  assert.deepEqual(counters, { readers: 1, reads: 2, cancels: 1, releases: 1 })
  const requestCount = requests.length
  await assert.rejects(runtime.loadFederatedAsset(request), (error) => error.code === FEDERATION_ERROR_CODES.ASSET_SIZE)
  assert.equal(requests.length, requestCount)
  assert.equal(scripts.length, 1)
})

test('default browser transport preflights metadata with HEAD and downloads each SRI body once', async () => {
  const manifestUrl = 'https://cdn.test/browser/manifest.json'
  const remoteUrl = 'https://cdn.test/browser/b1/remote.mjs'
  const exposeUrl = 'https://cdn.test/browser/b1/button.mjs'
  const commonUrl = 'https://cdn.test/browser/b1/common.mjs'
  const lazyUrl = 'https://cdn.test/browser/b1/chunks/lazy.mjs'
  const styleUrl = 'https://cdn.test/browser/b1/chunks/lazy.css'
  const tamperedUrl = 'https://cdn.test/browser/b1/chunks/tampered.mjs'
  const headUnsupportedUrl = 'https://cdn.test/browser/b1/chunks/no-head.mjs'
  const sharedLeftUrl = 'https://cdn.test/browser/b1/left/shared.mjs'
  const sharedRightUrl = 'https://cdn.test/browser/b1/right/shared.mjs'
  const remoteB2Url = 'https://cdn.test/browser/b2/remote.mjs'
  const exposeB2Url = 'https://cdn.test/browser/b2/button.mjs'
  const remoteBody = 'R'
  const exposeBody = 'E'
  const commonBody = 'D'
  const lazyBody = 'L'
  const styleBody = 'C'
  const browserManifest = manifest('browser', 'b1')
  browserManifest.remoteEntry = { ...asset(remoteUrl), mime: 'application/javascript', integrity: sri(remoteBody) }
  browserManifest.exposes['./Button'] = {
    ...browserManifest.exposes['./Button'],
    entry: { ...asset(exposeUrl), integrity: sri(exposeBody) },
    synchronousAssets: [{ ...asset(commonUrl), integrity: sri(commonBody) }],
    asynchronousAssets: [
      { ...asset(lazyUrl), integrity: sri(lazyBody) },
      { ...asset(styleUrl, 'css', 'text/css'), integrity: sri(styleBody) },
      { ...asset(tamperedUrl), integrity: sri('X') },
      { ...asset(headUnsupportedUrl), integrity: sri('H') },
      { ...asset(sharedLeftUrl), integrity: sri('S') },
    ],
  }
  browserManifest.exposes['./Card'] = {
    ...expose('https://cdn.test/browser/b1/card.mjs'),
    asynchronousAssets: [{ ...asset(sharedRightUrl), integrity: sri('T') }],
  }
  const browserManifestB2 = manifest('browser', 'b2', { generation: 1 })
  browserManifestB2.remoteEntry = { ...asset(remoteB2Url), integrity: sri('2') }
  browserManifestB2.exposes['./Button'] = {
    ...browserManifestB2.exposes['./Button'],
    entry: { ...asset(exposeB2Url), integrity: sri('B') },
  }
  const browserManifestB3 = manifest('browser', 'b3', { generation: 2 })
  browserManifestB3.remoteEntry = { ...asset(remoteB2Url), integrity: sri('2') }
  let manifestBody = JSON.stringify(browserManifest)
  const scripts = []
  const styles = []
  const executionContextsAtAppend = []
  const requests = []
  const nativeBodyDownloads = []
  let headBodyReads = 0
  let runtime
  const document = {
    createElement(tagName) {
      return { tagName, remove() {} }
    },
    head: {
      append(element) {
        if (element.src !== undefined) scripts.push(element)
        else styles.push(element)
        nativeBodyDownloads.push(element.src ?? element.href)
        const context = fakeWindow[Symbol.for('wake.federation.asset-contexts.v1')]?.get(element.src)
        if (context !== undefined) executionContextsAtAppend.push(context)
        if (element.src === remoteUrl) {
          runtime.registerContainer({
            name: 'browser',
            buildId: 'b1',
            container: { init() {}, get() { return () => ({ secure: true }) } },
          })
        } else if (element.src === remoteB2Url) {
          runtime.registerContainer({
            name: 'browser',
            buildId: 'b2',
            container: { init() {}, get() { return () => ({ secure: 'b2' }) } },
          })
        }
        if (element.src === tamperedUrl) {
          queueMicrotask(() => element.onerror?.(new Error('native SRI rejection')))
          return
        }
        queueMicrotask(() => element.onload?.())
      },
    },
  }
  const headResponse = (body, contentType, status = 200, contentEncoding) => ({
    ok: status >= 200 && status < 300,
    status,
    headers: new Headers({
      'content-length': String(body.length),
      'content-type': contentType,
      ...(contentEncoding === undefined ? {} : { 'content-encoding': contentEncoding }),
    }),
    async arrayBuffer() {
      headBodyReads += 1
      throw new Error('A HEAD preflight must not read a response body')
    },
  })
  const fakeWindow = {
    crypto: webcrypto,
    document,
    location: { href: 'https://host.test/' },
    async fetch(url, init = {}) {
      requests.push({ url, ...init })
      if (url === manifestUrl) return new Response(manifestBody, { headers: { 'content-type': 'application/json' } })
      if (url === tamperedUrl && init.method === 'GET') {
        return new Response('Y', { headers: { 'content-type': 'text/javascript' } })
      }
      assert.equal(init.method, 'HEAD')
      assert.equal(init.mode, 'cors')
      assert.equal(init.redirect, 'error')
      if (url === remoteUrl) return headResponse(remoteBody, 'text/javascript')
      if (url === exposeUrl) return headResponse(exposeBody, 'application/javascript')
      if (url === commonUrl) return headResponse(commonBody, 'text/javascript')
      if (url === lazyUrl) {
        // Encoded transfer bytes do not equal the Manifest's decoded asset size.
        return headResponse('XX', 'text/javascript', 200, 'br')
      }
      if (url === styleUrl) return headResponse(styleBody, 'text/css')
      if (url === tamperedUrl) return headResponse('Y', 'text/javascript')
      if (url === headUnsupportedUrl) return headResponse('', 'text/javascript', 405)
      if (url === sharedLeftUrl) return headResponse('S', 'text/javascript')
      if (url === sharedRightUrl) return headResponse('T', 'text/javascript')
      if (url === remoteB2Url) return headResponse('2', 'text/javascript')
      if (url === exposeB2Url) return headResponse('B', 'text/javascript')
      return headResponse('', 'text/plain', 404)
    },
  }
  fakeWindow.window = fakeWindow
  runtime = createFederationRuntime({ global: fakeWindow, nonce: 'wake-csp-nonce' })
  runtime.registerRemote({ name: 'browser', manifestUrl })

  assert.deepEqual(await runtime.loadRemote('browser/Button'), { secure: true })
  await Promise.all([
    runtime.loadFederatedAsset({ name: 'browser', buildId: 'b1', fileName: 'chunks/lazy.mjs', kind: 'javascript' }),
    runtime.loadFederatedAsset({ name: 'browser', buildId: 'b1', fileName: 'chunks/lazy.mjs', kind: 'javascript' }),
  ])
  await runtime.loadFederatedAsset({ name: 'browser', buildId: 'b1', fileName: 'chunks/lazy.css', kind: 'css' })
  assert.deepEqual(scripts.map((script) => script.src), [remoteUrl, commonUrl, exposeUrl, lazyUrl])
  assert.deepEqual(styles.map((style) => style.href), [styleUrl])
  for (const script of scripts) {
    assert.equal(script.type, 'module')
    assert.equal(script.crossOrigin, 'anonymous')
    assert.match(script.integrity, /^sha384-/u)
    assert.equal(script.nonce, 'wake-csp-nonce')
  }
  assert.equal(styles[0].nonce, 'wake-csp-nonce')
  const contexts = fakeWindow[Symbol.for('wake.federation.asset-contexts.v1')]
  assert.equal(Object.getOwnPropertyDescriptor(fakeWindow, Symbol.for('wake.federation.asset-contexts.v1')).writable, false)
  assert.deepEqual(contexts.get(remoteUrl), { name: 'browser', buildId: 'b1', generation: 0 })
  assert.deepEqual(contexts.get(commonUrl), { name: 'browser', buildId: 'b1', generation: 0 })
  assert.deepEqual(contexts.get(exposeUrl), { name: 'browser', buildId: 'b1', generation: 0, expose: './Button' })
  assert.deepEqual(contexts.get(lazyUrl), { name: 'browser', buildId: 'b1', generation: 0, expose: './Button' })
  assert.equal(Object.isFrozen(contexts.get(lazyUrl)), true)
  assert.equal(executionContextsAtAppend.every(Object.isFrozen), true)
  await assert.rejects(
    runtime.loadFederatedAsset({ name: 'browser', buildId: 'b1', fileName: 'chunks/tampered.mjs', kind: 'javascript' }),
    (error) => error.code === FEDERATION_ERROR_CODES.ASSET_INTEGRITY && error.retryable === false,
  )
  const requestsAfterTamper = requests.length
  const scriptsAfterTamper = scripts.length
  await assert.rejects(
    runtime.loadFederatedAsset({ name: 'browser', buildId: 'b1', fileName: 'chunks/tampered.mjs', kind: 'javascript' }),
    (error) => error.code === FEDERATION_ERROR_CODES.ASSET_INTEGRITY && error.retryable === false,
  )
  assert.equal(requests.length, requestsAfterTamper)
  assert.equal(scripts.length, scriptsAfterTamper)
  await assert.rejects(
    runtime.loadFederatedAsset({ name: 'browser', buildId: 'b1', fileName: 'chunks/no-head.mjs', kind: 'javascript' }),
    (error) => error.code === FEDERATION_ERROR_CODES.NETWORK && error.details.status === 405,
  )
  await assert.rejects(
    runtime.loadFederatedAsset({ name: 'browser', buildId: 'b1', fileName: 'missing.mjs', kind: 'javascript' }),
    (error) => error.code === FEDERATION_ERROR_CODES.ASSET_INTEGRITY,
  )
  await assert.rejects(
    runtime.loadFederatedAsset({ name: 'browser', buildId: 'b1', fileName: 'shared.mjs', kind: 'javascript' }),
    (error) => error.code === FEDERATION_ERROR_CODES.ASSET_INTEGRITY && error.details.matches.length === 2,
  )
  await runtime.loadFederatedAsset({
    name: 'browser',
    buildId: 'b1',
    expose: './Button',
    fileName: 'shared.mjs',
    kind: 'javascript',
  })
  assert.equal(scripts.at(-1).src, sharedLeftUrl)
  assert.equal(contexts.get(sharedLeftUrl).expose, './Button')
  assert.equal(headBodyReads, 0)
  assert.equal(requests.filter(({ url }) => url === manifestUrl).length, 1)
  assert.deepEqual(
    requests.filter(({ url }) => url === tamperedUrl).map(({ method }) => method),
    ['HEAD', 'GET'],
  )
  assert.equal(
    requests.filter(({ url }) => url !== manifestUrl && url !== tamperedUrl).every(({ method }) => method === 'HEAD'),
    true,
  )
  assert.equal(requests.filter(({ url }) => url === lazyUrl).length, 1)
  for (const url of [remoteUrl, commonUrl, exposeUrl, lazyUrl, styleUrl, tamperedUrl, sharedLeftUrl]) {
    assert.equal(nativeBodyDownloads.filter((loadedUrl) => loadedUrl === url).length, 1)
  }
  assert.equal(nativeBodyDownloads.includes(headUnsupportedUrl), false)

  runtime.applyDevUpdate(devUpdate('browser', 'b1', 'b2', 1))
  manifestBody = JSON.stringify(browserManifestB2)
  assert.deepEqual(await runtime.loadRemote('browser/Button'), { secure: 'b2' })
  assert.deepEqual(contexts.get(remoteB2Url), { name: 'browser', buildId: 'b2', generation: 1 })
  assert.deepEqual(contexts.get(remoteUrl), { name: 'browser', buildId: 'b1', generation: 0 })
  assert.throws(
    () => runtime.registerContainer({ name: 'browser', buildId: 'b1', container: {} }),
    (error) => error.code === FEDERATION_ERROR_CODES.CONTAINER_REGISTRATION,
  )

  runtime.applyDevUpdate(devUpdate('browser', 'b2', 'b3', 2))
  manifestBody = JSON.stringify(browserManifestB3)
  const scriptsBeforeUrlReuse = scripts.length
  await assert.rejects(
    runtime.loadRemote('browser/Button'),
    (error) => error.code === FEDERATION_ERROR_CODES.REMOTE_CONFLICT && error.retryable === false,
  )
  assert.equal(scripts.length, scriptsBeforeUrlReuse)
  assert.deepEqual(contexts.get(remoteB2Url), { name: 'browser', buildId: 'b2', generation: 1 })
})
