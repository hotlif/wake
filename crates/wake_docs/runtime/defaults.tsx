import "./styles.css";
import React, { useCallback, useEffect, useId, useRef, useState } from "react";
import type * as UI from "@crab-dev/wake/docs";
import { siteConfig } from "@@wake/docs/config.tsx";
import { docsRouteHref } from "./routes.mjs";
export function DefaultRoot({ children }: UI.RootProps) {
  useEffect(() => {
    const margin = document.body.style.margin;
    document.body.style.margin = '0';
    return () => { document.body.style.margin = margin; };
  }, []);
  return <>{children}</>;
}
export function DefaultLayout({ header, navigation, mobileNavigation, tableOfContents, searchDialog, children }: UI.LayoutProps) {
  return <div className="docs-shell">
    <a className="skip-link" href="#wake-docs-content">{text('Skip to content', '跳到正文')}</a>
    <ReadingProgress />
    {header}<aside className="sidebar">{navigation}</aside>{mobileNavigation}
    <div className="content">{children}</div><aside className="toc-column">{tableOfContents}</aside>{searchDialog}
  </div>;
}
export function DefaultHeader({ site, theme, search, mobileNavigation }: UI.HeaderProps) {
  return <header className="topbar">
    <button type="button" className="mobile-menu icon-button" aria-haspopup="dialog" aria-expanded={mobileNavigation.open} aria-controls="wake-docs-drawer" onClick={() => mobileNavigation.setOpen(true)} aria-label={text('Open navigation', '打开导航')}>☰</button>
    <Logo />
    <div className="topbar-actions">
      <button type="button" className="search-trigger" aria-haspopup="dialog" aria-expanded={search.open} aria-controls="wake-search-dialog" aria-keyshortcuts="Control+K Meta+K" onClick={() => search.setOpen(true)}><span>⌕ {text('Search', '搜索')}</span><kbd>{search.shortcut}</kbd></button>
      {site.repositoryUrl && <a className="icon-link" href={site.repositoryUrl} target="_blank" rel="noreferrer" aria-label={text('Repository', '代码仓库')}>↗</a>}
      <ThemeButton theme={theme.theme} setTheme={theme.setTheme} />
    </div>
  </header>;
}
export function DefaultNavigation({ groups, current, toggleSection, onNavigate }: UI.NavigationProps) {
  const root = useRef<HTMLElement>(null);
  const prefix = useId().replace(/:/g, '');
  useEffect(() => { root.current?.querySelector('[aria-current="page"]')?.scrollIntoView({ block: 'nearest' }); }, [current]);
  const link = (page: UI.PageLink, nested = false) => <a key={page.slug} href={page.href} className={(page.slug === current ? 'active' : '') + (nested ? ' nested' : '')} aria-current={page.slug === current ? 'page' : undefined}
    onClick={event => { if (event.button === 0 && !event.metaKey && !event.ctrlKey && !event.shiftKey && !event.altKey) { event.preventDefault(); onNavigate(page.slug); } }}><span>{page.title}</span></a>;
  return <nav ref={root} className="sidebar-nav" aria-label={text('Documentation', '文档导航')}>
    {groups.map(group => <div className="nav-group" key={group.id}><h2>{group.title}</h2>{group.pages.map(page => link(page))}
      {group.sections.map(section => <div className="nav-section" key={section.id}>
        <button type="button" className="nav-section-toggle" aria-expanded={section.expanded} aria-controls={prefix + section.id} onClick={() => toggleSection(section.id)}><span>{section.title}</span><i aria-hidden="true">›</i></button>
        <div className="nav-section-pages" id={prefix + section.id} hidden={!section.expanded}>{section.pages.map(page => link(page, true))}</div>
      </div>)}
    </div>)}
  </nav>;
}
export function DefaultMobileNavigation({ open, setOpen, navigation, route }: UI.MobileNavigationProps) {
  const closeButton = useRef<HTMLButtonElement>(null);
  const suppressFocusRestore = useDialogFocus(open, closeButton);
  const previousPath = useRef(route.path);
  useEffect(() => { if (route.path !== previousPath.current) suppressFocusRestore(); previousPath.current = route.path; }, [route.path]);
  if (!open) return null;
  return <div className="drawer-backdrop" onMouseDown={() => setOpen(false)}><aside id="wake-docs-drawer" className="drawer" role="dialog" aria-modal="true" aria-label={text('Documentation navigation', '文档导航')} onKeyDown={trapDialogFocus} onMouseDown={event => event.stopPropagation()}>
    <div className="drawer-head"><Logo onNavigate={() => { suppressFocusRestore(); setOpen(false); }} /><button ref={closeButton} type="button" className="icon-button" onClick={() => setOpen(false)} aria-label={text('Close navigation', '关闭导航')}>×</button></div>
    <div onClick={event => { if (event.defaultPrevented && (event.target as Element).closest('a[href]')) suppressFocusRestore(); }}>{navigation}</div>
  </aside></div>;
}
export function DefaultTableOfContents({ headings, activeId, variant }: UI.TableOfContentsProps) {
  const details = useRef<HTMLDetailsElement>(null);
  if (!headings.length) return null;
  const links = headings.map(heading => <a key={heading.id} href={heading.href} className={(heading.depth === 3 ? 'nested ' : '') + (heading.id === activeId ? 'active' : '')} aria-current={heading.id === activeId ? 'location' : undefined}>{heading.title}</a>);
  return variant === 'mobile'
    ? <details className="mobile-toc" ref={details}><summary><span>{text('On this page', '本页目录')}</span><small>{headings.length}</small></summary><nav aria-label={text('On this page', '本页目录')} onClick={() => details.current?.removeAttribute('open')}>{links}</nav></details>
    : <nav className="toc" aria-label={text('On this page', '本页目录')}><h2>{text('On this page', '本页目录')}</h2>{links}</nav>;
}
export function DefaultPageLoading() { return <div className="page-loading" role="status"><span aria-hidden="true" /><span className="sr-only">{text('Loading page…', '正在加载页面…')}</span></div>; }
export function DefaultPageError({ error, retry }: UI.PageErrorProps) { return <div className="frame-error" role="alert"><h1>{text('Page compilation failed', '页面编译失败')}</h1><pre>{error}</pre><button type="button" onClick={retry}>{text('Retry', '重试')}</button></div>; }
type Theme = UI.Theme;
const text = (en: string, zh: string) => siteConfig.locale.toLowerCase().startsWith('zh') ? zh : en;
const isChinese = siteConfig.locale.toLowerCase().startsWith('zh');
const docsHref = (slug: string) => docsRouteHref(siteConfig.basePath, slug) || '/';
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

function codeLanguageName(language: string): string {
  return ({ javascript: "JavaScript", typescript: "TypeScript", jsx: "JSX", tsx: "TSX", rust: "Rust", bash: "Shell", powershell: "PowerShell", python: "Python", sql: "SQL", json: "JSON", jsonc: "JSONC", toml: "TOML", yaml: "YAML", css: "CSS", scss: "SCSS", html: "HTML", mdx: "MDX", markdown: "Markdown", text: text("Text", "文本") } as Record<string, string>)[language] || language.toUpperCase();
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

function DemoSource({ source, highlighted, language, id, labelledBy, panel = false }: { source: string; highlighted: React.ReactNode; language: string; id?: string; labelledBy?: string; panel?: boolean }) {
  const rendered = highlighted || source || text("Loading source…", "正在加载源码…");
  const lineCount = Math.max(1, source.split(/\r?\n/).length);
  return <pre id={id} className={"demo-code" + (panel ? " demo-panel" : "")} role={panel ? "region" : undefined} aria-labelledby={labelledBy} tabIndex={0} data-language={language} data-line-numbers={lineCount > 1 ? "true" : "false"}><code>{rendered}</code></pre>;
}

export function DefaultCodeBlock({ language, code, title, children, copyStatus, copy }: UI.CodeBlockProps) {
  const lineCount = Math.max(1, code.split(/\r?\n/).length);
  const copyLabel = copyStatus === "copied" ? text("Copied", "已复制") : copyStatus === "error" ? text("Copy failed", "复制失败") : text("Copy", "复制");
  return <figure className="code-block" data-language={language} data-line-numbers={lineCount > 1 ? "true" : "false"}>
    <figcaption className="code-toolbar">
      <span className="code-identity">{title && <strong>{title}</strong>}<small>{codeLanguageName(language)}</small></span>
      <button type="button" className={copyStatus === "copied" ? "is-copied" : copyStatus === "error" ? "is-error" : ""} onClick={copy} aria-label={text("Copy code", "复制代码")} aria-live="polite">{copyLabel}</button>
    </figcaption>
    <pre tabIndex={0}><code>{children}</code></pre>
  </figure>;
}

export function DefaultPage({ page: meta, children, breadcrumbs: crumbs, previous, next }: UI.PageProps) {
  return <article className="mdx-page">
    <header className="page-header">
      <nav className="breadcrumbs" aria-label={text("Breadcrumb", "面包屑")}>{crumbs.map((crumb, index) => <React.Fragment key={crumb}><span>{crumb}</span>{index < crumbs.length - 1 && <i aria-hidden="true">/</i>}</React.Fragment>)}</nav>
      {(meta.status !== "stable" || meta.draft) && <div className="eyebrow"><StatusBadge status={meta.status} />{meta.draft && <span className="status status-draft">{statusText("draft")}</span>}</div>}
      <h1 tabIndex={-1}>{meta.title}</h1>
      {meta.description && <p className="page-description">{meta.description}</p>}
    </header>
    <div className="mdx-content">{children}</div>
    <nav className="page-pager" aria-label={text('Page navigation', '页面导航')}>
      {previous ? <a className="page-pager-link page-pager-previous" href={previous.href}><small>{text('Previous', '上一篇')}</small><strong>{previous.title}</strong></a> : <span />}
      {next && <a className="page-pager-link page-pager-next" href={next.href}><small>{text('Next', '下一篇')}</small><strong>{next.title}</strong></a>}
    </nav>
  </article>;
}

function StatusBadge({ status }: { status: string }) { return <span className={"status status-" + status}>{statusText(status)}</span>; }
export function DefaultDemo({ preview: suppliedPreview, title, description, background, padding,
  loading, error, source, highlightedSource, sourceLanguage, codeOpen, setCodeOpen, fullscreen, setFullscreen,
  viewport, setViewport, copied, copy }: UI.DemoProps) {
  const meta = { title, description, background, padding };
  const demo = { title };
  const visible = !loading;
  const fullscreenClose = useRef<HTMLButtonElement>(null);
  const domId = 'wake-demo-' + useId().replace(/:/g, '');
  const titleId = domId + '-title', previewPanelId = domId + '-preview', codeToggleId = domId + '-toggle', codePanelId = domId + '-code', playgroundTitleId = domId + '-playground';
  useDialogFocus(fullscreen, fullscreenClose);
  useEffect(() => {
    if (!fullscreen) return;
    const close = (event: KeyboardEvent) => { if (event.key === 'Escape') setFullscreen(false); };
    window.addEventListener('keydown', close);
    return () => window.removeEventListener('keydown', close);
  }, [fullscreen, setFullscreen]);
  const viewportOptions: Array<{ id: UI.Viewport; label: string; size: string }> = [
    { id: 'responsive', label: text('Desktop', '电脑'), size: text('Responsive', '自适应') },
    { id: 'tablet', label: text('Tablet', '平板'), size: '768 × 540' },
    { id: 'mobile', label: text('Mobile', '手机'), size: '390 × 700' },
  ];
  const preview = <div className={"demo-stage demo-bg-" + meta.background} style={{ padding: meta.padding === "none" ? 0 : meta.padding === "sm" ? 12 : meta.padding === "lg" ? 32 : 20 }}>
    <div className={"demo-viewport demo-viewport-" + viewport} data-viewport={viewport}>
      {viewport === "responsive" && <div className="demo-browser-chrome" aria-hidden="true"><span className="demo-window-dots"><i /><i /><i /></span><span className="demo-address-bar">localhost / preview</span><span className="demo-browser-menu">•••</span></div>}
      {viewport === "tablet" && <div className="demo-tablet-details" aria-hidden="true"><span className="demo-tablet-camera" /><span className="demo-tablet-button" /><span className="demo-tablet-port" /></div>}
      {viewport === "mobile" && <div className="demo-phone-details" aria-hidden="true"><span className="demo-phone-island" /><span className="demo-phone-volume-one" /><span className="demo-phone-volume-two" /><span className="demo-phone-power" /><span className="demo-phone-home" /></div>}
      <div className="demo-screen">
        {suppliedPreview}
        {!visible && <div className="demo-skeleton" />}
        {error && <div className="demo-error" role="alert"><strong>{text("Demo error", "演示错误")}</strong><span>{error}</span></div>}
      </div>
    </div>
  </div>;
  return <div className="demo-card">
    <div id={previewPanelId} className="demo-panel" role="region" aria-label={text("Demo preview", "组件预览")} tabIndex={0}>
      {!fullscreen && preview}
    </div>
    <div className="demo-titlebar">
      <div><strong id={titleId}>{meta.title || demo.title}</strong>{meta.description && <span>{meta.description}</span>}</div>
    </div>
    <div className="demo-toolbar">
      <div className="demo-device-tools" role="group" aria-label={text("Preview device", "预览设备")}>
        {viewportOptions.map((option) => <button className="demo-icon-button" aria-pressed={viewport === option.id} aria-label={option.label + " · " + option.size} data-tooltip={option.label + " · " + option.size} type="button" key={option.id} onClick={() => setViewport(option.id)}><i className={"viewport-icon viewport-icon-" + option.id} aria-hidden="true" /></button>)}
      </div>
      <div className="demo-toolbar-actions">
        <button className="demo-icon-button" id={codeToggleId} type="button" aria-label={codeOpen ? text("Collapse source", "收起源码") : text("Expand source", "展开源码")} aria-expanded={codeOpen} aria-controls={codePanelId} data-tooltip={codeOpen ? text("Collapse source", "收起源码") : text("Expand source", "展开源码")} onClick={() => setCodeOpen(!codeOpen)}><i className="demo-tool-icon demo-tool-icon-code" aria-hidden="true" /></button>
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

export function DefaultApiTable({ symbol, description, properties: props, total, filter, setFilter, error, inherited, warnings }: UI.ApiTableProps) {
  const statusId = 'api-status-' + useId().replace(/:/g, '');
  const doc = { description, props: { length: total }, inherited, warnings };
  if (error) return <div className="callout error" role="alert">{error}</div>;
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

export function DefaultSearchDialog({ search }: UI.SearchDialogProps) {
  const { open, query, results, activeIndex: active, setQuery, setActiveIndex: setActive } = search;
  const close = () => search.setOpen(false);
  const input = useRef<HTMLInputElement>(null);
  const suppressFocusRestore = useDialogFocus(open, input);
  useEffect(() => { if (open) document.getElementById('wake-search-result-' + active)?.scrollIntoView({ block: 'nearest' }); }, [active, query, open]);
  const choose = (slug: string) => { suppressFocusRestore(); search.select(slug); };
  const onKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActive(Math.min(active + 1, Math.max(0, results.length - 1)));
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActive(Math.max(0, active - 1));
    } else if (event.key === "Enter" && results[active]) {
      event.preventDefault();
      choose(results[active].slug);
    }
  };
  if (!open) return null;
  return <div className="search-backdrop" role="presentation" onMouseDown={close}>
    <div id="wake-search-dialog" className="search-dialog" role="dialog" aria-modal="true" aria-label={text("Search documentation", "搜索文档")} onKeyDown={trapDialogFocus} onMouseDown={(event) => event.stopPropagation()}>
      <div className="search-input"><span aria-hidden="true">⌕</span><input ref={input} value={query} onChange={(event) => { setQuery(event.target.value); setActive(0); }} onKeyDown={onKeyDown} role="combobox" aria-label={text("Search documentation", "搜索文档")} aria-autocomplete="list" aria-expanded={open} aria-controls="wake-search-results" aria-activedescendant={results[active] ? "wake-search-result-" + active : undefined} placeholder={text("Search pages, headings, commands, and props…", "搜索页面、章节、命令和属性…")} /><kbd>Esc</kbd></div>
      {search.error && <p role="alert">{search.error}</p>}
      <div className="search-results" id="wake-search-results" role="listbox" aria-label={text("Search results", "搜索结果")}>
        {results.map((item: any, index: number) => <div id={"wake-search-result-" + index} className={"search-result " + (index === active ? "active" : "")} role="option" aria-selected={index === active} key={item.slug + index} onMouseMove={() => setActive(index)} onMouseDown={(event) => event.preventDefault()} onClick={() => choose(item.slug)}><span><strong>{item.title}</strong><small>{item.detail}</small></span><em>{item.kind}</em></div>)}
        {!results.length && <p className="empty-search">{text("No results for", "没有找到")} “{query}”</p>}
      </div>
    </div>
  </div>;
}

export function DefaultNotFound() {
  const description = text("The requested documentation page could not be found.", "找不到请求的文档页面。");
  const title = text("Page not found", "页面不存在");
  return <div className="not-found"><span>404</span><h1 tabIndex={-1}>{title}</h1><p>{text("The document may have moved or is still being written.", "文档可能已移动，或仍在编写中。")}</p><a href={docsHref("/")}>{text("Back to documentation", "返回文档首页")}</a></div>;
}
