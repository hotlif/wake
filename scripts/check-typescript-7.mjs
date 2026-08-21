import { execFileSync } from 'node:child_process'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const compiler = resolve(repoRoot, 'node_modules/typescript/bin/tsc')
const project = resolve(repoRoot, 'fixtures/typescript-7/tsconfig.json')

function runCompiler(args, options = {}) {
  return execFileSync(process.execPath, [compiler, ...args], {
    cwd: repoRoot,
    encoding: 'utf8',
    ...options,
  })
}

const versionOutput = runCompiler(['--version']).trim()
const version = /^Version (\d+)\.(\d+)\.(\d+)$/.exec(versionOutput)

if (!version || version[1] !== '7') {
  throw new Error(`TypeScript 7 is required, but the local compiler reported ${JSON.stringify(versionOutput)}`)
}

runCompiler(['--project', project, '--pretty', 'false'], { stdio: 'inherit', encoding: undefined })
console.log(`TypeScript ${version.slice(1).join('.')} compatibility fixture passed.`)
