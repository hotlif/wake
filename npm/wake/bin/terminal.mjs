import stringWidth from 'string-width'

import {
  InputEditor,
  TerminalInputDecoder,
  extractSelection,
  lineToCells,
  selectionContains,
} from './console.mjs'

const RESET = '\x1b[0m'
const BOLD = '\x1b[1m'
const DIM = '\x1b[2m'
const MAX_ACTIVITY = 200
const MAX_PROBLEMS = 100
const MAX_CHANGES = 50
const VIEWS = ['activity', 'problems', 'changes']
const SPINNER = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧']
const OPTIONAL_CLIPBOARD_MODULE = 'clip' + 'boardy'
const OPTIONAL_OPEN_MODULE = 'op' + 'en'

const defaultClipboard = {
  async read() {
    const { default: clipboard } = await import(OPTIONAL_CLIPBOARD_MODULE)
    return clipboard.read()
  },
  async write(value) {
    const { default: clipboard } = await import(OPTIONAL_CLIPBOARD_MODULE)
    return clipboard.write(value)
  },
}

async function defaultOpenUrl(value) {
  const { default: open } = await import(OPTIONAL_OPEN_MODULE)
  return open(value)
}

export function supportsColor(stream = process.stderr, env = process.env) {
  return stream.isTTY === true && !Object.hasOwn(env, 'NO_COLOR')
}

export function supportsTui(input = process.stdin, output = process.stderr, env = process.env) {
  return input.isTTY === true
    && output.isTTY === true
    && String(env.TERM || '').toLowerCase() !== 'dumb'
}

export function createUi(color = supportsColor(), env = process.env) {
  const trueColor = color && /^(truecolor|24bit)$/i.test(env.COLORTERM || '')
  const wrap = (indexed, rgb, text) => {
    if (!color) return String(text)
    const code = trueColor
      ? `\x1b[38;2;${rgb.join(';')}m`
      : `\x1b[38;5;${indexed}m`
    return `${code}${text}${RESET}`
  }
  return {
    color,
    accent: (text) => wrap(81, [34, 211, 238], text),
    bold: (text) => color ? `${BOLD}${text}${RESET}` : String(text),
    brand: (text) => color ? `${BOLD}${wrap(213, [217, 70, 239], text)}${RESET}` : String(text),
    dim: (text) => color ? `${DIM}${text}${RESET}` : String(text),
    error: (text) => wrap(204, [251, 113, 133], text),
    ok: (text) => wrap(114, [74, 222, 128], text),
    warn: (text) => wrap(214, [251, 191, 36], text),
  }
}

export function humanDuration(durationMs) {
  const milliseconds = Math.max(1, Number(durationMs) || 0)
  if (milliseconds < 1_000) return `${milliseconds.toFixed(0)}ms`
  if (milliseconds < 60_000) return `${(milliseconds / 1_000).toFixed(2)}s`
  const seconds = milliseconds / 1_000
  return `${Math.floor(seconds / 60)}m${(seconds % 60).toFixed(1)}s`
}

function moduleCount(count) {
  return `${count} module${count === 1 ? '' : 's'}`
}

function cacheHitCount(count) {
  return `${count} cache hit${count === 1 ? '' : 's'}`
}

export function humanRuntime(durationMs) {
  const seconds = Math.max(0, Math.floor(durationMs / 1_000))
  if (seconds < 60) return `${seconds}s`
  if (seconds < 3_600) return `${Math.floor(seconds / 60)}m${String(seconds % 60).padStart(2, '0')}s`
  return `${Math.floor(seconds / 3_600)}h${String(Math.floor((seconds % 3_600) / 60)).padStart(2, '0')}m`
}

export function humanBytes(bytes) {
  const value = Number(bytes) || 0
  if (value >= 1024 * 1024) return `${(value / 1024 / 1024).toFixed(2)} MB`
  if (value >= 1024) return `${(value / 1024).toFixed(1)} KB`
  return `${value} B`
}

export function formatBanner(ui, command, currentVersion) {
  return [
    '',
    `  ${ui.warn('⚡')} ${ui.brand('WAKE')} ${ui.dim('/')} ${ui.bold(command.toUpperCase())}  ${ui.dim(`v${currentVersion}`)}`,
    '',
  ]
}

export function formatBuildResult(ui, result, label = 'Built', extra = '') {
  const bytes = (result.files || []).reduce((sum, file) => sum + Number(file.bytes || 0), 0)
  const lines = [
    `  ${ui.ok('✓')}  ${ui.bold(label)} ${ui.accent(`in ${humanDuration(result.durationMs)}`)}`,
    `     ${ui.accent(`${result.moduleCount} modules`)} ${ui.dim('·')} ${(result.files || []).length} ${ui.dim('files')} ${ui.dim('·')} ${ui.accent(humanBytes(bytes))}${extra}`,
  ]
  if (result.outputDir) lines.push(`     ${ui.dim('Output')}  ${ui.accent(result.outputDir)}`)
  for (const diagnostic of result.diagnostics || []) {
    lines.push(...formatDiagnostic(ui, diagnostic).map((line) => `     ${line}`))
  }
  lines.push('')
  return lines
}

export function formatGeneratorResult(ui, result, label) {
  const bytes = (result.files || []).reduce((sum, file) => sum + Number(file.bytes || 0), 0)
  return [
    `  ${ui.ok('✓')}  ${ui.bold(label)} ${ui.accent(`in ${humanDuration(result.durationMs)}`)}`,
    `     ${(result.files || []).length} ${ui.dim('files')} ${ui.dim('·')} ${ui.accent(humanBytes(bytes))}`,
    `     ${ui.dim('Output')}  ${ui.accent(result.outputFile)}`,
    '',
  ]
}

export function formatServerReady(ui, url, metrics) {
  const lines = [
    `  ${ui.ok('✓')}  ${ui.bold('Development server ready')}`,
    `     ${ui.dim('Local')}  ${ui.accent(url)}`,
  ]
  if (metrics) {
    lines.push(
      `     ${ui.accent(`${metrics.modules} modules`)} ${ui.dim('·')} ${metrics.chunks} ${ui.dim('chunks')} ${ui.dim('·')} ${metrics.assets} ${ui.dim('assets')}`,
    )
  }
  lines.push(`     ${ui.dim('Press Ctrl-C to stop')}`, '')
  return lines
}

export function formatError(ui, error) {
  const code = error?.code ? `[${error.code}]` : '[WAKE_INTERNAL]'
  const lines = [
    `  ${ui.error('✗')}  ${ui.bold('Wake failed')} ${ui.error(code)}`,
    `     ${error?.message || error}`,
  ]
  if (error?.path) lines.push(`     ${ui.dim('Path')}  ${ui.accent(error.path)}`)
  for (const diagnostic of error?.diagnostics || []) {
    lines.push(...formatDiagnostic(ui, diagnostic).map((line) => `     ${line}`))
  }
  lines.push('')
  return lines
}

export function formatDiagnostic(ui, diagnostic) {
  const severity = String(diagnostic?.severity || 'error').toUpperCase()
  const heading = diagnostic?.code
    ? `${severity} [${diagnostic.code}]: ${diagnostic.message}`
    : `${severity}: ${diagnostic?.message || ''}`
  const lines = [
    diagnostic?.severity === 'warning'
      ? ui.warn(heading)
      : diagnostic?.severity === 'error'
        ? ui.error(heading)
        : ui.accent(heading),
  ]
  const location = diagnostic?.location
  if (diagnostic?.path && location) {
    lines.push(` ${ui.dim('-->')} ${ui.accent(diagnostic.path)}:${location.line}:${location.column}`)
  } else if (diagnostic?.path) {
    lines.push(` ${ui.dim('-->')} ${ui.accent(diagnostic.path)}`)
  }
  if (location) {
    const lineText = expandTabs(String(location.lineText || ''))
    const gutter = String(location.line).length
    const start = displayColumn(String(location.lineText || ''), location.column)
    const end = location.endLine === location.line
      ? displayColumn(String(location.lineText || ''), location.endColumn)
      : stringWidth(lineText)
    const width = Math.max(1, end - start)
    const label = location.label ? ` ${location.label}` : ''
    lines.push(`${' '.repeat(gutter)} ${ui.dim('|')}`)
    lines.push(`${String(location.line).padStart(gutter)} ${ui.dim('|')} ${lineText}`)
    lines.push(`${' '.repeat(gutter)} ${ui.dim('|')} ${ui.error(`${' '.repeat(start)}${'^'.repeat(width)}${label}`)}`)
  }
  for (const note of diagnostic?.notes || []) lines.push(`  ${ui.dim('=')} note: ${note}`)
  return lines
}

function displayColumn(line, oneBasedColumn) {
  const prefix = [...line].slice(0, Math.max(0, Number(oneBasedColumn || 1) - 1)).join('')
  return stringWidth(expandTabs(prefix))
}

function expandTabs(line) {
  const tabStop = 4
  let width = 0
  let value = ''
  for (const character of line) {
    if (character === '\t') {
      const spaces = tabStop - (width % tabStop)
      value += ' '.repeat(spaces)
      width += spaces
    } else {
      value += character
      width += stringWidth(character)
    }
  }
  return value
}

export function formatFinalSummary(ui, state, reason, label = 'Server stopped') {
  const lines = [
    '',
    `  ${ui.dim('■')}  ${ui.bold(label)} ${ui.dim(`(${reason})`)}`,
  ]
  if (state.endpoint) {
    lines.push(`     ${ui.dim(state.endpointLabel)}  ${ui.accent(state.endpoint)}`)
  }
  lines.push(
    `     ${state.rebuilds} rebuilds ${ui.dim('·')} runtime ${ui.accent(humanRuntime(Date.now() - state.startedAt))}`,
    '',
  )
  return lines
}

export function observeServer(server, ui, output = console) {
  const onRebuildStart = (event) => {
    const count = event.changedPaths?.length || 0
    const detail = count === 1
      ? 'Rebuilding after 1 file change…'
      : count > 1
        ? `Rebuilding after ${count} file changes…`
        : 'Rebuilding…'
    output.error(`  ${ui.warn('↻')}  ${ui.dim(detail)}`)
  }
  const onRebuilt = (event) => {
    if (event.initial) return
    output.error(
      `  ${ui.ok('✓')}  ${ui.bold('Updated')}  ${ui.dim('·')}  ${ui.accent(moduleCount(event.updatedModules))}  ${ui.dim('·')}  ${ui.accent(cacheHitCount(event.cachedModules))}  ${ui.accent(humanDuration(event.durationMs))}`,
    )
  }
  const onDiagnostic = (diagnostic) => {
    for (const [index, line] of formatDiagnostic(ui, diagnostic).entries()) {
      output.error(index === 0 ? `  ${ui.error('✗')}  ${ui.bold(line)}` : `     ${line}`)
    }
  }

  server.on('rebuildStart', onRebuildStart)
  server.on('rebuilt', onRebuilt)
  server.on('diagnostic', onDiagnostic)
  return () => {
    server.off('rebuildStart', onRebuildStart)
    server.off('rebuilt', onRebuilt)
    server.off('diagnostic', onDiagnostic)
  }
}

function pushActivity(state, level, message) {
  const before = activityRowCount(state)
  if (state.activity.length === MAX_ACTIVITY) state.activity.shift()
  state.activity.push({
    elapsedMs: Date.now() - state.startedAt,
    level,
    message: String(message),
  })
  noteViewUpdate(state, 'activity', before, activityRowCount(state))
}

function diagnosticLevel(severity) {
  if (String(severity).toLowerCase() === 'error') return 'error'
  if (String(severity).toLowerCase() === 'warning') return 'warning'
  return 'info'
}

function pushProblem(state, diagnostic, rendered) {
  const before = problemRowCount(state)
  if (state.problems.length === MAX_PROBLEMS) state.problems.shift()
  state.problems.push({
    elapsedMs: Date.now() - state.startedAt,
    diagnostic,
    rendered,
  })
  noteViewUpdate(state, 'problems', before, problemRowCount(state))
}

function pushChange(state, event) {
  const before = changeRowCount(state)
  if (state.changes.length === MAX_CHANGES) state.changes.shift()
  state.changes.push({
    elapsedMs: Date.now() - state.startedAt,
    changedPaths: [...(event.changedPaths || [])],
    workspace: event.workspace,
    basePath: event.basePath,
    metrics: undefined,
  })
  noteViewUpdate(state, 'changes', before, changeRowCount(state))
}

function completeChange(state, event) {
  if (event.initial) return
  const before = changeRowCount(state)
  let change
  for (let index = state.changes.length - 1; index >= 0; index -= 1) {
    const candidate = state.changes[index]
    if (!candidate.metrics
      && candidate.workspace === event.workspace
      && candidate.basePath === event.basePath) {
      change = candidate
      break
    }
  }
  const metrics = {
    modules: event.modules,
    updatedModules: event.updatedModules,
    cachedModules: event.cachedModules,
    chunks: event.chunks,
    assets: event.assets,
    durationMs: event.durationMs,
  }
  if (change) change.metrics = metrics
  else {
    if (state.changes.length === MAX_CHANGES) state.changes.shift()
    state.changes.push({
      elapsedMs: Date.now() - state.startedAt,
      changedPaths: [],
      workspace: event.workspace,
      basePath: event.basePath,
      metrics,
    })
  }
  noteViewUpdate(state, 'changes', before, changeRowCount(state))
}

function noteViewUpdate(state, view, before, after) {
  const scroll = state.scroll[view]
  if (scroll.fromBottom === 0) return
  scroll.fromBottom = Math.max(0, scroll.fromBottom + after - before)
  scroll.unread += 1
}

function clearDashboardHistory(state) {
  state.activity.length = 0
  state.problems.length = 0
  state.changes.length = 0
  for (const view of VIEWS) state.scroll[view] = { fromBottom: 0, unread: 0 }
}

export function createDashboardState({
  command,
  root = '.',
  endpointLabel = 'LOCAL',
  watchLabel = 'HMR · source maps · watching',
}) {
  const state = {
    command,
    root,
    endpointLabel,
    endpoint: '',
    watchLabel,
    status: 'starting',
    metrics: undefined,
    rebuilds: 0,
    startedAt: Date.now(),
    activity: [],
    problems: [],
    changes: [],
    workspaceState: undefined,
    view: 'activity',
    scroll: Object.fromEntries(VIEWS.map((view) => [view, { fromBottom: 0, unread: 0 }])),
  }
  pushActivity(state, 'info', 'Starting Wake…')
  return state
}

export function applyDashboardEvent(state, event) {
  if (event.type === 'rebuildStart') {
    const count = event.changedPaths?.length || 0
    state.status = 'rebuilding'
    pushChange(state, event)
    pushActivity(
      state,
      'warning',
      count === 1
        ? 'Rebuilding after 1 file change…'
        : count > 1
          ? `Rebuilding after ${count} file changes…`
          : 'Rebuilding…',
    )
  } else if (event.type === 'rebuilt') {
    state.status = 'ready'
    state.metrics = {
      modules: event.modules,
      updatedModules: event.updatedModules,
      cachedModules: event.cachedModules,
      chunks: event.chunks,
      assets: event.assets,
      durationMs: event.durationMs,
    }
    if (!event.initial) state.rebuilds += 1
    completeChange(state, event)
    pushActivity(
      state,
      'success',
      event.initial
        ? `Initial build completed: ${event.modules} modules in ${humanDuration(event.durationMs)}`
        : `Updated ${moduleCount(event.updatedModules)} · ${cacheHitCount(event.cachedModules)} in ${humanDuration(event.durationMs)}`,
    )
  } else if (event.type === 'diagnostic') {
    const level = diagnosticLevel(event.diagnostic?.severity)
    if (level === 'error') state.status = 'error'
    const rendered = formatDiagnostic(createUi(false), event.diagnostic).join('\n')
    pushProblem(state, event.diagnostic, rendered)
    pushActivity(state, level, rendered)
  } else if (event.type === 'workspaceState') {
    state.workspaceState = {
      total: event.total,
      loaded: event.loaded,
      failed: event.failed,
      current: event.current,
      failedNames: event.failedNames || [],
    }
    if (event.current) {
      pushActivity(state, 'info', `Loading workspace ${event.current}…`)
    }
  } else if (event.type === 'closed') {
    state.status = 'stopped'
    pushActivity(state, 'info', 'Wake stopped')
  }
}

export function setDashboardEndpoint(state, endpoint) {
  state.endpoint = String(endpoint)
}

export function setDashboardStopping(state, reason) {
  state.status = 'stopping'
  pushActivity(state, 'info', `Stopping (${reason})…`)
}

export function setDashboardStopped(state) {
  state.status = 'stopped'
  pushActivity(state, 'info', 'Wake stopped')
}

function statusLabel(status) {
  return {
    starting: 'STARTING',
    ready: 'READY',
    rebuilding: 'REBUILDING',
    error: 'ERROR',
    stopping: 'STOPPING',
    stopped: 'STOPPED',
  }[status] || 'STARTING'
}

function statusSymbol(state) {
  if (state.status === 'ready') return '✓'
  if (state.status === 'error') return '✗'
  if (state.status === 'stopping') return '◌'
  if (state.status === 'stopped') return '■'
  return SPINNER[Math.floor((Date.now() - state.startedAt) / 100) % SPINNER.length]
}

function elapsedStamp(durationMs) {
  const seconds = Math.floor(durationMs / 1_000)
  return `+${String(Math.floor(seconds / 60) % 100).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`
}

function charLength(text) {
  return stringWidth(stripAnsi(String(text)))
}

function truncate(text, width) {
  const chars = [...String(text)]
  if (charLength(text) <= width) return String(text)
  if (width <= 1) return chars.slice(0, width).join('')
  let value = ''
  for (const character of chars) {
    if (charLength(value + character) > width - 1) break
    value += character
  }
  return `${value}…`
}

function pad(text, width) {
  const value = truncate(text, width)
  return value + ' '.repeat(Math.max(0, width - charLength(value)))
}

function boxLine(text, width) {
  if (width < 4) return pad(text, width)
  return `│ ${pad(text, width - 4)} │`
}

function topBorder(state, width) {
  const title = ` ⚡ WAKE / ${state.command.toUpperCase()}  v${state.version || ''} `
  if (width < 4) return '─'.repeat(width)
  const clipped = truncate(title, width - 2)
  return `╭${clipped}${'─'.repeat(Math.max(0, width - 2 - charLength(clipped)))}╮`
}

function separator(width, title = '') {
  if (width < 4) return '─'.repeat(width)
  const center = title ? ` ${title} ` : ''
  return `├${center}${'─'.repeat(Math.max(0, width - 2 - charLength(center)))}┤`
}

function bottomBorder(width) {
  return width < 4 ? '─'.repeat(width) : `╰${'─'.repeat(width - 2)}╯`
}

function metricsText(state) {
  const metrics = state.metrics
  if (!metrics) return 'BUILD   waiting for metrics…'
  return `BUILD   ${metrics.modules} modules · ${metrics.chunks} chunks · ${metrics.assets} assets · ${humanDuration(metrics.durationMs)}`
}

function eventRows(elapsedMs, level, message) {
  const symbol = { info: '·', success: '✓', warning: '⚠', error: '✗' }[level]
  return String(message).split('\n').map((line, index) => index === 0
    ? `${elapsedStamp(elapsedMs)}  ${symbol} ${line}`
    : `          ${line}`)
}

function activityRows(state) {
  return state.activity.flatMap((item) => eventRows(item.elapsedMs, item.level, item.message))
}

function problemRows(state) {
  return state.problems.flatMap((problem) => eventRows(
    problem.elapsedMs,
    diagnosticLevel(problem.diagnostic?.severity),
    problem.rendered,
  ))
}

function changeRows(state) {
  return state.changes.flatMap((change) => {
    const target = change.workspace && change.basePath
      ? `${change.workspace} · ${change.basePath}`
      : change.workspace || change.basePath || 'project'
    const rows = [`${elapsedStamp(change.elapsedMs)}  ↻ ${target}`]
    if (change.changedPaths.length === 0) rows.push('          Changed files unavailable')
    else rows.push(...change.changedPaths.map((path) => `          ${path}`))
    rows.push(change.metrics
      ? `          ✓ ${change.metrics.updatedModules} updated · ${change.metrics.cachedModules} cache hits · ${humanDuration(change.metrics.durationMs)}`
      : '          … rebuilding…')
    return rows
  })
}

function emptyViewRows(view) {
  if (view === 'problems') return ['No recent problems', 'Warnings and errors will appear here with source context.']
  if (view === 'changes') return ['No rebuilds yet', 'Edit a file to trigger a rebuild.']
  return ['No activity yet', 'Wake events will appear here.']
}

function allViewRows(state, view = state.view) {
  if (view === 'problems') return problemRows(state)
  if (view === 'changes') return changeRows(state)
  return activityRows(state)
}

function visibleViewRows(state, available) {
  let rows = allViewRows(state)
  if (rows.length === 0) rows = emptyViewRows(state.view)
  const end = Math.max(0, rows.length - state.scroll[state.view].fromBottom)
  const start = Math.max(0, end - Math.max(1, available))
  return rows.slice(start, end)
}

function workspaceText(state) {
  const workspaces = state.workspaceState
  if (!workspaces) return undefined
  const current = workspaces.current ? ` · loading ${workspaces.current}` : ''
  const failedNames = workspaces.failedNames?.length ? `: ${workspaces.failedNames.join(', ')}` : ''
  const failed = workspaces.failed ? ` · ${workspaces.failed} failed${failedNames}` : ''
  return `WORKSPACES  ${workspaces.loaded}/${workspaces.total} loaded${failed}${current}`
}

function activityRowCount(state) {
  return state.activity.reduce((count, item) => count + String(item.message).split('\n').length, 0)
}

function problemRowCount(state) {
  return state.problems.reduce((count, problem) => count + String(problem.rendered).split('\n').length, 0)
}

function changeRowCount(state) {
  return state.changes.reduce((count, change) => count + Math.max(1, change.changedPaths.length) + 2, 0)
}

function viewRowCount(state) {
  if (state.view === 'problems') return problemRowCount(state)
  if (state.view === 'changes') return changeRowCount(state)
  return activityRowCount(state)
}

function viewTitle(state) {
  return VIEWS.map((view) => {
    const label = view === 'activity'
      ? 'ACTIVITY'
      : view === 'problems'
        ? `RECENT PROBLEMS ${state.problems.length}`
        : `CHANGES ${state.changes.length}`
    const unread = state.scroll[view].unread ? ` +${state.scroll[view].unread}` : ''
    return state.view === view ? `[${label}${unread}]` : `${label}${unread}`
  }).join(' · ')
}

function cycleView(state, direction) {
  const index = VIEWS.indexOf(state.view)
  state.view = VIEWS[(index + direction + VIEWS.length) % VIEWS.length]
}

function scrollView(state, amount) {
  const scroll = state.scroll[state.view]
  const maximum = Math.max(0, viewRowCount(state) - 1)
  scroll.fromBottom = Math.max(0, Math.min(maximum, scroll.fromBottom + amount))
  if (scroll.fromBottom === 0) scroll.unread = 0
}

function scrollViewToBottom(state) {
  state.scroll[state.view] = { fromBottom: 0, unread: 0 }
}

function plainFrame(state, width, height, editor = new InputEditor(), notice) {
  width = Math.max(10, width || 80)
  height = Math.max(6, height || 24)
  const runtime = humanRuntime(Date.now() - state.startedAt)
  const header = `${statusSymbol(state)} ${statusLabel(state.status)}   uptime ${runtime}   ${state.rebuilds} rebuilds`
  const endpoint = state.endpoint || 'waiting…'
  let fixed
  let activityHeight

  if (width < 60 || height < 14) {
    const latest = String(state.problems.at(-1)?.rendered || state.activity.at(-1)?.message || 'Starting Wake…').split('\n')
    fixed = [topBorder(state, width), boxLine(header, width)]
    const diagnosticRows = latest.length > 1
    for (const line of latest.slice(0, Math.max(1, height - (diagnosticRows ? 5 : 6)))) {
      fixed.push(boxLine(line, width))
    }
    if (!diagnosticRows) fixed.push(boxLine('Resize for views · type help for commands', width))
    activityHeight = 0
  } else if (width < 80 || height < 20) {
    fixed = [
      topBorder(state, width),
      boxLine(header, width),
      boxLine(`${state.endpointLabel}  ${endpoint}`, width),
      boxLine(metricsText(state), width),
      separator(width, viewTitle(state)),
    ]
    if (workspaceText(state)) fixed.splice(-1, 0, boxLine(workspaceText(state), width))
    activityHeight = Math.max(1, height - fixed.length - 2)
  } else {
    fixed = [
      topBorder(state, width),
      boxLine(header, width),
      separator(width),
      boxLine(`${state.endpointLabel.padEnd(7)} ${endpoint}`, width),
      boxLine(`ROOT    ${state.root}`, width),
      boxLine(`MODE    ${state.watchLabel}`, width),
      separator(width),
      boxLine(metricsText(state), width),
      separator(width, viewTitle(state)),
    ]
    if (workspaceText(state)) fixed.splice(-1, 0, boxLine(workspaceText(state), width))
    activityHeight = Math.max(1, height - fixed.length - 2)
  }

  for (const row of visibleViewRows(state, activityHeight)) fixed.push(boxLine(row, width))
  while (fixed.length < height - 3) fixed.push(boxLine('', width))
  fixed.splice(Math.max(0, height - 3))
  const visible = editor.visible(Math.max(0, width - 6))
  fixed.push(boxLine(
    notice
      ? `${notice.error ? '✗' : '✓'} ${notice.message}`
      : 'Tab views · PgUp/PgDn scroll · Enter command · drag copy · Ctrl-C quit',
    width,
  ))
  fixed.push(boxLine(`› ${visible.text}`, width))
  fixed.push(bottomBorder(width))
  return { lines: fixed.slice(0, height), cursor: visible.cursor }
}

function colorizeLine(line, ui) {
  if (!ui.color) return line
  let value = line
  value = value.replace('WAKE', ui.brand('WAKE'))
  value = value.replace(/(✓|■)/, (match) => ui.ok(match))
  value = value.replace(/(✗)/, (match) => ui.error(match))
  value = value.replace(/(⚠|↻|⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|◌)/, (match) => ui.warn(match))
  value = value.replace(/(https?:\/\/\S+)/, (match) => ui.accent(match))
  return value
}

function renderDashboardFrame(
  state,
  width = 80,
  height = 24,
  ui = createUi(false),
  editor = new InputEditor(),
  selection,
  notice,
) {
  const frame = plainFrame(state, width, height, editor, notice)
  const rows = frame.lines.map((line) => lineToCells(line, width))
  const rendered = rows.map((cells, y) => {
    let selected = false
    let line = ''
    for (let x = 0; x < cells.length; x += 1) {
      const nextSelected = selectionContains(selection, x, y)
      if (nextSelected !== selected) {
        line += nextSelected ? '\x1b[7m' : RESET
        selected = nextSelected
      }
      line += cells[x]
    }
    if (selected) line += RESET
    return colorizeLine(line, ui)
  })
  return {
    text: rendered.join('\n'),
    rows,
    inputY: Math.max(0, rows.length - 2),
    inputX: 4,
    cursorX: Math.min(width - 2, 4 + frame.cursor),
  }
}

export function renderDashboard(state, width = 80, height = 24, ui = createUi(false)) {
  return renderDashboardFrame(state, width, height, ui).text
}

export function createDashboardSession(
  state,
  {
    input = process.stdin,
    output = process.stderr,
    ui = createUi(),
    clipboardAdapter = defaultClipboard,
    openUrl = defaultOpenUrl,
    env = process.env,
  } = {},
) {
  let closed = false
  let previousRaw = false
  const editor = new InputEditor()
  const decoder = new TerminalInputDecoder()
  let rows = []
  let inputY = 0
  let inputX = 0
  let selection
  let dragStart
  let lastSelection = ''
  let notice
  let eventQueue = Promise.resolve()
  let resolveExit
  const exit = new Promise((resolve) => { resolveExit = resolve })

  const draw = () => {
    if (closed) return
    const width = output.columns || 80
    const height = output.rows || 24
    if (notice && Date.now() - notice.startedAt >= 1500) notice = undefined
    const frame = renderDashboardFrame(state, width, height, ui, editor, selection, notice)
    rows = frame.rows
    inputY = frame.inputY
    inputX = frame.inputX
    output.write(`\x1b[H${frame.text}\x1b[J\x1b[${inputY + 1};${frame.cursorX + 1}H\x1b[?25h`)
  }
  const requestExit = (reason) => {
    if (!closed) resolveExit(reason)
  }
  const setNotice = (message, error = false) => {
    notice = { message, error, startedAt: Date.now() }
    draw()
  }
  const osc52 = (value) => {
    const sequence = `\x1b]52;c;${Buffer.from(value).toString('base64')}\x07`
    output.write(env.TMUX ? `\x1bPtmux;${sequence.replaceAll('\x1b', '\x1b\x1b')}\x1b\\` : sequence)
  }
  const copyText = async (value) => {
    if (env.SSH_CONNECTION || env.SSH_TTY) {
      osc52(value)
      setNotice('Copied to clipboard')
      return
    }
    try {
      await clipboardAdapter.write(value)
      setNotice('Copied to clipboard')
    } catch {
      try {
        osc52(value)
        setNotice('Copied to clipboard')
      } catch {
        setNotice('Failed to copy; terminal clipboard is unavailable', true)
      }
    }
  }
  const pasteClipboard = async () => {
    try {
      editor.insertPaste(await clipboardAdapter.read())
      selection = undefined
      draw()
    } catch {
      setNotice('Clipboard paste is unavailable; use terminal paste', true)
    }
  }
  const submit = async () => {
    const result = editor.submit()
    if (result.error) pushActivity(state, 'warning', result.error)
    else if (result.command === 'help') pushActivity(state, 'info', 'Commands: help · clear · open · quit (a leading / is optional)')
    else if (result.command === 'clear') {
      clearDashboardHistory(state)
      setNotice('Dashboard history cleared')
    } else if (result.command === 'open') {
      if (!/^https?:\/\//u.test(state.endpoint)) pushActivity(state, 'warning', 'No development-server URL is available to open')
      else {
        try {
          await openUrl(state.endpoint)
          setNotice('Opened development server')
        } catch (error) {
          pushActivity(state, 'warning', `Failed to open ${state.endpoint}: ${error.message || error}`)
        }
      }
    } else if (result.command === 'quit') requestExit('q')
    draw()
  }
  const handleEvent = async (event) => {
    if (event.type === 'paste') editor.insertPaste(event.value)
    else if (event.type === 'text') {
      editor.insert(event.value)
      selection = undefined
    } else if (event.type === 'mouse') {
      const position = { x: event.x, y: event.y }
      if (event.kind === 'down' && event.button === 'left') {
        dragStart = position
        selection = { start: position, end: position }
      } else if (event.kind === 'drag' && dragStart) selection = { start: dragStart, end: position }
      else if (event.kind === 'up' && event.button === 'left' && dragStart) {
        selection = { start: dragStart, end: position }
        dragStart = undefined
        const value = extractSelection(rows, selection)
        if (value) {
          lastSelection = value
          await copyText(value)
        } else {
          if (position.y === inputY) editor.setCursorFromCell(Math.max(0, position.x - inputX))
          selection = undefined
        }
      } else if (event.kind === 'down' && event.button === 'right' && position.y === inputY) await pasteClipboard()
      else if (event.kind === 'scroll-up') scrollView(state, 3)
      else if (event.kind === 'scroll-down') scrollView(state, -3)
    } else if (event.type === 'key') {
      if (event.key === 'ctrl-c') requestExit('SIGINT')
      else if (event.key === 'ctrl-y') lastSelection ? await copyText(lastSelection) : setNotice('No selected text to copy', true)
      else if (event.key === 'ctrl-v') await pasteClipboard()
      else if (event.key === 'enter') await submit()
      else if (event.key === 'tab') { cycleView(state, 1); selection = undefined }
      else if (event.key === 'shift-tab') { cycleView(state, -1); selection = undefined }
      else if (event.key === 'escape') { editor.clear(); selection = undefined }
      else if (event.key === 'left') editor.moveLeft()
      else if (event.key === 'right') editor.moveRight()
      else if (event.key === 'home') editor.moveHome()
      else if (event.key === 'end') editor.moveEnd()
      else if (event.key === 'ctrl-end') scrollViewToBottom(state)
      else if (event.key === 'backspace') editor.backspace()
      else if (event.key === 'delete') editor.delete()
      else if (event.key === 'up') editor.historyPrevious()
      else if (event.key === 'down') editor.historyNext()
      else if (event.key === 'pageup') scrollView(state, 10)
      else if (event.key === 'pagedown') scrollView(state, -10)
    }
    draw()
  }
  const onData = (chunk) => {
    for (const event of decoder.push(chunk)) {
      eventQueue = eventQueue.then(() => handleEvent(event))
    }
  }
  const onResize = () => draw()

  previousRaw = input.isRaw === true
  if (typeof input.setRawMode === 'function') input.setRawMode(true)
  input.resume?.()
  input.on('data', onData)
  output.on?.('resize', onResize)
  output.write('\x1b[?1049h\x1b[?1002h\x1b[?1006h\x1b[?2004h\x1b[?25l\x1b[2J')
  const timer = setInterval(draw, 100)
  timer.unref?.()
  draw()

  return {
    draw,
    exit,
    requestExit,
    close() {
      if (closed) return
      closed = true
      clearInterval(timer)
      input.off('data', onData)
      output.off?.('resize', onResize)
      if (typeof input.setRawMode === 'function') input.setRawMode(previousRaw)
      if (!previousRaw) input.pause?.()
      output.write('\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?25h\x1b[?1049l')
    },
  }
}

export function stripAnsi(value) {
  return String(value).replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '')
}
