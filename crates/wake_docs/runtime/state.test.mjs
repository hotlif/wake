import { test, expect } from '@crab-dev/wake/test';
import { navigationGroups, pageInfo } from './state.mjs';

const pages = [
  { slug: '/', title: 'Home', group_id: 'g', group: 'Guide', section_id: '', headings: [] },
  { slug: '/a', title: 'A', group_id: 'g', group: 'Guide', section_id: 's', section: 'Start', headings: [{ id: 'one', title: 'One', depth: 2 }], load() {}, file: 'private/file' },
  { slug: '/b', title: 'B', group_id: 'g', group: 'Guide', section_id: 't', section: 'More', headings: [] },
  { slug: '/hidden', title: 'Hidden', hidden: true, group_id: 'g', section_id: 's' },
];
const href = slug => '/docs' + slug;
test('active navigation is derived without persisting it as a user preference', () => {
  const preference = new Set(['g/t']);
  const first = navigationGroups(pages, preference, '/a', href);
  expect(first[0].sections.map(section => section.expanded)).toEqual([true, true]);
  expect(first[0].sections[0].pages.map(page => page.slug)).toEqual(['/a']);
  const second = navigationGroups(pages, preference, '/b', href);
  expect(second[0].sections.map(section => section.expanded)).toEqual([false, true]);
  expect([...preference]).toEqual(['g/t']);
});
test('public page metadata has deploy-aware URLs and excludes loaders and source paths', () => {
  const info = pageInfo(pages[1], href);
  expect(info.href).toBe('/docs/a');
  expect(info.headings[0].href).toBe('/docs/a#one');
  expect(info.file).toBeUndefined();
  expect(info.load).toBeUndefined();
});
