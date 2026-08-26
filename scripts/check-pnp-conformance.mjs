import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { cp, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { basename, join, resolve } from 'node:path'

const root = resolve(import.meta.dirname, '..')
const probe = join(root, 'target', 'debug', 'examples', `pnp_probe${process.platform === 'win32' ? '.exe' : ''}`)

function run(command, args, { cwd = root, capture = false, shell = false } = {}) {
  const result = spawnSync(command, args, {
    cwd,
    encoding: 'utf8',
    stdio: capture ? 'pipe' : 'inherit',
    shell,
  })
  if (result.error) throw result.error
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(' ')} exited with ${result.status}\n${result.stdout ?? ''}${result.stderr ?? ''}`)
  }
  return result.stdout?.trim() ?? ''
}

function corepack(args, options) {
  return run(process.platform === 'win32' ? 'corepack.cmd' : 'corepack', args, {
    ...options,
    shell: process.platform === 'win32',
  })
}

function npm(args, options) {
  return run(process.platform === 'win32' ? 'npm.cmd' : 'npm', args, {
    ...options,
    shell: process.platform === 'win32',
  })
}

function normalized(path) {
  return path.replace(/^\\\\\?\\/, '').replaceAll('\\', '/').replace(/\/$/, '')
}

function yarnResolution(project, issuerDirectory, request) {
  const loader = join(project, '.pnp.cjs')
  const source = [
    'const api=require(process.argv[1]);',
    'try{console.log(`OK\\t${api.resolveToUnqualified(process.argv[3],process.argv[2]+require("node:path").sep)}`)}',
    'catch(error){console.log(`ERR\\t${error.pnpCode||error.code||error.name}`)}',
  ].join('')
  return run(process.execPath, ['-e', source, loader, issuerDirectory, request], { cwd: project, capture: true })
}

function wakeResolution(issuerDirectory, request) {
  return run(probe, [issuerDirectory, request], { capture: true })
}

function compare(project, issuerDirectory, request, label, expected = undefined) {
  const yarn = yarnResolution(project, issuerDirectory, request)
  const wake = wakeResolution(issuerDirectory, request)
  const yarnOk = yarn.startsWith('OK\t')
  const wakeOk = wake.startsWith('OK\t')
  assert.equal(wakeOk, yarnOk, `${label}: Yarn=${yarn}; Wake=${wake}`)
  if (yarnOk) {
    const yarnPath = normalized(yarn.slice(3))
    const wakePath = normalized(wake.slice(3))
    if (expected === 'classic') {
      assert.ok(
        yarnPath === wakePath || yarnPath.startsWith(`${wakePath}/`),
        `${label}: Yarn native result ${yarnPath} is outside Wake package root ${wakePath}`,
      )
    } else {
      assert.equal(wakePath, yarnPath, `${label}: unqualified package roots differ`)
    }
    if (expected === 'zip') assert.match(yarnPath, /\.zip\/node_modules\//)
    if (expected === 'unplugged') assert.match(yarnPath, /\.yarn\/unplugged\//)
  } else if (expected === 'success') {
    assert.fail(`${label}: expected success; Yarn=${yarn}; Wake=${wake}`)
  }
}

async function json(path, value) {
  await writeFile(path, `${JSON.stringify(value, null, 2)}\n`)
}

async function makePackage(directory, name, extra = {}) {
  await mkdir(directory, { recursive: true })
  await json(join(directory, 'package.json'), {
    name,
    version: '1.0.0',
    main: 'index.js',
    ...extra,
  })
  await writeFile(join(directory, 'index.js'), `module.exports = ${JSON.stringify(name)}\n`)
}

async function pack(directory, destination) {
  const output = npm(['pack', directory, '--ignore-scripts', '--pack-destination', destination, '--json'], { capture: true })
  return JSON.parse(output)[0].filename
}

async function makeProject(directory, packs, {
  compressionLevel = 0,
  fallbackMode = 'none',
  ignorePatterns = [],
  unplugged = [],
} = {}) {
  await mkdir(directory, { recursive: true })
  const dependencies = Object.fromEntries(Object.entries(packs).map(([name, archive]) => (
    [name, `file:./packs/${basename(archive)}`]
  )))
  await json(join(directory, 'package.json'), {
    name: `pnp-${basename(directory)}`,
    private: true,
    packageManager: 'yarn@4.16.0',
    dependencies,
    dependenciesMeta: Object.fromEntries(unplugged.map((name) => [name, { unplugged: true }])),
  })
  // An empty lock marks nested fixtures as independent Yarn projects before their first install.
  await writeFile(join(directory, 'yarn.lock'), '')
  const ignore = ignorePatterns.length > 0
    ? `\npnpIgnorePatterns:\n${ignorePatterns.map((pattern) => `  - ${JSON.stringify(pattern)}`).join('\n')}\n`
    : '\n'
  await writeFile(join(directory, '.yarnrc.yml'), [
    'nodeLinker: pnp',
    'pnpMode: strict',
    `pnpFallbackMode: ${fallbackMode}`,
    'pnpEnableInlining: false',
    'enableGlobalCache: false',
    `compressionLevel: ${compressionLevel}`,
    'enableScripts: false',
  ].join('\n') + ignore)
  const packDirectory = join(directory, 'packs')
  await mkdir(packDirectory, { recursive: true })
  for (const archive of Object.values(packs)) await cp(archive, join(packDirectory, basename(archive)))
  corepack(['yarn', 'install', '--no-immutable'], { cwd: directory })
  assert.ok(existsSync(join(directory, '.pnp.cjs')))
}

run('cargo', ['+1.95.0', 'build', '-p', 'wake_resolver', '--example', 'pnp_probe', '--locked'])

// The source installation itself covers Yarn alias dependencies and unplugged package roots.
compare(root, root, '@typescript/native', 'alias dependency')
compare(root, root, 'happy-dom', 'source unplugged package', 'unplugged')

const temporary = await mkdtemp(join(tmpdir(), 'wake-pnp-conformance-'))
let completed = false
try {
  const sources = join(temporary, 'sources')
  const archives = join(temporary, 'archives')
  await mkdir(archives, { recursive: true })
  await makePackage(join(sources, 'plain'), 'plain')
  await makePackage(join(sources, 'peer-provider'), 'peer-provider')
  await makePackage(join(sources, 'peer-user'), 'peer-user', { peerDependencies: { 'peer-provider': '*' } })
  await makePackage(join(sources, 'fallback-user'), 'fallback-user')
  const packed = {}
  for (const name of ['plain', 'peer-provider', 'peer-user', 'fallback-user']) {
    packed[name] = join(archives, await pack(join(sources, name), archives))
  }

  for (const compressionLevel of [0, 9]) {
    const project = join(temporary, `compression-${compressionLevel}`)
    await makeProject(project, { plain: packed.plain }, { compressionLevel })
    compare(project, project, 'plain', `compressionLevel ${compressionLevel}`, 'zip')
  }

  const semantics = join(temporary, 'semantics')
  await makeProject(semantics, packed, {
    fallbackMode: 'all',
    ignorePatterns: ['./ignored/**'],
    unplugged: ['plain'],
  })
  compare(semantics, semantics, 'plain', 'direct unplugged dependency', 'unplugged')
  const peerIssuer = yarnResolution(semantics, semantics, 'peer-user').slice(3)
  compare(semantics, peerIssuer, 'peer-provider', 'virtual peer dependency')
  const fallbackIssuer = yarnResolution(semantics, semantics, 'fallback-user').slice(3)
  compare(semantics, fallbackIssuer, 'plain', 'Yarn top-level fallback', 'success')

  const strict = join(temporary, 'strict')
  await makeProject(strict, packed, { fallbackMode: 'none' })
  const strictIssuer = yarnResolution(strict, strict, 'fallback-user').slice(3)
  compare(strict, strictIssuer, 'plain', 'strict undeclared dependency rejection')

  const ignoredIssuer = join(semantics, 'ignored', 'tool')
  await makePackage(join(ignoredIssuer, 'node_modules', 'classic-only'), 'classic-only')
  compare(semantics, ignoredIssuer, 'classic-only', 'pnpIgnorePatterns classic routing', 'classic')

  const parent = join(temporary, 'nested')
  await makeProject(parent, { plain: packed.plain })
  const childSource = join(sources, 'child-only')
  await makePackage(childSource, 'child-only')
  const childArchive = join(archives, await pack(childSource, archives))
  const child = join(parent, 'child')
  await makeProject(child, { 'child-only': childArchive })
  compare(child, child, 'child-only', 'nearest nested PnP root', 'zip')

  completed = true
  console.log('Yarn 4.16 differential PnP conformance passed: alias, direct, peer/virtual, unplugged, fallback, ignore, nested roots, compression 0/9.')
} finally {
  if (completed || process.env.WAKE_PNP_KEEP_TEMP !== '1') await rm(temporary, { recursive: true, force: true })
  else console.error(`PnP conformance fixture retained at ${temporary}`)
}
