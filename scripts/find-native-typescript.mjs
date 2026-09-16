import { appendFileSync, existsSync, readdirSync, statSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const unplugged = join(root, '.yarn', 'unplugged')
const suffixes = {
  'win32-x64': 'typescript-win32-x64',
  'linux-x64': 'typescript-linux-x64',
  'linux-arm64': 'typescript-linux-arm64',
  'darwin-x64': 'typescript-darwin-x64',
  'darwin-arm64': 'typescript-darwin-arm64',
}
const suffix = suffixes[`${process.platform}-${process.arch}`]
if (!suffix) {
  throw new Error(`Unsupported native TypeScript runner: ${process.platform}/${process.arch}`)
}

function walk(directory) {
  if (!existsSync(directory)) return []
  const files = []
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name)
    if (entry.isDirectory()) files.push(...walk(path))
    else if (entry.isFile()) files.push(path)
  }
  return files
}

const expected = `@typescript/${suffix}/lib/tsc${process.platform === 'win32' ? '.exe' : ''}`
const compiler = walk(unplugged).find((path) => path.replaceAll('\\', '/').endsWith(expected))
if (!compiler || !statSync(compiler).isFile()) {
  throw new Error(`Unable to find PnP native TypeScript compiler ${suffix} under ${unplugged}`)
}

const output = resolve(compiler)
const mode = process.argv.slice(2)
if (mode.length === 0 || mode[0] === '--print') {
  console.log(output)
} else if (mode.length === 1 && mode[0] === '--github-env') {
  if (!process.env.GITHUB_ENV) throw new Error('--github-env requires GITHUB_ENV')
  appendFileSync(process.env.GITHUB_ENV, `WAKE_LINT_TYPESCRIPT_EXE=${output}${String.fromCharCode(10)}`)
  console.log(`Selected native TypeScript compiler: ${output}`)
} else {
  throw new Error('Usage: node scripts/find-native-typescript.mjs [--print|--github-env]')
}
