/** Side-effect-free handlers for integration with an existing application Service Worker. */
const ROOT = '/__wake_preview__/'
const PREFIX = 'wake-preview-v1:'
const encoder = new TextEncoder()
const lanes = new Map()
const token = value => typeof value === 'string' && /^[a-zA-Z0-9_-]{8,80}$/.test(value)
const validPath = path => typeof path === 'string' && path.length > 0 && !/[\\:%?#\x00-\x1f]/.test(path) && path.split('/').every(part => part && part !== '.' && part !== '..')
const response = (bytes, mime) => new Response(bytes, { headers: { 'Content-Type': mime, 'Content-Length': String(bytes.byteLength), 'Cache-Control': 'no-store' } })
const cachePrefix = (client, session) => `${PREFIX}${client}:${session}:`
const basePath = (session, snapshot) => `${ROOT}s/${session}/${snapshot}/`
const manifestPath = session => `${ROOT}s/${session}/wake-federation.json`
const absolute = path => new URL(path, self.location.origin).href

export async function cleanupBrowserPreviews() {
  const live = new Set((await self.clients.matchAll({ includeUncontrolled: true })).map(client => client.id))
  for (const name of await caches.keys()) {
    if (name.startsWith(PREFIX) && !live.has(name.slice(PREFIX.length).split(':')[0])) await caches.delete(name)
  }
}

function records(manifest) {
  const found = []
  const visit = value => {
    if (!value || typeof value !== 'object') return
    if (typeof value.url === 'string' && typeof value.integrity === 'string') { found.push(value); return }
    for (const child of Object.values(value)) visit(child)
  }
  visit(manifest)
  return found
}

async function integrity(bytes) {
  const hash = new Uint8Array(await crypto.subtle.digest('SHA-384', bytes))
  return `sha384-${btoa(String.fromCharCode(...hash))}`
}

async function message(client, data) {
  if (!token(data.session)) throw Error('Invalid preview session')
  const prefix = cachePrefix(client, data.session)
  const index = `${prefix}index`
  if (data.operation === 'dispose') {
    for (const name of await caches.keys()) if (name.startsWith(prefix)) await caches.delete(name)
    return null
  }
  if (!token(data.snapshot)) throw Error('Invalid preview snapshot')
  const cacheName = `${prefix}${data.snapshot}`
  const base = basePath(data.session, data.snapshot)
  const readyUrl = absolute(`${base}.ready`)
  if (data.operation === 'release') { await caches.delete(cacheName); return null }
  if (data.operation === 'activate') {
    const cache = await caches.open(cacheName)
    if (!await cache.match(readyUrl)) throw Error('Preview snapshot is not complete')
    const manifest = await cache.match(absolute(`${base}wake-federation.json`))
    const destination = await caches.open(index)
    await destination.put(absolute(manifestPath(data.session)), manifest)
    return null
  }
  if (data.operation !== 'stage' || !Array.isArray(data.files) || data.files.length > 40_000) throw Error('Invalid preview store request')
  if ((await caches.keys()).filter(name => name.startsWith(prefix) && name !== index).length >= 4) throw Error('Too many retained preview snapshots')
  const files = new Map()
  let size = 0
  for (const file of data.files) {
    if (!validPath(file.path) || !(file.bytes instanceof Uint8Array) || typeof file.mime !== 'string' || files.has(file.path)) throw Error('Invalid preview artifact')
    size += file.bytes.byteLength
    if (size > 256 * 1024 * 1024) throw Error('Preview artifacts exceed 256 MiB')
    files.set(file.path, file)
  }
  const manifest = structuredClone(data.manifest)
  const aliases = new Map()
  for (const asset of records(manifest)) {
    if (!asset.url.startsWith('./') || !validPath(asset.url.slice(2))) throw Error('Preview asset URL must be output-relative')
    const file = files.get(asset.url.slice(2))
    if (!file || file.bytes.byteLength !== asset.size || await integrity(file.bytes) !== asset.integrity) throw Error('Preview artifact integrity mismatch')
    if (asset.kind === 'other') {
      const alias = absolute(`${ROOT}assets/${file.path.split('/').at(-1)}`)
      const previous = aliases.get(alias)
      if (previous && previous.integrity !== asset.integrity) throw Error('Preview asset alias collision')
      aliases.set(alias, { file, integrity: asset.integrity })
    }
    asset.url = absolute(`${base}${asset.url.slice(2)}`)
  }
  const cache = await caches.open(cacheName)
  try {
    for (const file of files.values()) {
      if (file.path === 'wake-federation.json') continue
      const value = response(file.bytes, file.mime)
      if (file.mime === 'text/javascript' && files.has(`${file.path}.map`)) {
        value.headers.set('SourceMap', absolute(`${base}${file.path}.map`))
      }
      await cache.put(absolute(`${base}${file.path}`), value)
    }
    for (const [url, { file, integrity: digest }] of aliases) {
      for (const name of await caches.keys()) {
        if (!name.startsWith(`${PREFIX}${client}:`) || name === cacheName || name.endsWith(':index')) continue
        const previous = await (await caches.open(name)).match(url)
        if (previous && previous.headers.get('X-Wake-Integrity') !== digest) throw Error('Preview asset alias conflicts with an active build')
      }
      const value = response(file.bytes, file.mime)
      value.headers.set('X-Wake-Integrity', digest)
      await cache.put(url, value)
    }
    await cache.put(absolute(`${base}wake-federation.json`), response(encoder.encode(JSON.stringify(manifest)), 'application/json'))
    await cache.put(readyUrl, response(encoder.encode('ready'), 'text/plain'))
  } catch (error) { await caches.delete(cacheName); throw error }
  return { manifestUrl: absolute(manifestPath(data.session)), snapshot: data.snapshot }
}

export function handleBrowserPreviewMessage(event) {
  if (event.data?.protocol !== 'wake.browser.preview.v1') return false
  const port = event.ports[0]
  const client = event.source?.id
  if (!port || !client) return false
  const previous = lanes.get(client) ?? Promise.resolve()
  const work = previous.catch(() => {}).then(async () => {
    await cleanupBrowserPreviews()
    const source = await self.clients.get(client)
    if (!source || new URL(source.url).origin !== self.location.origin) throw Error('Preview owner is not an active same-origin client')
    return message(client, event.data)
  })
  lanes.set(client, work)
  event.waitUntil(work.then(value => port.postMessage({ ok: true, value }), error => port.postMessage({ ok: false, error: String(error) })).finally(() => {
    if (lanes.get(client) === work) lanes.delete(client)
    port.close()
  }))
  return true
}
export function handleBrowserPreviewFetch(event) {
  const url = new URL(event.request.url)
  if (url.origin !== self.location.origin || !url.pathname.startsWith(ROOT)) return false
  event.respondWith((async () => {
    if (!event.clientId || !['GET', 'HEAD'].includes(event.request.method)) return new Response('preview request rejected', { status: 403 })
    let result
    for (const name of await caches.keys()) {
      if (!name.startsWith(`${PREFIX}${event.clientId}:`)) continue
      result = await (await caches.open(name)).match(event.request.url)
      if (result) break
    }
    if (!result) return new Response('preview artifact is unavailable', { status: 404 })
    return event.request.method === 'HEAD' ? new Response(null, { status: result.status, headers: result.headers }) : result
  })())
  return true
}
