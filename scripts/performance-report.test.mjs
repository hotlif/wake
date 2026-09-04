import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { resolve } from 'node:path'
import test from 'node:test'

import { evaluateReport, normalizeEstimate, renderMarkdown } from './performance-report.mjs'

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
