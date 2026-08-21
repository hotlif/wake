'use strict'

const { EventEmitter } = require('node:events')
const { loadNative } = require('./loader.cjs')
const {
  WakeError,
  fromNativeError,
  invoke,
  splitOptions,
} = require('./errors.cjs')

const native = loadNative()

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

class BuildContext {
  #native
  #closed = false

  constructor(handle) {
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
    return new BuildContext(native.createBuildContext(JSON.stringify(value)))
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

  constructor(handle) {
    super()
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
      } else if (event.type === 'diagnostic') {
        this.emit('diagnostic', event.diagnostic)
      } else if (event.type === 'closed' && !this.#closed) {
        this.#closed = true
        clearInterval(this.#eventTimer)
        this.emit('closed')
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
    return new DevServer(await method(JSON.stringify(value), signal))
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
  WakeError,
  build,
  buildLibrary,
  buildDocs,
  bundle,
  generateCssToken,
  generateDocgen,
  createBuildContext,
  startDevServer,
  startDocsDevServer,
  version,
}
