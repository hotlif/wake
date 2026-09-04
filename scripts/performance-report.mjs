import { readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

export const PERFORMANCE_SCHEMA = 'wake.performance.v1'

function finiteNumber(value, description) {
  if (typeof value !== 'number' || !Number.isFinite(value) || value <= 0) {
    throw new Error(`${description} must be a positive finite number`)
  }
  return value
}

export function normalizeEstimate(raw, description = 'estimate') {
  const mean = raw?.mean
  const interval = mean?.confidence_interval
  const point = finiteNumber(mean?.point_estimate, `${description}.mean.point_estimate`)
  const lower = finiteNumber(interval?.lower_bound, `${description}.mean.confidence_interval.lower_bound`)
  const upper = finiteNumber(interval?.upper_bound, `${description}.mean.confidence_interval.upper_bound`)
  if (lower > point || point > upper) {
    throw new Error(`${description} confidence interval must contain its point estimate`)
  }
  return { point, lower, upper, confidenceLevel: interval.confidence_level ?? 0.95 }
}

function comparison(base, head) {
  return {
    changePercent: (head.point / base.point - 1) * 100,
    lowerPercent: (head.lower / base.upper - 1) * 100,
    upperPercent: (head.upper / base.lower - 1) * 100,
  }
}

export function evaluateBenchmark(benchmark, thresholds = {}) {
  const warningPercent = thresholds.warningPercent ?? 5
  const failurePercent = thresholds.failurePercent ?? 15
  if (benchmark.error) return { ...benchmark, status: 'invalid', reason: benchmark.error }
  if (!benchmark.base) return { ...benchmark, status: 'new', reason: 'not present in the base revision' }
  if (!benchmark.head) return { ...benchmark, status: 'missing', reason: 'not present in the head revision' }

  let base
  let head
  let retest
  try {
    base = normalizeEstimate(benchmark.base, `${benchmark.id} base`)
    head = normalizeEstimate(benchmark.head, `${benchmark.id} head`)
    retest = benchmark.retest
      ? normalizeEstimate(benchmark.retest, `${benchmark.id} retest`)
      : undefined
  } catch (error) {
    return { ...benchmark, status: 'invalid', reason: error.message }
  }

  const first = comparison(base, head)
  const second = retest ? comparison(base, retest) : undefined
  const confirmedRegression = benchmark.gate !== false
    && first.changePercent >= failurePercent
    && first.lowerPercent > warningPercent
    && second?.changePercent >= failurePercent
    && second.lowerPercent > warningPercent

  let status = 'stable'
  if (confirmedRegression) status = 'failure'
  else if (first.changePercent > warningPercent) status = 'warning'
  else if (first.changePercent < -warningPercent) status = 'improvement'

  return { ...benchmark, base, head, retest, first, second, status }
}

export function evaluateReport(report) {
  if (report?.schemaVersion !== PERFORMANCE_SCHEMA || !Array.isArray(report.benchmarks)) {
    throw new Error(`performance report must use ${PERFORMANCE_SCHEMA} and contain benchmarks`)
  }
  const thresholds = {
    warningPercent: report.warningThresholdPercent ?? 5,
    failurePercent: report.failureThresholdPercent ?? 15,
  }
  const benchmarks = report.benchmarks.map((benchmark) => evaluateBenchmark(benchmark, thresholds))
  return {
    ...report,
    ...thresholds,
    benchmarks,
    failed: report.enforce === true && benchmarks.some(({ status }) => status === 'failure'),
  }
}

function formatDuration(nanoseconds) {
  if (nanoseconds < 1_000) return `${nanoseconds.toFixed(1)} ns`
  if (nanoseconds < 1_000_000) return `${(nanoseconds / 1_000).toFixed(2)} us`
  if (nanoseconds < 1_000_000_000) return `${(nanoseconds / 1_000_000).toFixed(2)} ms`
  return `${(nanoseconds / 1_000_000_000).toFixed(2)} s`
}

function formatPercent(value) {
  return `${value >= 0 ? '+' : ''}${value.toFixed(1)}%`
}

const STATUS = {
  stable: 'OK',
  improvement: 'Improved',
  warning: 'Warning',
  failure: 'Failed',
  new: 'New',
  missing: 'Missing',
  invalid: 'Invalid',
}

export function renderMarkdown(report) {
  const lines = [
    '# Wake performance report',
    '',
    `Mode: **${report.mode}** · Base: \`${report.baseSha.slice(0, 12)}\` · Head: \`${report.headSha.slice(0, 12)}\``,
    '',
    `Thresholds: warning above ${report.warningPercent}% · confirmed failure above ${report.failurePercent}%`,
    '',
  ]
  if (report.baselineCompatibility?.compatible === false) {
    lines.push(
      `Baseline compatibility: **incompatible** (${report.baselineCompatibility.reasons.join('; ')}). Results are report-only.`,
      '',
    )
  }
  lines.push(
    '| Benchmark | Base | Head | Change (95% conservative range) | Result |',
    '| --- | ---: | ---: | ---: | --- |',
  )
  for (const benchmark of report.benchmarks) {
    if (!benchmark.first) {
      lines.push(`| \`${benchmark.id}\` | - | - | - | ${STATUS[benchmark.status]}: ${benchmark.reason} |`)
      continue
    }
    const range = `${formatPercent(benchmark.first.changePercent)} (${formatPercent(benchmark.first.lowerPercent)} to ${formatPercent(benchmark.first.upperPercent)})`
    const retest = benchmark.second
      ? `<br>Retest ${formatPercent(benchmark.second.changePercent)} (${formatPercent(benchmark.second.lowerPercent)} to ${formatPercent(benchmark.second.upperPercent)})`
      : ''
    lines.push(`| \`${benchmark.id}\` | ${formatDuration(benchmark.base.point)} | ${formatDuration(benchmark.head.point)} | ${range}${retest} | ${STATUS[benchmark.status]} |`)
  }
  lines.push('', report.failed
    ? 'Confirmed performance regression: this pull request is blocked.'
    : 'No confirmed blocking performance regression.')
  return `${lines.join('\n')}\n`
}

function parseArgs(args) {
  const parsed = {}
  for (let index = 0; index < args.length; index += 2) {
    const key = args[index]
    const value = args[index + 1]
    if (!key?.startsWith('--') || value === undefined) throw new Error(`invalid argument ${key ?? ''}`)
    parsed[key.slice(2)] = value
  }
  return parsed
}

export function runReportCli(args = process.argv.slice(2)) {
  const options = parseArgs(args)
  if (!options.input || !options.markdown || !options.output) {
    throw new Error('usage: performance-report.mjs --input input.json --markdown summary.md --output report.json')
  }
  const input = JSON.parse(readFileSync(resolve(options.input), 'utf8'))
  const report = evaluateReport(input)
  writeFileSync(resolve(options.output), `${JSON.stringify(report, null, 2)}\n`)
  writeFileSync(resolve(options.markdown), renderMarkdown(report))
  return report.failed ? 2 : 0
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exitCode = runReportCli()
  } catch (error) {
    console.error(error.stack ?? error.message)
    process.exitCode = 1
  }
}
