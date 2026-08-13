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
  const bootMarker = 'var __wake_entry__ = __wake__.require(0);'
  assert.equal(
    source.split(bootMarker).length - 1,
    1,
    'Components entry must contain exactly one Wake entry bootstrap',
  )

  const smokeSource = source.replace(
    bootMarker,
    'g.__wake_runtime_smoke__ = __wake__; var __wake_entry__ = {};',
  )
  const context = {}
  runInNewContext(smokeSource, context, {
    filename: entryPath,
    timeout: 10_000,
  })

  const runtime = context.__wake_runtime_smoke__
  assert.ok(runtime?.m && typeof runtime.require === 'function', 'Wake runtime was not registered')

  const [iconModuleId] = findSingleModule(
    runtime,
    (factory) => /createLucideIcon\(["']sliders-horizontal["']/.test(factory),
    'SlidersHorizontal icon',
  )
  const iconModule = runtime.require(Number(iconModuleId))
  assert.ok(
    isComponent(iconModule.default),
    'SlidersHorizontal module must preserve its default React component export',
  )

  const requiredIcon = `__wake_require__(${iconModuleId})`
  const barrelCandidates = Object.entries(runtime.m).filter(([, factory]) => {
    const source = factorySource(factory)
    return source.includes(requiredIcon) && source.includes('exports.SlidersHorizontal=')
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
