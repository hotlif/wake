import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync, readdirSync } from 'node:fs';
import { createRequire } from 'node:module';
import { resolve } from 'node:path';

const result = spawnSync('cargo', [
  'run', '--quiet', '--locked', '--offline', '--manifest-path', '../../Cargo.toml',
  '-p', 'wake_cli', '--', 'build', 'src/index.js', '--outdir', 'dist',
], {
  cwd: import.meta.dirname,
  encoding: 'utf8',
  env: { ...process.env, NO_COLOR: '1' },
  maxBuffer: 16 * 1024 * 1024,
});
if (result.error) throw result.error;
const output = `${result.stdout ?? ''}\n${result.stderr ?? ''}`;
process.stdout.write(output);
assert.equal(result.signal, null, 'Build must exit normally');
assert.equal(result.status, 0, 'Monaco package-local resource must build outside the project root');
const require = createRequire(import.meta.url);
const original = readFileSync(require.resolve('monaco-editor/editor/common/services/editorWebWorkerMain.js'));
const dist = resolve(import.meta.dirname, 'dist');
const files = readdirSync(dist, { recursive: true }).filter(name => /editorWebWorkerMain\.[^/\\]+\.js$/.test(name));
assert.equal(files.length, 1, 'Expected one published Monaco resource');
assert.deepEqual(readFileSync(resolve(dist, files[0])), original);
const scripts = readdirSync(dist, { recursive: true }).filter(name => name.endsWith('.js') && !files.includes(name));
assert.ok(scripts.some(name => readFileSync(resolve(dist, name), 'utf8').includes(files[0].replaceAll('\\', '/'))), 'Bundle must reference the published resource');
for (const name of scripts) {
  assert.doesNotMatch(readFileSync(resolve(dist, name), 'utf8'), /\.zip\/node_modules\/monaco-editor/);
}
console.log('Verified: Monaco builds with a package-local resource outside the project root.');
