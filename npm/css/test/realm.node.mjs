import assert from 'node:assert/strict'
import test from 'node:test'
import vm from 'node:vm'

import { defineTokens } from '../index.mjs'

test('defineTokens accepts plain structures created in another JavaScript realm', () => {
  const tokens = vm.runInNewContext("({ color: { accent: 'rebeccapurple' }, gap: [4, 8] })")
  const frozen = defineTokens(tokens)
  assert.equal(frozen.color.accent, 'rebeccapurple')
  assert.equal(Object.isFrozen(frozen), true)
  assert.equal(Object.isFrozen(frozen.color), true)
  assert.equal(Object.isFrozen(frozen.gap), true)
})
