#!/usr/bin/env node
/**
 * 2k-modules 生成器 — 真实业务项目风格
 *
 * 模拟一个中大型 Web 应用，包含：
 * - 框架运行时（类 React + hooks + store + router + api）
 * - 5 个业务 Feature（auth/dashboard/settings/admin/notifications）
 * - UI 组件库、Layout、表单
 * - Barrel 文件链、动态 import
 * - 模块大小差异（小 5-20 行 / 中 30-80 行 / 大 100-500 行 / 特大 600-2000 行）
 */
import fs from 'node:fs';
const { writeFileSync, mkdirSync, rmSync, readdirSync, statSync, readFileSync } = fs;
import { join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = fileURLToPath(new URL('.', import.meta.url));
const SRC = join(__dirname, 'input', 'src');
const INPUT = join(__dirname, 'input');
let FILE_ID = 0;

// ====== seedable PRNG ======
function createRng(seed) {
  let s = seed | 0;
  return () => {
    s = (s + 0x6d2b79f5) | 0;
    let t = Math.imul(s ^ (s >>> 15), 1 | s);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}
const rng = createRng(42);

function randInt(min, max) {
  return min + Math.floor(rng() * (max - min + 1));
}

function shuffle(arr) {
  const a = [...arr];
  for (let i = a.length - 1; i > 0; i--) {
    const j = Math.floor(rng() * (i + 1));
    [a[i], a[j]] = [a[j], a[i]];
  }
  return a;
}

function pick(arr, n) {
  if (n >= arr.length) return [...arr];
  return shuffle(arr).slice(0, n);
}

// ====== Module metadata ======
const ALL_MODULES = [];
const MOD_BY_CAT = {};
let MOD_ID = 0;

function defineModule(cat, name, size, dir, imports = [], content = null) {
  const id = MOD_ID++;
  const filePath = join(SRC, dir, name + '.js');
  const rel = () => {
    const r = relative(SRC, filePath).replace(/\\/g, '/');
    return r.startsWith('.') ? r : './' + r;
  };
  const m = { id, cat, name, size, dir, imports, content, filePath, rel };
  ALL_MODULES.push(m);
  (MOD_BY_CAT[cat] || (MOD_BY_CAT[cat] = [])).push(m);
  return m;
}

// ====== File writer ======
function write(mod, code) {
  mkdirSync(join(SRC, mod.dir), { recursive: true });
  mod.generated = code;
  writeFileSync(mod.filePath, code, 'utf8');
}

// ====== Import resolver ======
function imp(mod, ...names) {
  const fromDir = mod.dir;
  let fromPath = mod.filePath;
  const toDir = mod.dir.split('/').slice(0, -1).join('/') || '';
  const relPath = relative(join(SRC, fromDir), mod.filePath).replace(/\\/g, '');
  // This is called during generation; we store the import targets
  return names;
}

function resolveImport(fromMod, toMod) {
  const fromDir = fromMod.dir;
  const toPath = toMod.filePath;
  const rel = relative(join(SRC, fromDir), toPath).replace(/\\/g, '');
  return rel.startsWith('.') ? rel : './' + rel;
}

// ====== FRAMEWORK RUNTIME (hand-coded) ======
function buildRuntime() {
  // ---- shared/runtime.js ----
  const rtCode = [
    'globalThis.__reg || (globalThis.__reg = {});',
    '',
    'export function createElement(tag, attrs, ...children) {',
    '  return { tag, attrs: attrs || null, children: children.flat(Infinity).filter(Boolean) };',
    '}',
    '',
    'export function Fragment({ children }) { return children; }',
    '',
    'export function createRef(val) { return { current: val ?? null }; }',
    '',
    'export function cloneElement(el, overrides) { return { ...el, attrs: { ...el.attrs, ...overrides } }; }',
    '',
    'export function isValidElement(obj) { return obj && typeof obj === "object" && "tag" in obj; }',
    '',
    'export function toChildArray(children) { return Array.isArray(children) ? children : [children]; }',
    '',
    "globalThis.__reg['runtime'] = 1;",
    '',
  ].join('\n');
  write(defineModule('runtime', 'runtime', 'small', 'shared'), rtCode);

  // ---- shared/hooks.js ----
  const hooksCode = [
    'import { createElement } from "./runtime.js";',
    'globalThis.__reg || (globalThis.__reg = {});',
    '',
    'export function useState(initial) {',
    '  let state = typeof initial === "function" ? initial() : initial;',
    '  const get = () => state;',
    '  const set = (next) => { state = typeof next === "function" ? next(state) : next; };',
    '  return [get, set];',
    '}',
    '',
    'export function useEffect(fn, deps) {',
    '  let cleanup = null; let prevDeps = null;',
    '  const run = () => { if (cleanup) cleanup(); cleanup = fn() || null; };',
    '  const check = (nextDeps) => {',
    '    if (!prevDeps || !nextDeps || prevDeps.some((d, i) => d !== nextDeps[i])) {',
    '      run(); prevDeps = nextDeps ? [...nextDeps] : null;',
    '    }',
    '  };',
    '  return { run, check };',
    '}',
    '',
    'export function useMemo(fn, deps) {',
    '  let val; let computed = false; let prevDeps = null;',
    '  return () => {',
    '    if (!computed || !deps || !prevDeps || deps.some((d, i) => d !== prevDeps[i])) {',
    '      val = fn(); computed = true; prevDeps = deps ? [...deps] : null;',
    '    }',
    '    return val;',
    '  };',
    '}',
    '',
    'export function useCallback(fn, deps) { return useMemo(() => fn, deps); }',
    'export function useRef(initial) { return { current: initial ?? null }; }',
    '',
    "export function useReducer(reducer, initial) {",
    '  const [get, set] = useState(initial);',
    '  const dispatch = (action) => set(reducer(get(), action));',
    '  return [get, dispatch];',
    '}',
    '',
    "globalThis.__reg['hooks'] = 2;",
    '',
  ].join('\n');
  write(defineModule('runtime', 'hooks', 'large', 'shared'), hooksCode);

  // ---- shared/store.js ----
  const storeCode = [
    'globalThis.__reg || (globalThis.__reg = {});',
    '',
    'export function createStore(name, initial = {}) {',
    '  let state = { ...initial }; const listeners = new Set();',
    '  return { name, getState: () => state,',
    '    setState: (partial) => { state = { ...state, ...(typeof partial === "function" ? partial(state) : partial) }; listeners.forEach(fn => fn(state)); },',
    '    subscribe: (fn) => { listeners.add(fn); return () => listeners.delete(fn); },',
    '    destroy: () => { listeners.clear(); },',
    '  };',
    '}',
    '',
    'export function combineReducers(reducerMap) {',
    '  return (state = {}, action) => {',
    '    const next = {}; let changed = false;',
    '    for (const key of Object.keys(reducerMap)) {',
    '      next[key] = reducerMap[key](state[key], action);',
    '      if (next[key] !== state[key]) changed = true;',
    '    }',
    '    return changed ? next : state;',
    '  };',
    '}',
    '',
    'export function createSelector(store, selectFn) {',
    '  let lastState = store.getState(); let lastResult = selectFn(lastState);',
    '  store.subscribe((s) => { const next = selectFn(s); if (next !== lastResult) { lastResult = next; lastState = s; } });',
    '  return () => lastResult;',
    '}',
    '',
    "globalThis.__reg['store'] = 3;",
    '',
  ].join('\n');
  write(defineModule('runtime', 'store', 'medium', 'shared'), storeCode);

  // ---- shared/api.js ----
  const apiCode = [
    'globalThis.__reg || (globalThis.__reg = {});',
    'let baseUrl = "/api";',
    '',
    'export function configureApi(url, opts = {}) { baseUrl = url || baseUrl; return { baseUrl, ...opts }; }',
    '',
    'export async function request(method, path, body = null, headers = {}) {',
    '  const url = baseUrl + path;',
    '  const req = { url, method, headers: { "Content-Type": "application/json", ...headers } };',
    '  if (body) req.body = JSON.stringify(body);',
    '  return { ok: true, status: 200, data: null, timestamp: Date.now(), request: req };',
    '}',
    '',
    'export function get(path, params = {}) {',
    '  const qs = Object.entries(params).filter(([, v]) => v != null).map(([k, v]) => encodeURIComponent(k) + "=" + encodeURIComponent(v)).join("&");',
    '  return request("GET", qs ? path + "?" + qs : path);',
    '}',
    'export function post(path, data) { return request("POST", path, data); }',
    'export function put(path, data) { return request("PUT", path, data); }',
    'export function del(path) { return request("DELETE", path); }',
    '',
    'export function retry(fn, maxRetries = 3, delay = 300) {',
    '  return async (...args) => {',
    '    let lastErr;',
    '    for (let i = 0; i < maxRetries; i++) {',
    '      try { return await fn(...args); }',
    '      catch (e) { lastErr = e; if (i < maxRetries - 1) await new Promise(r => setTimeout(r, delay * (i + 1))); }',
    '    }',
    '    throw lastErr;',
    '  };',
    '}',
    '',
    "globalThis.__reg['api'] = 4;",
    '',
  ].join('\n');
  write(defineModule('runtime', 'api', 'medium', 'shared'), apiCode);

  // ---- shared/router.js ----
  const routerCode = [
    'import { createElement } from "./runtime.js";',
    'globalThis.__reg || (globalThis.__reg = {});',
    '',
    'const ROUTES = []; let currentPath = "/";',
    '',
    'export function createRouter(routes) { ROUTES.push(...routes); return { routes: ROUTES, navigate }; }',
    'export function navigate(path) { currentPath = path; return path; }',
    '',
    'export function matchRoute(path) {',
    '  for (const route of ROUTES) {',
    '    if (route.path === path) return route;',
    '    const match = path.match(new RegExp("^" + route.path.replace(/:[^/]+/g, "([^/]+)") + "$"));',
    '    if (match) return { ...route, params: match.slice(1) };',
    '  }',
    '  return null;',
    '}',
    '',
    'export function useRouter() { return { path: currentPath, match: matchRoute(currentPath), navigate }; }',
    '',
    'export function Link({ to, children, ...rest }) { return createElement("a", { href: to, ...rest }, children); }',
    '',
    "globalThis.__reg['router'] = 5;",
    '',
  ].join('\n');
  write(defineModule('runtime', 'router', 'medium', 'shared'), routerCode);

  // ---- shared/context.js ----
  const ctxCode = [
    'globalThis.__reg || (globalThis.__reg = {});',
    '',
    'export function createContext(defaultValue) {',
    '  let value = defaultValue; const listeners = new Set();',
    '  return { Provider: { value }, set: (v) => { value = v; listeners.forEach(fn => fn(v)); }, get: () => value,',
    '    subscribe: (fn) => { listeners.add(fn); return () => listeners.delete(fn); },',
    '  };',
    '}',
    '',
    'export function useContext(ctx) { return ctx.get(); }',
    '',
    "globalThis.__reg['context'] = 6;",
    '',
  ].join('\n');
  write(defineModule('runtime', 'context', 'small', 'shared'), ctxCode);
}

// ====== CONSTANTS ======
function buildConstants() {
  const dir = 'constants';
  const names = ['theme', 'api', 'routes', 'app', 'env', 'errors', 'config', 'permissions', 'features', 'limits',
    'colors', 'fonts', 'sizes', 'breakpoints', 'zindex', 'icons', 'labels', 'status', 'roles', 'events',
    'durations', 'weights', 'shadows', 'opacity', 'animations', 'transitions', 'cursors', 'outlines', 'filters', 'backdrops'];

  for (let i = 0; i < names.length; i++) {
    const name = names[i];
    const isLarge = name === 'colors' || name === 'icons';
    const size = isLarge ? 'large' : 'small';
    const s = randInt(5, 20);
    let code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;

    if (name === 'theme') {
      code += `export const COLORS = { primary: '#1890ff', success: '#52c41a', warning: '#faad14', error: '#ff4d4f', info: '#1890ff', text: '#333', bg: '#f5f5f5', white: '#fff', border: '#e8e8e8' };\n`;
      code += `export const FONTS = { sans: '-apple-system, BlinkMacSystemFont, sans-serif', mono: 'SFMono-Regular, monospace', size: 14 };\n`;
      code += `export const SPACING = { xs: 4, sm: 8, md: 16, lg: 24, xl: 32, xxl: 48 };\n`;
      code += `export const BREAKPOINTS = { xs: 480, sm: 576, md: 768, lg: 992, xl: 1200, xxl: 1600 };\n`;
      code += `export const RADIUS = { sm: 2, md: 4, lg: 8, xl: 16, round: 9999 };\n`;
    } else if (name === 'api') {
      code += `export const ENDPOINTS = {\n  auth: { login: '/auth/login', register: '/auth/register', logout: '/auth/logout', refresh: '/auth/refresh', me: '/auth/me' },\n  users: { list: '/users', detail: (id) => \`/users/\${id}\`, create: '/users', update: (id) => \`/users/\${id}\`, delete: (id) => \`/users/\${id}\` },\n  dashboard: { stats: '/dashboard/stats', activity: '/dashboard/activity', chart: '/dashboard/chart' },\n  settings: { get: '/settings', update: '/settings' },\n  admin: { health: '/admin/health', audit: '/admin/audit', config: '/admin/config' },\n  notifications: { list: '/notifications', read: (id) => \`/notifications/\${id}/read\`, clear: '/notifications/clear' },\n};\n`;
      code += `export const STATUS_OK = 200;\nexport const STATUS_CREATED = 201;\nexport const STATUS_NO_CONTENT = 204;\nexport const STATUS_BAD_REQUEST = 400;\nexport const STATUS_UNAUTHORIZED = 401;\nexport const STATUS_FORBIDDEN = 403;\nexport const STATUS_NOT_FOUND = 404;\nexport const STATUS_SERVER_ERROR = 500;\n`;
    } else if (name === 'routes') {
      code += `export const HOME = '/';\nexport const LOGIN = '/login';\nexport const REGISTER = '/register';\nexport const DASHBOARD = '/dashboard';\nexport const SETTINGS = '/settings';\nexport const ADMIN = '/admin';\nexport const NOTIFICATIONS = '/notifications';\nexport const PROFILE = '/profile';\nexport const FORGOT_PASSWORD = '/forgot-password';\nexport const path = { dashboard: { stats: '/dashboard/stats', reports: '/dashboard/reports' }, admin: { users: '/admin/users', roles: '/admin/roles', audit: '/admin/audit' } };\n`;
    } else if (name === 'app') {
      code += `export const APP_NAME = 'EnterpriseApp';\nexport const APP_VERSION = '2.${randInt(0, 9)}.${randInt(0, 99)}';\nexport const BUILD_TIME = Date.now();\nexport const DEFAULT_LOCALE = 'zh-CN';\nexport const SUPPORTED_LOCALES = ['zh-CN', 'en-US', 'ja-JP'];\nexport const PAGE_SIZE = 20;\nexport const UPLOAD_LIMIT = 10 * 1024 * 1024;\n`;
    } else if (name === 'env') {
      code += `export const isDev = ${Math.random() > 0.8 ? 'true' : 'false'};\nexport const isProd = ${Math.random() > 0.8 ? 'true' : 'false'};\nexport const API_BASE = isDev ? 'http://localhost:3000/api' : 'https://api.example.com';\nexport const WS_URL = isDev ? 'ws://localhost:3000' : 'wss://api.example.com';\nexport const SENTRY_DSN = isProd ? 'https://xxx@sentry.io/1' : '';\n`;
    } else if (name === 'errors') {
      code += `export const ERROR_CODES = { NETWORK: 'ERR_NETWORK', TIMEOUT: 'ERR_TIMEOUT', UNAUTHORIZED: 'ERR_UNAUTHORIZED', FORBIDDEN: 'ERR_FORBIDDEN', NOT_FOUND: 'ERR_NOT_FOUND', VALIDATION: 'ERR_VALIDATION', SERVER: 'ERR_SERVER', UNKNOWN: 'ERR_UNKNOWN' };\n`;
      code += `export const ERROR_MESSAGES = { [ERROR_CODES.NETWORK]: '网络错误', [ERROR_CODES.TIMEOUT]: '请求超时', [ERROR_CODES.UNAUTHORIZED]: '未授权', [ERROR_CODES.FORBIDDEN]: '无权限', [ERROR_CODES.NOT_FOUND]: '资源不存在', [ERROR_CODES.VALIDATION]: '数据验证失败', [ERROR_CODES.SERVER]: '服务器错误', [ERROR_CODES.UNKNOWN]: '未知错误' };\n`;
    } else if (isLarge) {
      const items = randInt(20, 50);
      code += `export const ${name.toUpperCase()} = {\n`;
      for (let j = 0; j < items; j++) {
        if (name === 'colors') code += `  c${j}: '#${Math.floor(Math.random() * 0x1000000).toString(16).padStart(6, '0')}',\n`;
        else if (name === 'icons') code += `  icon_${j}: 'icon-${name}-${j}',\n`;
        else code += `  ${name}_${j}: ${JSON.stringify(name + '_' + j)},\n`;
      }
      code += '};\n';
    } else {
      code += `export const ${name.toUpperCase()} = ${JSON.stringify(name + '_config')};\n`;
      for (let j = 0; j < s; j++) {
        code += `export const ${name}_opt_${j} = ${randInt(0, 100)};\n`;
      }
    }
    code += `\nglobalThis.__reg['constants_${name}'] = ${20 + i};\n`;
    write(defineModule('constants', `constants_${name}`, size, dir), code);
  }

  // No barrel file for constants — conflicting star exports (webpack compat)
}

// ====== UTILITIES ======
function buildUtils() {
  const dir = 'utils';
  // Define a rich set of utility functions
  const modules = [
    { name: 'string', functions: [
      `export function capitalize(str) { if (!str) return ''; return str[0].toUpperCase() + str.slice(1); }`,
      `export function truncate(str, len = 50) { if (!str) return ''; return str.length <= len ? str : str.slice(0, len) + '...'; }`,
      `export function camelCase(str) { return str.replace(/[-_\\s]+(.)/g, (_, c) => c.toUpperCase()).replace(/^[A-Z]/, c => c.toLowerCase()); }`,
      `export function kebabCase(str) { return str.replace(/([A-Z])/g, '-$1').toLowerCase().replace(/^-/, ''); }`,
      `export function snakeCase(str) { return str.replace(/([A-Z])/g, '_$1').toLowerCase().replace(/^_/, ''); }`,
      `export function padStart(str, n, ch = ' ') { str = String(str); while (str.length < n) str = ch + str; return str; }`,
      `export function padEnd(str, n, ch = ' ') { str = String(str); while (str.length < n) str += ch; return str; }`,
      `export function repeat(str, n) { let r = ''; for (let i = 0; i < n; i++) r += str; return r; }`,
      `export function reverse(str) { return str.split('').reverse().join(''); }`,
      `export function countChar(str, ch) { let c = 0; for (const x of str) if (x === ch) c++; return c; }`,
      `export function template(str, data) { return str.replace(/\\{(\\w+)\\}/g, (_, k) => data[k] != null ? data[k] : ''); }`,
      `export function pluralize(count, singular, plural) { return count === 1 ? singular : (plural || singular + 's'); }`,
    ]},
    { name: 'number', functions: [
      `export function clamp(v, min, max) { return v < min ? min : v > max ? max : v; }`,
      `export function lerp(a, b, t) { return a + (b - a) * Math.max(0, Math.min(1, t)); }`,
      `export function sum(...args) { return args.reduce((s, v) => s + v, 0); }`,
      `export function average(...args) { return args.length ? sum(...args) / args.length : 0; }`,
      `export function roundTo(n, digits = 0) { const m = Math.pow(10, digits); return Math.round(n * m) / m; }`,
      `export function random(min, max) { return min + Math.random() * (max - min); }`,
      `export function randomInt(min, max) { return Math.floor(random(min, max + 1)); }`,
      `export function clampInt(v, min, max) { return Math.round(clamp(v, min, max)); }`,
    ]},
    { name: 'array', functions: [
      `export function first(arr) { return arr && arr.length ? arr[0] : undefined; }`,
      `export function last(arr) { return arr && arr.length ? arr[arr.length - 1] : undefined; }`,
      `export function chunk(arr, size) { const r = []; for (let i = 0; i < arr.length; i += size) r.push(arr.slice(i, i + size)); return r; }`,
      `export function compact(arr) { return arr.filter(x => x != null && x !== false && x !== ''); }`,
      `export function unique(arr) { return [...new Set(arr)]; }`,
      `export function flatten(arr) { const r = []; for (const x of arr) Array.isArray(x) ? r.push(...flatten(x)) : r.push(x); return r; }`,
      `export function groupBy(arr, key) { const r = {}; for (const x of arr) { const k = typeof key === 'function' ? key(x) : x[key]; (r[k] ||= []).push(x); } return r; }`,
      `export function sortBy(arr, key, desc = false) { return [...arr].sort((a, b) => { const av = typeof key === 'function' ? key(a) : a[key]; const bv = typeof key === 'function' ? key(b) : b[key]; return av < bv ? (desc ? 1 : -1) : av > bv ? (desc ? -1 : 1) : 0; }); }`,
      `export function range(start, end) { const r = []; for (let i = start; i < end; i++) r.push(i); return r; }`,
      `export function partition(arr, pred) { const t = [], f = []; for (const x of arr) (pred(x) ? t : f).push(x); return [t, f]; }`,
      `export function intersection(a, b) { const s = new Set(b); return a.filter(x => s.has(x)); }`,
      `export function difference(a, b) { const s = new Set(b); return a.filter(x => !s.has(x)); }`,
    ]},
    { name: 'object', functions: [
      `export function pick(obj, ...keys) { const r = {}; for (const k of keys) if (k in obj) r[k] = obj[k]; return r; }`,
      `export function omit(obj, ...keys) { const s = new Set(keys); const r = {}; for (const k of Object.keys(obj)) if (!s.has(k)) r[k] = obj[k]; return r; }`,
      `export function merge(...objs) { const r = {}; for (const o of objs) if (o) Object.assign(r, o); return r; }`,
      `export function deepClone(obj) { return JSON.parse(JSON.stringify(obj)); }`,
      `export function mapValues(obj, fn) { const r = {}; for (const k of Object.keys(obj)) r[k] = fn(obj[k], k); return r; }`,
      `export function isEmpty(obj) { return obj == null || Object.keys(obj).length === 0; }`,
      `export function hasKey(obj, key) { return obj != null && Object.prototype.hasOwnProperty.call(obj, key); }`,
      `export function get(obj, path, def) { const keys = Array.isArray(path) ? path : path.split('.'); let cur = obj; for (const k of keys) { if (cur == null) return def; cur = cur[k]; } return cur ?? def; }`,
      `export function set(obj, path, val) { const keys = Array.isArray(path) ? path : path.split('.'); let cur = obj; for (let i = 0; i < keys.length - 1; i++) { if (cur[keys[i]] == null) cur[keys[i]] = {}; cur = cur[keys[i]]; } cur[keys[keys.length - 1]] = val; return obj; }`,
    ]},
    { name: 'date', functions: [
      `export function formatDate(d) { const dt = d instanceof Date ? d : new Date(d); return dt.getFullYear() + '-' + String(dt.getMonth() + 1).padStart(2, '0') + '-' + String(dt.getDate()).padStart(2, '0'); }`,
      `export function formatTime(d) { const dt = d instanceof Date ? d : new Date(d); return String(dt.getHours()).padStart(2, '0') + ':' + String(dt.getMinutes()).padStart(2, '0'); }`,
      `export function formatDateTime(d) { return formatDate(d) + ' ' + formatTime(d); }`,
      `export function isSameDay(a, b) { return formatDate(a) === formatDate(b); }`,
      `export function diffDays(a, b) { return Math.floor((new Date(b) - new Date(a)) / (86400000)); }`,
      `export function addDays(d, n) { const r = new Date(d); r.setDate(r.getDate() + n); return r; }`,
      `export function isLeapYear(y) { return (y % 4 === 0 && y % 100 !== 0) || y % 400 === 0; }`,
      `export function daysInMonth(y, m) { return new Date(y, m, 0).getDate(); }`,
      `export function isWeekend(d) { const day = new Date(d).getDay(); return day === 0 || day === 6; }`,
      `export function relativeTime(d) { const diff = Date.now() - new Date(d).getTime(); const mins = Math.floor(diff / 60000); if (mins < 1) return '刚刚'; if (mins < 60) return mins + '分钟前'; const hours = Math.floor(mins / 60); if (hours < 24) return hours + '小时前'; const days = Math.floor(hours / 24); if (days < 30) return days + '天前'; return formatDate(d); }`,
    ]},
    { name: 'validation', functions: [
      `export function isEmail(v) { return /^[^\\s@]+@[^\\s@]+\\.[^\\s@]+$/.test(v); }`,
      `export function isUrl(v) { try { new URL(v); return true; } catch { return false; } }`,
      `export function isPhone(v) { return /^1[3-9]\\d{9}$/.test(v); }`,
      `export function isNumeric(v) { return !isNaN(parseFloat(v)) && isFinite(v); }`,
      `export function minLength(v, min) { return String(v).length >= min; }`,
      `export function maxLength(v, max) { return String(v).length <= max; }`,
      `export function isInRange(v, min, max) { const n = Number(v); return !isNaN(n) && n >= min && n <= max; }`,
      `export function matches(v, pattern) { return pattern.test(String(v)); }`,
      `export function isRequired(v) { return v != null && v !== ''; }`,
    ]},
    { name: 'format', functions: [
      `export function formatBytes(bytes) { if (bytes === 0) return '0 B'; const k = 1024; const sizes = ['B', 'KB', 'MB', 'GB']; const i = Math.floor(Math.log(bytes) / Math.log(k)); return parseFloat((bytes / Math.pow(k, i)).toFixed(2)) + ' ' + sizes[i]; }`,
      `export function formatCurrency(amount, currency = 'CNY') { return new Intl.NumberFormat('zh-CN', { style: 'currency', currency }).format(amount); }`,
      `export function formatNumber(n) { return n.toLocaleString('zh-CN'); }`,
      `export function formatPercentage(n) { return (n * 100).toFixed(1) + '%'; }`,
      `export function ellipsis(str, maxLen) { return str && str.length > maxLen ? str.slice(0, maxLen) + '...' : str; }`,
      `export function maskPhone(phone) { return phone ? phone.replace(/(\\d{3})\\d{4}(\\d{4})/, '$1****$2') : ''; }`,
      `export function maskEmail(email) { return email ? email.replace(/^(.)(.*)(@.*)$/, (_, a, b, c) => a + '*'.repeat(Math.min(b.length, 4)) + c) : ''; }`,
    ]},
    { name: 'dom', functions: [
      `export function getScrollTop() { return window.scrollY || document.documentElement.scrollTop || 0; }`,
      `export function setTitle(title) { document.title = title; }`,
      `export function getViewport() { return { width: window.innerWidth, height: window.innerHeight }; }`,
      `export function isElementInView(el) { const r = el.getBoundingClientRect(); return r.top >= 0 && r.left >= 0 && r.bottom <= window.innerHeight && r.right <= window.innerWidth; }`,
      `export function on(event, el, handler) { el.addEventListener(event, handler); return () => el.removeEventListener(event, handler); }`,
    ]},
    { name: 'async', functions: [
      `export function debounce(fn, delay = 300) { let timer; return (...args) => { clearTimeout(timer); timer = setTimeout(() => fn(...args), delay); }; }`,
      `export function throttle(fn, interval = 300) { let last = 0; return (...args) => { const now = Date.now(); if (now - last >= interval) { last = now; fn(...args); } }; }`,
      `export function sleep(ms) { return new Promise(r => setTimeout(r, ms)); }`,
      `export function memoize(fn) { const cache = new Map(); return (...args) => { const k = JSON.stringify(args); if (cache.has(k)) return cache.get(k); const r = fn(...args); cache.set(k, r); return r; }; }`,
      `export function once(fn) { let called = false; let result; return (...args) => { if (!called) { called = true; result = fn(...args); } return result; }; }`,
    ]},
  ];

  // Split into individual files, varying in size
  for (const mod of modules) {
    const fns = mod.functions;
    // Medium utility file with ALL functions
    let code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
    for (const fn of fns) {
      code += fn + '\n\n';
    }
    code += `globalThis.__reg['utils_${mod.name}'] = ${100 + ALL_MODULES.length};\n`;
    write(defineModule('utils', `utils_${mod.name}`, 'medium', dir), code);
  }

  // Extra utility variants for more files
  const extraNames = [];
  for (let i = 0; i < 230; i++) extraNames.push(`util_extra_${String(i + 1).padStart(3, '0')}`);
  for (const name of extraNames) {
    let code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
    const fc = randInt(2, 5);
    for (let f = 0; f < fc; f++) {
      const fnName = `${name}_fn_${f}`;
      const param = ['x', 'val', 'a', 'b', 'input', 'data', 'obj', 'arr', 'n', 'key'][randInt(0, 9)];
      if (Math.random() > 0.5) {
        code += `export function ${fnName}(${param}) {\n  ${param} = ${param} || 0;\n  let result = 0;\n  for (let i = 0; i < 10; i++) result += ${param} + i;\n  return result;\n}\n\n`;
      } else {
        code += `export function ${fnName}(${param}, opts = {}) {\n  const defaulted = { ...{ flag: true, limit: 100, offset: 0 }, ...opts };\n  if (!${param}) return defaulted;\n  return { input: ${param}, ...defaulted, processed: true };\n}\n\n`;
      }
    }
    code += `globalThis.__reg['utils_${name}'] = ${100 + ALL_MODULES.length};\n`;
    write(defineModule('utils', `utils_${name}`, 'small', dir), code);
  }

  // Barrel
  const allUtils = MOD_BY_CAT['utils'].filter(m => m.name !== 'index');
  let barrel = '';
  for (const m of allUtils) {
    barrel += `export * from './${m.name}.js';\n`;
  }
  barrel += `\nglobalThis.__reg || (globalThis.__reg = {});\nglobalThis.__reg['utils_index'] = ${199 + ALL_MODULES.length};\n`;
  write(defineModule('utils', 'index', 'tiny', dir), barrel);
}

// ====== DATA (large mock data modules) ======
function buildData() {
  const dir = 'data';
  const names = ['users', 'products', 'orders', 'analytics', 'configs', 'reports', 'inventory', 'customers', 'vendors', 'invoices',
    'templates', 'workflows', 'audit_logs', 'activity_logs', 'email_templates', 'notifications', 'messages', 'comments', 'ratings', 'reviews',
    'articles', 'categories', 'tags', 'menus', 'widgets', 'dashboards', 'charts', 'maps', 'calendars', 'events',
    'schedules', 'tasks', 'projects', 'milestones', 'resources', 'assets', 'files', 'documents', 'images', 'videos',
    'articles_02', 'categories_02', 'tags_02', 'menus_02', 'widgets_02', 'dashboards_02', 'charts_02', 'maps_02',
    'calendars_02', 'events_02', 'schedules_02', 'tasks_02', 'projects_02', 'milestones_02', 'resources_02',
    'assets_02', 'files_02', 'documents_02', 'images_02', 'videos_02',
    'articles_03', 'categories_03', 'tags_03', 'menus_03', 'widgets_03', 'dashboards_03', 'charts_03',
    'schedules_03', 'tasks_03', 'projects_03', 'milestones_03', 'resources_03', 'assets_03', 'files_03',
    'documents_03', 'images_03',
    'data_bulk_01', 'data_bulk_02', 'data_bulk_03', 'data_bulk_04', 'data_bulk_05',
    'data_bulk_06', 'data_bulk_07', 'data_bulk_08', 'data_bulk_09', 'data_bulk_10',
    'data_bulk_11', 'data_bulk_12', 'data_bulk_13', 'data_bulk_14', 'data_bulk_15',
    'data_bulk_16', 'data_bulk_17', 'data_bulk_18', 'data_bulk_19', 'data_bulk_20',
  ];

  for (let i = 0; i < names.length; i++) {
    const name = names[i];
    const isHuge = i < 5; // First 5 are huge (500+ lines)
    const size = isHuge ? 'huge' : 'large';
    const itemCount = isHuge ? randInt(200, 500) : randInt(30, 100);
    let code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
    code += `export const ${name.toUpperCase()}_DATA = [\n`;
    for (let j = 0; j < itemCount; j++) {
      const item = { id: `${name}_${j}`,
        name: `${name} item ${j}`,
        value: randInt(0, 10000),
        active: Math.random() > 0.2,
        createdAt: `2026-0${randInt(1, 9)}-${String(randInt(1, 28)).padStart(2, '0')}`,
        updatedAt: `2026-0${randInt(1, 9)}-${String(randInt(1, 28)).padStart(2, '0')}`,
        meta: { version: randInt(1, 5), status: ['active', 'inactive', 'pending'][randInt(0, 2)] } };
      code += `  ${JSON.stringify(item)},\n`;
    }
    code += '];\n\n';
    code += `export function get${name.charAt(0).toUpperCase() + name.slice(1)}(id) {\n`;
    code += `  return ${name.toUpperCase()}_DATA.find(x => x.id === id) || null;\n}\n\n`;
    code += `export function filter${name.charAt(0).toUpperCase() + name.slice(1)}(pred) {\n`;
    code += `  return ${name.toUpperCase()}_DATA.filter(pred);\n}\n\n`;
    code += `globalThis.__reg['data_${name}'] = ${300 + ALL_MODULES.length};\n`;
    write(defineModule('data', `data_${name}`, size, dir), code);
  }

  // Barrel
  const allData = MOD_BY_CAT['data'].filter(m => m.name !== 'index');
  let barrel = '';
  for (const m of allData) {
    barrel += `export * from './${m.name}.js';\n`;
  }
  barrel += `\nglobalThis.__reg || (globalThis.__reg = {});\nglobalThis.__reg['data_index'] = ${399 + ALL_MODULES.length};\n`;
  write(defineModule('data', 'index', 'tiny', dir), barrel);
}

// ====== STORE ======
function buildStores() {
  const dir = 'store';
  const storeDefs = [
    { name: 'userStore', fields: ['currentUser', 'token', 'isAuthenticated', 'permissions', 'preferences'] },
    { name: 'appStore', fields: ['sidebar', 'theme', 'locale', 'loading', 'error'] },
    { name: 'dashboardStore', fields: ['stats', 'recentActivity', 'chartData', 'dateRange', 'loading'] },
    { name: 'settingsStore', fields: ['profile', 'security', 'notifications', 'appearance', 'privacy'] },
    { name: 'notificationStore', fields: ['items', 'unreadCount', 'loading', 'filter'] },
    { name: 'adminStore', fields: ['users', 'roles', 'auditLog', 'systemHealth', 'config'] },
    { name: 'authStore', fields: ['user', 'token', 'refreshToken', 'expiresAt', 'mfaEnabled'] },
    { name: 'themeStore', fields: ['mode', 'primaryColor', 'fontSize', 'spacing', 'borderRadius'] },
    { name: 'routerStore', fields: ['currentPath', 'params', 'query', 'history', 'matched'] },
    { name: 'cacheStore', fields: ['data', 'ttl', 'timestamps', 'maxEntries', 'hits'] },
    { name: 'formStore', fields: ['values', 'errors', 'touched', 'submitting', 'valid'] },
    { name: 'listStore', fields: ['items', 'total', 'page', 'pageSize', 'sort'] },
    { name: 'modalStore', fields: ['open', 'title', 'content', 'size', 'closable'] },
    { name: 'toastStore', fields: ['messages', 'position', 'maxVisible', 'duration'] },
    { name: 'undoStore', fields: ['stack', 'index', 'maxSize', 'disabled'] },
    { name: 'searchStore', fields: ['query', 'results', 'recent', 'suggestions', 'loading'] },
    { name: 'uploadStore', fields: ['queue', 'active', 'completed', 'failed', 'progress'] },
    { name: 'socketStore', fields: ['connected', 'channel', 'events', 'reconnectAttempts', 'latency'] },
    { name: 'featureStore', fields: ['flags', 'experiments', 'rollout', 'overrides'] },
    { name: 'analyticsStore', fields: ['events', 'pageViews', 'sessions', 'goals', 'funnels'] },
  ];

  for (const def of storeDefs) {
    let code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
    code += `let _state = { ${def.fields.map(f => `${f}: null`).join(', ')} };\n`;
    code += `const _listeners = new Set();\n\n`;
    code += `export function getState() { return { ..._state }; }\n\n`;
    code += `export function setState(partial) {\n  _state = { ..._state, ...(typeof partial === 'function' ? partial(_state) : partial) };\n  _listeners.forEach(fn => fn(_state));\n}\n\n`;
    code += `export function subscribe(fn) { _listeners.add(fn); return () => _listeners.delete(fn); }\n\n`;
    for (const field of def.fields) {
      code += `export function get${field.charAt(0).toUpperCase() + field.slice(1)}() { return _state.${field}; }\n`;
      code += `export function set${field.charAt(0).toUpperCase() + field.slice(1)}(val) { setState({ ${field}: val }); }\n\n`;
    }
    code += `export function reset() { _state = { ${def.fields.map(f => `${f}: null`).join(', ')} }; _listeners.clear(); }\n\n`;
    code += `globalThis.__reg['store_${def.name}'] = ${400 + ALL_MODULES.length};\n`;
    write(defineModule('store', `store_${def.name}`, 'medium', dir), code);
  }

  // No barrel for store — conflicting star exports (webpack compat)
}

// ====== HOOKS ======
function buildHooks() {
  const dir = 'hooks';
  const hooks = [
    { name: 'useAuth', deps: ['store/store_authStore', 'shared/api'], body: `export function useAuth() {\n  const { getState, subscribe } = requireOrImport('../store/store_authStore.js');\n  const state = getState();\n  return { user: state.currentUser, isAuthenticated: !!state.token, login: (u, t) => ({ user: u, token: t }), logout: () => {} };\n}` },
    { name: 'useForm', deps: ['shared/hooks'], body: `import { useState } from '../shared/hooks.js';\n\nexport function useForm(initial = {}) {\n  const [values, setValues] = useState(initial);\n  const [errors, setErrors] = useState({});\n  const [touched, setTouched] = useState({});\n  const setField = (name, value) => setValues(v => ({ ...v, [name]: value }));\n  const setFieldTouched = (name) => setTouched(t => ({ ...t, [name]: true }));\n  const validate = (rules) => { const errs = {}; for (const [k, v] of Object.entries(values())) { const rule = rules[k]; if (rule) { const msg = rule(v, values()); if (msg) errs[k] = msg; } } setErrors(errs); return Object.keys(errs).length === 0; };\n  return { values, errors, touched, setField, setFieldTouched, validate, setValues, setErrors, reset: () => { setValues(initial); setErrors({}); setTouched({}); } };\n}` },
    { name: 'useFetch', deps: ['shared/api', 'shared/hooks'], body: `import { get } from '../shared/api.js';\nimport { useState, useEffect } from '../shared/hooks.js';\n\nexport function useFetch(url, opts = {}) {\n  const [data, setData] = useState(null);\n  const [loading, setLoading] = useState(true);\n  const [error, setError] = useState(null);\n  const { refetch } = opts;\n  const load = async () => {\n    setLoading(true); setError(null);\n    try { const res = await get(url); setData(res.data); } catch (e) { setError(e); }\n    finally { setLoading(false); }\n  };\n  return { data, loading, error, refetch: load };\n}` },
    { name: 'useDebounce', deps: ['utils/utils_async'], body: `import { debounce } from '../utils/utils_async.js';\n\nexport function useDebounce(fn, delay = 300) {\n  return debounce(fn, delay);\n}` },
    { name: 'useLocalStorage', deps: [], body: `globalThis.__reg || (globalThis.__reg = {});\n\nexport function useLocalStorage(key, initial) {\n  const read = () => { try { const v = localStorage.getItem(key); return v ? JSON.parse(v) : initial; } catch { return initial; } };\n  const write = (val) => { try { localStorage.setItem(key, JSON.stringify(val)); } catch {} };\n  return { get: read, set: write, remove: () => localStorage.removeItem(key) };\n}` },
    { name: 'useMediaQuery', deps: [], body: `globalThis.__reg || (globalThis.__reg = {});\n\nexport function useMediaQuery(query) {\n  const mql = typeof window !== 'undefined' ? window.matchMedia(query) : null;\n  const matches = () => mql ? mql.matches : false;\n  return { matches, query, mql };\n}` },
    { name: 'usePagination', deps: [], body: `globalThis.__reg || (globalThis.__reg = {});\n\nexport function usePagination(total, pageSize = 20) {\n  const totalPages = Math.ceil(total / pageSize);\n  let page = 1;\n  return { page: () => page, totalPages, total, pageSize,\n    goTo: (p) => { if (p >= 1 && p <= totalPages) page = p; },\n    next: () => { if (page < totalPages) page++; },\n    prev: () => { if (page > 1) page--; },\n    hasNext: () => page < totalPages,\n    hasPrev: () => page > 1,\n    offset: () => (page - 1) * pageSize };\n}` },
    { name: 'useToggle', deps: [], body: `globalThis.__reg || (globalThis.__reg = {});\n\nexport function useToggle(initial = false) {\n  let value = initial;\n  return { get: () => value, set: (v) => { value = v; }, toggle: () => { value = !value; return value; }, reset: () => { value = initial; } };\n}` },
    { name: 'useCounter', deps: [], body: `globalThis.__reg || (globalThis.__reg = {});\n\nexport function useCounter(start = 0) {\n  let count = start;\n  return { get: () => count, inc: (n = 1) => { count += n; }, dec: (n = 1) => { count -= n; }, reset: () => { count = start; } };\n}` },
    { name: 'useTimer', deps: ['shared/hooks'], body: `import { useState, useEffect } from '../shared/hooks.js';\n\nexport function useTimer(initial = 0) {\n  const [get, set] = useState(initial);\n  let interval = null;\n  return { get, start: () => { if (!interval) interval = setInterval(() => set(v => v + 1), 1000); }, stop: () => { if (interval) { clearInterval(interval); interval = null; } }, reset: () => set(0) };\n}` },
    { name: 'useClipboard', deps: [], body: `globalThis.__reg || (globalThis.__reg = {});\n\nexport function useClipboard() {\n  let last = null;\n  return { copy: async (text) => { try { await navigator.clipboard.writeText(text); last = text; return true; } catch { return false; } }, last: () => last };\n}` },
    { name: 'useOnlineStatus', deps: [], body: `globalThis.__reg || (globalThis.__reg = {});\n\nexport function useOnlineStatus() {\n  const online = typeof navigator !== 'undefined' ? navigator.onLine : true;\n  return { online, isOffline: !online };\n}` },
    { name: 'useScrollPosition', deps: [], body: `globalThis.__reg || (globalThis.__reg = {});\n\nexport function useScrollPosition() {\n  let x = 0, y = 0;\n  if (typeof window !== 'undefined') { x = window.scrollX; y = window.scrollY; }\n  return { x, y, scrollTo: (nx, ny) => { if (typeof window !== 'undefined') window.scrollTo(nx, ny); } };\n}` },
    { name: 'usePrevious', deps: ['shared/hooks'], body: `import { useRef } from '../shared/hooks.js';\n\nexport function usePrevious(value) {\n  const ref = useRef();\n  const prev = ref.current;\n  ref.current = value;\n  return prev;\n}` },
    { name: 'useIntersection', deps: [], body: `globalThis.__reg || (globalThis.__reg = {});\n\nexport function useIntersection(opts = {}) {\n  let observer = null;\n  let isIntersecting = false;\n  return { observe: (el) => { if (typeof IntersectionObserver === 'undefined') return; observer = new IntersectionObserver(([entry]) => { isIntersecting = entry.isIntersecting; }, opts); observer.observe(el); }, isIntersecting: () => isIntersecting, disconnect: () => { if (observer) observer.disconnect(); } };\n}` },
  ];

  // Generate more hook variants
  const extraHooks = [];
  for (let i = 0; i < 130; i++) {
    extraHooks.push({
      name: `useCustom_${String(i + 1).padStart(3, '0')}`,
      deps: randInt(0, 3) > 1 ? ['shared/hooks'] : [],
      body: null,
    });
  }

  for (const h of [...hooks, ...extraHooks]) {
    let code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
    if (h.body) {
      code += h.body + '\n\n';
    } else {
      const fc = randInt(1, 3);
      for (let f = 0; f < fc; f++) {
        const fnName = `useCustom_${h.name.replace('useCustom_', '')}_fn${f}`;
        code += `export function ${fnName}() {\n  let state = ${JSON.stringify(`state_${h.name}_${f}`)};\n  return { get: () => state, set: (v) => { state = v; }, update: (fn) => { state = fn(state); } };\n}\n\n`;
      }
    }
    code += `globalThis.__reg['hooks_${h.name}'] = ${500 + ALL_MODULES.length};\n`;
    write(defineModule('hooks', `hooks_${h.name}`, 'medium', dir), code);
  }

  // Barrel
  const allHooks = MOD_BY_CAT['hooks'].filter(m => m.name !== 'index');
  let barrel = '';
  for (const m of allHooks) {
    barrel += `export * from './${m.name}.js';\n`;
  }
  barrel += `\nglobalThis.__reg || (globalThis.__reg = {});\nglobalThis.__reg['hooks_index'] = ${599 + ALL_MODULES.length};\n`;
  write(defineModule('hooks', 'index', 'tiny', dir), barrel);
}

// ====== SERVICES ======
function buildServices() {
  const dir = 'services';
  const services = [
    { name: 'apiClient', body: `globalThis.__reg || (globalThis.__reg = {});\nlet _baseUrl = '/api';\nlet _headers = { 'Content-Type': 'application/json' };\nlet _interceptors = [];\n\nexport function setBaseUrl(url) { _baseUrl = url; }\nexport function setHeaders(h) { _headers = { ..._headers, ...h }; }\nexport function addInterceptor(fn) { _interceptors.push(fn); return () => _interceptors = _interceptors.filter(x => x !== fn); }\n\nexport async function request(method, path, data = null) {\n  let config = { method, headers: { ..._headers }, url: _baseUrl + path };\n  if (data) config.body = JSON.stringify(data);\n  for (const interceptor of _interceptors) { const result = interceptor(config); if (result) config = result; }\n  return { ok: true, status: 200, data: null, config, timestamp: Date.now() };\n}\n\nexport function get(path) { return request('GET', path); }\nexport function post(path, data) { return request('POST', path, data); }\nexport function put(path, data) { return request('PUT', path, data); }\nexport function del(path) { return request('DELETE', path); }\n\nglobalThis.__reg['services_apiClient'] = ${600 + FILE_ID};` },
    { name: 'authService', body: null },
    { name: 'dashboardService', body: null },
    { name: 'settingsService', body: null },
    { name: 'adminService', body: null },
    { name: 'notificationService', body: null },
    { name: 'logger', body: null },
    { name: 'cache', body: null },
    { name: 'analytics', body: null },
    { name: 'storage', body: null },
    { name: 'webSocket', body: null },
    { name: 'fileUpload', body: null },
    { name: 'exportService', body: null },
    { name: 'importService', body: null },
    { name: 'templateService', body: null },
    { name: 'searchService', body: null },
    { name: 'reportService', body: null },
    { name: 'backupService', body: null },
    { name: 'monitoringService', body: null },
    { name: 'schedulerService', body: null },
    { name: 'notificationDispatchService', body: null },
    { name: 'paymentService', body: null },
    { name: 'invoiceService', body: null },
    { name: 'subscriptionService', body: null },
    { name: 'billingService', body: null },
    { name: 'emailService', body: null },
    { name: 'smsService', body: null },
    { name: 'pushService', body: null },
    { name: 'webhookService', body: null },
    { name: 'integrationService', body: null },
    { name: 'migrationService', body: null },
    { name: 'healthCheckService', body: null },
    { name: 'rateLimitService', body: null },
    { name: 'circuitBreakerService', body: null },
    { name: 'discoveryService', body: null },
    { name: 'configService', body: null },
    { name: 'featureFlagService', body: null },
    { name: 'auditService', body: null },
    { name: 'complianceService', body: null },
    { name: 'encryptionService', body: null },
    { name: 'keyManagementService', body: null },
    { name: 'tokenService', body: null },
    { name: 'sessionService', body: null },
    { name: 'permissionService', body: null },
    { name: 'roleService', body: null },
    { name: 'orgService', body: null },
    { name: 'tenantService', body: null },
    { name: 'workflowService', body: null },
    { name: 'approvalService', body: null },
  ];

  const serviceTemplates = [
    `globalThis.__reg || (globalThis.__reg = {});\nlet _initialized = false;\nlet _config = {};\n\nexport function init(opts = {}) { _config = { ..._config, ...opts }; _initialized = true; return _config; }\nexport function isInitialized() { return _initialized; }\nexport function getConfig() { return { ..._config }; }\nexport function reset() { _config = {}; _initialized = false; }\n`,
    `globalThis.__reg || (globalThis.__reg = {});\nconst _handlers = new Map();\n\nexport function on(event, handler) { if (!_handlers.has(event)) _handlers.set(event, []); _handlers.get(event).push(handler); return () => { const h = _handlers.get(event); if (h) { const idx = h.indexOf(handler); if (idx >= 0) h.splice(idx, 1); } }; }\nexport function emit(event, ...args) { const h = _handlers.get(event); if (h) h.forEach(fn => fn(...args)); }\nexport function clear() { _handlers.clear(); }\n`,
    `globalThis.__reg || (globalThis.__reg = {});\nconst _queue = [];\nlet _processing = false;\n\nexport function enqueue(task) { _queue.push(task); process(); }\nexport function dequeue() { return _queue.shift(); }\nexport function peek() { return _queue.length > 0 ? _queue[0] : null; }\nexport function size() { return _queue.length; }\nexport function clear() { _queue.length = 0; }\nasync function process() { if (_processing) return; _processing = true; while (_queue.length > 0) { const task = _queue[0]; try { await task(); _queue.shift(); } catch { _queue.shift(); } } _processing = false; }\n`,
    `globalThis.__reg || (globalThis.__reg = {});\nconst _cache = new Map();\nlet _maxSize = 100;\n\nexport function setMaxSize(n) { _maxSize = n; }\nexport function get(key) { return _cache.get(key) || null; }\nexport function set(key, val) { if (_cache.size >= _maxSize) { const first = _cache.keys().next().value; _cache.delete(first); } _cache.set(key, val); }\nexport function has(key) { return _cache.has(key); }\nexport function remove(key) { _cache.delete(key); }\nexport function clear() { _cache.clear(); }\nexport function size() { return _cache.size; }\n`,
  ];

  for (const svc of services) {
    let code;
    if (svc.body) {
      code = svc.body;
    } else {
      code = serviceTemplates[randInt(0, serviceTemplates.length - 1)];
      const id = `services_${svc.name}`;
      code += `\nglobalThis.__reg['${id}'] = ${600 + ALL_MODULES.length};\n`;
    }
    write(defineModule('services', `services_${svc.name}`, 'medium', dir), code);
  }

  // No barrel for services — conflicting star exports (webpack compat)
}

// ====== FEATURES (hand-crafted business modules) ======
function buildFeatures() {
  const features = {
    auth: {
      label: 'Auth',
      files: [
        { name: 'authService', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport async function login(email, password) {\n  if (!email || !password) throw new Error('Email and password required');\n  const response = await fetch('/api/auth/login', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ email, password }) });\n  if (!response.ok) throw new Error('Login failed');\n  return response.json();\n}\n\nexport async function register(data) {\n  const response = await fetch('/api/auth/register', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(data) });\n  if (!response.ok) throw new Error('Registration failed');\n  return response.json();\n}\n\nexport async function logout() {\n  await fetch('/api/auth/logout', { method: 'POST' });\n}\n\nexport async function refreshToken(token) {\n  const response = await fetch('/api/auth/refresh', { method: 'POST', headers: { 'Content-Type': 'application/json' }, body: JSON.stringify({ token }) });\n  if (!response.ok) throw new Error('Token refresh failed');\n  return response.json();\n}\n\nexport async function getCurrentUser() {\n  const response = await fetch('/api/auth/me');\n  if (!response.ok) throw new Error('Failed to get user');\n  return response.json();\n}\n\nglobalThis.__reg['features_auth_authService'] = ${700 + FILE_ID};` },
        { name: 'authStore', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nlet _user = null;\nlet _token = null;\nlet _refreshToken = null;\nlet _isAuthenticated = false;\nconst _listeners = new Set();\n\nfunction notify() { _listeners.forEach(fn => fn({ user: _user, token: _token, isAuthenticated: _isAuthenticated })); }\n\nexport function getAuth() { return { user: _user, token: _token, isAuthenticated: _isAuthenticated }; }\nexport function setAuth(user, token, refresh) { _user = user; _token = token; _refreshToken = refresh; _isAuthenticated = true; notify(); }\nexport function clearAuth() { _user = null; _token = null; _refreshToken = null; _isAuthenticated = false; notify(); }\nexport function subscribe(fn) { _listeners.add(fn); return () => _listeners.delete(fn); }\nexport function getToken() { return _token; }\nexport function getRefreshToken() { return _refreshToken; }\n\nglobalThis.__reg['features_auth_authStore'] = ${701 + FILE_ID};` },
        { name: 'useAuth', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport function useAuth() {\n  return {\n    login: async (email, password) => {\n      const { login } = await import('./authService.js');\n      const result = await login(email, password);\n      return result;\n    },\n    logout: async () => {\n      const { logout } = await import('./authService.js');\n      await logout();\n      const { clearAuth } = await import('./authStore.js');\n      clearAuth();\n    },\n    register: async (data) => {\n      const { register } = await import('./authService.js');\n      return await register(data);\n    },\n  };\n}\n\nglobalThis.__reg['features_auth_useAuth'] = ${702 + FILE_ID};` },
        { name: 'LoginPage', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport function LoginPage() {\n  return {\n    render: () => ({\n      tag: 'div',\n      attrs: { className: 'login-page' },\n      children: [\n        { tag: 'h1', attrs: {}, children: ['Login'] },\n        { tag: 'form', attrs: { onSubmit: 'handleLogin' }, children: [\n          { tag: 'input', attrs: { type: 'email', placeholder: 'Email', name: 'email' } },\n          { tag: 'input', attrs: { type: 'password', placeholder: 'Password', name: 'password' } },\n          { tag: 'button', attrs: { type: 'submit' }, children: ['Sign In'] },\n        ]},\n      ],\n    }),\n  };\n}\n\nglobalThis.__reg['features_auth_LoginPage'] = ${703 + FILE_ID};` },
        { name: 'RegisterPage', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport function RegisterPage() {\n  return {\n    render: () => ({\n      tag: 'div',\n      attrs: { className: 'register-page' },\n      children: [\n        { tag: 'h1', attrs: {}, children: ['Create Account'] },\n        { tag: 'form', attrs: { onSubmit: 'handleRegister' }, children: [\n          { tag: 'input', attrs: { type: 'text', placeholder: 'Name', name: 'name' } },\n          { tag: 'input', attrs: { type: 'email', placeholder: 'Email', name: 'email' } },\n          { tag: 'input', attrs: { type: 'password', placeholder: 'Password', name: 'password' } },\n          { tag: 'input', attrs: { type: 'password', placeholder: 'Confirm Password', name: 'confirmPassword' } },\n          { tag: 'button', attrs: { type: 'submit' }, children: ['Register'] },\n        ]},\n      ],\n    }),\n  };\n}\n\nglobalThis.__reg['features_auth_RegisterPage'] = ${704 + FILE_ID};` },
        { name: 'ForgotPasswordPage', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport function ForgotPasswordPage() {\n  return {\n    render: () => ({\n      tag: 'div',\n      attrs: { className: 'forgot-password-page' },\n      children: [\n        { tag: 'h2', attrs: {}, children: ['Reset Password'] },\n        { tag: 'p', attrs: {}, children: ['Enter your email to receive a reset link.'] },\n        { tag: 'form', attrs: {}, children: [\n          { tag: 'input', attrs: { type: 'email', placeholder: 'Email', name: 'email' } },\n          { tag: 'button', attrs: { type: 'submit' }, children: ['Send Reset Link'] },\n        ]},\n      ],\n    }),\n  };\n}\n\nglobalThis.__reg['features_auth_ForgotPasswordPage'] = ${705 + FILE_ID};` },
        { name: 'ProtectedRoute', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport function ProtectedRoute({ component: Component, fallback = '/login' }) {\n  const isAuthenticated = true;\n  if (!isAuthenticated) return { redirect: fallback };\n  return { component: Component, authenticated: true };\n}\n\nglobalThis.__reg['features_auth_ProtectedRoute'] = ${706 + FILE_ID};` },
        { name: 'index', deps: [], content: ctx => `export { login, register, logout, refreshToken, getCurrentUser } from './authService.js';\nexport { getAuth, setAuth, clearAuth, subscribe, getToken } from './authStore.js';\nexport { useAuth } from './useAuth.js';\nexport { LoginPage } from './LoginPage.js';\nexport { RegisterPage } from './RegisterPage.js';\nexport { ForgotPasswordPage } from './ForgotPasswordPage.js';\nexport { ProtectedRoute } from './ProtectedRoute.js';\nglobalThis.__reg || (globalThis.__reg = {});\nglobalThis.__reg['features_auth_index'] = ${707 + FILE_ID};` },
      ],
    },
    dashboard: {
      label: 'Dashboard',
      files: [
        { name: 'dashboardService', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport async function fetchStats() {\n  return { totalUsers: 12543, activeUsers: 8756, revenue: 452890, growth: 12.5, retention: 0.78, churn: 0.05 };\n}\n\nexport async function fetchActivity() {\n  return Array.from({ length: 20 }, (_, i) => ({ id: i, user: 'user_' + i, action: ['login', 'purchase', 'update', 'delete'][i % 4], timestamp: Date.now() - i * 60000 }));\n}\n\nexport async function fetchChartData(period = '7d') {\n  const points = period === '30d' ? 30 : period === '90d' ? 90 : 7;\n  return Array.from({ length: points }, (_, i) => ({ date: '2026-07-' + String(i + 1).padStart(2, '0'), value: Math.floor(Math.random() * 1000) }));\n}\n\nexport async function fetchKPI() {\n  return { mrr: 125000, arr: 1500000, activeSubscribers: 3200, trialUsers: 850, conversionRate: 0.068, avgSessionMs: 342000 };\n}\n\nglobalThis.__reg['features_dashboard_dashboardService'] = ${800 + FILE_ID};` },
        { name: 'dashboardStore', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nlet _state = { stats: null, activity: [], chartData: [], kpi: null, loading: false, error: null, dateRange: '7d' };\nconst _listeners = new Set();\n\nexport function getState() { return { ..._state }; }\nexport function setState(partial) { _state = { ..._state, ...(typeof partial === 'function' ? partial(_state) : partial) }; _listeners.forEach(fn => fn(_state)); }\nexport function subscribe(fn) { _listeners.add(fn); return () => _listeners.delete(fn); }\nexport function setDateRange(range) { _state.dateRange = range; }\nexport function setLoading(v) { _state.loading = v; }\nexport function setError(e) { _state.error = e; }\n\nglobalThis.__reg['features_dashboard_dashboardStore'] = ${801 + FILE_ID};` },
        { name: 'useDashboard', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport function useDashboard() {\n  return {\n    loadData: async () => {\n      const { fetchStats, fetchActivity, fetchKPI } = await import('./dashboardService.js');\n      const { setState } = await import('./dashboardStore.js');\n      const [stats, activity, kpi] = await Promise.all([fetchStats(), fetchActivity(), fetchKPI()]);\n      setState({ stats, activity, kpi, loading: false });\n    },\n  };\n}\n\nglobalThis.__reg['features_dashboard_useDashboard'] = ${802 + FILE_ID};` },
        { name: 'StatsCard', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport function StatsCard({ title, value, change, icon, color = 'blue' }) {\n  return {\n    tag: 'div',\n    attrs: { className: 'stats-card stats-card--' + color },\n    children: [\n      { tag: 'div', attrs: { className: 'stats-card__header' }, children: [\n        { tag: 'span', attrs: { className: 'stats-card__title' }, children: [title || ''] },\n        icon ? { tag: 'i', attrs: { className: 'stats-card__icon' } } : null,\n      ].filter(Boolean) },\n      { tag: 'div', attrs: { className: 'stats-card__value' }, children: [String(value ?? 0)] },\n      change != null ? { tag: 'div', attrs: { className: 'stats-card__change ' + (change >= 0 ? 'up' : 'down') }, children: [(change >= 0 ? '+' : '') + change.toFixed(1) + '%'] } : null,\n    ].filter(Boolean),\n  };\n}\n\nglobalThis.__reg['features_dashboard_StatsCard'] = ${803 + FILE_ID};` },
        { name: 'DataTable', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport function DataTable({ columns = [], data = [], pageSize = 10 }) {\n  return {\n    tag: 'div',\n    attrs: { className: 'data-table' },\n    children: [\n      { tag: 'table', attrs: { className: 'data-table__table' }, children: [\n        { tag: 'thead', attrs: {}, children: [{ tag: 'tr', attrs: {}, children: columns.map(col => ({ tag: 'th', attrs: {}, children: [col.title || col.key] })) }] },\n        { tag: 'tbody', attrs: {}, children: data.slice(0, pageSize).map(row => ({ tag: 'tr', attrs: {}, children: columns.map(col => ({ tag: 'td', attrs: {}, children: [String(row[col.key] ?? '')] })) })) },\n      ]},\n    ],\n  };\n}\n\nglobalThis.__reg['features_dashboard_DataTable'] = ${804 + FILE_ID};` },
        { name: 'ChartWidget', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport function ChartWidget({ title, data = [], type = 'line', height = 300 }) {\n  return {\n    tag: 'div',\n    attrs: { className: 'chart-widget' },\n    children: [\n      { tag: 'div', attrs: { className: 'chart-widget__header' }, children: [\n        { tag: 'h3', attrs: {}, children: [title || 'Chart'] },\n        { tag: 'div', attrs: { className: 'chart-widget__legend' }, children: data.slice(0, 5).map((d, i) => ({ tag: 'span', attrs: { className: 'legend-item' }, children: [d.label || 'Series ' + i] })) },\n      ]},\n      { tag: 'div', attrs: { className: 'chart-widget__body', style: 'height:' + height + 'px' }, children: [\n        { tag: 'svg', attrs: { viewBox: '0 0 500 ' + height }, children: data.map((d, i) => ({ tag: 'circle', attrs: { cx: 50 + i * 400 / Math.max(data.length - 1, 1), cy: height - (d.value || 0) / 100 * height, r: 4, fill: '#1890ff' } })) },\n      ]},\n    ],\n  };\n}\n\nglobalThis.__reg['features_dashboard_ChartWidget'] = ${805 + FILE_ID};` },
        { name: 'ActivityFeed', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport function ActivityFeed({ items = [] }) {\n  return {\n    tag: 'div',\n    attrs: { className: 'activity-feed' },\n    children: items.map(item => ({\n      tag: 'div',\n      attrs: { className: 'activity-feed__item' },\n      children: [\n        { tag: 'div', attrs: { className: 'activity-feed__avatar' }, children: [{ tag: 'img', attrs: { src: '/avatars/' + (item.user || 'default') + '.png', alt: item.user } }] },\n        { tag: 'div', attrs: { className: 'activity-feed__content' }, children: [\n          { tag: 'strong', attrs: {}, children: [item.user || 'Unknown'] },\n          { tag: 'span', attrs: {}, children: [' ' + (item.action || 'performed an action')] },\n        ]},\n        { tag: 'time', attrs: { className: 'activity-feed__time' }, children: [item.timestamp ? new Date(item.timestamp).toLocaleString() : ''] },\n      ],\n    })),\n  };\n}\n\nglobalThis.__reg['features_dashboard_ActivityFeed'] = ${806 + FILE_ID};` },
        { name: 'DashboardPage', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport function DashboardPage() {\n  const stats = [\n    { title: '总收入', value: '¥452,890', change: 12.5, color: 'blue' },\n    { title: '活跃用户', value: '8,756', change: 8.3, color: 'green' },\n    { title: '转化率', value: '6.8%', change: -2.1, color: 'orange' },\n    { title: '平均会话', value: '5.7min', change: 3.4, color: 'purple' },\n  ];\n  return {\n    render: () => ({\n      tag: 'div',\n      attrs: { className: 'dashboard-page' },\n      children: [\n        { tag: 'h2', attrs: {}, children: ['仪表盘'] },\n        { tag: 'div', attrs: { className: 'dashboard-stats-grid' }, children: stats.map(s => ({ tag: 'div', attrs: { className: 'stat-card stat-card--' + s.color }, children: [{ tag: 'h4', attrs: {}, children: [s.title] }, { tag: 'p', attrs: { className: 'stat-value' }, children: [s.value] }, { tag: 'span', attrs: { className: s.change >= 0 ? 'stat-up' : 'stat-down' }, children: [(s.change >= 0 ? '+' : '') + s.change + '%'] }] })) },\n      ],\n    }),\n  };\n}\n\nglobalThis.__reg['features_dashboard_DashboardPage'] = ${807 + FILE_ID};` },
        { name: 'index', deps: [], content: ctx => `export { fetchStats, fetchActivity, fetchChartData, fetchKPI } from './dashboardService.js';\nexport { getState, setState, subscribe, setDateRange } from './dashboardStore.js';\nexport { useDashboard } from './useDashboard.js';\nexport { StatsCard } from './StatsCard.js';\nexport { DataTable } from './DataTable.js';\nexport { ChartWidget } from './ChartWidget.js';\nexport { ActivityFeed } from './ActivityFeed.js';\nexport { DashboardPage } from './DashboardPage.js';\nglobalThis.__reg || (globalThis.__reg = {});\nglobalThis.__reg['features_dashboard_index'] = ${808 + FILE_ID};` },
      ],
    },
    settings: {
      label: 'Settings',
      files: [
        { name: 'settingsService', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport async function fetchSettings() {\n  return { profile: { name: 'John Doe', email: 'john@example.com', phone: '13800138000', avatar: null }, security: { twoFactor: false, lastLogin: '2026-07-24', devices: ['Chrome Windows', 'Safari iOS'] }, notifications: { email: true, push: true, sms: false, digest: 'daily' }, appearance: { theme: 'light', fontSize: 14, density: 'comfortable' } };\n}\n\nexport async function updateProfile(data) { return { ...data, updated: true }; }\nexport async function updateSecurity(data) { return { ...data, updated: true }; }\nexport async function updateNotifications(data) { return { ...data, updated: true }; }\nexport async function updateAppearance(data) { return { ...data, updated: true }; }\n\nglobalThis.__reg['features_settings_settingsService'] = ${900 + FILE_ID};` },
        { name: 'settingsStore', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nlet _settings = { profile: null, security: null, notifications: null, appearance: null };\nlet _loading = false;\nlet _saving = false;\nconst _listeners = new Set();\n\nexport function getSettings() { return { ..._settings }; }\nexport function updateSettings(partial) { _settings = { ..._settings, ...partial }; _listeners.forEach(fn => fn(_settings)); }\nexport function subscribe(fn) { _listeners.add(fn); return () => _listeners.delete(fn); }\nexport function setLoading(v) { _loading = v; }\nexport function setSaving(v) { _saving = v; }\nexport function isLoading() { return _loading; }\nexport function isSaving() { return _saving; }\n\nglobalThis.__reg['features_settings_settingsStore'] = ${901 + FILE_ID};` },
        { name: 'useSettings', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport function useSettings() {\n  return {\n    load: async () => {\n      const { fetchSettings } = await import('./settingsService.js');\n      const { updateSettings, setLoading } = await import('./settingsStore.js');\n      setLoading(true);\n      const data = await fetchSettings();\n      updateSettings(data);\n      setLoading(false);\n      return data;\n    },\n    save: async (section, data) => {\n      const { updateProfile, updateSecurity, updateNotifications, updateAppearance } = await import('./settingsService.js');\n      const { updateSettings, setSaving } = await import('./settingsStore.js');\n      setSaving(true);\n      const updaters = { profile: updateProfile, security: updateSecurity, notifications: updateNotifications, appearance: updateAppearance };\n      const result = await updaters[section](data);\n      updateSettings({ [section]: result });\n      setSaving(false);\n      return result;\n    },\n  };\n}\n\nglobalThis.__reg['features_settings_useSettings'] = ${902 + FILE_ID};` },
        { name: 'ProfileForm', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport function ProfileForm({ data, onSubmit }) {\n  return {\n    tag: 'form',\n    attrs: { className: 'profile-form', onSubmit: 'handleSubmit' },\n    children: [\n      { tag: 'div', attrs: { className: 'form-group' }, children: [{ tag: 'label', attrs: {}, children: ['姓名'] }, { tag: 'input', attrs: { type: 'text', name: 'name', defaultValue: data?.name || '', placeholder: '请输入姓名' } }] },\n      { tag: 'div', attrs: { className: 'form-group' }, children: [{ tag: 'label', attrs: {}, children: ['邮箱'] }, { tag: 'input', attrs: { type: 'email', name: 'email', defaultValue: data?.email || '', placeholder: '请输入邮箱' } }] },\n      { tag: 'div', attrs: { className: 'form-group' }, children: [{ tag: 'label', attrs: {}, children: ['手机号'] }, { tag: 'input', attrs: { type: 'tel', name: 'phone', defaultValue: data?.phone || '', placeholder: '请输入手机号' } }] },\n      { tag: 'button', attrs: { type: 'submit', className: 'btn btn-primary' }, children: ['保存'] },\n    ],\n  };\n}\n\nglobalThis.__reg['features_settings_ProfileForm'] = ${903 + FILE_ID};` },
        { name: 'SecurityForm', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport function SecurityForm({ data, onSubmit }) {\n  return {\n    tag: 'form',\n    attrs: { className: 'security-form' },\n    children: [\n      { tag: 'h3', attrs: {}, children: ['安全设置'] },\n      { tag: 'div', attrs: { className: 'form-group' }, children: [{ tag: 'label', attrs: {}, children: ['当前密码'] }, { tag: 'input', attrs: { type: 'password', name: 'currentPassword', placeholder: '输入当前密码' } }] },\n      { tag: 'div', attrs: { className: 'form-group' }, children: [{ tag: 'label', attrs: {}, children: ['新密码'] }, { tag: 'input', attrs: { type: 'password', name: 'newPassword', placeholder: '输入新密码' } }] },\n      { tag: 'div', attrs: { className: 'form-group' }, children: [{ tag: 'label', attrs: {}, children: ['确认密码'] }, { tag: 'input', attrs: { type: 'password', name: 'confirmPassword', placeholder: '再次输入新密码' } }] },\n      { tag: 'div', attrs: { className: 'form-group' }, children: [{ tag: 'label', attrs: {}, children: ['两步验证'] }, { tag: 'input', attrs: { type: 'checkbox', name: 'twoFactor', checked: data?.twoFactor || false } }] },\n      { tag: 'button', attrs: { type: 'submit', className: 'btn btn-primary' }, children: ['更新安全设置'] },\n    ],\n  };\n}\n\nglobalThis.__reg['features_settings_SecurityForm'] = ${904 + FILE_ID};` },
        { name: 'SettingsPage', deps: [], content: ctx => `globalThis.__reg || (globalThis.__reg = {});\n\nexport function SettingsPage() {\n  return {\n    render: () => ({\n      tag: 'div',\n      attrs: { className: 'settings-page' },\n      children: [\n        { tag: 'h2', attrs: {}, children: ['设置'] },\n        { tag: 'div', attrs: { className: 'settings-tabs' }, children: ['profile', 'security', 'notifications', 'appearance'].map(tab => ({ tag: 'button', attrs: { className: 'tab-item', 'data-tab': tab }, children: [{ tag: 'span', attrs: {}, children: [tab.charAt(0).toUpperCase() + tab.slice(1)] }] })) },\n        { tag: 'div', attrs: { className: 'settings-content' }, children: [{ tag: 'p', attrs: { className: 'text-muted' }, children: ['Select a tab to manage settings.'] }] },\n      ],\n    }),\n  };\n}\n\nglobalThis.__reg['features_settings_SettingsPage'] = ${905 + FILE_ID};` },
        { name: 'index', deps: [], content: ctx => `export { fetchSettings, updateProfile, updateSecurity, updateNotifications, updateAppearance } from './settingsService.js';\nexport { getSettings, updateSettings, subscribe } from './settingsStore.js';\nexport { useSettings } from './useSettings.js';\nexport { ProfileForm } from './ProfileForm.js';\nexport { SecurityForm } from './SecurityForm.js';\nexport { SettingsPage } from './SettingsPage.js';\nglobalThis.__reg || (globalThis.__reg = {});\nglobalThis.__reg['features_settings_index'] = ${906 + FILE_ID};` },
      ],
    },
  };

  const featureDirs = { auth: 'features/auth', dashboard: 'features/dashboard', settings: 'features/settings' };

  for (const [key, feature] of Object.entries(features)) {
    const dir = featureDirs[key];
    for (const file of feature.files) {
      FILE_ID++;
      const code = file.content(FILE_ID);
      const internalName = `features_${key}_${file.name}`;
      const mod = defineModule('features', internalName, 'medium', dir, [], code);
      // Use short file names (just the base name, no prefix) for realistic paths
      const shortPath = join(SRC, dir, file.name + '.js');
      mod.filePath = shortPath;
      mkdirSync(join(SRC, dir), { recursive: true });
      mod.generated = code;
      writeFileSync(shortPath, code, 'utf8');
    }
  }
}

// ====== UI COMPONENTS ======
function buildUIComponents() {
  const dir = 'components/ui';
  const components = [
    { name: 'Button', deps: ['constants/constants_theme'], sizes: ['small', 'medium', 'large'], variants: ['primary', 'secondary', 'danger', 'ghost', 'link'] },
    { name: 'Input', deps: [], variants: ['text', 'password', 'number', 'email', 'search', 'tel'] },
    { name: 'Select', deps: [], variants: ['single', 'multiple', 'searchable'] },
    { name: 'Checkbox', deps: [], variants: ['default', 'switch', 'button'] },
    { name: 'Radio', deps: [], variants: ['default', 'button', 'card'] },
    { name: 'Card', deps: [], variants: ['default', 'bordered', 'shadow', 'hoverable', 'clickable'] },
    { name: 'Modal', deps: [], variants: ['center', 'top', 'fullscreen', 'drawer'] },
    { name: 'Table', deps: [], variants: ['striped', 'bordered', 'compact', 'hoverable'] },
    { name: 'Tabs', deps: [], variants: ['line', 'card', 'pill', 'underline'] },
    { name: 'Badge', deps: [], variants: ['dot', 'count', 'text', 'status'] },
    { name: 'Alert', deps: [], variants: ['info', 'success', 'warning', 'error'] },
    { name: 'Tooltip', deps: [], variants: ['top', 'bottom', 'left', 'right'] },
    { name: 'Toast', deps: [], variants: ['success', 'error', 'warning', 'info', 'loading'] },
    { name: 'Spinner', deps: [], variants: ['small', 'medium', 'large', 'custom'] },
    { name: 'Pagination', deps: [], variants: ['simple', 'full', 'compact'] },
    { name: 'Breadcrumb', deps: [], variants: ['slash', 'arrow', 'bullet', 'dot'] },
    { name: 'Dropdown', deps: [], variants: ['click', 'hover', 'context'] },
    { name: 'Progress', deps: [], variants: ['linear', 'circle', 'steps'] },
    { name: 'Avatar', deps: [], variants: ['image', 'text', 'icon', 'group'] },
    { name: 'Tag', deps: [], variants: ['default', 'closable', 'addable', 'checkable'] },
  ];

  // Generate with more variants
  for (let ci = 0; ci < components.length; ci++) {
    const comp = components[ci];
    // Generate 6-12 variants per component
    const variantCount = randInt(6, 12);
    for (let vi = 0; vi < variantCount; vi++) {
      const variantName = comp.variants[vi % comp.variants.length];
      const sizeOption = comp.sizes ? comp.sizes[vi % comp.sizes.length] : 'medium';
      const fnName = comp.name.toLowerCase() + randInt(100, 999);
      let code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
      code += `export function ${fnName}({ children, className = '', ...props }) {\n`;
      code += `  const baseClass = '${comp.name.toLowerCase()}';\n`;
      code += `  const variantClass = baseClass + '--${variantName}';\n`;
      code += `  const sizeClass = baseClass + '--${sizeOption}';\n`;
      code += `  const cls = [baseClass, variantClass, sizeClass, className].filter(Boolean).join(' ');\n`;
      code += `  return { tag: 'div', attrs: { className: cls, ...props }, children };\n`;
      code += `}\n\n`;
      code += `export const ${fnName.toUpperCase()}_CONFIG = { name: '${comp.name}', variant: '${variantName}', size: '${sizeOption}' };\n\n`;
      code += `globalThis.__reg['ui_${fnName}'] = ${1000 + ALL_MODULES.length};\n`;
      write(defineModule('ui', `ui_${fnName}`, 'small', dir), code);
    }
  }

  // Also create some larger composite components
  for (let ci = 0; ci < 100; ci++) {
    const fnName = `composite_${String(ci + 1).padStart(3, '0')}`;
    let code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
    code += `export function ${fnName}({ data = [], config = {}, handlers = {}, children }) {\n`;
    code += `  const { title = '${fnName}', showHeader = true, showFooter = false, bordered = true, loading = false } = config;\n`;
    code += `  const { onClick, onDoubleClick, onContextMenu } = handlers;\n`;
    code += `  const items = Array.isArray(data) ? data : [data];\n`;
    code += `  return {\n`;
    code += `    tag: 'div',\n`;
    code += `    attrs: { className: 'composite ' + (bordered ? 'bordered' : '') + (loading ? ' loading' : '') },\n`;
    code += `    children: [\n`;
    code += `      showHeader ? { tag: 'div', attrs: { className: 'composite__header' }, children: [{ tag: 'h3', attrs: {}, children: [title] }] } : null,\n`;
    code += `      { tag: 'div', attrs: { className: 'composite__body' }, children: items.map((item, i) => ({ tag: 'div', attrs: { className: 'composite__item', onClick: onClick ? () => onClick(item, i) : undefined }, children: [String(item.name ?? item.id ?? i)] })) },\n`;
    code += `      showFooter ? { tag: 'div', attrs: { className: 'composite__footer' }, children: [{ tag: 'span', attrs: {}, children: [items.length + ' items'] }] } : null,\n`;
    code += `    ].filter(Boolean),\n`;
    code += `  };\n`;
    code += `}\n\n`;
    code += `globalThis.__reg['ui_${fnName}'] = ${2000 + ALL_MODULES.length};\n`;
    write(defineModule('ui', `ui_${fnName}`, 'medium', dir), code);
  }

  // Barrel
  const allUI = MOD_BY_CAT['ui'].filter(m => m.name !== 'index');
  let barrel = '';
  for (const m of allUI) {
    barrel += `export * from './${m.name}.js';\n`;
  }
  barrel += `\nglobalThis.__reg || (globalThis.__reg = {});\nglobalThis.__reg['ui_index'] = ${2999 + ALL_MODULES.length};\n`;
  write(defineModule('ui', 'index', 'tiny', dir), barrel);
}

// ====== LAYOUT ======
function buildLayout() {
  const dir = 'components/layout';
  const layouts = ['Container', 'Row', 'Col', 'Header', 'Sidebar', 'Footer', 'MainLayout', 'AuthLayout', 'EmptyLayout', 'AppShell',
    'PageHeader', 'PageFooter', 'PageSidebar', 'PageContent', 'Navbar', 'NavItem', 'NavGroup', 'BreadcrumbBar', 'ActionBar', 'StatusBar',
    'DashboardLayout', 'SettingsLayout', 'AdminLayout', 'ProfileLayout', 'SearchLayout',
    'GridLayout', 'FlexLayout', 'StackLayout', 'SplitLayout', 'PanelLayout',
    'Toolbar', 'SidePanel', 'QuickPanel', 'NotificationPanel', 'SearchPanel'];;

  for (let i = 0; i < layouts.length; i++) {
    const name = layouts[i];
    let code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
    code += `export function ${name}({ children, className = '', ...props }) {\n`;
    code += `  const cls = '${name.toLowerCase()}' + (className ? ' ' + className : '');\n`;
    code += `  return { tag: '${name === 'Row' ? 'div' : 'section'}', attrs: { className: cls, ...props }, children };\n`;
    code += `}\n\n`;
    if (i < 5) {
      code += `export function ${name}Header({ title, actions }) {\n  return { tag: 'header', attrs: { className: '${name.toLowerCase()}__header' }, children: [title ? { tag: 'h2', attrs: {}, children: [title] } : null, actions || null].filter(Boolean) };\n}\n\n`;
      code += `export function ${name}Body({ children }) {\n  return { tag: 'div', attrs: { className: '${name.toLowerCase()}__body' }, children };\n}\n\n`;
    }
    code += `globalThis.__reg['layout_${name}'] = ${3000 + ALL_MODULES.length};\n`;
    write(defineModule('layout', `layout_${name}`, 'medium', dir), code);
  }

  // Barrel
  const allLayout = MOD_BY_CAT['layout'].filter(m => m.name !== 'index');
  let barrel = '';
  for (const m of allLayout) {
    barrel += `export * from './${m.name}.js';\n`;
  }
  barrel += `\nglobalThis.__reg || (globalThis.__reg = {});\nglobalThis.__reg['layout_index'] = ${3099 + ALL_MODULES.length};\n`;
  write(defineModule('layout', 'index', 'tiny', dir), barrel);
}

// ====== PAGES (generated) ======
function buildPages() {
  const dir = 'pages';
  // 200 pages × 4 files = 800 files
  for (let i = 0; i < 200; i++) {
    const pageId = String(i + 1).padStart(3, '0');
    const name = `page_${pageId}`;

    // index.js
    const modName = `page_${pageId}`;
    let code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
    code += `export function ${modName}Page(ctx) {\n`;
    code += `  const { title = 'Page ${pageId}', params = {} } = ctx || {};\n`;
    code += `  return {\n`;
    code += `    id: '${modName}',\n`;
    code += `    title,\n`;
    code += `    params,\n`;
    code += `    render: () => ({\n`;
    code += `      tag: 'div',\n`;
    code += `      attrs: { className: 'page page--${modName}', 'data-page': '${modName}' },\n`;
    code += `      children: [\n`;
    code += `        { tag: 'header', attrs: { className: 'page__header' }, children: [{ tag: 'h1', attrs: {}, children: [title] }] },\n`;
    code += `        { tag: 'main', attrs: { className: 'page__content' }, children: [\n`;
    code += `          { tag: 'section', attrs: { className: 'page__section' }, children: [\n`;
    code += `            { tag: 'p', attrs: {}, children: ['This is page ${pageId}.'] },\n`;
    code += `            { tag: 'div', attrs: { className: 'page__actions' }, children: [\n`;
    code += `              { tag: 'button', attrs: { className: 'btn btn-primary' }, children: ['Action A'] },\n`;
    code += `              { tag: 'button', attrs: { className: 'btn btn-secondary' }, children: ['Action B'] },\n`;
    code += `            ]},\n`;
    code += `          ]},\n`;
    code += `        ]},\n`;
    code += `      ],\n`;
    code += `    }),\n`;
    code += `  };\n`;
    code += `}\n\n`;
    code += `globalThis.__reg['pages_${modName}_index'] = ${4000 + i * 4 + 0};\n`;
    write(defineModule('pages', `${modName}_index`, 'small', dir), code);

    // header.js
    code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
    code += `export function ${modName}Header({ title, subtitle, breadcrumb }) {\n`;
    code += `  const items = (breadcrumb || [{ label: 'Home', path: '/' }, { label: title || 'Page ${pageId}', path: '' }]);\n`;
    code += `  return {\n`;
    code += `    tag: 'header',\n`;
    code += `    attrs: { className: 'page-header page-header--${modName}' },\n`;
    code += `    children: [\n`;
    code += `      { tag: 'nav', attrs: { className: 'breadcrumb' }, children: items.map((item, i) => ({ tag: i < items.length - 1 ? 'a' : 'span', attrs: { href: item.path || undefined }, children: [item.label] })) },\n`;
    code += `      { tag: 'h1', attrs: { className: 'page-title' }, children: [title || 'Page ${pageId}'] },\n`;
    code += `      subtitle ? { tag: 'p', attrs: { className: 'page-subtitle' }, children: [subtitle] } : null,\n`;
    code += `    ].filter(Boolean),\n`;
    code += `  };\n`;
    code += `}\n\n`;
    code += `globalThis.__reg['pages_${modName}_header'] = ${4000 + i * 4 + 1};\n`;
    write(defineModule('pages', `${modName}_header`, 'small', dir), code);

    // content.js
    code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
    code += `export function ${modName}Content({ data, loading, error, empty }) {\n`;
    code += `  if (loading) return { tag: 'div', attrs: { className: 'loading' }, children: ['加载中...'] };\n`;
    code += `  if (error) return { tag: 'div', attrs: { className: 'error' }, children: ['加载失败: ' + (error.message || '未知错误')] };\n`;
    code += `  if (empty) return { tag: 'div', attrs: { className: 'empty' }, children: ['暂无数据'] };\n`;
    code += `  const items = data || [];\n`;
    code += `  return {\n`;
    code += `    tag: 'div',\n`;
    code += `    attrs: { className: 'page-content page-content--${modName}' },\n`;
    code += `    children: [\n`;
    code += `      { tag: 'div', attrs: { className: 'content-grid' }, children: items.length > 0 ? items.slice(0, 12).map((item, i) => ({\n`;
    code += `        tag: 'div',\n`;
    code += `        attrs: { className: 'content-card', 'data-index': i },\n`;
    code += `        children: [\n`;
    code += `          { tag: 'h3', attrs: {}, children: [item.title || 'Item ' + i] },\n`;
    code += `          { tag: 'p', attrs: {}, children: [item.description || 'Description for item ' + i] },\n`;
    code += `        ],\n`;
    code += `      })) : [{ tag: 'p', attrs: { className: 'text-muted' }, children: ['No content available.'] }] },\n`;
    code += `    ],\n`;
    code += `  };\n`;
    code += `}\n\n`;
    code += `globalThis.__reg['pages_${modName}_content'] = ${4000 + i * 4 + 2};\n`;
    write(defineModule('pages', `${modName}_content`, 'medium', dir), code);

    // sidebar.js
    code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
    code += `export function ${modName}Sidebar({ menu, activeKey }) {\n`;
    code += `  const items = menu || ['overview', 'details', 'history', 'settings', 'analytics'].map(s => ({ key: s, label: s.charAt(0).toUpperCase() + s.slice(1) }));\n`;
    code += `  return {\n`;
    code += `    tag: 'aside',\n`;
    code += `    attrs: { className: 'page-sidebar page-sidebar--${modName}' },\n`;
    code += `    children: [\n`;
    code += `      { tag: 'ul', attrs: { className: 'sidebar-menu' }, children: items.map(item => ({\n`;
    code += `        tag: 'li',\n`;
    code += `        attrs: { className: 'sidebar-menu__item' + (item.key === activeKey ? ' active' : '') },\n`;
    code += `        children: [{ tag: 'a', attrs: { href: '#' + item.key }, children: [item.label] }],\n`;
    code += `      })) },\n`;
    code += `    ],\n`;
    code += `  };\n`;
    code += `}\n\n`;
    code += `globalThis.__reg['pages_${modName}_sidebar'] = ${4000 + i * 4 + 3};\n`;
    write(defineModule('pages', `${modName}_sidebar`, 'small', dir), code);
  }

  // No barrel for pages (too many, main.js handles them)
}

// ====== SECTIONS ======
function buildSections() {
  const dir = 'sections';
  const sectionTypes = ['hero', 'features', 'gallery', 'pricing', 'faq', 'testimonials', 'stats', 'team', 'contact', 'cta',
    'banner', 'carousel', 'cards', 'list', 'grid', 'timeline', 'steps', 'comparison', 'sidebar', 'footer',
    'header', 'nav', 'tabbar', 'sidebar', 'content', 'form', 'table', 'chart', 'map', 'widget'];

  for (let si = 0; si < 100; si++) {
    const type = sectionTypes[si % sectionTypes.length];
    const sectionId = String(si + 1).padStart(3, '0');
    const fnName = `${type}_${sectionId}`;
    let code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
    code += `export function ${fnName}({ data = [], title, className = '', ...opts }) {\n`;
    code += `  const heading = title || '${type.charAt(0).toUpperCase() + type.slice(1)} ${sectionId}';\n`;
    code += `  const items = Array.isArray(data) ? data : [data];\n`;
    code += `  return {\n`;
    code += `    tag: 'section',\n`;
    code += `    attrs: { className: 'section section--${type} ' + className, 'data-section': '${fnName}' },\n`;
    code += `    children: [\n`;
    code += `      { tag: 'h2', attrs: { className: 'section__title' }, children: [heading] },\n`;
    code += `      { tag: 'div', attrs: { className: 'section__body' }, children: items.slice(0, 6).map((item, i) => ({\n`;
    code += `        tag: 'div',\n`;
    code += `        attrs: { className: 'section__item', key: i },\n`;
    code += `        children: [\n`;
    code += `          { tag: 'h3', attrs: {}, children: [item.title || 'Item ' + i] },\n`;
    code += `          { tag: 'p', attrs: {}, children: [item.description || ''] },\n`;
    code += `        ],\n`;
    code += `      })) },\n`;
    code += `    ],\n`;
    code += `  };\n`;
    code += `}\n\n`;
    code += `globalThis.__reg['sections_${fnName}'] = ${5000 + ALL_MODULES.length};\n`;
    write(defineModule('sections', `sections_${fnName}`, 'medium', dir), code);
  }

  // Barrel
  const allSections = MOD_BY_CAT['sections'].filter(m => m.name !== 'index');
  let barrel = '';
  for (const m of allSections) {
    barrel += `export * from './${m.name}.js';\n`;
  }
  barrel += `\nglobalThis.__reg || (globalThis.__reg = {});\nglobalThis.__reg['sections_index'] = ${5099 + ALL_MODULES.length};\n`;
  write(defineModule('sections', 'index', 'tiny', dir), barrel);
}

// ====== ROUTES (with dynamic imports) ======
function buildRoutes() {
  let code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
  code += `// 路由配置 — 使用动态 import() 实现懒加载\n`;
  code += `export const routes = [\n`;
  code += `  { path: '/', component: () => import('./features/dashboard/DashboardPage.js'), meta: { title: '首页', auth: true } },\n`;
  code += `  { path: '/login', component: () => import('./features/auth/LoginPage.js'), meta: { title: '登录', auth: false } },\n`;
  code += `  { path: '/register', component: () => import('./features/auth/RegisterPage.js'), meta: { title: '注册', auth: false } },\n`;
  code += `  { path: '/forgot-password', component: () => import('./features/auth/ForgotPasswordPage.js'), meta: { title: '重置密码', auth: false } },\n`;
  code += `  { path: '/dashboard', component: () => import('./features/dashboard/DashboardPage.js'), meta: { title: '仪表盘', auth: true } },\n`;
  code += `  { path: '/settings', component: () => import('./features/settings/SettingsPage.js'), meta: { title: '设置', auth: true } },\n`;
  code += `  { path: '/page/:id', component: () => import('./pages/page_001_index.js'), meta: { title: '页面', auth: true } },\n`;
  code += `];\n\n`;
  code += `export function matchRoute(path) {\n`;
  code += `  for (const route of routes) {\n`;
  code += `    if (route.path === path) return route;\n`;
  code += `    const paramMatch = path.match(new RegExp('^' + route.path.replace(/:([^/]+)/g, '([^/]+)') + '$'));\n`;
  code += `    if (paramMatch) return { ...route, params: paramMatch.slice(1) };\n`;
  code += `  }\n`;
  code += `  return null;\n`;
  code += `}\n\n`;
  code += `globalThis.__reg['routes'] = ${6000};\n`;
  write(defineModule('infra', 'routes', 'medium', '', []), code);
}

// ====== APP ======
function buildApp() {
  let code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
  code += `// App 入口 — 组合所有模块\n`;
  code += `export function createApp(config = {}) {\n`;
  code += `  return {\n`;
  code += `    config: { name: 'Enterprise App', version: '2.0', ...config },\n`;
  code += `    bootstrap: () => {\n`;
  code += `      const moduleCount = Object.keys(globalThis.__reg || {}).length;\n`;
  code += `      return { ready: true, modules: moduleCount, timestamp: Date.now() };\n`;
  code += `    },\n`;
  code += `    destroy: () => {\n`;
  code += `      // cleanup\n`;
  code += `    },\n`;
  code += `  };\n`;
  code += `}\n\n`;
  code += `globalThis.__reg['app'] = ${6001};\n`;
  write(defineModule('infra', 'app', 'small', '', []), code);
}

// ====== BULK FILL to ~2000 ======
function buildBulk() {
  const dir = 'data';
  // Add ~200 more data-like modules to fill to 2000
  for (let i = 0; i < 200; i++) {
    const bulkName = `bulk_data_${String(i + 1).padStart(3, '0')}`;
    let code = `globalThis.__reg || (globalThis.__reg = {});\n\n`;
    code += `export const ${bulkName.toUpperCase()}_CONFIG = { id: '${bulkName}', version: ${i}, active: true };\n\n`;
    const fields = randInt(3, 8);
    for (let f = 0; f < fields; f++) {
      code += `export function ${bulkName}_fn_${f}(x = 0) {\n  return { input: x, output: x * ${i + 1}, meta: { source: '${bulkName}', field: ${f} } };\n}\n\n`;
    }
    code += `globalThis.__reg['${bulkName}'] = ${7000 + i};\n`;
    write(defineModule('data', bulkName, 'small', dir), code);
  }
}

// ====== MAIN.js — imports ALL modules to trigger side effects ======
function buildMain() {
  let lines = ['// Application entry — imports all modules to trigger registration\n'];
  for (const mod of ALL_MODULES) {
    if (mod.cat === 'infra') continue;
    const fromDir = '';
    let rel = relative(join(SRC, ''), mod.filePath).replace(/\\/g, '/');
    if (!rel.startsWith('.')) rel = './' + rel;
    lines.push(`import '${rel}';`);
  }
  // Also import infra modules
  for (const mod of ALL_MODULES) {
    if (mod.cat !== 'infra') continue;
    const fromDir = '';
    let rel = relative(join(SRC, ''), mod.filePath).replace(/\\/g, '/');
    if (!rel.startsWith('.')) rel = './' + rel;
    lines.push(`import '${rel}';`);
  }
  writeFileSync(join(SRC, 'main.js'), lines.join('\n') + '\n', 'utf8');
}

// ====== ENTRY ======
function buildEntry() {
  const entryCode = [
    `import './src/main.js';`,
    ``,
    `const reg = globalThis.__reg || {};`,
    `const keys = Object.keys(reg).sort();`,
    `let hash = 0;`,
    `for (const k of keys) {`,
    `  const v = reg[k];`,
    `  hash = ((hash << 5) - hash + v + k.length) >>> 0;`,
    `}`,
    `console.log('modules=' + keys.length + ' hash=' + hash);`,
  ].join('\n');
  writeFileSync(join(INPUT, 'entry.js'), entryCode, 'utf8');
}

function buildPackageJson() {
  writeFileSync(join(INPUT, 'package.json'), JSON.stringify({
    type: 'module', private: true, sideEffects: true,
  }, null, 2) + '\n', 'utf8');
}

// ====== COMPUTE EXPECTED CHECKSUM ======
function computeExpectedChecksum() {
  const reg = {};
  for (const mod of ALL_MODULES) {
    // The code assigns different values
  }
  // Instead, re-compute by running the actual registration
  // We'll compute it differently: each file has a registration statement
  // that we can parse, but it's easier to just compute from the module IDs
  let hash = 0;
  const keys = [];
  for (const mod of ALL_MODULES) {
    // Each registration is like: globalThis.__reg['<name>'] = <value>;
    // The value is stored in mod.regValue
  }
  return { modules: ALL_MODULES.length, hash: 0 };
}

// ====== MAIN ======
function generate() {
  rmSync(SRC, { recursive: true, force: true });
  mkdirSync(SRC, { recursive: true });

  // Build in specific order to ensure layered deps
  buildRuntime();
  buildConstants();
  buildUtils();
  buildData();
  buildStores();
  buildHooks();
  buildServices();
  buildFeatures();
  buildUIComponents();
  buildLayout();
  buildSections();
  buildPages();
  buildRoutes();
  buildApp();
  buildBulk();
  buildMain();
  buildEntry();
  buildPackageJson();

  // Compute expected checksum from actual files on disk
  function walkDir(dir) {
    const r = [];
    for (const n of readdirSync(dir)) {
      const f = dir + '/' + n;
      const s = statSync(f);
      if (s.isDirectory()) r.push(...walkDir(f));
      else if (n.endsWith('.js') && n !== 'main.js') r.push(f);
    }
    return r;
  }
  const allFiles = walkDir(SRC);
  const reg = {};
  for (const f of allFiles) {
    const c = readFileSync(f, 'utf8');
    const m2 = c.match(/globalThis\.__reg\['?(\w+)'?\]\s*=\s*(\d+);/);
    if (m2) reg[m2[1]] = parseInt(m2[2]);
  }
  const expectedKeys = Object.keys(reg).sort();
  let expectedHash = 0;
  for (const k of expectedKeys) {
    expectedHash = ((expectedHash << 5) - expectedHash + reg[k] + k.length) >>> 0;
  }

  mkdirSync(join(__dirname, 'expected'), { recursive: true });
  writeFileSync(join(__dirname, 'expected', 'checksum.txt'),
    `modules=${expectedKeys.length} hash=${expectedHash}\n`, 'utf8');

  console.log(`Generated ${ALL_MODULES.length} modules (${expectedKeys.length} registered) in input/src/`);
  console.log(`Expected: modules=${expectedKeys.length} hash=${expectedHash}`);
}

generate();
