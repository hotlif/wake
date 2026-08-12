import { spawnSync } from 'node:child_process'
import { performance } from 'node:perf_hooks'
import { resolve } from 'node:path'

const root = resolve(import.meta.dirname, '..')
const cli = resolve(root, 'npm/wake/bin/wake.mjs')
const samples = []

for (let index = 0; index < 5; index += 1) {
  const started = performance.now()
  const result = spawnSync(process.execPath, [cli, '--version'], {
    cwd: root,
    env: process.env,
    encoding: 'utf8',
  })
  const elapsed = performance.now() - started
  if (result.status !== 0 || result.stdout.trim() !== '0.1.15') {
    throw new Error(
      `wake --version failed: ${result.stderr || result.stdout || result.status}`,
    )
  }
  samples.push(elapsed)
}

samples.sort((left, right) => left - right)
const median = samples[Math.floor(samples.length / 2)]
console.log(`wake --version median: ${median.toFixed(1)}ms`)
if (median > 500) {
  throw new Error(`Wake startup median ${median.toFixed(1)}ms exceeds 500ms`)
}
