#!/usr/bin/env node
import { execSync, spawnSync } from 'node:child_process';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { performance } from 'node:perf_hooks';
import { fileURLToPath } from 'node:url';

const __dirname = fileURLToPath(new URL('.', import.meta.url));
const psWrap = join(__dirname, 'memwrap.ps1');
const MEASURE_RUNS = 5;
const expected = readFileSync(join(__dirname, 'expected', 'checksum.txt'), 'utf8').trim();

function formatMs(ms) {
  return `${ms.toFixed(0)}ms`;
}

function formatBytes(bytes) {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)}MB`;
}

function buildWithMem(exe, ...args) {
  const result = spawnSync('powershell', [
    '-NoProfile', '-ExecutionPolicy', 'Bypass',
    '-File', psWrap,
    exe, ...args,
  ], { cwd: __dirname, timeout: 300_000 });
  const stdout = result.stdout?.toString() || '';
  const stderr = result.stderr?.toString() || '';
  if (stderr) process.stderr.write(stderr);
  const memMatch = stdout.match(/__PEAK_MB__:(\d+)/);
  const peakMem = memMatch ? parseInt(memMatch[1]) : 0;
  return { peakMem };
}

function verifyBundle(bundlePath) {
  try {
    const out = execSync(`node "${bundlePath}"`, { cwd: __dirname, stdio: 'pipe', timeout: 30_000 });
    const text = out.toString().trim();
    if (!text.includes(expected)) {
      process.stderr.write(`  ❌ 输出不匹配! 期望: ${expected}\n     实际: ${text}\n`);
      return false;
    }
    return true;
  } catch (e) {
    process.stderr.write(`  ❌ 运行 bundle 失败: ${e.message}\n`);
    return false;
  }
}

function measure(name, buildFn, bundlePath, runs) {
  const times = [];
  const mems = [];
  for (let i = 0; i < runs; i++) {
    const start = performance.now();
    const { peakMem } = buildFn();
    const elapsed = performance.now() - start;
    times.push(elapsed);
    mems.push(peakMem);
    if (!verifyBundle(bundlePath)) process.exit(1);
    process.stdout.write(`    run ${i + 1}/${runs}: ${formatMs(elapsed)}  peak=${peakMem}MB  ✓\n`);
  }
  const avg = times.reduce((a, b) => a + b, 0) / times.length;
  const min = Math.min(...times);
  const max = Math.max(...times);
  const avgMem = Math.round(mems.reduce((a, b) => a + b, 0) / mems.length);
  const peakMem = Math.max(...mems);
  return { times, avg, min, max, avgMem, peakMem };
}

// ---- 1. generate ----
const inputDir = join(__dirname, 'input');
const srcDir = join(inputDir, 'src');
if (!existsSync(srcDir)) {
  console.log('[1/5] Generating 1970 synthetic modules (≈2000)...');
  execSync('node generate.mjs', { cwd: __dirname, stdio: 'pipe', timeout: 120_000 });
} else {
  console.log('[1/5] Input modules already generated, skipping.');
}

// ---- 2. tool paths ----
const wakeBin = join(__dirname, '..', '..', 'target', 'release', 'wake.exe');
const wakeOut = join(__dirname, 'dist-wake');
const wakeBundle = join(wakeOut, 'bundle.js');
const webpackCfg = join(__dirname, 'webpack.config.mjs');
const webpackOut = join(__dirname, 'dist-webpack');
const webpackBundle = join(webpackOut, 'bundle.js');
const entry = join(__dirname, 'input', 'entry.js');

// ---- 3. wake ----
console.log('[2/5] Building with wake...');
const wakeBuild = () => buildWithMem(wakeBin, 'build', entry, '--outdir', wakeOut);

console.log('  warmup...');
wakeBuild();
verifyBundle(wakeBundle);
console.log('  measuring...');
const wakeStats = measure('wake', wakeBuild, wakeBundle, MEASURE_RUNS);

let wakeBundleSize = 0;
try { wakeBundleSize = statSync(wakeBundle).size; } catch {}

// ---- 4. webpack ----
console.log('[3/5] Building with webpack...');
const nodeExe = process.execPath;
const wpEntry = join(__dirname, 'node_modules', 'webpack-cli', 'bin', 'cli.js');
const wpBuild = () => buildWithMem(nodeExe, wpEntry, '--config', webpackCfg);

console.log('  warmup...');
wpBuild();
verifyBundle(webpackBundle);
console.log('  measuring...');
const wpStats = measure('webpack', wpBuild, webpackBundle, MEASURE_RUNS);

let wpBundleSize = 0;
try { wpBundleSize = statSync(webpackBundle).size; } catch {}

// ---- 5. results ----
const speedup = wpStats.avg / wakeStats.avg;
const timeSaved = Math.round(wpStats.avg - wakeStats.avg);
const memSaved = wpStats.avgMem - wakeStats.avgMem;
const memPct = Math.round((1 - wakeStats.avgMem / wpStats.avgMem) * 100);
const BAR = 20;

function bar(val, maxVal) {
  const filled = Math.max(1, Math.round((val / maxVal) * BAR));
  return '█'.repeat(filled) + '░'.repeat(BAR - filled);
}

console.log(`\n${'─'.repeat(56)}`);
console.log('  2k-modules · wake vs webpack 构建对比');
console.log('  真实业务项目风格 (2013 模块, ~2000 文件)');
console.log('  基准: webpack');
console.log(`${'─'.repeat(56)}`);

console.log(`\n  构建时间 (avg, 越小越好)`);
console.log(
  `    wake     ${formatMs(wakeStats.avg).padStart(7)}  ${bar(wakeStats.avg, wpStats.avg)}  ${speedup.toFixed(1)}× 更快`
);
console.log(
  `    webpack  ${formatMs(wpStats.avg).padStart(7)}  ${bar(wpStats.avg, wpStats.avg)}  基准`
);
console.log(`  ⚡ CPU: wake 快 ${formatMs(timeSaved)} (${speedup.toFixed(1)}×)`);

console.log(`\n  内存峰值 (avg, 越小越好)`);
console.log(
  `    wake     ${`${wakeStats.avgMem}MB`.padStart(7)}  ${bar(wakeStats.avgMem, wpStats.avgMem)}  节省 ${memPct}%`
);
console.log(
  `    webpack  ${`${wpStats.avgMem}MB`.padStart(7)}  ${bar(wpStats.avgMem, wpStats.avgMem)}  基准`
);
console.log(`  💾 内存: wake 节约 ${formatBytes(memSaved * 1024 * 1024)} (${memPct}%)`);

console.log(`\n  产物大小  wake ${formatBytes(wakeBundleSize)}  vs  webpack ${formatBytes(wpBundleSize)}`);
console.log(`${'─'.repeat(56)}\n`);
