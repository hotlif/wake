import {
  __diagnoseFederatedAssetFailure,
  __preflightFederatedAsset,
  FEDERATION_DEV_UPDATE_SCHEMA,
  FEDERATION_ERROR_CODES,
  FEDERATION_ISOLATED_REMOUNT_EVENT,
  FederationError,
} from './federation.mjs'

const DEFAULT_STYLE_TIMEOUT_MS = 15_000

function isRecord(value) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

function bridgeError(code, message, details, cause) {
  return new FederationError(code, message, {
    phase: 'react-bridge',
    retryable: false,
    details,
    cause,
  })
}

function cloneStructured(value, path = 'props', seen = new Set()) {
  if (value === null || typeof value === 'string' || typeof value === 'boolean') return value
  if (typeof value === 'number' && Number.isFinite(value)) return value
  if (typeof value !== 'object') {
    throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_PROPS, 'Isolated props must contain only structured data', {
      path,
      valueType: typeof value,
    })
  }
  if (seen.has(value)) {
    throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_PROPS, 'Isolated props must not contain cycles', { path })
  }
  if (Object.getOwnPropertySymbols(value).length > 0) {
    throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_PROPS, 'React values cannot cross an isolated federation boundary', { path })
  }
  const descriptors = Object.getOwnPropertyDescriptors(value)
  const propertyNames = Object.keys(descriptors)
  const looksLikeRef = Object.prototype.hasOwnProperty.call(descriptors, 'current') && propertyNames.length === 1
  if (Object.prototype.hasOwnProperty.call(descriptors, '$$typeof') || looksLikeRef) {
    throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_PROPS, 'React values cannot cross an isolated federation boundary', { path })
  }
  seen.add(value)
  try {
    if (Array.isArray(value)) {
      const clone = []
      for (let index = 0; index < value.length; index += 1) {
        const descriptor = descriptors[String(index)]
        if (!hasOwnDataValue(descriptor) || !descriptor.enumerable) {
          throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_PROPS, 'Structured arrays must be dense own data values', {
            path: `${path}[${index}]`,
          })
        }
        clone.push(cloneStructured(descriptor.value, `${path}[${index}]`, seen))
      }
      if (propertyNames.length !== value.length + 1 || !hasOwnDataValue(descriptors.length)) {
        throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_PROPS, 'Structured arrays must not contain extra properties or accessors', {
          path,
        })
      }
      return Object.freeze(clone)
    }
    const prototype = Object.getPrototypeOf(value)
    if (prototype !== Object.prototype && prototype !== null) {
      throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_PROPS, 'Isolated props must use plain objects and arrays', {
        path,
        prototype: 'non-plain',
      })
    }
    const clone = {}
    for (const [key, descriptor] of Object.entries(descriptors)) {
      if (!descriptor.enumerable) continue
      if (!hasOwnDataValue(descriptor)) {
        throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_PROPS, 'Structured objects must contain only own data values', {
          path: `${path}.${key}`,
        })
      }
      clone[key] = cloneStructured(descriptor.value, `${path}.${key}`, seen)
    }
    return Object.freeze(clone)
  } finally {
    seen.delete(value)
  }
}

function isDomNode(value, document) {
  if (!isRecord(value)) return false
  const constructors = [
    value.ownerDocument?.defaultView?.Node,
    document?.defaultView?.Node,
    globalThis.Node,
  ]
  let hasNodeConstructor = false
  for (const NodeConstructor of constructors) {
    if (typeof NodeConstructor !== 'function') continue
    hasNodeConstructor = true
    if (value instanceof NodeConstructor) return true
  }
  if (hasNodeConstructor) return false
  return Number.isInteger(value.nodeType) && typeof value.nodeName === 'string' &&
    typeof value.cloneNode === 'function'
}

function normalizeSlots(value, document) {
  if (value === undefined) return Object.freeze({})
  if (!isRecord(value)) {
    throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_PROPS, 'Isolated slots must be a plain record of DOM Nodes', {})
  }
  const prototype = Object.getPrototypeOf(value)
  if (prototype !== Object.prototype && prototype !== null) {
    throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_PROPS, 'Isolated slots must use a plain record', {})
  }
  if (Object.getOwnPropertySymbols(value).length > 0) {
    throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_PROPS, 'Isolated slot names must be strings', {})
  }
  const slots = {}
  for (const [name, descriptor] of Object.entries(Object.getOwnPropertyDescriptors(value))) {
    if (!hasOwnDataValue(descriptor) || !descriptor.enumerable || !isDomNode(descriptor.value, document)) {
      throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_PROPS, 'Each isolated slot must be an own DOM Node value', {
        slot: name,
      })
    }
    slots[name] = descriptor.value
  }
  return Object.freeze(slots)
}

function hasOwnDataValue(descriptor) {
  return descriptor !== undefined && Object.prototype.hasOwnProperty.call(descriptor, 'value')
}

function normalizeStyles(styles) {
  if (styles === undefined) return []
  if (!Array.isArray(styles)) {
    throw bridgeError(FEDERATION_ERROR_CODES.STYLE_LOAD, 'styles must be an array', {})
  }
  return styles.map((style, index) => {
    if (typeof style === 'string') return Object.freeze({ url: style })
    if (!isRecord(style) || (typeof style.url !== 'string' && typeof style.cssText !== 'string')) {
      throw bridgeError(FEDERATION_ERROR_CODES.STYLE_LOAD, 'Each style needs url or cssText', { index })
    }
    return Object.freeze({ ...style })
  })
}

function normalizeDevIdentity(value) {
  if (value === undefined) return null
  if (!isRecord(value) || typeof value.remote !== 'string' ||
      !/^[A-Za-z][A-Za-z0-9_-]*$/u.test(value.remote) || value.remote.length > 64 ||
      typeof value.expose !== 'string' || !value.expose.startsWith('./')) {
    throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, 'dev requires a valid remote and canonical expose identity', {})
  }
  const path = value.expose.slice(2)
  if (path.length === 0 || value.expose.length > 256 || !/^[A-Za-z0-9/@_.-]+$/u.test(path) ||
      path.split('/').some((segment) => segment.length === 0 || segment === '.' || segment === '..')) {
    throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, 'dev expose must use canonical ./path form', {
      expose: value.expose,
    })
  }
  return Object.freeze({
    remote: value.remote,
    expose: value.expose,
    eventTarget: value.eventTarget,
  })
}

async function defaultStyleLoader(style, context) {
  if (typeof style.cssText === 'string') {
    const element = context.document.createElement('style')
    if (context.nonce !== undefined) element.nonce = context.nonce
    element.textContent = style.cssText
    context.root.append(element)
    return element
  }
  const targetGlobal = context.document.defaultView ?? globalThis
  const isManifestAsset = style.kind === 'css' && typeof style.integrity === 'string' &&
    typeof style.mime === 'string' && Number.isSafeInteger(style.size)
  if (isManifestAsset) {
    await __preflightFederatedAsset(style, {
      global: targetGlobal,
      signal: context.signal,
      maxAssetSize: context.maxAssetSize,
    })
  }
  return new Promise((resolve, reject) => {
    const element = context.document.createElement('link')
    element.rel = 'stylesheet'
    element.href = style.url
    if (style.integrity !== undefined) {
      element.integrity = style.integrity
      element.crossOrigin = 'anonymous'
    }
    if (context.nonce !== undefined) element.nonce = context.nonce
    const cleanup = () => {
      element.onload = null
      element.onerror = null
      context.signal?.removeEventListener('abort', onAbort)
    }
    const onAbort = () => {
      cleanup()
      element.remove()
      reject(new FederationError(FEDERATION_ERROR_CODES.TIMEOUT, 'Isolated stylesheet loading timed out', {
        phase: 'react-bridge-style',
        retryable: true,
        details: { url: style.url },
      }))
    }
    element.onload = () => {
      cleanup()
      resolve(element)
    }
    element.onerror = (cause) => {
      cleanup()
      element.remove()
      const fallbackError = new FederationError(FEDERATION_ERROR_CODES.STYLE_LOAD, 'Isolated stylesheet failed to load', {
        phase: 'react-bridge-style',
        retryable: true,
        details: { url: style.url },
        cause,
      })
      if (!isManifestAsset) {
        reject(fallbackError)
        return
      }
      void __diagnoseFederatedAssetFailure(style, {
        global: targetGlobal,
        signal: context.signal,
        maxAssetSize: context.maxAssetSize,
      }, fallbackError).catch(reject)
    }
    context.signal?.addEventListener('abort', onAbort, { once: true })
    context.root.append(element)
  })
}

function lifecycleFromNamespace(namespace) {
  const lifecycle = isRecord(namespace?.default) && typeof namespace.default.mount === 'function'
    ? namespace.default
    : namespace
  if (!isRecord(lifecycle) ||
      typeof lifecycle.mount !== 'function' ||
      typeof lifecycle.update !== 'function' ||
      typeof lifecycle.unmount !== 'function') {
    throw bridgeError(
      FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE,
      'An isolated remote must export mount(), update(), and unmount()',
      {},
    )
  }
  return lifecycle
}

function removeNodes(nodes) {
  for (const node of nodes.reverse()) node?.remove?.()
}

export function createIsolatedBridge(options) {
  if (!isRecord(options) || typeof options.load !== 'function') {
    throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, 'createIsolatedBridge requires load()', {})
  }
  if (options.shadowMode !== undefined && options.shadowMode !== 'open') {
    throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, 'Federated isolated roots must use open Shadow DOM', {
      shadowMode: options.shadowMode,
    })
  }
  const styles = normalizeStyles(options.styles)
  const dev = normalizeDevIdentity(options.dev)
  const loadStyle = options.loadStyle ?? defaultStyleLoader
  const timeoutMs = options.styleTimeoutMs ?? DEFAULT_STYLE_TIMEOUT_MS
  let status = 'idle'
  let host
  let shadowRoot
  let mountRoot
  let portalRoot
  let lifecycle
  let instance
  let currentProps
  let currentSlots
  let ownedNodes = []
  let detachStyleTarget
  let mountFlight = null
  let operationTail = Promise.resolve()
  let devEventTarget
  let devListener
  let lastDevGeneration = -1

  const enqueue = (operation) => {
    const next = operationTail.then(operation, operation)
    operationTail = next.catch(() => {})
    return next
  }

  const eventEmitter = (type, detail) => {
    if (typeof type !== 'string' || type.length === 0) {
      throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_PROPS, 'Bridge event type must be a non-empty string', {})
    }
    const cloned = cloneStructured(detail, 'event.detail')
    const EventConstructor = host.ownerDocument?.defaultView?.CustomEvent ?? globalThis.CustomEvent
    if (typeof EventConstructor !== 'function' || typeof host.dispatchEvent !== 'function') {
      throw bridgeError(FEDERATION_ERROR_CODES.UNSUPPORTED_ENVIRONMENT, 'CustomEvent is unavailable', {})
    }
    return host.dispatchEvent(new EventConstructor(type, {
      bubbles: true,
      composed: true,
      detail: cloned,
    }))
  }

  const context = () => Object.freeze({
    host,
    shadowRoot,
    mountRoot,
    portalRoot,
    props: currentProps,
    slots: currentSlots,
    emit: eventEmitter,
  })

  const loadOneStyle = async (style, document) => {
    const Controller = document.defaultView?.AbortController ?? globalThis.AbortController
    const controller = Controller === undefined ? null : new Controller()
    let timer
    try {
      const loaded = await Promise.race([
        Promise.resolve(loadStyle(style, {
          root: shadowRoot,
          document,
          signal: controller?.signal,
          nonce: options.nonce,
          maxAssetSize: options.maxAssetSize ?? 64 * 1024 * 1024,
        })),
        new Promise((_, reject) => {
          timer = setTimeout(() => {
            controller?.abort()
            reject(new FederationError(FEDERATION_ERROR_CODES.TIMEOUT, 'Isolated stylesheet loading timed out', {
              phase: 'react-bridge-style', retryable: true, details: { url: style.url, timeoutMs },
            }))
          }, timeoutMs)
        }),
      ])
      if (loaded !== undefined) ownedNodes.push(loaded)
    } finally {
      clearTimeout(timer)
    }
  }

  const removeDevListener = () => {
    if (devEventTarget !== undefined && devListener !== undefined) {
      devEventTarget.removeEventListener(FEDERATION_ISOLATED_REMOUNT_EVENT, devListener)
    }
    devEventTarget = undefined
    devListener = undefined
  }

  const releaseStyleTarget = () => {
    const detach = detachStyleTarget
    detachStyleTarget = undefined
    detach?.()
  }

  const unmountMounted = async (removeListener) => {
    if (status !== 'mounted') return
    status = 'unmounting'
    try {
      await lifecycle.unmount(instance, context())
    } catch (cause) {
      if (cause instanceof FederationError) throw cause
      throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, 'Isolated remote unmount() failed', {}, cause)
    } finally {
      removeNodes(ownedNodes)
      ownedNodes = []
      releaseStyleTarget()
      status = 'unmounted'
      if (removeListener) removeDevListener()
    }
  }

  const installDevListener = (document) => {
    if (dev === null || devListener !== undefined) return
    const target = dev.eventTarget ?? document.defaultView ?? globalThis
    if (typeof target?.addEventListener !== 'function' || typeof target?.removeEventListener !== 'function') {
      throw bridgeError(FEDERATION_ERROR_CODES.UNSUPPORTED_ENVIRONMENT, 'Federation dev remount requires an EventTarget', {})
    }
    devEventTarget = target
    devListener = (event) => {
      const update = event?.detail
      if (!isRecord(update) || update.schemaVersion !== FEDERATION_DEV_UPDATE_SCHEMA ||
          update.action !== 'isolated-remount' || update.remote !== dev.remote ||
          !Array.isArray(update.changedExposes) || !update.changedExposes.includes(dev.expose) ||
          !Number.isSafeInteger(update.generation) || update.generation <= lastDevGeneration) return
      lastDevGeneration = update.generation
      void enqueue(async () => {
        if (status !== 'mounted') return
        const remountHost = host
        const remountProps = currentProps
        const remountSlots = currentSlots
        await unmountMounted(false)
        status = 'idle'
        return mountInto(remountHost, remountProps, { slots: remountSlots })
      }).catch((error) => {
        if (typeof options.onDevRemountError === 'function') options.onDevRemountError(error, update)
        else globalThis.console?.error?.('[Wake Federation] isolated remount failed', error)
      })
    }
    target.addEventListener(FEDERATION_ISOLATED_REMOUNT_EVENT, devListener)
  }

  const mountInto = async (target, props, mountOptions) => {
    if (!isRecord(target) || typeof target.attachShadow !== 'function') {
      throw bridgeError(FEDERATION_ERROR_CODES.UNSUPPORTED_ENVIRONMENT, 'mount() requires an HTMLElement-like host', {})
    }
    status = 'mounting'
    host = target
    try {
      currentProps = cloneStructured(props)
      const document = target.ownerDocument ?? globalThis.document
      if (document === undefined || typeof document.createElement !== 'function') {
        throw bridgeError(FEDERATION_ERROR_CODES.UNSUPPORTED_ENVIRONMENT, 'A DOM document is required', {})
      }
      currentSlots = normalizeSlots(mountOptions.slots, document)
      shadowRoot = target.shadowRoot ?? target.attachShadow({ mode: 'open' })
      if (shadowRoot.mode !== undefined && shadowRoot.mode !== 'open') {
        throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, 'Existing ShadowRoot must be open', {})
      }
      if (options.attachStyleTarget !== undefined) {
        const detach = await options.attachStyleTarget(shadowRoot)
        if (typeof detach !== 'function') {
          throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, 'Style target attachment must return detach()', {})
        }
        detachStyleTarget = detach
      }
      mountRoot = document.createElement('div')
      mountRoot.setAttribute?.('data-wake-federation-root', '')
      portalRoot = document.createElement('div')
      portalRoot.setAttribute?.('data-wake-federation-portal-root', '')
      shadowRoot.append(mountRoot, portalRoot)
      ownedNodes.push(mountRoot, portalRoot)
      for (const style of styles) await loadOneStyle(style, document)
      lifecycle = lifecycleFromNamespace(await options.load())
      instance = await lifecycle.mount(context())
      status = 'mounted'
      installDevListener(document)
      return instance
    } catch (cause) {
      status = 'failed'
      removeNodes(ownedNodes)
      ownedNodes = []
      releaseStyleTarget()
      if (cause instanceof FederationError) throw cause
      throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, 'Isolated remote mount() failed', {}, cause)
    }
  }

  return Object.freeze({
    get status() {
      return status
    },
    get shadowRoot() {
      return shadowRoot
    },
    get mountRoot() {
      return mountRoot
    },
    get portalRoot() {
      return portalRoot
    },

    mount(target, props = {}, mountOptions = {}) {
      if (mountFlight !== null && (status === 'idle' || status === 'mounting')) return mountFlight
      if (status !== 'idle') {
        return Promise.reject(bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, 'Bridge instances can only be mounted once', { status }))
      }
      mountFlight = enqueue(() => mountInto(target, props, mountOptions))
      return mountFlight
    },

    async update(props) {
      if (mountFlight !== null) await mountFlight
      return enqueue(async () => {
        if (status !== 'mounted') {
          throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, 'update() requires a mounted bridge', { status })
        }
        currentProps = cloneStructured(props)
        try {
          return await lifecycle.update(instance, context())
        } catch (cause) {
          if (cause instanceof FederationError) throw cause
          throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, 'Isolated remote update() failed', {}, cause)
        }
      })
    },

    async unmount() {
      if (mountFlight !== null) {
        try {
          await mountFlight
        } catch {
          return
        }
      }
      return enqueue(async () => {
        if (status === 'unmounted' || status === 'idle') return
        if (status === 'failed') {
          removeDevListener()
          return
        }
        if (status !== 'mounted') {
          throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, 'unmount() requires a mounted bridge', { status })
        }
        await unmountMounted(true)
      })
    },
  })
}

export const createIsolatedReactBridge = createIsolatedBridge
export { FEDERATION_ISOLATED_REMOUNT_EVENT }

export async function createFederatedIsolatedBridge(runtime, specifier, options = {}) {
  if (!isRecord(runtime) || typeof runtime.describeRemote !== 'function' || typeof runtime.loadRemote !== 'function' ||
      typeof runtime.attachIsolatedStyleTarget !== 'function' ||
      typeof specifier !== 'string' || !isRecord(options)) {
    throw bridgeError(
      FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE,
      'createFederatedIsolatedBridge requires a federation runtime, specifier, and options object',
      {},
    )
  }
  const descriptor = await runtime.describeRemote(specifier)
  if (descriptor.mode !== 'isolated' || descriptor.shadow !== 'open') {
    throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, 'Federated bridge requires an isolated expose', {
      specifier,
      mode: descriptor.mode,
      shadow: descriptor.shadow,
    })
  }
  const {
    styles: additionalStyles = [],
    dev: devOptions,
    ...bridgeOptions
  } = options
  if (!Array.isArray(additionalStyles) ||
      (devOptions !== undefined && devOptions !== false && !isRecord(devOptions))) {
    throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, 'Federated isolated bridge options are invalid', {
      specifier,
    })
  }
  const dev = descriptor.development && devOptions !== false
    ? {
        ...(devOptions ?? {}),
        remote: descriptor.name,
        expose: descriptor.expose,
      }
    : undefined
  return createIsolatedBridge({
    ...bridgeOptions,
    styles: additionalStyles,
    attachStyleTarget: (root) => runtime.attachIsolatedStyleTarget(specifier, root),
    load: () => runtime.loadRemote(specifier),
    dev,
  })
}

export const createFederatedIsolatedReactBridge = createFederatedIsolatedBridge

export function createHostRenderedLazyFactory(loadModule, options = {}) {
  if (typeof loadModule !== 'function') {
    throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, 'createHostRenderedLazyFactory requires loadModule()', {})
  }
  const exportName = options.exportName ?? 'default'
  let flight
  return async function wakeHostRenderedLazyFactory() {
    if (flight === undefined) {
      flight = Promise.resolve().then(loadModule).then(async (namespace) => {
        const component = namespace?.[exportName]
        if (component === undefined || component === null) {
          throw bridgeError(FEDERATION_ERROR_CODES.BRIDGE_LIFECYCLE, `Remote export ${JSON.stringify(exportName)} is missing`, {
            exportName,
          })
        }
        const adapted = typeof options.adapt === 'function'
          ? await options.adapt(component, namespace)
          : component
        await options.onResolved?.(adapted, namespace)
        return Object.freeze({ default: adapted })
      })
    }
    try {
      return await flight
    } catch (error) {
      if (error instanceof FederationError && error.retryable) flight = undefined
      throw error
    }
  }
}

export const createHostRenderedAdapter = createHostRenderedLazyFactory
