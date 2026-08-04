const RESET = '\x1b[0m'
const BOLD = '\x1b[1m'
const DIM = '\x1b[2m'
const MAX_ACTIVITY = 200
const SPINNER = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧']

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
    lines.push(`     ${ui.warn(String(diagnostic.severity || 'warning').toUpperCase())}  ${diagnostic.message}`)
  }
  lines.push('')
  return lines
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
    const diagnosticCode = diagnostic.code ? `[${diagnostic.code}] ` : ''
    lines.push(
      `     ${ui.warn(String(diagnostic.severity || 'error').toUpperCase())}  ${diagnosticCode}${diagnostic.message}`,
    )
    for (const note of diagnostic.notes || []) lines.push(`        ${ui.dim('·')} ${note}`)
  }
  lines.push('')
  return lines
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
    const code = diagnostic.code ? `[${diagnostic.code}] ` : ''
    output.error(`  ${ui.error('✗')}  ${ui.bold('Build failed')}  ${code}${diagnostic.message}`)
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
  if (state.activity.length === MAX_ACTIVITY) state.activity.shift()
  state.activity.push({
    elapsedMs: Date.now() - state.startedAt,
    level,
    message: String(message),
  })
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
    scrollFromBottom: 0,
  }
  pushActivity(state, 'info', 'Starting Wake…')
  return state
}

export function applyDashboardEvent(state, event) {
  if (event.type === 'rebuildStart') {
    const count = event.changedPaths?.length || 0
    state.status = 'rebuilding'
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
    pushActivity(
      state,
      'success',
      event.initial
        ? `Initial build completed: ${event.modules} modules in ${humanDuration(event.durationMs)}`
        : `Updated ${moduleCount(event.updatedModules)} · ${cacheHitCount(event.cachedModules)} in ${humanDuration(event.durationMs)}`,
    )
  } else if (event.type === 'diagnostic') {
    state.status = 'error'
    pushActivity(state, 'error', event.message)
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
  return [...stripAnsi(String(text))].length
}

function truncate(text, width) {
  const chars = [...String(text)]
  if (chars.length <= width) return String(text)
  if (width <= 1) return chars.slice(0, width).join('')
  return `${chars.slice(0, width - 1).join('')}…`
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

function activityRows(state, available) {
  const end = Math.max(0, state.activity.length - state.scrollFromBottom)
  const start = Math.max(0, end - Math.max(1, available))
  return state.activity.slice(start, end).map((item) => {
    const symbol = { info: '·', success: '✓', warning: '↻', error: '✗' }[item.level]
    return `${elapsedStamp(item.elapsedMs)}  ${symbol} ${String(item.message).replaceAll('\n', ' ')}`
  })
}

function plainFrame(state, width, height) {
  width = Math.max(10, width || 80)
  height = Math.max(6, height || 24)
  const runtime = humanRuntime(Date.now() - state.startedAt)
  const header = `${statusSymbol(state)} ${statusLabel(state.status)}   uptime ${runtime}   ${state.rebuilds} rebuilds`
  const endpoint = state.endpoint || 'waiting…'
  let fixed
  let activityHeight

  if (width < 60 || height < 14) {
    fixed = [
      topBorder(state, width),
      boxLine(header, width),
      boxLine(state.endpoint || state.watchLabel, width),
      boxLine(state.activity.at(-1)?.message || 'Starting Wake…', width),
      boxLine('Resize for details · q/Ctrl-C quit', width),
    ]
    activityHeight = 0
  } else if (width < 80 || height < 20) {
    fixed = [
      topBorder(state, width),
      boxLine(header, width),
      boxLine(`${state.endpointLabel}  ${endpoint}`, width),
      boxLine(metricsText(state), width),
      separator(width, 'ACTIVITY'),
    ]
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
      separator(width, 'ACTIVITY'),
    ]
    activityHeight = Math.max(1, height - fixed.length - 2)
  }

  for (const row of activityRows(state, activityHeight)) fixed.push(boxLine(row, width))
  while (activityHeight > 0 && fixed.length < height - 2) fixed.push(boxLine('', width))
  if (height >= 14) fixed.push(boxLine('↑↓/PgUp/PgDn scroll · End follow · c clear · q/Ctrl-C quit', width))
  fixed.push(bottomBorder(width))
  return fixed.slice(0, height)
}

function colorizeLine(line, ui) {
  if (!ui.color) return line
  let value = line
  value = value.replace('WAKE', ui.brand('WAKE'))
  value = value.replace(/(✓|■)/, (match) => ui.ok(match))
  value = value.replace(/(✗)/, (match) => ui.error(match))
  value = value.replace(/(↻|⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|◌)/, (match) => ui.warn(match))
  value = value.replace(/(https?:\/\/\S+)/, (match) => ui.accent(match))
  return value
}

export function renderDashboard(state, width = 80, height = 24, ui = createUi(false)) {
  return plainFrame(state, width, height).map((line) => colorizeLine(line, ui)).join('\n')
}

export function createDashboardSession(
  state,
  { input = process.stdin, output = process.stderr, ui = createUi() } = {},
) {
  let closed = false
  let previousRaw = false
  let resolveExit
  const exit = new Promise((resolve) => { resolveExit = resolve })

  const draw = () => {
    if (closed) return
    const width = output.columns || 80
    const height = output.rows || 24
    output.write(`\x1b[H${renderDashboard(state, width, height, ui)}\x1b[J`)
  }
  const requestExit = (reason) => {
    if (!closed) resolveExit(reason)
  }
  const onData = (chunk) => {
    const key = chunk.toString('utf8')
    if (key === '\u0003') requestExit('SIGINT')
    else if (key === 'q' || key === 'Q') requestExit('q')
    else if (key === 'c' || key === 'C') {
      state.activity.length = 0
      state.scrollFromBottom = 0
      draw()
    } else if (key === '\x1b[A') state.scrollFromBottom = Math.max(0, Math.min(state.activity.length - 1, state.scrollFromBottom + 1))
    else if (key === '\x1b[B') state.scrollFromBottom = Math.max(0, state.scrollFromBottom - 1)
    else if (key === '\x1b[5~') state.scrollFromBottom = Math.max(0, Math.min(state.activity.length - 1, state.scrollFromBottom + 10))
    else if (key === '\x1b[6~') state.scrollFromBottom = Math.max(0, state.scrollFromBottom - 10)
    else if (key === '\x1b[F' || key === '\x1b[4~') state.scrollFromBottom = 0
    draw()
  }
  const onResize = () => draw()

  previousRaw = input.isRaw === true
  if (typeof input.setRawMode === 'function') input.setRawMode(true)
  input.resume?.()
  input.on('data', onData)
  output.on?.('resize', onResize)
  output.write('\x1b[?1049h\x1b[?25l\x1b[2J')
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
      output.write('\x1b[?25h\x1b[?1049l')
    },
  }
}

export function stripAnsi(value) {
  return String(value).replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '')
}
