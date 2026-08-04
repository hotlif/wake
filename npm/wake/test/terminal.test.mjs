import assert from 'node:assert/strict'
import { EventEmitter } from 'node:events'
import { test } from 'node:test'
import {
  createUi,
  formatBanner,
  formatServerReady,
  humanDuration,
  observeServer,
  supportsColor,
} from '../bin/terminal.mjs'

test('terminal colors follow tty and NO_COLOR', () => {
  assert.equal(supportsColor({ isTTY: true }, {}), true)
  assert.equal(supportsColor({ isTTY: false }, {}), false)
  assert.equal(supportsColor({ isTTY: true }, { NO_COLOR: '' }), false)
})

test('plain startup panel stays aligned with the Rust CLI', () => {
  const ui = createUi(false)
  assert.deepEqual(formatBanner(ui, 'dev', '0.1.3'), [
    '',
    '  ⚡ wake v0.1.3  dev',
    '',
  ])
  assert.deepEqual(formatServerReady(ui, 'http://127.0.0.1:5173/', 24.4), [
    '  ✓  开发服务器已就绪  ·  24ms',
    '',
    '    Local http://127.0.0.1:5173/',
    '    提示 按 Ctrl-C 退出',
    '',
  ])
})

test('colored UI applies ANSI styles', () => {
  const banner = formatBanner(createUi(true), 'docs dev', '0.1.3').join('\n')
  assert.match(banner, /\x1b\[/)
  assert.match(banner, /wake/)
})

test('durations use the same human-readable scale as the Rust CLI', () => {
  assert.equal(humanDuration(0), '1ms')
  assert.equal(humanDuration(24), '24ms')
  assert.equal(humanDuration(3_430), '3.43s')
  assert.equal(humanDuration(63_200), '1m3.2s')
})

test('server activity is reported and observers can be detached', () => {
  const server = new EventEmitter()
  const logs = []
  const errors = []
  const stop = observeServer(server, createUi(false), {
    log: (message) => logs.push(message),
    error: (message) => errors.push(message),
  })

  server.emit('rebuildStart', { changedPaths: ['src/a.ts', 'src/b.ts'] })
  server.emit('rebuilt', { modules: 12, durationMs: 18.6 })
  server.emit('diagnostic', {
    code: 'WAKE_BUILD',
    message: 'Unexpected token',
  })
  assert.deepEqual(logs, [
    '  ↻  检测到 2 个文件变更，正在重建…',
    '  ✓  热重建  ·  12 模块  ·  19ms',
  ])
  assert.deepEqual(errors, [
    '  ✗  构建失败  [WAKE_BUILD] Unexpected token',
  ])

  stop()
  server.emit('rebuilt', { modules: 1, durationMs: 1 })
  assert.equal(logs.length, 2)
})
