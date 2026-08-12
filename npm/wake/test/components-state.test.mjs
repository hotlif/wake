import assert from 'node:assert/strict'
import test from 'node:test'
import {
  applyLocationArgs,
  locationHash,
  readLocationHash,
} from '../../../crates/wake_docs/runtime/components-state.mjs'

test('round-trips changed and explicitly unset component args', () => {
  const defaults = {
    disabled: false,
    label: '保存更改',
    size: 'middle',
  }
  const args = {
    label: '提交',
    size: 'large',
  }

  const hash = locationHash('docs/components/demos/basic.demo.tsx', args, defaults, 'mobile')
  const location = readLocationHash(hash)

  assert.equal(location.id, 'docs/components/demos/basic.demo.tsx')
  assert.equal(location.viewport, 'mobile')
  assert.deepEqual(location.args, { label: '提交', size: 'large' })
  assert.deepEqual(location.unset, ['disabled'])
  assert.deepEqual(applyLocationArgs(defaults, location), args)
})

test('omits unchanged defaults and tolerates malformed URL state', () => {
  const defaults = { disabled: false, size: 'middle' }

  assert.equal(
    locationHash('basic.demo.tsx', defaults, defaults, 'responsive'),
    '#/components/basic.demo.tsx',
  )
  assert.deepEqual(readLocationHash('#/components/%E0%A4%A'), {
    args: {},
    unset: [],
    viewport: 'responsive',
  })

  const malformed = readLocationHash('#/components/basic.demo.tsx?args=%7B&unset=%5B')
  assert.deepEqual(malformed.args, {})
  assert.deepEqual(malformed.unset, [])
})
