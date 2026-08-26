import { execFileSync } from 'node:child_process'
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, resolve } from 'node:path'
import { createRequire } from 'node:module'
import { fileURLToPath } from 'node:url'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const require = createRequire(import.meta.url)
const pnpapi = require('pnpapi')
const compilerRoot = pnpapi.resolveToUnqualified('@typescript/native', fileURLToPath(import.meta.url))
const compiler = resolve(compilerRoot, 'bin/tsc')
const project = resolve(repoRoot, 'fixtures/typescript-7/tsconfig.json')

function pnpPackageRoot(name) {
  return dirname(pnpapi.resolveToUnqualified(`${name}/package.json`, fileURLToPath(import.meta.url)))
}

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

const temporary = mkdtempSync(resolve(tmpdir(), 'wake-typescript-7-'))
try {
  const config = JSON.parse(readFileSync(project, 'utf8'))
  config.compilerOptions.paths = {
    react: [resolve(pnpPackageRoot('@types/react'), 'index.d.ts')],
    'react/*': [resolve(pnpPackageRoot('@types/react'), '*')],
    csstype: [resolve(pnpPackageRoot('csstype'), 'index.d.ts')],
  }
  config.include = [resolve(repoRoot, 'fixtures/typescript-7/src')]
  const pnpProject = resolve(temporary, 'tsconfig.json')
  writeFileSync(pnpProject, `${JSON.stringify(config, null, 2)}\n`)
  runCompiler(['--project', pnpProject, '--pretty', 'false'], {
    stdio: 'inherit',
    encoding: undefined,
  })
} finally {
  rmSync(temporary, { recursive: true, force: true })
}
console.log(`TypeScript ${version.slice(1).join('.')} compatibility fixture passed.`)
