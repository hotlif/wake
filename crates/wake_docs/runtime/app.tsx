import React, { Suspense, startTransition, useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from "react";
import { apiDocs, demos, pages } from "@@wake/docs/registry.ts";
import { Preview, siteConfig } from "@@wake/docs/config.tsx";
import { docsRouteHref, findPageForPath, routePathFromLocation } from "./routes.mjs";
import { createSearchIndex, searchDocs } from "./search.mjs";

type Theme = "light" | "dark" | "system";
type ResolvedTheme = "light" | "dark";
type DemoRecord = (typeof demos)[number];
type PageRecord = (typeof pages)[number];
type ViewportPreset = "responsive" | "tablet" | "mobile";
type NavSection = { id: string; title: string; pages: PageRecord[] };
type NavGroup = { id: string; title: string; pages: PageRecord[]; sections: NavSection[] };

const isChinese = siteConfig.locale.toLowerCase().startsWith("zh");
const text = (english: string, chinese: string) => isChinese ? chinese : english;
const pageLoads = new Map<string, Promise<any>>();
const lazyPages = new Map<string, React.LazyExoticComponent<React.ComponentType>>();

function loadPage(page: PageRecord): Promise<any> {
  const cached = pageLoads.get(page.slug);
  if (cached) return cached;
  const pending = page.load().catch((reason) => {
    pageLoads.delete(page.slug);
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

function focusableElements(container: HTMLElement) {
  return Array.from(container.querySelectorAll(
    'a[href], button:not([disabled]), iframe, input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
  )).filter((element) => element.getAttribute("aria-hidden") !== "true" && element.getClientRects().length > 0) as HTMLElement[];
}

function trapDialogFocus(event: React.KeyboardEvent<HTMLElement>) {
  if (event.key !== "Tab") return;
  const focusable = focusableElements(event.currentTarget);
  if (!focusable.length) {
    event.preventDefault();
    event.currentTarget.focus();
    return;
  }
  const first = focusable[0];
  const last = focusable[focusable.length - 1];
  if (event.shiftKey && document.activeElement === first) {
    event.preventDefault();
    last.focus();
  } else if (!event.shiftKey && document.activeElement === last) {
    event.preventDefault();
    first.focus();
  }
}

function useDialogFocus(open: boolean, initialFocus: { current: HTMLElement | null }) {
  const returnFocus = useRef<HTMLElement | null>(null);
  const shouldRestore = useRef(true);
  useEffect(() => {
    if (!open) return;
    shouldRestore.current = true;
    returnFocus.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    const focusFrame = requestAnimationFrame(() => initialFocus.current?.focus());
    return () => {
      cancelAnimationFrame(focusFrame);
      document.body.style.overflow = previousOverflow;
      if (!shouldRestore.current) return;
      const target = returnFocus.current;
      requestAnimationFrame(() => { if (target?.isConnected) target.focus(); });
    };
  }, [open, initialFocus]);
  return useCallback(() => { shouldRestore.current = false; }, []);
}

function statusText(status: string): string {
  if (!isChinese) return status;
  return ({ beta: "测试版", experimental: "实验性", deprecated: "已废弃", draft: "草稿" } as Record<string, string>)[status] || status;
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
    if (siteConfig.accentColor) {
      document.documentElement.style.setProperty("--wake-accent", siteConfig.accentColor);
    }
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

function StatusBadge({ status }: { status: string }) {
  return status === "stable" ? null : <span className={"status status-" + status}>{statusText(status)}</span>;
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

function codeLanguageName(language: string): string {
  return ({ javascript: "JavaScript", typescript: "TypeScript", jsx: "JSX", tsx: "TSX", rust: "Rust", bash: "Shell", powershell: "PowerShell", python: "Python", sql: "SQL", json: "JSON", jsonc: "JSONC", toml: "TOML", yaml: "YAML", css: "CSS", scss: "SCSS", html: "HTML", mdx: "MDX", markdown: "Markdown", text: text("Text", "文本") } as Record<string, string>)[language] || language.toUpperCase();
}

export function CodeBlock({ language, code, title, children }: { language: string; code: string; title?: string; children: React.ReactNode }) {
  const [copyStatus, setCopyStatus] = useState<"idle" | "copied" | "error">("idle");
  const lineCount = Math.max(1, code.split(/\r?\n/).length);
  const copy = () => copyText(code).then(() => {
    setCopyStatus("copied");
    window.setTimeout(() => setCopyStatus("idle"), 1600);
  }).catch(() => {
    setCopyStatus("error");
    window.setTimeout(() => setCopyStatus("idle"), 1600);
  });
  const copyLabel = copyStatus === "copied" ? text("Copied", "已复制") : copyStatus === "error" ? text("Copy failed", "复制失败") : text("Copy", "复制");
  return <figure className="code-block" data-language={language} data-line-numbers={lineCount > 1 ? "true" : "false"}>
    <figcaption className="code-toolbar">
      <span className="code-identity">{title && <strong>{title}</strong>}<small>{codeLanguageName(language)}</small></span>
      <button type="button" className={copyStatus === "copied" ? "is-copied" : copyStatus === "error" ? "is-error" : ""} onClick={copy} aria-label={text("Copy code", "复制代码")} aria-live="polite">{copyLabel}</button>
    </figcaption>
    <pre tabIndex={0}><code>{children}</code></pre>
  </figure>;
}

function PagePager({ current }: { current: string }) {
  const visiblePages = pages.filter((page) => !page.hidden);
  const index = visiblePages.findIndex((page) => page.slug === current);
  const previous = index > 0 ? visiblePages[index - 1] : undefined;
  const next = index >= 0 && index < visiblePages.length - 1 ? visiblePages[index + 1] : undefined;
  const link = (page: PageRecord, direction: "previous" | "next") => <a
    className={"page-pager-link page-pager-" + direction}
    href={docsHref(page.slug)}
  >
    <small>{direction === "previous" ? text("Previous", "上一篇") : text("Next", "下一篇")}</small>
    <strong>{page.title}</strong>
  </a>;
  if (!previous && !next) return null;
  return <nav className="page-pager" aria-label={text("Page navigation", "页面导航")}>
    {previous ? link(previous, "previous") : <span />}
    {next ? link(next, "next") : <span />}
  </nav>;
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
  return <article className="mdx-page">
    <header className="page-header">
      <nav className="breadcrumbs" aria-label={text("Breadcrumb", "面包屑")}>{crumbs.map((crumb, index) => <React.Fragment key={crumb}><span>{crumb}</span>{index < crumbs.length - 1 && <i aria-hidden="true">/</i>}</React.Fragment>)}</nav>
      {(meta.status !== "stable" || meta.draft) && <div className="eyebrow"><StatusBadge status={meta.status} />{meta.draft && <span className="status status-draft">{statusText("draft")}</span>}</div>}
      <h1 tabIndex={-1}>{meta.title}</h1>
      {meta.description && <p className="page-description">{meta.description}</p>}
    </header>
    <div className="mdx-content">{children}</div>
    <PagePager current={meta.slug} />
  </article>;
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

function DemoSource({ source, highlighted, language, id, labelledBy, panel = false }: { source: string; highlighted: React.ReactNode; language: string; id?: string; labelledBy?: string; panel?: boolean }) {
  const rendered = highlighted || source || text("Loading source…", "正在加载源码…");
  const lineCount = Math.max(1, source.split(/\r?\n/).length);
  return <pre id={id} className={"demo-code" + (panel ? " demo-panel" : "")} role={panel ? "region" : undefined} aria-labelledby={labelledBy} tabIndex={0} data-language={language} data-line-numbers={lineCount > 1 ? "true" : "false"}><code>{rendered}</code></pre>;
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
  const fullscreenClose = useRef<HTMLButtonElement>(null);
  const domId = "wake-demo-" + useId().replace(/:/g, "");
  const titleId = domId + "-title";
  const previewPanelId = domId + "-preview-panel";
  const codeToggleId = domId + "-code-toggle";
  const codePanelId = domId + "-code-panel";
  const playgroundTitleId = domId + "-playground-title";
  useDialogFocus(fullscreen, fullscreenClose);
  useEffect(() => {
    if (!fullscreen) return;
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === "Escape") setFullscreen(false);
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [fullscreen]);
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

  const viewportOptions: Array<{ id: ViewportPreset; label: string; size: string }> = [
    { id: "responsive", label: text("Desktop", "电脑"), size: text("Responsive", "自适应") },
    { id: "tablet", label: text("Tablet", "平板"), size: "768 × 540" },
    { id: "mobile", label: text("Mobile", "手机"), size: "390 × 700" },
  ];
  const preview = <div className={"demo-stage demo-bg-" + meta.background} style={{ padding: meta.padding === "none" ? 0 : meta.padding === "sm" ? 12 : meta.padding === "lg" ? 32 : 20 }}>
    <div className={"demo-viewport demo-viewport-" + viewport} data-viewport={viewport}>
      {viewport === "responsive" && <div className="demo-browser-chrome" aria-hidden="true"><span className="demo-window-dots"><i /><i /><i /></span><span className="demo-address-bar">localhost / preview</span><span className="demo-browser-menu">•••</span></div>}
      {viewport === "tablet" && <div className="demo-tablet-details" aria-hidden="true"><span className="demo-tablet-camera" /><span className="demo-tablet-button" /><span className="demo-tablet-port" /></div>}
      {viewport === "mobile" && <div className="demo-phone-details" aria-hidden="true"><span className="demo-phone-island" /><span className="demo-phone-volume-one" /><span className="demo-phone-volume-two" /><span className="demo-phone-power" /><span className="demo-phone-home" /></div>}
      <div className="demo-screen">
        {visible && <iframe ref={frameRef} data-wake-demo title={meta.title || demo.title} src={iframeUrl} style={{ height }} sandbox="allow-scripts allow-same-origin" />}
        {!visible && <div className="demo-skeleton" />}
        {error && <div className="demo-error" role="alert"><strong>{text("Demo error", "演示错误")}</strong><span>{error}</span></div>}
      </div>
    </div>
  </div>;
  return <div className="demo-card" ref={ref}>
    <div id={previewPanelId} className="demo-panel" role="region" aria-label={text("Demo preview", "组件预览")} tabIndex={0}>
      {preview}
    </div>
    <div className="demo-titlebar">
      <div><strong id={titleId}>{meta.title || demo.title}</strong>{meta.description && <span>{meta.description}</span>}</div>
    </div>
    <div className="demo-toolbar">
      <div className="demo-device-tools" role="group" aria-label={text("Preview device", "预览设备")}>
        {viewportOptions.map((option) => <button className="demo-icon-button" aria-pressed={viewport === option.id} aria-label={option.label + " · " + option.size} data-tooltip={option.label + " · " + option.size} type="button" key={option.id} onClick={() => setViewport(option.id)}><i className={"viewport-icon viewport-icon-" + option.id} aria-hidden="true" /></button>)}
      </div>
      <div className="demo-toolbar-actions">
        <button className="demo-icon-button" id={codeToggleId} type="button" aria-label={codeOpen ? text("Collapse source", "收起源码") : text("Expand source", "展开源码")} aria-expanded={codeOpen} aria-controls={codePanelId} data-tooltip={codeOpen ? text("Collapse source", "收起源码") : text("Expand source", "展开源码")} onClick={() => setCodeOpen((open) => !open)}><i className="demo-tool-icon demo-tool-icon-code" aria-hidden="true" /></button>
        {codeOpen && <button className="demo-icon-button" type="button" onClick={copy} aria-label={text("Copy demo source", "复制演示源码")} data-tooltip={copied ? text("Copied", "已复制") : text("Copy", "复制")}><i className="demo-tool-icon demo-tool-icon-copy" aria-hidden="true" /></button>}
        <button className="demo-icon-button" type="button" aria-label={text("Open playground", "全屏预览")} data-tooltip={text("Open playground", "全屏预览")} aria-haspopup="dialog" onClick={() => setFullscreen(true)}><i className="demo-tool-icon demo-tool-icon-fullscreen" aria-hidden="true" /></button>
      </div>
    </div>
    {codeOpen && <DemoSource id={codePanelId} labelledBy={codeToggleId} panel source={source} highlighted={highlightedSource} language={sourceLanguage} />}
    {fullscreen && <div className="playground-backdrop" onMouseDown={() => setFullscreen(false)}>
      <div className="playground" role="dialog" aria-modal="true" aria-labelledby={playgroundTitleId} onKeyDown={trapDialogFocus} onMouseDown={(event) => event.stopPropagation()}>
        <div className="playground-bar"><strong id={playgroundTitleId}>{meta.title || demo.title}</strong><span>{text("Device preview and highlighted source", "设备预览与高亮源码")}</span><button ref={fullscreenClose} type="button" onClick={() => setFullscreen(false)}>{text("Close", "关闭")}</button></div>
        <div className="playground-body">{preview}<DemoSource source={source} highlighted={highlightedSource} language={sourceLanguage} /></div>
      </div>
    </div>}
  </div>;
}

export function Demo({ src, __wakePage }: { src: string; __wakePage: string }) {
  const id = resolveFromPage(__wakePage, src);
  const demo = demos.find((item) => normalizePath(item.id) === id);
  return demo ? <DemoCard demo={demo} /> : <div className="callout error">{text("Demo not found", "找不到演示")}: {src}</div>;
}

export function Demos({ glob, columns = 1, __wakePage }: { glob: string; columns?: number; __wakePage: string }) {
  const pattern = resolveFromPage(__wakePage, glob);
  const matches = demos.filter((item) => wildcardMatch(normalizePath(item.id), pattern));
  return <div className="demos-grid" style={{ "--demo-columns": Math.max(1, Number(columns) || 1) } as React.CSSProperties}>
    {matches.map((demo) => <DemoCard key={demo.id} demo={demo} />)}
    {!matches.length && <div className="callout error">{text("No demos match", "没有匹配的演示")} {glob}</div>}
  </div>;
}

export function API({ source, symbol, __wakePage }: { source: string; symbol: string; component?: string; __wakePage: string }) {
  const key = __wakePage + "|" + source + "|" + symbol;
  const doc = (apiDocs as Record<string, any>)[key];
  const [filter, setFilter] = useState("");
  const statusId = "api-status-" + useId().replace(/:/g, "");
  if (!doc) return <div className="callout error">{text("API data not found", "找不到 API 数据")}: {symbol}</div>;
  const props = doc.props.filter((prop: any) => (prop.name + " " + prop.description + " " + prop.type_text).toLowerCase().includes(filter.toLowerCase()));
  return <section className="api-section" aria-labelledby={"api-" + symbol}>
    <div className="api-heading">
      <div><span className="eyebrow">{text("Props", "属性")}</span><h2 id={"api-" + symbol}>{symbol}</h2></div>
      <div className="api-filter"><span aria-hidden="true">⌕</span><input aria-label={text("Filter properties", "筛选属性")} aria-describedby={statusId} placeholder={text("Filter by name or type…", "按名称或类型筛选…")} value={filter} onChange={(event) => setFilter(event.target.value)} /></div>
    </div>
    {doc.description && <p>{doc.description}</p>}
    <p className="api-filter-status" id={statusId} role="status" aria-live="polite">{filter ? text(props.length + " of " + doc.props.length + " properties", "找到 " + props.length + " / " + doc.props.length + " 个属性") : text(doc.props.length + " properties", "共 " + doc.props.length + " 个属性")}</p>
    {props.length > 0 ? <div className="api-table-wrap"><table className="api-table"><caption className="sr-only">{symbol} {text("properties", "属性")}</caption><thead><tr><th>{text("Property", "属性")}</th><th>{text("Type", "类型")}</th><th>{text("Default", "默认值")}</th><th>{text("Description", "说明")}</th></tr></thead><tbody>
      {props.map((prop: any) => <tr key={prop.name} className={prop.deprecated ? "deprecated" : ""}>
        <td data-label={text("Property", "属性")}><code>{prop.name}</code>{prop.required && <span className="required">{text("required", "必填")}</span>}{prop.deprecated && <span className="deprecated-badge">{text("deprecated", "已废弃")}</span>}</td>
        <td data-label={text("Type", "类型")}><code className="type-code">{prop.type_text}</code></td>
        <td data-label={text("Default", "默认值")}><code>{prop.default_value || "—"}</code></td>
        <td data-label={text("Description", "说明")}>{prop.description || "—"}{prop.since && <small>{text("Since", "始于")} {prop.since}</small>}</td>
      </tr>)}
    </tbody></table></div> : <div className="api-empty"><strong>{text("No matching properties", "没有匹配的属性")}</strong><span>{text("Try another name or type keyword.", "请尝试其他属性名或类型关键词。")}</span><button type="button" onClick={() => setFilter("")}>{text("Clear filter", "清除筛选")}</button></div>}
    {doc.inherited.map((group: any) => <details className="inherited" key={group.name + group.source}><summary>{text("Inherited from", "继承自")} <code>{group.name}</code></summary><p>{group.type_text} · {group.source}</p></details>)}
    {doc.warnings.map((warning: string) => <div className="callout warning" key={warning}>{warning}</div>)}
  </section>;
}

function Search({ open, close, go }: { open: boolean; close: () => void; go: (slug: string) => void }) {
  const [query, setQuery] = useState("");
  const [active, setActive] = useState(0);
  const [searchTextByPage, setSearchTextByPage] = useState<Record<string, string>>({});
  const corpusRequested = useRef(false);
  const input = useRef<HTMLInputElement>(null);
  const suppressFocusRestore = useDialogFocus(open, input);
  useEffect(() => { if (open) { setQuery(""); setActive(0); } }, [open]);
  useEffect(() => {
    if (!open || corpusRequested.current) return;
    corpusRequested.current = true;
    import("@@wake/docs/search-corpus.ts")
      .then((module) => setSearchTextByPage(module.searchTextByPage))
      .catch(() => { corpusRequested.current = false; });
  }, [open]);
  const sectionKind = text("Section", "章节");
  const propKind = text("Prop", "属性");
  const index = useMemo(() => createSearchIndex(pages, apiDocs, { section: sectionKind, prop: propKind }, searchTextByPage), [sectionKind, propKind, searchTextByPage]);
  const results = useMemo(() => searchDocs(index, query, query.trim() ? 12 : 8), [index, query]);
  useEffect(() => { if (open) document.getElementById("wake-search-result-" + active)?.scrollIntoView({ block: "nearest" }); }, [active, query, open]);
  const choose = (slug: string) => { suppressFocusRestore(); go(slug); close(); };
  const onKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive((value) => Math.min(value + 1, Math.max(0, results.length - 1)));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive((value) => Math.max(0, value - 1));
    } else if (event.key === "Enter" && results[active]) {
      event.preventDefault();
      choose(results[active].slug);
    }
  };
  if (!open) return null;
  return <div className="search-backdrop" role="presentation" onMouseDown={close}>
    <div id="wake-search-dialog" className="search-dialog" role="dialog" aria-modal="true" aria-label={text("Search documentation", "搜索文档")} onKeyDown={trapDialogFocus} onMouseDown={(event) => event.stopPropagation()}>
      <div className="search-input"><span aria-hidden="true">⌕</span><input ref={input} value={query} onChange={(event) => { setQuery(event.target.value); setActive(0); }} onKeyDown={onKeyDown} role="combobox" aria-label={text("Search documentation", "搜索文档")} aria-autocomplete="list" aria-expanded={open} aria-controls="wake-search-results" aria-activedescendant={results[active] ? "wake-search-result-" + active : undefined} placeholder={text("Search pages, headings, commands, and props…", "搜索页面、章节、命令和属性…")} /><kbd>Esc</kbd></div>
      <div className="search-results" id="wake-search-results" role="listbox" aria-label={text("Search results", "搜索结果")}>
        {results.map((item: any, index: number) => <div id={"wake-search-result-" + index} className={"search-result " + (index === active ? "active" : "")} role="option" aria-selected={index === active} key={item.slug + index} onMouseMove={() => setActive(index)} onMouseDown={(event) => event.preventDefault()} onClick={() => choose(item.slug)}><span><strong>{item.title}</strong><small>{item.detail}</small></span><em>{item.kind}</em></div>)}
        {!results.length && <p className="empty-search">{text("No results for", "没有找到")} “{query}”</p>}
      </div>
    </div>
  </div>;
}

function ThemeButton({ theme, setTheme }: { theme: Theme; setTheme: (theme: Theme) => void }) {
  const next: Record<Theme, Theme> = { system: "light", light: "dark", dark: "system" };
  const icon = theme === "light" ? "☀" : theme === "dark" ? "☾" : "◐";
  const name = ({ system: text("system", "跟随系统"), light: text("light", "浅色"), dark: text("dark", "深色") } as Record<Theme, string>)[theme];
  const label = text("Theme", "主题") + ": " + name;
  return <button type="button" className="icon-button" onClick={() => setTheme(next[theme])} aria-label={label} title={label}>{icon}</button>;
}

function Logo({ onNavigate }: { onNavigate?: () => void }) {
  return <a className="brand" href={docsHref("/")} onClick={onNavigate}>
    {siteConfig.logo ? <img src={siteConfig.logo} alt="" /> : <span className="brand-mark">W</span>}
    <span><strong>{siteConfig.title}</strong>{siteConfig.description && <small>{siteConfig.description}</small>}</span>
  </a>;
}

function Sidebar({ current, close }: { current: string; close?: () => void }) {
  const groups = useMemo(() => pages.filter((page) => !page.hidden).reduce((result, page) => {
    let group = result.find((item) => item.id === page.group_id);
    if (!group) {
      group = { id: page.group_id, title: page.group, pages: [], sections: [] };
      result.push(group);
    }
    if (!page.section_id) group.pages.push(page);
    else {
      let section = group.sections.find((item) => item.id === page.section_id);
      if (!section) {
        section = { id: page.section_id, title: page.section, pages: [] };
        group.sections.push(section);
      }
      section.pages.push(page);
    }
    return result;
  }, [] as NavGroup[]), []);
  const activeSection = groups.flatMap((group) => group.sections.map((section) => ({ group, section }))).find(({ section }) => section.pages.some((page) => page.slug === current));
  const activeKey = activeSection ? activeSection.group.id + "/" + activeSection.section.id : "";
  const [expandedByUser, setExpandedByUser] = useState<Set<string>>(() => {
    try {
      const saved = JSON.parse(sessionStorage.getItem("wake-docs-user-expanded-sections") || "[]");
      return new Set(Array.isArray(saved) ? saved.filter((value) => typeof value === "string") : []);
    } catch {
      return new Set();
    }
  });
  useEffect(() => {
    try { sessionStorage.setItem("wake-docs-user-expanded-sections", JSON.stringify([...expandedByUser])); } catch { /* Navigation still works without persistence. */ }
  }, [expandedByUser]);
  useEffect(() => {
    document.querySelector('.sidebar-nav a[aria-current="page"]')?.scrollIntoView({ block: "nearest" });
  }, [current]);
  const link = (page: PageRecord, nested = false) => {
    const active = page.slug === current;
    return <a
      key={page.slug}
      className={(active ? "active" : "") + (nested ? " nested" : "")}
      aria-current={active ? "page" : undefined}
      href={docsHref(page.slug)}
      onClick={() => close?.()}
    >
      <span>{page.title}</span>
    </a>;
  };
  return <nav className="sidebar-nav" aria-label={text("Documentation", "文档导航")}>
    {groups.map((group) => <div className="nav-group" key={group.id}>
      <h2>{group.title}</h2>
      {group.pages.map((page) => link(page))}
      {group.sections.map((section) => {
        const key = group.id + "/" + section.id;
        const open = expandedByUser.has(key) || key === activeKey;
        const controls = "wake-nav-" + group.id + "-" + section.id;
        return <div className="nav-section" key={key}>
          <button type="button" className="nav-section-toggle" aria-expanded={open} aria-controls={controls} onClick={() => setExpandedByUser((current) => {
            const next = new Set(current);
            if (next.has(key)) next.delete(key); else next.add(key);
            return next;
          })}>
            <span>{section.title}</span><i aria-hidden="true">›</i>
          </button>
          <div className="nav-section-pages" id={controls} hidden={!open}>
            {section.pages.map((page) => link(page, true))}
          </div>
        </div>;
      })}
    </div>)}
  </nav>;
}

function TableOfContents({ page }: { page: PageRecord }) {
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
  if (!headings.length) return null;
  return <nav className="toc" aria-label={text("On this page", "本页目录")}><h2>{text("On this page", "本页目录")}</h2>{headings.map((heading, index) => <a className={(heading.depth === 3 ? "nested " : "") + (active === heading.id ? "active" : "")} aria-current={active === heading.id ? "location" : undefined} key={heading.id + index} href={"#" + heading.id}>{heading.title}</a>)}</nav>;
}

function MobileTableOfContents({ page }: { page: PageRecord }) {
  const headings = page.headings.filter((heading) => heading.depth > 1 && heading.depth < 4);
  const details = useRef<HTMLDetailsElement>(null);
  if (!headings.length) return null;
  return <details className="mobile-toc" ref={details}>
    <summary><span>{text("On this page", "本页目录")}</span><small>{headings.length}</small></summary>
    <nav aria-label={text("On this page", "本页目录")} onClick={(event) => {
      if ((event.target as Element).closest("a[href]")) details.current?.removeAttribute("open");
    }}>
      {headings.map((heading, index) => <a className={heading.depth === 3 ? "nested" : ""} key={heading.id + index} href={"#" + heading.id}>{heading.title}</a>)}
    </nav>
  </details>;
}

function NotFound() {
  const description = text("The requested documentation page could not be found.", "找不到请求的文档页面。");
  const title = text("Page not found", "页面不存在");
  useEffect(() => {
    updateDocumentMetadata("404 · " + siteConfig.title, description);
    window.dispatchEvent(new CustomEvent("wake:page-ready", { detail: { slug: appPath().split("#")[0], title } }));
  }, [description, title]);
  return <div className="not-found"><span>404</span><h1 tabIndex={-1}>{title}</h1><p>{text("The document may have moved or is still being written.", "文档可能已移动，或仍在编写中。")}</p><a href={docsHref("/")}>{text("Back to documentation", "返回文档首页")}</a></div>;
}

function ReadingProgress() {
  const progress = useRef<HTMLSpanElement>(null);
  const [showBackToTop, setShowBackToTop] = useState(false);
  useEffect(() => {
    let frame = 0;
    const update = () => {
      const distance = Math.max(0, document.documentElement.scrollHeight - window.innerHeight);
      if (progress.current) progress.current.style.transform = "scaleX(" + (distance ? Math.min(1, window.scrollY / distance) : 0) + ")";
      const next = window.scrollY > Math.max(640, window.innerHeight * .75);
      setShowBackToTop((current) => current === next ? current : next);
    };
    const schedule = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(update);
    };
    const resize = new ResizeObserver(schedule);
    resize.observe(document.documentElement);
    update();
    window.addEventListener("scroll", schedule, { passive: true });
    window.addEventListener("resize", schedule);
    return () => {
      cancelAnimationFrame(frame);
      resize.disconnect();
      window.removeEventListener("scroll", schedule);
      window.removeEventListener("resize", schedule);
    };
  }, []);
  return <>
    <div className="reading-progress" aria-hidden="true"><span ref={progress} /></div>
    {showBackToTop && <button type="button" className="back-to-top" aria-label={text("Back to top", "返回顶部")} onClick={() => window.scrollTo({ top: 0, behavior: window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth" })}>↑</button>}
  </>;
}

type DemoErrorBoundaryProps = {
  resetKey: number;
  onError: (reason: unknown) => void;
  children: React.ReactNode;
};

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
    <div className="demo-frame-root"><Preview><Component {...args} /></Preview></div>
  </DemoErrorBoundary>;
}

export function App() {
  const { theme, resolved, setTheme } = useTheme();
  const [path, setPath] = useState(appPath);
  const [search, setSearch] = useState(false);
  const [drawer, setDrawer] = useState(false);
  const [announcement, setAnnouncement] = useState({ key: 0, message: "" });
  const drawerClose = useRef<HTMLButtonElement>(null);
  const pathRef = useRef(path);
  const pendingNavigation = useRef<{ route: string; hash: string; focus: boolean } | null>(window.location.hash ? {
    route: appPath().split("#")[0],
    hash: window.location.hash.slice(1),
    focus: false,
  } : null);
  const suppressDrawerFocusRestore = useDialogFocus(drawer, drawerClose);
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
        target = document.querySelector<HTMLElement>("#wake-docs-content h1") || document.getElementById("wake-docs-content");
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
  if (demoId) return <DemoFrame id={demoId} resolved={resolved} />;
  const routePath = path.split("#")[0].replace(/\/$/, "") || "/";
  const page = pageForPath(routePath);
  const LazyPage = page ? lazyPage(page) : null;
  const searchShortcut = /Mac|iPhone|iPad/.test(navigator.platform) ? "⌘K" : "Ctrl K";
  return <div className="docs-shell">
    <a className="skip-link" href="#wake-docs-content">{text("Skip to content", "跳到正文")}</a>
    <ReadingProgress />
    <p className="sr-only" role="status" aria-live="polite" aria-atomic="true" key={announcement.key}>{announcement.message}</p>
    <header className="topbar">
      <button type="button" className="mobile-menu icon-button" aria-haspopup="dialog" aria-expanded={drawer} aria-controls="wake-docs-drawer" onClick={() => setDrawer(true)} aria-label={text("Open navigation", "打开导航")}>☰</button>
      <Logo />
      <div className="topbar-actions">
        <button type="button" className="search-trigger" aria-haspopup="dialog" aria-controls="wake-search-dialog" aria-keyshortcuts="Control+K Meta+K" aria-expanded={search} onClick={() => setSearch(true)}><span>⌕ {text("Search", "搜索")}</span><kbd>{searchShortcut}</kbd></button>
        {siteConfig.repositoryUrl && <a className="icon-link" href={siteConfig.repositoryUrl} target="_blank" rel="noreferrer" aria-label={text("Repository", "代码仓库")}>↗</a>}
        <ThemeButton theme={theme} setTheme={setTheme} />
      </div>
    </header>
    <aside className="sidebar"><Sidebar current={page?.slug || ""} /></aside>
    {drawer && <div className="drawer-backdrop" onMouseDown={() => setDrawer(false)}><aside id="wake-docs-drawer" className="drawer" role="dialog" aria-modal="true" aria-label={text("Documentation navigation", "文档导航")} onKeyDown={trapDialogFocus} onMouseDown={(event) => event.stopPropagation()}><div className="drawer-head"><Logo onNavigate={() => { suppressDrawerFocusRestore(); setDrawer(false); }} /><button ref={drawerClose} type="button" className="icon-button" onClick={() => setDrawer(false)} aria-label={text("Close navigation", "关闭导航")}>×</button></div><Sidebar current={page?.slug || ""} close={() => { suppressDrawerFocusRestore(); setDrawer(false); }} /></aside></div>}
    <main className="content" id="wake-docs-content" tabIndex={-1}>
      {page && <MobileTableOfContents page={page} />}
      <Suspense fallback={<div className="page-loading" role="status"><span aria-hidden="true" /><span className="sr-only">{text("Loading page…", "正在加载页面…")}</span></div>}>{LazyPage ? <LazyPage /> : <NotFound />}</Suspense>
    </main>
    <aside className="toc-column">{page && <TableOfContents page={page} />}</aside>
    <Search open={search} close={() => setSearch(false)} go={go} />
  </div>;
}
