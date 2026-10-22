import assert from 'node:assert/strict'
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import test from 'node:test'
import { deflateRawSync } from 'node:zlib'

import { buildHistory, extractReportFromArtifact, renderHistoryMarkdown, runHistoryCli } from './performance-history.mjs'
import { evaluateReport } from './performance-report.mjs'

const sha = (character) => character.repeat(40)
const definition = (character) => character.repeat(64)
const environment = {
  rustc: 'rustc 1.95.0',
  runnerOs: 'Linux',
  runnerArch: 'X64',
  runnerImageVersion: '20261020.1',
  cpuModel: 'example CPU',
  logicalCpuCount: 4,
  totalMemoryBytes: 16_000_000_000,
  node: 'v24.0.0',
}

function estimate(point) {
  return {
    point,
    lower: point * 0.99,
    upper: point * 1.01,
    confidenceLevel: 0.95,
  }
}

function report({ headSha, point, env = environment, definitionSha256 = definition('d') }) {
  return {
    schemaVersion: 'wake.performance.v1',
    mode: 'trend',
    headSha,
    environment: env,
    benchmarks: [{ id: 'parser/parse_module', definitionSha256, head: estimate(point) }],
  }
}

function snapshot(runId, point, options = {}) {
  return {
    runId,
    createdAt: new Date(Date.UTC(2026, 9, runId)).toISOString(),
    report: report({ headSha: sha(String(runId % 10)), point, ...options }),
  }
}

test('history compares only matching benchmark definitions and environment, using a median and MAD', () => {
  const current = report({ headSha: sha('f'), point: 125 })
  const history = buildHistory(current, [
    snapshot(4, 500, { env: { ...environment, cpuModel: 'other CPU' } }),
    snapshot(3, 300, { definitionSha256: definition('e') }),
    snapshot(2, 110),
    snapshot(1, 90),
  ])
  assert.equal(history.enforce, false)
  assert.equal(history.benchmarks[0].sampleCount, 2)
  assert.deepEqual(history.benchmarks[0].runIds, [2, 1])
  assert.equal(history.benchmarks[0].historicalMedianNs, 100)
  assert.equal(history.benchmarks[0].medianAbsoluteDeviationNs, 10)
  assert.equal(history.benchmarks[0].changePercent, 25)
  assert.match(renderHistoryMarkdown(history), /report.only/i)
})

test('repeat measurements of one commit remain useful, but duplicate run ids are not counted twice', () => {
  const current = report({ headSha: sha('f'), point: 110 })
  const duplicate = snapshot(2, 90)
  const history = buildHistory(current, [
    { ...duplicate, createdAt: '2026-10-03T00:00:00.000Z' },
    duplicate,
    snapshot(1, 100),
    snapshot(4, 200, { headSha: sha('f') }),
  ], { maxSamples: 1 })
  assert.deepEqual(history.benchmarks[0].runIds, [4])
  assert.equal(history.benchmarks[0].historicalMedianNs, 200)
})

test('missing environment or benchmark definition cannot establish a historical comparison', () => {
  const current = report({ headSha: sha('f'), point: 100, env: { ...environment, cpuModel: null } })
  const history = buildHistory(current, [snapshot(1, 90)])
  assert.equal(history.benchmarks[0].sampleCount, 0)
  assert.equal(history.benchmarks[0].historicalMedianNs, null)
  assert.match(renderHistoryMarkdown(history), /no comparable history/i)
  const legacy = buildHistory(report({ headSha: sha('f'), point: 100 }), [
    snapshot(1, 90, { definitionSha256: null }),
  ])
  assert.equal(legacy.benchmarks[0].sampleCount, 0)
})

test('invalid saved reports fail visibly instead of contributing to history', () => {
  const current = report({ headSha: sha('f'), point: 100 })
  assert.throws(() => buildHistory(current, [
    { runId: 1, createdAt: '2026-10-01T00:00:00Z', report: { ...report({ headSha: sha('a'), point: 90 }), schemaVersion: 'other' } },
  ]), /schemaVersion/)
  assert.throws(() => buildHistory(current, [
    snapshot(1, -90),
  ]), /positive finite number/)
})

test('GitHub artifact ZIP yields only its bounded report.json entry', () => {
  const name = Buffer.from('report.json')
  const plain = Buffer.from(JSON.stringify(report({ headSha: sha('a'), point: 90 })))
  const compressed = deflateRawSync(plain)
  const local = Buffer.alloc(30)
  local.writeUInt32LE(0x04034b50, 0)
  local.writeUInt16LE(8, 8)
  local.writeUInt32LE(compressed.length, 18)
  local.writeUInt32LE(plain.length, 22)
  local.writeUInt16LE(name.length, 26)
  const central = Buffer.alloc(46)
  central.writeUInt32LE(0x02014b50, 0)
  central.writeUInt16LE(8, 10)
  central.writeUInt32LE(compressed.length, 20)
  central.writeUInt32LE(plain.length, 24)
  central.writeUInt16LE(name.length, 28)
  const directoryOffset = local.length + name.length + compressed.length
  const end = Buffer.alloc(22)
  end.writeUInt32LE(0x06054b50, 0)
  end.writeUInt16LE(1, 8)
  end.writeUInt16LE(1, 10)
  end.writeUInt32LE(central.length + name.length, 12)
  end.writeUInt32LE(directoryOffset, 16)
  const archive = Buffer.concat([local, name, compressed, central, name, end])
  assert.deepEqual(extractReportFromArtifact(archive), JSON.parse(plain))
  assert.throws(() => extractReportFromArtifact(Buffer.from('not a zip')), /ZIP archive/)
})

test('offline history CLI writes reusable JSON and Markdown reports', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'wake-performance-history-'))
  const historyDir = join(directory, 'snapshots')
  mkdirSync(historyDir)
  try {
    writeFileSync(join(directory, 'current.json'), JSON.stringify(report({ headSha: sha('f'), point: 125 })))
    writeFileSync(join(historyDir, 'prior.json'), JSON.stringify(snapshot(1, 100)))
    await runHistoryCli([
      '--current', join(directory, 'current.json'),
      '--output', join(directory, 'history.json'),
      '--markdown', join(directory, 'history.md'),
      '--history-dir', historyDir,
    ])
    const history = JSON.parse(readFileSync(join(directory, 'history.json'), 'utf8'))
    assert.equal(history.benchmarks[0].changePercent, 25)
    assert.match(readFileSync(join(directory, 'history.md'), 'utf8'), /report.only/i)
  } finally {
    rmSync(directory, { recursive: true, force: true })
  }
})

test('history reads the evaluated report.json shape produced by the performance gate', () => {
  function evaluated(headSha, point) {
    return evaluateReport({
      schemaVersion: 'wake.performance.v1',
      mode: 'trend',
      headSha,
      enforce: false,
      environment,
      benchmarks: [{
        id: 'parser/parse_module',
        definitionSha256: definition('d'),
        base: { mean: { point_estimate: point, confidence_interval: {
          lower_bound: point * 0.99, upper_bound: point * 1.01,
        } } },
        head: { mean: { point_estimate: point, confidence_interval: {
          lower_bound: point * 0.99, upper_bound: point * 1.01,
        } } },
      }],
    })
  }
  const current = evaluated(sha('f'), 125)
  const prior = evaluated(sha('a'), 100)
  const history = buildHistory(current, [{ runId: 1, createdAt: '2026-10-01T00:00:00Z', report: prior }])
  assert.equal(history.benchmarks[0].changePercent, 25)
})
