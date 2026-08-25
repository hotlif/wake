'use strict'

const contexts = new WeakMap()

function registerTestContext(context, handle) {
  contexts.set(context, { handle, fatalError: undefined })
}

function sendTestWatchControl(context, control) {
  const record = contexts.get(context)
  if (!record) throw new TypeError('Expected a Wake TestContext')
  record.handle.watchControl(JSON.stringify(control))
}

function setTestContextFatalError(context, error) {
  const record = contexts.get(context)
  if (!record) throw new TypeError('Expected a Wake TestContext')
  record.fatalError = error
}

function getTestContextFatalError(context) {
  return contexts.get(context)?.fatalError
}

module.exports = {
  getTestContextFatalError,
  registerTestContext,
  sendTestWatchControl,
  setTestContextFatalError,
}
