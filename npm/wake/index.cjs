'use strict'

const { EventEmitter } = require('node:events')
const { loadNative } = require('./loader.cjs')
const {
  registerTestContext,
  setTestContextFatalError,
} = require('./test-context-internal.cjs')
const {
  WakeError,
  fromNativeError,
  invoke,
  splitOptions,
} = require('./errors.cjs')

const native = loadNative()
const INTERNAL_CONTEXT_CONSTRUCTOR = Symbol('Wake context constructor')

function assertInternalContextConstructor(token, name, factory) {
  if (token !== INTERNAL_CONTEXT_CONSTRUCTOR) {
    throw new WakeError('WAKE_CONFIG', `${name} must be created with ${factory}()`)
  }
}

function version() {
  return native.version()
}

async function build(options) {
  const [value, signal] = splitOptions(options)
  return invoke(native.build(JSON.stringify(value), signal))
}

async function bundle(options) {
  const [value, signal] = splitOptions(options)
  return invoke(native.bundle(JSON.stringify(value), signal))
}

function splitTestOptions(options) {
  try {
    return splitOptions(options)
  } catch (error) {
    if (error instanceof WakeError && error.code === 'WAKE_CONFIG') {
      throw new WakeError('WAKE_TEST_CONFIG', error.message, { cause: error })
    }
    throw error
  }
}

async function runTests(options) {
  const [value, signal] = splitTestOptions(options)
  return invoke(native.runTests(
    JSON.stringify(value),
    signal,
    native.__wakeTestHostPath,
  ))
}

class TestContext extends EventEmitter {
  #native
  #closed = false
  #eventPoll

  constructor(handle, token) {
    super()
    assertInternalContextConstructor(token, 'TestContext', 'createTestContext')
    this.#native = handle
    registerTestContext(this, handle)
  }

  get closed() {
    return this.#closed || this.#native.closed
  }

  get watching() {
    return Boolean(this.#native.watching)
  }

  async run() {
    if (this.closed) throw new WakeError('WAKE_TEST_CONTEXT', 'TestContext has already been closed')
    try {
      const result = await invoke(this.#native.run())
      this.#flushEvents()
      return result
    } catch (error) {
      this.#flushEvents()
      throw error
    }
  }

  startWatch() {
    if (this.closed) throw new WakeError('WAKE_TEST_CONTEXT', 'TestContext has already been closed')
    try {
      this.#native.startWatch()
      this.#flushEvents()
      this.#startEventPoll()
      return this
    } catch (error) {
      throw fromNativeError(error)
    }
  }

  stopWatch() {
    if (this.closed) return this
    try {
      this.#native.stopWatch()
      this.#flushEvents()
      this.#stopEventPoll()
      return this
    } catch (error) {
      throw fromNativeError(error)
    }
  }

  async close() {
    if (this.#closed) return
    this.#closed = true
    this.#stopEventPoll()
    try {
      await invoke(this.#native.close())
    } finally {
      this.#flushEvents()
    }
  }

  async [Symbol.asyncDispose]() {
    await this.close()
  }

  #flushEvents() {
    this.#dispatchEvents(this.#readEvents())
  }

  #readEvents() {
    let events
    try {
      events = JSON.parse(this.#native.eventsJson())
    } catch (cause) {
      const detail = cause instanceof Error ? cause.message : String(cause)
      throw new WakeError(
        'WAKE_TEST_HOST',
        `Wake returned an invalid test event stream: ${detail}`,
        { cause },
      )
    }
    if (!Array.isArray(events)) {
      throw new WakeError('WAKE_TEST_HOST', 'Wake returned a non-array test event stream')
    }
    return events
  }

  #dispatchEvents(events) {
    for (const event of events) {
      switch (event.type) {
        case 'runStart':
          this.emit('runStart', { runId: event.runId, watching: event.watching })
          break
        case 'testCaseResult':
          this.emit('testCaseResult', {
            runId: event.runId,
            suiteId: event.suiteId,
            result: event.result,
          })
          break
        case 'suiteResult':
          this.emit('suiteResult', {
            runId: event.runId,
            result: event.result,
          })
          break
        case 'diagnostic':
          this.emit('diagnostic', event.diagnostic)
          break
        case 'runComplete':
          this.emit('runComplete', event.result)
          break
        case 'closed':
          this.emit('closed')
          break
        default:
          throw new WakeError('WAKE_TEST_HOST', `Unknown test-host event: ${String(event.type)}`)
      }
    }
  }

  #startEventPoll() {
    if (this.#eventPoll) return
    this.#eventPoll = setInterval(() => {
      let events
      try {
        events = this.#readEvents()
      } catch (error) {
        this.#stopEventPoll()
        const fatalError = error instanceof WakeError
          ? error
          : new WakeError('WAKE_TEST_HOST', error instanceof Error ? error.message : String(error), {
              cause: error,
            })
        setTestContextFatalError(this, fatalError)
        // Host terminals are drained as ordered native events before this error becomes visible;
        // closing then supplies the final public `closed` event without fabricating a duplicate
        // JavaScript diagnostic.
        void this.close().catch(() => {})
        return
      }
      // User listeners execute outside the native/protocol error boundary. Their exceptions are
      // ordinary EventEmitter failures and must never be relabeled as WAKE_TEST_HOST.
      this.#dispatchEvents(events)
    }, 25)
  }

  #stopEventPoll() {
    if (!this.#eventPoll) return
    clearInterval(this.#eventPoll)
    this.#eventPoll = undefined
  }
}

async function createTestContext(options) {
  const [value] = splitTestOptions(options)
  try {
    return new TestContext(
      native.createTestContext(JSON.stringify(value), native.__wakeTestHostPath),
      INTERNAL_CONTEXT_CONSTRUCTOR,
    )
  } catch (error) {
    throw fromNativeError(error)
  }
}

async function buildLibrary(options) {
  const [value, signal] = splitOptions(options)
  return invoke(native.buildLibrary(JSON.stringify(value), signal))
}

async function generateCssToken(options) {
  const [value, signal] = splitOptions(options)
  return invoke(native.generateCssToken(JSON.stringify(value), signal))
}

async function generateDocgen(options) {
  const [value, signal] = splitOptions(options)
  return invoke(native.generateDocgen(JSON.stringify(value), signal))
}

async function initializeFederation(options) {
  const [value, signal] = splitOptions(options)
  return invoke(native.initializeFederation(JSON.stringify(value), signal))
}

async function generateFederationLock(options) {
  const [value, signal] = splitOptions(options)
  return invoke(native.generateFederationLock(JSON.stringify(value), signal))
}

class BuildContext {
  #native
  #closed = false

  constructor(handle, token) {
    assertInternalContextConstructor(token, 'BuildContext', 'createBuildContext')
    this.#native = handle
  }

  get closed() {
    return this.#closed || this.#native.closed
  }

  async rebuild(changedPaths = [], options) {
    if (this.closed) {
      throw new WakeError('WAKE_INTERNAL', 'BuildContext has already been closed')
    }
    if (!Array.isArray(changedPaths)) {
      options = changedPaths
      changedPaths = []
    }
    const [, signal] = splitOptions(options)
    return invoke(this.#native.rebuild(changedPaths.map(String), signal))
  }

  async close() {
    if (this.#closed) return
    this.#closed = true
    await invoke(this.#native.close())
  }

  async [Symbol.asyncDispose]() {
    await this.close()
  }
}

async function createBuildContext(options) {
  const [value] = splitOptions(options)
  try {
    return new BuildContext(
      native.createBuildContext(JSON.stringify(value)),
      INTERNAL_CONTEXT_CONSTRUCTOR,
    )
  } catch (error) {
    throw fromNativeError(error)
  }
}

class DevServer extends EventEmitter {
  #native
  #closePromise
  #closed = false
  #closing = false
  #eventTimer

  constructor(handle, token) {
    super()
    assertInternalContextConstructor(token, 'DevServer', 'startDevServer')
    this.#native = handle
    this.url = handle.url
    const reference = new WeakRef(this)
    const timer = setInterval(() => {
      const server = reference.deref()
      if (server) server.#drainEvents()
      else clearInterval(timer)
    }, 25)
    this.#eventTimer = timer
    timer.unref()
  }

  #drainEvents() {
    let events
    try {
      events = JSON.parse(this.#native.eventsJson())
    } catch (error) {
      this.emit('diagnostic', {
        severity: 'error',
        code: 'WAKE_INTERNAL',
        message: String(error?.message || error),
      })
      return
    }
    for (const event of events) {
      if (this.#closing && event.type !== 'closed') continue
      if (event.type === 'rebuildStart') {
        this.emit('rebuildStart', event)
      } else if (event.type === 'rebuilt') {
        this.emit('rebuilt', event)
      } else if (event.type === 'workspaceState') {
        this.emit('workspaceState', event)
      } else if (event.type === 'federationUpdated') {
        this.emit('federationUpdated', event)
      } else if (event.type === 'diagnostic') {
        this.emit('diagnostic', event.diagnostic)
      } else if (event.type === 'closed' && !this.#closed) {
        this.#closed = true
        clearInterval(this.#eventTimer)
        this.emit('closed')
      } else {
        this.emit('diagnostic', {
          severity: 'error',
          code: 'WAKE_INTERNAL',
          message: `Unknown development server event: ${String(event?.type)}`,
        })
      }
    }
  }

  async close() {
    if (!this.#closePromise) {
      this.#closing = true
      this.#closePromise = invoke(this.#native.close())
        .then(() => {
          this.#drainEvents()
          if (!this.#closed) {
            this.#closed = true
            clearInterval(this.#eventTimer)
            this.emit('closed')
          }
        })
    }
    return this.#closePromise
  }

  async waitUntilClosed() {
    if (this.#closed) return
    try {
      await invoke(this.#native.waitUntilClosed())
      this.#drainEvents()
      if (!this.#closed) {
        this.#closed = true
        clearInterval(this.#eventTimer)
        this.emit('closed')
      }
    } catch (error) {
      const diagnostic = {
        severity: 'error',
        code: error.code || 'WAKE_INTERNAL',
        message: error.message,
      }
      this.emit('diagnostic', diagnostic)
      throw error
    }
  }

  unref() {
    // Wake owns native threads rather than libuv handles. Not awaiting
    // waitUntilClosed() is therefore the unreferenced state.
    return this
  }

  async [Symbol.asyncDispose]() {
    await this.close()
  }
}

async function startServer(method, options) {
  const [value, signal] = splitOptions(options)
  try {
    return new DevServer(
      await method(JSON.stringify(value), signal),
      INTERNAL_CONTEXT_CONSTRUCTOR,
    )
  } catch (error) {
    throw fromNativeError(error)
  }
}

function startDevServer(options) {
  return startServer(native.startDevServer, options)
}

async function buildDocs(options) {
  const [value, signal] = splitOptions(options)
  return invoke(native.buildDocs(JSON.stringify(value), signal))
}

function startDocsDevServer(options) {
  return startServer(native.startDocsDevServer, options)
}

module.exports = {
  BuildContext,
  DevServer,
  TestContext,
  WakeError,
  build,
  buildLibrary,
  buildDocs,
  bundle,
  runTests,
  generateCssToken,
  generateDocgen,
  initializeFederation,
  generateFederationLock,
  createBuildContext,
  createTestContext,
  startDevServer,
  startDocsDevServer,
  version,
}
