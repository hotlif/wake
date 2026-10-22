import { spawnSync } from 'node:child_process'
import {
  cpSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { availableParallelism, cpus, totalmem } from 'node:os'
import { createHash } from 'node:crypto'
import { isDeepStrictEqual } from 'node:util'

import {
  PERFORMANCE_SCHEMA,
  evaluateReport,
  renderMarkdown,
} from './performance-report.mjs'

const root = resolve(import.meta.dirname, '..')
const CRITERION_BASELINE = 'wake-base'
const PROBE_SCHEMA = 'wake.performance-equivalence.v1'
const PROBE_CASES = ['bundle_2k', 'missing_dependency']
export const keyBenchmarks = [
  { package: 'wake_ecma_lexer', bench: 'lexer', filter: 'lexer/tokenize' },
  { package: 'wake_ecma_parser', bench: 'parser', filter: 'parser/parse_module' },
  {
    package: 'wake_lint_core',
    bench: 'lint',
    filter: 'lint/lint_text',
    gateIds: ['lint/lint_text/16384', 'lint/lint_text/131072'],
  },
  { package: 'wake_compiler', bench: 'transpile', filter: 'compiler/transpile_tsx_module' },
  { package: 'wake_turbo', bench: 'engine', filter: 'shallow_green/request_1000_memoized' },
  { package: 'wake_bundler', bench: 'bundle', filter: 'bundle_2k/cold' },
  { package: 'wake_bundler', bench: 'bundle', filter: 'bundle_2k/edit_one' },
]
const fullBenchmarks = [
  { package: 'wake_common', bench: 'interner' },
  { package: 'wake_ecma_lexer', bench: 'lexer' },
  { package: 'wake_ecma_parser', bench: 'parser' },
  { package: 'wake_lint_core', bench: 'lint' },
  { package: 'wake_compiler', bench: 'transpile' },
  { package: 'wake_turbo', bench: 'engine' },
  { package: 'wake_resolver', bench: 'resolve' },
  { package: 'wake_bundler', bench: 'bundle' },
]

function gateIdsFor(spec) {
  return spec.gateIds ?? [spec.filter]
}

export function keyRetestSpecsForIds(ids) {
  const remaining = new Set(ids)
  const specs = keyBenchmarks.filter((spec) => gateIdsFor(spec).some((id) => remaining.has(id)))
  for (const spec of specs) {
    for (const id of gateIdsFor(spec)) remaining.delete(id)
  }
  if (remaining.size > 0) {
    throw new Error(`no benchmark command registered for ${[...remaining].join(', ')}`)
  }
  return specs
}

function parseArgs(args) {
  const parsed = {}
  for (let index = 0; index < args.length; index += 2) {
    if (!args[index]?.startsWith('--') || args[index + 1] === undefined) {
      throw new Error(`invalid argument ${args[index] ?? ''}`)
    }
    parsed[args[index].slice(2)] = args[index + 1]
  }
  return parsed
}

function command(program, args, options = {}) {
  console.log(`> ${program} ${args.join(' ')}`)
  const result = spawnSync(program, args, {
    cwd: options.cwd ?? root,
    env: { ...process.env, ...options.env },
    encoding: options.capture ? 'utf8' : undefined,
    stdio: options.capture ? 'pipe' : 'inherit',
    maxBuffer: options.maxBuffer ?? 1024 * 1024,
  })
  if (result.error) throw result.error
  if (result.status !== 0) {
    const detail = options.capture ? `\n${result.stderr || result.stdout}` : ''
    throw new Error(`${program} exited with status ${result.status}${detail}`)
  }
  return options.capture ? result.stdout.trim() : ''
}

function cargoBench(worktree, spec, criterionArgs, targetDir) {
  const args = [
    'bench',
    '--locked',
    '--manifest-path', join(worktree, 'Cargo.toml'),
    '-p', spec.package,
    '--bench', spec.bench,
    '--',
  ]
  if (spec.filter) args.push(spec.filter)
  args.push(...criterionArgs)
  command('cargo', args, {
    env: {
      CARGO_TARGET_DIR: targetDir,
      CARGO_NET_OFFLINE: 'true',
    },
  })
}

function clearTransientCriterion(directory) {
  if (!existsSync(directory)) return
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue
    const path = join(directory, entry.name)
    if (['new', 'change', 'report'].includes(entry.name)) rmSync(path, { recursive: true, force: true })
    else clearTransientCriterion(path)
  }
}

function readCriterionSnapshot(directory, snapshot) {
  const benchmarks = new Map()
  if (!existsSync(directory)) return benchmarks
  function visit(current) {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      if (!entry.isDirectory()) continue
      const path = join(current, entry.name)
      if (entry.name === snapshot) {
        const benchmarkPath = join(path, 'benchmark.json')
        const estimatePath = join(path, 'estimates.json')
        if (!existsSync(benchmarkPath) || !existsSync(estimatePath)) continue
        const metadata = JSON.parse(readFileSync(benchmarkPath, 'utf8'))
        benchmarks.set(metadata.full_id, JSON.parse(readFileSync(estimatePath, 'utf8')))
      } else {
        visit(path)
      }
    }
  }
  visit(directory)
  return benchmarks
}

export function reportEntries(base, head, gatedIds, definitionDigests = new Map()) {
  const ids = [...new Set([...base.keys(), ...head.keys(), ...gatedIds])].sort()
  return ids.map((id) => ({
    id,
    gate: gatedIds.has(id),
    base: base.get(id),
    head: head.get(id),
    ...(definitionDigests.has(id) ? { definitionSha256: definitionDigests.get(id) } : {}),
    ...(!base.has(id) && !head.has(id)
      ? { error: 'no Criterion measurement in either revision' }
      : {}),
  }))
}

function digest(value) {
  return createHash('sha256').update(JSON.stringify(value)).digest('hex')
}

export function compareProbeResults(base, head) {
  if (base?.schemaVersion !== PROBE_SCHEMA || head?.schemaVersion !== PROBE_SCHEMA
      || !Array.isArray(base.cases) || !Array.isArray(head.cases)) {
    return { status: 'unavailable', reason: `probe data must use ${PROBE_SCHEMA}`, cases: [] }
  }
  const baseCases = new Map(base.cases.map((item) => [item.id, item]))
  const headCases = new Map(head.cases.map((item) => [item.id, item]))
  if (baseCases.size !== PROBE_CASES.length || headCases.size !== PROBE_CASES.length
      || base.cases.length !== PROBE_CASES.length || head.cases.length !== PROBE_CASES.length
      || PROBE_CASES.some((id) => !baseCases.has(id) || !headCases.has(id))) {
    return { status: 'unavailable', reason: 'probe cases are missing or duplicated', cases: [] }
  }
  const cases = PROBE_CASES.map((id) => {
    const before = baseCases.get(id)
    const after = headCases.get(id)
    if (!Object.hasOwn(before, 'output') || !Array.isArray(before.diagnostics)
        || !Object.hasOwn(after, 'output') || !Array.isArray(after.diagnostics)) {
      return null
    }
    return {
      id,
      outputEqual: isDeepStrictEqual(before.output, after.output),
      diagnosticsEqual: isDeepStrictEqual(before.diagnostics, after.diagnostics),
      baseOutputSha256: digest(before.output),
      headOutputSha256: digest(after.output),
      baseDiagnosticsSha256: digest(before.diagnostics),
      headDiagnosticsSha256: digest(after.diagnostics),
    }
  })
  if (cases.includes(null)) {
    return { status: 'unavailable', reason: 'probe output or diagnostics are missing', cases: [] }
  }
  return {
    status: cases.every(({ outputEqual, diagnosticsEqual }) => outputEqual && diagnosticsEqual)
      ? 'equivalent' : 'different',
    cases,
  }
}

function runEquivalenceProbe(baseWorktree, targetDir, outputDir) {
  const relative = join('crates', 'wake_bundler', 'examples', 'performance_equivalence.rs')
  const baseProbe = join(baseWorktree, relative)
  const probeDir = join(outputDir, 'equivalence')
  mkdirSync(dirname(baseProbe), { recursive: true })
  mkdirSync(probeDir, { recursive: true })
  copyFileSync(join(root, relative), baseProbe)
  const run = (worktree) => command('cargo', [
    'run', '--quiet', '--release', '--locked', '--offline',
    '--manifest-path', join(worktree, 'Cargo.toml'),
    '-p', 'wake_bundler', '--example', 'performance_equivalence',
  ], {
    capture: true,
    maxBuffer: 32 * 1024 * 1024,
    env: { CARGO_TARGET_DIR: targetDir, CARGO_NET_OFFLINE: 'true', CARGO_INCREMENTAL: '0' },
  })
  try {
    const base = JSON.parse(run(baseWorktree))
    writeFileSync(join(probeDir, 'base.json'), `${JSON.stringify(base, null, 2)}\n`)
    const head = JSON.parse(run(root))
    writeFileSync(join(probeDir, 'head.json'), `${JSON.stringify(head, null, 2)}\n`)
    return compareProbeResults(base, head)
  } catch (error) {
    writeFileSync(join(probeDir, 'error.txt'), `${error.stack ?? error.message}\n`)
    return { status: 'unavailable', reason: 'probe compilation, execution, or JSON parsing failed; see equivalence/error.txt', cases: [] }
  }
}

function benchProfile(manifest) {
  const lines = readFileSync(manifest, 'utf8').split(/\r?\n/)
  const start = lines.findIndex((line) => line.trim() === '[profile.bench]')
  if (start === -1) return ''
  let end = start + 1
  while (end < lines.length && !/^\s*\[.+\]\s*$/.test(lines[end])) end += 1
  return lines.slice(start, end).join('\n').trim()
}

function benchmarkDefinitionSha256(spec) {
  const hash = createHash('sha256')
  hash.update('wake-benchmark-definition-v1\0')
  hash.update(benchProfile(join(root, 'Cargo.toml')))
  hash.update('\0')
  hash.update(readFileSync(join(root, 'Cargo.lock')))
  hash.update('\0')
  hash.update(readFileSync(join(root, 'crates', spec.package, 'benches', `${spec.bench}.rs`)))
  return hash.digest('hex')
}

function baselineCompatibility(baseWorktree, specs) {
  const reasons = []
  if (benchProfile(join(baseWorktree, 'Cargo.toml')) !== benchProfile(join(root, 'Cargo.toml'))) {
    reasons.push('the Cargo bench profile changed')
  }
  for (const spec of specs) {
    const relative = join('crates', spec.package, 'benches', `${spec.bench}.rs`)
    const basePath = join(baseWorktree, relative)
    const headPath = join(root, relative)
    if (!existsSync(basePath) || !existsSync(headPath)
      || readFileSync(basePath, 'utf8') !== readFileSync(headPath, 'utf8')) {
      reasons.push(`${relative} changed`)
    }
  }
  return { compatible: reasons.length === 0, reasons: [...new Set(reasons)] }
}

function writeFailure(outputDir, error) {
  mkdirSync(outputDir, { recursive: true })
  writeFileSync(join(outputDir, 'summary.md'), `# Wake performance report\n\nBenchmark infrastructure failed: ${error.message}\n`)
  writeFileSync(join(outputDir, 'error.txt'), `${error.stack ?? error.message}\n`)
}

export function runPerformance(args = process.argv.slice(2)) {
  const options = parseArgs(args)
  const mode = options.mode ?? 'key'
  if (!['key', 'full'].includes(mode)) throw new Error('--mode must be key or full')

  const headRef = options.head ?? 'HEAD'
  const headSha = command('git', ['rev-parse', headRef], { capture: true })
  const baseRef = options.base ?? `${headSha}^`
  const baseSha = command('git', ['rev-parse', baseRef], { capture: true })
  const outputDir = resolve(options.output ?? join(root, '.wake-performance'))
  const targetDir = resolve(process.env.CARGO_TARGET_DIR ?? join(root, 'target'))
  const criterionDir = join(targetDir, 'criterion')
  const tempRoot = resolve(process.env.RUNNER_TEMP ?? join(root, '.tmp'))
  const baseWorktree = join(tempRoot, `wake-performance-base-${process.pid}`)
  const specs = mode === 'key' ? keyBenchmarks : fullBenchmarks
  const gatedIds = new Set(mode === 'key' ? keyBenchmarks.flatMap(gateIdsFor) : [])

  rmSync(outputDir, { recursive: true, force: true })
  mkdirSync(outputDir, { recursive: true })
  rmSync(criterionDir, { recursive: true, force: true })
  mkdirSync(tempRoot, { recursive: true })

  let worktreeAdded = false
  try {
    command('git', ['worktree', 'add', '--detach', baseWorktree, baseSha])
    worktreeAdded = true
    command('cargo', ['fetch', '--locked', '--manifest-path', join(baseWorktree, 'Cargo.toml')])
    command('cargo', ['fetch', '--locked', '--manifest-path', join(root, 'Cargo.toml')])

    // A benchmark added in the current revision has no executable base. Keep it
    // in the report as `new` instead of failing the whole comparison before the
    // compatibility check can explain why the result is report-only.
    const baseSpecs = specs.filter((spec) =>
      existsSync(join(baseWorktree, 'crates', spec.package, 'Cargo.toml'))
      && existsSync(join(baseWorktree, 'crates', spec.package, 'benches', `${spec.bench}.rs`)),
    )
    for (const spec of baseSpecs) {
      cargoBench(baseWorktree, spec, ['--save-baseline', CRITERION_BASELINE], targetDir)
    }
    const base = readCriterionSnapshot(criterionDir, CRITERION_BASELINE)
    clearTransientCriterion(criterionDir)

    const definitionDigests = new Map()
    for (const spec of specs) {
      const existingIds = new Set(readCriterionSnapshot(criterionDir, 'new').keys())
      cargoBench(root, spec, ['--baseline-lenient', CRITERION_BASELINE], targetDir)
      for (const id of readCriterionSnapshot(criterionDir, 'new').keys()) {
        if (!existingIds.has(id)) definitionDigests.set(id, benchmarkDefinitionSha256(spec))
      }
    }
    const head = readCriterionSnapshot(criterionDir, 'new')
    const compatibility = baselineCompatibility(baseWorktree, specs)
    const raw = {
      schemaVersion: PERFORMANCE_SCHEMA,
      mode: mode === 'key' ? 'pull-request' : 'trend',
      baseSha,
      headSha,
      warningThresholdPercent: 5,
      failureThresholdPercent: 15,
      enforce: mode === 'key' && compatibility.compatible,
      baselineCompatibility: compatibility,
      environment: {
        rustc: command('rustc', ['-Vv'], { capture: true }),
        runnerOs: process.env.RUNNER_OS ?? process.platform,
        runnerArch: process.env.RUNNER_ARCH ?? process.arch,
        runnerImageVersion: process.env.ImageVersion ?? null,
        cpuModel: cpus()[0]?.model ?? null,
        logicalCpuCount: availableParallelism(),
        totalMemoryBytes: totalmem(),
        node: process.version,
      },
      benchmarks: reportEntries(base, head, gatedIds, definitionDigests),
    }

    const first = evaluateReport(raw)
    const candidates = first.benchmarks.filter((benchmark) =>
      benchmark.gate && benchmark.first?.changePercent >= raw.failureThresholdPercent)
    const candidateIds = new Set(candidates.map(({ id }) => id))
    for (const spec of keyRetestSpecsForIds(candidateIds)) {
      cargoBench(root, spec, ['--baseline-lenient', CRITERION_BASELINE], targetDir)
      const retested = readCriterionSnapshot(criterionDir, 'new')
      for (const id of gateIdsFor(spec)) {
        if (candidateIds.has(id)) {
          raw.benchmarks.find((benchmark) => benchmark.id === id).retest = retested.get(id)
        }
      }
    }

    raw.requireOutputEquivalence = mode === 'key'
    raw.outputEquivalence = runEquivalenceProbe(baseWorktree, targetDir, outputDir)

    const report = evaluateReport(raw)
    writeFileSync(join(outputDir, 'report.json'), `${JSON.stringify(report, null, 2)}\n`)
    writeFileSync(join(outputDir, 'summary.md'), renderMarkdown(report))
    if (existsSync(criterionDir)) cpSync(criterionDir, join(outputDir, 'criterion'), { recursive: true })
    return report.failed ? 2 : 0
  } catch (error) {
    writeFailure(outputDir, error)
    throw error
  } finally {
    if (worktreeAdded) command('git', ['worktree', 'remove', '--force', baseWorktree])
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exitCode = runPerformance()
  } catch (error) {
    console.error(error.stack ?? error.message)
    process.exitCode = 1
  }
}
