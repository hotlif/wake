const RESET = '\x1b[0m'
const BOLD = '\x1b[1m'
const DIM = '\x1b[2m'
const RED = '\x1b[31m'
const GREEN = '\x1b[32m'
const YELLOW = '\x1b[33m'
const CYAN = '\x1b[36m'
const MAGENTA_BOLD = '\x1b[1;35m'

export function supportsColor(stream = process.stderr, env = process.env) {
  return stream.isTTY === true && !Object.hasOwn(env, 'NO_COLOR')
}

export function createUi(color = supportsColor()) {
  const wrap = (code, text) => color ? `${code}${text}${RESET}` : String(text)
  return {
    accent: (text) => wrap(CYAN, text),
    bold: (text) => wrap(BOLD, text),
    brand: (text) => wrap(MAGENTA_BOLD, text),
    dim: (text) => wrap(DIM, text),
    error: (text) => wrap(RED, text),
    ok: (text) => wrap(GREEN, text),
    warn: (text) => wrap(YELLOW, text),
  }
}

export function humanDuration(durationMs) {
  const milliseconds = Math.max(1, Number(durationMs) || 0)
  if (milliseconds < 1_000) return `${milliseconds.toFixed(0)}ms`
  if (milliseconds < 60_000) return `${(milliseconds / 1_000).toFixed(2)}s`
  const seconds = milliseconds / 1_000
  return `${Math.floor(seconds / 60)}m${(seconds % 60).toFixed(1)}s`
}

export function formatBanner(ui, command, currentVersion) {
  return [
    '',
    `  ${ui.warn('⚡')} ${ui.brand('wake')} ${ui.dim(`v${currentVersion}`)}  ${ui.dim(command)}`,
    '',
  ]
}

export function formatServerReady(ui, url, durationMs) {
  return [
    `  ${ui.ok('✓')}  ${ui.bold('开发服务器已就绪')}  ${ui.dim('·')}  ${ui.accent(humanDuration(durationMs))}`,
    '',
    `    ${ui.dim('Local')} ${ui.accent(url)}`,
    `    ${ui.dim('提示')} ${ui.dim('按')} ${ui.bold('Ctrl-C')} ${ui.dim('退出')}`,
    '',
  ]
}

export function observeServer(server, ui, output = console) {
  const onRebuildStart = (event) => {
    const count = event.changedPaths?.length || 0
    const detail = count > 0 ? `检测到 ${count} 个文件变更，正在重建…` : '正在重建…'
    output.log(`  ${ui.warn('↻')}  ${ui.dim(detail)}`)
  }
  const onRebuilt = (event) => {
    output.log(
      `  ${ui.ok('✓')}  ${ui.bold('热重建')}  ${ui.dim('·')}  ${ui.accent(`${event.modules} 模块`)}  ${ui.dim('·')}  ${ui.accent(humanDuration(event.durationMs))}`,
    )
  }
  const onDiagnostic = (diagnostic) => {
    const code = diagnostic.code ? `[${diagnostic.code}] ` : ''
    output.error(`  ${ui.error('✗')}  ${ui.bold('构建失败')}  ${code}${diagnostic.message}`)
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
