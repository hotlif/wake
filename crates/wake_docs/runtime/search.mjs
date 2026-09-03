const DEFAULT_LABELS = Object.freeze({
  section: "Section",
  prop: "Prop",
});

function normalizeSearchText(value) {
  return String(value ?? "")
    .normalize("NFKC")
    .trim()
    .toLowerCase()
    .replace(/\s+/gu, " ");
}

function resultLimit(value, fallback) {
  if (value === undefined) return fallback;
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed >= 0 ? Math.floor(parsed) : fallback;
}

function locationFor(page) {
  return [page.group, page.section].filter(Boolean).join(" / ");
}

function indexedEntry(result, body, order) {
  const normalized = {
    title: normalizeSearchText(result.title),
    detail: normalizeSearchText(result.detail),
    kind: normalizeSearchText(result.kind),
    body: normalizeSearchText(body),
  };
  return {
    result,
    normalized,
    searchable: Object.values(normalized).filter(Boolean).join(" "),
    order,
  };
}

/**
 * Build an immutable-by-convention search index from Wake's generated registry.
 * The returned value contains no browser state and can be reused across queries.
 */
export function createSearchIndex(pages, apiDocs = {}, labels = {}, searchTextByPage = {}) {
  const entries = [];
  const suggestions = [];
  const sectionLabel = String(labels.section || DEFAULT_LABELS.section);
  const propLabel = String(labels.prop || DEFAULT_LABELS.prop);
  const docs = apiDocs && typeof apiDocs === "object" ? Object.entries(apiDocs) : [];
  let order = 0;

  for (const page of Array.isArray(pages) ? pages : []) {
    if (!page || page.hidden) continue;

    const location = locationFor(page);
    const pageResult = {
      title: String(page.title || ""),
      detail: String(page.description || ""),
      slug: String(page.slug || ""),
      kind: location,
      type: "page",
    };
    const body = Object.prototype.hasOwnProperty.call(searchTextByPage, page.id)
      ? searchTextByPage[page.id]
      : page.searchText;
    const pageEntry = indexedEntry(pageResult, body, order++);
    entries.push(pageEntry);
    suggestions.push(pageResult);

    for (const heading of Array.isArray(page.headings) ? page.headings : []) {
      if (!heading || Number(heading.depth) <= 1) continue;
      const detail = [page.title, location].filter(Boolean).join(" · ");
      const result = {
        title: String(heading.title || ""),
        detail,
        slug: `${pageResult.slug}#${String(heading.id || "")}`,
        kind: sectionLabel,
        type: "heading",
      };
      entries.push(indexedEntry(result, "", order++));
    }

    const apiPrefix = `${String(page.file || "")}|`;
    for (const [key, doc] of docs) {
      if (!apiPrefix || !key.startsWith(apiPrefix) || !doc) continue;
      const symbol = String(doc.symbol || "");
      for (const prop of Array.isArray(doc.props) ? doc.props : []) {
        if (!prop) continue;
        const result = {
          title: String(prop.name || ""),
          detail: pageResult.title,
          slug: `${pageResult.slug}#api-${symbol}`,
          kind: propLabel,
          type: "prop",
        };
        const body = [prop.description, prop.type_text, prop.default_value]
          .filter(Boolean)
          .join(" ");
        entries.push(indexedEntry(result, body, order++));
      }
    }
  }

  return { entries, suggestions };
}

function fieldScore(value, term, weights) {
  if (!value.includes(term)) return -1;
  if (value === term) return weights.exact;
  if (value.startsWith(term)) return weights.prefix;
  if (value.includes(` ${term}`)) return weights.word;
  return weights.contains;
}

function rankEntry(entry, query, terms) {
  const { title, detail, kind, body } = entry.normalized;
  let score = 0;

  if (title === query) score += 1_000;
  else if (title.startsWith(query)) score += 700;
  else if (title.includes(query)) score += 500;
  else if (detail.includes(query)) score += 100;
  else if (body.includes(query)) score += 25;

  for (const term of terms) {
    if (!entry.searchable.includes(term)) return -1;
    score += Math.max(
      fieldScore(title, term, { exact: 320, prefix: 240, word: 180, contains: 140 }),
      fieldScore(detail, term, { exact: 90, prefix: 70, word: 55, contains: 40 }),
      fieldScore(kind, term, { exact: 50, prefix: 35, word: 25, contains: 20 }),
      fieldScore(body, term, { exact: 16, prefix: 12, word: 8, contains: 4 }),
    );
  }
  return score;
}

/** Search a prebuilt index. Blank queries return page-only suggestions. */
export function searchDocs(index, query, limit) {
  const normalizedQuery = normalizeSearchText(query);
  if (!normalizedQuery) {
    return (index?.suggestions || []).slice(0, resultLimit(limit, 8));
  }

  const terms = [...new Set(normalizedQuery.split(" "))];
  const ranked = [];
  for (const entry of index?.entries || []) {
    const score = rankEntry(entry, normalizedQuery, terms);
    if (score >= 0) ranked.push({ entry, score });
  }
  ranked.sort((left, right) => right.score - left.score || left.entry.order - right.entry.order);
  return ranked
    .slice(0, resultLimit(limit, 12))
    .map(({ entry }) => entry.result);
}
