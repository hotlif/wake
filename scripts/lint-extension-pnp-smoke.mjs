import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { basename, join, resolve, sep } from 'node:path'

const repository = resolve(import.meta.dirname, '..')
const binary = resolve(repository, 'target', 'debug', process.platform === 'win32' ? 'wake.exe' : 'wake')
const project = mkdtempSync(join(tmpdir(), 'wake-lint-extension-pnp-'))
const packageName = '@smoke/wake-lint-plugin'
const ruleId = 'smoke/no-debugger'
const marker = join(project, 'plugin-loaded')

function run(command, args, { cwd = project, env = process.env, shell = false } = {}) {
  const result = spawnSync(command, args, { cwd, env, shell, encoding: 'utf8', maxBuffer: 4 * 1024 * 1024 })
  assert.equal(result.error, undefined, result.error?.message)
  return result
}

function runCorepack(args) {
  const command = process.platform === 'win32' ? 'corepack.cmd' : 'corepack'
  const result = run(command, args, {
    shell: process.platform === 'win32',
    env: {
      ...process.env,
      YARN_CACHE_FOLDER: join(project, '.yarn', 'cache'),
      YARN_ENABLE_GLOBAL_CACHE: '0',
      YARN_GLOBAL_FOLDER: join(project, '.yarn', 'global'),
    },
  })
  assert.equal(result.status, 0, `corepack ${args.join(' ')} failed:\n${result.stdout}\n${result.stderr}`)
  return result
}

function lint(args) {
  return run(binary, ['lint', '--root', project, '--format', 'json', ...args], {
    env: { ...process.env, WAKE_LINT_EXTENSION_MARKER: marker },
  })
}

function jsonOutput(result) {
  try {
    return JSON.parse(result.stdout)
  } catch (error) {
    throw new Error(`wake lint returned non-JSON (${result.status}): ${result.stderr}`, { cause: error })
  }
}

try {
  const plugin = join(project, 'plugin')
  mkdirSync(plugin, { recursive: true })
  writeFileSync(join(project, 'package.json'), JSON.stringify({
    name: 'wake-lint-extension-pnp-smoke',
    version: '1.0.0',
    private: true,
    type: 'module',
    packageManager: 'yarn@4.16.0',
    dependencies: { [packageName]: 'file:./plugin' },
  }))
  writeFileSync(join(project, 'yarn.lock'), '')
  writeFileSync(join(project, '.yarnrc.yml'), [
    'nodeLinker: pnp',
    'pnpMode: strict',
    'pnpFallbackMode: none',
    'pnpEnableInlining: false',
    'enableGlobalCache: false',
    'compressionLevel: 0',
    'enableScripts: false',
    '',
  ].join('\n'))
  writeFileSync(join(project, 'wake.config.toml'), `[lint]
recommended = false
extensions = ["${packageName}"]

[lint.rules]
"${ruleId}" = "error"
`)
  const source = '// 🙂\ndebugger;\n'
  writeFileSync(join(project, 'a.js'), source)
  writeFileSync(join(plugin, 'package.json'), JSON.stringify({
    name: packageName,
    version: '1.0.0',
    type: 'module',
    exports: { import: './index.mjs' },
    wakeLint: { manifest: './wake-lint.manifest.json', module: './index.mjs' },
  }))
  writeFileSync(join(plugin, 'wake-lint.manifest.json'), JSON.stringify({
    schema: 'wake.lint.package.v1',
    name: packageName,
    version: '1.0.0',
    protocol: 'wake.lint.extension.v1',
    sdkMajor: 1,
    rules: [{
      id: ruleId,
      meta: { description: 'Find debugger statements', category: 'problem', fixable: false },
      messageIds: ['found'],
      optionsSchema: { type: 'object', additionalProperties: false, properties: {} },
    }],
  }))
  writeFileSync(join(plugin, 'index.mjs'), `import { writeFileSync } from 'node:fs'
writeFileSync(process.env.WAKE_LINT_EXTENSION_MARKER, 'loaded')
export default {
  protocol: 'wake.lint.extension.v1', sdkMajor: 1,
  name: '${packageName}', version: '1.0.0',
  rules: [{
    protocol: 'wake.lint.extension.v1', id: '${ruleId}',
    meta: { description: 'Find debugger statements', category: 'problem', fixable: false },
    create(context) {
      const index = context.source.indexOf('debugger')
      if (index < 0) return
      const start = Buffer.byteLength(context.source.slice(0, index))
      context.report({ message: 'debugger is not allowed', messageId: 'found', start, end: start + 8 })
    },
  }],
}
`)

  runCorepack(['yarn', 'install', '--no-immutable'])
  const loader = join(project, '.pnp.cjs')
  assert.ok(existsSync(loader), 'Yarn did not create the PnP loader')
  assert.ok(existsSync(join(project, '.pnp.loader.mjs')), 'Yarn did not create the PnP ESM loader')
  const location = run(process.execPath, [
    '--require', loader,
    '-e', "console.log(require('pnpapi').resolveToUnqualified(process.argv[1], process.argv[2]))",
    packageName, join(project, 'package.json'),
  ])
  assert.equal(location.status, 0, location.stderr)
  assert.match(location.stdout.replaceAll('\\', '/'), /\.zip\/node_modules\/@smoke\/wake-lint-plugin\//,
    `extension dependency must be loaded from a PnP ZIP: ${location.stdout}`)
  assert.ok(!existsSync(marker), 'installation unexpectedly imported the plugin')

  const denied = lint(['a.js'])
  assert.equal(denied.status, 2, denied.stderr)
  assert.match(denied.stderr, /WAKE_LINT_CONFIG/)
  assert.match(denied.stderr, /allow-lint-extensions|allowExtensions/)
  assert.ok(!existsSync(marker), 'unauthorized lint imported the plugin')

  const deniedConfig = lint(['--print-config', 'a.js'])
  assert.equal(deniedConfig.status, 2, deniedConfig.stderr)
  assert.match(deniedConfig.stderr, /WAKE_LINT_CONFIG/)
  assert.ok(!existsSync(marker), 'unauthorized print-config imported the plugin')

  const explained = lint(['--allow-lint-extensions', '--print-config', 'a.js'])
  assert.equal(explained.status, 0, explained.stderr)
  assert.equal(jsonOutput(explained).config.rules[ruleId].level, 'error')
  assert.ok(!existsSync(marker), 'authorized print-config imported the plugin')

  const checked = lint(['--allow-lint-extensions', 'a.js'])
  assert.equal(checked.status, 1, checked.stderr)
  const result = jsonOutput(checked)
  assert.equal(result.errorCount, 1)
  const diagnostic = result.files.flatMap((file) => file.diagnostics).find((item) => item.code === ruleId)
  assert.ok(diagnostic, JSON.stringify(result))
  assert.equal(diagnostic.messageId, 'found')
  assert.equal(diagnostic.start, Buffer.byteLength('// 🙂\n'))
  assert.ok(existsSync(marker), 'authorized lint did not import the plugin')
  assert.equal(readFileSync(join(project, 'a.js'), 'utf8'), source)
  console.log('wake lint PnP ZIP extension smoke ok')
} finally {
  assert.ok(resolve(project).startsWith(`${resolve(tmpdir())}${sep}`))
  assert.ok(basename(project).startsWith('wake-lint-extension-pnp-'))
  rmSync(project, { recursive: true, force: true })
}
