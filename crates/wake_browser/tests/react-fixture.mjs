import { createBrowserBuildContext, createBrowserPreview } from '/browser.mjs'
import { getFederationRuntime } from '/federation.mjs'

export async function run() {
  const registration = await navigator.serviceWorker.register('/application-service-worker.mjs', { type: 'module', scope: '/' })
  await navigator.serviceWorker.ready
  if (!navigator.serviceWorker.controller) await new Promise(resolve => navigator.serviceWorker.addEventListener('controllerchange', resolve, { once: true }))
  if (await (await fetch('/application-owned')).text() !== 'application handler') throw Error('application Service Worker routing was replaced')
  const dependencies = Object.fromEntries(Object.entries(await (await fetch('/dependencies.json')).json()).map(([path, bytes]) => [path, new Uint8Array(bytes)]))
  const shared = globalThis.__wakeBrowserTestShared
  const hostShared = await (await fetch('/host-shared.json')).json()
  const sharedConfig = Object.fromEntries(Object.keys(shared).map(key => [key, { scope: 'react-preview', owner: 'host', requiredVersion: shared.react.version, singleton: true, strict: true, fallback: false, coherenceGroup: 'react' }]))
  const builder = await createBrowserBuildContext({ wasmUrl: '/wake.wasm', workerUrl: '/browser-worker.mjs', hostShared, federation: {
    enabled: true, name: 'react-preview', exposes: { './page': { entry: 'src/page.tsx', mode: 'host-rendered', scope: 'react-preview' } }, shared: sharedConfig,
  } })
  const source = "import * as React from 'react';import styles from './page.module.css';import {css} from '@crab-dev/css';import image from './image.svg';const pad=css`padding: 7px;`;export const sharedReact=React;export function Page(){const [count]=React.useState(7);return <button data-count={count} className={styles.root+' '+pad}>Browser React<img src={image}/></button>};export const page=<Page/>;export const lazy=()=>import('./lazy.ts');"
  const files = { ...dependencies,
    'src/page.tsx': source,
    'src/page.module.css': '.root { color: rgb(0, 128, 0); }',
    'src/image.svg': '<svg xmlns="http://www.w3.org/2000/svg" width="12" height="8"><rect width="12" height="8" fill="green"/></svg>',
    'src/lazy.ts': 'export const value: number = 17;',
  }
  const output = await builder.build(files)
  let rejectedGlobalCss = false
  try {
    await builder.build({ ...files, 'src/page.tsx': `${source}\nimport './global.css';`, 'src/global.css': 'body { color: red; }' })
  } catch (error) { rejectedGlobalCss = error.code === 'FED_CONFIG_INVALID' }
  if (!rejectedGlobalCss) throw Error('host-rendered global CSS policy was bypassed')
  const runtime = getFederationRuntime()
  runtime.registerHostShared(Object.entries(hostShared).map(([shareKey, identity]) => ({
    shareKey, module: shared[shareKey], ...identity,
    scope: 'react-preview', owner: 'host', singleton: true, strict: true, coherenceGroup: 'react',
  })))
  const preview = await createBrowserPreview({ registration, name: 'react-preview', expose: './page', runtime })
  const host = document.createElement('main'); document.body.append(host)
  let namespace
  async function prepare(exports) {
    // Namespace interop wrappers can differ; the React implementation and Hooks must be shared.
    if (exports.sharedReact.useState !== shared.react.useState || exports.sharedReact.createElement !== shared.react.createElement) throw Error('Preview bundled a second React implementation')
    const element = document.createElement('div')
    const root = shared['react-dom/client'].createRoot(element)
    root.render(exports.page)
    const deadline = performance.now() + 5000
    while (!element.querySelector('button')) {
      if (performance.now() > deadline) throw Error('React render timed out')
      await new Promise(resolve => setTimeout(resolve, 10))
    }
    return { commit() { host.replaceChildren(element); namespace = exports }, dispose() { root.unmount(); element.remove() } }
  }
  await preview.update(output, prepare)
  await host.querySelector('img').decode()
  const styles = getComputedStyle(host.querySelector('button'))
  if (host.querySelector('button').dataset.count !== '7' || styles.paddingTop !== '7px' || styles.color !== 'rgb(0, 128, 0)' || (await namespace.lazy()).value !== 17) throw Error('Hooks, TSX, Wake CSS, CSS Modules or lazy load failed')
  const second = await builder.build({ ...files, 'src/page.tsx': source.replace('Browser React', 'Updated React') })
  await preview.update(second, prepare)
  if (host.textContent !== 'Updated React') throw Error('React preview did not update')
  await preview.dispose(); builder.dispose()
  return { ok: true, react: shared.react.version, shared: true, jsx: true, wakeCss: true, cssModules: true, image: true, lazy: true }
}
