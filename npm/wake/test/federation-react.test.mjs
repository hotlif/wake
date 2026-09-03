import assert from 'node:assert/strict'
import { test } from 'node:test'

import {
  FEDERATION_ERROR_CODES,
  FEDERATION_ISOLATED_REMOUNT_EVENT,
  FederationError,
} from '../federation.mjs'
import {
  createFederatedIsolatedBridge,
  createHostRenderedLazyFactory,
  createIsolatedBridge,
} from '../federation-react.mjs'

class FakeCustomEvent {
  constructor(type, options) {
    this.type = type
    Object.assign(this, options)
  }
}

class FakeEventTarget {
  constructor() {
    this.listeners = new Map()
    this.AbortController = AbortController
    this.CustomEvent = FakeCustomEvent
  }

  addEventListener(type, listener) {
    const listeners = this.listeners.get(type) ?? new Set()
    listeners.add(listener)
    this.listeners.set(type, listeners)
  }

  removeEventListener(type, listener) {
    this.listeners.get(type)?.delete(listener)
  }

  dispatchEvent(event) {
    for (const listener of this.listeners.get(event.type) ?? []) listener(event)
    return true
  }
}

class FakeNode {
  constructor(tagName, ownerDocument) {
    this.tagName = tagName
    this.nodeName = tagName.toUpperCase()
    this.nodeType = tagName === '#shadow-root' ? 11 : 1
    this.ownerDocument = ownerDocument
    this.children = []
    this.parentNode = null
    this.attributes = new Map()
  }

  append(...nodes) {
    for (const node of nodes) {
      node.parentNode = this
      this.children.push(node)
      if (node.tagName === 'link') queueMicrotask(() => node.onload?.())
    }
  }

  setAttribute(name, value) {
    this.attributes.set(name, value)
  }

  cloneNode() {
    return new FakeNode(this.tagName, this.ownerDocument)
  }

  remove() {
    if (this.parentNode === null) return
    const index = this.parentNode.children.indexOf(this)
    if (index >= 0) this.parentNode.children.splice(index, 1)
    this.parentNode = null
  }
}

class FakeShadowRoot extends FakeNode {
  constructor(ownerDocument, host) {
    super('#shadow-root', ownerDocument)
    this.mode = 'open'
    this.host = host
  }
}

class FakeHost extends FakeNode {
  constructor(ownerDocument) {
    super('host', ownerDocument)
    this.shadowRoot = null
    this.events = []
  }

  attachShadow(options) {
    assert.deepEqual(options, { mode: 'open' })
    this.shadowRoot = new FakeShadowRoot(this.ownerDocument, this)
    return this.shadowRoot
  }

  dispatchEvent(event) {
    this.events.push(event)
    return true
  }
}

class FakeDocument {
  constructor() {
    this.defaultView = new FakeEventTarget()
  }

  createElement(tagName) {
    return new FakeNode(tagName, this)
  }
}

test('isolated bridge mounts into an open ShadowRoot with styles and a portal root', async () => {
  const document = new FakeDocument()
  const host = new FakeHost(document)
  const calls = []
  let mountedContext
  const lifecycle = {
    mount(context) {
      calls.push('mount')
      mountedContext = context
      assert.equal(context.shadowRoot.children.some((node) => node.tagName === 'style'), true)
      assert.equal(Object.isFrozen(context.props), true)
      context.emit('ready', { value: 1 })
      return { root: context.mountRoot }
    },
    update(instance, context) {
      calls.push('update')
      assert.equal(instance.root, context.mountRoot)
      return context.props.count
    },
    unmount(instance, context) {
      calls.push('unmount')
      assert.equal(instance.root, context.mountRoot)
    },
  }
  const bridge = createIsolatedBridge({
    load: async () => ({ default: lifecycle }),
    styles: [
      { cssText: ':host { color: red; }' },
      { url: 'https://cdn.test/isolated.css', integrity: 'sha384-test' },
    ],
  })

  const instance = await bridge.mount(host, { count: 1 })
  assert.equal(bridge.status, 'mounted')
  assert.equal(host.shadowRoot.mode, 'open')
  assert.equal(bridge.mountRoot, instance.root)
  assert.equal(mountedContext.portalRoot, bridge.portalRoot)
  const externalStyle = host.shadowRoot.children.find((node) => node.tagName === 'link')
  assert.equal(externalStyle.href, 'https://cdn.test/isolated.css')
  assert.equal(externalStyle.integrity, 'sha384-test')
  assert.equal(externalStyle.crossOrigin, 'anonymous')
  assert.equal(host.events[0].type, 'ready')
  assert.deepEqual(host.events[0].detail, { value: 1 })
  assert.equal(host.events[0].composed, true)
  assert.equal(await bridge.update({ count: 2 }), 2)
  await bridge.unmount()
  assert.equal(bridge.status, 'unmounted')
  assert.deepEqual(host.shadowRoot.children, [])
  assert.deepEqual(calls, ['mount', 'update', 'unmount'])
})

test('isolated bridge rejects React-marked values before calling the remote lifecycle', async () => {
  const document = new FakeDocument()
  const host = new FakeHost(document)
  let loaded = false
  const bridge = createIsolatedBridge({
    load() {
      loaded = true
      return { mount() {}, update() {}, unmount() {} }
    },
  })
  await assert.rejects(
    bridge.mount(host, { child: { $$typeof: Symbol.for('react.element'), type: 'div' } }),
    (error) => error.code === FEDERATION_ERROR_CODES.BRIDGE_PROPS,
  )
  assert.equal(bridge.status, 'failed')
  assert.equal(loaded, false)

  const refBridge = createIsolatedBridge({ load: () => ({ mount() {}, update() {}, unmount() {} }) })
  await assert.rejects(
    refBridge.mount(new FakeHost(document), { ref: { current: null } }),
    (error) => error.code === FEDERATION_ERROR_CODES.BRIDGE_PROPS,
  )
})

test('isolated structured props and event details reject accessors without invoking them', async () => {
  const document = new FakeDocument()
  let loads = 0
  let getterCalls = 0
  const props = {}
  Object.defineProperty(props, 'count', {
    enumerable: true,
    get() {
      getterCalls += 1
      return 1
    },
  })
  const bridge = createIsolatedBridge({
    load() {
      loads += 1
      return { mount() {}, update() {}, unmount() {} }
    },
  })

  await assert.rejects(
    bridge.mount(new FakeHost(document), props),
    (error) => error.code === FEDERATION_ERROR_CODES.BRIDGE_PROPS,
  )
  assert.equal(loads, 0)
  assert.equal(getterCalls, 0)

  const sparse = []
  sparse.length = 1
  const sparseBridge = createIsolatedBridge({ load: () => ({ mount() {}, update() {}, unmount() {} }) })
  await assert.rejects(
    sparseBridge.mount(new FakeHost(document), { sparse }),
    (error) => error.code === FEDERATION_ERROR_CODES.BRIDGE_PROPS,
  )

  const detail = {}
  Object.defineProperty(detail, 'value', {
    enumerable: true,
    get() {
      getterCalls += 1
      return 2
    },
  })
  const eventBridge = createIsolatedBridge({
    load: () => ({
      mount(context) { context.emit('unsafe', detail) },
      update() {},
      unmount() {},
    }),
  })
  await assert.rejects(
    eventBridge.mount(new FakeHost(document)),
    (error) => error.code === FEDERATION_ERROR_CODES.BRIDGE_PROPS,
  )
  assert.equal(getterCalls, 0)
})

test('isolated bridge accepts only own DOM Node slot values without invoking accessors', async () => {
  const document = new FakeDocument()
  document.defaultView.Node = FakeNode
  let loads = 0
  const lifecycle = { mount() {}, update() {}, unmount() {} }
  const invalidSlots = [
    { content: { $$typeof: Symbol.for('react.element'), type: 'div' } },
    { content: { current: null } },
    { content() {} },
    new Map([['content', new FakeNode('div', document)]]),
    { content: { nodeType: 1, nodeName: 'DIV', cloneNode() {} } },
  ]
  let getterCalls = 0
  const accessorSlots = {}
  Object.defineProperty(accessorSlots, 'content', {
    enumerable: true,
    get() {
      getterCalls += 1
      return new FakeNode('div', document)
    },
  })
  invalidSlots.push(accessorSlots)

  for (const slots of invalidSlots) {
    const bridge = createIsolatedBridge({
      load() {
        loads += 1
        return lifecycle
      },
    })
    await assert.rejects(
      bridge.mount(new FakeHost(document), {}, { slots }),
      (error) => error.code === FEDERATION_ERROR_CODES.BRIDGE_PROPS,
    )
  }
  assert.equal(loads, 0)
  assert.equal(getterCalls, 0)
})

test('opted-in isolated bridge statelessly remounts only for its matching remote expose update', async () => {
  const document = new FakeDocument()
  const host = new FakeHost(document)
  const slot = new FakeNode('slot-content', document)
  const mounted = []
  let loads = 0
  let unmounts = 0
  const bridge = createIsolatedBridge({
    dev: { remote: 'catalog', expose: './Button' },
    async load() {
      const load = ++loads
      return {
        mount(context) {
          mounted.push({ load, props: context.props, slot: context.slots.content, root: context.mountRoot })
          return { load }
        },
        update() {},
        unmount() { unmounts += 1 },
      }
    },
  })
  await bridge.mount(host, { count: 7 }, { slots: { content: slot } })

  const emitUpdate = (overrides = {}) => document.defaultView.dispatchEvent(new FakeCustomEvent(
    FEDERATION_ISOLATED_REMOUNT_EVENT,
    {
      detail: {
        schemaVersion: 'wake.federation.dev-update.v1',
        remote: 'catalog',
        oldBuildId: 'old',
        newBuildId: 'new',
        changedExposes: ['./Button'],
        typesHash: 'types-new',
        generation: 2,
        action: 'isolated-remount',
        ...overrides,
      },
    },
  ))

  emitUpdate({ remote: 'other' })
  emitUpdate({ changedExposes: ['./Card'] })
  await new Promise((resolve) => setImmediate(resolve))
  assert.equal(loads, 1)

  emitUpdate()
  await new Promise((resolve) => setImmediate(resolve))
  assert.equal(loads, 2)
  assert.equal(unmounts, 1)
  assert.equal(bridge.status, 'mounted')
  assert.deepEqual(mounted.map(({ load, props }) => ({ load, props })), [
    { load: 1, props: { count: 7 } },
    { load: 2, props: { count: 7 } },
  ])
  assert.equal(mounted[1].slot, slot)
  assert.notEqual(mounted[1].root, mounted[0].root)
  assert.deepEqual(host.shadowRoot.children, [bridge.mountRoot, bridge.portalRoot])

  emitUpdate()
  await new Promise((resolve) => setImmediate(resolve))
  assert.equal(loads, 2)
  await bridge.unmount()
  emitUpdate({ generation: 3, oldBuildId: 'new', newBuildId: 'newer' })
  await new Promise((resolve) => setImmediate(resolve))
  assert.equal(loads, 2)
})

test('federated isolated bridge attaches the ShadowRoot before loading its remote module', async () => {
  const document = new FakeDocument()
  const host = new FakeHost(document)
  const calls = []
  let moduleLoads = 0
  const lifecycle = {
    mount(context) {
      calls.push('mount')
      assert.equal(context.shadowRoot.children.filter((node) => node.tagName === 'link').length, 2)
      return {}
    },
    update() {},
    unmount() {},
  }
  const runtime = {
    async describeRemote(specifier) {
      calls.push('describe')
      assert.equal(specifier, 'catalog/Button')
      return {
        specifier,
        name: 'catalog',
        buildId: 'catalog-1',
        generation: 1,
        development: false,
        expose: './Button',
        mode: 'isolated',
        scope: 'react18',
        shadow: 'open',
        css: [
          { url: 'base.css', integrity: 'sha384-base' },
          { url: 'theme.css', integrity: 'sha384-theme' },
        ],
      }
    },
    async attachIsolatedStyleTarget(specifier, root) {
      calls.push('attach')
      assert.equal(specifier, 'catalog/Button')
      assert.equal(root, host.shadowRoot)
      const nodes = ['base.css', 'theme.css'].map((url) => {
        const link = document.createElement('link')
        link.href = url
        root.append(link)
        return link
      })
      return () => {
        calls.push('detach')
        for (const node of nodes) node.remove()
      }
    },
    async loadRemote() {
      calls.push('loadRemote')
      moduleLoads += 1
      return lifecycle
    },
  }
  const bridge = await createFederatedIsolatedBridge(runtime, 'catalog/Button')

  assert.deepEqual(calls, ['describe'])
  assert.equal(moduleLoads, 0)
  await bridge.mount(host, { count: 1 })
  assert.deepEqual(calls, ['describe', 'attach', 'loadRemote', 'mount'])
  assert.equal(moduleLoads, 1)
  await bridge.unmount()
  assert.equal(calls.at(-1), 'detach')
  assert.deepEqual(host.shadowRoot.children, [])

  await assert.rejects(
    createFederatedIsolatedBridge({
      describeRemote: async () => ({ mode: 'host-rendered', shadow: 'none' }),
      attachIsolatedStyleTarget: async () => () => {},
      loadRemote: async () => lifecycle,
    }, 'catalog/HostButton'),
    (error) => error.code === FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE,
  )
})

test('federated isolated bridge does not load code when style target attachment fails', async () => {
  const document = new FakeDocument()
  const host = new FakeHost(document)
  let moduleLoads = 0
  const runtime = {
    async describeRemote() {
      return {
        name: 'catalog',
        buildId: 'catalog-1',
        generation: 1,
        development: false,
        expose: './Button',
        mode: 'isolated',
        scope: 'react18',
        shadow: 'open',
        css: [],
      }
    },
    async attachIsolatedStyleTarget() {
      throw new FederationError(FEDERATION_ERROR_CODES.ASSET_SIZE, 'stylesheet is too large', {
        phase: 'style-load', retryable: false,
      })
    },
    async loadRemote() {
      moduleLoads += 1
      return { mount() {}, update() {}, unmount() {} }
    },
  }
  const bridge = await createFederatedIsolatedBridge(runtime, 'catalog/Button')

  await assert.rejects(
    bridge.mount(host, {}),
    (error) => error.code === FEDERATION_ERROR_CODES.ASSET_SIZE && error.retryable === false,
  )
  assert.equal(moduleLoads, 0)
  assert.deepEqual(host.shadowRoot.children, [])
})

test('federated development remount detaches the old style target before attaching the new one', async () => {
  const document = new FakeDocument()
  const host = new FakeHost(document)
  const calls = []
  let attachment = 0
  let load = 0
  const runtime = {
    async describeRemote() {
      return {
        name: 'catalog',
        buildId: 'catalog-1',
        generation: 1,
        development: true,
        expose: './Button',
        mode: 'isolated',
        scope: 'react18',
        shadow: 'open',
        css: [],
      }
    },
    async attachIsolatedStyleTarget(_specifier, root) {
      const current = ++attachment
      calls.push(`attach:${current}`)
      const link = document.createElement('link')
      link.href = `style-${current}.css`
      root.append(link)
      return () => {
        calls.push(`detach:${current}`)
        link.remove()
      }
    },
    async loadRemote() {
      const current = ++load
      calls.push(`load:${current}`)
      return {
        mount() { calls.push(`mount:${current}`); return { current } },
        update() {},
        unmount() { calls.push(`unmount:${current}`) },
      }
    },
  }
  const bridge = await createFederatedIsolatedBridge(runtime, 'catalog/Button')
  await bridge.mount(host, {})

  document.defaultView.dispatchEvent(new FakeCustomEvent(FEDERATION_ISOLATED_REMOUNT_EVENT, {
    detail: {
      schemaVersion: 'wake.federation.dev-update.v1',
      remote: 'catalog',
      oldBuildId: 'catalog-1',
      newBuildId: 'catalog-2',
      changedExposes: ['./Button'],
      typesHash: 'types-2',
      generation: 2,
      action: 'isolated-remount',
    },
  }))
  await new Promise((resolve) => setImmediate(resolve))

  assert.deepEqual(calls, [
    'attach:1', 'load:1', 'mount:1',
    'unmount:1', 'detach:1',
    'attach:2', 'load:2', 'mount:2',
  ])
  assert.deepEqual(host.shadowRoot.children.filter(({ tagName }) => tagName === 'link').map(({ href }) => href), [
    'style-2.css',
  ])
  await bridge.unmount()
  assert.deepEqual(calls.slice(-2), ['unmount:2', 'detach:2'])
})

test('host-rendered adapter has React.lazy shape without importing React and is single-flight', async () => {
  const component = function Button() {}
  let loads = 0
  let adaptations = 0
  const lazyFactory = createHostRenderedLazyFactory(
    async () => {
      loads += 1
      return { Button: component }
    },
    {
      exportName: 'Button',
      adapt(value) {
        adaptations += 1
        return value
      },
    },
  )
  const [left, right] = await Promise.all([lazyFactory(), lazyFactory()])
  assert.equal(left, right)
  assert.deepEqual(left, { default: component })
  assert.equal(loads, 1)
  assert.equal(adaptations, 1)
})

test('isolated bridge requires all three lifecycle methods', async () => {
  const document = new FakeDocument()
  const bridge = createIsolatedBridge({ load: () => ({ mount() {} }) })
  await assert.rejects(
    bridge.mount(new FakeHost(document), {}),
    (error) => error.code === FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE,
  )
})
