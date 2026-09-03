import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { runInNewContext } from 'node:vm'

const WORKBENCH_LUCIDE_ICONS = [
  'Check',
  'Code2',
  'Copy',
  'Menu',
  'Monitor',
  'Moon',
  'RotateCcw',
  'SlidersHorizontal',
  'Sun',
]

function factorySource(factory) {
  return Function.prototype.toString.call(factory)
}

function escapeRegExp(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

function factoryBindings(factory) {
  const match = factorySource(factory).match(
    /^function(?:\s+[$\w]+)?\s*\(\s*([$\w]+)\s*,\s*([$\w]+)\s*,\s*([$\w]+)\s*\)/,
  )
  return match ? { exportsName: match[2], requireName: match[3] } : undefined
}

function exposeRuntime(source) {
  const legacyMarker = 'var __wake_entry__ = __wake__.require(0);'
  if (source.includes(legacyMarker)) {
    assert.equal(
      source.split(legacyMarker).length - 1,
      1,
      'Components entry must contain exactly one Wake entry bootstrap',
    )
    return source.replace(
      legacyMarker,
      'g.__wake_runtime_smoke__ = __wake__; var __wake_entry__ = {};',
    )
  }

  const compactBootstrap = /([$\w]+)\.m=([$\w]+);\1\.c=([$\w]+);var ([$\w]+)=\1\(0\);/g
  const matches = [...source.matchAll(compactBootstrap)]
  assert.equal(
    matches.length,
    1,
    'Components entry must contain exactly one Wake entry bootstrap',
  )
  const [marker, requireName, modulesName, cacheName, entryName] = matches[0]
  return source.replace(
    marker,
    `${requireName}.m=${modulesName};${requireName}.c=${cacheName};` +
      `g.__wake_runtime_smoke__={m:${modulesName},require:${requireName}};var ${entryName}={};`,
  )
}

function findSingleModule(runtime, predicate, label) {
  const matches = Object.entries(runtime.m).filter(([, factory]) =>
    predicate(factorySource(factory)),
  )
  assert.equal(
    matches.length,
    1,
    `Components runtime smoke expected one ${label} module, found ${matches.length}`,
  )
  return matches[0]
}

function isComponent(value) {
  return value !== null && (typeof value === 'function' || typeof value === 'object')
}

export async function assertComponentsRuntime(entryPath) {
  const source = await readFile(entryPath, 'utf8')
  const smokeSource = exposeRuntime(source)
  // Some component dependencies probe browser globals while their module factory initializes.
  // The smoke only inspects exports, so a minimal self-referential window/document is sufficient
  // and keeps the check independent from a full DOM implementation.
  const document = {}
  const window = { document }
  window.window = window
  const context = { document, window, queueMicrotask() {} }
  runInNewContext(smokeSource, context, {
    filename: entryPath,
    timeout: 10_000,
  })

  const runtime = context.__wake_runtime_smoke__
  assert.ok(runtime?.m && typeof runtime.require === 'function', 'Wake runtime was not registered')

  const [iconModuleId] = findSingleModule(
    runtime,
    (factory) =>
      /(?:createLucideIcon|\(\s*0\s*,\s*[$\w]+\.default\s*\))\(\s*["']sliders-horizontal["']/.test(
        factory,
      ),
    'SlidersHorizontal icon',
  )
  const iconModule = runtime.require(Number(iconModuleId))
  assert.ok(
    isComponent(iconModule.default),
    'SlidersHorizontal module must preserve its default React component export',
  )

  const barrelCandidates = Object.entries(runtime.m).filter(([, factory]) => {
    const source = factorySource(factory)
    const bindings = factoryBindings(factory)
    if (!bindings) return false
    const requiredIcon = new RegExp(
      `${escapeRegExp(bindings.requireName)}\\(\\s*${iconModuleId}\\s*\\)`,
    )
    const exportedIcon = new RegExp(
      `(?:${escapeRegExp(bindings.exportsName)}\\.SlidersHorizontal\\s*=` +
        `|Object\\.defineProperty\\(\\s*${escapeRegExp(bindings.exportsName)}\\s*,` +
        `|${escapeRegExp(bindings.requireName)}\\.objectDefineProperty\\(\\s*` +
        `${escapeRegExp(bindings.exportsName)}\\s*,` +
        `\\s*["']SlidersHorizontal["'])`,
    )
    return requiredIcon.test(source) && exportedIcon.test(source)
  })
  assert.ok(barrelCandidates.length > 0, 'Components runtime smoke found no Lucide barrel')
  const matchingBarrels = barrelCandidates
    .map(([moduleId]) => runtime.require(Number(moduleId)))
    .filter((candidate) => WORKBENCH_LUCIDE_ICONS.every((icon) => isComponent(candidate[icon])))
  assert.ok(
    matchingBarrels.length > 0,
    'No Lucide barrel exposes every icon used by the Components workbench',
  )
  const lucide = matchingBarrels[0]
  for (const icon of WORKBENCH_LUCIDE_ICONS) {
    assert.ok(
      isComponent(lucide[icon]),
      `Lucide barrel must expose a non-null ${icon} React component`,
    )
  }
}
