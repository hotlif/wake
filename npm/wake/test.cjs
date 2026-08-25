'use strict'

function contextError() {
  const error = new Error('@crab-dev/wake/test can only be used inside wake test')
  error.name = 'WakeError'
  error.code = 'WAKE_TEST_CONTEXT'
  return error
}

function unavailable() {
  throw contextError()
}

function callable(properties) {
  const entry = (...args) => unavailable(...args)
  return Object.freeze(Object.assign(entry, properties))
}

function testApi() {
  const entry = (...args) => unavailable(...args)
  entry.only = entry
  entry.skip = entry
  entry.todo = unavailable
  entry.each = unavailable
  return Object.freeze(entry)
}

function describeApi() {
  const entry = (...args) => unavailable(...args)
  entry.only = entry
  entry.skip = entry
  entry.each = unavailable
  return Object.freeze(entry)
}

const test = testApi()
const describe = describeApi()

const expect = callable({
  extend: unavailable,
  addEqualityTesters: unavailable,
  addSnapshotSerializer: unavailable,
  assertions: unavailable,
  hasAssertions: unavailable,
  getState: unavailable,
  setState: unavailable,
  any: unavailable,
  anything: unavailable,
  arrayContaining: unavailable,
  objectContaining: unavailable,
  stringContaining: unavailable,
  stringMatching: unavailable,
  closeTo: unavailable,
})

const mock = Object.freeze({
  fn: unavailable,
  spyOn: unavailable,
  replaceProperty: unavailable,
  module: unavailable,
  import: unavailable,
  actual: unavailable,
  isolate: unavailable,
  clearAll: unavailable,
  resetAll: unavailable,
  restoreAll: unavailable,
})

const clock = Object.freeze({
  fake: unavailable,
  restore: unavailable,
  advanceBy: unavailable,
  advanceTo: unavailable,
  runNext: unavailable,
  runAll: unavailable,
  flushMicrotasks: unavailable,
})

const network = Object.freeze({
  route: unavailable,
  allow: unavailable,
  requests: unavailable,
  reset: unavailable,
})

module.exports = Object.freeze({
  afterAll: unavailable,
  afterEach: unavailable,
  beforeAll: unavailable,
  beforeEach: unavailable,
  clock,
  describe,
  expect,
  it: test,
  mock,
  network,
  test,
})
