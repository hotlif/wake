import { getFederationRuntime } from './federation.mjs'

const token = () => crypto.randomUUID().replaceAll('-', '')
const cancelled = () => Object.assign(new Error('Preview update was superseded or disposed'), { code: 'WAKE_CANCELLED' })

/** Uses an already installed application Service Worker; it never registers or replaces one. */
export async function createBrowserPreview({ registration, name, expose = './page', runtime = getFederationRuntime() }) {
  const controller = navigator.serviceWorker.controller
  if (!controller || controller !== registration?.active) throw new Error('The current page must be controlled by the supplied preview Service Worker')
  if (typeof name !== 'string' || typeof expose !== 'string') throw new TypeError('Preview name and expose are required')
  const session = token()
  const manifestUrl = new URL(`/__wake_preview__/s/${session}/wake-federation.json`, location.href).href
  runtime.registerRemote({ name, manifestUrl, allowedOrigins: [location.origin], mode: 'preview' })
  let current, disposed = false, sequence = 0, lane = Promise.resolve()
  function send(operation, extra = {}) {
    return new Promise((resolve, reject) => {
      const channel = new MessageChannel()
      const timer = setTimeout(() => { channel.port1.close(); reject(new Error('Preview resource store timed out')) }, 30_000)
      channel.port1.onmessage = event => {
        clearTimeout(timer); channel.port1.close()
        if (event.data.ok) resolve(event.data.value)
        else reject(new Error(event.data.error))
      }
      const active = navigator.serviceWorker.controller
      if (!active) { clearTimeout(timer); channel.port1.close(); reject(new Error('Preview Service Worker is unavailable')); return }
      active.postMessage({ protocol: 'wake.browser.preview.v1', operation, session, ...extra }, [channel.port2])
    })
  }
  async function release(candidate) {
    if (!candidate) return
    try { await candidate.view?.dispose() } finally {
      try { await runtime.releasePreviewBuild(name, candidate.buildId) } finally { await send('release', { snapshot: candidate.snapshot }) }
    }
  }
  const onPageHide = event => { if (!event.persisted) void send('dispose').catch(() => {}) }
  addEventListener('pagehide', onPageHide)
  return Object.freeze({
    update(output, prepare) {
      if (disposed) return Promise.reject(cancelled())
      if (output?.manifest?.name !== name || typeof prepare !== 'function') return Promise.reject(new TypeError('Preview output/name or prepare callback is invalid'))
      const revision = ++sequence
      const run = async () => {
        if (disposed || revision !== sequence) throw cancelled()
        if (current?.buildId === output.manifest.buildId) return { buildId: current.buildId }
        const candidate = { snapshot: token(), buildId: output.manifest.buildId }
        let staged = false
        try {
          await send('stage', { snapshot: candidate.snapshot, manifest: output.manifest, files: output.files })
          staged = true
          if (disposed || revision !== sequence) throw cancelled()
          await send('activate', { snapshot: candidate.snapshot })
          await runtime.refreshPreviewRemote(name)
          const exports = await runtime.loadRemote(`${name}/${expose.replace(/^\.\//, '')}`)
          if (disposed || revision !== sequence) throw cancelled()
          candidate.view = await prepare(exports)
          if (!candidate.view || typeof candidate.view.commit !== 'function' || typeof candidate.view.dispose !== 'function') throw new TypeError('prepare must return synchronous commit() and dispose() callbacks')
          if (disposed || revision !== sequence) throw cancelled()
          const committed = candidate.view.commit()
          if (committed && typeof committed.then === 'function') throw new TypeError('Preview commit must be synchronous')
          const previous = current
          current = candidate
          try { await release(previous) } catch (error) {
            return { buildId: candidate.buildId, cleanupError: String(error) }
          }
          return { buildId: candidate.buildId }
        } catch (error) {
          try {
            if (staged && current && !disposed) {
              await send('activate', { snapshot: current.snapshot })
              await runtime.refreshPreviewRemote(name)
            }
          } finally {
            // The store may have committed even if its stage reply timed out.
            await release(candidate)
          }
          throw error
        }
      }
      const operation = lane.then(run, run)
      lane = operation.catch(() => {})
      return operation
    },
    async dispose() {
      if (disposed) return
      disposed = true
      sequence += 1
      removeEventListener('pagehide', onPageHide)
      await lane
      try { await release(current) } finally {
        current = undefined
        try { await runtime.unregisterPreviewRemote(name) } finally { await send('dispose') }
      }
    },
  })
}
