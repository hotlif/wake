import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import test from 'node:test'

import { evaluateReport, normalizeEstimate, renderMarkdown } from './performance-report.mjs'
import * as performanceRunner from './run-performance.mjs'

const { compareProbeResults, keyBenchmarks, reportEntries } = performanceRunner

const cases = JSON.parse(readFileSync(
  resolve(import.meta.dirname, 'fixtures/performance-report/cases.json'),
  'utf8',
))

function estimate(point, lower = point * 0.99, upper = point * 1.01) {
  return {
    mean: {
      point_estimate: point,
      confidence_interval: { confidence_level: 0.95, lower_bound: lower, upper_bound: upper },
    },
  }
}

for (const fixture of cases) {
  test(fixture.name, () => {
    const report = evaluateReport({
      schemaVersion: 'wake.performance.v1',
      mode: 'pull-request',
      baseSha: 'a'.repeat(40),
      headSha: 'b'.repeat(40),
      warningThresholdPercent: 5,
      failureThresholdPercent: 15,
      enforce: true,
      benchmarks: fixture.benchmarks.map((item) => ({
        id: item.id,
        gate: item.gate,
        base: item.base && estimate(...item.base),
        head: item.head && estimate(...item.head),
        retest: item.retest && estimate(...item.retest),
      })),
    })
    assert.deepEqual(report.benchmarks.map(({ status }) => status), fixture.statuses)
    assert.equal(report.failed, fixture.failed)
    assert.match(renderMarkdown(report), /Wake performance report/)
  })
}

test('invalid Criterion estimate is rejected', () => {
  const invalid = JSON.parse(readFileSync(
    resolve(import.meta.dirname, 'fixtures/performance-report/invalid-estimates.json'),
    'utf8',
  ))
  assert.throws(() => normalizeEstimate(invalid), /positive finite number/)
})

test('invalid report schema is rejected', () => {
  assert.throws(() => evaluateReport({ benchmarks: [] }), /wake\.performance\.v1/)
})

test('an incompatible baseline is clearly rendered as report-only', () => {
  const report = evaluateReport({
    schemaVersion: 'wake.performance.v1',
    mode: 'pull-request',
    baseSha: 'a'.repeat(40),
    headSha: 'b'.repeat(40),
    enforce: false,
    baselineCompatibility: { compatible: false, reasons: ['benchmark fixture changed'] },
    benchmarks: [{
      id: 'changed',
      gate: true,
      base: estimate(100, 99, 101),
      head: estimate(120, 118, 122),
      retest: estimate(119, 117, 121),
    }],
  })
  assert.equal(report.failed, false)
  assert.match(renderMarkdown(report), /incompatible.*report-only/)
})

test('a gated benchmark without usable comparison data blocks the PR', () => {
  for (const [status, base, head] of [
    ['new', undefined, estimate(100)],
    ['missing', estimate(100), undefined],
    ['invalid', estimate(100), estimate(-1)],
  ]) {
    const report = evaluateReport({
      schemaVersion: 'wake.performance.v1',
      mode: 'pull-request',
      baseSha: 'a'.repeat(40),
      headSha: 'b'.repeat(40),
      enforce: true,
      benchmarks: [{ id: 'required', gate: true, base, head }],
    })
    assert.equal(report.benchmarks[0].status, status)
    assert.equal(report.failed, true, `${status} must fail closed`)
    assert.match(renderMarkdown(report), /benchmark data.*blocked/i)
  }
})

test('a key benchmark omitted by both Criterion runs is retained in the report', () => {
  const entries = reportEntries(new Map(), new Map(), new Set(['required']))
  assert.deepEqual(entries, [{
    id: 'required',
    gate: true,
    base: undefined,
    head: undefined,
    error: 'no Criterion measurement in either revision',
  }])
  const report = evaluateReport({
    schemaVersion: 'wake.performance.v1',
    mode: 'pull-request',
    baseSha: 'a'.repeat(40),
    headSha: 'b'.repeat(40),
    enforce: true,
    benchmarks: entries,
  })
  assert.equal(report.failed, true)
  assert.equal(report.benchmarks[0].status, 'invalid')
})

test('both lint input sizes are gated by their measured Criterion full IDs', () => {
  const measurements = new Map([
    ['lint/lint_text/16384', estimate(100)],
    ['lint/lint_text/131072', estimate(200)],
  ])
  const lintIds = new Set(keyBenchmarks
    .filter((spec) => spec.package === 'wake_lint_core')
    .flatMap(({ gateIds, filter }) => gateIds ?? [filter]))
  const entries = reportEntries(measurements, measurements, lintIds)
  assert.deepEqual(entries.map(({ id, gate, error }) => ({ id, gate, error })), [
    { id: 'lint/lint_text/131072', gate: true, error: undefined },
    { id: 'lint/lint_text/16384', gate: true, error: undefined },
  ])
  const report = evaluateReport({
    schemaVersion: 'wake.performance.v1',
    mode: 'pull-request',
    baseSha: 'a'.repeat(40),
    headSha: 'b'.repeat(40),
    enforce: true,
    benchmarks: entries,
  })
  assert.equal(report.failed, false)
})

test('a lint retest runs the shared Criterion filter once for both gated IDs', () => {
  const specs = performanceRunner.keyRetestSpecsForIds([
    'lint/lint_text/16384',
    'lint/lint_text/131072',
  ])
  assert.deepEqual(specs.map(({ filter }) => filter), ['lint/lint_text'])
})

test('report entries retain the measured benchmark definition digest for historical comparison', () => {
  const digest = 'd'.repeat(64)
  const entries = reportEntries(
    new Map(),
    new Map([['parser/parse_module', estimate(100)]]),
    new Set(),
    new Map([['parser/parse_module', digest]]),
  )
  assert.equal(entries[0].definitionSha256, digest)
})

test('required output and diagnostic equivalence fails closed without changing timing thresholds', () => {
  for (const status of ['different', 'unavailable']) {
    const report = evaluateReport({
      schemaVersion: 'wake.performance.v1',
      mode: 'pull-request',
      baseSha: 'a'.repeat(40),
      headSha: 'b'.repeat(40),
      enforce: false,
      requireOutputEquivalence: true,
      outputEquivalence: { status, reason: `${status} probe`, cases: [] },
      benchmarks: [],
    })
    assert.equal(report.blockingEquivalenceError, true)
    assert.equal(report.failed, true)
    assert.match(renderMarkdown(report), /output and diagnostic equivalence.*blocked/i)
  }
  const equivalent = evaluateReport({
    schemaVersion: 'wake.performance.v1',
    mode: 'pull-request',
    baseSha: 'a'.repeat(40),
    headSha: 'b'.repeat(40),
    enforce: false,
    requireOutputEquivalence: true,
    outputEquivalence: { status: 'equivalent', cases: [] },
    benchmarks: [],
  })
  assert.equal(equivalent.failed, false)
})

test('Bundler probe compares complete output and diagnostics by scenario', () => {
  const base = {
    schemaVersion: 'wake.performance-equivalence.v1',
    cases: [
      { id: 'bundle_2k', output: { bundle: 'code', chunks: ['chunk'] }, diagnostics: [] },
      { id: 'missing_dependency', output: { bundle: '' }, diagnostics: [{ code: 'WAKE0301' }] },
    ],
  }
  assert.equal(compareProbeResults(base, structuredClone(base)).status, 'equivalent')
  const changed = structuredClone(base)
  changed.cases[0].output.bundle = 'different code'
  changed.cases[1].diagnostics[0].code = 'WAKE0302'
  const result = compareProbeResults(base, changed)
  assert.equal(result.status, 'different')
  assert.deepEqual(result.cases.map(({ outputEqual, diagnosticsEqual }) =>
    [outputEqual, diagnosticsEqual]), [[false, true], [true, false]])
  assert.equal(compareProbeResults(base, { ...base, cases: [] }).status, 'unavailable')
})
