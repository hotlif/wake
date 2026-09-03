import assert from "node:assert/strict";
import test from "node:test";

import { createSearchIndex, searchDocs } from "./search.mjs";
import {
  canonicalRoutePath,
  docsRouteHref,
  findPageForPath,
  routePathFromDecoded,
  routePathFromLocation,
} from "./routes.mjs";

function page(overrides = {}) {
  return {
    id: "index",
    file: "docs/index.mdx",
    title: "Wake",
    description: "Rust-native tooling",
    slug: "/",
    group: "Start",
    section: "",
    hidden: false,
    headings: [],
    searchText: "",
    ...overrides,
  };
}

test("search keeps CLI flags such as --minify searchable and trims the query", () => {
  const index = createSearchIndex([
    page({ searchText: "wake build --minify for production" }),
  ]);

  assert.deepEqual(searchDocs(index, "  \n--minify\t  ").map((item) => item.slug), ["/"]);
});

test("a separately loaded corpus makes body text searchable without bloating page metadata", () => {
  const metadata = page({ searchText: undefined });
  const index = createSearchIndex([metadata], {}, {}, { index: "public_path --minify" });

  assert.deepEqual(searchDocs(index, "public_path").map((item) => item.slug), ["/"]);
});

test("search applies NFKC normalization and lowercase matching", () => {
  const index = createSearchIndex([page({ title: "Wake CLI" })]);

  assert.deepEqual(searchDocs(index, "ＷＡＫＥ").map((item) => item.slug), ["/"]);
});

test("multiple query words use AND matching across fields", () => {
  const index = createSearchIndex([
    page({ title: "Fast compiler", searchText: "Implemented in Rust" }),
    page({
      title: "Fast preview",
      description: "Browser tooling",
      slug: "/preview",
      searchText: "Browser runtime",
    }),
  ]);

  assert.deepEqual(searchDocs(index, "fast   rust").map((item) => item.slug), ["/"]);
  assert.deepEqual(searchDocs(index, "fast missing"), []);
});

test("stable weighted ranking prefers exact title, then prefix, then body", () => {
  const index = createSearchIndex([
    page({ title: "Guide A", slug: "/body-a", searchText: "minify output" }),
    page({ title: "Minify options", slug: "/prefix" }),
    page({ title: "Minify", slug: "/exact" }),
    page({ title: "Guide B", slug: "/body-b", searchText: "minify output" }),
  ]);

  assert.deepEqual(searchDocs(index, "minify").map((item) => item.slug), [
    "/exact",
    "/prefix",
    "/body-a",
    "/body-b",
  ]);
});

test("hidden pages, headings, and props never enter the index", () => {
  const hidden = page({
    file: "docs/hidden.mdx",
    title: "Secret",
    slug: "/secret",
    hidden: true,
    headings: [{ depth: 2, title: "Hidden heading", id: "hidden-heading" }],
    searchText: "private marker",
  });
  const apiDocs = {
    "docs/hidden.mdx|../src/secret.ts|SecretProps": {
      symbol: "SecretProps",
      props: [{ name: "secretProp", description: "private marker", type_text: "string" }],
    },
  };
  const index = createSearchIndex([hidden], apiDocs);

  assert.deepEqual(searchDocs(index, "secret"), []);
  assert.deepEqual(searchDocs(index, ""), []);
});

test("blank queries suggest visible pages only in source order", () => {
  const pages = Array.from({ length: 10 }, (_, index) => page({
    title: `Page ${index}`,
    slug: `/page-${index}`,
    headings: [{ depth: 2, title: `Heading ${index}`, id: `heading-${index}` }],
  }));
  const index = createSearchIndex(pages);
  const results = searchDocs(index, "  \t ");

  assert.equal(results.length, 8);
  assert.ok(results.every((item) => item.type === "page"));
  assert.deepEqual(results.map((item) => item.slug), pages.slice(0, 8).map((item) => item.slug));
});

test("API prop results remain searchable with localized labels", () => {
  const apiDocs = {
    "docs/index.mdx|../src/button.tsx|ButtonProps": {
      symbol: "ButtonProps",
      props: [{ name: "disabled", description: "Disables interaction", type_text: "boolean" }],
    },
  };
  const index = createSearchIndex([page()], apiDocs, { section: "章节", prop: "属性" });

  assert.deepEqual(searchDocs(index, "disabled"), [{
    title: "disabled",
    detail: "Wake",
    slug: "/#api-ButtonProps",
    kind: "属性",
    type: "prop",
  }]);
});

test("route identity is canonical across direct refresh and client navigation", () => {
  const encoded = "/100%25%20%23%20%E4%B8%AD%E6%96%87";
  const routePath = { decoded: "/100% # 中文", encoded };
  const routePage = page({ slug: encoded, routePath });

  assert.deepEqual(canonicalRoutePath("/100%25%20%23%20%e4%b8%ad%e6%96%87/"), routePath);
  assert.deepEqual(routePathFromDecoded("/100% # 中文"), routePath);
  assert.equal(routePathFromLocation("/docs/", "/docs").encoded, "/");
  assert.deepEqual(routePathFromLocation("/docs/", "/docs" + encoded), routePath);
  assert.equal(docsRouteHref("/docs/", encoded), "/docs" + encoded);
  assert.equal(docsRouteHref("/docs/", "/"), "/docs/");
  assert.equal(findPageForPath([routePage], encoded), routePage);
  assert.equal(findPageForPath([routePage], "/100%25%20%23%20%e4%b8%ad%e6%96%87"), routePage);

  assert.equal(canonicalRoutePath("/%"), null);
  assert.equal(canonicalRoutePath("/%2F"), null);
  assert.equal(canonicalRoutePath("/%5C"), null);
  assert.equal(routePathFromDecoded("/bad\\name"), null);
  assert.equal(canonicalRoutePath("/100%2525").encoded, "/100%2525");
});
