import { test, expect } from '@crab-dev/wake/test'
import { isSlidersHorizontalFactory } from './components-runtime-smoke.mjs'

test('Components smoke accepts positional Lucide factories', () => {
  expect(isSlidersHorizontalFactory('const icon = createLucideIcon("sliders-horizontal", []);')).toBe(true)
  expect(isSlidersHorizontalFactory('const icon = (0,a.default)("sliders-horizontal", []);')).toBe(true)
})

test('Components smoke accepts metadata Lucide factories', () => {
  expect(isSlidersHorizontalFactory('const data = {name:"sliders-horizontal",size:24,node:[]}; const icon = createLucideIcon(data);')).toBe(true)
  expect(isSlidersHorizontalFactory('const data = {"name": "sliders-horizontal",size:24,node:[]}; const icon = (0,a.default)(data);')).toBe(true)
})

test('Components smoke rejects other icons and unrelated string literals', () => {
  expect(isSlidersHorizontalFactory('const icon = createLucideIcon("other-icon", []);')).toBe(false)
  expect(isSlidersHorizontalFactory('const data = {name:"other-icon"}; const icon = createLucideIcon(data);')).toBe(false)
  expect(isSlidersHorizontalFactory('const label = "sliders-horizontal";')).toBe(false)
  expect(isSlidersHorizontalFactory('const data = {name:"sliders-horizontal"};')).toBe(false)
})
