'use strict'

const path = require('node:path')

const targets = {
  'win32-x64': '@crab-dev/wake-win32-x64-msvc',
  'linux-x64': '@crab-dev/wake-linux-x64-gnu',
  'linux-arm64': '@crab-dev/wake-linux-arm64-gnu',
  'darwin-x64': '@crab-dev/wake-darwin-x64',
  'darwin-arm64': '@crab-dev/wake-darwin-arm64',
}

function unsupported(message, cause) {
  const error = new Error(message, cause ? { cause } : undefined)
  error.name = 'WakeError'
  error.code = 'WAKE_UNSUPPORTED_PLATFORM'
  return error
}

function attachTestHost(native, nativePath) {
  const hostPath = process.env.WAKE_TEST_HOST_PATH || path.join(
    path.dirname(nativePath),
    'test-host',
    process.platform === 'win32' ? 'wake-test-host.exe' : 'wake-test-host',
  )
  Object.defineProperty(native, '__wakeTestHostPath', {
    configurable: false,
    enumerable: false,
    writable: false,
    value: hostPath,
  })
  return native
}

function loadNative() {
  if (process.env.WAKE_NATIVE_PATH) {
    const nativePath = path.resolve(process.env.WAKE_NATIVE_PATH)
    return attachTestHost(require(nativePath), nativePath)
  }

  const key = `${process.platform}-${process.arch}`
  if (
    process.platform === 'linux' &&
    !process.report?.getReport?.().header?.glibcVersionRuntime
  ) {
    throw unsupported(
      `Wake does not provide a native binary for ${process.platform}/${process.arch} with musl. ` +
      'Install on a glibc 2.28 or newer system.',
    )
  }
  const packageName = targets[key]
  if (!packageName) {
    throw unsupported(
      `Wake does not provide a native binary for ${process.platform}/${process.arch}. ` +
      'Supported targets: Windows x64, Linux glibc x64/arm64, and macOS x64/arm64.',
    )
  }

  try {
    const nativePath = require.resolve(packageName)
    return attachTestHost(require(nativePath), nativePath)
  } catch (cause) {
    throw unsupported(
      `Unable to load ${packageName} for ${process.platform}/${process.arch}. ` +
      `Optional dependencies may have been omitted; ` +
      'reinstall @crab-dev/wake without --omit=optional and verify that your platform is supported.',
      cause,
    )
  }
}

module.exports = { loadNative }
