#!/usr/bin/env node
/**
 * Deterministic Northstar Commerce Console fixture.
 *
 * The generated project is intentionally dependency-free, but it behaves like a
 * production operations application: generated API clients, domain models,
 * policies, analytics, UI components, locale packs, pages, and an application
 * shell all participate in one executable business scenario.
 */
import { createHash } from 'node:crypto';
import {
  existsSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const FIXTURE_DIR = fileURLToPath(new URL('.', import.meta.url));
const INPUT_DIR = join(FIXTURE_DIR, 'input');
const SOURCE_DIR = join(INPUT_DIR, 'src');
const EXPECTED_DIR = join(FIXTURE_DIR, 'expected');
const TARGET_MODULES = 2000;
const UPDATE_ORACLE = process.argv.includes('--update-oracle');

for (const argument of process.argv.slice(2)) {
  if (argument !== '--update-oracle') throw new Error('unknown generator option: ' + argument);
}

const DOMAINS = [
  'identity',
  'organizations',
  'catalog',
  'pricing',
  'inventory',
  'orders',
  'fulfillment',
  'billing',
  'customers',
  'support',
  'analytics',
  'administration',
];
const LOCALES = ['en-US', 'de-DE', 'fr-FR', 'es-ES', 'ja-JP', 'zh-CN', 'pt-BR', 'nl-NL'];
const MODULES = [];
const MODULE_PATHS = new Set();

function lines(...values) {
  return values.join('\n') + '\n';
}

function pad(value, width = 3) {
  return String(value).padStart(width, '0');
}

function compareText(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

function domainFor(index) {
  return DOMAINS[index % DOMAINS.length];
}

function leafModulePath(directory, prefix, index) {
  return 'domains/' + domainFor(index) + '/' + directory + '/' + prefix + '-' + pad(index + 1) + '.js';
}

function quoted(value) {
  return JSON.stringify(value);
}

function importSpecifier(fromPath, toPath) {
  let value = relative(dirname(fromPath), toPath).replaceAll('\\', '/');
  if (!value.startsWith('.')) value = './' + value;
  return value;
}

function writeModule(path, kind, source, dependencies = []) {
  if (MODULE_PATHS.has(path)) {
    throw new Error('duplicate generated module: ' + path);
  }
  const normalized = source.replaceAll('\r\n', '\n').replace(/\s+$/, '') + '\n';
  mkdirSync(dirname(join(SOURCE_DIR, path)), { recursive: true });
  writeFileSync(join(SOURCE_DIR, path), normalized, 'utf8');
  MODULE_PATHS.add(path);
  MODULES.push({ path, kind, source: normalized, dependencies: [...dependencies] });
}

function buildCore() {
  writeModule('core/config.js', 'core', lines(
    'export const CONFIG = Object.freeze({',
    "  name: 'Northstar Commerce Console',",
    "  version: '2026.8',",
    "  tenantId: 'northstar-eu',",
    "  currency: 'USD',",
    "  locale: 'en-US',",
    "  fixedNow: '2026-08-31T09:30:00.000Z',",
    '  pageSize: 24,',
    '});',
  ));

  writeModule('core/clock.js', 'core', lines(
    'export function createClock(initialIso) {',
    '  let tick = 0;',
    '  const epoch = Date.parse(initialIso);',
    '  return Object.freeze({',
    '    now: () => new Date(epoch + tick++ * 1000).toISOString(),',
    '    peek: () => new Date(epoch + tick * 1000).toISOString(),',
    '  });',
    '}',
  ));

  writeModule('core/ids.js', 'core', lines(
    'export function createIdFactory(prefix) {',
    '  let next = 1;',
    "  return () => prefix + '-' + String(next++).padStart(5, '0');",
    '}',
  ));

  writeModule('core/hash.js', 'core', lines(
    'export function stableHash(value) {',
    '  let hash = 2166136261;',
    '  const text = String(value);',
    '  for (let index = 0; index < text.length; index++) {',
    '    hash ^= text.charCodeAt(index);',
    '    hash = Math.imul(hash, 16777619);',
    '  }',
    "  return (hash >>> 0).toString(16).padStart(8, '0');",
    '}',
  ));

  writeModule('core/money.js', 'core', lines(
    'export function money(minor, currency) {',
    '  if (!Number.isInteger(minor)) throw new TypeError("money minor units must be an integer");',
    '  return Object.freeze({ minor, currency });',
    '}',
    'export function sumMoney(values, currency) {',
    '  return money(values.reduce((total, value) => total + value.minor, 0), currency);',
    '}',
    'export function formatMoney(value) {',
    "  const sign = value.minor < 0 ? '-' : '';",
    '  const absolute = Math.abs(value.minor);',
    "  return sign + value.currency + ' ' + Math.floor(absolute / 100) + '.' + String(absolute % 100).padStart(2, '0');",
    '}',
  ));

  writeModule('core/text.js', 'core', lines(
    'export function normalizeText(value) {',
    "  return String(value == null ? '' : value).trim().replace(/\\s+/g, ' ');",
    '}',
    'export function interpolate(template, values) {',
    "  return template.replace(/\\{(\\w+)\\}/g, (_match, key) => String(values[key] == null ? '' : values[key]));",
    '}',
  ));

  writeModule('core/collections.js', 'core', lines(
    'export function groupBy(values, selectKey) {',
    '  const groups = new Map();',
    '  for (const value of values) {',
    '    const key = selectKey(value);',
    '    if (!groups.has(key)) groups.set(key, []);',
    '    groups.get(key).push(value);',
    '  }',
    '  return groups;',
    '}',
    'export function sumBy(values, selectValue) {',
    '  let total = 0;',
    '  for (const value of values) total += selectValue(value);',
    '  return total;',
    '}',
  ));

  writeModule('core/validation.js', 'core', lines(
    'export function validateFields(value, requiredFields) {',
    '  const missing = requiredFields.filter((field) => value[field] == null || value[field] === "");',
    '  return Object.freeze({ ok: missing.length === 0, missing });',
    '}',
  ));

  writeModule('core/result.js', 'core', lines(
    'export function ok(value) { return Object.freeze({ ok: true, value }); }',
    'export function err(code, detail) { return Object.freeze({ ok: false, error: { code, detail } }); }',
  ));

  writeModule('core/events.js', 'core', lines(
    'export function createEventBus() {',
    '  const listeners = new Map();',
    '  return Object.freeze({',
    '    on(type, listener) {',
    '      if (!listeners.has(type)) listeners.set(type, new Set());',
    '      listeners.get(type).add(listener);',
    '      return () => listeners.get(type).delete(listener);',
    '    },',
    '    emit(type, payload) {',
    '      for (const listener of listeners.get(type) || []) listener(payload);',
    '    },',
    '  });',
    '}',
  ));

  writeModule('core/store.js', 'core', lines(
    "import { createEventBus } from './events.js';",
    'export function createStore(initialState) {',
    '  let state = Object.freeze({ ...initialState });',
    '  let version = 0;',
    '  const events = createEventBus();',
    '  return Object.freeze({',
    '    getState: () => state,',
    '    getVersion: () => version,',
    '    update(patch) {',
    '      state = Object.freeze({ ...state, ...patch });',
    '      version += 1;',
    "      events.emit('change', state);",
    '      return state;',
    '    },',
    "    subscribe(listener) { return events.on('change', listener); },",
    '  });',
    '}',
  ), ['core/events.js']);

  writeModule('core/query.js', 'core', lines(
    'export function paginate(values, page, pageSize) {',
    '  const safePage = Math.max(1, page);',
    '  const start = (safePage - 1) * pageSize;',
    '  return Object.freeze({',
    '    page: safePage,',
    '    pageSize,',
    '    total: values.length,',
    '    items: values.slice(start, start + pageSize),',
    '  });',
    '}',
  ));

  writeModule('core/router.js', 'core', lines(
    'export function createRouter(pages) {',
    '  const routes = new Map(pages.map((page) => [page.route, page]));',
    '  return Object.freeze({',
    '    size: routes.size,',
    '    match(path) { return routes.get(path) || null; },',
    '    paths() { return [...routes.keys()].sort(); },',
    '  });',
    '}',
  ));

  writeModule('core/virtual-dom.js', 'core', lines(
    'function flatten(values, output) {',
    '  for (const value of values) {',
    '    if (Array.isArray(value)) flatten(value, output);',
    '    else if (value !== null && value !== undefined && value !== false) output.push(value);',
    '  }',
    '  return output;',
    '}',
    'export function h(tag, attrs, ...children) {',
    '  return Object.freeze({ tag, attrs: Object.freeze(attrs || {}), children: Object.freeze(flatten(children, [])) });',
    '}',
    'export function countNodes(node) {',
    "  if (node == null || typeof node !== 'object') return 1;",
    '  return 1 + (node.children || []).reduce((total, child) => total + countNodes(child), 0);',
    '}',
    'export function textContent(node) {',
    "  if (node == null) return '';",
    "  if (typeof node !== 'object') return String(node);",
    "  return (node.children || []).map(textContent).join(' ');",
    '}',
  ));

  writeModule('core/permissions.js', 'core', lines(
    'const GRANTS = Object.freeze({',
    "  viewer: Object.freeze(['read']),",
    "  operator: Object.freeze(['read', 'write']),",
    "  administrator: Object.freeze(['read', 'write', 'admin']),",
    '});',
    'export function can(role, permission) {',
    '  return (GRANTS[role] || []).includes(permission);',
    '}',
  ));

  writeModule('core/telemetry.js', 'core', lines(
    "import { createClock } from './clock.js';",
    'export function createTelemetry(initialIso) {',
    '  const clock = createClock(initialIso);',
    '  const events = [];',
    '  return Object.freeze({',
    '    record(type, detail) { events.push(Object.freeze({ sequence: events.length + 1, type, at: clock.now(), detail })); },',
    '    snapshot() { return events.slice(); },',
    '  });',
    '}',
  ), ['core/clock.js']);
}

function apiModule(index, path) {
  const domain = domainFor(index);
  const method = ['GET', 'POST', 'PUT', 'PATCH', 'DELETE'][index % 5];
  const permission = ['read', 'read', 'write', 'write', 'admin'][index % 5];
  const operationId = domain + '-' + method.toLowerCase() + '-' + pad(index + 1);
  const validationPath = importSpecifier(path, 'core/validation.js');
  const textPath = importSpecifier(path, 'core/text.js');
  const required = method === 'GET' || method === 'DELETE' ? ['tenantId'] : ['tenantId', 'payload'];
  return {
    dependencies: ['core/validation.js', 'core/text.js'],
    source: lines(
      'import { validateFields } from ' + quoted(validationPath) + ';',
      'import { normalizeText } from ' + quoted(textPath) + ';',
      'const definition = Object.freeze(' + JSON.stringify({
        id: operationId,
        domain,
        method,
        path: '/v1/' + domain + '/resources/' + pad(index + 1),
        permission,
        timeoutMs: 1000 + (index % 8) * 250,
        cache: method === 'GET' ? 'tenant' : 'none',
        required,
      }) + ');',
      'export function buildRequest(input) {',
      '  const validation = validateFields(input, definition.required);',
      "  if (!validation.ok) throw new Error(definition.id + ': missing ' + validation.missing.join(','));",
      '  return Object.freeze({',
      '    operationId: definition.id,',
      '    method: definition.method,',
      '    url: definition.path + "?tenant=" + encodeURIComponent(normalizeText(input.tenantId)),',
      '    timeoutMs: definition.timeoutMs,',
      '    cache: definition.cache,',
      '    body: input.payload == null ? null : JSON.stringify(input.payload),',
      '  });',
      '}',
      'export default Object.freeze({ ...definition, buildRequest });',
    ),
  };
}

function modelModule(index, path) {
  const domain = domainFor(index);
  const entity = domain.slice(0, 3) + '-entity-' + pad(index + 1);
  const validationPath = importSpecifier(path, 'core/validation.js');
  const textPath = importSpecifier(path, 'core/text.js');
  return {
    dependencies: ['core/validation.js', 'core/text.js'],
    source: lines(
      'import { validateFields } from ' + quoted(validationPath) + ';',
      'import { normalizeText } from ' + quoted(textPath) + ';',
      'const definition = Object.freeze(' + JSON.stringify({
        id: 'model-' + pad(index + 1),
        domain,
        entity,
        fields: ['id', 'tenantId', 'label', 'amountMinor', 'status', 'priority'],
        indexedBy: index % 2 === 0 ? 'status' : 'priority',
      }) + ');',
      'export function createRecord(seed) {',
      '  const record = {',
      "    id: definition.entity + '-' + String(seed).padStart(4, '0'),",
      "    tenantId: 'northstar-eu',",
      "    label: normalizeText(definition.domain + ' record ' + seed),",
      '    amountMinor: 500 + ((seed * ' + (37 + (index % 29)) + ' + ' + index + ') % 250000),',
      "    status: ['active', 'pending', 'review', 'archived'][(seed + " + index + ') % 4],',
      '    priority: 1 + ((seed + ' + index + ') % 5),',
      '  };',
      "  const validation = validateFields(record, ['id', 'tenantId', 'label', 'status']);",
      '  return Object.freeze({ ...record, valid: validation.ok });',
      '}',
      'export default Object.freeze({ ...definition, create: createRecord });',
    ),
  };
}

function ruleModule(index, path) {
  const domain = domainFor(index);
  const resultPath = importSpecifier(path, 'core/result.js');
  const threshold = 25 + (index % 55);
  return {
    dependencies: ['core/result.js'],
    source: lines(
      'import { ok, err } from ' + quoted(resultPath) + ';',
      'const definition = Object.freeze(' + JSON.stringify({
        id: 'policy-' + domain + '-' + pad(index + 1),
        domain,
        threshold,
        severity: ['info', 'warning', 'critical'][index % 3],
      }) + ');',
      'export function evaluate(record, context) {',
      '  const score = (record.amountMinor + record.priority * ' + (index + 11) + ' + context.riskSeed) % 101;',
      '  return score >= definition.threshold',
      '    ? ok(Object.freeze({ policyId: definition.id, score, severity: definition.severity }))',
      "    : err('POLICY_REJECTED', Object.freeze({ policyId: definition.id, score, threshold: definition.threshold }));",
      '}',
      'export default Object.freeze({ ...definition, evaluate });',
    ),
  };
}

function metricModule(index, path) {
  const domain = domainFor(index);
  const collectionsPath = importSpecifier(path, 'core/collections.js');
  const factor = 3 + (index % 31);
  return {
    dependencies: ['core/collections.js'],
    source: lines(
      'import { sumBy } from ' + quoted(collectionsPath) + ';',
      'const definition = Object.freeze(' + JSON.stringify({
        id: 'metric-' + domain + '-' + pad(index + 1),
        domain,
        unit: index % 3 === 0 ? 'minor' : index % 3 === 1 ? 'count' : 'basis-points',
        window: ['1h', '24h', '7d', '30d'][index % 4],
      }) + ');',
      'export function compute(records) {',
      '  const total = sumBy(records, (record) => record.amountMinor + record.priority * ' + factor + ');',
      '  return (total + records.length * ' + (index + 1) + ') % 1000003;',
      '}',
      'export default Object.freeze({ ...definition, compute });',
    ),
  };
}

function componentModule(index, path) {
  const domain = domainFor(index);
  const vdomPath = importSpecifier(path, 'core/virtual-dom.js');
  const moneyPath = importSpecifier(path, 'core/money.js');
  const name = domain + '-panel-' + pad(index + 1);
  return {
    dependencies: ['core/virtual-dom.js', 'core/money.js'],
    source: lines(
      'import { h } from ' + quoted(vdomPath) + ';',
      'import { formatMoney, money } from ' + quoted(moneyPath) + ';',
      'const definition = Object.freeze(' + JSON.stringify({
        id: 'component-' + pad(index + 1),
        domain,
        name,
        density: ['compact', 'comfortable', 'spacious'][index % 3],
        tone: ['neutral', 'positive', 'warning', 'danger'][index % 4],
      }) + ');',
      'export function render(props) {',
      "  const label = props.label || definition.name;",
      '  const value = Number.isInteger(props.amountMinor) ? props.amountMinor : 0;',
      "  return h('article', { className: 'panel panel--' + definition.tone, 'data-component': definition.id },",
      "    h('header', { className: 'panel__header' }, h('h3', {}, label)),",
      "    h('div', { className: 'panel__value', role: 'status' }, formatMoney(money(value, props.currency || 'USD'))),",
      "    h('footer', { className: 'panel__footer' }, definition.domain + ' · ' + definition.density),",
      '  );',
      '}',
      'export default Object.freeze({ ...definition, render });',
    ),
  };
}

function localeModule(index, path) {
  const locale = LOCALES[index % LOCALES.length];
  const domain = domainFor(index);
  const textPath = importSpecifier(path, 'core/text.js');
  const namespace = domain + '-' + pad(Math.floor(index / LOCALES.length) + 1);
  return {
    dependencies: ['core/text.js'],
    source: lines(
      'import { interpolate } from ' + quoted(textPath) + ';',
      'const messages = Object.freeze(' + JSON.stringify({
        title: locale + ' ' + domain + ' workspace',
        empty: 'No ' + domain + ' records are available',
        updated: '{count} ' + domain + ' records updated for {tenant}',
        action: 'Review ' + namespace,
      }) + ');',
      'export function translate(key, values = {}) {',
      "  return interpolate(messages[key] || key, values);",
      '}',
      'const definition = Object.freeze(' + JSON.stringify({
        id: 'locale-' + pad(index + 1),
        locale,
        domain,
        namespace,
      }) + ');',
      'export default Object.freeze({ ...definition, messages, translate });',
    ),
  };
}

function pageModule(index, path) {
  const domain = domainFor(index);
  const domainIndex = index % DOMAINS.length;
  const candidatesForDomain = (total) => Array.from(
    { length: Math.ceil((total - domainIndex) / DOMAINS.length) },
    (_unused, position) => domainIndex + position * DOMAINS.length,
  ).filter((value) => value < total);
  const componentCandidates = candidatesForDomain(352);
  const metricCandidates = candidatesForDomain(320);
  const componentIndexes = [0, 7, 17].map(
    (offset) => componentCandidates[(Math.floor(index / DOMAINS.length) * 3 + offset) % componentCandidates.length],
  );
  const metricIndex = metricCandidates[(Math.floor(index / DOMAINS.length) * 5) % metricCandidates.length];
  const componentPaths = componentIndexes.map((value) => leafModulePath('components', 'component', value));
  const metricPath = leafModulePath('metrics', 'metric', metricIndex);
  const dependencies = [...componentPaths, metricPath];
  const imports = componentPaths.map((componentPath, position) =>
    'import component' + position + ' from ' + quoted(importSpecifier(path, componentPath)) + ';'
  );
  imports.push('import metric from ' + quoted(importSpecifier(path, metricPath)) + ';');
  const route = '/workspaces/' + domain + '/' + pad(index + 1);
  return {
    dependencies,
    source: lines(
      ...imports,
      'const definition = Object.freeze(' + JSON.stringify({
        id: 'page-' + pad(index + 1),
        domain,
        route,
        title: domain[0].toUpperCase() + domain.slice(1) + ' workspace ' + pad(index + 1),
        permission: ['read', 'write', 'admin'][index % 3],
      }) + ');',
      'export function renderPage(context) {',
      '  const metricValue = metric.compute(context.records);',
      '  const props = { label: definition.title, amountMinor: metricValue, currency: context.currency };',
      '  return Object.freeze({',
      "    tag: 'main',",
      "    attrs: Object.freeze({ className: 'workspace', 'data-page': definition.id }),",
      '    children: Object.freeze([',
      '      component0.render(props),',
      '      component1.render({ ...props, label: definition.domain + " activity" }),',
      '      component2.render({ ...props, label: definition.domain + " forecast" }),',
      '    ]),',
      '  });',
      '}',
      'export default Object.freeze({ ...definition, render: renderPage });',
    ),
  };
}

function writeRegistry(spec) {
  const leafPaths = [];
  for (let index = 0; index < spec.count; index++) {
    const path = leafModulePath(spec.directory, spec.prefix, index);
    const generated = spec.build(index, path);
    writeModule(path, spec.kind, generated.source, generated.dependencies);
    leafPaths.push(path);
  }

  if (spec.count % spec.groups !== 0) {
    throw new Error(spec.kind + ' count must be divisible by group count');
  }
  const groupSize = spec.count / spec.groups;
  const groupPaths = [];
  for (let groupIndex = 0; groupIndex < spec.groups; groupIndex++) {
    const path = 'registries/' + spec.directory + '/groups/group-' + pad(groupIndex + 1, 2) + '.js';
    const members = leafPaths.slice(groupIndex * groupSize, (groupIndex + 1) * groupSize);
    const imports = members.map((member, index) =>
      'import member' + index + ' from ' + quoted(importSpecifier(path, member)) + ';'
    );
    writeModule(path, spec.kind + '-group', lines(
      ...imports,
      'export default Object.freeze([' + members.map((_member, index) => 'member' + index).join(', ') + ']);',
    ), members);
    groupPaths.push(path);
  }

  const indexPath = 'registries/' + spec.directory + '/index.js';
  const imports = groupPaths.map((groupPath, index) =>
    'import group' + index + ' from ' + quoted(importSpecifier(indexPath, groupPath)) + ';'
  );
  writeModule(indexPath, spec.kind + '-index', lines(
    ...imports,
    'export default Object.freeze([' + groupPaths.map((_groupPath, index) => '...group' + index).join(', ') + ']);',
  ), groupPaths);
  return indexPath;
}

function buildRegistries() {
  return {
    api: writeRegistry({ kind: 'api', directory: 'api', prefix: 'operation', count: 240, groups: 8, build: apiModule }),
    models: writeRegistry({ kind: 'models', directory: 'models', prefix: 'model', count: 320, groups: 10, build: modelModule }),
    rules: writeRegistry({ kind: 'rules', directory: 'rules', prefix: 'rule', count: 360, groups: 12, build: ruleModule }),
    metrics: writeRegistry({ kind: 'metrics', directory: 'metrics', prefix: 'metric', count: 320, groups: 10, build: metricModule }),
    components: writeRegistry({ kind: 'components', directory: 'components', prefix: 'component', count: 352, groups: 11, build: componentModule }),
    locales: writeRegistry({ kind: 'locales', directory: 'locales', prefix: 'locale', count: 240, groups: 8, build: localeModule }),
    pages: writeRegistry({ kind: 'pages', directory: 'pages', prefix: 'page', count: 80, groups: 4, build: pageModule }),
  };
}

function buildRoots(registries) {
  const registryPaths = Object.values(registries);
  writeModule('project-manifest.js', 'root', lines(
    "import operations from './registries/api/index.js';",
    "import models from './registries/models/index.js';",
    "import rules from './registries/rules/index.js';",
    "import metrics from './registries/metrics/index.js';",
    "import components from './registries/components/index.js';",
    "import localePacks from './registries/locales/index.js';",
    "import pages from './registries/pages/index.js';",
    'export const projectManifest = Object.freeze({',
    "  name: 'northstar-commerce-console',",
    '  modules: 2000,',
    '  counts: Object.freeze({',
    '    core: 16,',
    '    api: operations.length,',
    '    models: models.length,',
    '    rules: rules.length,',
    '    metrics: metrics.length,',
    '    components: components.length,',
    '    locales: localePacks.length,',
    '    pages: pages.length,',
    '    aggregates: 70,',
    '    roots: 2,',
    '  }),',
    '});',
  ), registryPaths);

  const coreDependencies = [
    'core/config.js',
    'core/clock.js',
    'core/ids.js',
    'core/hash.js',
    'core/money.js',
    'core/text.js',
    'core/collections.js',
    'core/validation.js',
    'core/result.js',
    'core/events.js',
    'core/store.js',
    'core/query.js',
    'core/router.js',
    'core/virtual-dom.js',
    'core/permissions.js',
    'core/telemetry.js',
  ];
  writeModule('application.js', 'root', lines(
    "import { CONFIG } from './core/config.js';",
    "import { createClock } from './core/clock.js';",
    "import { createIdFactory } from './core/ids.js';",
    "import { stableHash } from './core/hash.js';",
    "import { money, sumMoney } from './core/money.js';",
    "import { normalizeText } from './core/text.js';",
    "import { groupBy, sumBy } from './core/collections.js';",
    "import { validateFields } from './core/validation.js';",
    "import { ok, err } from './core/result.js';",
    "import { createEventBus } from './core/events.js';",
    "import { createStore } from './core/store.js';",
    "import { paginate } from './core/query.js';",
    "import { createRouter } from './core/router.js';",
    "import { h, countNodes, textContent } from './core/virtual-dom.js';",
    "import { can } from './core/permissions.js';",
    "import { createTelemetry } from './core/telemetry.js';",
    "import operations from './registries/api/index.js';",
    "import models from './registries/models/index.js';",
    "import rules from './registries/rules/index.js';",
    "import metrics from './registries/metrics/index.js';",
    "import components from './registries/components/index.js';",
    "import localePacks from './registries/locales/index.js';",
    "import pages from './registries/pages/index.js';",
    "import { projectManifest } from './project-manifest.js';",
    'export function runApplication() {',
    "  const role = 'operator';",
    '  const clock = createClock(CONFIG.fixedNow);',
    "  const nextInvoiceId = createIdFactory('invoice');",
    '  const eventBus = createEventBus();',
    '  const telemetry = createTelemetry(CONFIG.fixedNow);',
    '  const store = createStore({ processed: 0, alerts: 0 });',
    '  const observedChanges = [];',
    '  const workflow = [];',
    '  const allowedTransitions = Object.freeze({',
    '    idle: Object.freeze(["authenticated"]),',
    '    authenticated: Object.freeze(["quoted"]),',
    '    quoted: Object.freeze(["inventory-reserved"]),',
    '    "inventory-reserved": Object.freeze(["invoiced"]),',
    '    invoiced: Object.freeze(["fulfilled"]),',
    '    fulfilled: Object.freeze([]),',
    '  });',
    '  let workflowState = "idle";',
    '  const transition = (state, detail) => {',
    '    if (!(allowedTransitions[workflowState] || []).includes(state)) {',
    '      return err("INVALID_TRANSITION", Object.freeze({ from: workflowState, to: state }));',
    '    }',
    '    const event = Object.freeze({ state, detail, at: clock.now() });',
    '    workflow.push(event);',
    '    workflowState = state;',
    '    telemetry.record("workflow." + state, detail);',
    '    return ok(event);',
    '  };',
    '  const rejectedTransition = transition("fulfilled", "premature");',
    '  store.subscribe((state) => observedChanges.push(state.processed));',
    "  eventBus.on('invoice.created', (invoice) => telemetry.record('invoice.created', invoice.id));",
    '  transition("authenticated", role);',
    "  const requestInput = { tenantId: CONFIG.tenantId, payload: { source: 'benchmark', requestedAt: clock.now() } };",
    '  const allowedOperations = operations.filter((operation) => can(role, operation.permission));',
    '  const requests = allowedOperations.map((operation) => operation.buildRequest(requestInput));',
    '  const operationCatalog = operations.map(({ buildRequest: _buildRequest, ...definition }) => definition);',
    '  const records = models.map((model, index) => model.create(index + 1));',
    '  const validation = validateFields(records[0], ["id", "tenantId", "label", "status"]);',
    '  const decisions = rules.map((rule, index) => rule.evaluate(records[index % records.length], { riskSeed: 17 }));',
    '  const metricInput = paginate(records, 1, 64).items;',
    '  const metricValues = metrics.map((metric) => metric.compute(metricInput));',
    '  const componentTrees = components.map((component, index) => component.render({',
    '    label: component.name,',
    '    amountMinor: records[index % records.length].amountMinor,',
    '    currency: CONFIG.currency,',
    '  }));',
    '  const translations = localePacks.map((pack) => pack.translate("updated", { count: records.length, tenant: CONFIG.tenantId }));',
    '  const allowedPages = pages.filter((page) => can(role, page.permission));',
    '  const pageCatalog = pages.map(({ render: _render, ...definition }) => definition);',
    '  const router = createRouter(allowedPages);',
    '  const pageTrees = allowedPages.map((page) => page.render({ records: metricInput, currency: CONFIG.currency }));',
    '  const shell = h("div", { id: "northstar-app" }, componentTrees.slice(0, 12), pageTrees.slice(0, 8));',
    '  const subtotal = sumMoney(records.slice(0, 12).map((record) => money(record.amountMinor, CONFIG.currency)), CONFIG.currency);',
    '  const discountMinor = decisions.filter((decision) => decision.ok).length >= 160 ? Math.floor(subtotal.minor * 500 / 10000) : 0;',
    '  const taxableMinor = subtotal.minor - discountMinor;',
    '  const tax = Math.floor(taxableMinor * 825 / 10000);',
    '  const reservedUnits = sumBy(records.slice(0, 24), (record) => record.priority);',
    '  const inventoryBefore = 5000;',
    '  const reserveInventory = (available, requested) => requested <= available',
    '    ? ok(Object.freeze({ before: available, reserved: requested, after: available - requested }))',
    '    : err("INSUFFICIENT_INVENTORY", Object.freeze({ available, requested }));',
    '  const rejectedReservation = reserveInventory(10, reservedUnits);',
    '  const reservation = reserveInventory(inventoryBefore, reservedUnits);',
    '  transition("quoted", taxableMinor);',
    '  transition("inventory-reserved", reservation.value.reserved);',
    '  const invoice = Object.freeze({',
    '    id: nextInvoiceId(),',
    '    subtotalMinor: subtotal.minor,',
    '    discountMinor,',
    '    taxMinor: tax,',
    '    totalMinor: taxableMinor + tax,',
    "    status: 'issued',",
    '    issuedAt: clock.now(),',
    '  });',
    "  eventBus.emit('invoice.created', invoice);",
    '  transition("invoiced", invoice.id);',
    '  transition("fulfilled", reservation.value.after);',
    '  store.update({ processed: records.length });',
    '  store.update({ alerts: decisions.filter((decision) => !decision.ok).length });',
    '  const domainCounts = Object.fromEntries(',
    '    [...groupBy(records, (record) => record.id.split("-")[0]).entries()]',
    '      .sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)',
    '      .map(([key, values]) => [key, values.length]),',
    '  );',
    '  const matched = router.match(allowedPages[17].route);',
    '  const normalizedProjectName = normalizeText(CONFIG.name);',
    '  const resultProbe = decisions[0].ok ? ok("accepted") : err("REJECTED", "probe");',
    '  const summary = {',
    '    project: projectManifest.name,',
    '    version: CONFIG.version,',
    '    moduleCount: projectManifest.modules,',
    '    counts: projectManifest.counts,',
    '    tenant: CONFIG.tenantId,',
    '    role,',
    '    requests: Object.freeze({ allowed: requests.length, denied: operations.length - requests.length, first: requests[0].operationId, digest: stableHash(JSON.stringify(requests)), catalogDigest: stableHash(JSON.stringify(operationCatalog)) }),',
    '    records: Object.freeze({ total: records.length, valid: records.filter((record) => record.valid).length, domains: domainCounts, digest: stableHash(JSON.stringify(records)) }),',
    '    policies: Object.freeze({ passed: decisions.filter((decision) => decision.ok).length, failed: decisions.filter((decision) => !decision.ok).length, probe: resultProbe.ok, digest: stableHash(JSON.stringify(decisions)) }),',
    '    analytics: Object.freeze({ metrics: metricValues.length, aggregate: sumBy(metricValues, (value) => value) % 1000000007, digest: stableHash(JSON.stringify(metricValues)) }),',
    '    interface: Object.freeze({ components: componentTrees.length, pages: allowedPages.length, routes: router.size, matched: matched.id, nodes: countNodes(shell), textHash: stableHash(textContent(shell)), componentDigest: stableHash(JSON.stringify(componentTrees)), pageDigest: stableHash(JSON.stringify(pageTrees)), pageCatalogDigest: stableHash(JSON.stringify(pageCatalog)) }),',
    '    localization: Object.freeze({ packs: localePacks.length, characters: sumBy(translations, (value) => value.length), digest: stableHash(JSON.stringify(translations)) }),',
    '    invoice,',
    '    fulfillment: Object.freeze({ reservedUnits, inventoryBefore, inventoryAfter: reservation.value.after, state: workflowState, workflow: workflow.map((event) => event.state), rejectedTransition: rejectedTransition.error.code, rejectedReservation: rejectedReservation.error.code, digest: stableHash(JSON.stringify(workflow)) }),',
    '    state: Object.freeze({ version: store.getVersion(), processed: store.getState().processed, alerts: store.getState().alerts, observedChanges }),',
    '    telemetry: Object.freeze({ events: telemetry.snapshot().length, first: telemetry.snapshot()[0].type }),',
    '    labelHash: stableHash(normalizedProjectName),',
    '    validation: validation.ok,',
    '  };',
    '  return Object.freeze({ ...summary, integrity: stableHash(JSON.stringify(summary)) });',
    '}',
  ), [...coreDependencies, ...registryPaths, 'project-manifest.js']);
}

function treeHash() {
  const hash = createHash('sha256');
  for (const module of [...MODULES].sort((left, right) => compareText(left.path, right.path))) {
    hash.update(module.path);
    hash.update('\0');
    hash.update(module.source);
    hash.update('\0');
  }
  return hash.digest('hex');
}

function reachableModules(entryPath) {
  const byPath = new Map(MODULES.map((module) => [module.path, module]));
  const visited = new Set();
  const visit = (path) => {
    if (visited.has(path)) return;
    const module = byPath.get(path);
    if (!module) throw new Error('missing dependency in generated graph: ' + path);
    visited.add(path);
    for (const dependency of module.dependencies) visit(dependency);
  };
  visit(entryPath);
  return visited;
}

async function generate() {
  rmSync(SOURCE_DIR, { recursive: true, force: true });
  mkdirSync(SOURCE_DIR, { recursive: true });
  mkdirSync(EXPECTED_DIR, { recursive: true });

  buildCore();
  const registries = buildRegistries();
  buildRoots(registries);

  if (MODULES.length !== TARGET_MODULES) {
    throw new Error('expected ' + TARGET_MODULES + ' generated modules, received ' + MODULES.length);
  }
  const reachable = reachableModules('application.js');
  if (reachable.size !== TARGET_MODULES) {
    const unreachable = MODULES.filter((module) => !reachable.has(module.path)).map((module) => module.path);
    throw new Error('generated graph has unreachable modules: ' + unreachable.slice(0, 10).join(', '));
  }

  const entrySource = lines(
    "import { runApplication } from './src/application.js';",
    "console.log('northstar=' + JSON.stringify(runApplication()));",
  );
  writeFileSync(join(INPUT_DIR, 'entry.js'), entrySource, 'utf8');
  writeFileSync(join(INPUT_DIR, 'package.json'), JSON.stringify({
    name: 'northstar-commerce-console-fixture',
    private: true,
    type: 'module',
    sideEffects: false,
  }, null, 2) + '\n', 'utf8');

  const digest = treeHash();
  const applicationUrl = pathToFileURL(join(SOURCE_DIR, 'application.js')).href + '?tree=' + digest;
  const { runApplication } = await import(applicationUrl);
  const oracle = runApplication();
  const categoryCounts = Object.fromEntries(
    [...new Set(MODULES.map((module) => module.kind))]
      .sort(compareText)
      .map((kind) => [kind, MODULES.filter((module) => module.kind === kind).length]),
  );
  const project = {
    schemaVersion: 1,
    project: 'northstar-commerce-console',
    modules: {
      target: TARGET_MODULES,
      total: MODULES.length,
      categories: categoryCounts,
    },
    graph: {
      entry: 'application.js',
      reachable: reachable.size,
      bundlerModules: TARGET_MODULES + 1,
      staticEdges: MODULES.reduce((total, module) => total + module.dependencies.length, 0),
      boundedContexts: DOMAINS.length,
      privateCrossDomainEdges: 0,
      importAllModules: 0,
      globalRegistrations: 0,
    },
    source: {
      files: MODULES.length,
      bytes: MODULES.reduce((total, module) => total + Buffer.byteLength(module.source), 0),
      lines: MODULES.reduce((total, module) => total + module.source.split('\n').length - 1, 0),
      treeHash: digest,
    },
    oracle,
  };
  const projectText = JSON.stringify(project, null, 2) + '\n';
  const checksumText = 'northstar=' + JSON.stringify(oracle) + '\n';
  const projectPath = join(EXPECTED_DIR, 'project.json');
  const checksumPath = join(EXPECTED_DIR, 'checksum.txt');
  if (UPDATE_ORACLE) {
    writeFileSync(projectPath, projectText, 'utf8');
    writeFileSync(checksumPath, checksumText, 'utf8');
  } else {
    if (!existsSync(projectPath) || !existsSync(checksumPath)) {
      throw new Error('committed oracle is missing; run node generate.mjs --update-oracle');
    }
    if (readFileSync(projectPath, 'utf8').replaceAll('\r\n', '\n') !== projectText
      || readFileSync(checksumPath, 'utf8').replaceAll('\r\n', '\n') !== checksumText) {
      throw new Error('generated project differs from the committed oracle; review the change, then run node generate.mjs --update-oracle');
    }
  }

  process.stdout.write(
    'Generated ' + MODULES.length + ' reachable modules, ' +
    project.graph.staticEdges + ' static edges, ' +
    project.source.bytes + ' bytes\n',
  );
  process.stdout.write('Tree SHA-256: ' + digest + '\n');
  process.stdout.write(UPDATE_ORACLE ? 'Updated committed oracle\n' : 'Committed oracle matched\n');
}

await generate();
