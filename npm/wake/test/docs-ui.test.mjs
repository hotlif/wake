import { test, expect } from '@crab-dev/wake/test';
import { defineDocsUI } from '../docs.mjs';

test('UI registration preserves component identity and only defaults omitted slots', () => {
  const Header = () => null;
  const ui = defineDocsUI({ Header });
  expect(ui.Header).toBe(Header);
  expect(ui.Layout).toBeUndefined();
  expect(Object.isFrozen(ui)).toBe(true);
});

test('invalid UI registrations fail explicitly', () => {
  for (const value of [null, [], true, { Unknown: () => null }, { Header: null }, { Header: 42 }, { Header: 'header' }]) {
    expect(() => defineDocsUI(value)).toThrow(/Wake Docs UI/);
  }
  expect(() => defineDocsUI({ get Header() { throw new Error('getter executed'); } })).toThrow(/Wake Docs UI/);
});

test('React memo, forwardRef and lazy components are valid registrations', () => {
  for (const kind of ['react.memo', 'react.forward_ref', 'react.lazy']) {
    const component = { $$typeof: Symbol.for(kind) };
    expect(defineDocsUI({ Header: component }).Header).toBe(component);
  }
});
