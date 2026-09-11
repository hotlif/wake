import { DefaultRoot, DefaultLayout, DefaultHeader, DefaultNavigation, DefaultMobileNavigation, DefaultSearchDialog, DefaultTableOfContents, DefaultPage, DefaultDemo, DefaultCodeBlock, DefaultApiTable, DefaultPageLoading, DefaultPageError, DefaultNotFound } from "./defaults.tsx";
import './content.css';
import type * as UI from "@crab-dev/wake/docs";
import { Slot, UIProvider, UIErrorBoundary, useCommon } from "./ui.tsx";
import { navigationGroups, pageInfo, pageLink } from "./state.mjs";
import React, { Suspense, startTransition, useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { apiDocs, demos, pages, developmentDemand } from "@@wake/docs/registry.ts";
import { Preview, siteConfig, resolveUI } from "@@wake/docs/config.tsx";
import { docsRouteHref, findPageForPath, routePathFromLocation } from "./routes.mjs";
import { createSearchIndex, searchDocs } from "./search.mjs";

type Theme = "light" | "dark" | "system";
type ResolvedTheme = "light" | "dark";
type DemoRecord = (typeof demos)[number];
type PageRecord = (typeof pages)[number];
type ViewportPreset = "responsive" | "tablet" | "mobile";

const isChinese = siteConfig.locale.toLowerCase().startsWith("zh");
const text = (english: string, chinese: string) => isChinese ? chinese : english;
const pageLoads = new Map<string, Promise<any>>();
const lazyPages = new Map<string, React.LazyExoticComponent<React.ComponentType>>();

function loadPage(page: PageRecord): Promise<any> {
  const cached = pageLoads.get(page.slug);
  if (cached) return cached;
  const pending = page.load().catch((reason) => {
    pageLoads.delete(page.slug);
    lazyPages.delete(page.slug);
    throw reason;
  });
  pageLoads.set(page.slug, pending);
  return pending;
}

function lazyPage(page: PageRecord) {
  const cached = lazyPages.get(page.slug);
  if (cached) return cached;
  const component = React.lazy(() => loadPage(page));
  lazyPages.set(page.slug, component);
  return component;
}

function docsHref(slug: string): string {
  return docsRouteHref(siteConfig.basePath, slug) || "/";
}

function pageForPath(pathname: string): PageRecord | undefined {
  return findPageForPath(pages, pathname);
}

function internalPageLink(anchor: HTMLAnchorElement): { page: PageRecord; slug: string } | null {
  if (anchor.hasAttribute("download") || (anchor.target && anchor.target !== "_self")) return null;
  const url = new URL(anchor.href, window.location.href);
  if (url.origin !== window.location.origin || url.search) return null;
  const routePath = routePathFromLocation(siteConfig.basePath, url.pathname);
  if (!routePath) return null;
  const page = pageForPath(routePath.encoded);
  return page ? { page, slug: page.slug + url.hash } : null;
}



const defaultMeta = {
  title: text("Demo", "演示"),
  description: "",
  height: "auto",
  viewport: "responsive",
  background: "surface",
  padding: "md",
  isolation: "iframe",
};

function normalizePath(value: string): string {
  const result: string[] = [];
  value.replace(/\\/g, "/").split("/").forEach((part) => {
    if (!part || part === ".") return;
    if (part === "..") result.pop();
    else result.push(part);
  });
  return result.join("/");
}

function resolveFromPage(pageFile: string, value: string): string {
  const base = pageFile.split("/").slice(0, -1).join("/");
  return normalizePath(base + "/" + value);
}

function wildcardMatch(value: string, pattern: string): boolean {
  const escaped = pattern.replace(/[.+^$(){}|[\]\\]/g, "\\$&").replace(/\*\*/g, "__WAKE_GLOBSTAR__").replace(/\*/g, "[^/]*").replace(/__WAKE_GLOBSTAR__/g, ".*");
  return new RegExp("^" + escaped + "$").test(value);
}

function resolvedTheme(theme: Theme): ResolvedTheme {
  if (theme !== "system") return theme;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

function useTheme() {
  const [theme, setTheme] = useState<Theme>(() => {
    try {
      const saved = localStorage.getItem("wake-docs-theme");
      if (saved === "light" || saved === "dark" || saved === "system") return saved;
    } catch {
      // Storage can be disabled by browser privacy policies. The configured theme remains usable.
    }
    return siteConfig.defaultTheme as Theme;
  });
  const [resolved, setResolved] = useState<ResolvedTheme>(() => resolvedTheme(theme));
  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const update = () => setResolved(resolvedTheme(theme));
    update();
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, [theme]);
  useEffect(() => {
    try { localStorage.setItem("wake-docs-theme", theme); } catch { /* Keep theme changes session-local. */ }
    document.documentElement.lang = siteConfig.locale;
    document.documentElement.dataset.theme = resolved;

    document.querySelectorAll("iframe[data-wake-demo]").forEach((frame) => {
      (frame as HTMLIFrameElement).contentWindow?.postMessage({ type: "wake:theme", theme: resolved }, "*");
    });
  }, [theme, resolved]);
  return { theme, resolved, setTheme };
}

function appPath(): string {
  const routePath = routePathFromLocation(siteConfig.basePath, window.location.pathname);
  return (routePath?.encoded || "/__wake-invalid-route__") + window.location.hash;
}



function pageTitle(title: string): string {
  return !title || title === siteConfig.title ? siteConfig.title : title + " · " + siteConfig.title;
}

function updateDocumentMetadata(title: string, description: string) {
  document.title = title;
  let metadata = document.querySelector<HTMLMetaElement>('meta[name="description"]');
  if (!metadata) {
    metadata = document.createElement("meta");
    metadata.name = "description";
    document.head.appendChild(metadata);
  }
  metadata.content = description;
}

function copyText(value: string): Promise<void> {
  if (navigator.clipboard?.writeText) return navigator.clipboard.writeText(value);
  const textarea = document.createElement("textarea");
  textarea.value = value;
  textarea.setAttribute("readonly", "");
  textarea.style.position = "fixed";
  textarea.style.opacity = "0";
  document.body.appendChild(textarea);
  textarea.select();
  try {
    return document.execCommand("copy") ? Promise.resolve() : Promise.reject(new Error("copy unavailable"));
  } catch (reason) {
    return Promise.reject(reason);
  } finally {
    textarea.remove();
  }
}



export function CodeBlock({ language, code, title, children }: { language: string; code: string; title?: string; children: React.ReactNode }) {
  const [copyStatus, setCopyStatus] = useState<"idle" | "copied" | "error">("idle");
  const copy = () => copyText(code).then(() => {
    setCopyStatus("copied");
    window.setTimeout(() => setCopyStatus("idle"), 1600);
  }).catch(() => {
    setCopyStatus("error");
    window.setTimeout(() => setCopyStatus("idle"), 1600);
  });
  return <Slot name="CodeBlock" fallback={DefaultCodeBlock} {...{ language, code, title, children, copyStatus, copy }} />;
}



export function MdxPage({ meta, children }: { meta: PageRecord; children: React.ReactNode }) {
  useEffect(() => {
    updateDocumentMetadata(pageTitle(meta.title), meta.description || siteConfig.description);
    const ready = () => window.dispatchEvent(new CustomEvent("wake:page-ready", { detail: { slug: meta.slug, title: meta.title } }));
    ready();
    const frame = requestAnimationFrame(ready);
    return () => cancelAnimationFrame(frame);
  }, [meta.slug, meta.title, meta.description]);
  const crumbs = [meta.group, meta.section, meta.title]
    .filter(Boolean)
    .filter((crumb, index, values) => values.indexOf(crumb) === index);
  const visiblePages = pages.filter((page) => !page.hidden);
  const index = visiblePages.findIndex((page) => page.slug === meta.slug);
  return <Slot name="Page" fallback={DefaultPage} page={pageInfo(meta, docsHref)} breadcrumbs={crumbs}
    previous={index > 0 ? pageLink(visiblePages[index - 1], docsHref) : null}
    next={index >= 0 && index + 1 < visiblePages.length ? pageLink(visiblePages[index + 1], docsHref) : null}>{children}</Slot>;
}

function useVisible<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [visible, setVisible] = useState(false);
  useEffect(() => {
    if (!ref.current || visible) return;
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        setVisible(true);
        observer.disconnect();
      }
    }, { rootMargin: "300px" });
    observer.observe(ref.current);
    return () => observer.disconnect();
  }, [visible]);
  return { ref, visible };
}



function DemoCard({ demo }: { demo: DemoRecord }) {
  const { ref, visible } = useVisible<HTMLDivElement>();
  const [codeOpen, setCodeOpen] = useState(false);
  const [height, setHeight] = useState(220);
  const [viewport, setViewport] = useState<ViewportPreset>("responsive");
  const [error, setError] = useState("");
  const [meta, setMeta] = useState(defaultMeta);
  const [source, setSource] = useState("");
  const [highlightedSource, setHighlightedSource] = useState<React.ReactNode>(null);
  const [sourceLanguage, setSourceLanguage] = useState("tsx");
  const [fullscreen, setFullscreen] = useState(false);
  const [copied, setCopied] = useState(false);
  const frameRef = useRef<HTMLIFrameElement>(null);

  useEffect(() => {
    if (!visible) return;
    Promise.all([demo.load(), demo.loadSource()]).then(([module, sourceModule]) => {
      const nextMeta = { ...defaultMeta, ...(module.meta || {}) };
      setMeta(nextMeta);
      setViewport(nextMeta.viewport === "mobile" || nextMeta.viewport === "tablet" ? nextMeta.viewport : "responsive");
      setSource(sourceModule.default || "");
      setHighlightedSource(sourceModule.highlighted || null);
      setSourceLanguage(sourceModule.language || "tsx");
    }).catch((reason) => setError(String(reason)));
  }, [visible, demo]);
  useEffect(() => {
    const receive = (event: MessageEvent) => {
      if (event.source !== frameRef.current?.contentWindow || !event.data) return;
      if (event.data.type === "wake:resize") setHeight(Math.max(80, Number(event.data.height) || 220));
      if (event.data.type === "wake:error") setError(String(event.data.error || text("Demo failed", "演示运行失败")));
      if (event.data.type === "wake:ready") {
        const theme = document.documentElement.dataset.theme || "light";
        frameRef.current?.contentWindow?.postMessage({ type: "wake:theme", theme }, "*");
      }
    };
    window.addEventListener("message", receive);
    return () => window.removeEventListener("message", receive);
  }, []);
  const iframeUrl = siteConfig.basePath + "?__wake_demo=" + encodeURIComponent(demo.id);
  const copy = () => copyText(source).then(() => {
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1400);
  }).catch((reason) => setError(text("Copy failed", "复制失败") + ": " + String(reason)));

  const preview = visible ? <iframe ref={frameRef} data-wake-demo title={meta.title || demo.title}
    src={iframeUrl} style={{ height, width: '100%', border: 0 }} sandbox="allow-scripts allow-same-origin" /> : null;
  return <div ref={ref} data-wake-demo><Slot name="Demo" fallback={DefaultDemo}
    id={demo.id} title={meta.title || demo.title} description={meta.description || ''}
    background={meta.background} padding={meta.padding} loading={!visible || !source} error={error || null}
    {...{ source, highlightedSource, sourceLanguage, codeOpen, setCodeOpen, fullscreen, setFullscreen,
      viewport, setViewport, copied, copy, preview }} /></div>;
}

export function Demo({ src, __wakePage }: { src: string; __wakePage: string }) {
  const id = resolveFromPage(__wakePage, src);
  const demo = demos.find((item) => normalizePath(item.id) === id);
  return demo ? <DemoCard demo={demo} /> : <div className="callout error">{text("Demo not found", "找不到演示")}: {src}</div>;
}

export function Demos({ glob, columns = 1, __wakePage }: { glob: string; columns?: number; __wakePage: string }) {
  const pattern = resolveFromPage(__wakePage, glob);
  const matches = demos.filter((item) => wildcardMatch(normalizePath(item.id), pattern));
  return <div className="demos-grid wake-docs-demos-grid" style={{ "--demo-columns": Math.max(1, Number(columns) || 1) } as React.CSSProperties}>
    {matches.map((demo) => <DemoCard key={demo.id} demo={demo} />)}
    {!matches.length && <div className="callout error">{text("No demos match", "没有匹配的演示")} {glob}</div>}
  </div>;
}

export function API({ source, symbol, __wakePage }: { source: string; symbol: string; component?: string; __wakePage: string }) {
  const key = __wakePage + "|" + source + "|" + symbol;
  const doc = (apiDocs as Record<string, any>)[key];
  const [filter, setFilter] = useState("");

  const props = (doc?.props || []).filter((prop: any) => (prop.name + " " + prop.description + " " + prop.type_text).toLowerCase().includes(filter.toLowerCase()));
  return <Slot name="ApiTable" fallback={DefaultApiTable} symbol={symbol} description={doc?.description || ''}
    properties={props} total={doc?.props.length || 0} filter={filter} setFilter={setFilter}
    inherited={doc?.inherited || []} warnings={doc?.warnings || []}
    error={doc ? null : text('API data not found', '找不到 API 数据') + ': ' + symbol} />;
}

function useSearch(open: boolean, setOpen: (open: boolean) => void, go: (slug: string) => void): UI.SearchState {
  const [query, updateQuery] = useState('');
  const [activeIndex, setActiveIndex] = useState(0);
  const [corpus, setCorpus] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const loaded = useRef(false);
  useEffect(() => { if (open) { updateQuery(''); setActiveIndex(0); } }, [open]);
  useEffect(() => {
    if (!open || loaded.current) return;
    let cancelled = false;
    setLoading(true); setError(null);
    import('@@wake/docs/search-corpus.ts').then(module => {
      if (!cancelled) { loaded.current = true; setCorpus(module.searchTextByPage); }
    }).catch(reason => { if (!cancelled) setError(String(reason)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [open]);
  const index = useMemo(() => createSearchIndex(pages, apiDocs, { section: text('Section', '章节'), prop: text('Prop', '属性') }, corpus), [corpus]);
  const results = useMemo(() => searchDocs(index, query, query.trim() ? 12 : 8).map((item: any) => ({ ...item, href: docsHref(item.slug) })), [index, query]);
  return { open, query, results, activeIndex, loading, error, setOpen, setActiveIndex,
    shortcut: /Mac|iPhone|iPad/.test(navigator.platform) ? '⌘K' : 'Ctrl K',
    setQuery(value) { updateQuery(value); setActiveIndex(0); },
    select(slug) { go(slug); setOpen(false); },
  };
}



function useNavigation(current: string) {
  const [expandedByUser, setExpanded] = useState<Set<string>>(() => {
    try { const saved = JSON.parse(sessionStorage.getItem('wake-docs-user-expanded-sections') || '[]');
      return new Set(Array.isArray(saved) ? saved.filter(value => typeof value === 'string') : []);
    } catch { return new Set(); }
  });
  useEffect(() => {
    try { sessionStorage.setItem('wake-docs-user-expanded-sections', JSON.stringify([...expandedByUser])); } catch {}
  }, [expandedByUser]);
  return { groups: navigationGroups(pages, expandedByUser, current, docsHref), current,
    toggleSection(id: string) { setExpanded(current => { const next = new Set(current); if (next.has(id)) next.delete(id); else next.add(id); return next; }); },
  };
}

function TableOfContents({ page, variant = "desktop" }: { page: PageRecord; variant?: "desktop" | "mobile" }) {
  const common = useCommon();
  const headings = page.headings.filter((heading) => heading.depth > 1 && heading.depth < 4);
  const [active, setActive] = useState(headings[0]?.id || "");
  useEffect(() => {
    let frame = 0;
    const update = () => {
      const visible = headings.map((heading) => ({ id: heading.id, top: document.getElementById(heading.id)?.getBoundingClientRect().top ?? Infinity })).filter((heading) => heading.top <= 132);
      setActive(visible[visible.length - 1]?.id || headings[0]?.id || "");
    };
    const schedule = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(update);
    };
    update();
    window.addEventListener("scroll", schedule, { passive: true });
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("scroll", schedule);
    };
  }, [page.file]);
  return <Slot name="TableOfContents" fallback={DefaultTableOfContents} variant={variant}
    headings={pageInfo(page, docsHref)!.headings.filter((heading: UI.Heading) => heading.depth > 1 && heading.depth < 4)} activeId={active}
    navigate={(id: string) => common.route.navigate(page.slug + '#' + encodeURIComponent(id))} />;
}





type DemoErrorBoundaryProps = {
  resetKey: number;
  onError: (reason: unknown) => void;
  children: React.ReactNode;
};

class PageErrorBoundary extends React.Component<{ children: React.ReactNode }, { error: string }> {
  state = { error: "" };
  static getDerivedStateFromError(reason: unknown) {
    return { error: String((reason as any)?.message || reason) };
  }
  render() {
    return this.state.error ? <Slot name="PageError" fallback={DefaultPageError} error={this.state.error} retry={() => location.reload()} /> : this.props.children;
  }
}

class DemoErrorBoundary extends React.Component<DemoErrorBoundaryProps, { error: string }> {
  state = { error: "" };

  static getDerivedStateFromError(reason: unknown) {
    return { error: String((reason as any)?.stack || reason) };
  }

  componentDidCatch(reason: unknown) {
    this.props.onError(reason);
  }

  componentDidUpdate(previous: Readonly<DemoErrorBoundaryProps>) {
    if (previous.resetKey !== this.props.resetKey && this.state.error) {
      this.setState({ error: "" });
    }
  }

  render() {
    return this.state.error
      ? <div className="frame-error">{this.state.error}</div>
      : this.props.children;
  }
}

function DemoFrame({ id, resolved }: { id: string; resolved: ResolvedTheme }) {
  const demo = demos.find((item) => item.id === id);
  const [module, setModule] = useState<any>(null);
  const [args, setArgs] = useState<Record<string, unknown>>({});
  const [argsRevision, setArgsRevision] = useState(0);
  const [error, setError] = useState("");
  useEffect(() => {
    if (!demo) return;
    setError("");
    demo.load().then((nextModule) => {
      setModule(nextModule);
      const initialArgs = nextModule.meta?.args;
      if (initialArgs && typeof initialArgs === "object" && !Array.isArray(initialArgs)) {
        try {
          setArgs(JSON.parse(JSON.stringify(initialArgs)));
        } catch {
          setArgs({});
        }
      }
    }).catch((reason) => setError(String(reason?.stack || reason)));
  }, [demo]);
  useEffect(() => {
    const send = (message: any) => window.parent.postMessage(message, "*");
    const resize = new ResizeObserver(() => send({ type: "wake:resize", height: Math.ceil(document.documentElement.scrollHeight) }));
    resize.observe(document.documentElement);
    const receive = (event: MessageEvent) => {
      if (event.source !== window.parent || !event.data) return;
      if (event.data.type === "wake:theme") document.documentElement.dataset.theme = event.data.theme;
      if (event.data.type === "wake:args" && (!event.data.id || event.data.id === id)) {
        const nextArgs = event.data.args;
        if (nextArgs && typeof nextArgs === "object" && !Array.isArray(nextArgs)) {
          setArgs(nextArgs);
          setArgsRevision((current) => current + 1);
        }
      }
    };
    const onError = (event: ErrorEvent) => send({ type: "wake:error", error: event.error?.stack || event.message });
    const onUnhandledRejection = (event: PromiseRejectionEvent) => send({ type: "wake:error", error: event.reason?.stack || String(event.reason) });
    window.addEventListener("message", receive);
    window.addEventListener("error", onError);
    window.addEventListener("unhandledrejection", onUnhandledRejection);
    document.documentElement.dataset.theme = resolved;
    return () => {
      resize.disconnect();
      window.removeEventListener("message", receive);
      window.removeEventListener("error", onError);
      window.removeEventListener("unhandledrejection", onUnhandledRejection);
    };
  }, [id, resolved]);
  useEffect(() => {
    if (module) window.parent.postMessage({ type: "wake:ready", id }, "*");
  }, [id, module]);
  if (!demo) return <div className="frame-error">{text("Demo not found", "找不到演示")}: {id}</div>;
  if (error) { window.parent.postMessage({ type: "wake:error", error }, "*"); return <div className="frame-error">{error}</div>; }
  if (!module) return <div className="frame-loading">{text("Loading preview…", "正在加载预览…")}</div>;
  const Component = module.default;
  const reportError = (reason: unknown) => window.parent.postMessage({
    type: "wake:error",
    error: String((reason as any)?.stack || reason),
  }, "*");
  return <DemoErrorBoundary resetKey={argsRevision} onError={reportError}>
    <div className="demo-frame-root" data-wake-default={siteConfig.customPreview ? undefined : 'Preview'}
      data-wake-custom={siteConfig.customPreview ? 'Preview' : undefined} data-theme={resolved}
      style={siteConfig.accentColor ? { '--wake-accent': siteConfig.accentColor } as React.CSSProperties : undefined}>
      <Preview>{siteConfig.customPreview ? <Component {...args} /> : <div data-wake-demo><Component {...args} /></div>}</Preview>
    </div>
  </DemoErrorBoundary>;
}

function Application() {
  const components = useMemo(() => resolveUI(), []);
  const { theme, resolved, setTheme } = useTheme();
  const [path, setPath] = useState(appPath);
  const [search, setSearch] = useState(false);
  const [drawer, setDrawer] = useState(false);
  const [announcement, setAnnouncement] = useState({ key: 0, message: "" });
  const contentRef = useRef<HTMLElement>(null);
  const pathRef = useRef(path);
  const pendingNavigation = useRef<{ route: string; hash: string; focus: boolean } | null>(window.location.hash ? {
    route: appPath().split("#")[0],
    hash: window.location.hash.slice(1),
    focus: false,
  } : null);

  useLayoutEffect(() => { pathRef.current = path; }, [path]);

  const finishNavigation = useCallback((slug: string, title: string) => {
    const pending = pendingNavigation.current;
    const route = slug.split("#")[0].replace(/\/$/, "") || "/";
    if (!pending || (pending.route.replace(/\/$/, "") || "/") !== route) return;
    pendingNavigation.current = null;
    requestAnimationFrame(() => {
      let target: HTMLElement | null = null;
      if (pending.hash) {
        try { target = document.getElementById(decodeURIComponent(pending.hash)); } catch { target = document.getElementById(pending.hash); }
      }
      if (target) {
        target.scrollIntoView({ block: "start" });
      } else {
        window.scrollTo({ top: 0, behavior: "instant" as ScrollBehavior });
        target = contentRef.current?.querySelector<HTMLElement>("h1") || contentRef.current;
      }
      if (pending.focus && target) {
        if (!target.hasAttribute("tabindex")) target.setAttribute("tabindex", "-1");
        target.focus({ preventScroll: true });
        setAnnouncement((current) => ({ key: current.key + 1, message: text("Opened " + title, "已打开“" + title + "”") }));
      }
    });
  }, []);

  const go = useCallback((slug: string) => {
    const [rawRoute, rawHash = ""] = slug.split("#", 2);
    const route = rawRoute.replace(/\/$/, "") || "/";
    const targetPage = pageForPath(route);
    pendingNavigation.current = { route, hash: rawHash, focus: true };
    const next = docsHref(route) + (rawHash ? "#" + rawHash : "");
    if (window.location.pathname + window.location.hash !== next) history.pushState(null, "", next);
    if (targetPage) void loadPage(targetPage).catch(() => {});
    const currentRoute = pathRef.current.split("#")[0].replace(/\/$/, "") || "/";
    if (currentRoute === route) {
      setPath(appPath());
      finishNavigation(route, targetPage?.title || siteConfig.title);
    } else {
      startTransition(() => setPath(appPath()));
    }
  }, [finishNavigation]);

  useEffect(() => {
    const update = () => {
      const nextPath = appPath();
      const route = nextPath.split("#")[0].replace(/\/$/, "") || "/";
      const targetPage = pageForPath(route);
      const currentRoute = pathRef.current.split("#")[0].replace(/\/$/, "") || "/";
      pendingNavigation.current = { route, hash: window.location.hash.slice(1), focus: true };
      startTransition(() => setPath(nextPath));
      if (currentRoute === route) finishNavigation(route, targetPage?.title || siteConfig.title);
    };
    const keys = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") { event.preventDefault(); setSearch(true); }
      if (event.key === "Escape") { setSearch(false); setDrawer(false); }
    };
    window.addEventListener("popstate", update);
    window.addEventListener("keydown", keys);
    return () => { window.removeEventListener("popstate", update); window.removeEventListener("keydown", keys); };
  }, [finishNavigation]);
  useEffect(() => {
    const ready = (event: Event) => {
      const detail = (event as CustomEvent<{ slug: string; title: string }>).detail;
      if (detail) finishNavigation(detail.slug, detail.title);
    };
    window.addEventListener("wake:page-ready", ready);
    return () => window.removeEventListener("wake:page-ready", ready);
  }, [finishNavigation]);
  useEffect(() => {
    const anchorFor = (event: Event) => event.target instanceof Element ? event.target.closest<HTMLAnchorElement>("a[href]") : null;
    const preload = (event: Event) => {
      if (developmentDemand) return;
      const anchor = anchorFor(event);
      if (!anchor) return;
      const link = internalPageLink(anchor);
      if (link) void loadPage(link.page).catch(() => {});
    };
    const click = (event: MouseEvent) => {
      if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
      const anchor = anchorFor(event);
      if (!anchor) return;
      const link = internalPageLink(anchor);
      if (!link) return;
      event.preventDefault();
      go(link.slug);
    };
    document.addEventListener("click", click);
    document.addEventListener("pointerover", preload, { passive: true });
    document.addEventListener("focusin", preload);
    return () => {
      document.removeEventListener("click", click);
      document.removeEventListener("pointerover", preload);
      document.removeEventListener("focusin", preload);
    };
  }, [go]);
  const demoId = new URLSearchParams(window.location.search).get("__wake_demo");
  const routePath = path.split('#')[0].replace(/\/$/, '') || '/';
  const page = pageForPath(routePath);
  const LazyPage = page ? lazyPage(page) : null;
  const searchState = useSearch(search, setSearch, go);
  const navigationState = useNavigation(page?.slug || '');
  const common: UI.CommonProps = {
    site: { title: siteConfig.title, description: siteConfig.description, locale: siteConfig.locale,
      logo: siteConfig.logo, repositoryUrl: siteConfig.repositoryUrl, basePath: siteConfig.basePath, homeHref: docsHref('/'), accentColor: siteConfig.accentColor },
    theme: { theme, resolved, setTheme }, route: { path, page: pageInfo(page, docsHref), navigate: go, href: docsHref,
      focusContent() {
        const target = contentRef.current?.querySelector<HTMLElement>('h1') || contentRef.current;
        if (target) { target.tabIndex = -1; target.focus({ preventScroll: true }); }
      },
    },
  };
  useEffect(() => {
    if (!page && !demoId) updateDocumentMetadata('404 · ' + siteConfig.title, text('The requested documentation page could not be found.', '找不到请求的文档页面。'));
  }, [page, demoId]);
  const navigate = (slug: string) => { go(slug); setDrawer(false); };
  const navigation = <Slot name="Navigation" fallback={DefaultNavigation} {...navigationState} onNavigate={navigate} />;
  const content = <main id="wake-docs-content" ref={contentRef} tabIndex={-1}>
    {page && <TableOfContents page={page} variant="mobile" />}
    <PageErrorBoundary key={routePath}><Suspense fallback={<Slot name="PageLoading" fallback={DefaultPageLoading} />}>
      {LazyPage ? <LazyPage /> : <Slot name="NotFound" fallback={DefaultNotFound} />}
    </Suspense></PageErrorBoundary>
  </main>;
  return <UIProvider value={common} components={components}>
    {demoId ? <DemoFrame id={demoId} resolved={resolved} /> : <Slot name="Root" fallback={DefaultRoot}>
      <p role="status" aria-live="polite" aria-atomic="true" key={announcement.key} style={{ position: 'absolute', width: 1, height: 1, overflow: 'hidden', clipPath: 'inset(50%)' }}>{announcement.message}</p>
      <Slot name="Layout" fallback={DefaultLayout}
        header={<Slot name="Header" fallback={DefaultHeader} search={searchState} mobileNavigation={{ open: drawer, setOpen: setDrawer }} />}
        navigation={navigation}
        mobileNavigation={<Slot name="MobileNavigation" fallback={DefaultMobileNavigation} open={drawer} setOpen={setDrawer} navigation={navigation} />}
        tableOfContents={page ? <TableOfContents page={page} /> : null}
        searchDialog={<Slot name="SearchDialog" fallback={DefaultSearchDialog} search={searchState} />}>
        {content}
      </Slot>
    </Slot>}
  </UIProvider>;
}

export function App() { return <UIErrorBoundary><Application /></UIErrorBoundary>; }
