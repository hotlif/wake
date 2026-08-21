import assert from 'node:assert/strict'
import { EventEmitter } from 'node:events'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import {
  applyDashboardEvent,
  createDashboardSession,
  createDashboardState,
  createUi,
  formatBanner,
  formatBuildResult,
  formatDiagnostic,
  formatServerReady,
  humanDuration,
  observeServer,
  renderDashboard,
  setDashboardEndpoint,
  stripAnsi,
  supportsColor,
  supportsTui,
} from '../bin/terminal.mjs'

const diagnosticContract = JSON.parse(readFileSync(
  new URL('../../../fixtures/terminal-diagnostic-contract.json', import.meta.url),
  'utf8',
))

test('terminal capability detection follows tty, NO_COLOR, and TERM', () => {
  assert.equal(supportsColor({ isTTY: true }, {}), true)
  assert.equal(supportsColor({ isTTY: false }, {}), false)
  assert.equal(supportsColor({ isTTY: true }, { NO_COLOR: '' }), false)
  assert.equal(supportsTui({ isTTY: true }, { isTTY: true }, {}), true)
  assert.equal(supportsTui({ isTTY: true }, { isTTY: true }, { TERM: 'dumb' }), false)
  assert.equal(supportsTui({ isTTY: false }, { isTTY: true }, {}), false)
})

test('plain static reports use the branded English hierarchy', () => {
  const ui = createUi(false)
  assert.deepEqual(formatBanner(ui, 'docs build', '0.1.3'), [
    '',
    '  ⚡ WAKE / DOCS BUILD  v0.1.3',
    '',
  ])
  assert.deepEqual(formatServerReady(ui, 'http://127.0.0.1:5173/', {
    modules: 12,
    chunks: 2,
    assets: 1,
  }), [
    '  ✓  Development server ready',
    '     Local  http://127.0.0.1:5173/',
    '     12 modules · 2 chunks · 1 assets',
    '     Press Ctrl-C to stop',
    '',
  ])
  assert.deepEqual(formatBuildResult(ui, {
    moduleCount: 12,
    durationMs: 24.4,
    outputDir: 'dist',
    files: [
      { bytes: 1024 },
      { bytes: 512 },
    ],
    diagnostics: [],
  }), [
    '  ✓  Built in 24ms',
    '     12 modules · 2 files · 1.5 KB',
    '     Output  dist',
    '',
  ])
})

test('colored output strips to the exact plain content', () => {
  const plain = formatBanner(createUi(false), 'dev', '0.1.3').join('\n')
  const colored = formatBanner(createUi(true), 'dev', '0.1.3').join('\n')
  assert.match(colored, /\x1b\[/)
  assert.equal(stripAnsi(colored), plain)
})

test('shared diagnostic contract renders an exact source code frame', () => {
  assert.deepEqual(
    formatDiagnostic(createUi(false), diagnosticContract.diagnostic),
    diagnosticContract.plainLines,
  )
  const colored = formatDiagnostic(createUi(true), diagnosticContract.diagnostic).join('\n')
  assert.equal(stripAnsi(colored), diagnosticContract.plainLines.join('\n'))
})

test('durations use the same human-readable scale as Rust', () => {
  assert.equal(humanDuration(0), '1ms')
  assert.equal(humanDuration(24), '24ms')
  assert.equal(humanDuration(3_430), '3.43s')
  assert.equal(humanDuration(63_200), '1m3.2s')
})

test('dashboard renders full, compact, and minimal states', () => {
  const state = createDashboardState({ command: 'dev', root: 'demo' })
  state.version = '0.1.3'
  setDashboardEndpoint(state, 'http://127.0.0.1:5173/')
  applyDashboardEvent(state, {
    type: 'rebuilt',
    initial: true,
    modules: 128,
    updatedModules: 128,
    cachedModules: 0,
    chunks: 3,
    assets: 4,
    durationMs: 42,
  })
  applyDashboardEvent(state, {
    type: 'workspaceState',
    total: 2,
    loaded: 1,
    failed: 0,
    current: 'rc-alpha',
    failedNames: [],
  })

  const full = renderDashboard(state, 90, 24, createUi(false))
  const compact = renderDashboard(state, 70, 18, createUi(false))
  const minimal = renderDashboard(state, 50, 10, createUi(false))
  assert.match(full, /WAKE \/ DEV/)
  assert.match(full, /128 modules · 3 chunks · 4 assets · 42ms/)
  assert.match(full, /WORKSPACES  1\/2 loaded · loading rc-alpha/)
  assert.match(full, /ACTIVITY/)
  assert.match(compact, /READY/)
  assert.match(minimal, /Resize for details/)
  assert.doesNotMatch(full, /\x1b\[/)
})

test('dashboard activity history is bounded and tracks rebuilds', () => {
  const state = createDashboardState({ command: 'dev' })
  for (let index = 0; index < 250; index += 1) {
    applyDashboardEvent(state, {
      type: 'diagnostic',
      diagnostic: { severity: 'error', message: `error ${index}` },
    })
  }
  assert.equal(state.activity.length, 200)
  applyDashboardEvent(state, {
    type: 'rebuilt',
    initial: false,
    modules: 1,
    updatedModules: 1,
    cachedModules: 0,
    chunks: 1,
    assets: 0,
    durationMs: 8,
  })
  assert.equal(state.rebuilds, 1)
  assert.match(state.activity.at(-1).message, /0 cache hits/)
})

test('plain server activity reports rebuilds and diagnostics to stderr', () => {
  const server = new EventEmitter()
  const errors = []
  const stop = observeServer(server, createUi(false), {
    error: (message) => errors.push(message),
  })

  server.emit('rebuildStart', { changedPaths: ['src/a.ts', 'src/b.ts'] })
  server.emit('rebuilt', {
    initial: false,
    modules: 12,
    updatedModules: 1,
    cachedModules: 11,
    chunks: 2,
    assets: 1,
    durationMs: 18.6,
  })
  server.emit('diagnostic', diagnosticContract.diagnostic)
  assert.deepEqual(errors, [
    '  ↻  Rebuilding after 2 file changes…',
    '  ✓  Updated  ·  1 module  ·  11 cache hits  19ms',
    `  ✗  ${diagnosticContract.plainLines[0]}`,
    ...diagnosticContract.plainLines.slice(1).map((line) => `     ${line}`),
  ])

  stop()
  server.emit('rebuilt', { initial: false, modules: 1, durationMs: 1 })
  assert.equal(errors.length, 2 + diagnosticContract.plainLines.length)
})

test('dashboard session restores raw mode and every interactive terminal mode', () => {
  class Input extends EventEmitter {
    isTTY = true
    isRaw = false
    rawChanges = []
    setRawMode(value) {
      this.isRaw = value
      this.rawChanges.push(value)
    }
    resume() {}
    pause() {}
  }
  class Output extends EventEmitter {
    isTTY = true
    columns = 80
    rows = 20
    writes = []
    write(value) {
      this.writes.push(String(value))
    }
  }
  const input = new Input()
  const output = new Output()
  const state = createDashboardState({ command: 'dev' })
  state.version = '0.1.3'
  const session = createDashboardSession(state, {
    input,
    output,
    ui: createUi(false),
  })
  session.close()

  assert.deepEqual(input.rawChanges, [true, false])
  assert.match(output.writes.join(''), /\x1b\[\?1049h/)
  assert.match(output.writes.join(''), /\x1b\[\?1002h/)
  assert.match(output.writes.join(''), /\x1b\[\?1006h/)
  assert.match(output.writes.join(''), /\x1b\[\?2004h/)
  assert.match(output.writes.join(''), /\x1b\[\?25l/)
  assert.match(output.writes.join(''), /\x1b\[\?2004l/)
  assert.match(output.writes.join(''), /\x1b\[\?1006l/)
  assert.match(output.writes.join(''), /\x1b\[\?1002l/)
  assert.match(output.writes.join(''), /\x1b\[\?25h/)
  assert.match(output.writes.join(''), /\x1b\[\?1049l/)
})

test('dashboard session accepts commands, mouse copy, and clipboard paste', async () => {
  class Input extends EventEmitter {
    isTTY = true
    isRaw = false
    setRawMode(value) { this.isRaw = value }
    resume() {}
    pause() {}
  }
  class Output extends EventEmitter {
    isTTY = true
    columns = 80
    rows = 20
    writes = []
    write(value) { this.writes.push(String(value)) }
  }
  const input = new Input()
  const output = new Output()
  const copied = []
  const opened = []
  const state = createDashboardState({ command: 'dev' })
  state.version = '0.1.3'
  setDashboardEndpoint(state, 'http://127.0.0.1:5173/')
  const session = createDashboardSession(state, {
    input,
    output,
    ui: createUi(false),
    clipboardAdapter: {
      async write(value) { copied.push(value) },
      async read() { return '/help\n' },
    },
    async openUrl(value) { opened.push(value) },
  })

  input.emit('data', Buffer.from('open\r'))
  await new Promise((resolve) => setImmediate(resolve))
  assert.deepEqual(opened, ['http://127.0.0.1:5173/'])

  input.emit('data', Buffer.from('\x1b[<0;2;1M\x1b[<0;9;1m'))
  await new Promise((resolve) => setImmediate(resolve))
  assert.ok(copied.at(-1)?.includes('WAKE'), copied)

  input.emit('data', Buffer.from('\x1b[<2;5;19M\r'))
  await new Promise((resolve) => setImmediate(resolve))
  assert.match(state.activity.at(-1).message, /Commands:/)
  session.close()
})
