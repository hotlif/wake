import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { once } from 'node:events'
import { appendFile, cp, mkdtemp, rm } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { createConnection, createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { after, test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { Worker } from 'node:worker_threads'
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
const contexts = []

after(async () => {
  await Promise.all(contexts.map((context) => context.close()))
})

test('loads the same API from ESM and CommonJS', () => {
  assert.equal(version(), '0.1.3')
  assert.equal(commonjs.version(), version())
  assert.equal(typeof commonjs.build, 'function')
})

test('reports platform details when the optional native package is missing', () => {
  const cwd = fileURLToPath(new URL('../../../', import.meta.url))
  const env = { ...process.env }
  delete env.WAKE_NATIVE_PATH
  const script = `
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

test('emits rebuild events and releases the port on idempotent close', async () => {
  const source = fileURLToPath(new URL('../../../fixtures/hello-esm/', import.meta.url))
  const cwd = await mkdtemp(join(tmpdir(), 'wake-node-dev-'))
  await cp(source, cwd, { recursive: true })
  const reservation = createServer()
  const port = await listen(reservation)
  await new Promise((resolve) => reservation.close(resolve))

  const server = await startDevServer({ cwd, port })
  let hmrSocket
  let closedEvents = 0
  server.on('closed', () => closedEvents += 1)
  try {
    assert.equal(server.url, `http://127.0.0.1:${port}/`)
    const [initial] = await once(server, 'rebuilt', { signal: AbortSignal.timeout(10_000) })
    assert.equal(initial.initial, true)
    assert.ok(initial.modules > 0)
    assert.equal(initial.updatedModules + initial.cachedModules, initial.modules)
    assert.ok(initial.chunks > 0)
    hmrSocket = await openHmrSocket(port)
    const rebuilding = once(server, 'rebuildStart', { signal: AbortSignal.timeout(10_000) })
    const rebuilt = once(server, 'rebuilt', { signal: AbortSignal.timeout(10_000) })
    await appendFile(join(cwd, 'src/index.js'), '\nexport const changed = true;\n')
    const [start] = await rebuilding
    assert.equal(start.type, 'rebuildStart')
    assert.equal(start.changedPaths.length, 1)
    const [event] = await rebuilt
    assert.equal(event.type, 'rebuilt')
    assert.equal(event.initial, false)
    assert.ok(event.modules > 0)
    assert.equal(event.updatedModules, 1)
    assert.equal(event.cachedModules, event.modules - 1)
    assert.equal(event.updatedModules + event.cachedModules, event.modules)
    assert.ok(event.durationMs >= 0)
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
  const outdir = fileURLToPath(new URL('../../../target/node-docs-api-test/', import.meta.url))
  const result = await buildDocs({ cwd, outdir })
  assert.equal(result.success, true)
  assert.ok(result.routes.length > 0)

  const reservation = createServer()
  const port = await listen(reservation)
  await new Promise((resolve) => reservation.close(resolve))
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
})
