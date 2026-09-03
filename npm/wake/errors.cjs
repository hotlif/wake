'use strict'

class WakeError extends Error {
  constructor(code, message, options = {}) {
    super(message, options.cause !== undefined ? { cause: options.cause } : undefined)
    this.name = 'WakeError'
    this.code = code
    if (options.path !== undefined) this.path = options.path
    if (options.diagnostics !== undefined) this.diagnostics = options.diagnostics
  }
}

function wakeError(value, cause) {
  if (value instanceof WakeError) return value
  const details = value && typeof value === 'object' ? value : {}
  return new WakeError(
    details.code || 'WAKE_INTERNAL',
    details.message || String(value || 'Unknown Wake error'),
    {
      path: details.path,
      diagnostics: details.diagnostics,
      cause,
    },
  )
}

function fromNativeError(error) {
  if (error && (error.name === 'AbortError' || error.code === 'ABORT_ERR')) {
    return new WakeError('WAKE_CANCELLED', 'Wake operation was cancelled', { cause: error })
  }
  if (error && error.code === 'WAKE_UNSUPPORTED_PLATFORM') {
    return wakeError(error, error.cause)
  }
  const message = String(error && error.message ? error.message : error)
  const marker = 'WAKE_ERROR_JSON:'
  const index = message.indexOf(marker)
  if (index !== -1) {
    try {
      return wakeError(JSON.parse(message.slice(index + marker.length)), error)
    } catch {
      // Fall through to the stable internal error.
    }
  }
  return new WakeError('WAKE_INTERNAL', message, { cause: error })
}

function decodeEnvelope(text) {
  let envelope
  try {
    envelope = JSON.parse(text)
  } catch (cause) {
    throw new WakeError('WAKE_INTERNAL', 'Wake returned an invalid native response', { cause })
  }
  if (!envelope.ok) throw wakeError(envelope.error)
  return envelope.value
}

async function invoke(promise) {
  try {
    return decodeEnvelope(await promise)
  } catch (error) {
    if (error instanceof WakeError) throw error
    throw fromNativeError(error)
  }
}

function splitOptions(options) {
  if (options === undefined) return [{}, undefined]
  if (options === null || typeof options !== 'object' || Array.isArray(options)) {
    throw new WakeError('WAKE_CONFIG', 'Wake options must be an object')
  }
  const { signal, ...nativeOptions } = options
  if (signal === null) {
    throw new WakeError(
      'WAKE_CONFIG',
      'explicit null is not allowed in a Node request at /signal; omit the field to use its default',
    )
  }
  if (signal?.aborted) {
    throw new WakeError('WAKE_CANCELLED', 'Wake operation was cancelled')
  }
  return [nativeOptions, signal]
}

module.exports = {
  WakeError,
  decodeEnvelope,
  fromNativeError,
  invoke,
  splitOptions,
  wakeError,
}
