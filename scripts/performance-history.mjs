import { inflateRawSync } from 'node:zlib'
import { readFileSync, readdirSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { normalizeEstimate, PERFORMANCE_SCHEMA } from './performance-report.mjs'

const HISTORY_SCHEMA = 'wake.performance-history.v1'
const ENVIRONMENT_FIELDS = [
  'rustc', 'runnerOs', 'runnerArch', 'runnerImageVersion', 'cpuModel',
  'logicalCpuCount', 'totalMemoryBytes', 'node',
]
const MAX_ARCHIVE_BYTES = 30 * 1024 * 1024
const MAX_REPORT_BYTES = 2 * 1024 * 1024

function reportPoint(estimate, description) {
  // run-performance writes evaluateReport(), whose estimates are normalized.
  return normalizeEstimate({
    mean: {
      point_estimate: estimate?.point,
      confidence_interval: {
        lower_bound: estimate?.lower,
        upper_bound: estimate?.upper,
        confidence_level: estimate?.confidenceLevel,
      },
    },
  }, description).point
}

function validateReport(report, label) {
  if (report?.schemaVersion !== PERFORMANCE_SCHEMA) {
    throw new Error(`${label} schemaVersion must be ${PERFORMANCE_SCHEMA}`)
  }
  if (report.mode !== 'trend' || !Array.isArray(report.benchmarks)
      || typeof report.headSha !== 'string') {
    throw new Error(`${label} must be a complete ${PERFORMANCE_SCHEMA} trend report`)
  }
  const seen = new Set()
  for (const benchmark of report.benchmarks) {
    if (typeof benchmark.id !== 'string' || !benchmark.id || seen.has(benchmark.id)) {
      throw new Error(`${label} has a missing or duplicate benchmark id`)
    }
    seen.add(benchmark.id)
    if (benchmark.head) reportPoint(benchmark.head, `${label} ${benchmark.id} head`)
  }
}

function matchingEnvironment(current, prior) {
  return ENVIRONMENT_FIELDS.every((field) =>
    current?.[field] !== null && current?.[field] !== undefined
    && prior?.[field] !== null && prior?.[field] !== undefined
    && current[field] === prior[field])
}

function median(values) {
  const ordered = [...values].sort((left, right) => left - right)
  const middle = Math.floor(ordered.length / 2)
  return ordered.length % 2 === 0
    ? (ordered[middle - 1] + ordered[middle]) / 2
    : ordered[middle]
}

export function buildHistory(current, snapshots, { maxSamples = 12 } = {}) {
  validateReport(current, 'current report')
  if (!Number.isInteger(maxSamples) || maxSamples < 1 || maxSamples > 100) {
    throw new Error('maxSamples must be an integer from 1 to 100')
  }
  const seenRuns = new Set()
  const eligible = []
  for (const snapshot of [...snapshots].sort((left, right) =>
    Date.parse(right.createdAt) - Date.parse(left.createdAt))) {
    if (!Number.isSafeInteger(snapshot.runId) || snapshot.runId <= 0
        || !Number.isFinite(Date.parse(snapshot.createdAt))) {
      throw new Error('history snapshot requires a valid runId and createdAt')
    }
    validateReport(snapshot.report, `run ${snapshot.runId}`)
    if (seenRuns.has(snapshot.runId)) continue
    seenRuns.add(snapshot.runId)
    if (matchingEnvironment(current.environment, snapshot.report.environment)) eligible.push(snapshot)
  }

  const benchmarks = current.benchmarks.map((benchmark) => {
    const currentNs = benchmark.head
      ? reportPoint(benchmark.head, `current ${benchmark.id} head`)
      : null
    const definitionSha256 = benchmark.definitionSha256
    const samples = currentNs !== null && /^[0-9a-f]{64}$/.test(definitionSha256 ?? '')
      ? eligible.flatMap((snapshot) => {
        const previous = snapshot.report.benchmarks.find(({ id }) => id === benchmark.id)
        return previous?.definitionSha256 === definitionSha256 && previous.head
          ? [{ runId: snapshot.runId, headSha: snapshot.report.headSha,
            point: reportPoint(previous.head, `run ${snapshot.runId} ${benchmark.id} head`) }]
          : []
      }).slice(0, maxSamples)
      : []
    const historicalMedianNs = samples.length ? median(samples.map(({ point }) => point)) : null
    const medianAbsoluteDeviationNs = historicalMedianNs === null
      ? null
      : median(samples.map(({ point }) => Math.abs(point - historicalMedianNs)))
    return {
      id: benchmark.id,
      definitionSha256: definitionSha256 ?? null,
      currentNs,
      historicalMedianNs,
      medianAbsoluteDeviationNs,
      changePercent: historicalMedianNs === null ? null : (currentNs / historicalMedianNs - 1) * 100,
      sampleCount: samples.length,
      runIds: samples.map(({ runId }) => runId),
      headShas: samples.map(({ headSha }) => headSha),
    }
  })
  return {
    schemaVersion: HISTORY_SCHEMA,
    mode: 'report-only',
    enforce: false,
    currentSha: current.headSha,
    source: 'successful canary full trend runs',
    maxSamples,
    inspectedRunCount: seenRuns.size,
    matchingEnvironmentRunCount: eligible.length,
    physicalRunnerVerified: false,
    benchmarks,
  }
}

function formatDuration(nanoseconds) {
  if (nanoseconds === null) return '-'
  if (nanoseconds < 1_000) return `${nanoseconds.toFixed(1)} ns`
  if (nanoseconds < 1_000_000) return `${(nanoseconds / 1_000).toFixed(2)} us`
  if (nanoseconds < 1_000_000_000) return `${(nanoseconds / 1_000_000).toFixed(2)} ms`
  return `${(nanoseconds / 1_000_000_000).toFixed(2)} s`
}

function markdownCell(value) {
  return String(value).replaceAll('|', '\\|').replaceAll(/[\r\n]/g, ' ')
}

export function renderHistoryMarkdown(history) {
  const lines = [
    '# Wake performance history',
    '',
    `Current: \`${history.currentSha.slice(0, 12)}\` · ${history.inspectedRunCount} prior runs inspected · ${history.matchingEnvironmentRunCount} matched the recorded environment`,
    '',
    'Historical comparison is **report-only**. The recorded environment does not verify a fixed physical runner, and per-benchmark noise thresholds have not been calibrated.',
    '',
    '| Benchmark | Current | Historical median | Change | Median absolute deviation | Samples |',
    '| --- | ---: | ---: | ---: | ---: | ---: |',
  ]
  for (const benchmark of history.benchmarks) {
    const change = benchmark.changePercent === null
      ? 'no comparable history'
      : `${benchmark.changePercent >= 0 ? '+' : ''}${benchmark.changePercent.toFixed(1)}%`
    lines.push(`| \`${markdownCell(benchmark.id)}\` | ${formatDuration(benchmark.currentNs)} | ${formatDuration(benchmark.historicalMedianNs)} | ${change} | ${formatDuration(benchmark.medianAbsoluteDeviationNs)} | ${benchmark.sampleCount} |`)
  }
  return `${lines.join('\n')}\n`
}

export function extractReportFromArtifact(archive) {
  if (archive.length > MAX_ARCHIVE_BYTES) throw new Error('performance artifact exceeds download limit')
  let end = -1
  for (let index = archive.length - 22; index >= Math.max(0, archive.length - 65_557); index -= 1) {
    if (archive.readUInt32LE(index) === 0x06054b50) { end = index; break }
  }
  if (end < 0) throw new Error('performance artifact is not a ZIP archive')
  const count = archive.readUInt16LE(end + 10)
  let offset = archive.readUInt32LE(end + 16)
  for (let index = 0; index < count; index += 1) {
    if (offset + 46 > archive.length || archive.readUInt32LE(offset) !== 0x02014b50) {
      throw new Error('performance artifact has an invalid ZIP directory')
    }
    const compression = archive.readUInt16LE(offset + 10)
    const compressedSize = archive.readUInt32LE(offset + 20)
    const uncompressedSize = archive.readUInt32LE(offset + 24)
    const nameLength = archive.readUInt16LE(offset + 28)
    const extraLength = archive.readUInt16LE(offset + 30)
    const commentLength = archive.readUInt16LE(offset + 32)
    const localOffset = archive.readUInt32LE(offset + 42)
    const name = archive.toString('utf8', offset + 46, offset + 46 + nameLength)
    offset += 46 + nameLength + extraLength + commentLength
    if (!['report.json', '.wake-performance/report.json'].includes(name)) continue
    if (uncompressedSize > MAX_REPORT_BYTES || compressedSize > MAX_ARCHIVE_BYTES
        || localOffset + 30 > archive.length || archive.readUInt32LE(localOffset) !== 0x04034b50) {
      throw new Error('performance artifact report exceeds limit or has an invalid ZIP entry')
    }
    const dataOffset = localOffset + 30 + archive.readUInt16LE(localOffset + 26)
      + archive.readUInt16LE(localOffset + 28)
    if (dataOffset + compressedSize > archive.length) throw new Error('truncated performance report entry')
    const compressed = archive.subarray(dataOffset, dataOffset + compressedSize)
    const bytes = compression === 0 ? compressed
      : compression === 8 ? inflateRawSync(compressed, { maxOutputLength: MAX_REPORT_BYTES })
        : null
    if (!bytes || bytes.length !== uncompressedSize) {
      throw new Error('unsupported or inconsistent performance report ZIP entry')
    }
    return JSON.parse(bytes.toString('utf8'))
  }
  throw new Error('performance artifact does not contain report.json')
}

async function githubGet(url, token, binary = false) {
  const response = await fetch(url, {
    headers: {
      Accept: 'application/vnd.github+json',
      Authorization: `Bearer ${token}`,
      'X-GitHub-Api-Version': '2022-11-28',
    },
    signal: AbortSignal.timeout(30_000),
  })
  if (!response.ok) throw new Error(`GitHub artifact request failed (${response.status})`)
  if (!binary) return response.json()
  const chunks = []
  let size = 0
  for await (const chunk of response.body) {
    size += chunk.length
    if (size > MAX_ARCHIVE_BYTES) throw new Error('performance artifact exceeds download limit')
    chunks.push(chunk)
  }
  return Buffer.concat(chunks)
}

export async function fetchCanaryHistory(repository, token, { maxRuns = 24 } = {}) {
  if (!/^[\w.-]+\/[\w.-]+$/.test(repository) || !token) {
    throw new Error('GitHub history requires owner/repository and GITHUB_TOKEN')
  }
  const apiRoot = (process.env.GITHUB_API_URL ?? 'https://api.github.com').replace(/\/$/, '')
  const base = `${apiRoot}/repos/${repository}`
  const listing = await githubGet(`${base}/actions/workflows/performance.yml/runs?branch=canary&status=success&per_page=50`, token)
  if (!Array.isArray(listing.workflow_runs)) throw new Error('GitHub did not return workflow runs')
  const snapshots = []
  for (const run of listing.workflow_runs) {
    if (snapshots.length >= maxRuns) break
    if (!['push', 'schedule', 'workflow_dispatch'].includes(run.event)
        || String(run.id) === process.env.GITHUB_RUN_ID) continue
    const artifacts = await githubGet(`${base}/actions/runs/${run.id}/artifacts?per_page=100`, token)
    if (!Array.isArray(artifacts.artifacts)) throw new Error(`GitHub did not return artifacts for run ${run.id}`)
    const artifact = [
      `performance-snapshot-full-${run.id}`,
      `performance-full-${run.id}`,
    ].map((name) => artifacts.artifacts.find((item) => item.name === name && !item.expired
      && item.size_in_bytes <= MAX_ARCHIVE_BYTES)).find(Boolean)
    if (!artifact) continue
    const archive = await githubGet(`${base}/actions/artifacts/${artifact.id}/zip`, token, true)
    snapshots.push({
      runId: run.id,
      createdAt: run.created_at,
      report: extractReportFromArtifact(archive),
    })
  }
  return snapshots
}

function parseArgs(args) {
  const options = {}
  for (let index = 0; index < args.length; index += 2) {
    if (!args[index]?.startsWith('--') || args[index + 1] === undefined) {
      throw new Error(`invalid argument ${args[index] ?? ''}`)
    }
    options[args[index].slice(2)] = args[index + 1]
  }
  return options
}

export async function runHistoryCli(args = process.argv.slice(2)) {
  const options = parseArgs(args)
  if (!options.current || !options.output || !options.markdown
      || Boolean(options['history-dir']) === Boolean(options.repository)) {
    throw new Error('usage: performance-history.mjs --current report.json --output history.json --markdown history.md (--history-dir path | --repository owner/repo)')
  }
  const current = JSON.parse(readFileSync(resolve(options.current), 'utf8'))
  const snapshots = options.repository
    ? await fetchCanaryHistory(options.repository, process.env.GITHUB_TOKEN)
    : readdirSync(resolve(options['history-dir']))
      .filter((name) => name.endsWith('.json'))
      .map((name) => JSON.parse(readFileSync(resolve(options['history-dir'], name), 'utf8')))
  const history = buildHistory(current, snapshots)
  writeFileSync(resolve(options.output), `${JSON.stringify(history, null, 2)}\n`)
  writeFileSync(resolve(options.markdown), renderHistoryMarkdown(history))
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  runHistoryCli().catch((error) => {
    console.error(error.stack ?? error.message)
    process.exitCode = 1
  })
}
