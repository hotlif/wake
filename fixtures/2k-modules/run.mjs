#!/usr/bin/env node
// 2k-modules · wake vs webpack 构建对比（时间 + 内存峰值 + 产物大小）。
//
// 计量口径（关键）：**时间与内存分开测，各用各的干净口径**——
//  - 时间：直接 spawn 构建进程计墙钟，**不套** memwrap.ps1。因为内存包装器是一层 PowerShell 进程 +
//    100ms 轮询循环，实测给每次构建叠 ~320ms 固定开销（PowerShell 启动 ~250ms + Start-Process/轮询）。
//    这个常数加在两侧，但 webpack 有 3000ms+ 几乎无感、wake 只有 ~160ms 会被它主导——把真实的 ~19×
//    硬拉到 ~6.7×（同「把 node 验证跑算进构建时间」一类的计量陷阱）。
//  - 内存：用 memwrap.ps1 包装采样峰值 WorkingSet，其墙钟含 PowerShell 开销，**只取内存不取时间**。
import { execSync, spawnSync } from 'node:child_process';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { performance } from 'node:perf_hooks';
import { fileURLToPath } from 'node:url';

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

// 纯构建墙钟（不套内存包装器）。
function timeBuild(exe, args) {
  const start = performance.now();
  const r = spawnSync(exe, args, { cwd: __dirname, stdio: 'pipe', timeout: 300_000 });
  const elapsed = performance.now() - start;
  if (r.status !== 0) {
    process.stderr.write((r.stdout?.toString() || '') + (r.stderr?.toString() || ''));
    process.stderr.write(`  ❌ 构建失败: ${exe}\n`);
    process.exit(1);
  }
  return elapsed;
}

// 峰值内存（memwrap.ps1 包装采样；只取内存，其墙钟含 PowerShell 开销故丢弃）。
function memBuild(exe, args) {
  const r = spawnSync('powershell', [
    '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', psWrap, exe, ...args,
  ], { cwd: __dirname, timeout: 300_000 });
  if (r.stderr) process.stderr.write(r.stderr.toString());
  const m = (r.stdout?.toString() || '').match(/__PEAK_MB__:(\d+)/);
  return m ? parseInt(m[1]) : 0;
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
  timeBuild(exe, args);
  if (!verifyBundle(bundlePath)) process.exit(1);

  // 时间（不套包装）
  const times = [];
  for (let i = 0; i < TIME_RUNS; i++) {
    times.push(timeBuild(exe, args));
    process.stdout.write(`    time ${i + 1}/${TIME_RUNS}: ${formatMs(times.at(-1))}\n`);
  }
  // 内存（套包装采样峰值）
  const mems = [];
  for (let i = 0; i < MEM_RUNS; i++) {
    mems.push(memBuild(exe, args));
    process.stdout.write(`    mem  ${i + 1}/${MEM_RUNS}: peak=${mems.at(-1)}MB\n`);
  }
  const avg = times.reduce((a, b) => a + b, 0) / times.length;
  return { avg, min: Math.min(...times), max: Math.max(...times), avgMem: Math.round(mems.reduce((a, b) => a + b, 0) / mems.length) };
}

// ---- 1. generate ----
const inputDir = join(__dirname, 'input');
if (!existsSync(join(inputDir, 'src'))) {
  console.log('[1/4] Generating synthetic modules...');
  execSync('node generate.mjs', { cwd: __dirname, stdio: 'pipe', timeout: 120_000 });
} else {
  console.log('[1/4] Input modules already generated, skipping.');
}

// ---- 2. tool paths ----
const wakeBin = join(__dirname, '..', '..', 'target', 'release', 'wake.exe');
const wakeOut = join(__dirname, 'dist-wake');
const wakeBundle = join(wakeOut, 'bundle.js');
const entry = join(__dirname, 'input', 'entry.js');
const nodeExe = process.execPath;
const wpEntry = join(__dirname, 'node_modules', 'webpack-cli', 'bin', 'cli.js');
const webpackCfg = join(__dirname, 'webpack.config.mjs');
const webpackOut = join(__dirname, 'dist-webpack');
const webpackBundle = join(webpackOut, 'bundle.js');

// ---- 3. wake ----
console.log('[2/4] Building with wake...');
const wakeStats = measure('wake', wakeBin, ['build', entry, '--outdir', wakeOut], wakeBundle);
let wakeBundleSize = 0; try { wakeBundleSize = statSync(wakeBundle).size; } catch {}

// ---- 4. webpack ----
console.log('[3/4] Building with webpack...');
const wpStats = measure('webpack', nodeExe, [wpEntry, '--config', webpackCfg], webpackBundle);
let wpBundleSize = 0; try { wpBundleSize = statSync(webpackBundle).size; } catch {}

// ---- 5. results ----
const speedup = wpStats.avg / wakeStats.avg;
const timeSaved = Math.round(wpStats.avg - wakeStats.avg);
const memSaved = wpStats.avgMem - wakeStats.avgMem;
const memPct = Math.round((1 - wakeStats.avgMem / wpStats.avgMem) * 100);
const BAR = 20;
const bar = (v, mx) => '█'.repeat(Math.max(1, Math.round((v / mx) * BAR))) + '░'.repeat(BAR - Math.max(1, Math.round((v / mx) * BAR)));

console.log(`\n${'─'.repeat(56)}`);
console.log('  2k-modules · wake vs webpack 构建对比');
console.log('  真实业务项目风格 (2013 模块, ~2000 文件)');
console.log('  时间=纯构建墙钟(不套内存包装器) · 内存=memwrap 采样峰值');
console.log(`${'─'.repeat(56)}`);
console.log(`\n  构建时间 (avg, 越小越好)`);
console.log(`    wake     ${formatMs(wakeStats.avg).padStart(7)}  ${bar(wakeStats.avg, wpStats.avg)}  ${speedup.toFixed(1)}× 更快`);
console.log(`    webpack  ${formatMs(wpStats.avg).padStart(7)}  ${bar(wpStats.avg, wpStats.avg)}  基准`);
console.log(`  ⚡ CPU: wake 快 ${formatMs(timeSaved)} (${speedup.toFixed(1)}×)`);
console.log(`\n  内存峰值 (avg, 越小越好)`);
console.log(`    wake     ${`${wakeStats.avgMem}MB`.padStart(7)}  ${bar(wakeStats.avgMem, wpStats.avgMem)}  节省 ${memPct}%`);
console.log(`    webpack  ${`${wpStats.avgMem}MB`.padStart(7)}  ${bar(wpStats.avgMem, wpStats.avgMem)}  基准`);
console.log(`  💾 内存: wake 节约 ${formatBytes(memSaved * 1024 * 1024)} (${memPct}%)`);
console.log(`\n  产物大小  wake ${formatBytes(wakeBundleSize)}  vs  webpack ${formatBytes(wpBundleSize)}`);
console.log(`${'─'.repeat(56)}\n`);
