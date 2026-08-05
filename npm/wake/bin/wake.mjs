#!/usr/bin/env node

import { readFile } from 'node:fs/promises'
import {
  build,
  buildDocs,
  startDevServer,
  startDocsDevServer,
  version,
} from '../index.mjs'
import { parse, tokenize } from '../experimental.mjs'
import {
  applyDashboardEvent,
  createDashboardSession,
  createDashboardState,
  createUi,
  formatBanner,
  formatBuildResult,
  formatError,
  formatFinalSummary,
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
  wake dev [root] [--entry FILE] [--host HOST] [--port PORT] [--open]
  wake docs build [root] [--mode site|components] [--outdir DIR] [--base PATH]
  wake docs dev [root] [--mode site|components] [--host HOST] [--port PORT] [--open]
  wake parse <file> [--format auto|human|json]
  wake tokenize <file> [--format auto|human|json]
  wake --version

Options:
  --ui MODE   Terminal UI mode for long-running commands (default: auto)
  --no-color  Disable terminal colors; also honors NO_COLOR
  --format    Human or JSON output for parse/tokenize (default: auto)
`

function takeOption(args, name) {
  const index = args.indexOf(name)
  if (index === -1) return undefined
  if (index + 1 >= args.length) throw new Error(`${name} requires a value`)
  const [value] = args.splice(index + 1, 1)
  args.splice(index, 1)
  return value
}

function takeFlag(args, name) {
  const index = args.indexOf(name)
  if (index === -1) return false
  args.splice(index, 1)
  return true
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
      ? 'MDX · HMR · search index · watching'
      : command === 'docs components'
        ? 'Demo · Controls · HMR · watching'
        : 'HMR · source maps · watching',
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
      applyDashboardEvent(state, { type: 'diagnostic', message: diagnostic.message })
      dashboard?.draw()
    }
    const onClosed = () => {
      applyDashboardEvent(state, { type: 'closed' })
      dashboard?.draw()
    }
    server.on('rebuildStart', onRebuildStart)
    server.on('rebuilt', onRebuilt)
    server.on('diagnostic', onDiagnostic)
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
  process.exitCode = 1
}
