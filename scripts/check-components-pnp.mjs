import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { existsSync } from 'node:fs'
import { cp, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from 'node:fs/promises'
import { tmpdir } from 'node:os'
import { basename, join, resolve } from 'node:path'
import { assertComponentsRuntime } from './components-runtime-smoke.mjs'

const root = resolve(import.meta.dirname, '..')
const fixture = resolve(root, 'fixtures/react-components-yarn-pnp')
const packageDirectories = [
  'npm/wake',
  'npm/wake-win32-x64-msvc',
  'npm/wake-linux-x64-gnu',
  'npm/wake-linux-arm64-gnu',
  'npm/wake-darwin-x64',
  'npm/wake-darwin-arm64',
]
const platformPackageNames = [
  '@crab-dev/wake-win32-x64-msvc',
  '@crab-dev/wake-linux-x64-gnu',
  '@crab-dev/wake-linux-arm64-gnu',
  '@crab-dev/wake-darwin-x64',
  '@crab-dev/wake-darwin-arm64',
]
const componentPrefixes = [
  'rc-checkbox-',
  'rc-dropdown-container-',
  'rc-spin-',
  'rc-virtual-',
  'rc-alert-',
  'rc-button-',
  'rc-dialog-',
  'rc-drawer-',
  'rc-empty-',
  'rc-line-edit-',
  'rc-number-edit-',
  'rc-segmented-',
  'rc-select-',
  'rc-switch-',
  'rc-tag-',
  'rc-text-edit-',
  'rc-tooltip-',
  'rc-tree-',
]

function run(command, args, options = {}) {
  const result = spawnSync(command, args, {
    cwd: options.cwd ?? root,
    env: options.env ?? process.env,
    encoding: 'utf8',
    shell: options.shell ?? false,
    stdio: options.capture ? 'pipe' : 'inherit',
  })
  if (result.error) throw result.error
  if (result.status !== 0) {
    const detail = [result.stdout, result.stderr].filter(Boolean).join('\n').trim()
    throw new Error(
      `${command} ${args.join(' ')} exited with ${result.status}${detail ? `\n${detail}` : ''}`,
    )
  }
  return result
}

function runNpm(args, options = {}) {
  if (process.env.npm_execpath) {
    return run(process.execPath, [process.env.npm_execpath, ...args], options)
  }
  return run(process.platform === 'win32' ? 'npm.cmd' : 'npm', args, {
    ...options,
    shell: process.platform === 'win32',
  })
}

function runCorepack(args, options = {}) {
  return run(process.platform === 'win32' ? 'corepack.cmd' : 'corepack', args, {
    ...options,
    shell: process.platform === 'win32',
  })
}

function nativeSuffix() {
  if (process.platform === 'win32' && process.arch === 'x64') return 'win32-x64-msvc'
  if (process.platform === 'darwin' && process.arch === 'x64') return 'darwin-x64'
  if (process.platform === 'darwin' && process.arch === 'arm64') return 'darwin-arm64'
  if (process.platform === 'linux' && process.arch === 'x64') return 'linux-x64-gnu'
  if (process.platform === 'linux' && process.arch === 'arm64') return 'linux-arm64-gnu'
  throw new Error(`Unsupported PnP gate platform: ${process.platform}/${process.arch}`)
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

const fixtureManifest = JSON.parse(await readFile(join(fixture, 'package.json'), 'utf8'))
const corepackVersion = runCorepack(['--version'], { capture: true }).stdout.trim()
assert.equal(corepackVersion, '0.34.6', 'The PnP gate must run with Corepack 0.34.6')
assert.equal(fixtureManifest.packageManager, 'yarn@4.16.0')
assert.deepEqual(
  Object.keys(fixtureManifest.dependencies).sort(),
  ['@crab-dev/wake', 'react', 'react-dom'],
  'The committed PnP fixture must declare only Wake, React, and React DOM',
)

const nativePath = resolve(
  process.env.WAKE_NATIVE_PATH || join(root, 'npm/wake', `wake.${nativeSuffix()}.node`),
)
assert.ok(
  existsSync(nativePath),
  `Build the native module first or set WAKE_NATIVE_PATH (missing ${nativePath})`,
)

const temporaryProject = await mkdtemp(join(tmpdir(), 'wake-components-pnp-'))
let completed = false
try {
  await cp(fixture, temporaryProject, { recursive: true })
  const packsDirectory = join(temporaryProject, '.wake-packs')
  await mkdir(packsDirectory, { recursive: true })

  const packed = new Map()
  for (const directory of packageDirectories) {
    const result = runNpm([
      'pack',
      resolve(root, directory),
      '--ignore-scripts',
      '--pack-destination',
      packsDirectory,
      '--json',
    ], { capture: true })
    const [metadata] = JSON.parse(result.stdout)
    assert.ok(metadata?.filename, `npm pack did not report an archive for ${directory}`)
    packed.set(metadata.name, metadata)
  }

  assert.deepEqual(
    [...packed.keys()].sort(),
    ['@crab-dev/wake', ...platformPackageNames].sort(),
    'The PnP gate must pack the main package and all five platform packages',
  )
  const mainPack = packed.get('@crab-dev/wake')
  assert.ok(
    mainPack.files.some((file) => file.path === 'internal/components-runtime.mjs'),
    'Wake tarball must include the Components runtime module',
  )
  assert.ok(
    mainPack.files.some((file) => file.path === 'internal/components-runtime.d.ts'),
    'Wake tarball must include the Components runtime declarations',
  )
  assert.equal(
    mainPack.files.some((file) => file.path.endsWith('.node')),
    false,
    'Wake main tarball must not contain a native module',
  )

  const internalRuntime = await readFile(
    resolve(root, 'npm/wake/internal/components-runtime.mjs'),
    'utf8',
  )
  assert.doesNotMatch(
    internalRuntime,
    /["'][^"'\r\n]+\.css(?:\?[^"'\r\n]*)?["']/,
    'The Components runtime must not import CSS explicitly',
  )

  const projectManifestPath = join(temporaryProject, 'package.json')
  const projectManifest = JSON.parse(await readFile(projectManifestPath, 'utf8'))
  const localReference = (metadata) => `file:./.wake-packs/${basename(metadata.filename)}`
  projectManifest.dependencies['@crab-dev/wake'] = localReference(mainPack)
  projectManifest.resolutions = Object.fromEntries(
    platformPackageNames.map((name) => [name, localReference(packed.get(name))]),
  )
  await writeFile(projectManifestPath, `${JSON.stringify(projectManifest, null, 2)}\n`)

  const environment = { ...process.env, WAKE_NATIVE_PATH: nativePath }
  const yarnVersion = runCorepack(['yarn', '--version'], {
    cwd: temporaryProject,
    env: environment,
    capture: true,
  }).stdout.trim()
  assert.equal(yarnVersion, '4.16.0', 'The PnP gate must run with Yarn 4.16.0')
  runCorepack(['yarn', 'install'], { cwd: temporaryProject, env: environment })
  assert.ok(existsSync(join(temporaryProject, '.pnp.cjs')), 'Yarn must use Plug\'n\'Play')
  runCorepack(['yarn', 'run', 'components:build'], {
    cwd: temporaryProject,
    env: environment,
  })

  const generatedRuntime = await readFile(
    join(temporaryProject, '.wake/docs/generated/runtime/components.tsx'),
    'utf8',
  )
  assert.match(
    generatedRuntime,
    /from\s+["']@crab-dev\/wake\/internal\/components-runtime["']/,
    'Generated workbench code must import the Wake internal runtime',
  )
  assert.doesNotMatch(
    generatedRuntime,
    /from\s+["']@crab-dev\/rc-/,
    'Generated workbench code must not import Crab UI packages directly',
  )
  assert.doesNotMatch(
    generatedRuntime,
    /["'][^"'\r\n]+\.css(?:\?[^"'\r\n]*)?["']/,
    'Generated workbench code must not import component CSS explicitly',
  )

  const outputDirectory = join(temporaryProject, 'dist')
  const outputFiles = await readdir(outputDirectory)
  const entryFile = outputFiles.find((file) => /^entry\.[0-9a-f]{8}\.js$/.test(file))
  assert.ok(entryFile, 'Components build must emit a hashed JavaScript entry')
  await assertComponentsRuntime(join(outputDirectory, entryFile))
  const cssFile = outputFiles.find((file) => /^styles\.[0-9a-f]{8}\.css$/.test(file))
  assert.ok(cssFile, 'Components build must emit a hashed CSS asset')
  const html = await readFile(join(outputDirectory, 'index.html'), 'utf8')
  assert.match(
    html,
    new RegExp(`href=["'][^"']*${escapeRegExp(cssFile)}["']`),
    'index.html must link the emitted hashed CSS asset',
  )
  const css = await readFile(join(outputDirectory, cssFile), 'utf8')
  for (const prefix of componentPrefixes) {
    assert.match(css, new RegExp(escapeRegExp(prefix)), `Components CSS must include ${prefix}`)
  }

  completed = true
  console.log(JSON.stringify({
    corepack: corepackVersion,
    yarn: yarnVersion,
    packages: [...packed.values()].map((metadata) => metadata.filename).sort(),
    css: cssFile,
    cssBytes: Buffer.byteLength(css),
    componentPrefixes: componentPrefixes.length,
  }, null, 2))
} finally {
  if (process.env.WAKE_PNP_KEEP_TEMP === '1') {
    console.error(`PnP fixture retained at ${temporaryProject}`)
  } else {
    if (!completed) {
      console.error(
        `PnP fixture failed at ${temporaryProject}; set WAKE_PNP_KEEP_TEMP=1 to retain future failures`,
      )
    }
    await rm(temporaryProject, { recursive: true, force: true })
  }
}
