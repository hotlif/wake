import test from 'node:test'
import assert from 'node:assert/strict'

import {
  PROTOCOL_VERSION,
  definePlugin,
  defineRule,
  runRule,
  assertCompatible,
} from '../index.mjs'

test('extension rules receive frozen UTF-8 source facts and stable diagnostics', async () => {
  let frozen = false
  const rule = defineRule({
    id: 'demo/no-debugger',
    meta: { description: 'find debugger statements', category: 'problem', fixable: false },
    create(context) {
      frozen = Object.isFrozen(context) && Object.isFrozen(context.syntax)
      const start = context.source.indexOf('debugger')
      if (start >= 0) context.report({ message: 'debugger is not allowed', start: 0, end: Buffer.byteLength('🙂') })
    },
  })
  const result = await runRule(rule, {
    path: 'src/example.js',
    language: 'js',
    source: '🙂; debugger;',
    syntax: { tokens: [{ kind: 'identifier', start: 4, end: 12 }] },
  })
  assert.equal(result.protocol, PROTOCOL_VERSION)
  assert.equal(frozen, true)
  assert.deepEqual(result.diagnostics, [{
    message: 'debugger is not allowed',
    start: 0,
    end: 4,
  }])
})

test('rule failures are isolated and invalid edits are rejected', async () => {
  const bad = defineRule({
    id: 'demo/bad',
    meta: { description: 'throws', category: 'problem', fixable: true },
    create() { throw new Error('boom') },
  })
  const failed = await runRule(bad, { path: 'x.js', language: 'js', source: 'x' })
  assert.equal(failed.error.code, 'WAKE_LINT_EXTENSION')

  const invalid = defineRule({
    id: 'demo/invalid',
    meta: { description: 'bad edit', category: 'suggestion', fixable: true },
    create(context) { context.report({ message: 'bad', start: 1, end: 2, fix: { edits: [{ start: 1, end: 1, text: 'x' }, { start: 0, end: 1, text: 'y' }] } }) },
  })
  const rejected = await runRule(invalid, { path: 'x.js', language: 'js', source: '🙂' })
  assert.equal(rejected.error.code, 'WAKE_LINT_EXTENSION')
})

test('plugin loading validates names and upgrade compatibility', () => {
  const plugin = definePlugin({
    name: '@demo/wake-lint-plugin',
    version: '1.2.0',
    rules: [defineRule({
      id: 'demo/example',
      meta: { description: 'example', category: 'layout', fixable: false },
      create() {},
    })],
  })
  assert.equal(plugin.protocol, PROTOCOL_VERSION)
  assert.doesNotThrow(() => assertCompatible(plugin, { sdkMajor: 1 }))
  assert.throws(() => assertCompatible(plugin, { sdkMajor: 2 }), /SDK major/)
})
