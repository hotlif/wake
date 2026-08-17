import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { once } from 'node:events'
import { access, appendFile, cp, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { createConnection, createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { after, test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { Worker } from 'node:worker_threads'
import { assertComponentsRuntime } from '../../../scripts/components-runtime-smoke.mjs'
import {
  WakeError,
  build,
  buildDocs,
  bundle,
  createBuildContext,
  startDevServer,
  startDocsDevServer,
  version,
} from '../index.mjs'
import { analyze, parse, tokenize, transform } from '../experimental.mjs'

const require = createRequire(import.meta.url)
const commonjs = require('../index.cjs')
const packageVersion = JSON.parse(
  await readFile(new URL('../package.json', import.meta.url), 'utf8'),
).version
const contexts = []

after(async () => {
  await Promise.all(contexts.map((context) => context.close()))
})

test('loads the same API from ESM and CommonJS', () => {
  assert.equal(version(), packageVersion)
  assert.equal(commonjs.version(), version())
  assert.equal(typeof commonjs.build, 'function')
})

test('reports platform details when the optional native package is missing', () => {
  const cwd = fileURLToPath(new URL('../../../', import.meta.url))
  const env = { ...process.env }
  delete env.WAKE_NATIVE_PATH
  const script = `
    const Module = require('node:module')
    const load = Module._load
    Module._load = function (request) {
      if (/^@crab-dev\\/wake-(?:win32|linux|darwin)-/.test(request)) {
        const error = new Error('Simulated missing Wake native package')
        error.code = 'MODULE_NOT_FOUND'
        throw error
      }
      return load.apply(this, arguments)
    }
    try { require('./npm/wake/index.cjs') }
    catch (error) { console.log(error.code); console.log(error.message) }
  `
  const result = spawnSync(process.execPath, ['-e', script], {
    cwd,
    env,
    encoding: 'utf8',
  })
  assert.equal(result.status, 0)
  assert.match(result.stdout, /WAKE_UNSUPPORTED_PLATFORM/)
  assert.match(result.stdout, new RegExp(`${process.platform}/${process.arch}`))
  assert.match(result.stdout, /without --omit=optional/)
})

test('exposes disposable experimental compiler handles', async () => {
  const tokens = tokenize('export const answer = 42')
  assert.ok(tokens.tokens.length > 2)

  const module = parse('export const answer = 42')
  assert.equal(module.summary.statementCount, 1)
  assert.match(transform(module).code, /answer/)
  assert.equal(analyze(module).schemaVersion, 'wake.semantic.v1')
  const worker = new Worker('setInterval(() => {}, 1_000)', { eval: true })
  assert.throws(
    () => worker.postMessage(module),
    (error) => error.name === 'DataCloneError',
  )
  await worker.terminate()
  module.dispose()
  assert.equal(module.disposed, true)
  assert.throws(() => module.summary)
})

test('builds, bundles, and serializes context rebuilds', async () => {
  const cwd = fileURLToPath(new URL('../../../fixtures/hello-esm/', import.meta.url))
  const outdir = fileURLToPath(new URL('../../../target/node-api-test/', import.meta.url))

  const result = await build({ cwd, outdir })
  assert.equal(result.success, true)
  assert.ok(result.files.some((file) => file.kind === 'html'))

  const bundled = await bundle({ cwd })
  assert.equal(typeof bundled.code, 'string')
  assert.equal(bundled.outputDir, undefined)

  const context = await createBuildContext({ cwd, outdir })
  contexts.push(context)
  const [first, second] = await Promise.all([
    context.rebuild(),
    context.rebuild(),
  ])
  assert.equal(first.success, true)
  assert.equal(second.success, true)
  await context.close()
  assert.equal(context.closed, true)
  await context.close()
})

test('writes an executable Node CommonJS bundle to an exact outfile', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'wake-node-bundle-'))
  const outfile = join(cwd, 'dist', 'extension.cjs')
  await writeFile(join(cwd, 'package.json'), '{"name":"fixture"}')
  await writeFile(
    join(cwd, 'extension.ts'),
    "import path from 'node:path'; export const answer: number = path.sep.length + 41; " +
      'export const runtimeDir = __dirname; export const runtimeFile = __filename;',
  )
  const result = await bundle({
    cwd,
    entry: 'extension.ts',
    outfile,
    platform: 'node',
    target: 'node20',
  })
  assert.equal(result.outputFile, outfile)
  const loaded = require(outfile)
  assert.equal(loaded.answer, 42)
  assert.equal(loaded.runtimeDir, dirname(outfile))
  assert.equal(loaded.runtimeFile, outfile)
  const mappedOutfile = join(cwd, 'dist', 'mapped.cjs')
  const mapped = await bundle({
    cwd,
    entry: 'extension.ts',
    outfile: mappedOutfile,
    platform: 'node',
    sourceMap: true,
  })
  assert.equal(typeof mapped.sourceMap, 'string')
  assert.equal(mapped.sourceMapFile, `${mappedOutfile}.map`)
  assert.equal(await readFile(`${mappedOutfile}.map`, 'utf8'), mapped.sourceMap)
  assert.match(await readFile(mappedOutfile, 'utf8'), /sourceMappingURL=mapped\.cjs\.map/)
  assert.equal(await readFile(join(cwd, 'package.json'), 'utf8'), '{"name":"fixture"}')
  await rm(cwd, { recursive: true, force: true })
})

async function listen(server, port = 0) {
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(port, '127.0.0.1', resolve)
  })
  return server.address().port
}

async function openHmrSocket(port) {
  const socket = createConnection({ host: '127.0.0.1', port })
  await once(socket, 'connect', { signal: AbortSignal.timeout(10_000) })
  socket.write([
    'GET /__wake_hmr HTTP/1.1',
    `Host: 127.0.0.1:${port}`,
    'Upgrade: websocket',
    'Connection: Upgrade',
    'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==',
    'Sec-WebSocket-Version: 13',
    '',
    '',
  ].join('\r\n'))
  const [response] = await once(socket, 'data', { signal: AbortSignal.timeout(10_000) })
  assert.match(response.toString(), /^HTTP\/1\.1 101 /)
  return socket
}

function onceMatching(emitter, eventName, predicate, timeoutMs = 10_000) {
  return new Promise((resolve, reject) => {
    const unmatched = []
    const timeout = setTimeout(() => {
      cleanup()
      reject(new Error(
        `Timed out waiting for matching ${eventName} event; observed ${JSON.stringify(unmatched)}`,
      ))
    }, timeoutMs)
    timeout.unref()

    const onEvent = (event) => {
      if (!predicate(event)) {
        unmatched.push(event)
        return
      }
      cleanup()
      resolve(event)
    }
    const cleanup = () => {
      clearTimeout(timeout)
      emitter.off(eventName, onEvent)
    }
    emitter.on(eventName, onEvent)
  })
}

test('emits rebuild events and releases the port on idempotent close', async () => {
  const source = fileURLToPath(new URL('../../../fixtures/hello-esm/', import.meta.url))
  const cwd = await mkdtemp(join(tmpdir(), 'wake-node-dev-'))
  await cp(source, cwd, { recursive: true })
  const reservation = createServer()
  const port = await listen(reservation)
  await new Promise((resolve) => reservation.close(resolve))

  // Windows path identity is case-insensitive, while bundler cache keys are lexical. Start from
  // an alternate spelling so this test also locks canonical project roots against notify paths.
  const serverCwd = process.platform === 'win32' ? cwd.toUpperCase() : cwd
  const server = await startDevServer({ cwd: serverCwd, port })
  let hmrSocket
  let closedEvents = 0
  server.on('closed', () => closedEvents += 1)
  try {
    assert.equal(server.url, `http://127.0.0.1:${port}/`)
    const initialBuild = once(server, 'rebuilt', { signal: AbortSignal.timeout(10_000) })
    const rebuilding = once(server, 'rebuildStart', { signal: AbortSignal.timeout(10_000) })
    // Windows can emit more than one watcher notification for one append. Ignore an
    // intermediate no-op rebuild, but still time out if no substantive rebuild follows.
    const rebuilt = onceMatching(
      server,
      'rebuilt',
      (event) => !event.initial && event.updatedModules > 0,
    )
    // `startDevServer()` returning is the readiness contract: an immediate write must
    // already be covered by the native watcher, without a sleep or retry in user code.
    const [[initial], [start], event] = await Promise.all([
      initialBuild,
      rebuilding,
      rebuilt,
      appendFile(join(cwd, 'src/index.js'), '\nexport const changed = true;\n'),
    ])
    assert.equal(initial.initial, true)
    assert.ok(initial.modules > 0)
    assert.equal(initial.updatedModules + initial.cachedModules, initial.modules)
    assert.ok(initial.chunks > 0)
    assert.equal(start.type, 'rebuildStart')
    assert.equal(start.changedPaths.length, 1)
    assert.equal(event.type, 'rebuilt')
    assert.equal(event.initial, false)
    assert.ok(event.modules > 0)
    assert.equal(event.updatedModules, 1)
    assert.equal(event.cachedModules, event.modules - 1)
    assert.equal(event.updatedModules + event.cachedModules, event.modules)
    assert.ok(event.durationMs >= 0)
    hmrSocket = await openHmrSocket(port)
  } finally {
    const closingAt = performance.now()
    try {
      await Promise.all([server.close(), server.waitUntilClosed()])
      const closeDurationMs = performance.now() - closingAt
      assert.ok(closeDurationMs < 2_000, `dev server close took ${closeDurationMs.toFixed(0)}ms`)
    } finally {
      hmrSocket?.destroy()
    }
    await server.close()
  }
  assert.equal(closedEvents, 1)

  const probe = createServer()
  await listen(probe, port)
  await new Promise((resolve) => probe.close(resolve))
  await rm(cwd, { recursive: true, force: true })
})

test('addon cleanup releases native resources when a Worker exits', async () => {
  const source = fileURLToPath(new URL('../../../fixtures/hello-esm/', import.meta.url))
  const cwd = await mkdtemp(join(tmpdir(), 'wake-node-worker-'))
  await cp(source, cwd, { recursive: true })
  const reservation = createServer()
  const port = await listen(reservation)
  await new Promise((resolve) => reservation.close(resolve))

  const worker = new Worker(`
    const { parentPort, workerData } = require('node:worker_threads')
    ;(async () => {
      const wake = require(workerData.api)
      globalThis.server = await wake.startDevServer({
        cwd: workerData.cwd,
        port: workerData.port,
      })
      parentPort.postMessage('started')
    })().catch((error) => { throw error })
  `, {
    eval: true,
    workerData: {
      api: fileURLToPath(new URL('../index.cjs', import.meta.url)),
      cwd,
      port,
    },
  })
  const exited = once(worker, 'exit')
  const [message] = await once(worker, 'message')
  assert.equal(message, 'started')
  const [exitCode] = await exited
  assert.equal(exitCode, 0)

  const probe = createServer()
  await listen(probe, port)
  await new Promise((resolve) => probe.close(resolve))
  await rm(cwd, { recursive: true, force: true })
})

test('builds docs and controls the docs dev server lifecycle', async () => {
  const cwd = fileURLToPath(new URL('../../../', import.meta.url))
  const componentsRuntime = await readFile(
    fileURLToPath(new URL('../internal/components-runtime.mjs', import.meta.url)),
    'utf8',
  )
  assert.doesNotMatch(
    componentsRuntime,
    /["'][^"'\r\n]+\.css(?:\?[^"'\r\n]*)?["']/,
    'Components runtime must rely on Wake auto-style injection and contain no CSS import',
  )
  const outdir = fileURLToPath(new URL('../../../target/node-docs-api-test/', import.meta.url))
  const result = await buildDocs({ cwd, outdir })
  assert.equal(result.success, true)
  assert.ok(result.routes.length > 0)
  const componentsCwd = fileURLToPath(new URL('../../../fixtures/react-docs/', import.meta.url))
  const componentsRoot = await mkdtemp(join(tmpdir(), 'wake-components-api-'))
  const componentsOutdir = join(componentsRoot, 'dist')
  const workbench = await buildDocs({
    cwd: componentsCwd,
    outdir: componentsOutdir,
    basePath: '/workbench/',
    mode: 'components',
  })
  assert.equal(workbench.mode, 'components')
  assert.deepEqual(workbench.routes, [])
  assert.ok(workbench.demos.some((demo) => demo.component === '按钮' && demo.controlCount > 0))
  const generatedComponentsRuntime = await readFile(
    join(componentsCwd, '.wake/docs/generated/runtime/components.tsx'),
    'utf8',
  )
  assert.match(
    generatedComponentsRuntime,
    /from\s+["']@crab-dev\/wake\/internal\/components-runtime["']/,
    'Generated workbench code must import only the Wake Components runtime',
  )
  assert.doesNotMatch(generatedComponentsRuntime, /from\s+["']@crab-dev\/rc-/)
  assert.doesNotMatch(
    generatedComponentsRuntime,
    /["'][^"'\r\n]+\.css(?:\?[^"'\r\n]*)?["']/,
  )
  await Promise.all([
    access(join(componentsOutdir, 'index.html')),
    access(join(componentsOutdir, '404.html')),
  ])
  const componentFiles = await readdir(componentsOutdir)
  const componentManifest = JSON.parse(
    await readFile(join(componentsOutdir, 'manifest.json'), 'utf8'),
  )
  const componentEntryFile = componentManifest.entry
  assert.match(
    componentEntryFile,
    /^entry\.[0-9a-f]{8}\.js$/,
    'Components build must emit a content-hashed JavaScript entry',
  )
  assert.ok(
    componentFiles.includes(componentEntryFile),
    'Components build must emit the JavaScript entry named by its manifest',
  )
  await assertComponentsRuntime(join(componentsOutdir, componentEntryFile))
  const componentCssFile = componentFiles.find((file) => /^styles\.[0-9a-f]{8}\.css$/.test(file))
  assert.ok(componentCssFile, 'Components build must emit a CSS asset')
  const componentHtml = await readFile(join(componentsOutdir, 'index.html'), 'utf8')
  assert.match(
    componentHtml,
    new RegExp(`href=["'][^"']*${componentCssFile.replaceAll('.', '\\.')}["']`),
    'index.html must link the emitted hashed CSS asset',
  )
  const componentCss = await readFile(join(componentsOutdir, componentCssFile), 'utf8')
  for (const prefix of [
    'rc-checkbox-',
    'rc-dropdown-container-',
    'rc-spin-',
    'rc-virtual-',
    'rc-alert-',
    'rc-button-',
    'rc-dialog-',
    'rc-drawer-',
    'rc-empty-',
    'rc-line-edit-',
    'rc-number-edit-',
    'rc-segmented-',
    'rc-select-',
    'rc-switch-',
    'rc-tag-',
    'rc-text-edit-',
    'rc-tooltip-',
    'rc-tree-',
  ]) {
    assert.match(componentCss, new RegExp(prefix), `Components CSS must include ${prefix}`)
  }

  const reservation = createServer()
  const port = await listen(reservation)
  await new Promise((resolve) => reservation.close(resolve))
  await rm(componentsRoot, { recursive: true, force: true })
  const server = await startDocsDevServer({ cwd, port })
  await server.close()
  const probe = createServer()
  await listen(probe, port)
  await new Promise((resolve) => probe.close(resolve))

  const invalid = fileURLToPath(new URL('../../../fixtures/hello-esm/', import.meta.url))
  await assert.rejects(
    buildDocs({ cwd: invalid }),
    (error) => error instanceof WakeError && error.code === 'WAKE_BUILD',
  )
})

test('normalizes cancellation and configuration failures', async () => {
  const controller = new AbortController()
  controller.abort()
  await assert.rejects(
    build({ cwd: '.', signal: controller.signal }),
    (error) => error instanceof WakeError && error.code === 'WAKE_CANCELLED',
  )

  await assert.rejects(
    build({ cwd: 'does-not-exist' }),
    (error) => error instanceof WakeError && error.code === 'WAKE_CONFIG',
  )

  await assert.rejects(
    bundle({ minify: true, sourceMap: true }),
    (error) => error instanceof WakeError && error.code === 'WAKE_CONFIG',
  )
})
