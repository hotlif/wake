import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { cp, mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { basename, dirname, join, resolve } from 'node:path'

const root = resolve(import.meta.dirname, '..')
const probe = join(root, 'target', 'debug', 'examples', `pnp_probe${process.platform === 'win32' ? '.exe' : ''}`)
const typeFixture = join(root, 'fixtures', 'pnp-type-consumer')

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

function sourcePackageVersion(name) {
  const source = [
    'const fs=require("node:fs"),path=require("node:path"),api=require("pnpapi");',
    'const manifest=api.resolveToUnqualified(`${process.argv[1]}/package.json`,process.argv[2]+path.sep);',
    'console.log(JSON.parse(fs.readFileSync(manifest,"utf8")).version);',
  ].join('')
  return run(process.execPath, [
    '--require', join(root, '.pnp.cjs'), '-e', source, name, root,
  ], { capture: true })
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

async function checkTypeConsumer(temporary, { typecheck = true } = {}) {
  const project = join(temporary, 'type-consumer')
  const archiveDirectory = join(temporary, 'type-archives')
  const wakeSource = join(root, 'npm', 'wake')
  const wakeProjection = join(temporary, 'wake-type-projection')
  await cp(typeFixture, project, { recursive: true })
  await mkdir(archiveDirectory, { recursive: true })
  const rootManifest = JSON.parse(await readFile(join(root, 'package.json'), 'utf8'))
  const wakeManifest = JSON.parse(await readFile(join(wakeSource, 'package.json'), 'utf8'))
  const declarationPaths = [...new Set(Object.entries(wakeManifest.exports).flatMap(([subpath, entry]) => {
    if (subpath === './package.json') return []
    assert.ok(typeof entry?.types === 'string', `Missing Wake types export for ${subpath}`)
    assert.match(entry.types, /^\.\/[\w/-]+\.d\.(?:ts|mts|cts)$/)
    return [entry.types.slice(2)]
  }))]
  await mkdir(wakeProjection, { recursive: true })
  for (const path of declarationPaths) {
    const source = join(wakeSource, path)
    const destination = join(wakeProjection, path)
    await mkdir(dirname(destination), { recursive: true })
    await cp(source, destination)
    assert.deepEqual(await readFile(destination), await readFile(source), `Wake declaration changed: ${path}`)
  }
  await json(join(wakeProjection, 'package.json'), {
    name: wakeManifest.name,
    version: wakeManifest.version,
    type: wakeManifest.type,
    main: wakeManifest.main,
    module: wakeManifest.module,
    types: wakeManifest.types,
    exports: wakeManifest.exports,
    files: declarationPaths,
  })
  // This archive tests the published declaration map and bytes. Runtime dependencies and
  // platform binaries are exercised by the separate Components PnP and npm consumer gates.
  const sources = new Map([
    ['@crab-dev/wake', wakeProjection],
    ['@crab-dev/css', join(root, 'npm', 'css')],
    ['@wake/pnp-shared-tsconfig', join(typeFixture, 'shared-config')],
  ])
  const packed = new Map()
  const packDirectory = join(project, 'packs')
  await mkdir(packDirectory, { recursive: true })
  for (const [name, source] of sources) {
    const archive = join(archiveDirectory, await pack(source, archiveDirectory))
    await cp(archive, join(packDirectory, basename(archive)))
    packed.set(name, `file:./packs/${basename(archive)}`)
  }
  const dependencies = Object.fromEntries([
    ...['@crab-dev/wake', '@crab-dev/css', '@wake/pnp-shared-tsconfig']
      .map((name) => [name, packed.get(name)]),
    ...(typecheck ? [
      ['@types/node', sourcePackageVersion('@types/node')],
      ...['@yarnpkg/pnpify', 'typescript']
        .map((name) => [name, rootManifest.devDependencies[name]]),
    ] : []),
  ])
  for (const [name, version] of Object.entries(dependencies)) {
    assert.ok(typeof version === 'string' && version.length > 0, `Missing ${name} dependency`)
  }
  await json(join(project, 'package.json'), {
    name: 'wake-pnp-type-consumer',
    private: true,
    type: 'module',
    packageManager: 'yarn@4.16.0',
    dependencies,
    resolutions: Object.fromEntries(
      [...packed].filter(([name]) => name.startsWith('@crab-dev/')),
    ),
  })
  // Keep the compiler graph pinned to the tested root lock. Yarn replaces the
  // root workspace entry with this independent project's packed dependencies.
  await cp(join(root, 'yarn.lock'), join(project, 'yarn.lock'))
  await writeFile(join(project, '.yarnrc.yml'), [
    'nodeLinker: pnp',
    'pnpMode: strict',
    'pnpFallbackMode: none',
    'pnpEnableInlining: false',
    'enableGlobalCache: false',
    'compressionLevel: 0',
    'enableScripts: false',
    '',
  ].join('\n'))
  assert.equal(corepack(['yarn', '--version'], { cwd: project, capture: true }), '4.16.0')
  corepack(['yarn', 'install', '--no-immutable'], { cwd: project })
  assert.ok(existsSync(join(project, '.pnp.cjs')), 'The type consumer must use PnP')

  for (const name of ['@crab-dev/wake', '@crab-dev/css', '@wake/pnp-shared-tsconfig']) {
    compare(project, project, name, `independent packed ${name}`, 'zip')
  }
  const typeEntryProbe = [
    'const fs=require("node:fs"),path=require("node:path"),api=require("pnpapi");',
    'const issuer=process.argv[1]+path.sep,entries=[];',
    'for(const [name,subpath] of [["@crab-dev/wake","."],["@crab-dev/wake","./test"],["@crab-dev/css","."]]){',
    'const manifestPath=api.resolveToUnqualified(`${name}/package.json`,issuer);',
    'const manifest=JSON.parse(fs.readFileSync(manifestPath,"utf8"));',
    'const entry=path.resolve(path.dirname(manifestPath),manifest.exports[subpath].types);',
    'if(!fs.statSync(entry).isFile())throw new Error(`Missing ${name}${subpath} types: ${entry}`);',
    'entries.push(entry);}',
    'const shared=api.resolveToUnqualified("@wake/pnp-shared-tsconfig/strict.json",issuer);',
    'if(!fs.statSync(shared).isFile())throw new Error(`Missing shared tsconfig: ${shared}`);',
    'console.log(JSON.stringify({entries,shared}));',
  ].join('')
  const entries = JSON.parse(run(process.execPath, [
    '--require', join(project, '.pnp.cjs'), '-e', typeEntryProbe, project,
  ], { cwd: project, capture: true }))
  for (const path of [...entries.entries, entries.shared]) {
    assert.match(normalized(path), /\.zip\/node_modules\//, `Expected a zipped PnP type entry: ${path}`)
  }

  if (!typecheck) return

  const config = JSON.parse(corepack([
    'yarn', 'exec', 'pnpify', 'tsc', '--project', 'tsconfig.json', '--showConfig',
  ], { cwd: project, capture: true }))
  assert.equal(config.compilerOptions.strict, true, 'The shared tsconfig must set strict')
  assert.equal(
    config.compilerOptions.noUncheckedIndexedAccess,
    true,
    'The shared tsconfig must set noUncheckedIndexedAccess',
  )
  corepack([
    'yarn', 'exec', 'pnpify', 'tsc', '--project', 'tsconfig.json', '--pretty', 'false',
  ], { cwd: project })
}

async function checkResolverMatrix(temporary) {
  // The source installation itself covers Yarn alias dependencies and unplugged package roots.
  compare(root, root, '@typescript/native', 'alias dependency')
  compare(root, root, 'happy-dom', 'source unplugged package', 'unplugged')

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
}

const mode = process.argv[2] ?? 'all'
assert.ok(
  ['all', '--type-consumer-only', '--local-entries-only'].includes(mode),
  `Unknown PnP conformance mode: ${mode}`,
)
run('cargo', ['+1.95.0', 'build', '-p', 'wake_resolver', '--example', 'pnp_probe', '--locked'])
const temporary = await mkdtemp(join(tmpdir(), 'wake-pnp-conformance-'))
let completed = false
try {
  if (mode === 'all') await checkResolverMatrix(temporary)
  await checkTypeConsumer(temporary, { typecheck: mode !== '--local-entries-only' })

  completed = true
  console.log(mode === 'all'
    ? 'Yarn 4.16 differential PnP conformance passed: alias, direct, peer/virtual, unplugged, fallback, ignore, nested roots, compression 0/9, shared tsconfig, Wake type projection and packed CSS types.'
    : mode === '--type-consumer-only'
      ? 'Yarn 4.16 independent PnP type consumer passed: shared tsconfig, Wake type projection and packed CSS types.'
      : 'Yarn 4.16 local PnP entries passed: shared tsconfig, Wake type projection and packed CSS declaration paths.')
} finally {
  if (completed || process.env.WAKE_PNP_KEEP_TEMP !== '1') await rm(temporary, { recursive: true, force: true })
  else console.error(`PnP conformance fixture retained at ${temporary}`)
}
