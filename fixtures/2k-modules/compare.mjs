#!/usr/bin/env node
import { execSync } from 'node:child_process';
import { readFileSync, statSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { performance } from 'node:perf_hooks';

const __dirname = fileURLToPath(new URL('.', import.meta.url));
const WARMUP_RUNS = 1;
const MEASURE_RUNS = 5;
const expected = readFileSync(join(__dirname, 'expected', 'checksum.txt'), 'utf8').trim();

function formatMs(ms) { return `${ms.toFixed(0)}ms`; }

function formatBytes(bytes) {
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)}KB`;
  return `${(bytes / (1024 * 1024)).toFixed(2)}MB`;
}

function buildAndVerify(bin, args, bundlePath) {
  try {
    execSync(`"${bin}" ${args}`, { cwd: __dirname, stdio: 'pipe', timeout: 120_000 });
  } catch (e) {
    console.log('  build failed:', e.message);
    console.log(e.stdout?.toString() || '');
    console.log(e.stderr?.toString() || '');
    process.exit(1);
  }
  // verify output
  try {
    const out = execSync(`node "${bundlePath}"`, { cwd: __dirname, stdio: 'pipe', timeout: 30_000 });
    const text = out.toString().trim();
    if (text.includes(expected)) {
      console.log(`  ✓ ${text}`);
    } else {
      console.log(`  ❌ 期望: ${expected}, 实际: ${text}`);
      process.exit(1);
    }
  } catch (e) {
    console.log('  runtime assertion failed:', e.message);
    process.exit(1);
  }
}

// --- wake ---
{
  const wakeBin = join(__dirname, '..', '..', 'target', 'release', 'wake.exe');
  const entry = join(__dirname, 'input', 'entry.js');
  const outdir = join(__dirname, 'dist-wake');
  const bundlePath = join(outdir, 'bundle.js');

  console.log('=== wake ===');
  console.log('  warmup...');
  buildAndVerify(wakeBin, `build "${entry}" --outdir "${outdir}"`, bundlePath);

  const times = [];
  for (let i = 0; i < MEASURE_RUNS; i++) {
    const start = performance.now();
    buildAndVerify(wakeBin, `build "${entry}" --outdir "${outdir}"`, bundlePath);
    times.push(performance.now() - start);
    console.log(`  run ${i + 1}/${MEASURE_RUNS}: ${formatMs(times.at(-1))}`);
  }

  const avg = times.reduce((a, b) => a + b, 0) / times.length;
  const min = Math.min(...times);
  const max = Math.max(...times);
  const bundleSize = existsSync(bundlePath) ? statSync(bundlePath).size : 0;

  console.log(`\n  wake: min=${formatMs(min)} avg=${formatMs(avg)} max=${formatMs(max)}  bundle=${formatBytes(bundleSize)}`);
  console.log('');
}

// --- webpack ---
{
  const configPath = join(__dirname, 'webpack.config.mjs');
  const bundlePath = join(__dirname, 'dist-webpack', 'bundle.js');

  console.log('=== webpack ===');
  console.log('  warmup...');
  buildAndVerify('npx', `webpack --config "${configPath}"`, bundlePath);

  const times = [];
  for (let i = 0; i < MEASURE_RUNS; i++) {
    const start = performance.now();
    buildAndVerify('npx', `webpack --config "${configPath}"`, bundlePath);
    times.push(performance.now() - start);
    console.log(`  run ${i + 1}/${MEASURE_RUNS}: ${formatMs(times.at(-1))}`);
  }

  const avg = times.reduce((a, b) => a + b, 0) / times.length;
  const min = Math.min(...times);
  const max = Math.max(...times);
  const bundleSize = statSync(bundlePath).size;

  console.log(`\n  webpack: min=${formatMs(min)} avg=${formatMs(avg)} max=${formatMs(max)}  bundle=${formatBytes(bundleSize)}`);

  const speedup = avg > 0 ? (avg / (times.length > 0 ? 1 : 1)) : 0;
  console.log(`\n  倍数: wake vs webpack ≈ ${(avg / (times.reduce((a, b) => a + b, 0) / times.length)).toFixed(1)}×`);
}
