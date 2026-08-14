import { execFileSync } from 'node:child_process'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'

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

const npmCommand = process.platform === 'win32' ? process.execPath : 'npm'
const npmPrefix = process.platform === 'win32'
  ? [process.env.npm_execpath || (() => { throw new Error('npm_execpath is required on Windows') })()]
  : []

for (const { directory } of manifests) {
  const output = execFileSync(
    npmCommand,
    [...npmPrefix, 'pack', '--dry-run', '--ignore-scripts', '--json'],
    {
      cwd: resolve(root, directory),
      encoding: 'utf8',
    },
  )
  const [pack] = JSON.parse(output)
  const files = pack.files.map((file) => file.path)
  const nativeFiles = files.filter((file) => file.endsWith('.node'))
  const isWake = directory === wakePackageDir
  const isCss = directory === cssPackageDir
  const isJavaScript = isWake || isCss
  if (isJavaScript && nativeFiles.length !== 0) {
    throw new Error(`${directory} must not contain native binaries`)
  }
  if (isWake) {
    for (const required of [
      'internal/components-runtime.mjs',
      'internal/components-runtime.d.ts',
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
  const limit = isCss
    ? 128 * 1024
    : isWake
      ? 500 * 1024
      : 15 * 1024 * 1024
  if (pack.size > limit) {
    throw new Error(`${directory} packed size ${pack.size} exceeds ${limit}`)
  }
  for (const required of ['LICENSE-MIT', 'LICENSE-APACHE', 'README.md']) {
    if (!files.includes(required)) {
      throw new Error(`${directory} is missing ${required}`)
    }
  }
  if (files.some((file) => file.startsWith('target/') || file.endsWith('.rs'))) {
    throw new Error(`${directory} contains Rust build or source files`)
  }
  console.log(`${directory}: ${pack.size} bytes, ${files.length} files`)
}
