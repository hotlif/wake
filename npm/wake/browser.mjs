export { createBrowserPreview } from './browser-preview.mjs'

export class BrowserBuildError extends Error {
  constructor(code, message, diagnostics = []) {
    super(message)
    this.name = 'BrowserBuildError'
    this.code = code
    this.diagnostics = diagnostics
  }
}

const cancelled = message => new BrowserBuildError('WAKE_CANCELLED', message)
const decodeError = error => new BrowserBuildError(error.code ?? 'WAKE_INTERNAL', error.message ?? String(error), error.diagnostics ?? [])

/** One compiler Worker, bounded to one running and one newest pending immutable snapshot. */
export async function createBrowserBuildContext({ wasmUrl, workerUrl = new URL('./browser-worker.mjs', import.meta.url), ...options }) {
  if (wasmUrl === undefined) throw new BrowserBuildError('WAKE_CONFIG', 'wasmUrl is required')
  const wasm = new URL(wasmUrl, globalThis.location?.href ?? import.meta.url).href
  const workerAddress = new URL(workerUrl, globalThis.location?.href ?? import.meta.url).href
  const configuration = structuredClone(options)
  let worker, initialize, active, queued, sequence = 0, disposed = false
  let ready = false
  function finish(request, error, value) {
    if (request === undefined || request.settled) return
    request.settled = true
    request.signal?.removeEventListener('abort', request.abort)
    if (error) request.reject(error)
    else request.resolve(value)
  }
  function stop() {
    worker?.terminate()
    worker = undefined
    ready = false
    initialize = undefined
  }
  function start() {
    if (initialize !== undefined) return initialize
    const current = new Worker(workerAddress, { type: 'module', name: 'Wake browser compiler' })
    worker = current
    initialize = new Promise((resolve, reject) => {
      current.onmessage = event => {
        if (worker !== current || disposed) return
        const response = event.data
        if (response.id === 0) {
          if (response.ok) { ready = true; resolve() }
          else { reject(decodeError(response.error)); stop() }
          return
        }
        if (active?.id !== response.id) return
        const request = active
        active = undefined
        if (request.id === sequence) finish(request, response.ok ? undefined : decodeError(response.error), response.value)
        else finish(request, cancelled('A newer browser build superseded this request'))
        void pump()
      }
      current.onerror = event => {
        if (worker !== current) return
        const error = new BrowserBuildError('WAKE_INTERNAL', event.message || 'Browser compiler Worker failed')
        reject(error)
        finish(active, error)
        active = undefined
        stop()
        void pump()
      }
      current.postMessage({ id: 0, operation: 'initialize', wasmUrl: wasm, options: configuration })
    })
    return initialize
  }
  async function pump() {
    if (disposed || active || queued === undefined) return
    if (!ready) {
      try { await start() } catch (error) { finish(queued, error); queued = undefined; return }
      if (disposed || active || queued === undefined) return
    }
    active = queued
    queued = undefined
    worker.postMessage({ id: active.id, operation: 'build', files: active.files }, Object.values(active.files).map(bytes => bytes.buffer))
  }
  await start()
  return Object.freeze({
    build(files, { signal } = {}) {
      if (disposed) return Promise.reject(cancelled('Browser build context is disposed'))
      if (signal?.aborted) return Promise.reject(cancelled('Browser build was aborted'))
      let snapshot
      try {
        if (files === null || typeof files !== 'object' || Array.isArray(files)) throw new BrowserBuildError('WAKE_CONFIG', 'files must be a path-to-content record')
        const encoder = new TextEncoder()
        let size = 0
        const entries = Object.entries(files)
        if (entries.length > 20_000) throw new BrowserBuildError('WAKE_CONFIG', 'Virtual project exceeds 20,000 files')
        snapshot = Object.fromEntries(entries.map(([path, value]) => {
          if (typeof value !== 'string' && !(value instanceof Uint8Array)) throw new BrowserBuildError('WAKE_CONFIG', `File ${path} must contain a string or Uint8Array`)
          const bytes = typeof value === 'string' ? encoder.encode(value) : new Uint8Array(value)
          size += bytes.byteLength
          if (size > 128 * 1024 * 1024) throw new BrowserBuildError('WAKE_CONFIG', 'Virtual project exceeds 128 MiB')
          return [path, bytes]
        }))
      } catch (error) { return Promise.reject(error) }
      const id = ++sequence
      finish(queued, cancelled('A newer browser build superseded this request'))
      finish(active, cancelled('A newer browser build superseded this request'))
      return new Promise((resolve, reject) => {
        const request = { id, files: snapshot, signal, resolve, reject, settled: false }
        request.abort = () => {
          finish(request, cancelled('Browser build was aborted'))
          if (queued === request) queued = undefined
          if (active === request) { active = undefined; stop(); void pump() }
        }
        signal?.addEventListener('abort', request.abort, { once: true })
        queued = request
        void pump()
      })
    },
    dispose() {
      if (disposed) return
      disposed = true
      finish(queued, cancelled('Browser build context is disposed'))
      finish(active, cancelled('Browser build context is disposed'))
      queued = active = undefined
      stop()
    },
  })
}
