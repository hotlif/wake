import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { once } from 'node:events'
import { access, appendFile, cp, mkdir, mkdtemp, readFile, readdir, rm, stat, symlink, writeFile } from 'node:fs/promises'
import { createRequire } from 'node:module'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { dirname, join } from 'node:path'
import { runInNewContext } from 'node:vm'
// This is the one intentionally Node-owned gate: it loads the real `.node` addon and exercises
// Worker, socket, and cleanup-hook lifecycles. It is not a Wake Test runner or product fallback.
import { after, test } from 'node:test'
import { fileURLToPath } from 'node:url'
import { Worker } from 'node:worker_threads'
import { assertComponentsRuntime } from '../../../scripts/components-runtime-smoke.mjs'
import {
  WakeError,
  build,
  buildLibrary,
  buildDocs,
  bundle,
  createBuildContext,
  createTestContext,
  generateFederationLock,
  generateCssToken,
  generateDocgen,
  initializeFederation,
  runTests,
  startDevServer,
  startDocsDevServer,
  version,
} from '../index.mjs'
import * as esmApi from '../index.mjs'
import { analyze, parse, tokenize, transform } from '../experimental.mjs'

const require = createRequire(import.meta.url)
const commonjs = require('../index.cjs')
const { splitOptions } = require('../errors.cjs')
const {
  getTestContextFatalError,
  sendTestWatchControl,
} = require('../test-context-internal.cjs')
const packageVersion = JSON.parse(
  await readFile(new URL('../package.json', import.meta.url), 'utf8'),
).version
const contexts = []

test('native lint module graphs share catalog source overlays cache bypass and failures', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-native-lint-modules-'))
  let context
  try {
    await writeFile(join(root, 'a.ts'), "import './b';")
    await writeFile(join(root, 'b.ts'), "import './a';")
    const rules = { 'import/no-cycle': 'error', 'import/no-unresolved': 'error' }
    const options = { root, paths: ['a.ts'], rules, cache: true }
    const catalog = (await esmApi.lint({ listRules: true })).catalog.rules.filter(rule => rule.analysis === 'module-graph')
    assert.equal(catalog.length, 6)
    assert.ok(catalog.every(rule => rule.defaultLevel === 'off' && !rule.fixable))
    assert.equal(catalog.find(rule => rule.id === 'import/order').optionsSchema.properties.groups.type, 'array')
    const result = await esmApi.lint(options)
    assert.equal(result.errorCount, 1)
    assert.equal(result.files[0].diagnostics[0].messageId, 'cycle')
    assert.deepEqual(result.cache, { hits: 0, misses: 0, writes: 0, bypassed: 1, warnings: [] })
    assert.deepEqual(await commonjs.lint(options), result)
    context = await esmApi.createLintContext(options)
    await context.updateDocument({ filename: 'b.ts', version: 1, text: 'export {};' })
    assert.equal((await context.check()).result.errorCount, 0)
    assert.equal(await readFile(join(root, 'b.ts'), 'utf8'), "import './a';")
    const stdin = await esmApi.lint({ root, rules, stdin: { filename: 'a.ts', text: 'export {};' } })
    assert.equal(stdin.errorCount, 0)
    await writeFile(join(root, '.pnp.cjs'), 'broken')
    await assert.rejects(esmApi.lint(options), { code: 'WAKE_LINT_ANALYSIS' })
    await assert.rejects(context.check(), { code: 'WAKE_LINT_ANALYSIS' })
  } finally {
    await context?.close()
    await rm(root, { recursive: true, force: true })
  }
})

test('native lint type service shares config, final fixes and unsaved dependency facts', async () => {
  const parent = join(dirname(fileURLToPath(import.meta.url)), '../../../.tmp/lint-validation')
  await mkdir(parent, { recursive: true })
  const root = await mkdtemp(join(parent, 'typed-addon-'))
  let context
  try {
    await writeFile(join(root, 'wake.config.toml'), '[lint]\nrecommended=false\n[lint.types]\ncompiler="@typescript/native"\nprojects=["tsconfig.json"]\n[lint.rules]\n"ts/no-unsafe-call"="error"\n"style/eol-last"="error"')
    await writeFile(join(root, 'tsconfig.json'), JSON.stringify({ compilerOptions: { strict: true, types: [], target: 'es2022', module: 'esnext', moduleResolution: 'bundler' }, files: ['a.ts', 'b.ts'] }))
    await writeFile(join(root, 'a.ts'), "import {value} from './b'; value();")
    await writeFile(join(root, 'b.ts'), 'export const value:any = 1;\n')
    const options = { root, paths: ['a.ts'], cache: true }
    const explained = await esmApi.lint({ root, printConfig: 'a.ts' })
    assert.deepEqual(explained.config.types, { compiler: '@typescript/native', projects: ['tsconfig.json'] })
    const cold = await commonjs.lint(options)
    assert.equal(cold.errorCount, 2)
    assert.equal(cold.cache.bypassed, 1)
    const fixed = await esmApi.lint({ ...options, fix: 'dry-run' })
    assert.equal(fixed.errorCount, 1)
    assert.equal(fixed.files[0].output, "import {value} from './b'; value();\n")
    context = await esmApi.createLintContext(options)
    await context.updateDocument({ filename: 'b.ts', version: 1, text: 'export const value = () => 1;\n' })
    const checked = await context.check()
    assert.equal(checked.result.errorCount, 1)
    assert.deepEqual(checked.result.files[0].diagnostics.map(d => d.code), ['style/eol-last'])
    const catalog = (await esmApi.lint({ listRules: true })).catalog.rules.find(rule => rule.id === 'ts/no-unsafe-call')
    assert.equal(catalog.analysis, 'type-information')
    assert.equal(catalog.defaultLevel, 'off')
    assert.equal(catalog.fixable, false)
    await context.close()
    context = undefined
    await writeFile(join(root, 'a.ts'), "// 😀\nimport {value} from './b'; const text = `${value}`;\n")
    const templates = await esmApi.lint({ ...options, rules: { 'ts/restrict-template-expressions': { level: 'error', options: { allow_any: false } } } })
    assert.equal(templates.errorCount, 1)
    assert.equal(templates.files[0].diagnostics[0].code, 'ts/restrict-template-expressions')
    assert.equal(templates.files[0].diagnostics[0].location.line, 2)
  } finally {
    await context?.close()
    await rm(root, { recursive: true, force: true })
  }
})

test('native lint globals share configuration, normalized cache and context validation', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-native-lint-globals-'))
  try {
    await writeFile(join(root, 'a.js'), 'Promise; injected;')
    await writeFile(join(root, 'wake.config.toml'), '[lint.globals]\ninjected="readonly"')
    const options = { root, globals: { Promise: 'off', injected: 'writable' } }
    const explained = await esmApi.lint({ ...options, printConfig: 'a.js' })
    assert.deepEqual(explained.config.globals.Promise, { mode: 'off', source: 'request' })
    assert.deepEqual(explained.config.globals.injected, { mode: 'writable', source: 'request' })
    const cold = await commonjs.lint({ root, cache: true })
    assert.equal(cold.cache.misses, 1)
    assert.equal((await esmApi.lint({ root, cache: true, globals: { Promise: 'readonly', absent: 'off' } })).cache.hits, 1)
    assert.equal((await esmApi.lint({ ...options, cache: true })).cache.misses, 1)
    const context = await esmApi.createLintContext({ ...options, cache: true })
    try { assert.equal((await context.check()).result.cache.hits, 1) } finally { await context.close() }
    for (const globals of [{ bad: true }, { 'x.y': 'off' }, { bad: 'write' }]) {
      await assert.rejects(esmApi.lint({ root, globals }), { code: 'WAKE_LINT_CONFIG' })
      await assert.rejects(esmApi.createLintContext({ root, globals }), { code: 'WAKE_LINT_CONFIG' })
    }
    await assert.rejects(esmApi.lint({ listRules: true, globals: { injected: 'readonly' } }), { code: 'WAKE_LINT_CONFIG' })
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('native lint environments provide versioned browser and Node globals', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-native-lint-environments-'))
  try {
    await writeFile(join(root, 'a.js'), 'window; process;')
    const options = { root, environments: ['browser', 'node'], rules: { 'js/no-undef': 'error' } }
    const result = await esmApi.lint(options)
    assert.equal(result.errorCount, 0)
    const explained = await commonjs.lint({ ...options, printConfig: 'a.js' })
    assert.deepEqual(explained.config.environments, ['browser', 'node'])
    assert.equal(explained.config.globals.window.source, 'request:environment:browser@1')
    assert.equal(explained.config.globals.process.source, 'request:environment:node@1')
    await assert.rejects(esmApi.lint({ root, environments: ['deno'] }), { code: 'WAKE_LINT_CONFIG' })
    await assert.rejects(esmApi.createLintContext({ root, environments: ['deno'] }), { code: 'WAKE_LINT_CONFIG' })
    await assert.rejects(esmApi.lint({ listRules: true, environments: ['browser'] }), { code: 'WAKE_LINT_CONFIG' })
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('native lint undefined values and JSX roots use globals across disk and overlays', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-native-undefined-'))
  const rules = { 'js/no-undef': 'error', 'react/jsx-no-undef': 'error' }
  try {
    await writeFile(join(root, 'a.tsx'), 'const node=<External value={injected}/>;')
    const cold = await esmApi.lint({ root, rules, cache: true })
    assert.deepEqual(cold.files[0].diagnostics.map(d => d.code), ['react/jsx-no-undef', 'js/no-undef'])
    const globals = { External: 'readonly', injected: 'writable' }
    const context = await esmApi.createLintContext({ root, rules, globals, cache: true })
    try {
      const clean = await context.check()
      assert.equal(clean.result.errorCount, 0)
      assert.equal(clean.result.cache.misses, 1)
      assert.equal((await context.check()).result.cache.hits, 1)
      context.updateDocument({ filename: 'a.tsx', version: 1, text: 'const node=<External/>; missing;' })
      const overlay = await context.check()
      assert.equal(overlay.result.cache.bypassed, 1)
      assert.deepEqual(overlay.result.files[0].diagnostics.map(d => d.code), ['js/no-undef'])
    } finally { await context.close() }
    const stdin = await commonjs.lint({ root, rules, globals, stdin: { filename: 'virtual.ts', text: 'typeof optional; typeof object.member;' } })
    assert.equal(stdin.errorCount, 1)
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('native lint prefers const with safe publication and Unicode identifier identity', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-native-const-'))
  try {
    const source = 'let a\u0301 = 1; let changed = 1; changed++; a\\u0301;'
    await writeFile(join(root, 'a.ts'), source)
    const rules = { 'js/prefer-const': 'warn', 'js/no-undef': 'error' }
    const preview = await esmApi.lint({ root, rules, fix: 'dry-run' })
    assert.equal(preview.files[0].changed, true)
    assert.equal(preview.files[0].written, false)
    assert.equal(await readFile(join(root, 'a.ts'), 'utf8'), source)
    assert.equal(preview.files[0].output, source.replace('let a', 'const a'))
    assert.equal(preview.errorCount, 0)
    assert.equal(preview.warningCount, 0)
    const published = await commonjs.lint({ root, rules, fix: 'write' })
    assert.equal(published.files[0].written, true)
    assert.equal(await readFile(join(root, 'a.ts'), 'utf8'), preview.files[0].output)
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('native lint unused binding filters and JSX usage are shared by CJS ESM and contexts', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-native-unused-'))
  try {
    await writeFile(join(root, 'a.tsx'), "import { Component, Other } from 'ui'; export const node=<Component/>; const _ignored=1;")
    const rules = { 'js/no-unused-vars': { level: 'error', options: { vars_ignore_pattern: '^_' } } }
    const cold = await esmApi.lint({ root, rules, cache: true })
    assert.equal(cold.errorCount, 2)
    const jsxRules = { ...rules, 'react/jsx-uses-vars': 'warn' }
    const warm = await commonjs.lint({ root, rules: jsxRules, cache: true })
    assert.equal(warm.errorCount, 1)
    assert.equal(warm.cache.misses, 1)
    assert.equal(warm.files[0].diagnostics[0].messageId, 'unused')
    const context = await esmApi.createLintContext({ root, rules: jsxRules, cache: true })
    try {
      assert.equal((await context.check()).result.cache.hits, 1)
      context.updateDocument({ filename: 'a.tsx', version: 1, text: 'export type Used=string; type Unused=number;' })
      assert.equal((await context.check()).result.errorCount, 1)
    } finally { await context.close() }
    const catalog = await esmApi.lint({ listRules: true })
    const rule = catalog.catalog.rules.find(rule => rule.id === 'js/no-unused-vars')
    assert.equal(rule.optionsSchema.properties.vars_ignore_pattern.format, 'rust-regex')
    await assert.rejects(esmApi.lint({ root, rules: { 'js/no-unused-vars': { level: 'off', options: { vars_ignore_pattern: '[' } } } }), { code: 'WAKE_LINT_CONFIG' })
    await writeFile(join(root, 'wake.config.toml'), "[[lint.overrides]]\nfiles=['never/**']\nrules={'js/no-unused-vars'={level='off',options={args_ignore_pattern='['}}}")
    await assert.rejects(esmApi.lint({ root }), { code: 'WAKE_LINT_CONFIG' })
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('native lint semicolons preserve grammar across cache preview publication and overlays', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-native-semi-'))
  const rules = { 'style/semi': 'warn' }
  const source = 'const 名=1 // keep\nfunction f(){return 名}'
  try {
    await writeFile(join(root, 'a.ts'), source)
    const cold = await esmApi.lint({ root, rules, cache: true })
    assert.equal(cold.warningCount, 2)
    assert.equal((await commonjs.lint({ root, rules, cache: true })).cache.hits, 1)
    const preview = await esmApi.lint({ root, rules, fix: 'dry-run' })
    assert.equal(preview.files[0].output, 'const 名=1; // keep\nfunction f(){return 名;}')
    assert.equal(await readFile(join(root, 'a.ts'), 'utf8'), source)
    const published = await commonjs.lint({ root, rules, fix: 'write' })
    assert.equal(published.files[0].written, true)
    assert.equal(await readFile(join(root, 'a.ts'), 'utf8'), preview.files[0].output)
    const context = await esmApi.createLintContext({ root, rules: { 'style/semi': { level: 'warn', options: { mode: 'never' } } }, cache: true })
    try {
      context.updateDocument({ filename: 'a.ts', version: 1, text: 'value;\n(call)()' })
      const checked = await context.check()
      assert.equal(checked.result.warningCount, 0)
      assert.equal(checked.result.cache.bypassed, 1)
    } finally { await context.close() }
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('native lint trailing commas preserve TSX holes and typed lists through publication', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-native-comma-'))
  const rules = { 'style/comma-dangle': { level: 'warn', options: { mode: 'always' } } }
  const source = 'const f=<T,>(x:T)=>x; const a=[1 /*keep*/]; const holes=[1,,];'
  try {
    await writeFile(join(root, 'a.tsx'), source)
    const cold = await commonjs.lint({ root, rules, cache: true })
    assert.equal(cold.warningCount, 2)
    assert.equal((await esmApi.lint({ root, rules, cache: true })).cache.hits, 1)
    const preview = await esmApi.lint({ root, rules, fix: 'dry-run' })
    assert.equal(preview.files[0].output, 'const f=<T,>(x:T,)=>x; const a=[1, /*keep*/]; const holes=[1,,];')
    assert.equal(await readFile(join(root, 'a.tsx'), 'utf8'), source)
    assert.equal((await commonjs.lint({ root, rules, fix: 'write' })).files[0].written, true)
    const never = { 'style/comma-dangle': 'warn' }
    const reversed = await esmApi.lint({ root, rules: never, fix: 'write' })
    assert.equal(reversed.files[0].output, source)
    assert.equal(await readFile(join(root, 'a.tsx'), 'utf8'), source)
    const catalog = await esmApi.lint({ listRules: true })
    assert.deepEqual(catalog.catalog.rules.find(rule => rule.id === 'style/comma-dangle').optionsSchema.properties.mode.enum, ['never', 'always', 'always-multiline', 'only-multiline'])
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('native lint indentation validates integer options and preserves raw JSX attributes', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-native-indent-'))
  const source = 'function f(){\nconst view=<div title="first\n raw"/>;\n}'
  const rules = { 'style/indent': { level: 'warn', options: { width: 4 } } }
  try {
    await writeFile(join(root, 'a.tsx'), source)
    const cold = await commonjs.lint({ root, rules, cache: true })
    assert.equal(cold.warningCount, 1)
    assert.equal((await esmApi.lint({ root, rules, cache: true })).cache.hits, 1)
    const preview = await esmApi.lint({ root, rules, fix: 'dry-run' })
    assert.equal(preview.files[0].output, source.replace('\nconst', '\n    const'))
    assert.equal(await readFile(join(root, 'a.tsx'), 'utf8'), source)
    assert.equal((await commonjs.lint({ root, rules, fix: 'write' })).files[0].written, true)
    const context = await esmApi.createLintContext({ root, rules })
    try {
      context.updateDocument({ filename: 'a.tsx', version: 1, text: 'function f(){\ncall();\n}' })
      assert.equal((await context.check()).result.warningCount, 1)
    } finally { await context.close() }
    const catalog = await esmApi.lint({ listRules: true })
    assert.deepEqual(catalog.catalog.rules.find(rule => rule.id === 'style/indent').optionsSchema.properties.width, { type: 'integer', default: 2, minimum: 1, maximum: 8 })
    await writeFile(join(root, 'wake.config.toml'), "[[lint.overrides]]\nfiles=['never/**']\nrules={'style/indent'={level='off',options={style='tabs',width=0}}}")
    await assert.rejects(esmApi.lint({ root }), { code: 'WAKE_LINT_CONFIG' })
  } finally { await rm(root, { recursive: true, force: true }) }
})

async function assertSameExistingPath(actual, expected) {
  const [actualStats, expectedStats] = await Promise.all([
    stat(actual, { bigint: true }),
    stat(expected, { bigint: true }),
  ])
  assert.equal(actualStats.dev, expectedStats.dev)
  assert.equal(actualStats.ino, expectedStats.ino)
}

function loadBuiltNative() {
  const nativeSuffixes = {
    'win32-x64': 'win32-x64-msvc',
    'linux-x64': 'linux-x64-gnu',
    'linux-arm64': 'linux-arm64-gnu',
    'darwin-x64': 'darwin-x64',
    'darwin-arm64': 'darwin-arm64',
  }
  const suffix = nativeSuffixes[`${process.platform}-${process.arch}`]
  assert.ok(suffix, 'the native ABI gate only runs on a supported release target')
  return require(join(dirname(fileURLToPath(import.meta.url)), '..', `wake.${suffix}.node`))
}

async function withNativeFactory(name, factory, create) {
  const rawNative = loadBuiltNative()
  const original = rawNative[name]
  rawNative[name] = factory
  try {
    return await create()
  } finally {
    rawNative[name] = original
  }
}

after(async () => {
  await Promise.all(contexts.map((context) => context.close()))
})

test('native lint explains virtual configuration and applies request parameters', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-lint-config-'))
  try {
    await writeFile(join(root, 'wake.config.toml'), '[lint]\npresets=["react@1"]')
    const explained = await esmApi.lint({ root, printConfig: 'src/virtual.tsx',
      rules: { 'js/eqeqeq': { level: 'error', options: { allow_null: true } } } })
    assert.equal(explained.config.schema, 'wake.lint.config.v1')
    assert.equal(explained.config.rules['js/eqeqeq'].source, 'request')
    assert.equal(explained.config.rules['js/eqeqeq'].options.allow_null, true)
    assert.equal(explained.config.rules['react/no-danger'].source, 'preset:react@1')
    assert.equal(explained.files.length, 0)
    await assert.rejects(access(join(root, 'src')), { code: 'ENOENT' })
    const result = await esmApi.lint({ root, stdin: { filename: 'x.js', text: 'x == null;' },
      rules: { 'js/eqeqeq': { level: 'error', options: { allow_null: true } } } })
    assert.equal(result.errorCount, 0)
    assert.equal(result.warningCount, 0)
    for (const options of [
      { printConfig: true }, { rules: null }, { rules: [] },
      { printConfig: 'x.js', fix: 'dry-run' },
      { rules: { 'js/eqeqeq': { level: 'off', options: { allow_null: 'true' } } } },
      { rules: { 'js/eqeqeq': { level: 'off', typo: true } } },
    ]) await assert.rejects(esmApi.lint({ root, ...options }), { code: 'WAKE_LINT_CONFIG' })
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('native lint exposes one project-independent rule catalog', async () => {
  const result = await esmApi.lint({ listRules: true, root: join(tmpdir(), 'missing-wake-lint-catalog') })
  assert.equal(result.catalog.schema, 'wake.lint.rules.v1')
  assert.deepEqual(result.files, [])
  const rule = result.catalog.rules.find((rule) => rule.id === 'style/quotes')
  assert.equal(rule.defaultLevel, 'off')
  assert.equal(rule.fixable, true)
  assert.deepEqual(rule.optionsSchema.properties.quote.enum, ['single', 'double'])
  const binary = result.catalog.rules.find((rule) => rule.id === 'js/no-constant-binary-expression')
  assert.equal(binary.defaultLevel, 'off')
  assert.equal(binary.analysis, 'syntax')
  assert.equal(binary.fixable, false)
  const array = result.catalog.rules.find((rule) => rule.id === 'ts/array-type')
  assert.equal(array.defaultLevel, 'off')
  assert.deepEqual(array.languages, ['ts', 'tsx'])
  assert.deepEqual(array.optionsSchema.properties.syntax.enum, ['array', 'generic'])
  const arrayResult = await esmApi.lint({ root: fileURLToPath(new URL('../../../', import.meta.url)), stdin: { filename: 'virtual.ts', text: 'type X=Array<string>;' }, rules: { 'ts/array-type': 'error' } })
  assert.deepEqual(arrayResult.files[0].diagnostics.filter((diagnostic) => diagnostic.code === array.id).map((diagnostic) => diagnostic.messageId), ['array'])
  const checked = await esmApi.lint({ root: fileURLToPath(new URL('../../../', import.meta.url)), stdin: { filename: 'virtual.ts', text: '(x < max) ?? 10; [] === value;' }, rules: { 'js/no-constant-binary-expression': 'error' } })
  assert.deepEqual(checked.files[0].diagnostics.filter((diagnostic) => diagnostic.code === binary.id).map((diagnostic) => diagnostic.messageId), ['constant-nullish', 'constant-comparison'])
  assert.deepEqual((await commonjs.lint({ listRules: true })).catalog, result.catalog)
  for (const options of [{ listRules: 'true' }, { listRules: true, paths: ['x.js'] }, { listRules: true, printConfig: 'x.js' }]) {
    await assert.rejects(esmApi.lint(options), { code: 'WAKE_LINT_CONFIG' })
  }
})

test('native lint checks Hooks identities and propagates analysis limits', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-lint-hooks-'))
  const rules = { 'react-hooks/rules-of-hooks': 'error' }
  try {
    const catalog = (await esmApi.lint({ listRules: true })).catalog
    assert.equal(catalog.rules.find((r) => r.id === 'react-hooks/rules-of-hooks').analysis, 'scope-control-flow')
    const text = "import {useState as h} from 'react'; function App(x) { if(x) h(0); }"
    const options = { root, stdin: { filename: 'app.tsx', text }, rules }
    const result = await esmApi.lint(options)
    assert.deepEqual(result.files[0].diagnostics.map((d) => d.messageId), ['conditional'])
    assert.deepEqual(await commonjs.lint(options), result)
    for (const fix of [undefined, 'dry-run']) {
      await assert.rejects(esmApi.lint({ ...options, fix, stdin: { filename: 'app.tsx', text: `function App() { ${'ordinary();'.repeat(60000)} }` } }), { code: 'WAKE_LINT_ANALYSIS' })
    }
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('native lint checks original Hook dependencies and custom effect aliases', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-lint-dependencies-'))
  try {
    const rules = { 'react-hooks/exhaustive-deps': { level: 'error', options: { additional_effect_hooks: '^useObserve$' } } }
    const text = "import {useEffect, useState} from 'react'; import {useObserve as observe} from 'store'; function App(props) { const [value,set] = useState(0); useEffect(() => { set(1); consume(props.name); }, []); observe(() => consume(value), []); }"
    const options = { root, rules, stdin: { filename: 'app.tsx', text } }
    const result = await esmApi.lint(options)
    assert.equal(result.errorCount, 2)
    assert.deepEqual(result.files[0].diagnostics.map((d) => d.messageId), ['missing', 'missing'])
    assert.deepEqual(await commonjs.lint(options), result)
    const context = await esmApi.createLintContext({ root, rules })
    try {
      await context.updateDocument({ filename: 'app.tsx', version: 1, text })
      assert.equal((await context.check()).result.errorCount, 2)
      await context.updateDocument({ filename: 'app.tsx', version: 2, text: "import {useEffect} from 'react'; function App(p) { useEffect(() => consume(p), [p]); }" })
      assert.equal((await context.check()).result.errorCount, 0)
    } finally { await context.close() }
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('native lint exposes the accessibility catalog and checks original JSX values', async () => {
  const catalog = (await esmApi.lint({ listRules: true })).catalog.rules.filter((rule) => rule.id.startsWith('a11y/'))
  assert.equal(catalog.length, 9)
  for (const rule of catalog) {
    assert.equal(rule.defaultLevel, 'off')
    assert.equal(rule.fixable, false)
    assert.deepEqual(rule.languages, ['jsx', 'tsx'])
  }
  const result = await esmApi.lint({
    root: fileURLToPath(new URL('../../../', import.meta.url)),
    stdin: { filename: 'virtual.tsx', text: 'const el=<><img/><a/><label/><div aria-labl="x" aria-hidden="bad" role="unknown"/><div role="button" onClick={run}/></>;' },
    rules: Object.fromEntries(catalog.map((rule) => [rule.id, 'error'])),
  })
  assert.deepEqual([...new Set(result.files[0].diagnostics.map((diagnostic) => diagnostic.code))].sort(), catalog.map((rule) => rule.id).sort())
  assert.equal(result.errorCount, 10)
  assert.equal(result.exitCode, 1)
})

test('native lint checks JSX list keys and callback index bindings', async () => {
  const catalog = (await esmApi.lint({ listRules: true })).catalog.rules
  for (const id of ['react/jsx-key', 'react/no-array-index-key']) {
    const rule = catalog.find((rule) => rule.id === id)
    assert.equal(rule.defaultLevel, 'off')
    assert.equal(rule.fixable, false)
    assert.deepEqual(rule.languages, ['jsx', 'tsx'])
  }
  const result = await esmApi.lint({
    root: fileURLToPath(new URL('../../../', import.meta.url)),
    stdin: { filename: 'lists.tsx', text: 'const items=[<Row/>]; items.map((item,index)=><Row key={index}/>);' },
    rules: { 'react/jsx-key': 'error', 'react/no-array-index-key': 'error' },
  })
  assert.deepEqual(result.files[0].diagnostics.map((diagnostic) => diagnostic.code), ['react/jsx-key', 'react/no-array-index-key'])
  assert.equal(result.errorCount, 2)
})

test('native lint uses lexical block, static block and parameter environments', async () => {
  const text = "{ function console() {} console.log('local'); } console.log('global'); class C { static { var Promise; new Promise(async () => {}); } } new Promise(async () => {}); function check(console = () => 0) { var console; console.log('local'); }"
  const result = await esmApi.lint({
    root: fileURLToPath(new URL('../../../', import.meta.url)),
    stdin: { filename: 'scopes.mjs', text },
    rules: { 'js/no-console': 'error', 'js/no-async-promise-executor': 'error' },
  })
  assert.deepEqual(result.files[0].diagnostics.filter((diagnostic) =>
    diagnostic.code === 'js/no-console' || diagnostic.code === 'js/no-async-promise-executor'
  ).map((diagnostic) => diagnostic.code), ['js/no-console', 'js/no-async-promise-executor'])
})

test('native lint distinguishes original type imports from ambient and generic bindings', async () => {
  const id = 'ts/consistent-type-imports'
  const rule = (await esmApi.lint({ listRules: true })).catalog.rules.find((rule) => rule.id === id)
  assert.ok(rule)
  assert.equal(rule.defaultLevel, 'off')
  assert.equal(rule.analysis, 'scope')
  assert.equal(rule.fixable, false)
  const text = "import { Used, Shared, Generic, Runtime } from 'pkg'; let used: Used; type Local<Generic> = Generic; declare namespace N { interface Shared {} } declare namespace N { const value: Shared; } const value = Runtime;"
  const result = await esmApi.lint({
    root: fileURLToPath(new URL('../../../', import.meta.url)),
    stdin: { filename: 'type-imports.ts', text }, rules: { [id]: 'error' },
  })
  assert.equal(result.errorCount, 1)
  assert.deepEqual(result.files[0].diagnostics.map((diagnostic) => [diagnostic.code, diagnostic.messageId]), [[id, 'type']])
  const exports = await esmApi.lint({
    root: fileURLToPath(new URL('../../../', import.meta.url)),
    stdin: { filename: 'type-exports.ts', text: 'interface Shape {} declare const Dual: number; type Dual = number; export { Shape, Dual };' },
    rules: { 'ts/consistent-type-exports': 'error' },
  })
  assert.equal(exports.errorCount, 1)
  assert.deepEqual(exports.files[0].diagnostics.map((diagnostic) => diagnostic.code), ['ts/consistent-type-exports'])
})

test('native lint checks declaration identity, ordering and shadow options', async () => {
  const result = await esmApi.lint({
    root: fileURLToPath(new URL('../../../', import.meta.url)),
    stdin: { filename: 'binding-rules.mjs', text: 'var repeated; var repeated; let outer; {let outer;} later; let later;' },
    rules: {
      'js/no-redeclare': 'error',
      'js/no-shadow': { level: 'error', options: { hoist: false } },
      'js/no-use-before-define': { level: 'error', options: { functions: false } },
    },
  })
  assert.equal(result.errorCount, 3)
  assert.deepEqual(result.files[0].diagnostics.map((diagnostic) => diagnostic.code), ['js/no-redeclare', 'js/no-shadow', 'js/no-use-before-define'])
})

test('native lint context owns document versions, cancellation and disposal', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-lint-context-api-'))
  let context
  try {
    await writeFile(join(root, 'a.js'), 'debugger;')
    context = await esmApi.createLintContext({ root })
    assert.ok(context instanceof commonjs.LintContext)
    context.updateDocument({ filename: 'a.js', version: 1, text: 'run();' })
    context.updateDocument({ filename: 'new.ts', version: 2, text: 'debugger;' })
    const first = await context.check()
    assert.deepEqual(first.documents, { 'a.js': 1, 'new.ts': 2 })
    assert.equal(first.result.errorCount, 1)
    assert.equal(first.generation, context.generation)
    assert.throws(() => context.updateDocument({ filename: 'new.ts', version: 2, text: '' }), { code: 'WAKE_LINT_CONFIG' })
    const stale = context.check()
    const rejected = assert.rejects(stale, { code: 'WAKE_CANCELLED' })
    context.updateDocument({ filename: 'new.ts', version: 3, text: 'run();' })
    await rejected
    assert.equal((await context.check()).result.errorCount, 0)
    context.closeDocument('a.js', 2)
    assert.equal((await context.check()).result.errorCount, 1)
    const before = context.generation
    assert.ok(context.invalidate() > before)
    const controller = new AbortController()
    controller.abort()
    await assert.rejects(context.check({ signal: controller.signal }), { code: 'WAKE_CANCELLED' })
    await assert.rejects(context.check({ typo: true }), { code: 'WAKE_LINT_CONFIG' })
    await context[Symbol.asyncDispose]()
    await context.close()
    assert.equal(context.closed, true)
    await assert.rejects(context.check())
    assert.throws(() => context.updateDocument({ filename: 'a.js', version: 3, text: '' }))
    for (const options of [{ fix: 'write' }, { stdin: { filename: 'a.js', text: '' } }, { listRules: true }, { baseline: { path: 'x.json', mode: 'generate' } }]) {
      await assert.rejects(esmApi.createLintContext({ root, ...options }), { code: 'WAKE_LINT_CONFIG' })
    }
    assert.throws(() => new commonjs.LintContext(), /createLintContext/)
  } finally { await context?.close(); await rm(root, { recursive: true, force: true }) }
})

test('native lint cancellation stops active checks and rejects invalid signals before starting', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-lint-cancel-api-'))
  const context = await esmApi.createLintContext({ root })
  try {
    context.updateDocument({ filename: 'a.js', version: 1, text: 'debugger;\n'.repeat(30000) })
    const controller = new AbortController()
    const pending = context.check({ signal: controller.signal })
    const rejection = assert.rejects(pending, { code: 'WAKE_CANCELLED' })
    controller.abort()
    await rejection
    context.updateDocument({ filename: 'a.js', version: 2, text: 'run();' })
    assert.equal((await context.check()).result.errorCount, 0)
    const once = new AbortController()
    const checking = esmApi.lint({ root, stdin: { filename: 'large.js', text: 'debugger;\n'.repeat(30000) }, signal: once.signal })
    const cancelled = assert.rejects(checking, { code: 'WAKE_CANCELLED' })
    once.abort()
    await cancelled
    for (const signal of [{}, 1, { aborted: false }]) {
      await assert.rejects(context.check({ signal }), { code: 'WAKE_LINT_CONFIG' })
      await assert.rejects(esmApi.lint({ root, signal }), { code: 'WAKE_LINT_CONFIG' })
    }
  } finally { await context.close(); await rm(root, { recursive: true, force: true }) }
})

test('native lint context cleanup cancels work when its Worker is terminated', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-lint-context-worker-'))
  const worker = new Worker(`
    const { parentPort, workerData } = require('node:worker_threads')
    parentPort.on('message', () => {})
    ;(async () => {
      const wake = require(workerData.api)
      globalThis.context = await wake.createLintContext({ root: workerData.root, watch: true })
      context.updateDocument({ filename: 'open.js', version: 1, text: 'debugger;\\n'.repeat(30000) })
      globalThis.pending = context.check().catch(() => {})
      parentPort.postMessage('admitted')
    })().catch((error) => { throw error })
  `, { eval: true, workerData: { root, api: fileURLToPath(new URL('../index.cjs', import.meta.url)) } })
  try {
    assert.deepEqual(await once(worker, 'message'), ['admitted'])
    assert.equal(await worker.terminate(), 1)
    const context = await commonjs.createLintContext({ root })
    assert.equal((await context.check()).result.files.length, 0)
    await context.close()
  } finally { await worker.terminate(); await rm(root, { recursive: true, force: true }) }
})

test('native lint one-shot cleanup survives Worker termination during analysis', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-lint-operation-worker-'))
  const worker = new Worker(`
    const { parentPort, workerData } = require('node:worker_threads')
    parentPort.on('message', () => {})
    const wake = require(workerData.api)
    globalThis.pending = wake.lint({ root: workerData.root, stdin: { filename: 'large.js', text: 'debugger;\\n'.repeat(30000) } }).catch(() => {})
    parentPort.postMessage('admitted')
  `, { eval: true, workerData: { root, api: fileURLToPath(new URL('../index.cjs', import.meta.url)) } })
  try {
    assert.deepEqual(await once(worker, 'message'), ['admitted'])
    assert.equal(await worker.terminate(), 1)
  } finally { await worker.terminate(); await rm(root, { recursive: true, force: true }) }
})

test('native lint watch recovers invalid configuration and preserves context after stopping', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-lint-watch-api-'))
  let context
  try {
    await writeFile(join(root, 'wake.config.toml'), '[lint\n')
    context = await esmApi.createLintContext({ root, watch: true })
    assert.equal(context.watching, true)
    const event = (name, accepts = () => true) => new Promise((resolve, reject) => {
      const timeout = setTimeout(() => { context.off(name, listener); reject(new Error(`missing lint ${name}`)) }, 10000)
      const listener = (value) => { if (accepts(value)) { clearTimeout(timeout); context.off(name, listener); resolve(value) } }
      context.on(name, listener)
    })
    const invalid = await event('diagnostic')
    assert.equal(invalid.error.code, 'WAKE_LINT_CONFIG')
    const recovered = event('checked')
    await writeFile(join(root, 'wake.config.toml'), '[lint]\n')
    const first = await recovered
    assert.equal(first.generation, context.generation)
    const next = event('checked', (snapshot) => snapshot.result.errorCount === 1)
    context.updateDocument({ filename: 'new.ts', version: 1, text: 'debugger;' })
    assert.equal((await next).documents['new.ts'], 1)
    await Promise.all([context.stopWatch(), context.stopWatch()])
    assert.equal(context.watching, false)
    assert.equal(context.closed, false)
    assert.equal((await context.check()).result.errorCount, 1)
    const resumed = event('checked')
    assert.equal(context.startWatch(), context)
    assert.equal(context.startWatch(), context)
    await resumed
    const closed = once(context, 'closed')
    await context.close()
    await closed
    assert.equal(context.watching, false)
  } finally { await context?.close(); await rm(root, { recursive: true, force: true }) }
})

test('native lint context rejects root redirection through a replacement directory link', async () => {
  const parent = await mkdtemp(join(tmpdir(), 'wake-lint-root-identity-'))
  const root = join(parent, 'project')
  const target = join(parent, 'different-project')
  let context
  try {
    await mkdir(root)
    await mkdir(target)
    await writeFile(join(target, 'private.js'), 'debugger;')
    context = await esmApi.createLintContext({ root })
    await rm(root, { recursive: true })
    await symlink(target, root, process.platform === 'win32' ? 'junction' : 'dir')
    await assert.rejects(context.check(), { code: 'WAKE_LINT_IO' })
    assert.throws(() => context.startWatch(), { code: 'WAKE_LINT_IO' })
  } finally { await context?.close(); await rm(parent, { recursive: true, force: true }) }
})

test('native lint baseline generates, checks, prunes and validates its closed request', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-lint-baseline-api-'))
  try {
    await writeFile(join(root, 'a.js'), 'debugger;\n')
    const baseline = { path: 'lint-baseline.json', mode: 'generate' }
    const generated = await esmApi.lint({ root, baseline })
    assert.equal(generated.exitCode, 0)
    assert.equal(generated.baseline.written, true)
    assert.equal((await commonjs.lint({ root, baseline: { path: baseline.path } })).baseline.suppressed, 1)
    await writeFile(join(root, 'a.js'), 'run();\n')
    const pruned = await esmApi.lint({ root, baseline: { ...baseline, mode: 'prune' } })
    assert.equal(pruned.baseline.stale, 1)
    assert.equal(pruned.baseline.entries, 0)
    for (const baseline of [{}, { path: 'x.json', mode: 'guess' }, { path: 'x.json', force: true }, 'x.json']) {
      await assert.rejects(esmApi.lint({ root, baseline }), { code: 'WAKE_LINT_CONFIG' })
    }
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('native lint cache preserves diagnostics and bypasses fixes', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-lint-cache-api-'))
  try {
    await writeFile(join(root, 'a.js'), 'debugger;')
    const cold = await esmApi.lint({ root, cache: true })
    assert.equal(cold.cache.writes, 1)
    const warm = await commonjs.lint({ root, cache: true })
    assert.equal(warm.cache.hits, 1)
    assert.deepEqual(cold.files, warm.files)
    assert.equal((await esmApi.lint({ root, cache: true, fix: 'dry-run' })).cache.bypassed, 1)
    for (const options of [{ cache: 'true' }, { cache: null }, { cache: true, printConfig: 'x.js' }, { cache: true, listRules: true }]) {
      await assert.rejects(esmApi.lint({ root, ...options }), { code: 'WAKE_LINT_CONFIG' })
    }
  } finally { await rm(root, { recursive: true, force: true }) }
})

test('native lint checks unsaved snapshots and rejects malformed requests', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-lint-api-'))
  try {
    assert.equal(typeof esmApi.lint, 'function')
    const result = await esmApi.lint({ root, stdin: { filename: 'unsaved.ts', text: '// 😀\r\ndebugger;' } })
    assert.equal(result.errorCount, 1)
    assert.equal(result.exitCode, 1)
    assert.equal(result.files[0].diagnostics[0].location.line, 2)
    for (const options of [{ root, typo: true }, { root, maxWarnings: -1 }, { root, stdin: { filename: 'a.ts' } }, { root, stdin: null }]) {
      await assert.rejects(esmApi.lint(options), { code: 'WAKE_LINT_CONFIG' })
    }
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})

test('native lint fix modes return final source and publish only when requested', async () => {
  const root = await mkdtemp(join(tmpdir(), 'wake-lint-fix-api-'))
  try {
    await writeFile(join(root, 'wake.config.toml'), "[lint.rules]\n'style/eol-last'='error'")
    await writeFile(join(root, 'a.js'), 'run();')
    const checked = await esmApi.lint({ root })
    assert.equal(checked.files[0].diagnostics[0].messageId, 'missing')
    assert.deepEqual(checked.files[0].diagnostics[0].fix.edits, [{ start: 6, end: 6, text: '\n' }])
    const preview = await esmApi.lint({ root, fix: 'dry-run' })
    assert.equal(preview.exitCode, 0)
    assert.equal(preview.files[0].output, 'run();\n')
    assert.equal(preview.files[0].written, false)
    assert.equal(await readFile(join(root, 'a.js'), 'utf8'), 'run();')
    const result = await esmApi.lint({ root, fix: 'write' })
    assert.equal(result.files[0].written, true)
    assert.equal(await readFile(join(root, 'a.js'), 'utf8'), 'run();\n')
    for (const fix of [true, 'yes', null]) await assert.rejects(esmApi.lint({ root, fix }), { code: 'WAKE_LINT_CONFIG' })
    await assert.rejects(esmApi.lint({ root, fix: 'write', stdin: { filename: 'x.js', text: 'x;' } }), { code: 'WAKE_LINT_CONFIG' })
  } finally {
    await rm(root, { recursive: true, force: true })
  }
})

test('loads the same API from ESM and CommonJS', () => {
  const expectedRuntimeExports = [
    'BuildContext',
    'LintContext',
    'DevServer',
    'TestContext',
    'WakeError',
    'build',
    'buildDocs',
    'buildLibrary',
    'bundle',
    'createBuildContext',
    'createLintContext',
    'createTestContext',
    'generateCssToken',
    'generateDocgen',
    'generateFederationLock',
    'initializeFederation',
    'lint',
    'runTests',
    'startDevServer',
    'startDocsDevServer',
    'version',
  ].sort()
  assert.deepEqual(Object.keys(commonjs).sort(), expectedRuntimeExports)
  assert.deepEqual(Object.keys(esmApi).sort(), [...expectedRuntimeExports, 'default'].sort())
  assert.equal(version(), packageVersion)
  assert.equal(commonjs.version(), version())
  assert.equal(typeof commonjs.build, 'function')
  assert.equal(typeof commonjs.buildLibrary, 'function')
  assert.equal(typeof commonjs.generateCssToken, 'function')
  assert.equal(typeof commonjs.generateDocgen, 'function')
  assert.equal(typeof commonjs.initializeFederation, 'function')
  assert.equal(typeof commonjs.generateFederationLock, 'function')
  assert.equal(typeof commonjs.TestContext.prototype.startWatch, 'function')
  assert.equal(typeof commonjs.TestContext.prototype.stopWatch, 'function')
  assert.equal('initTestConfig' in commonjs, false)
})

test('loads the exact freshly built native addon ABI', () => {
  const expectedNativeExports = [
    'NativeBuildContext',
    'NativeLintContext',
    'NativeLintTask',
    'NativeDevServer',
    'NativeParsedModule',
    'NativeTestContext',
    'build',
    'buildDocs',
    'buildLibrary',
    'bundle',
    'createBuildContext',
    'createLintContext',
    'createTestContext',
    'generateCssToken',
    'generateDocgen',
    'generateFederationLock',
    'initializeFederation',
    'lint',
    'parse',
    'runTests',
    'startDevServer',
    'startDocsDevServer',
    'tokenize',
    'version',
  ].sort()
  assert.deepEqual(Object.keys(loadBuiltNative()).sort(), expectedNativeExports)
})

test('shares Federation initialization and lock services across Node frontends', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'wake-node-federation-control-'))
  await writeFile(
    join(cwd, 'wake.config.toml'),
    "[federation]\nenabled = true\nname = 'shell'\n",
  )
  try {
    const first = await initializeFederation({ cwd })
    await assertSameExistingPath(first.projectRoot, cwd)
    assert.equal(first.declaration, 'created')
    assert.equal(first.typesIndex, 'created')
    assert.equal(
      await readFile(first.declarationPath, 'utf8'),
      '/// <reference path="./.wake/federation/types/index.d.ts" />\n',
    )
    const second = await commonjs.initializeFederation({ cwd })
    assert.equal(second.declaration, 'unchanged')
    assert.equal(second.typesIndex, 'unchanged')

    await assert.rejects(
      generateFederationLock({ cwd }),
      (error) => error instanceof WakeError && error.code === 'FED_CONFIG_INVALID',
    )
  } finally {
    await rm(cwd, { recursive: true, force: true })
  }
})

test('forwards native federationUpdated events through DevServer', async () => {
  const update = {
    type: 'federationUpdated',
    remote: 'catalog',
    oldBuildId: 'catalog-1',
    newBuildId: 'catalog-2',
    changedExposes: ['./Button'],
    typesHash: 'types-2',
    action: 'isolated-remount',
  }
  const nativeEvents = [update, { type: 'futureNativeEvent' }]
  const nativeServer = {
    url: 'http://127.0.0.1:5173/',
    eventsJson() {
      return JSON.stringify(nativeEvents.splice(0))
    },
    async close() {
      nativeEvents.push({ type: 'closed' })
      return JSON.stringify({ ok: true, value: null })
    },
  }
  const server = await withNativeFactory(
    'startDevServer',
    async () => nativeServer,
    () => commonjs.startDevServer(),
  )
  let timeout
  try {
    const [[event], [diagnostic]] = await Promise.race([
      Promise.all([once(server, 'federationUpdated'), once(server, 'diagnostic')]),
      new Promise((_, reject) => {
        timeout = setTimeout(() => reject(new Error('federationUpdated was not forwarded')), 2_000)
      }),
    ])
    assert.deepEqual(event, update)
    assert.equal(diagnostic.code, 'WAKE_INTERNAL')
    assert.match(diagnostic.message, /futureNativeEvent/)
  } finally {
    clearTimeout(timeout)
    await server.close()
  }
})

test('keeps the raw native test ABI on the persistent v3 context', () => {
  const rawNative = loadBuiltNative()
  assert.equal('initTestConfig' in rawNative, false)
  assert.equal(typeof rawNative.runTests, 'function')
  assert.equal(typeof rawNative.createTestContext, 'function')
  assert.equal(typeof rawNative.NativeTestContext, 'function')
  for (const method of ['run', 'startWatch', 'stopWatch', 'watchControl', 'eventsJson', 'close']) {
    assert.equal(typeof rawNative.NativeTestContext.prototype[method], 'function', method)
  }
})

test('rejects Node contract drift before starting product work', async () => {
  const rawNative = loadBuiltNative()
  const explicitFalseCause = new WakeError('WAKE_CONFIG', 'invalid', { cause: false })
  assert.equal(explicitFalseCause.cause, false)
  for (const [Context, factory] of [
    [commonjs.BuildContext, 'createBuildContext'],
    [commonjs.DevServer, 'startDevServer'],
    [commonjs.TestContext, 'createTestContext'],
  ]) {
    assert.throws(
      () => new Context(),
      (error) => error instanceof WakeError
        && error.code === 'WAKE_CONFIG'
        && error.message.includes(`${factory}()`),
    )
  }
  assert.deepEqual(splitOptions(undefined), [{}, undefined])
  assert.throws(
    () => splitOptions(null),
    (error) => error instanceof WakeError && error.code === 'WAKE_CONFIG',
  )
  assert.throws(
    () => splitOptions({ signal: null }),
    (error) => error instanceof WakeError
      && error.code === 'WAKE_CONFIG'
      && error.message.includes('/signal'),
  )
  await assert.rejects(
    build(null),
    (error) => error instanceof WakeError && error.code === 'WAKE_CONFIG',
  )
  await assert.rejects(
    runTests(null),
    (error) => error instanceof WakeError && error.code === 'WAKE_TEST_CONFIG',
  )
  assert.throws(() => rawNative.createBuildContext('null'))
  for (const federation of [
    null,
    { enabled: true },
    { enabled: false, name: 'shell' },
    { enabled: true, name: 'shell', typo: true },
    {
      enabled: true,
      name: 'shell',
      remotes: { catalog: { manifest_url: 'https://catalog.test/wake-federation.json' } },
    },
    {
      enabled: true,
      name: 'shell',
      exposes: { Button: { entry: 'src/button.tsx', allow_global_css: true } },
    },
    {
      enabled: true,
      name: 'shell',
      shared: { react: { required_version: '^19' } },
    },
    {
      enabled: true,
      name: 'shell',
      remotes: { catalog: { manifestUrl: null } },
    },
    {
      enabled: true,
      name: 'shell',
      remotes: {
        catalog: {
          manifestUrl: 'https://catalog.test/wake-federation.json',
          allowedOrigins: [null],
        },
      },
    },
    {
      enabled: true,
      name: 'shell',
      exposes: { Button: { entry: 'src/button.tsx', scope: null } },
    },
    {
      enabled: true,
      name: 'shell',
      exposes: { Button: { entry: 'src/button.tsx', shadow: null } },
    },
    {
      enabled: true,
      name: 'shell',
      shared: { react: { requiredVersion: null } },
    },
    {
      enabled: true,
      name: 'shell',
      shared: { react: { coherenceGroup: null } },
    },
    {
      enabled: true,
      name: 'shell',
      shared: { react: { owner: null } },
    },
  ]) {
    assert.throws(() => rawNative.createBuildContext(JSON.stringify({ federation })))
  }

  for (const options of [
    null,
    { environment: 'jsdom' },
    { environment: null },
    { updateSnapshots: 'overwrite' },
    { updateSnapshots: null },
    { workers: 0 },
    { workers: null },
    { workers: '0%' },
    { workers: '101%' },
    { workers: '01%' },
    { related: [null] },
  ]) {
    assert.throws(() => rawNative.createTestContext(JSON.stringify(options)))
  }

  const missingContext = rawNative.createTestContext()
  await missingContext.close()
  const undefinedContext = rawNative.createTestContext(JSON.stringify({ environment: undefined }))
  await undefinedContext.close()

  const context = rawNative.createTestContext('{}')
  try {
    for (const control of [
      { type: 'all', pattern: 'unexpected' },
      { type: 'all', pattern: null },
      { type: 'path' },
      { type: 'name', pattern: 'renders', typo: true },
    ]) {
      assert.throws(() => context.watchControl(JSON.stringify(control)))
    }
  } finally {
    await context.close()
  }
})

test('reuses one real native test context across runs and watch control', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'wake-test-context-'))
  const context = await createTestContext({ root: cwd, allowNoTests: true })
  const events = []
  const completedRunIds = []
  context.on('runStart', (event) => events.push(['start', event.runId]))
  context.on('runComplete', (result) => events.push(['complete', result.runId]))
  context.on('closed', () => events.push(['closed']))
  try {
    const first = await context.run()
    const second = await context.run()
    assert.equal(first.success, true)
    assert.equal(second.success, true)
    assert.notEqual(first.runId, second.runId)
    completedRunIds.push(first.runId, second.runId)
    context.startWatch()
    assert.equal(context.watching, true)
    context.stopWatch()
    assert.equal(context.watching, false)
  } finally {
    await context.close()
    await rm(cwd, { recursive: true, force: true })
  }
  assert.deepEqual(events.map(([type]) => type), [
    'start', 'complete', 'start', 'complete', 'closed',
  ])
  assert.deepEqual(
    events.filter(([type]) => type === 'start').map(([, runId]) => runId),
    completedRunIds,
  )
})

test('publishes strict Wake test and React entrypoints outside the test realm', async () => {
  const testExports = [
    'afterAll',
    'afterEach',
    'beforeAll',
    'beforeEach',
    'clock',
    'describe',
    'expect',
    'it',
    'mock',
    'network',
    'test',
  ]
  const reactExports = [...testExports, ...[
    'act',
    'cleanup',
    'fireEvent',
    'prettyDOM',
    'render',
    'renderHook',
    'screen',
    'userEvent',
    'waitFor',
    'waitForElementToBeRemoved',
    'within',
  ]].sort()
  const esmTest = await import('../test.mjs')
  const commonjsTest = require('../test.cjs')
  const esmReact = await import('../test-react.mjs')
  const commonjsReact = require('../test-react.cjs')

  assert.deepEqual(Object.keys(esmTest).sort(), testExports)
  assert.deepEqual(Object.keys(commonjsTest).sort(), testExports)
  assert.deepEqual(Object.keys(esmReact).sort(), reactExports)
  assert.deepEqual(Object.keys(commonjsReact).sort(), reactExports)
  assert.deepEqual(Object.keys(esmTest.mock).sort(), [
    'actual',
    'clearAll',
    'fn',
    'import',
    'isolate',
    'module',
    'replaceProperty',
    'resetAll',
    'restoreAll',
    'spyOn',
  ])
  assert.deepEqual(Object.keys(esmTest.clock).sort(), [
    'advanceBy',
    'advanceTo',
    'fake',
    'flushMicrotasks',
    'restore',
    'runAll',
    'runNext',
  ])
  assert.deepEqual(Object.keys(esmTest.network).sort(), [
    'allow',
    'requests',
    'reset',
    'route',
  ])
  assert.equal(esmTest.it, esmTest.test)
  assert.equal(commonjsTest.it, commonjsTest.test)
  assert.equal(esmReact.test, esmTest.test)
  assert.equal(commonjsReact.test, commonjsTest.test)
  assert.equal('concurrent' in esmTest.test, false)
  assert.equal('concurrent' in commonjsTest.test, false)

  for (const invoke of [
    () => esmTest.test('outside', () => {}),
    () => commonjsTest.expect(42),
    () => esmTest.mock.fn(),
    () => commonjsTest.clock.fake(),
    () => esmTest.network.route('/api', () => ({ status: 200 })),
    () => esmReact.render(null),
    () => commonjsReact.screen.getByRole('button'),
    () => esmReact.prettyDOM(),
  ]) {
    assert.throws(
      invoke,
      (error) => error.name === 'WakeError' && error.code === 'WAKE_TEST_CONTEXT',
    )
  }

  await assert.rejects(
    runTests(42),
    (error) => error instanceof WakeError && error.code === 'WAKE_TEST_CONFIG',
  )
  await assert.rejects(
    createTestContext([]),
    (error) => error instanceof WakeError && error.code === 'WAKE_TEST_CONFIG',
  )

  const v1Result = {
    schemaVersion: 'wake.test.v1',
    runId: 'run-contract-test',
    success: true,
    seed: 'seed-contract-test',
    durationMs: 1,
    terminationReason: 'completed',
    environment: { kind: 'dom', react: null, reactDom: null, v8: 'test', browser: null },
    suites: [],
    counts: {
      suites: { total: 0, passed: 0, failed: 0, skipped: 0 },
      tests: { total: 0, passed: 0, failed: 0, skipped: 0, todo: 0 },
    },
    snapshot: { added: 0, matched: 0, unmatched: 0, updated: 0, obsolete: 0, filesRemoved: 0 },
    coverage: {
      summary: {
        lines: { covered: 0, total: 0, percent: 100 },
        functions: { covered: 0, total: 0, percent: 100 },
        blocks: { covered: 0, total: 0, percent: 100 },
      },
      files: [],
      reportArtifactIds: [],
    },
    leaks: [],
    artifacts: [],
    diagnostics: [],
  }
  const envelope = (value) => JSON.stringify({ ok: true, value })
  const nativeEvents = [
    { type: 'runStart', runId: 'run-contract-test', watching: false },
    { type: 'diagnostic', runId: 'run-contract-test', diagnostic: {
      severity: 'note', code: 'WAKE_TEST_RUNTIME', message: 'native event', path: null, location: null,
    } },
    { type: 'runComplete', result: v1Result },
  ]
  const nativeContext = {
    closed: false,
    watching: false,
    run: async () => envelope(v1Result),
    startWatch() { this.watching = true },
    stopWatch() { this.watching = false },
    controls: [],
    watchControl(value) { this.controls.push(JSON.parse(value)) },
    eventsJson() { return JSON.stringify(nativeEvents.splice(0)) },
    async close() {
      this.closed = true
      nativeEvents.push({ type: 'closed' })
      return envelope(null)
    },
  }
  const context = await withNativeFactory(
    'createTestContext',
    () => nativeContext,
    () => commonjs.createTestContext(),
  )
  const starts = []
  const eventOrder = []
  context.on('runStart', (event) => starts.push(event))
  context.on('runStart', () => eventOrder.push('runStart'))
  context.on('diagnostic', () => eventOrder.push('diagnostic'))
  context.on('runComplete', () => eventOrder.push('runComplete'))
  context.on('closed', () => eventOrder.push('closed'))
  assert.deepEqual(await context.run(), v1Result)
  assert.deepEqual(starts, [{
    runId: 'run-contract-test',
    watching: false,
  }])
  assert.deepEqual(eventOrder, ['runStart', 'diagnostic', 'runComplete'])
  assert.equal(context.watching, false)
  assert.equal(context.startWatch(), context)
  assert.equal(context.watching, true)
  sendTestWatchControl(context, { type: 'failed' })
  sendTestWatchControl(context, { type: 'name', pattern: 'renders' })
  assert.deepEqual(nativeContext.controls, [
    { type: 'failed' },
    { type: 'name', pattern: 'renders' },
  ])
  assert.equal(context.stopWatch(), context)
  assert.equal(context.watching, false)
  await context.close()
  assert.deepEqual(eventOrder, ['runStart', 'diagnostic', 'runComplete', 'closed'])
})

test('closes a watched context after a fatal host event-pump failure', async () => {
  let eventReads = 0
  const nativeContext = {
    closed: false,
    watching: false,
    startWatch() { this.watching = true },
    stopWatch() { this.watching = false },
    eventsJson() {
      if (this.closed) return JSON.stringify([{ type: 'closed' }])
      if (eventReads++ === 0) return '[]'
      throw new Error('synthetic protocol failure')
    },
    async close() {
      this.closed = true
      this.watching = false
      return JSON.stringify({ ok: true, value: null })
    },
  }
  const context = await withNativeFactory(
    'createTestContext',
    () => nativeContext,
    () => commonjs.createTestContext(),
  )
  const closed = new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error('context did not close')), 2_000)
    context.once('closed', () => {
      clearTimeout(timeout)
      resolve()
    })
  })
  context.startWatch()
  await closed
  assert.equal(context.closed, true)
  const fatal = getTestContextFatalError(context)
  assert.equal(fatal?.code, 'WAKE_TEST_HOST')
  assert.match(fatal?.message, /synthetic protocol failure/)
})

test('generates native design tokens with strict references', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'wake-token-'))
  await writeFile(join(cwd, 'token.toml'), `[build]\noutput='./src/token.ts'\nprefix='demo'\n[token]\ncolor='red'\n`)
  const result = await generateCssToken({ cwd })
  assert.equal(result.success, true)
  await assertSameExistingPath(result.outputFile, join(cwd, 'src', 'token.ts'))
  assert.match(await readFile(result.outputFile, 'utf8'), /--demo-color/)
  await rm(cwd, { recursive: true, force: true })
})

test('generates native component docgen with deterministic output', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'wake-docgen-'))
  await mkdir(join(cwd, 'src'), { recursive: true })
  await writeFile(join(cwd, 'package.json'), '{"docgen":{"entry":"./src/button.tsx"}}')
  await writeFile(join(cwd, 'src', 'button.tsx'), '/** Button docs. */\nexport default function Button(props: ButtonProps) { return null }\n/** Public props. */\ninterface ButtonProps { /** Label. */ label: string }\n')
  const result = await generateDocgen({ cwd })
  assert.equal(result.success, true)
  const docgen = JSON.parse(await readFile(result.outputFile, 'utf8'))
  assert.equal(docgen['./src/button.tsx'][0].displayName, 'Button')
  assert.equal(docgen['./src/button.tsx'][0].props.label.required, true)
  await rm(cwd, { recursive: true, force: true })
})

test('builds native component-library entry contracts', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'wake-library-'))
  await mkdir(join(cwd, 'src'), { recursive: true })
  await writeFile(join(cwd, 'package.json'), '{"name":"@demo/button","type":"module"}')
  await writeFile(join(cwd, 'src', 'index.ts'), "import Button from './button.js';\nexport type { ButtonProps } from './button.js';\nexport default Button;\n")
  await writeFile(join(cwd, 'src', 'button.tsx'), "import type { FC } from 'react';\nexport interface ButtonProps { label: string; }\nconst Button: FC<ButtonProps> = (props) => <button>{props.label}</button>;\nexport default Button;\n")
  const result = await buildLibrary({ cwd })
  assert.equal(result.success, true)
  await assertSameExistingPath(result.esmEntry, join(cwd, 'esm', 'index.mjs'))
  await assertSameExistingPath(result.cjsEntry, join(cwd, 'cjs', 'index.cjs'))
  await assertSameExistingPath(result.declarationEntry, join(cwd, 'declarations', 'index.d.ts'))
  assert.equal(result.cssEntry, undefined)
  await access(result.esmEntry)
  await access(result.cjsEntry)
  await access(result.declarationEntry)
  await rm(cwd, { recursive: true, force: true })
})

test('reports platform details when the optional native package is missing', () => {
  const cwd = fileURLToPath(new URL('../../../', import.meta.url))
  const env = { ...process.env }
  delete env.WAKE_NATIVE_PATH
  const script = `
    const Module = require('node:module')
    const load = Module._load
    const resolve = Module._resolveFilename
    Module._resolveFilename = function (request) {
      if (/^@crab-dev\\/wake-(?:win32|linux|darwin)-/.test(request)) {
        const error = new Error('Simulated missing Wake native package')
        error.code = 'MODULE_NOT_FOUND'
        throw error
      }
      return resolve.apply(this, arguments)
    }
    Module._load = function (request) {
      if (/^@crab-dev\\/wake-(?:win32|linux|darwin)-/.test(request)) {
        const error = new Error('Simulated missing Wake native package')
        error.code = 'MODULE_NOT_FOUND'
        throw error
      }
      return load.apply(this, arguments)
    }
    try { require('./npm/wake/index.cjs') }
    catch (error) { console.log(error.code); console.log(error.message) }
  `
  const result = spawnSync(process.execPath, ['-e', script], {
    cwd,
    env,
    encoding: 'utf8',
  })
  assert.equal(result.status, 0)
  assert.match(result.stdout, /WAKE_UNSUPPORTED_PLATFORM/)
  assert.match(result.stdout, new RegExp(`${process.platform}/${process.arch}`))
  assert.match(result.stdout, /without --omit=optional/)
})

test('exposes disposable experimental compiler handles', async () => {
  const tokens = tokenize('export const answer = 42')
  assert.ok(tokens.tokens.length > 2)

  const module = parse('export const answer = 42')
  assert.equal(module.summary.statementCount, 1)
  assert.match(transform(module).code, /answer/)
  assert.equal(analyze(module).schemaVersion, 'wake.semantic.v1')
  const scoped = parse('const f=function self(){var self;return [self,arguments,()=>arguments]};arguments;')
  try {
    const semantic = analyze(scoped)
    const argumentsRefs = semantic.references.filter((reference) => reference.name === 'arguments')
    assert.notEqual(argumentsRefs[0].resolved, null)
    assert.equal(argumentsRefs[0].resolved, argumentsRefs[1].resolved)
    assert.equal(argumentsRefs[2].resolved, null)
    const implicit = semantic.symbols[argumentsRefs[0].resolved]
    assert.equal(implicit.declarationKind, 'arguments')
    assert.equal(implicit.start, 0)
    assert.equal(implicit.end, 0)
    const selfBindings = semantic.symbols.filter((symbol) => symbol.name === 'self')
    assert.equal(selfBindings.length, 2)
    assert.notEqual(selfBindings[0].scope, selfBindings[1].scope)
  } finally { scoped.dispose() }
  const legacy = parse('{ item; function item(){return item} } item;', { sourceType: 'script' })
  try {
    const semantic = analyze(legacy)
    const refs = semantic.references.filter((reference) => reference.name === 'item')
    assert.equal(refs[0].resolved, refs[1].resolved)
    assert.notEqual(refs[1].resolved, refs[2].resolved)
    const outer = semantic.symbols[refs[2].resolved]
    assert.equal(outer.declarationKind, 'var')
    assert.equal(outer.start, 0)
    assert.equal(outer.end, 0)
    assert.equal(outer.scope, 0)
  } finally { legacy.dispose() }
  const worker = new Worker('setInterval(() => {}, 1_000)', { eval: true })
  assert.throws(
    () => worker.postMessage(module),
    (error) => error.name === 'DataCloneError',
  )
  await worker.terminate()
  module.dispose()
  assert.equal(module.disposed, true)
  assert.throws(() => module.summary)
})

test('builds, bundles, and serializes context rebuilds', async () => {
  const cwd = fileURLToPath(new URL('../../../fixtures/hello-esm/', import.meta.url))
  const outputRoot = await mkdtemp(join(tmpdir(), 'wake-node-api-build-'))
  const outdir = join(outputRoot, 'dist')

  const result = await build({ cwd, outdir })
  assert.equal(result.success, true)
  assert.ok(result.files.some((file) => file.kind === 'html'))

  const bundled = await bundle({ cwd })
  assert.equal(typeof bundled.code, 'string')
  assert.equal(bundled.outputDir, undefined)

  const context = await createBuildContext({ cwd, outdir })
  contexts.push(context)
  const [first, second] = await Promise.all([
    context.rebuild(),
    context.rebuild(),
  ])
  assert.equal(first.success, true)
  assert.equal(second.success, true)
  await context.close()
  assert.equal(context.closed, true)
  await context.close()
  await rm(outputRoot, { recursive: true, force: true })
})

test('writes an executable Node CommonJS bundle to an exact outfile', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'wake-node-bundle-'))
  const outfile = join(cwd, 'dist', 'extension.cjs')
  await writeFile(join(cwd, 'package.json'), '{"name":"fixture"}')
  await writeFile(
    join(cwd, 'extension.ts'),
    "import path from 'node:path'; export const answer: number = path.sep.length + 41; " +
      'export const runtimeDir = __dirname; export const runtimeFile = __filename;',
  )
  const result = await bundle({
    cwd,
    entry: 'extension.ts',
    outfile,
    platform: 'node',
    target: 'node20',
  })
  assert.equal(result.outputFile, outfile)
  const loaded = require(outfile)
  assert.equal(loaded.answer, 42)
  assert.equal(loaded.runtimeDir, dirname(outfile))
  assert.equal(loaded.runtimeFile, outfile)
  const mappedOutfile = join(cwd, 'dist', 'mapped.cjs')
  const mapped = await bundle({
    cwd,
    entry: 'extension.ts',
    outfile: mappedOutfile,
    platform: 'node',
    sourceMap: true,
  })
  assert.equal(typeof mapped.sourceMap, 'string')
  assert.equal(mapped.sourceMapFile, `${mappedOutfile}.map`)
  assert.equal(await readFile(`${mappedOutfile}.map`, 'utf8'), mapped.sourceMap)
  assert.match(await readFile(mappedOutfile, 'utf8'), /sourceMappingURL=mapped\.cjs\.map/)
  assert.equal(await readFile(join(cwd, 'package.json'), 'utf8'), '{"name":"fixture"}')
  await rm(cwd, { recursive: true, force: true })
})

test('exposes the reachable exact-output collision code', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'wake-node-output-collision-'))
  const entry = join(cwd, 'src', 'index.js')
  const source = 'export const value = 42\n'
  await mkdir(join(cwd, 'src'), { recursive: true })
  await Promise.all([
    writeFile(join(cwd, 'wake.config.toml'), '[html]\nentry = "src/index.js"\n'),
    writeFile(entry, source),
  ])
  try {
    await assert.rejects(
      bundle({ cwd, entry: 'src/index.js', outfile: 'src/index.js' }),
      (error) => error instanceof WakeError && error.code === 'WAKE_OUTPUT_COLLISION',
    )
    assert.equal(await readFile(entry, 'utf8'), source)
  } finally {
    await rm(cwd, { recursive: true, force: true })
  }
})

test('minifies with a source map without changing the JavaScript payload', async () => {
  const cwd = await mkdtemp(join(tmpdir(), 'wake-node-minified-map-'))
  const outfile = join(cwd, 'dist', 'bundle.cjs')
  try {
    await writeFile(join(cwd, 'package.json'), '{"name":"minified-map-fixture"}')
    await writeFile(
      join(cwd, 'entry.js'),
      'function compute(descriptiveParameter) { const foldedValue = 1 + 2; return descriptiveParameter + foldedValue; } export const answer = compute(39);',
    )

    const unmapped = await bundle({
      cwd,
      entry: 'entry.js',
      outfile,
      platform: 'node',
      minify: true,
    })
    const mapped = await bundle({
      cwd,
      entry: 'entry.js',
      outfile,
      platform: 'node',
      minify: true,
      sourceMap: true,
    })

    const trailer = '//# sourceMappingURL=bundle.cjs.map\n'
    assert.equal(unmapped.success, true)
    assert.equal(mapped.success, true)
    assert.equal(typeof mapped.sourceMap, 'string')
    assert.equal(mapped.sourceMapFile, `${outfile}.map`)
    assert.ok(mapped.files.some((file) => file.kind === 'map'))
    assert.ok(mapped.code.endsWith(trailer))
    assert.equal(mapped.code.slice(0, -trailer.length), unmapped.code)
    assert.equal(await readFile(outfile, 'utf8'), mapped.code)
    assert.equal(await readFile(`${outfile}.map`, 'utf8'), mapped.sourceMap)
    assert.equal(require(outfile).answer, 42)
  } finally {
    await rm(cwd, { recursive: true, force: true })
  }
})

async function listen(server, port = 0) {
  await new Promise((resolve, reject) => {
    server.once('error', reject)
    server.listen(port, '127.0.0.1', resolve)
  })
  return server.address().port
}

function nextWebSocketJson(socket, predicate = () => true, timeoutMs = 10_000) {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => {
      cleanup()
      reject(new Error('Timed out waiting for a Live Reload WebSocket message'))
    }, timeoutMs)
    const onMessage = (event) => {
      let message
      try {
        message = JSON.parse(String(event.data))
      } catch {
        return
      }
      if (!predicate(message)) return
      cleanup()
      resolve(message)
    }
    const onError = () => {
      cleanup()
      reject(new Error('Live Reload WebSocket failed'))
    }
    const cleanup = () => {
      clearTimeout(timeout)
      socket.removeEventListener('message', onMessage)
      socket.removeEventListener('error', onError)
    }
    socket.addEventListener('message', onMessage)
    socket.addEventListener('error', onError)
  })
}

async function openLiveReloadSocket(port) {
  const socket = new WebSocket(`ws://127.0.0.1:${port}/__wake_live_reload`)
  const ready = nextWebSocketJson(socket, (message) => message.type === 'ready')
  await once(socket, 'open', { signal: AbortSignal.timeout(10_000) })
  assert.deepEqual(await ready, { type: 'ready' })
  return socket
}

async function assertServedClientPerformsFullPageReload(url, message) {
  const source = await fetch(new URL('/__wake/client.js', url)).then((response) => response.text())
  let socket
  let reloads = 0
  class BrowserSocket {
    constructor(socketUrl) {
      this.url = socketUrl
      socket = this
    }

    close() {}
  }
  const window = { __WAKE_MOUNT__: '' }
  const location = {
    protocol: 'http:',
    host: new URL(url).host,
    reload() {
      reloads += 1
    },
  }
  runInNewContext(source, {
    console,
    document: {},
    encodeURIComponent,
    location,
    setTimeout,
    Symbol,
    WebSocket: BrowserSocket,
    window,
  })
  assert.equal(socket.url, `ws://${location.host}/__wake_live_reload?mount=`)
  socket.onmessage({ data: JSON.stringify(message) })
  assert.equal(reloads, 1)
}

function onceMatching(emitter, eventName, predicate, timeoutMs = 10_000) {
  return new Promise((resolve, reject) => {
    const unmatched = []
    const timeout = setTimeout(() => {
      cleanup()
      reject(new Error(
        `Timed out waiting for matching ${eventName} event; observed ${JSON.stringify(unmatched)}`,
      ))
    }, timeoutMs)

    const onEvent = (event) => {
      if (!predicate(event)) {
        unmatched.push(event)
        return
      }
      cleanup()
      resolve(event)
    }
    const cleanup = () => {
      clearTimeout(timeout)
      emitter.off(eventName, onEvent)
    }
    emitter.on(eventName, onEvent)
  })
}

test('emits rebuild events, performs Live Reload, and releases the port', async () => {
  const source = fileURLToPath(new URL('../../../fixtures/hello-esm/', import.meta.url))
  const cwd = await mkdtemp(join(tmpdir(), 'wake-node-dev-'))
  await cp(source, cwd, { recursive: true })
  const reservation = createServer()
  const port = await listen(reservation)
  await new Promise((resolve) => reservation.close(resolve))

  // Windows path identity is case-insensitive, while bundler cache keys are lexical. Start from
  // an alternate spelling so this test also locks canonical project roots against notify paths.
  const serverCwd = process.platform === 'win32' ? cwd.toUpperCase() : cwd
  const server = await startDevServer({ cwd: serverCwd, port })
  let liveReloadSocket
  let closedEvents = 0
  server.on('closed', () => closedEvents += 1)
  try {
    assert.equal(server.url, `http://127.0.0.1:${port}/`)
    const initialBuild = once(server, 'rebuilt', { signal: AbortSignal.timeout(10_000) })
    // Native startup is fenced behind an authoritative Rescan, but its completed events can still
    // be waiting in the Node event pump. Match the file-backed generation instead of consuming
    // that startup `rebuildStart` with an intentionally empty path list.
    const rebuilding = onceMatching(
      server,
      'rebuildStart',
      (event) => event.changedPaths.length > 0,
    )
    // The same queued startup Rescan can report a full rebuild. This append changes exactly one
    // module, so select the incremental result paired with the substantive rebuild start.
    const rebuilt = onceMatching(
      server,
      'rebuilt',
      (event) => !event.initial
        && event.updatedModules === 1
        && event.cachedModules === event.modules - 1,
    )
    liveReloadSocket = await openLiveReloadSocket(port)
    const reloadFrame = nextWebSocketJson(
      liveReloadSocket,
      (message) => message.type === 'reload',
    )
    // `startDevServer()` returning is the readiness contract: an immediate write must
    // already be covered by the native watcher, without a sleep or retry in user code.
    const [[initial], start, event, reload] = await Promise.all([
      initialBuild,
      rebuilding,
      rebuilt,
      reloadFrame,
      appendFile(join(cwd, 'src/index.js'), '\nexport const changed = true;\n'),
    ])
    assert.equal(initial.initial, true)
    assert.ok(initial.modules > 0)
    assert.equal(initial.updatedModules + initial.cachedModules, initial.modules)
    assert.ok(initial.chunks > 0)
    assert.equal(start.type, 'rebuildStart')
    assert.equal(start.changedPaths.length, 1)
    assert.equal(event.type, 'rebuilt')
    assert.equal(event.initial, false)
    assert.ok(event.modules > 0)
    assert.equal(event.updatedModules, 1)
    assert.equal(event.cachedModules, event.modules - 1)
    assert.equal(event.updatedModules + event.cachedModules, event.modules)
    assert.ok(event.durationMs >= 0)
    assert.deepEqual(reload, { type: 'reload', mount: '' })
    await assertServedClientPerformsFullPageReload(server.url, reload)
  } finally {
    liveReloadSocket?.close()
    const closingAt = performance.now()
    await Promise.all([server.close(), server.waitUntilClosed()])
    const closeDurationMs = performance.now() - closingAt
    assert.ok(closeDurationMs < 2_000, `dev server close took ${closeDurationMs.toFixed(0)}ms`)
    await server.close()
  }
  assert.equal(closedEvents, 1)

  const probe = createServer()
  await listen(probe, port)
  await new Promise((resolve) => probe.close(resolve))
  await rm(cwd, { recursive: true, force: true })
})

test('dev diagnostics preserve native source locations', async () => {
  const source = fileURLToPath(new URL('../../../fixtures/hello-esm/', import.meta.url))
  const cwd = await mkdtemp(join(tmpdir(), 'wake-node-diagnostic-'))
  await cp(source, cwd, { recursive: true })
  await writeFile(
    join(cwd, 'src/index.js'),
    'const first = 1;\nconst second = 2;\nconst broken = ;\n',
  )
  const reservation = createServer()
  const port = await listen(reservation)
  await new Promise((resolve) => reservation.close(resolve))
  const server = await startDevServer({ cwd, port })
  try {
    const diagnostic = await onceMatching(server, 'diagnostic', () => true)
    assert.equal(diagnostic.severity, 'error')
    assert.equal(diagnostic.location.line, 3)
    assert.equal(diagnostic.location.lineText, 'const broken = ;')
    assert.ok(diagnostic.location.column > 1)
  } finally {
    await server.close()
    await rm(cwd, { recursive: true, force: true })
  }
})

test('addon cleanup releases native resources when a Worker exits', async () => {
  const source = fileURLToPath(new URL('../../../fixtures/hello-esm/', import.meta.url))
  const cwd = await mkdtemp(join(tmpdir(), 'wake-node-worker-'))
  await cp(source, cwd, { recursive: true })
  const reservation = createServer()
  const port = await listen(reservation)
  await new Promise((resolve) => reservation.close(resolve))

  const worker = new Worker(`
    const { parentPort, workerData } = require('node:worker_threads')
    ;(async () => {
      const wake = require(workerData.api)
      globalThis.server = await wake.startDevServer({
        cwd: workerData.cwd,
        port: workerData.port,
      })
      parentPort.postMessage('started')
    })().catch((error) => { throw error })
  `, {
    eval: true,
    workerData: {
      api: fileURLToPath(new URL('../index.cjs', import.meta.url)),
      cwd,
      port,
    },
  })
  const exited = once(worker, 'exit')
  const [message] = await once(worker, 'message')
  assert.equal(message, 'started')
  const [exitCode] = await exited
  assert.equal(exitCode, 0)

  const probe = createServer()
  await listen(probe, port)
  await new Promise((resolve) => probe.close(resolve))
  await rm(cwd, { recursive: true, force: true })
})

test('rejects fields outside each closed Node request contract', async () => {
  for (const operation of [
    () => build({ source_map: true }),
    () => bundle({ source_map: true }),
    () => generateCssToken({ config_path: 'token.toml' }),
    () => generateDocgen({ entryPath: 'src/button.tsx' }),
    () => buildLibrary({ entryPath: 'src/index.ts' }),
    () => buildDocs({ base_path: '/docs/' }),
    () => startDevServer({ mode: 'site' }),
    () => startDocsDevServer({ entry: 'src/index.ts' }),
    () => startDocsDevServer({ federation: { enabled: false } }),
  ]) {
    await assert.rejects(
      operation(),
      (error) => error instanceof WakeError && error.code === 'WAKE_CONFIG',
    )
  }

  const missingRoot = join(tmpdir(), `wake-node-unknown-priority-${process.pid}`)
  await assert.rejects(
    build({ cwd: missingRoot, source_map: true }),
    (error) => error instanceof WakeError
      && error.code === 'WAKE_CONFIG'
      && /unknown field `source_map`/.test(error.message),
    'wire-shape rejection must run before project discovery or build work',
  )

  for (const options of [
    { watch: true },
    { reporter: 'json' },
    { output: 'artifacts/tests' },
  ]) {
    await assert.rejects(
      runTests(options),
      (error) => error instanceof WakeError && error.code === 'WAKE_TEST_CONFIG',
    )
    await assert.rejects(
      createTestContext(options),
      (error) => error instanceof WakeError && error.code === 'WAKE_TEST_CONFIG',
    )
  }
})

test('builds docs and controls the docs dev server lifecycle', async () => {
  const cwd = fileURLToPath(new URL('../../../', import.meta.url))
  const docsOutputRoot = await mkdtemp(join(tmpdir(), 'wake-node-docs-api-'))
  const componentsRuntime = await readFile(
    fileURLToPath(new URL('../internal/components-runtime.mjs', import.meta.url)),
    'utf8',
  )
  assert.doesNotMatch(
    componentsRuntime,
    /["'][^"'\r\n]+\.css(?:\?[^"'\r\n]*)?["']/,
    'Components runtime must rely on Wake auto-style injection and contain no CSS import',
  )
  const outdir = join(docsOutputRoot, 'site')
  const result = await buildDocs({ cwd, outdir })
  assert.equal(result.success, true)
  assert.ok(result.routes.length > 0)
  assert.deepEqual(result.workspaces, [])
  assert.deepEqual(Object.keys(result).sort(), [
    'cachedModuleCount',
    'demos',
    'diagnostics',
    'durationMs',
    'files',
    'mode',
    'moduleCount',
    'outputDir',
    'routes',
    'success',
    'updatedModuleCount',
    'workspaces',
  ])
  assert.deepEqual(Object.keys(result.routes[0]).sort(), [
    'description',
    'draft',
    'file',
    'group',
    'groupId',
    'headings',
    'hidden',
    'id',
    'kind',
    'section',
    'sectionId',
    'slug',
    'status',
    'title',
  ])
  if (result.routes[0].headings.length > 0) {
    assert.deepEqual(Object.keys(result.routes[0].headings[0]).sort(), ['depth', 'id', 'title'])
  }
  assert.deepEqual(Object.keys(result.files[0]).sort(), ['bytes', 'kind', 'path'])
  const componentsCwd = fileURLToPath(new URL('../../../fixtures/react-docs/', import.meta.url))
  const componentsRoot = await mkdtemp(join(tmpdir(), 'wake-components-api-'))
  const componentsOutdir = join(componentsRoot, 'dist')
  const workbench = await buildDocs({
    cwd: componentsCwd,
    outdir: componentsOutdir,
    basePath: '/workbench/',
    mode: 'components',
  })
  assert.equal(workbench.mode, 'components')
  assert.deepEqual(workbench.routes, [])
  assert.deepEqual(workbench.workspaces, [])
  assert.ok(workbench.demos.some((demo) => demo.component === '按钮' && demo.controlCount > 0))
  assert.deepEqual(Object.keys(workbench.demos[0]).sort(), [
    'component',
    'controlCount',
    'group',
    'id',
    'order',
    'title',
    'warnings',
  ])
  await Promise.all([
    access(join(componentsOutdir, 'index.html')),
    access(join(componentsOutdir, '404.html')),
  ])
  const componentFiles = await readdir(componentsOutdir)
  const componentManifest = JSON.parse(
    await readFile(join(componentsOutdir, 'manifest.json'), 'utf8'),
  )
  const componentEntryFile = componentManifest.entry
  assert.match(
    componentEntryFile,
    /^entry\.[0-9a-f]{8}\.js$/,
    'Components build must emit a content-hashed JavaScript entry',
  )
  assert.ok(
    componentFiles.includes(componentEntryFile),
    'Components build must emit the JavaScript entry named by its manifest',
  )
  await assertComponentsRuntime(join(componentsOutdir, componentEntryFile))
  const componentCssFile = componentFiles.find((file) => /^styles\.[0-9a-f]{8}\.css$/.test(file))
  assert.ok(componentCssFile, 'Components build must emit a CSS asset')
  const componentHtml = await readFile(join(componentsOutdir, 'index.html'), 'utf8')
  assert.match(
    componentHtml,
    new RegExp(`href=["'][^"']*${componentCssFile.replaceAll('.', '\\.')}["']`),
    'index.html must link the emitted hashed CSS asset',
  )
  const componentCss = await readFile(join(componentsOutdir, componentCssFile), 'utf8')
  for (const prefix of [
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
  ]) {
    assert.match(componentCss, new RegExp(prefix), `Components CSS must include ${prefix}`)
  }

  const aggregateCwd = fileURLToPath(
    new URL('../../../fixtures/react-docs-workspaces/', import.meta.url),
  )
  const aggregateOutdir = join(docsOutputRoot, 'workspaces')
  const aggregate = await buildDocs({ cwd: aggregateCwd, outdir: aggregateOutdir })
  assert.deepEqual(
    aggregate.workspaces.map(({ name, basePath, presentation, demos }) => ({
      name,
      basePath,
      presentation,
      demos,
    })),
    [
      {
        name: 'rc-alpha',
        basePath: '/docs/components/rc-alpha/workbench/',
        presentation: 'embedded',
        demos: 1,
      },
      {
        name: 'rc-beta',
        basePath: '/docs/components/rc-beta/workbench/',
        presentation: 'standalone',
        demos: 1,
      },
    ],
  )
  assert.deepEqual(Object.keys(aggregate.workspaces[0]).sort(), [
    'basePath',
    'demos',
    'mode',
    'name',
    'presentation',
    'root',
  ])
  const aggregateManifest = JSON.parse(
    await readFile(join(aggregateOutdir, 'manifest.json'), 'utf8'),
  )
  assert.deepEqual(
    aggregateManifest.workspaces.map(({ name }) => name),
    ['rc-alpha', 'rc-beta'],
  )

  const reservation = createServer()
  const port = await listen(reservation)
  await new Promise((resolve) => reservation.close(resolve))
  await rm(componentsRoot, { recursive: true, force: true })
  const server = await startDocsDevServer({ cwd, port })
  await server.close()
  const probe = createServer()
  await listen(probe, port)
  await new Promise((resolve) => probe.close(resolve))

  const aggregateReservation = createServer()
  const aggregatePort = await listen(aggregateReservation)
  await new Promise((resolve) => aggregateReservation.close(resolve))
  const aggregateServer = await startDocsDevServer({ cwd: aggregateCwd, port: aggregatePort })
  try {
    const allLoaded = new Promise((resolve, reject) => {
      const timeout = setTimeout(() => reject(new Error('lazy workspace did not load')), 15_000)
      aggregateServer.on('workspaceState', (event) => {
        if (event.total === 2 && event.loaded === 2 && event.failed === 0) {
          clearTimeout(timeout)
          resolve(event)
        }
      })
    })
    const parentRoute = await fetch(
      `http://127.0.0.1:${aggregatePort}/docs/components/rc-alpha/`,
    )
    assert.equal(parentRoute.status, 200)
    const lazyRoute = await fetch(
      `http://127.0.0.1:${aggregatePort}/docs/components/rc-alpha/workbench/`,
    )
    assert.equal(lazyRoute.status, 200)
    await allLoaded
  } finally {
    await aggregateServer.close()
  }

  const invalid = fileURLToPath(new URL('../../../fixtures/hello-esm/', import.meta.url))
  await assert.rejects(
    buildDocs({ cwd: invalid }),
    (error) => error instanceof WakeError && error.code === 'WAKE_BUILD',
  )
  await rm(docsOutputRoot, { recursive: true, force: true })
})

test('normalizes cancellation and configuration failures', async () => {
  const controller = new AbortController()
  controller.abort()
  await assert.rejects(
    build({ cwd: '.', signal: controller.signal }),
    (error) => error instanceof WakeError && error.code === 'WAKE_CANCELLED',
  )

  await assert.rejects(
    build({ cwd: 'does-not-exist' }),
    (error) => error instanceof WakeError && error.code === 'WAKE_CONFIG',
  )

  await assert.rejects(
    bundle({ platform: 'node', format: 'iife' }),
    (error) => error instanceof WakeError && error.code === 'WAKE_CONFIG',
  )
})
