#!/usr/bin/env node
// wake vs webpack 构建性能对比。
//
// 计量口径（重要）：**只计构建进程本身的墙钟**——`node bundle.js` 的产物验证跑是一次性正确性
// 检查，不计入构建时间（旧版把它算进去了：每次测量都叠一次 ~135ms 的 node 启动，且这个常数加在
// 两侧会把「快侧」的倍数硬拉向 1，使 wake 的真实优势被系统性低估）。验证在 warmup 各做一次即可。
import { execSync } from 'node:child_process';
import { readFileSync, statSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';

const __dirname = fileURLToPath(new URL('.', import.meta.url));
const MEASURE_RUNS = 5;
const expected = readFileSync(join(__dirname, 'expected', 'checksum.txt'), 'utf8').trim();

function formatMs(ms) { return `${ms.toFixed(0)}ms`; }

function formatBytes(bytes) {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)}MB`;
}

// 只跑构建进程，返回墙钟毫秒（不含验证）。
function timeBuild(bin, args) {
  const start = performance.now();
  try {
    execSync(`"${bin}" ${args}`, { cwd: __dirname, stdio: 'pipe', timeout: 120_000 });
  } catch (e) {
    console.log('  build failed:', e.message);
    console.log(e.stdout?.toString() || '');
    console.log(e.stderr?.toString() || '');
    process.exit(1);
  }
  return performance.now() - start;
}

// 一次性正确性验证：跑产物，断言输出含期望校验和。不计时。
function verify(bundlePath) {
  try {
    const out = execSync(`node "${bundlePath}"`, { cwd: __dirname, stdio: 'pipe', timeout: 30_000 });
    const text = out.toString().trim();
    if (text.includes(expected)) {
      console.log(`  ✓ 产物正确: ${text}`);
    } else {
      console.log(`  ❌ 期望含: ${expected}, 实际: ${text}`);
      process.exit(1);
    }
  } catch (e) {
    console.log('  runtime assertion failed:', e.message);
    process.exit(1);
  }
}

// 跑一个工具：warmup(建+验证一次) → 只对构建计时 MEASURE_RUNS 次。返回 {avg,min,max,size}。
function bench(label, bin, args, bundlePath) {
  console.log(`=== ${label} ===`);
  console.log('  warmup + 正确性验证...');
  timeBuild(bin, args);
  verify(bundlePath);

  const times = [];
  for (let i = 0; i < MEASURE_RUNS; i++) {
    times.push(timeBuild(bin, args));
    console.log(`  run ${i + 1}/${MEASURE_RUNS}: ${formatMs(times.at(-1))}`);
  }
  const avg = times.reduce((a, b) => a + b, 0) / times.length;
  const min = Math.min(...times);
  const max = Math.max(...times);
  const size = existsSync(bundlePath) ? statSync(bundlePath).size : 0;
  console.log(`\n  ${label}: min=${formatMs(min)} avg=${formatMs(avg)} max=${formatMs(max)}  bundle=${formatBytes(size)}\n`);
  return { avg, min, max, size };
}

const wakeBin = join(__dirname, '..', '..', 'target', 'release', 'wake.exe');
const wake = bench(
  'wake',
  wakeBin,
  `build "${join(__dirname, 'input', 'entry.js')}" --outdir "${join(__dirname, 'dist-wake')}"`,
  join(__dirname, 'dist-wake', 'bundle.js'),
);

// 直接跑本地 webpack CLI（`npx` 在 execSync + cwd 下解析不稳）。process.execPath = 当前 node。
const webpack = bench(
  'webpack',
  process.execPath,
  `"${join(__dirname, 'node_modules', 'webpack', 'bin', 'webpack.js')}" --config "${join(__dirname, 'webpack.config.mjs')}"`,
  join(__dirname, 'dist-webpack', 'bundle.js'),
);

// 倍数：webpack 平均 / wake 平均（纯构建墙钟，越大越快）。
const speedup = webpack.avg / wake.avg;
console.log('─'.repeat(48));
console.log(`  纯构建墙钟(不含验证跑):`);
console.log(`    wake    ${formatMs(wake.avg)}   (${formatBytes(wake.size)})`);
console.log(`    webpack ${formatMs(webpack.avg)}   (${formatBytes(webpack.size)})`);
console.log(`  ⚡ wake 比 webpack 快 ${speedup.toFixed(1)}×`);
