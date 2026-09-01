#!/usr/bin/env node
// Northstar 2k modules: Wake vs Vite vs webpack production bundle benchmark.
// Timing and memory are separate: timed samples spawn builds directly; memory wrappers' time is ignored.
import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { cpus, totalmem } from 'node:os';
import { extname, join, relative } from 'node:path';
import { performance } from 'node:perf_hooks';
import { fileURLToPath } from 'node:url';
import { brotliCompressSync, constants as zlibConstants, gzipSync } from 'node:zlib';

const fixtureDir = fileURLToPath(new URL('.', import.meta.url));
const repositoryDir = join(fixtureDir, '..', '..');
const inputEntry = join(fixtureDir, 'input', 'entry.js');
const projectManifestPath = join(fixtureDir, 'expected', 'project.json');
const expectedStdoutPath = join(fixtureDir, 'expected', 'checksum.txt');
const memoryWrapper = join(fixtureDir, 'memwrap.ps1');
const TIME_RUNS = 5;
const MEMORY_RUNS = 2;
const COMMAND_TIMEOUT_MS = 300_000;
const MAX_BUFFER_BYTES = 64 * 1024 * 1024;
const BROWSER_TARGETS = 'Chrome/Edge 120 · Firefox 121 · Safari/iOS 17.2';

function fail(message, detail = '') {
  process.stderr.write(`  ❌ ${message}\n`);
  if (detail) process.stderr.write(detail.endsWith('\n') ? detail : `${detail}\n`);
  process.exit(1);
}

function run(executable, args, options = {}) {
  const result = spawnSync(executable, args, {
    cwd: fixtureDir,
    encoding: Object.hasOwn(options, 'encoding') ? options.encoding : 'utf8',
    maxBuffer: MAX_BUFFER_BYTES,
    stdio: options.stdio ?? 'pipe',
    timeout: options.timeout ?? COMMAND_TIMEOUT_MS,
    windowsHide: true,
  });
  if (result.error || result.status !== 0) {
    const stdout = Buffer.isBuffer(result.stdout) ? result.stdout.toString() : (result.stdout || '');
    const stderr = Buffer.isBuffer(result.stderr) ? result.stderr.toString() : (result.stderr || '');
    fail(
      `${options.label || executable} failed${result.status == null ? '' : ` (exit ${result.status})`}`,
      `${stdout}${stderr}${result.error ? `${result.error.message}\n` : ''}`,
    );
  }
  return result;
}

function cleanOutput(outDir) {
  rmSync(outDir, { recursive: true, force: true });
}

function walkFiles(directory) {
  if (!existsSync(directory)) return [];
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...walkFiles(path));
    else if (entry.isFile()) files.push(path);
  }
  return files.sort((a, b) => a.localeCompare(b));
}

function assertSingleJavaScript(name, outDir) {
  const files = walkFiles(outDir);
  const maps = files.filter((path) => path.endsWith('.map'));
  const scripts = files.filter((path) => extname(path) === '.js');
  if (maps.length !== 0) {
    fail(`${name} emitted source maps although the benchmark disables them`, maps.join('\n'));
  }
  if (scripts.length !== 1) {
    const listing = files.length === 0
      ? '(output directory is empty)'
      : files.map((path) => relative(outDir, path)).join('\n');
    fail(`${name} must emit exactly one JavaScript artifact; found ${scripts.length}`, listing);
  }
  return scripts[0];
}

function executeJavaScript(label, path) {
  return run(process.execPath, [path], { label, encoding: null, timeout: 30_000 }).stdout;
}

function assertExactStdout(label, actual, oracle) {
  if (Buffer.compare(actual, oracle) !== 0) {
    fail(
      `${label} stdout differs from the source oracle`,
      `expected (${oracle.length} bytes): ${JSON.stringify(oracle.toString())}\nactual (${actual.length} bytes): ${JSON.stringify(actual.toString())}`,
    );
  }
}

function verifyBuiltArtifact(tool, oracle) {
  const script = assertSingleJavaScript(tool.name, tool.outDir);
  assertExactStdout(`${tool.name} bundle`, executeJavaScript(`${tool.name} bundle`, script), oracle);
  return script;
}

function build(tool) {
  return run(tool.executable, tool.args, { label: `${tool.name} build` });
}

function timedBuild(tool, oracle) {
  cleanOutput(tool.outDir);
  const started = performance.now();
  build(tool);
  const elapsed = performance.now() - started;
  verifyBuiltArtifact(tool, oracle);
  return elapsed;
}

function windowsMemoryBuild(tool) {
  const result = run('powershell', [
    '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', memoryWrapper,
    tool.executable, ...tool.args,
  ], { label: `${tool.name} memory sample` });
  const match = result.stdout.match(/__PEAK_MB__:(\d+(?:\.\d+)?)/);
  if (!match) fail(`${tool.name} memory wrapper did not report peak WorkingSet`, result.stdout);
  return Number(match[1]);
}

function linuxMemoryBuild(tool) {
  if (!existsSync('/usr/bin/time')) return null;
  const result = run('/usr/bin/time', ['-v', tool.executable, ...tool.args], {
    label: `${tool.name} memory sample`,
  });
  const match = result.stderr.match(/Maximum resident set size \(kbytes\):\s*(\d+)/);
  if (!match) fail(`${tool.name} GNU time did not report maximum resident set size`, result.stderr);
  return Number(match[1]) / 1024;
}

function macMemoryBuild(tool) {
  if (!existsSync('/usr/bin/time')) return null;
  const result = run('/usr/bin/time', ['-l', tool.executable, ...tool.args], {
    label: `${tool.name} memory sample`,
  });
  const match = result.stderr.match(/^\s*(\d+)\s+maximum resident set size$/m);
  if (!match) fail(`${tool.name} BSD time did not report maximum resident set size`, result.stderr);
  return Number(match[1]) / (1024 * 1024);
}

function memoryBuild(tool, oracle) {
  cleanOutput(tool.outDir);
  let peakMb = null;
  if (process.platform === 'win32') peakMb = windowsMemoryBuild(tool);
  else if (process.platform === 'linux') peakMb = linuxMemoryBuild(tool);
  else if (process.platform === 'darwin') peakMb = macMemoryBuild(tool);
  if (peakMb == null) build(tool);
  verifyBuiltArtifact(tool, oracle);
  return peakMb;
}

function artifactStats(path) {
  const bytes = readFileSync(path);
  return {
    raw: bytes.length,
    gzip: gzipSync(bytes, { level: 9, mtime: 0 }).length,
    brotli: brotliCompressSync(bytes, {
      params: { [zlibConstants.BROTLI_PARAM_QUALITY]: 11 },
    }).length,
  };
}

function mean(values) {
  return values.reduce((sum, value) => sum + value, 0) / values.length;
}

function measure(tool, oracle) {
  timedBuild(tool, oracle); // warmup
  const times = [];
  for (let index = 0; index < TIME_RUNS; index++) {
    times.push(timedBuild(tool, oracle));
    process.stdout.write(`    time ${index + 1}/${TIME_RUNS}: ${formatMs(times.at(-1))}\n`);
  }

  const memory = [];
  for (let index = 0; index < MEMORY_RUNS; index++) {
    const peak = memoryBuild(tool, oracle);
    if (peak == null) break;
    memory.push(peak);
    process.stdout.write(`    mem  ${index + 1}/${MEMORY_RUNS}: peak=${formatMb(peak)}\n`);
  }

  const script = assertSingleJavaScript(tool.name, tool.outDir);
  return {
    timing: { average: mean(times), min: Math.min(...times), max: Math.max(...times) },
    averageMemoryMb: memory.length === 0 ? null : mean(memory),
    artifact: artifactStats(script),
  };
}

function readProjectManifest() {
  let project;
  try {
    project = JSON.parse(readFileSync(projectManifestPath, 'utf8'));
  } catch (error) {
    fail(`cannot read generated project manifest ${projectManifestPath}`, error.message);
  }
  const valid = project?.schemaVersion === 1
    && typeof project.project === 'string'
    && Number.isInteger(project.modules?.total)
    && project.modules.total > 0
    && project.modules.categories
    && Number.isInteger(project.source?.files)
    && Number.isInteger(project.source?.bytes)
    && Number.isInteger(project.source?.lines);
  if (!valid) fail('expected/project.json does not match schemaVersion 1');
  return project;
}

function formatMs(value) { return `${value.toFixed(0)}ms`; }
function formatMb(value) { return `${value.toFixed(1)}MB`; }
function formatBytes(value) {
  if (value < 1024) return `${value}B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)}KB`;
  return `${(value / (1024 * 1024)).toFixed(2)}MB`;
}

const wakeExecutable = join(
  repositoryDir, 'target', 'release', process.platform === 'win32' ? 'wake.exe' : 'wake',
);
const viteExecutable = join(fixtureDir, 'node_modules', 'vite', 'bin', 'vite.js');
const webpackExecutable = join(fixtureDir, 'node_modules', 'webpack-cli', 'bin', 'cli.js');
for (const [name, path] of [
  ['Wake release binary', wakeExecutable],
  ['Vite CLI', viteExecutable],
  ['webpack CLI', webpackExecutable],
]) {
  if (!existsSync(path)) fail(`${name} not found at ${path}`);
}

console.log('[1/6] Regenerating deterministic Northstar sources...');
run(process.execPath, [join(fixtureDir, 'generate.mjs')], {
  label: 'Northstar generator', stdio: 'inherit', timeout: 120_000,
});
console.log('[2/6] Verifying project graph and source manifest...');
run(process.execPath, [join(fixtureDir, 'verify-project.mjs')], {
  label: 'Northstar project verifier', stdio: 'inherit', timeout: 120_000,
});

const project = readProjectManifest();
const expectedStdout = readFileSync(expectedStdoutPath);
const sourceStdout = executeJavaScript('Northstar source oracle', inputEntry);
assertExactStdout('Northstar source', sourceStdout, expectedStdout);

const tools = [
  {
    name: 'wake',
    executable: wakeExecutable,
    args: [
      'bundle', inputEntry,
      '--outfile', join(fixtureDir, 'dist-wake', 'bundle.js'),
      '--platform', 'browser', '--format', 'iife', '--minify',
      '--config', join(fixtureDir, 'wake.config.toml'),
    ],
    outDir: join(fixtureDir, 'dist-wake'),
  },
  {
    name: 'Vite',
    executable: process.execPath,
    args: [viteExecutable, 'build', '--config', join(fixtureDir, 'vite.config.mjs')],
    outDir: join(fixtureDir, 'dist-vite'),
  },
  {
    name: 'webpack',
    executable: process.execPath,
    args: [webpackExecutable, '--config', join(fixtureDir, 'webpack.config.mjs')],
    outDir: join(fixtureDir, 'dist-webpack'),
  },
];

const measured = [];
for (const [index, tool] of tools.entries()) {
  console.log(`[${index + 3}/6] Building with ${tool.name}...`);
  measured.push({ name: tool.name, ...measure(tool, sourceStdout) });
}

console.log('[6/6] Results');
const categories = Object.entries(project.modules.categories)
  .map(([name, count]) => `${name}=${count}`)
  .join(', ');
console.log(`\n${'─'.repeat(72)}`);
console.log(`  ${project.project} · Wake vs Vite vs webpack`);
console.log(`  application modules=${project.modules.total} + entry wrapper · bundler inputs=${project.graph.bundlerModules}`);
console.log(`  categories: ${categories}`);
console.log(`  source=${project.source.files} files / ${formatBytes(project.source.bytes)} / ${project.source.lines} lines`);
console.log(`  environment=${process.platform}-${process.arch} · Node ${process.version} · ${cpus()[0]?.model || 'unknown CPU'} · ${formatBytes(totalmem())} RAM`);
console.log(`  targets=${BROWSER_TARGETS}`);
console.log('  production minify · no source maps · no persistent cache · one JS artifact');
console.log(`${'─'.repeat(72)}`);

console.log('\n  Build time (average; min–max)');
for (const result of measured) {
  const { average, min, max } = result.timing;
  console.log(`    ${result.name.padEnd(8)} ${formatMs(average).padStart(7)}  (${formatMs(min)}–${formatMs(max)})`);
}
console.log('\n  Peak resident memory (average)');
for (const result of measured) {
  const memory = result.averageMemoryMb == null ? 'unavailable' : formatMb(result.averageMemoryMb);
  console.log(`    ${result.name.padEnd(8)} ${memory.padStart(11)}`);
}
console.log('\n  Final JavaScript size (raw / gzip-9 / brotli-11)');
for (const result of measured) {
  const { raw, gzip, brotli } = result.artifact;
  console.log(`    ${result.name.padEnd(8)} ${formatBytes(raw).padStart(8)} / ${formatBytes(gzip).padStart(8)} / ${formatBytes(brotli).padStart(8)}  [${raw} / ${gzip} / ${brotli} B]`);
}
const wake = measured[0];
console.log('\n  Relative to Wake');
for (const result of measured.slice(1)) {
  const timeRatio = result.timing.average / wake.timing.average;
  const memoryRatio = wake.averageMemoryMb == null || result.averageMemoryMb == null
    ? 'memory unavailable'
    : `memory ${(result.averageMemoryMb / wake.averageMemoryMb).toFixed(2)}×`;
  console.log(`    ${result.name.padEnd(8)} time ${timeRatio.toFixed(2)}× · ${memoryRatio}`);
}
console.log(`${'─'.repeat(72)}\n`);
