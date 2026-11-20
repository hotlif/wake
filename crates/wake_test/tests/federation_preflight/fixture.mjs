async (sources) => {
  const { createFederationRuntime } = await import('/runtime.mjs')
  const originalFetch = window.fetch.bind(window)
  const bytes = (text) => new TextEncoder().encode(text)
  const integrity = async (body) => {
    const digest = new Uint8Array(await crypto.subtle.digest('SHA-384', bytes(body)))
    return `sha384-${btoa(String.fromCharCode(...digest))}`
  }
  const initialize = async (buildId) => {
    const prefix = `${location.origin}/${buildId}`
    const manifestUrl = `${prefix}/manifest.json`
    const byName = Object.fromEntries(await Promise.all(sources.map(async ([name, body]) => [
      name,
      {
        url: `${prefix}/${name}`,
        kind: name.endsWith('.css') ? 'css' : 'javascript',
        mime: name.endsWith('.css') ? 'text/css' : 'text/javascript',
        contentHash: name,
        size: bytes(body).length,
        integrity: await integrity(body),
      },
    ])))
    const types = {
      buildId, url: `${prefix}/types.json`, contentHash: 'types',
      integrity: await integrity('{}'), size: 2, format: 'declaration-bundle',
    }
    const manifest = {
      schemaVersion: 'wake.federation.manifest.v1',
      runtimeAbi: 'wake.federation.v1',
      name: 'catalog', buildId, browserTarget: 'chrome >= 120',
      remoteEntry: byName['container.mjs'],
      types,
      exposes: {
        './Button': {
          mode: 'isolated', scope: 'button', shadow: 'open',
          entry: byName['entry.mjs'],
          css: [byName['button.css']],
          synchronousAssets: [byName['first.mjs'], byName['base.css'], byName['second.mjs']],
          asynchronousAssets: [],
        },
      },
      shared: { offers: [], requirements: [] },
    }
    const body = JSON.stringify(manifest)
    window.fetch = (url, options) => url === manifestUrl
      ? Promise.resolve(new Response(body, { headers: { 'content-type': 'application/json' } }))
      : originalFetch(url, options)
    globalThis.execution = []
    globalThis.runtime = createFederationRuntime()
    runtime.registerRemote({
      name: 'catalog', manifestUrl, mode: 'production', assetPreflight: 'verified-get',
      lock: {
        buildId, manifestUrl, manifestIntegrity: await integrity(body), hasExposes: true,
        typesIntegrity: types.integrity,
        allowedAssets: Object.fromEntries([...Object.values(byName), types].map((asset) => [asset.url, asset.integrity])),
      },
    })
    return new Map(Object.values(byName).map((asset) => [asset.url, asset.integrity]))
  }
  const root = () => {
    const host = document.createElement('div')
    host.style.color = 'black'
    document.body.append(host)
    const shadow = host.attachShadow({ mode: 'open' })
    const probe = document.createElement('span')
    probe.className = 'probe'
    shadow.append(probe)
    return { shadow, probe }
  }
  const attach = (target) => runtime.attachIsolatedStyleTarget('catalog/Button', target.shadow)
  const load = () => runtime.loadRemote('catalog/Button')
  const code = async (promise) => promise.then(() => 'unexpected-success', (error) => error.code)
  const links = (target) => [...target.shadow.querySelectorAll('link[rel=stylesheet]')]

  const expected = await initialize('valid')
  const roots = [root(), root()]
  await Promise.all(roots.map(attach))
  const value = await load()
  const installed = [...document.querySelectorAll('script[src]'), ...roots.flatMap(links)]
  const valid = {
    value,
    execution: [...execution],
    styles: roots.map((target) => links(target).map((node) => node.href.split('/').at(-1))),
    colors: roots.map(({ probe }) => getComputedStyle(probe).color),
    integrity: installed.length === 8 && installed.every((node) =>
      node.integrity === expected.get(node.src ?? node.href)),
  }
  await initialize('preflight-tamper')
  const preflightTarget = root()
  const preflight = [await code(attach(preflightTarget)), links(preflightTarget).length]
  await initialize('native-css-tamper')
  const cssTarget = root()
  const nativeCss = [await code(attach(cssTarget)), links(cssTarget).length, getComputedStyle(cssTarget.probe).color]
  await initialize('native-js-tamper')
  const nativeJs = [await code(load()), [...execution]]
  await initialize('redirect')
  const redirectTarget = root()
  const redirect = [await code(attach(redirectTarget)), links(redirectTarget).length]
  return { valid, preflight, nativeCss, nativeJs, redirect }
}
