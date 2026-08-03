'use strict'

const { loadNative } = require('./loader.cjs')
const { WakeError, fromNativeError } = require('./errors.cjs')

const native = loadNative()

class ParsedModule {
  #native

  constructor(handle) {
    this.#native = handle
    Object.defineProperty(this, '__wakeOpaqueHandle', {
      configurable: false,
      enumerable: true,
      writable: false,
      value: () => {},
    })
  }

  get disposed() {
    return this.#native.disposed
  }

  get summary() {
    this.#assertOpen()
    return JSON.parse(this.#native.summaryJson())
  }

  dispose() {
    this.#native.dispose()
  }

  _nativeHandle() {
    this.#assertOpen()
    return this.#native
  }

  #assertOpen() {
    if (this.disposed) {
      throw new WakeError('WAKE_INTERNAL', 'ParsedModule has already been disposed')
    }
  }

  [Symbol.dispose]() {
    this.dispose()
  }
}

function wrapNative(operation) {
  try {
    return operation()
  } catch (error) {
    throw fromNativeError(error)
  }
}

function tokenize(source, options = {}) {
  void options
  return wrapNative(() => JSON.parse(native.tokenize(String(source))))
}

function parse(source, options = {}) {
  const sourceType = options.sourceType || 'module'
  return wrapNative(() => new ParsedModule(native.parse(String(source), sourceType)))
}

function moduleFrom(value, options) {
  return typeof value === 'string' ? parse(value, options) : value
}

function transform(sourceOrModule, options) {
  const owned = typeof sourceOrModule === 'string'
  const module = moduleFrom(sourceOrModule, options)
  if (!(module instanceof ParsedModule)) {
    throw new TypeError('transform() expects source text or a ParsedModule')
  }
  try {
    return wrapNative(() => JSON.parse(module._nativeHandle().transformJson()))
  } finally {
    if (owned) module.dispose()
  }
}

function analyze(sourceOrModule, options) {
  const owned = typeof sourceOrModule === 'string'
  const module = moduleFrom(sourceOrModule, options)
  if (!(module instanceof ParsedModule)) {
    throw new TypeError('analyze() expects source text or a ParsedModule')
  }
  try {
    return wrapNative(() => JSON.parse(module._nativeHandle().analyzeJson()))
  } finally {
    if (owned) module.dispose()
  }
}

module.exports = { ParsedModule, analyze, parse, tokenize, transform }
