#!/usr/bin/env node
// 2k-modules · wake vs Vite vs webpack 构建对比（时间 + 内存峰值 + 产物大小）。
//
// 计量口径（关键）：**时间与内存分开测，各用各的干净口径**——
//  - 时间：直接 spawn 构建进程计墙钟，**不套** memwrap.ps1。因为内存包装器是一层 PowerShell 进程 +
//    轮询循环，会给每次构建叠加 PowerShell 启动、Start-Process 和轮询开销。
//    这个常数会主导更快的构建器并系统性压低差距，与把 node 产物验证计入构建时间是同类计量陷阱。
//  - 内存：用 memwrap.ps1 包装采样峰值 WorkingSet，其墙钟含 PowerShell 开销，**只取内存不取时间**。
import { execSync, spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { performance } from 'node:perf_hooks';
import { fileURLToPath } from 'node:url';
import { brotliCompressSync, constants as zlibConstants, gzipSync } from 'node:zlib';

const __dirname = fileURLToPath(new URL('.', import.meta.url));
const psWrap = join(__dirname, 'memwrap.ps1');
const TIME_RUNS = 5;
const MEM_RUNS = 2;
const expected = readFileSync(join(__dirname, 'expected', 'checksum.txt'), 'utf8').trim();

function formatMs(ms) { return `${ms.toFixed(0)}ms`; }
function formatBytes(bytes) {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)}MB`;
}

function artifactStats(path) {
  const bytes = readFileSync(path);
  const source = bytes.toString('utf8');
  const count = (text) => source.split(text).length - 1;
  return {
    raw: bytes.length,
    gzip: gzipSync(bytes, { level: 9 }).length,
    brotli: brotliCompressSync(bytes, {
      params: {
        [zlibConstants.BROTLI_PARAM_QUALITY]: 11,
      },
    }).length,
    wrappers: [...source.matchAll(/(?:^|[,;{])\d+:function\(/g)].length,
    requires: count('__wake_require__'),
  };
}

// 纯构建墙钟（不套内存包装器）。
function timeBuild(name, exe, args) {
  const start = performance.now();
  const r = spawnSync(exe, args, { cwd: __dirname, stdio: 'pipe', timeout: 300_000 });
  const elapsed = performance.now() - start;
  if (r.status !== 0) {
    process.stderr.write((r.stdout?.toString() || '') + (r.stderr?.toString() || ''));
    process.stderr.write(`  ❌ ${name} 构建失败: ${exe}\n`);
    process.exit(1);
  }
  return elapsed;
}

// 峰值内存（memwrap.ps1 包装采样；只取内存，其墙钟含 PowerShell 开销故丢弃）。
function memBuild(name, exe, args) {
  const r = spawnSync('powershell', [
    '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', psWrap, exe, ...args,
  ], { cwd: __dirname, timeout: 300_000 });
  if (r.status !== 0) {
    process.stderr.write((r.stdout?.toString() || '') + (r.stderr?.toString() || ''));
    process.stderr.write(`  ❌ ${name} 内存采样失败\n`);
    process.exit(1);
  }
  const m = (r.stdout?.toString() || '').match(/__PEAK_MB__:(\d+)/);
  if (!m) {
    process.stderr.write(`  ❌ ${name} 内存采样未返回峰值\n`);
    process.exit(1);
  }
  return parseInt(m[1]);
}

function verifyBundle(bundlePath) {
  try {
    const out = execSync(`node "${bundlePath}"`, { cwd: __dirname, stdio: 'pipe', timeout: 30_000 });
    if (!out.toString().trim().includes(expected)) {
      process.stderr.write(`  ❌ 输出不匹配! 期望: ${expected}\n`);
      return false;
    }
    return true;
  } catch (e) {
    process.stderr.write(`  ❌ 运行 bundle 失败: ${e.message}\n`);
    return false;
  }
}

function measure(name, exe, args, bundlePath) {
  // warmup + 正确性验证
  timeBuild(name, exe, args);
  if (!verifyBundle(bundlePath)) process.exit(1);

  // 时间（不套包装）
  const times = [];
  for (let i = 0; i < TIME_RUNS; i++) {
    times.push(timeBuild(name, exe, args));
    process.stdout.write(`    time ${i + 1}/${TIME_RUNS}: ${formatMs(times.at(-1))}\n`);
  }
  // 内存（套包装采样峰值）
  const mems = [];
  for (let i = 0; i < MEM_RUNS; i++) {
    mems.push(memBuild(name, exe, args));
    process.stdout.write(`    mem  ${i + 1}/${MEM_RUNS}: peak=${mems.at(-1)}MB\n`);
  }
  const avg = times.reduce((a, b) => a + b, 0) / times.length;
  return { avg, min: Math.min(...times), max: Math.max(...times), avgMem: Math.round(mems.reduce((a, b) => a + b, 0) / mems.length) };
}

// ---- 1. generate ----
const inputDir = join(__dirname, 'input');
if (!existsSync(join(inputDir, 'src'))) {
  console.log('[1/5] Generating synthetic modules...');
  execSync('node generate.mjs', { cwd: __dirname, stdio: 'pipe', timeout: 120_000 });
} else {
  console.log('[1/5] Input modules already generated, skipping.');
}

// ---- 2. tool paths ----
const wakeBin = join(__dirname, '..', '..', 'target', 'release', 'wake.exe');
const wakeOut = join(__dirname, 'dist-wake');
const wakeBundle = join(wakeOut, 'bundle.js');
const entry = join(__dirname, 'input', 'entry.js');
const nodeExe = process.execPath;
const viteEntry = join(__dirname, 'node_modules', 'vite', 'bin', 'vite.js');
const viteCfg = join(__dirname, 'vite.config.mjs');
const viteOut = join(__dirname, 'dist-vite');
const viteBundle = join(viteOut, 'bundle.js');
const wpEntry = join(__dirname, 'node_modules', 'webpack-cli', 'bin', 'cli.js');
const webpackCfg = join(__dirname, 'webpack.config.mjs');
const webpackOut = join(__dirname, 'dist-webpack');
const webpackBundle = join(webpackOut, 'bundle.js');

// ---- 3. wake ----
console.log('[2/5] Building with wake...');
const wakeStats = measure('wake', wakeBin, ['build', entry, '--outdir', wakeOut], wakeBundle);
const wakeArtifact = artifactStats(wakeBundle);

// ---- 4. Vite ----
console.log('[3/5] Building with Vite...');
const viteStats = measure('Vite', nodeExe, [viteEntry, 'build', '--config', viteCfg], viteBundle);
const viteArtifact = artifactStats(viteBundle);

// ---- 5. webpack ----
console.log('[4/5] Building with webpack...');
const wpStats = measure('webpack', nodeExe, [wpEntry, '--config', webpackCfg], webpackBundle);
const wpArtifact = artifactStats(webpackBundle);

// ---- 6. results ----
console.log('[5/5] Results');
const results = [
  { name: 'wake', stats: wakeStats, artifact: wakeArtifact },
  { name: 'Vite', stats: viteStats, artifact: viteArtifact },
  { name: 'webpack', stats: wpStats, artifact: wpArtifact },
];
const maxTime = Math.max(...results.map(({ stats }) => stats.avg));
const maxMem = Math.max(...results.map(({ stats }) => stats.avgMem));
const BAR = 20;
const bar = (v, mx) => '█'.repeat(Math.max(1, Math.round((v / mx) * BAR))) + '░'.repeat(BAR - Math.max(1, Math.round((v / mx) * BAR)));
const comparison = (candidate, baseline) => {
  const ratio = candidate / baseline;
  if (ratio >= 1) return `wake 快 ${ratio.toFixed(1)}×`;
  return `wake 慢 ${(1 / ratio).toFixed(1)}×`;
};
const memoryComparison = (candidate, baseline) => {
  const percent = Math.round(Math.abs(1 - baseline / candidate) * 100);
  return candidate >= baseline ? `wake 少 ${percent}%` : `wake 多 ${percent}%`;
};

console.log(`\n${'─'.repeat(56)}`);
console.log('  2k-modules · wake vs Vite vs webpack 构建对比');
console.log('  真实业务项目风格 (2013 模块, ~2000 文件)');
console.log('  时间=纯构建墙钟(不套内存包装器) · 内存=memwrap 采样峰值');
console.log(`${'─'.repeat(56)}`);
console.log(`\n  构建时间 (avg, 越小越好)`);
for (const { name, stats } of results) {
  const note = name === 'wake' ? '基准' : comparison(stats.avg, wakeStats.avg);
  console.log(`    ${name.padEnd(8)} ${formatMs(stats.avg).padStart(7)}  ${bar(stats.avg, maxTime)}  ${note}`);
}
console.log(`\n  内存峰值 (avg, 越小越好)`);
for (const { name, stats } of results) {
  const note = name === 'wake' ? '基准' : memoryComparison(stats.avgMem, wakeStats.avgMem);
  console.log(`    ${name.padEnd(8)} ${`${stats.avgMem}MB`.padStart(7)}  ${bar(stats.avgMem, maxMem)}  ${note}`);
}
console.log(`\n  产物大小 (raw / gzip -9 / brotli 11)`);
for (const { name, artifact } of results) {
  console.log(`    ${name.padEnd(8)} ${formatBytes(artifact.raw).padStart(8)} / ${formatBytes(artifact.gzip).padStart(8)} / ${formatBytes(artifact.brotli).padStart(8)}`);
}
console.log(`  结构统计  wake wrappers=${wakeArtifact.wrappers} require-token=${wakeArtifact.requires}`);
console.log(`${'─'.repeat(56)}\n`);
