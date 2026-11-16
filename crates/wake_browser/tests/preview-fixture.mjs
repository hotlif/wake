import { createBrowserBuildContext, createBrowserPreview } from '/browser.mjs'

export async function run() {
  const registration = await navigator.serviceWorker.register('/browser-service-worker.mjs', { type: 'module', scope: '/' })
  await navigator.serviceWorker.ready
  if (!navigator.serviceWorker.controller) await new Promise(resolve => navigator.serviceWorker.addEventListener('controllerchange', resolve, { once: true }))
  const sentinel = document.createElement('input')
  sentinel.value = 'unsaved editor content'
  document.body.append(sentinel)
  const host = document.createElement('main')
  document.body.append(host)
  const options = { wasmUrl: '/wake.wasm', workerUrl: '/browser-worker.mjs', federation: {
    enabled: true, name: 'preview', exposes: { './page': { entry: 'src/page.ts' } },
  } }
  const builder = await createBrowserBuildContext(options)
  const preview = await createBrowserPreview({ registration, name: 'preview', expose: './page' })
  const files = {
    'src/page.ts': "import styles from './page.module.css';import image from './image.svg';export const page={styles,image,value:1};export const lazy=()=>import('./lazy.ts');",
    'src/page.module.css': '.root { color: rgb(255, 0, 0); background-image: url("./image.svg"); }',
    'src/image.svg': '<svg xmlns="http://www.w3.org/2000/svg" width="12" height="8"><rect width="12" height="8" fill="red"/></svg>',
    'src/lazy.ts': 'export const value: number = 42;',
  }
  let namespace
  function prepare(exports) {
    const element = document.createElement('div')
    element.className = exports.page.styles.root
    element.textContent = String(exports.page.value)
    const image = document.createElement('img'); image.src = exports.page.image; element.append(image)
    return { commit() { host.replaceChildren(element); namespace = exports }, dispose() { element.remove() } }
  }
  const first = await builder.build(files)
  await preview.update(first, prepare)
  const oldNamespace = namespace
  const remoteScript = [...document.querySelectorAll('script[type="module"][src]')].find(script => script.src.endsWith('/remoteEntry.mjs'))
  const scriptResponse = await fetch(remoteScript.src)
  const mapUrl = scriptResponse.headers.get('SourceMap')
  if (!mapUrl || (await (await fetch(mapUrl)).json()).version !== 3) throw Error('preview source map is not discoverable from its JavaScript response')
  const image = host.querySelector('img')
  await Promise.race([image.decode(), new Promise((_, reject) => setTimeout(() => reject(Error('image timeout')), 5000))])
  if (image.naturalWidth !== 12 || getComputedStyle(host.firstElementChild).color !== 'rgb(255, 0, 0)') throw Error('initial style/image failed')
  dispatchEvent(new PageTransitionEvent('pagehide', { persisted: true }))
  await new Promise(resolve => setTimeout(resolve, 100))
  if (!(await fetch(image.src)).ok) throw Error('back/forward cache suspension released a live preview')
  let compileFailed = false
  try { await builder.build({ ...files, 'src/page.ts': 'export const = ;' }) } catch (error) { compileFailed = error.code === 'WAKE_BUILD' && error.diagnostics.some(item => item.path === 'src/page.ts' && item.line) }
  if (!compileFailed || host.textContent !== '1') throw Error('failed build replaced the successful page or lost diagnostics')
  const second = await builder.build({ ...files, 'src/page.ts': files['src/page.ts'].replace('value:1', 'value:2'), 'src/page.module.css': '.root { color: rgb(0, 0, 255); }', 'src/lazy.ts': 'export const value = 43;' })
  let prepareFailed = false
  try { await preview.update(second, () => { throw Error('prepare rejected') }) } catch { prepareFailed = true }
  if (!prepareFailed || host.textContent !== '1' || getComputedStyle(host.firstElementChild).color !== 'rgb(255, 0, 0)') throw Error('failed preparation changed old preview')
  await preview.update(second, async exports => {
    if ((await oldNamespace.lazy()).value !== 42) throw Error('old lazy chunk was released early')
    return prepare(exports)
  })
  if ((await namespace.lazy()).value !== 43 || host.textContent !== '2' || getComputedStyle(host.firstElementChild).color !== 'rgb(0, 0, 255)') throw Error('new preview did not become active')
  const superseded = builder.build({ ...files, 'src/page.ts': files['src/page.ts'].replace('value:1', 'value:3') }).then(() => false, error => error.code === 'WAKE_CANCELLED')
  const latest = await builder.build({ ...files, 'src/page.ts': files['src/page.ts'].replace('value:1', 'value:4') })
  if (!await superseded) throw Error('superseded build was returned as current')
  await preview.update(latest, prepare)
  const controller = new AbortController()
  options.federation.name = 'mutated-after-construction'
  const cancelled = builder.build(files, { signal: controller.signal }).then(() => false, error => error.code === 'WAKE_CANCELLED')
  controller.abort()
  if (!await cancelled) throw Error('cancellation failed')
  const recovered = await builder.build(files)
  await preview.update(recovered, prepare)
  if (sentinel.value !== 'unsaved editor content' || !sentinel.isConnected || host.textContent !== '1') throw Error('editor state or cancellation recovery failed')
  let started, unblock, abandoned = false
  const preparing = new Promise(resolve => { started = resolve })
  const gate = new Promise(resolve => { unblock = resolve })
  const stale = preview.update(second, async exports => {
    started(); await gate
    const view = prepare(exports)
    return { commit() { throw Error('stale preview committed') }, dispose() { abandoned = true; view.dispose() } }
  }).then(() => false, error => error.code === 'WAKE_CANCELLED')
  await preparing
  const active = preview.update(latest, prepare)
  unblock()
  if (!await stale) throw Error('stale preview was not cancelled')
  await active
  if (!abandoned || host.textContent !== '4') throw Error('latest preview did not own the visible view')
  await preview.dispose()
  builder.dispose()
  const again = await createBrowserPreview({ registration, name: 'preview', expose: './page' })
  await again.update(first, prepare)
  let readyToClose, finishPreparation
  const ready = new Promise(resolve => { readyToClose = resolve })
  const finish = new Promise(resolve => { finishPreparation = resolve })
  const late = again.update(second, async exports => {
    readyToClose(); await finish
    const view = prepare(exports)
    return { commit() { throw Error('disposed preview committed') }, dispose: view.dispose }
  }).then(() => false, error => error.code === 'WAKE_CANCELLED')
  await ready
  const closed = again.dispose()
  finishPreparation()
  if (!await late) throw Error('dispose did not invalidate a pending preview')
  await closed
  const runtimeRegistry = globalThis[Symbol.for('wake.federation.bundle-runtimes.v1')]
  if (runtimeRegistry?.size) throw Error('compiled runtime registries leaked')
  const cacheNames = await caches.keys()
  if (cacheNames.some(name => name.startsWith('wake-preview-v1:'))) throw Error('preview caches leaked')
  await lostStageReplyIsReleased(first)
  return { ok: true, image: true, css: true, lazy: true, failures: true, latest: true, cancellation: true, editor: true }
}

async function lostStageReplyIsReleased(output) {
  const descriptor = Object.getOwnPropertyDescriptor(navigator, 'serviceWorker')
  const timeout = globalThis.setTimeout
  let releases = 0, preview
  const controller = {
    postMessage(request, ports) {
      // Staging may succeed even when the reply is lost. The client still owns cleanup.
      if (request.operation === 'stage') return
      if (request.operation === 'release') releases++
      ports[0].postMessage({ ok: true, value: null })
    },
  }
  try {
    Object.defineProperty(navigator, 'serviceWorker', { configurable: true, value: { controller } })
    globalThis.setTimeout = (callback, ms, ...args) => timeout(callback, ms === 30_000 ? 20 : ms, ...args)
    preview = await createBrowserPreview({ registration: { active: controller }, name: 'preview', runtime: {
      registerRemote() {}, releasePreviewBuild() {}, unregisterPreviewRemote() {},
    } })
    let failed = false
    try { await preview.update(output, () => { throw Error('unreachable') }) } catch { failed = true }
    if (!failed || releases !== 1) throw Error('lost stage reply leaked its candidate snapshot')
  } finally {
    await preview?.dispose()
    globalThis.setTimeout = timeout
    if (descriptor) Object.defineProperty(navigator, 'serviceWorker', descriptor)
    else delete navigator.serviceWorker
  }
}
