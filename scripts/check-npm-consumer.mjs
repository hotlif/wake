import { execFileSync } from 'node:child_process'
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  realpathSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import { tmpdir } from 'node:os'
import { dirname, join, relative, resolve } from 'node:path'

const repositoryRoot = resolve(import.meta.dirname, '..')
const npmCommand = process.platform === 'win32' ? 'npm.cmd' : 'npm'
const npxCommand = process.platform === 'win32' ? 'npx.cmd' : 'npx'
const childEnvironment = { ...process.env }
delete childEnvironment.NODE_OPTIONS
delete childEnvironment.npm_execpath

const hostPlatforms = new Map([
  ['win32-x64', 'win32-x64-msvc'],
  ['linux-x64', 'linux-x64-gnu'],
  ['linux-arm64', 'linux-arm64-gnu'],
  ['darwin-x64', 'darwin-x64'],
  ['darwin-arm64', 'darwin-arm64'],
])

function parseArguments(values) {
  const supported = new Set(['artifacts', 'platform', 'project', 'version'])
  const parsed = new Map()
  for (let index = 0; index < values.length; index += 1) {
    const argument = values[index]
    if (!argument.startsWith('--')) throw new Error(`Unexpected argument ${argument}`)
    const [name, inlineValue] = argument.slice(2).split('=', 2)
    if (!supported.has(name)) throw new Error(`Unknown option --${name}`)
    const value = inlineValue ?? values[++index]
    if (!value || value.startsWith('--')) throw new Error(`--${name} requires a value`)
    if (parsed.has(name)) throw new Error(`--${name} may only be provided once`)
    parsed.set(name, value)
  }
  return parsed
}

function run(command, arguments_, cwd) {
  execFileSync(command, arguments_, {
    cwd,
    env: childEnvironment,
    stdio: 'inherit',
    shell: process.platform === 'win32' && command.toLowerCase().endsWith('.cmd'),
  })
}

function repositoryRelative(path) {
  return relative(repositoryRoot, path).replaceAll('\\', '/')
}

function isInsideRepository(path) {
  const value = repositoryRelative(path)
  return value === '' || (!value.startsWith('../') && value !== '..')
}

function assertNoPnpAncestor(start) {
  let current = resolve(start)
  for (;;) {
    const loader = join(current, '.pnp.cjs')
    if (existsSync(loader)) {
      throw new Error(`npm consumer project must not be nested below Yarn PnP loader ${loader}`)
    }
    const parent = dirname(current)
    if (parent === current) return
    current = parent
  }
}

function fileDependency(project, archive) {
  let path = relative(project, archive).replaceAll('\\', '/')
  if (!path.startsWith('.')) path = `./${path}`
  return `file:${path}`
}

function readJson(path) {
  return JSON.parse(readFileSync(path, 'utf8'))
}

function assertArchiveSource(value, archiveName, label) {
  if (typeof value !== 'string'
    || !value.startsWith('file:')
    || !value.replaceAll('\\', '/').endsWith(archiveName)) {
    throw new Error(`${label} must resolve from this build's ${archiveName}; found ${value}`)
  }
}

function collectFiles(directory, output = []) {
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name)
    if (entry.isDirectory()) collectFiles(path, output)
    else output.push(path)
  }
  return output
}

const argumentsMap = parseArguments(process.argv.slice(2))
const artifacts = resolve(
  argumentsMap.get('artifacts')
    ?? process.env.WAKE_NPM_ARTIFACTS
    ?? join(repositoryRoot, 'artifacts'),
)
const platform = argumentsMap.get('platform')
  ?? process.env.WAKE_NPM_PLATFORM
  ?? hostPlatforms.get(`${process.platform}-${process.arch}`)
const expectedPlatform = hostPlatforms.get(`${process.platform}-${process.arch}`)
if (!expectedPlatform) {
  throw new Error(`Wake has no npm consumer target for ${process.platform}-${process.arch}`)
}
if (platform !== expectedPlatform) {
  throw new Error(`npm consumer on ${process.platform}-${process.arch} requires ${expectedPlatform}; received ${platform}`)
}

const rootManifest = readJson(join(repositoryRoot, 'npm/wake/package.json'))
const version = argumentsMap.get('version')
  ?? process.env.WAKE_NPM_VERSION
  ?? rootManifest.version
if (version !== rootManifest.version) {
  throw new Error(`npm consumer version ${version} does not match source manifest ${rootManifest.version}`)
}

const configuredProject = argumentsMap.get('project') ?? process.env.WAKE_NPM_PROJECT
const project = configuredProject
  ? resolve(configuredProject)
  : mkdtempSync(join(tmpdir(), 'wake-npm-consumer-'))
if (isInsideRepository(project)) {
  throw new Error(`npm consumer project must live outside the source repository: ${project}`)
}
if (configuredProject) {
  if (existsSync(project)) throw new Error(`npm consumer project already exists: ${project}`)
  mkdirSync(project, { recursive: false })
}
assertNoPnpAncestor(project)

const archiveNames = {
  wake: `crab-dev-wake-${version}.tgz`,
  css: `crab-dev-css-${version}.tgz`,
  platform: `crab-dev-wake-${platform}-${version}.tgz`,
}
const archives = Object.fromEntries(
  Object.entries(archiveNames).map(([name, archive]) => {
    const path = join(artifacts, archive)
    if (!existsSync(path) || !statSync(path).isFile()) {
      throw new Error(`Missing npm consumer archive ${path}`)
    }
    return [name, path]
  }),
)
const platformPackage = `@crab-dev/wake-${platform}`
const dependencies = {
  '@crab-dev/css': fileDependency(project, archives.css),
  '@crab-dev/wake': fileDependency(project, archives.wake),
  [platformPackage]: fileDependency(project, archives.platform),
  react: '19.2.8',
  'react-dom': '19.2.8',
}

mkdirSync(join(project, 'packages/app/src'), { recursive: true })
mkdirSync(join(project, 'packages/shared'), { recursive: true })
writeFileSync(join(project, 'package.json'), `${JSON.stringify({
  name: 'wake-npm-consumer',
  private: true,
  version: '0.0.0',
  type: 'module',
  workspaces: ['packages/*'],
  scripts: { 'wake-build': 'wake build' },
  dependencies,
}, null, 2)}\n`)
writeFileSync(join(project, 'packages/app/package.json'), `${JSON.stringify({
  name: 'wake-npm-consumer-app',
  private: true,
  version: '1.0.0',
  dependencies: { 'wake-npm-consumer-shared': '1.0.0' },
}, null, 2)}\n`)
writeFileSync(join(project, 'packages/shared/package.json'), `${JSON.stringify({
  name: 'wake-npm-consumer-shared',
  private: true,
  version: '1.0.0',
  type: 'module',
  exports: './index.js',
}, null, 2)}\n`)
writeFileSync(
  join(project, 'packages/shared/index.js'),
  "export const value = 'WAKE_NPM_WORKSPACE_CLASSIC'\n",
)
writeFileSync(
  join(project, 'packages/app/src/index.js'),
  "import { value } from 'wake-npm-consumer-shared'; console.log(value); export { value };\n",
)
writeFileSync(
  join(project, 'wake.config.toml'),
  '[html]\nentry = "packages/app/src/index.js"\n',
)
writeFileSync(
  join(project, 'smoke.test.mjs'),
  "import { test, expect } from '@crab-dev/wake/test'; test('npm consumer host', () => expect(42).toBe(42));\n",
)

const installArguments = ['--ignore-scripts', '--omit=optional', '--no-audit', '--no-fund']
run(npmCommand, ['install', '--package-lock-only', ...installArguments], project)
if (!existsSync(join(project, 'package-lock.json'))) {
  throw new Error('npm install --package-lock-only did not create package-lock.json')
}
if (existsSync(join(project, 'node_modules'))) {
  throw new Error('package-lock-only unexpectedly created node_modules before npm ci')
}
run(npmCommand, ['ci', ...installArguments], project)

if (existsSync(join(project, '.pnp.cjs'))) {
  throw new Error('npm consumer unexpectedly generated .pnp.cjs')
}
if (!existsSync(join(project, 'node_modules'))) {
  throw new Error('npm ci did not create a physical node_modules tree')
}

const projectManifest = readJson(join(project, 'package.json'))
const packageLock = readJson(join(project, 'package-lock.json'))
const lockedRoot = packageLock.packages?.['']
if (!lockedRoot || packageLock.lockfileVersion < 3) {
  throw new Error('npm consumer requires a modern package-lock with a root package record')
}
const expectedPackages = new Map([
  ['@crab-dev/wake', archiveNames.wake],
  ['@crab-dev/css', archiveNames.css],
  [platformPackage, archiveNames.platform],
])
for (const [name, archiveName] of expectedPackages) {
  assertArchiveSource(projectManifest.dependencies?.[name], archiveName, `package.json ${name}`)
  assertArchiveSource(lockedRoot.dependencies?.[name], archiveName, `package-lock.json ${name}`)
  const installed = readJson(join(project, 'node_modules', name, 'package.json'))
  if (installed.name !== name || installed.version !== version) {
    throw new Error(`${name} installed as ${installed.name}@${installed.version}; expected ${name}@${version}`)
  }
}

const workspaceLink = realpathSync(join(project, 'node_modules/wake-npm-consumer-shared'))
const workspaceSource = realpathSync(join(project, 'packages/shared'))
const normalizeCase = (value) => process.platform === 'win32' ? value.toLowerCase() : value
if (normalizeCase(workspaceLink) !== normalizeCase(workspaceSource)) {
  throw new Error(`npm workspace link points to ${workspaceLink}; expected ${workspaceSource}`)
}

run(npxCommand, ['--no-install', 'wake', '--version'], project)
run(process.execPath, ['-e', [
  "const wake=require('@crab-dev/wake')",
  "const css=require('@crab-dev/css')",
  `const native=require('${platformPackage}')`,
  `if(wake.version()!=='${version}'||native.version()!=='${version}'||css.cx('wake',{npm:true})!=='wake npm')process.exit(1)`,
].join(';')], project)
run(process.execPath, ['--input-type=module', '-e', [
  "import {version} from '@crab-dev/wake'",
  "import {cx} from '@crab-dev/css'",
  `if(version()!=='${version}'||cx('wake',{esm:true})!=='wake esm')process.exit(1)`,
].join(';')], project)
run(npmCommand, ['run', 'wake-build'], project)

const builtFiles = collectFiles(join(project, 'dist'))
const builtJavaScript = builtFiles
  .filter((path) => /\.[cm]?js$/.test(path))
  .map((path) => readFileSync(path, 'utf8'))
  .join('\n')
if (!builtJavaScript.includes('WAKE_NPM_WORKSPACE_CLASSIC')) {
  throw new Error('Wake build did not resolve the npm workspace link through node_modules')
}

run(process.execPath, ['--input-type=module', '-e', [
  "import {build} from '@crab-dev/wake'",
  "const result=await build({cwd:process.cwd()})",
  "if(!result.success)throw new Error(JSON.stringify(result))",
].join(';')], project)
run(npxCommand, ['--no-install', 'wake', 'test', 'smoke.test.mjs', '--serial'], project)

console.log(`npm consumer verified ${platform} at ${project}`)
