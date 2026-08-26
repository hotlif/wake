import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'

import {
  PLATFORM_PACKED_LIMIT,
  PLATFORM_PACKED_WARNING,
  PLATFORM_UNPACKED_LIMIT,
  expectedPlatformFiles,
  platformContract,
} from './native-package-contract.mjs'
import { verifyNativePackage } from './verify-native-package.mjs'

const root = resolve(import.meta.dirname, '..')
const wakePackageDir = 'npm/wake'
const cssPackageDir = 'npm/css'
const allPackageDirs = [
  cssPackageDir,
  wakePackageDir,
  'npm/wake-win32-x64-msvc',
  'npm/wake-linux-x64-gnu',
  'npm/wake-linux-arm64-gnu',
  'npm/wake-darwin-x64',
  'npm/wake-darwin-arm64',
]
const packageDirs = process.env.WAKE_PACK_TARGETS
  ? process.env.WAKE_PACK_TARGETS.split(',').map((value) => value.trim())
  : allPackageDirs
const rootManifest = JSON.parse(
  readFileSync(resolve(root, wakePackageDir, 'package.json'), 'utf8'),
)

const manifests = packageDirs.map((directory) => {
  const path = resolve(root, directory, 'package.json')
  return {
    directory,
    value: JSON.parse(readFileSync(path, 'utf8')),
  }
})

const version = rootManifest.version
for (const { directory, value } of manifests) {
  if (value.version !== version) {
    throw new Error(`${directory} version ${value.version} does not match ${version}`)
  }
  if (value.license !== 'MIT OR Apache-2.0') {
    throw new Error(`${directory} must use the workspace dual license`)
  }
}

for (const [name, dependencyVersion] of Object.entries(
  rootManifest.optionalDependencies,
)) {
  if (dependencyVersion !== version) {
    throw new Error(`${name} must be pinned to ${version}`)
  }
}

// `npm pack` is intentionally a publication-consumer compatibility check. When this script is
// launched by Yarn, npm_execpath points at Yarn and must not be reused as though it were npm.
const npmCommand = process.platform === 'win32' ? 'npm.cmd' : 'npm'
const npmPrefix = []

for (const { directory, value: packageManifest } of manifests) {
  const isWake = directory === wakePackageDir
  const isCss = directory === cssPackageDir
  const isJavaScript = isWake || isCss
  const nativeVerification = isJavaScript
    ? undefined
    : verifyNativePackage(resolve(root, directory))
  const output = execFileSync(
    npmCommand,
    [...npmPrefix, 'pack', '--dry-run', '--ignore-scripts', '--json'],
    {
      cwd: resolve(root, directory),
      encoding: 'utf8',
      shell: process.platform === 'win32',
    },
  )
  const [pack] = JSON.parse(output)
  const files = pack.files.map((file) => file.path)
  const nativeFiles = files.filter((file) => file.endsWith('.node'))
  if (isJavaScript && nativeFiles.length !== 0) {
    throw new Error(`${directory} must not contain native binaries`)
  }
  if (isWake) {
    for (const required of [
      'internal/components-runtime.mjs',
      'internal/components-runtime.d.ts',
      'test.cjs',
      'test.mjs',
      'test.d.ts',
    ]) {
      if (!files.includes(required)) {
        throw new Error(`The main package is missing ${required}`)
      }
    }
  }
  if (isCss) {
    const expected = [
      'LICENSE-APACHE',
      'LICENSE-MIT',
      'README.md',
      'index.cjs',
      'index.d.ts',
      'index.mjs',
      'package.json',
    ]
    if (JSON.stringify(files.slice().sort()) !== JSON.stringify(expected)) {
      throw new Error(`${directory} must contain exactly: ${expected.join(', ')}`)
    }
  }
  if (!isJavaScript && nativeFiles.length !== 1) {
    throw new Error(`${directory} must contain exactly one native binary`)
  }
  if (!isJavaScript) {
    const contract = platformContract(packageManifest.name)
    const expected = expectedPlatformFiles(packageManifest, contract)
    const actual = files.slice().sort()
    if (JSON.stringify(actual) !== JSON.stringify(expected)) {
      throw new Error(`${directory} must contain exactly: ${expected.join(', ')}`)
    }
    if (!files.includes(contract.hostPath)) {
      throw new Error(`${directory} is missing ${contract.hostPath}`)
    }
    const host = pack.files.find((file) => file.path === contract.hostPath)
    if (!contract.hostPath.endsWith('.exe') && (host.mode & 0o111) === 0) {
      throw new Error(`${directory} test host must be executable`)
    }
  }
  const limit = isCss
    ? 128 * 1024
    : isWake
      ? 500 * 1024
      : PLATFORM_PACKED_LIMIT
  if (pack.size > limit) {
    throw new Error(`${directory} packed size ${pack.size} exceeds ${limit}`)
  }
  if (!isJavaScript) {
    if (!Number.isSafeInteger(pack.unpackedSize) || pack.unpackedSize < 0) {
      throw new Error(`${directory} did not report a valid unpacked size`)
    }
    const measuredUnpackedSize = pack.files.reduce(
      (total, file) => total + file.size,
      0,
    )
    if (pack.unpackedSize !== measuredUnpackedSize) {
      throw new Error(
        `${directory} unpacked size ${pack.unpackedSize} does not match ${measuredUnpackedSize}`,
      )
    }
    if (pack.unpackedSize > PLATFORM_UNPACKED_LIMIT) {
      throw new Error(
        `${directory} unpacked size ${pack.unpackedSize} exceeds ${PLATFORM_UNPACKED_LIMIT}`,
      )
    }
    if (pack.size > PLATFORM_PACKED_WARNING) {
      console.warn(
        `WARNING: ${directory} packed size ${pack.size} exceeds the ${PLATFORM_PACKED_WARNING} warning threshold`,
      )
    }
  }
  for (const required of ['LICENSE-MIT', 'LICENSE-APACHE', 'README.md']) {
    if (!files.includes(required)) {
      throw new Error(`${directory} is missing ${required}`)
    }
  }
  if (files.some((file) => file.startsWith('target/') || file.endsWith('.rs'))) {
    throw new Error(`${directory} contains Rust build or source files`)
  }
  const unpacked = isJavaScript ? '' : `, ${pack.unpackedSize} bytes unpacked`
  const build = nativeVerification ? `, ${nativeVerification.buildId}` : ''
  console.log(
    `${directory}: ${pack.size} bytes packed${unpacked}, ${files.length} files${build}`,
  )
}
