#!/usr/bin/env node
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import {
  existsSync,
  readFileSync,
  readdirSync,
} from 'node:fs';
import { dirname, join, posix, relative } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const FIXTURE_DIR = fileURLToPath(new URL('.', import.meta.url));
const SOURCE_DIR = join(FIXTURE_DIR, 'input', 'src');
const ENTRY_PATH = join(FIXTURE_DIR, 'input', 'entry.js');
const MANIFEST_PATH = join(FIXTURE_DIR, 'expected', 'project.json');
const CHECKSUM_PATH = join(FIXTURE_DIR, 'expected', 'checksum.txt');
const GENERATOR_PATH = join(FIXTURE_DIR, 'generate.mjs');

const EXPECTED_CATEGORIES = Object.freeze({
  api: 240,
  'api-group': 8,
  'api-index': 1,
  components: 352,
  'components-group': 11,
  'components-index': 1,
  core: 16,
  locales: 240,
  'locales-group': 8,
  'locales-index': 1,
  metrics: 320,
  'metrics-group': 10,
  'metrics-index': 1,
  models: 320,
  'models-group': 10,
  'models-index': 1,
  pages: 80,
  'pages-group': 4,
  'pages-index': 1,
  root: 2,
  rules: 360,
  'rules-group': 12,
  'rules-index': 1,
});

function compareText(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

function fail(message) {
  throw new Error('Northstar fixture verification failed: ' + message);
}

function runGenerator() {
  const result = spawnSync(process.execPath, [GENERATOR_PATH], {
    cwd: FIXTURE_DIR,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
    timeout: 120_000,
    windowsHide: true,
  });
  if (result.error || result.status !== 0) {
    fail('generator exited unsuccessfully\n' + (result.stdout || '') + (result.stderr || '') + (result.error ? result.error.message : ''));
  }
}

function walkJavaScript(directory) {
  if (!existsSync(directory)) return [];
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) files.push(...walkJavaScript(path));
    else if (entry.isFile() && entry.name.endsWith('.js')) files.push(path);
  }
  return files.sort(compareText);
}

function projectPath(path) {
  return relative(SOURCE_DIR, path).replaceAll('\\', '/');
}

function readManifest() {
  return JSON.parse(readFileSync(MANIFEST_PATH, 'utf8'));
}

function parseImports(path, source) {
  const dependencies = [];
  const pattern = /\bimport\s+(?:[^'";]+?\s+from\s+)?['"]([^'"]+)['"]/g;
  for (const match of source.matchAll(pattern)) {
    const specifier = match[1];
    if (!specifier.startsWith('.')) {
      fail(path + ' contains a bare import: ' + specifier);
    }
    dependencies.push(posix.normalize(posix.join(posix.dirname(path), specifier)));
  }
  return dependencies;
}

function collectSnapshot() {
  const manifest = readManifest();
  const files = walkJavaScript(SOURCE_DIR);
  const modules = new Map();
  const hash = createHash('sha256');
  let bytes = 0;
  let lines = 0;
  let staticEdges = 0;

  for (const file of files) {
    const path = projectPath(file);
    const source = readFileSync(file, 'utf8').replaceAll('\r\n', '\n');
    if (source.includes('globalThis.__reg')) fail(path + ' uses the removed global registry');
    if (source.includes('Math.random(')) fail(path + ' uses runtime randomness');
    if (source.includes('Date.now(')) fail(path + ' uses the wall clock');
    const dependencies = parseImports(path, source);
    modules.set(path, dependencies);
    bytes += Buffer.byteLength(source);
    lines += source.split('\n').length - 1;
    staticEdges += dependencies.length;
    hash.update(path);
    hash.update('\0');
    hash.update(source);
    hash.update('\0');
  }

  return {
    manifest,
    files: files.map(projectPath),
    modules,
    bytes,
    lines,
    staticEdges,
    treeHash: hash.digest('hex'),
  };
}

function verifyReachability(snapshot) {
  const visited = new Set();
  const visit = (path) => {
    if (visited.has(path)) return;
    const dependencies = snapshot.modules.get(path);
    if (!dependencies) fail('graph references missing module ' + path);
    visited.add(path);
    for (const dependency of dependencies) visit(dependency);
  };
  visit(snapshot.manifest.graph.entry);
  return visited;
}

function privateCrossDomainEdges(snapshot) {
  let count = 0;
  for (const [path, dependencies] of snapshot.modules) {
    const sourceDomain = path.match(/^domains\/([^/]+)\//)?.[1];
    if (!sourceDomain) continue;
    for (const dependency of dependencies) {
      const targetDomain = dependency.match(/^domains\/([^/]+)\//)?.[1];
      if (targetDomain && targetDomain !== sourceDomain) count += 1;
    }
  }
  return count;
}

function stableSnapshot(snapshot) {
  return {
    manifest: snapshot.manifest,
    files: snapshot.files,
    bytes: snapshot.bytes,
    lines: snapshot.lines,
    staticEdges: snapshot.staticEdges,
    treeHash: snapshot.treeHash,
  };
}

if (!existsSync(SOURCE_DIR) || walkJavaScript(SOURCE_DIR).length !== 2000) {
  runGenerator();
}
const first = collectSnapshot();
runGenerator();
const second = collectSnapshot();
assert.deepStrictEqual(
  stableSnapshot(second),
  stableSnapshot(first),
  'two consecutive generations must produce byte-identical sources and manifests',
);

const project = second.manifest;
assert.equal(project.schemaVersion, 1);
assert.equal(project.project, 'northstar-commerce-console');
assert.equal(project.modules.target, 2000);
assert.equal(project.modules.total, 2000);
assert.deepStrictEqual(project.modules.categories, EXPECTED_CATEGORIES);
assert.equal(second.files.length, 2000);
assert.equal(second.bytes, project.source.bytes);
assert.equal(second.lines, project.source.lines);
assert.equal(second.treeHash, project.source.treeHash);
assert.equal(second.staticEdges, project.graph.staticEdges);
assert.ok(second.staticEdges >= 5000, 'the real project graph must keep at least 5,000 static edges');
assert.equal(project.graph.boundedContexts, 12);
assert.equal(project.graph.bundlerModules, 2001);
assert.equal(privateCrossDomainEdges(second), 0);
assert.equal(project.graph.privateCrossDomainEdges, 0);
assert.equal(project.graph.importAllModules, 0);
assert.equal(project.graph.globalRegistrations, 0);
assert.ok(!second.files.includes('main.js'), 'the removed import-all main.js must not return');

const reachable = verifyReachability(second);
assert.equal(reachable.size, 2000);
assert.equal(project.graph.reachable, 2000);

const entrySource = readFileSync(ENTRY_PATH, 'utf8').replaceAll('\r\n', '\n');
const entryImports = [...entrySource.matchAll(/\bimport\s+(?:[^'";]+?\s+from\s+)?['"]([^'"]+)['"]/g)];
assert.equal(entryImports.length, 1);
assert.equal(entryImports[0][1], './src/application.js');

const applicationUrl = pathToFileURL(join(SOURCE_DIR, 'application.js')).href + '?verify=' + second.treeHash;
const { runApplication } = await import(applicationUrl);
const oracle = runApplication();
assert.deepStrictEqual(oracle, project.oracle);
assert.equal(oracle.moduleCount, 2000);
assert.equal(oracle.records.total, 320);
assert.equal(oracle.records.valid, 320);
assert.equal(oracle.requests.allowed + oracle.requests.denied, 240);
assert.equal(oracle.interface.components, 352);
assert.equal(oracle.localization.packs, 240);
assert.equal(oracle.validation, true);
assert.match(oracle.integrity, /^[0-9a-f]{8}$/);
for (const digest of [
  oracle.requests.digest,
  oracle.requests.catalogDigest,
  oracle.records.digest,
  oracle.policies.digest,
  oracle.analytics.digest,
  oracle.interface.componentDigest,
  oracle.interface.pageDigest,
  oracle.interface.pageCatalogDigest,
  oracle.localization.digest,
  oracle.fulfillment.digest,
]) {
  assert.match(digest, /^[0-9a-f]{8}$/);
}
assert.deepStrictEqual(oracle.fulfillment.workflow, [
  'authenticated',
  'quoted',
  'inventory-reserved',
  'invoiced',
  'fulfilled',
]);
assert.equal(oracle.fulfillment.state, 'fulfilled');
assert.equal(oracle.fulfillment.rejectedTransition, 'INVALID_TRANSITION');
assert.equal(oracle.fulfillment.rejectedReservation, 'INSUFFICIENT_INVENTORY');
assert.ok(oracle.fulfillment.inventoryAfter >= 0);
assert.equal(
  oracle.invoice.totalMinor,
  oracle.invoice.subtotalMinor - oracle.invoice.discountMinor + oracle.invoice.taxMinor,
);

const expectedStdout = 'northstar=' + JSON.stringify(oracle) + '\n';
assert.equal(readFileSync(CHECKSUM_PATH, 'utf8').replaceAll('\r\n', '\n'), expectedStdout);

process.stdout.write(
  'Verified Northstar: 2000/2000 reachable modules, ' +
  second.staticEdges + ' static edges, deterministic tree ' +
  second.treeHash.slice(0, 12) + '\n',
);
