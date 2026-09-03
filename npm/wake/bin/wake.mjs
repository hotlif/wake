#!/usr/bin/env node

import { readFileSync, writeFileSync } from 'node:fs'
import { readFile } from 'node:fs/promises'
import {
  build,
  buildLibrary,
  buildDocs,
  bundle,
  createTestContext,
  generateFederationLock,
  generateCssToken,
  generateDocgen,
  initializeFederation,
  runTests,
  startDevServer,
  startDocsDevServer,
  version,
  WakeError,
} from '../index.mjs'
import { parse, tokenize } from '../experimental.mjs'
import testContextInternal from '../test-context-internal.cjs'
import {
  applyDashboardEvent,
  createDashboardSession,
  createDashboardState,
  createUi,
  formatBanner,
  formatBuildResult,
  formatError,
  formatFinalSummary,
  formatGeneratorResult,
  formatServerReady,
  observeServer,
  setDashboardEndpoint,
  setDashboardStopped,
  setDashboardStopping,
  supportsColor,
  supportsTui,
} from './terminal.mjs'

const HELP = `Wake ${version()}

Usage:
  wake [--ui auto|tui|plain] [--no-color] <command>
  wake build [entry] [--outdir DIR] [--cache] [--sourcemap]
  wake bundle <entry> --outfile FILE [--platform browser|node] [--format iife|cjs]
              [--target node20] [--external PACKAGE] [--minify] [--sourcemap]
              [--cache] [--config FILE]
  wake library token [project] [--config token.toml]
  wake federation init [root]
  wake federation lock [root]
  wake dev [root] [--entry FILE] [--host HOST] [--port PORT] [--open]
  wake docs build [root] [--mode site|components] [--outdir DIR] [--base PATH]
  wake docs dev [root] [--mode site|components] [--host HOST] [--port PORT] [--open]
  wake parse <file> [--format auto|human|json]
  wake tokenize <file> [--format auto|human|json]
  wake test [patterns...] [--root DIR] [--name-pattern TEXT] [--project NAME]
            [--environment auto|dom|browser] [--watch] [--changed] [--related PATH...]
            [--coverage] [--update-snapshots] [--serial] [--workers COUNT]
            [--bail [COUNT]] [--shard INDEX/TOTAL] [--seed SEED] [--shuffle]
            [--reporter pretty|json|junit] [--output FILE] [--allow-no-tests]
            [--browser-path FILE] [--headful]
  wake --version

Options:
  --ui MODE   Terminal UI mode for long-running commands (default: auto)
  --no-color  Disable terminal colors; also honors NO_COLOR
  --format    Human or JSON output for parse/tokenize (default: auto)
`

function takeOption(args, name, usage = false) {
  const index = args.indexOf(name)
  if (index === -1) return undefined
  if (index + 1 >= args.length) {
    const message = `${name} requires a value`
    throw usage ? usageError(message) : new Error(message)
  }
  const [value] = args.splice(index + 1, 1)
  args.splice(index, 1)
  return value
}

function usageError(message) {
  const error = new WakeError('WAKE_CONFIG', message)
  error.exitCode = 2
  return error
}

function takeFlag(args, name) {
  const index = args.indexOf(name)
  if (index === -1) return false
  args.splice(index, 1)
  return true
}

function takeOptions(args, name, usage = false) {
  const values = []
  for (;;) {
    const value = takeOption(args, name, usage)
    if (value === undefined) return values
    values.push(value)
  }
}

function takeVariadicOptions(args, name) {
  const values = []
  for (;;) {
    const index = args.indexOf(name)
    if (index === -1) return values
    let end = index + 1
    while (end < args.length && !args[end].startsWith('-')) end += 1
    if (end === index + 1) throw testUsageError(`${name} requires at least one value`)
    values.push(...args.slice(index + 1, end))
    args.splice(index, end - index)
  }
}

function testUsageError(message) {
  const error = new WakeError('WAKE_TEST_CONFIG', message)
  error.exitCode = 2
  return error
}

function validateTestChoice(value, name, choices) {
  if (value !== undefined && !choices.includes(value)) {
    throw testUsageError(`${name} must be one of: ${choices.join(', ')}`)
  }
  return value
}

function parseTestWorkers(value) {
  if (value === undefined || value === 'auto') return value
  if (/^[1-9][0-9]*$/.test(value)) {
    const count = Number(value)
    if (Number.isSafeInteger(count)) return count
  }
  const match = /^([1-9][0-9]?)%$|^(100)%$/.exec(value)
  if (match) return `${Number(match[1] || match[2])}%`
  throw testUsageError('--workers requires auto, a positive integer, or 1%-100%')
}

function takeTestBail(args) {
  const index = args.indexOf('--bail')
  if (index === -1) return undefined
  const candidate = args[index + 1]
  const hasValue = candidate !== undefined && !candidate.startsWith('-')
  const value = hasValue ? Number(candidate) : 1
  args.splice(index, hasValue ? 2 : 1)
  if (!Number.isSafeInteger(value) || value < 0) {
    throw testUsageError('--bail requires a non-negative integer')
  }
  return value
}

function parseTestShard(value) {
  if (value === undefined) return undefined
  const match = /^([1-9][0-9]*)\/([1-9][0-9]*)$/.exec(value)
  if (!match) throw testUsageError('--shard requires the 1-based INDEX/TOTAL form')
  const index = Number(match[1])
  const total = Number(match[2])
  if (!Number.isSafeInteger(index) || !Number.isSafeInteger(total) || index > total) {
    throw testUsageError('--shard requires 1 <= INDEX <= TOTAL')
  }
  return `${index}/${total}`
}

function commonOptions(args) {
  return {
    configPath: takeOption(args, '--config'),
  }
}

function printLines(lines, output = console.error) {
  for (const line of lines) output(line)
}

function validateChoice(value, name, choices) {
  if (!choices.includes(value)) {
    throw new Error(`${name} must be one of: ${choices.join(', ')}`)
  }
  return value
}

function validateBundleChoice(value, name, choices) {
  if (!choices.includes(value)) {
    throw usageError(`${name} must be one of: ${choices.join(', ')}`)
  }
  return value
}

function ensureStaticMode(uiMode) {
  if (uiMode === 'tui') {
    throw new Error('--ui tui is only available for dev and docs dev')
  }
}

function resolveTui(uiMode) {
  const supported = supportsTui()
  if (uiMode === 'plain') return false
  if (uiMode === 'tui' && !supported) {
    throw new Error('--ui tui requires interactive stdin and stderr and a capable terminal')
  }
  return supported
}

function resolveFormat(value) {
  const format = validateChoice(value || 'auto', '--format', ['auto', 'human', 'json'])
  return format === 'auto' ? (process.stdout.isTTY ? 'human' : 'json') : format
}

function printResult(ui, result, label, extra = '') {
  printLines(formatBuildResult(ui, result, label, extra))
}

function formatTestFailure(failure) {
  let rendered = failure.code ? `${failure.code}: ` : ''
  rendered += failure.message
  if (failure.location) {
    rendered += `\n  at ${failure.location.path}:${failure.location.line}:${failure.location.column}`
  }
  if (failure.diff?.unified) rendered += `\n${failure.diff.unified}`
  if (failure.stack && !rendered.includes(failure.stack)) rendered += `\n${failure.stack}`
  return rendered
}

function xmlEscape(value) {
  return String(value)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&apos;')
}

function junitTestReport(result) {
  const suiteErrors = result.suites.filter((suite) => suite.failures.length > 0).length
  const pending = result.counts.tests.skipped + result.counts.tests.todo
  const lines = [
    '<?xml version="1.0" encoding="UTF-8"?>',
    `<testsuites tests="${result.counts.tests.total + suiteErrors}" failures="${result.counts.tests.failed}" errors="${suiteErrors}" skipped="${pending}" time="${(result.durationMs / 1_000).toFixed(3)}">`,
  ]
  for (const suite of result.suites) {
    const failures = suite.tests.filter((testCase) => testCase.status === 'failed').length
    const skipped = suite.tests.filter((testCase) => testCase.status === 'skipped' || testCase.status === 'todo').length
    const suiteError = suite.failures.length > 0 ? 1 : 0
    lines.push(`  <testsuite name="${xmlEscape(suite.path)}" tests="${suite.tests.length + suiteError}" failures="${failures}" errors="${suiteError}" skipped="${skipped}" time="${(suite.durationMs / 1_000).toFixed(3)}">`)
    for (const testCase of suite.tests) {
      const prefix = `    <testcase name="${xmlEscape(testCase.name)}" classname="${xmlEscape(suite.path)}" time="${(testCase.durationMs / 1_000).toFixed(3)}">`
      if (testCase.status === 'failed') {
        lines.push(`${prefix}<failure>${xmlEscape(testCase.failures.map(formatTestFailure).join('\n'))}</failure></testcase>`)
      } else if (testCase.status === 'skipped' || testCase.status === 'todo') {
        lines.push(`${prefix}<skipped /></testcase>`)
      } else {
        lines.push(`${prefix}</testcase>`)
      }
    }
    if (suite.failures.length > 0) {
      lines.push(`    <testcase name="[suite setup]"><error>${xmlEscape(suite.failures.map(formatTestFailure).join('\n'))}</error></testcase>`)
    }
    lines.push('  </testsuite>')
  }
  lines.push('</testsuites>')
  return lines.join('\n')
}

function printTestRun(result, reporter = 'pretty', output, includeDiagnostics = true) {
  if (reporter !== 'pretty') {
    const report = reporter === 'json' ? JSON.stringify(result) : junitTestReport(result)
    if (output) writeFileSync(output, report)
    else console.log(report)
    return
  }
  for (const suite of result.suites) {
    const status = suite.status === 'failed'
      ? 'FAIL'
      : suite.status === 'skipped'
        ? 'SKIP'
        : 'PASS'
    console.error(`${status} ${suite.path}`)
    for (const testCase of suite.tests) {
      const marker = testCase.status === 'passed'
        ? '✓'
        : testCase.status === 'failed'
          ? '✕'
          : testCase.status === 'todo'
            ? '✎'
            : '○'
      console.error(`  ${marker} ${testCase.name}`)
      for (const failure of testCase.failures) {
        console.error(`    ${formatTestFailure(failure).replaceAll('\n', '\n    ')}`)
      }
    }
    for (const failure of suite.failures) {
      console.error(`  ${formatTestFailure(failure).replaceAll('\n', '\n  ')}`)
    }
  }
  console.error(`Test Suites: ${result.counts.suites.passed} passed, ${result.counts.suites.failed} failed, ${result.counts.suites.total} total`)
  console.error(`Tests:       ${result.counts.tests.passed} passed, ${result.counts.tests.failed} failed, ${result.counts.tests.skipped + result.counts.tests.todo} pending, ${result.counts.tests.total} total`)
  const coverageText = result.artifacts.find((artifact) => artifact.kind === 'coverage-text')
  if (coverageText) {
    console.error(readFileSync(coverageText.path, 'utf8').trimEnd())
  }
  console.error(`Seed:        ${result.seed}`)
  console.error(`Time:        ${result.durationMs} ms`)
  if (result.terminationReason !== 'completed') {
    console.error(`Termination: ${result.terminationReason}`)
  }
  if (includeDiagnostics) {
    for (const diagnostic of result.diagnostics) {
      console.error(`${diagnostic.code}: ${diagnostic.message}`)
    }
  }
}

function testResultExitCode(result) {
  if (result.terminationReason === 'cancelled' || result.terminationReason === 'watch-restart') {
    return 130
  }
  if (['host-crash', 'oom', 'internal-error'].includes(result.terminationReason)) return 2
  return result.success ? 0 : 1
}

function metricsFromEvent(event) {
  return {
    modules: event.modules,
    updatedModules: event.updatedModules,
    cachedModules: event.cachedModules,
    chunks: event.chunks,
    assets: event.assets,
    durationMs: event.durationMs,
  }
}

async function runServer(factory, options, command, root, ui, uiMode) {
  const controller = new AbortController()
  const useTui = resolveTui(uiMode)
  const state = createDashboardState({
    command,
    root: root || '.',
    watchLabel: command === 'docs dev'
      ? 'MDX · Live reload · search index · watching'
      : command === 'docs components'
        ? 'Demo · Controls · Live reload · watching'
        : 'Live reload · source maps · watching',
  })
  state.version = version()
  let dashboard
  let server
  let stopObserving = () => {}
  let initialMetrics
  let finalReason = 'server closed'

  if (useTui) dashboard = createDashboardSession(state, { ui })
  else printLines(formatBanner(ui, command, version()))

  try {
    server = await factory({ ...options, signal: controller.signal })
    setDashboardEndpoint(state, server.url)

    const onRebuildStart = (event) => {
      applyDashboardEvent(state, event)
      dashboard?.draw()
    }
    const onRebuilt = (event) => {
      if (event.initial) initialMetrics = metricsFromEvent(event)
      applyDashboardEvent(state, event)
      dashboard?.draw()
    }
    const onDiagnostic = (diagnostic) => {
      applyDashboardEvent(state, { type: 'diagnostic', diagnostic })
      dashboard?.draw()
    }
    const onWorkspaceState = (event) => {
      applyDashboardEvent(state, event)
      dashboard?.draw()
    }
    const onClosed = () => {
      applyDashboardEvent(state, { type: 'closed' })
      dashboard?.draw()
    }
    server.on('rebuildStart', onRebuildStart)
    server.on('rebuilt', onRebuilt)
    server.on('diagnostic', onDiagnostic)
    server.on('workspaceState', onWorkspaceState)
    server.on('closed', onClosed)

    if (!useTui) stopObserving = observeServer(server, ui)
    await new Promise((resolve) => setTimeout(resolve, 35))
    if (!useTui) {
      printLines(formatServerReady(ui, server.url, initialMetrics))
    } else {
      dashboard.draw()
    }

    let resolveSignal
    const signalExit = new Promise((resolve) => { resolveSignal = resolve })
    const onSigint = () => resolveSignal('SIGINT')
    const onSigterm = () => resolveSignal('SIGTERM')
    process.once('SIGINT', onSigint)
    process.once('SIGTERM', onSigterm)

    try {
      const closed = server.waitUntilClosed().then(() => 'closed')
      const reason = await Promise.race([
        closed,
        signalExit,
        dashboard ? dashboard.exit : new Promise(() => {}),
      ])
      if (reason !== 'closed') {
        finalReason = reason
        setDashboardStopping(state, reason === 'q' ? 'q' : reason)
        dashboard?.draw()
        controller.abort()
        await server.close()
        await closed
      }
      setDashboardStopped(state)
      dashboard?.draw()
      return reason
    } finally {
      process.off('SIGINT', onSigint)
      process.off('SIGTERM', onSigterm)
      server.off('rebuildStart', onRebuildStart)
      server.off('rebuilt', onRebuilt)
      server.off('diagnostic', onDiagnostic)
      server.off('workspaceState', onWorkspaceState)
      server.off('closed', onClosed)
    }
  } catch (error) {
    if (dashboard) {
      dashboard.close()
      dashboard = undefined
      printLines(formatBanner(ui, command, version()))
    }
    throw error
  } finally {
    stopObserving()
    dashboard?.close()
    if (server) printLines(formatFinalSummary(ui, state, finalReason))
  }
}

async function runTestCommand(args) {
  try {
    const root = takeOption(args, '--root', true)
    const namePattern = takeOption(args, '--name-pattern', true)
    const projects = takeOptions(args, '--project', true)
    const environment = validateTestChoice(
      takeOption(args, '--environment', true),
      '--environment',
      ['auto', 'dom', 'browser'],
    )
    const watch = takeFlag(args, '--watch')
    const changed = takeFlag(args, '--changed')
    const related = takeVariadicOptions(args, '--related')
    const coverage = takeFlag(args, '--coverage')
    const updateSnapshots = takeFlag(args, '--update-snapshots') ? 'all' : undefined
    const serial = takeFlag(args, '--serial')
    const workers = parseTestWorkers(takeOption(args, '--workers', true))
    const bail = takeTestBail(args)
    const shard = parseTestShard(takeOption(args, '--shard', true))
    const seed = takeOption(args, '--seed', true)
    const shuffle = takeFlag(args, '--shuffle')
    const reporter = validateTestChoice(
      takeOption(args, '--reporter', true),
      '--reporter',
      ['pretty', 'json', 'junit'],
    )
    const output = takeOption(args, '--output', true)
    const allowNoTests = takeFlag(args, '--allow-no-tests')
    const browserPath = takeOption(args, '--browser-path', true)
    const headful = takeFlag(args, '--headful')

    if (changed && related.length > 0) {
      throw testUsageError('--changed cannot be combined with --related')
    }
    if (serial && workers !== undefined) {
      throw testUsageError('--serial cannot be combined with --workers')
    }
    if (output && reporter === undefined) {
      throw testUsageError('--output requires --reporter json or --reporter junit')
    }
    if (output && reporter === 'pretty') {
      throw testUsageError('--output requires --reporter json or --reporter junit')
    }
    if (args.some((argument) => argument.startsWith('-'))) {
      throw testUsageError(`unknown test arguments: ${args.filter((argument) => argument.startsWith('-')).join(' ')}`)
    }

    const nativeOptions = {
      root,
      patterns: args.splice(0),
      namePattern,
      projects,
      environment,
      changed,
      related,
      coverage,
      updateSnapshots,
      serial,
      workers,
      bail,
      shard,
      seed,
      shuffle,
      allowNoTests,
      browserPath,
      headful,
    }
    const selectedReporter = reporter || 'pretty'

    if (!watch) {
      const result = await runTests(nativeOptions)
      printTestRun(result, selectedReporter, output)
      return testResultExitCode(result)
    }

    const context = await createTestContext(nativeOptions)
    let lastExitCode = 0
    let outputError
    let finish
    const finished = new Promise((resolve) => { finish = resolve })
    let closing
    const requestClose = () => {
      closing ||= context.close().catch((error) => {
        outputError ||= error
      })
      return closing
    }
    const finishAndClose = (reason) => {
      finish(reason)
      void requestClose()
    }
    const onSigint = () => finishAndClose('signal')
    const onSigterm = () => finishAndClose('signal')
    const onClosed = () => {
      const fatalError = testContextInternal.getTestContextFatalError(context)
      if (fatalError) {
        outputError = fatalError
        finish('fatal')
      } else {
        finish('closed')
      }
    }
    process.once('SIGINT', onSigint)
    process.once('SIGTERM', onSigterm)
    context.once('closed', onClosed)
    context.on('runComplete', (result) => {
      if (result.terminationReason === 'watch-restart' || result.terminationReason === 'cancelled') {
        return
      }
      lastExitCode = testResultExitCode(result)
      try {
        printTestRun(result, selectedReporter, output, false)
      } catch (error) {
        outputError = error
        finish('output-error')
      }
    })
    context.on('diagnostic', (diagnostic) => {
      console.error(`${diagnostic.code}: ${diagnostic.message}`)
    })
    const stdin = process.stdin
    const interactive = Boolean(stdin.isTTY && stdin.setRawMode)
    const previousRaw = interactive ? Boolean(stdin.isRaw) : false
    let prompt
    const sendControl = (control) => {
      try {
        testContextInternal.sendTestWatchControl(context, control)
      } catch (error) {
        outputError = error
        finish('control-error')
      }
    }
    const onWatchKey = (chunk) => {
      for (const character of chunk.toString('utf8')) {
        if (character === '\u0003') {
          finishAndClose('signal')
          continue
        }
        if (prompt) {
          if (character === '\r' || character === '\n') {
            process.stdout.write('\n')
            const value = prompt.value.trim()
            if (value) sendControl({ type: prompt.type, pattern: value })
            prompt = undefined
          } else if (character === '\u001b') {
            process.stdout.write('\n')
            prompt = undefined
          } else if (character === '\b' || character === '\u007f') {
            if (prompt.value) {
              prompt.value = prompt.value.slice(0, -1)
              process.stdout.write('\b \b')
            }
          } else if (character >= ' ') {
            prompt.value += character
            process.stdout.write(character)
          }
          continue
        }
        switch (character) {
          case 'a':
            sendControl({ type: 'all' })
            break
          case 'f':
            sendControl({ type: 'failed' })
            break
          case 'p':
            prompt = { type: 'path', value: '' }
            process.stdout.write('\nPath pattern: ')
            break
          case 't':
            prompt = { type: 'name', value: '' }
            process.stdout.write('\nTest name pattern: ')
            break
          case 'u':
            sendControl({ type: 'updateSnapshots' })
            break
          case 'r':
            sendControl({ type: 'rerun' })
            break
          case 'q':
            finishAndClose('quit')
            break
          default:
        }
      }
    }
    try {
      context.startWatch()
      if (interactive) {
        stdin.setRawMode(true)
        stdin.resume()
        stdin.on('data', onWatchKey)
        console.error('Watch keys: a all · f failed · p path · t name · u snapshots · r rerun · q quit')
      }
      // StartWatch schedules the first host-owned run. The JS thread remains free for watch
      // controls and cancellation even when the suite itself is an infinite loop.
      const reason = await finished
      if (closing) await closing
      if (outputError) throw outputError
      return reason === 'closed' || reason === 'quit' ? lastExitCode : 130
    } finally {
      process.off('SIGINT', onSigint)
      process.off('SIGTERM', onSigterm)
      context.off('closed', onClosed)
      if (interactive) {
        stdin.off('data', onWatchKey)
        stdin.setRawMode(previousRaw)
        if (!previousRaw) stdin.pause()
      }
      await (closing || context.close())
    }
  } catch (error) {
    if (error && typeof error === 'object') {
      if (error instanceof WakeError && error.code === 'WAKE_CONFIG') {
        const mapped = new WakeError('WAKE_TEST_CONFIG', error.message, { cause: error })
        mapped.exitCode = 2
        throw mapped
      }
      error.exitCode = error.code === 'WAKE_CANCELLED' ? 130 : 2
      throw error
    }
    const wrapped = new WakeError('WAKE_TEST_RUNTIME', String(error))
    wrapped.exitCode = 2
    throw wrapped
  }
}

export async function runCli(argv = process.argv.slice(2)) {
  const args = [...argv]
  const noColor = takeFlag(args, '--no-color')
  const uiMode = validateChoice(takeOption(args, '--ui') || 'auto', '--ui', ['auto', 'tui', 'plain'])
  const ui = createUi(!noColor && supportsColor())

  if (takeFlag(args, '--version') || takeFlag(args, '-V')) {
    console.log(version())
    return 0
  }
  if (args.length === 0 || takeFlag(args, '--help') || takeFlag(args, '-h')) {
    console.log(HELP)
    return 0
  }

  const command = args.shift()
  if (command === 'test') {
    if (takeFlag(args, '--help') || takeFlag(args, '-h')) {
      console.log(HELP)
      return 0
    }
    ensureStaticMode(uiMode)
    return runTestCommand(args)
  }
  if (command === 'build') {
    ensureStaticMode(uiMode)
    const options = commonOptions(args)
    options.outdir = takeOption(args, '--outdir')
    options.cache = takeFlag(args, '--cache')
    options.sourceMap = takeFlag(args, '--sourcemap')
    if (args[0]) options.entry = args.shift()
    if (args.length) throw new Error(`unknown build arguments: ${args.join(' ')}`)
    printLines(formatBanner(ui, 'build', version()))
    printResult(ui, await build(options), 'Built')
    return 0
  }

  if (command === 'bundle') {
    ensureStaticMode(uiMode)
    const options = { configPath: takeOption(args, '--config', true) }
    options.outfile = takeOption(args, '--outfile', true)
    const platform = takeOption(args, '--platform', true)
    if (platform !== undefined) {
      options.platform = validateBundleChoice(platform, '--platform', ['browser', 'node'])
    }
    const format = takeOption(args, '--format', true)
    if (format !== undefined) {
      options.format = validateBundleChoice(format, '--format', ['iife', 'cjs'])
    }
    options.target = takeOption(args, '--target', true)
    options.external = takeOptions(args, '--external', true)
    options.minify = takeFlag(args, '--minify')
    options.sourceMap = takeFlag(args, '--sourcemap')
    options.cache = takeFlag(args, '--cache')
    options.entry = args.shift()
    if (!options.entry || !options.outfile) {
      throw usageError('bundle requires one entry and --outfile FILE')
    }
    if (args.length) throw usageError(`unknown bundle arguments: ${args.join(' ')}`)
    printLines(formatBanner(ui, 'bundle', version()))
    printResult(ui, await bundle(options), 'Bundled')
    return 0
  }

  if (command === 'library') {
    ensureStaticMode(uiMode)
    const action = args.shift()
    if (action !== 'build' && action !== 'token' && action !== 'docgen') {
      throw usageError('library requires build, token, or docgen')
    }
    const options = action === 'token'
      ? { configPath: takeOption(args, '--config', true) }
      : { entry: takeOption(args, '--entry', true) }
    if (args[0]) options.cwd = args.shift()
    if (args.length) throw usageError(`unknown library ${action} arguments: ${args.join(' ')}`)
    printLines(formatBanner(ui, `library ${action}`, version()))
    if (action === 'build') {
      printResult(ui, await buildLibrary(options), 'Built library')
      return 0
    }
    const result = action === 'token' ? await generateCssToken(options) : await generateDocgen(options)
    printLines(formatGeneratorResult(ui, result, action === 'token' ? 'Tokens generated' : 'Docgen generated'))
    return 0
  }

  if (command === 'federation') {
    ensureStaticMode(uiMode)
    const action = args.shift()
    if (action !== 'init' && action !== 'lock') {
      throw usageError('federation requires init or lock')
    }
    const cwd = args.shift() || '.'
    if (args.length) throw usageError(`unknown federation ${action} arguments: ${args.join(' ')}`)
    printLines(formatBanner(ui, `federation ${action}`, version()))
    if (action === 'init') {
      const result = await initializeFederation({ cwd })
      const unchanged = result.declaration === 'unchanged' && result.typesIndex === 'unchanged'
      printLines([
        `  ${ui.ok('✓')}  ${ui.bold(unchanged ? 'Already initialized' : 'Initialized')} federation types`,
        `     ${ui.dim('Project')}  ${ui.accent(result.projectRoot)}`,
        '',
      ])
      return 0
    }
    const result = await generateFederationLock({ cwd })
    printLines([
      `  ${ui.ok('✓')}  ${ui.bold(`Locked ${result.remotes} remote${result.remotes === 1 ? '' : 's'}`)}`,
      `     ${ui.dim('Output')}  ${ui.accent(result.lockPath)}`,
      '',
    ])
    return 0
  }

  if (command === 'dev') {
    const options = commonOptions(args)
    options.entry = takeOption(args, '--entry')
    options.host = takeOption(args, '--host')
    const port = takeOption(args, '--port')
    if (port) options.port = Number(port)
    options.open = takeFlag(args, '--open')
    if (args[0]) options.cwd = args.shift()
    if (args.length) throw new Error(`unknown dev arguments: ${args.join(' ')}`)
    const reason = await runServer(startDevServer, options, 'dev', options.cwd, ui, uiMode)
    if (reason === 'SIGINT') return 130
    if (reason === 'SIGTERM') return 143
    return 0
  }

  if (command === 'docs') {
    const action = args.shift()
    const options = commonOptions(args)
    options.outdir = takeOption(args, '--outdir')
    options.basePath = takeOption(args, '--base')
    options.mode = validateChoice(takeOption(args, '--mode') || 'site', '--mode', ['site', 'components'])
    options.host = takeOption(args, '--host')
    const port = takeOption(args, '--port')
    if (port) options.port = Number(port)
    options.open = takeFlag(args, '--open')
    if (args[0]) options.cwd = args.shift()
    if (args.length) throw new Error(`unknown docs arguments: ${args.join(' ')}`)
    if (action === 'build') {
      ensureStaticMode(uiMode)
      const components = options.mode === 'components'
      printLines(formatBanner(ui, components ? 'docs components build' : 'docs build', version()))
      const result = await buildDocs(options)
      const count = components ? (result.demos || []).length : (result.routes || []).length
      const noun = components ? 'demos' : 'routes'
      printResult(ui, result, components ? 'Component workbench built' : 'Documentation built', `  ${ui.dim('·')} ${count} ${noun}`)
      return 0
    }
    if (action === 'dev') {
      const commandName = options.mode === 'components' ? 'docs components' : 'docs dev'
      const reason = await runServer(startDocsDevServer, options, commandName, options.cwd, ui, uiMode)
      if (reason === 'SIGINT') return 130
      if (reason === 'SIGTERM') return 143
      return 0
    }
    throw new Error('docs requires build or dev')
  }

  if (command === 'parse' || command === 'tokenize') {
    ensureStaticMode(uiMode)
    const format = resolveFormat(takeOption(args, '--format'))
    const file = args.shift()
    if (!file || args.length) throw new Error(`${command} requires one source file`)
    const source = await readFile(file, 'utf8')
    if (format === 'human') printLines(formatBanner(ui, command, version()))

    if (command === 'tokenize') {
      const result = tokenize(source)
      if (format === 'json') {
        console.log(JSON.stringify(result, null, 2))
      } else {
        console.log('  START..END    KIND               TEXT')
        for (const token of result.tokens) {
          const newline = token.newlineBefore ? ' ↵' : ''
          console.log(
            `  ${String(token.start).padStart(5)}..${String(token.end).padEnd(5)} ${String(token.kind).padEnd(18)} ${JSON.stringify(token.text)}${newline}`,
          )
        }
      }
      return result.diagnostics?.some((diagnostic) => diagnostic.severity === 'error') ? 1 : 0
    }

    const module = parse(source, {
      sourceType: file.endsWith('.cjs') ? 'script' : 'module',
    })
    try {
      if (format === 'json') {
        console.log(JSON.stringify(module.summary, null, 2))
      } else {
        console.log(`Parsed ${file}`)
        console.log(`  Statements ${String(module.summary.statementCount).padEnd(8)} Dependencies ${module.summary.dependencies}`)
        console.log(`  Source bytes ${module.summary.sourceBytes}`)
      }
      return module.summary.diagnostics?.some((diagnostic) => diagnostic.severity === 'error') ? 1 : 0
    } finally {
      module.dispose()
    }
  }

  throw new Error(`unknown command: ${command}`)
}

try {
  process.exitCode = await runCli()
} catch (error) {
  const noColor = process.argv.includes('--no-color')
  const ui = createUi(!noColor && supportsColor())
  printLines(formatError(ui, error))
  process.exitCode = error.exitCode || 1
}
