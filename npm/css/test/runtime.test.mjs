import assert from 'node:assert/strict'
import { createRequire } from 'node:module'
import { readFile } from 'node:fs/promises'
import { test } from '@crab-dev/wake/test'

import * as esm from '../index.mjs'

const require = createRequire(import.meta.url)
const cjs = require('../index.cjs')

function errorMessageIncludes(fragment) {
  return (error) => error instanceof Error && error.message.includes(fragment)
}

function isGeneratedVar(value, label) {
  const prefix = `var(--crab-css-${label}-`
  if (!value.startsWith(prefix) || !value.endsWith(')')) return false
  const id = value.slice(prefix.length, -1)
  return id.length > 0 && [...id].every((character) => {
    const code = character.codePointAt(0)
    return (code >= 48 && code <= 57) || (code >= 97 && code <= 122)
  })
}

const publicExports = [
  'assignVars',
  'createVar',
  'css',
  'cx',
  'defineTokens',
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
          assert.ok(error.message.includes(`@crab-dev/css: ${name}`))
          assert.ok(error.message.includes('compile-time template tag'))
          assert.ok(error.message.includes('Build this module with Wake'))
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

test('defineTokens deeply freezes pure structures without invoking accessors', () => {
  for (const implementation of [esm, cjs]) {
    const tokens = implementation.defineTokens({
      color: 'red',
      nested: { gap: 8 },
      steps: ['sm', { value: null }],
    })
    assert.ok(Object.isFrozen(tokens))
    assert.ok(Object.isFrozen(tokens.nested))
    assert.ok(Object.isFrozen(tokens.steps))
    assert.ok(Object.isFrozen(tokens.steps[1]))
    assert.throws(() => {
      tokens.nested.gap = 12
    }, TypeError)
  }

  let getterCalls = 0
  const accessor = Object.defineProperty({}, 'color', {
    get() {
      getterCalls += 1
      return 'red'
    },
  })
  assert.throws(() => esm.defineTokens(accessor), errorMessageIncludes('does not accept accessors'))
  assert.equal(getterCalls, 0)
  for (const invalid of [null, new Date(), { value: Infinity }, { value: () => 'red' }]) {
    assert.throws(() => esm.defineTokens(invalid), TypeError)
  }
})

test('createVar is realm-unique across ESM and CommonJS and sanitizes debug labels', () => {
  const first = esm.createVar('accent color')
  const second = cjs.createVar('accent color')
  const fallback = esm.createVar('!!!')

  assert.ok(isGeneratedVar(first, 'accent-color'))
  assert.ok(isGeneratedVar(second, 'accent-color'))
  assert.ok(isGeneratedVar(fallback, 'var'))
  assert.notEqual(first, second)
  assert.notEqual(second, fallback)
  assert.throws(() => esm.createVar(42), errorMessageIncludes('debugName must be a string'))
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
  assert.throws(() => esm.assignVars(null), errorMessageIncludes('expects a CSS variable map'))
  assert.throws(
    () => esm.assignVars({ '--raw': 'red' }),
    errorMessageIncludes('not a CSS variable reference'),
  )
  assert.throws(
    () => esm.assignVars({ 'var(--bad)': Infinity }),
    errorMessageIncludes('string or finite number'),
  )
  assert.throws(
    () => esm.assignVars({ 'var(--bad)': true }),
    errorMessageIncludes('string or finite number'),
  )
  for (const invalid of ['var(--bad value)', 'var(--bad())', 'var(--)', 'var( --bad)']) {
    assert.throws(
      () => esm.assignVars({ [invalid]: 'red' }),
      errorMessageIncludes('not a CSS variable reference'),
    )
  }
})

test('package metadata has no runtime dependencies and publishes only artifacts', async () => {
  const packageJson = JSON.parse(
    await readFile(new URL('../package.json', import.meta.url), 'utf8'),
  )

  assert.equal(packageJson.name, '@crab-dev/css')
  assert.equal(packageJson.version, '0.1.24')
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
