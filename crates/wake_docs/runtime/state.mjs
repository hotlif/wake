export function pageLink(page, href) {
  return { slug: page.slug, title: page.title, href: href(page.slug) };
}
export function pageInfo(page, href) {
  if (!page) return null;
  return {
    ...pageLink(page, href), description: page.description || '', status: page.status || 'stable',
    draft: Boolean(page.draft),
    headings: (page.headings || []).map(({ id, title, depth }) => ({ id, title, depth, href: href(page.slug) + '#' + encodeURIComponent(id) })),
  };
}
export function navigationGroups(pages, expanded, current, href) {
  const groups = [];
  for (const page of pages) {
    if (page.hidden) continue;
    let group = groups.find(item => item.id === page.group_id);
    if (!group) groups.push(group = { id: page.group_id, title: page.group, pages: [], sections: [] });
    if (!page.section_id) group.pages.push(pageLink(page, href));
    else {
      const id = page.group_id + '/' + page.section_id;
      let section = group.sections.find(item => item.id === id);
      if (!section) group.sections.push(section = { id, title: page.section, pages: [], expanded: expanded.has(id), active: false });
      section.pages.push(pageLink(page, href));
      if (page.slug === current) { section.active = true; section.expanded = true; }
    }
  }
  return groups;
}
