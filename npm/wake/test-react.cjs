'use strict'

const core = require('./test.cjs')

function contextError() {
  const error = new Error('@crab-dev/wake/test/react can only be used inside wake test')
  error.name = 'WakeError'
  error.code = 'WAKE_TEST_CONTEXT'
  return error
}

function unavailable() {
  throw contextError()
}

const screenEntries = { debug: unavailable }
for (const family of [
  'Role',
  'Text',
  'LabelText',
  'DisplayValue',
  'PlaceholderText',
  'AltText',
  'Title',
  'TestId',
]) {
  for (const prefix of ['getBy', 'getAllBy', 'queryBy', 'queryAllBy', 'findBy', 'findAllBy']) {
    screenEntries[`${prefix}${family}`] = unavailable
  }
}
const screen = Object.freeze(screenEntries)

const userEvent = Object.freeze({ setup: unavailable })

const fireEvent = Object.freeze(Object.assign(
  (...args) => unavailable(...args),
  {
    change: unavailable,
    click: unavailable,
    input: unavailable,
    keyDown: unavailable,
    keyUp: unavailable,
    submit: unavailable,
  },
))

module.exports = Object.freeze({
  ...core,
  act: unavailable,
  cleanup: unavailable,
  fireEvent,
  prettyDOM: unavailable,
  render: unavailable,
  renderHook: unavailable,
  screen,
  userEvent,
  waitFor: unavailable,
  waitForElementToBeRemoved: unavailable,
  within: unavailable,
})
