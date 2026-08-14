import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
import { readFile } from 'node:fs/promises'
import test from 'node:test'

import * as esm from '../index.mjs'

const require = createRequire(import.meta.url)
const cjs = require('../index.cjs')

const publicExports = [
  'assignVars',
  'createVar',
  'css',
  'cx',
  'globalStyle',
  'keyframes',
]

test('ESM and CommonJS expose the same public API', () => {
  assert.deepEqual(Object.keys(esm).sort(), publicExports)
  assert.deepEqual(Object.keys(cjs).sort(), publicExports)
})

test('compile-time tags fail clearly when Wake did not transform them', () => {
  for (const implementation of [esm, cjs]) {
    for (const name of ['css', 'keyframes', 'globalStyle']) {
      assert.throws(
        () => implementation[name]`color: red;`,
        (error) => {
          assert.equal(error.code, 'ERR_WAKE_CSS_NOT_COMPILED')
          assert.match(error.message, new RegExp(`@crab-dev/css: ${name}`))
          assert.match(error.message, /compile-time template tag/)
          assert.match(error.message, /Build this module with Wake/)
          return true
        },
      )
    }
  }
})

test('cx flattens nested arrays and conditional objects in source order', () => {
  assert.equal(
    esm.cx(
      'button',
      false,
      null,
      undefined,
      0,
      ['primary', ['', ['compact', { disabled: false, selected: 1 }]]],
      { loading: true, hidden: 0 },
      '  with-spacing  ',
    ),
    'button primary compact selected loading with-spacing',
  )
  assert.equal(cjs.cx(), '')
})

test('createVar is realm-unique across ESM and CommonJS and sanitizes debug labels', () => {
  const first = esm.createVar('accent color')
  const second = cjs.createVar('accent color')
  const fallback = esm.createVar('!!!')

  assert.match(first, /^var\(--crab-css-accent-color-[0-9a-z]+\)$/)
  assert.match(second, /^var\(--crab-css-accent-color-[0-9a-z]+\)$/)
  assert.match(fallback, /^var\(--crab-css-var-[0-9a-z]+\)$/)
  assert.notEqual(first, second)
  assert.notEqual(second, fallback)
  assert.throws(() => esm.createVar(42), /debugName must be a string/)
})

test('assignVars creates a fresh custom-property style object', () => {
  const color = esm.createVar('color')
  const spacing = esm.createVar('spacing')
  const input = { [color]: 'rebeccapurple', [spacing]: 8 }
  const styles = esm.assignVars(input)

  assert.deepEqual(styles, {
    [color.slice(4, -1)]: 'rebeccapurple',
    [spacing.slice(4, -1)]: 8,
  })
  assert.notEqual(styles, input)
  assert.throws(() => esm.assignVars(null), /expects a CSS variable map/)
  assert.throws(() => esm.assignVars({ '--raw': 'red' }), /not a CSS variable reference/)
  assert.throws(() => esm.assignVars({ 'var(--bad)': Infinity }), /string or finite number/)
  assert.throws(() => esm.assignVars({ 'var(--bad)': true }), /string or finite number/)
})

test('package metadata has no runtime dependencies and publishes only artifacts', async () => {
  const packageJson = JSON.parse(
    await readFile(new URL('../package.json', import.meta.url), 'utf8'),
  )

  assert.equal(packageJson.name, '@crab-dev/css')
  assert.equal(packageJson.version, '0.1.16')
  assert.equal(packageJson.sideEffects, false)
  assert.equal(packageJson.dependencies, undefined)
  assert.deepEqual(packageJson.files, [
    'index.cjs',
    'index.mjs',
    'index.d.ts',
    'README.md',
    'LICENSE-MIT',
    'LICENSE-APACHE',
  ])
})
