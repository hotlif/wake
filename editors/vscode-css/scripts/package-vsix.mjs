import { execFileSync } from 'node:child_process'
import { chmodSync, copyFileSync, mkdirSync, readFileSync, rmSync } from 'node:fs'
import { createRequire } from 'node:module'
import { basename, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { inspectVsix } from './check-vsix.mjs'

const extensionRoot = resolve(fileURLToPath(new URL('..', import.meta.url)))
const extensionManifest = JSON.parse(
  readFileSync(join(extensionRoot, 'package.json'), 'utf8'),
)
const supportedTargets = new Set([
  'win32-x64',
  'linux-x64',
  'linux-arm64',
  'darwin-x64',
  'darwin-arm64',
])
const args = new Map()
for (let index = 2; index < process.argv.length; index += 2) {
  args.set(process.argv[index], process.argv[index + 1])
}
const target = args.get('--target')
const binary = args.get('--binary')
const output = resolve(args.get('--out') ?? join(extensionRoot, 'artifacts'))
if (!target || !supportedTargets.has(target) || !binary) {
  throw new Error('usage: yarn package:vsix --target <target> --binary <path> [--out <directory>]')
}

const serverDirectory = join(extensionRoot, 'server')
mkdirSync(serverDirectory, { recursive: true })
for (const filename of ['wake-css-language-server', 'wake-css-language-server.exe']) {
  rmSync(join(serverDirectory, filename), { force: true })
}
const serverName = target.startsWith('win32')
  ? 'wake-css-language-server.exe'
  : 'wake-css-language-server'
const stagedBinary = join(serverDirectory, serverName)
copyFileSync(resolve(binary), stagedBinary)
if (!target.startsWith('win32')) chmodSync(stagedBinary, 0o755)
mkdirSync(output, { recursive: true })

const archive = join(output, `crab-css-${target}-${extensionManifest.version}.vsix`)
const require = createRequire(import.meta.url)
const vsce = require.resolve('@vscode/vsce/vsce')
execFileSync(process.execPath, [
  vsce,
  'package',
  '--target',
  target,
  '--out',
  archive,
], { cwd: extensionRoot, stdio: 'inherit' })
inspectVsix(archive, target)
console.log(`Packaged ${basename(stagedBinary)} for ${target}`)
