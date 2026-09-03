import { accessSync } from 'node:fs'
import { fileURLToPath } from 'node:url'

const suffixes = Object.freeze({
  'win32-x64': 'win32-x64-msvc',
  'linux-x64': 'linux-x64-gnu',
  'linux-arm64': 'linux-arm64-gnu',
  'darwin-x64': 'darwin-x64',
  'darwin-arm64': 'darwin-arm64',
})

const suffix = suffixes[`${process.platform}-${process.arch}`]
if (!suffix) {
  throw new Error(
    `Wake's native addon gate does not support ${process.platform}/${process.arch}`,
  )
}

const nativePath = fileURLToPath(new URL(`../wake.${suffix}.node`, import.meta.url))
accessSync(nativePath)

// The workspace optional platform package can contain a previously staged release artifact.
// Addon conformance must exercise the binding produced by the immediately preceding native:build.
process.env.WAKE_NATIVE_PATH = nativePath
