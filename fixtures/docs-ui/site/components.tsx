import React, { useEffect, useRef, useState } from 'react';
import type * as UI from '@crab-dev/wake/docs';
import Button from '@crab-dev/rc-button';
import Dialog from '@crab-dev/rc-dialog';
import LineEdit from '@crab-dev/rc-line-edit';
import { ExampleContext } from './shared';

export function Root({ children, theme }: UI.RootProps) {
  return <ExampleContext.Provider value="shared-context"><div className="crab-root" data-theme={theme.resolved}>{children}</div></ExampleContext.Provider>;
}
export function Layout(props: UI.LayoutProps) {
  return <div className="crab-layout">{props.header}<aside>{props.navigation}</aside>
    <section>{props.children}</section><aside>{props.tableOfContents}</aside>{props.mobileNavigation}{props.searchDialog}</div>;
}
export function Header({ site, search, theme, mobileNavigation }: UI.HeaderProps) {
  return <header className="crab-header"><a href={site.homeHref}>{site.title}</a>
    <Button onClick={() => mobileNavigation.setOpen(true)}>Navigation</Button>
    <Button onClick={() => search.setOpen(true)}>Search</Button>
    <Button onClick={() => theme.setTheme(theme.resolved === 'dark' ? 'light' : 'dark')}>Theme: {theme.resolved}</Button>
  </header>;
}
export function Navigation({ groups, toggleSection, onNavigate, current }: UI.NavigationProps) {
  const link = (page: UI.PageLink) => <Button key={page.slug} aria-current={page.slug === current ? 'page' : undefined} onClick={() => onNavigate(page.slug)}>{page.title}</Button>;
  return <nav className="crab-navigation" aria-label="Documentation">{groups.map(group => <section key={group.id}>
    <h2>{group.title}</h2>{group.pages.map(link)}{group.sections.map(section => <div key={section.id}>
      <Button aria-expanded={section.expanded} onClick={() => toggleSection(section.id)}>{section.title}</Button>
      {section.expanded && section.pages.map(link)}
    </div>)}
  </section>)}</nav>;
}
export function MobileNavigation({ open, setOpen, navigation, route }: UI.MobileNavigationProps) {
  const dialog = useRef<HTMLDialogElement>(null);
  const dismissed = useRef(false);
  const focusContent = useRef(route.focusContent);
  focusContent.current = route.focusContent;
  useEffect(() => { if (open) dismissed.current = false; }, [open]);
  useEffect(() => {
    const element = dialog.current;
    const closed = () => { if (!dismissed.current) focusContent.current(); };
    element?.addEventListener('close', closed);
    return () => element?.removeEventListener('close', closed);
  }, []);
  return <Dialog ref={dialog} title="Documentation" open={open} onOpenChange={value => { dismissed.current = !value; setOpen(value); }}>{navigation}</Dialog>;
}
export function SearchDialog({ search, route }: UI.SearchDialogProps) {
  const input = useRef<HTMLInputElement>(null);
  const dialog = useRef<HTMLDialogElement>(null);
  const navigating = useRef(false);
  const focusContent = useRef(route.focusContent);
  focusContent.current = route.focusContent;
  useEffect(() => {
    const element = dialog.current;
    const closed = () => { if (navigating.current) { navigating.current = false; focusContent.current(); } };
    element?.addEventListener('close', closed);
    return () => element?.removeEventListener('close', closed);
  }, []);
  const select = (slug: string) => { navigating.current = true; search.select(slug); };
  useEffect(() => { if (search.open) input.current?.focus(); }, [search.open]);
  return <Dialog ref={dialog} title="Search documentation" open={search.open} onOpenChange={search.setOpen}>
    <LineEdit ref={input} aria-label="Search documentation" value={search.query} onChange={event => search.setQuery(event.target.value)}
      onKeyDown={event => {
        if (event.key === 'ArrowDown') { event.preventDefault(); search.setActiveIndex(Math.min(search.activeIndex + 1, search.results.length - 1)); }
        if (event.key === 'ArrowUp') { event.preventDefault(); search.setActiveIndex(Math.max(search.activeIndex - 1, 0)); }
        if (event.key === 'Enter' && search.results[search.activeIndex]) { event.preventDefault(); select(search.results[search.activeIndex].slug); }
      }} />
    {search.loading && <p role="status">Loading search…</p>}{search.error && <p role="alert">{search.error}</p>}
    <ul>{search.results.map((result, index) => <li key={result.slug + index}><Button aria-pressed={index === search.activeIndex} onClick={() => select(result.slug)}>{result.title}</Button></li>)}</ul>
  </Dialog>;
}
export function TableOfContents({ headings, activeId, variant }: UI.TableOfContentsProps) {
  return <nav className={'crab-toc crab-toc-' + variant} aria-label="On this page">{headings.map(heading => <a key={heading.id} href={heading.href} aria-current={heading.id === activeId ? 'location' : undefined}>{heading.title}</a>)}</nav>;
}
export function Page({ page, breadcrumbs, children, previous, next }: UI.PageProps) {
  return <article className="crab-page"><p>{breadcrumbs.join(' / ')}</p><h1>{page.title}</h1><p>{page.description}</p><div className="crab-content">{children}</div>
    <nav>{previous && <a href={previous.href}>{previous.title}</a>}{next && <a href={next.href}>{next.title}</a>}</nav>
  </article>;
}
export function Demo(props: UI.DemoProps) {
  const dialog = useRef<HTMLDialogElement>(null);
  const [dialogSurface, setDialogSurface] = useState(false);
  useEffect(() => { if (props.fullscreen) setDialogSurface(true); }, [props.fullscreen]);
  useEffect(() => {
    const element = dialog.current;
    const closed = () => setDialogSurface(false);
    element?.addEventListener('close', closed);
    return () => element?.removeEventListener('close', closed);
  }, []);
  // Keep one iframe while Dialog retains its exiting children for animation.
  const inDialog = props.fullscreen || dialogSurface;
  return <section className="crab-demo"><h3>{props.title}</h3><p>{props.description}</p>
    {props.error && <p role="alert">{props.error}</p>}
    {!inDialog && <div style={{ maxWidth: props.viewport === 'mobile' ? 390 : props.viewport === 'tablet' ? 768 : '100%' }}>{props.preview}</div>}
    <Button onClick={() => props.setCodeOpen(!props.codeOpen)}>Source</Button>
    <Button onClick={() => props.setViewport(props.viewport === 'mobile' ? 'responsive' : 'mobile')}>Viewport: {props.viewport}</Button>
    <Button onClick={() => props.setFullscreen(true)}>Fullscreen</Button>
    <Button onClick={props.copy}>{props.copied ? 'Copied' : 'Copy'}</Button>
    {props.codeOpen && <pre><code>{props.highlightedSource || props.source}</code></pre>}
    <Dialog ref={dialog} title={props.title} open={props.fullscreen} onOpenChange={props.setFullscreen}>
      {inDialog && props.preview}
    </Dialog>
  </section>;
}
export function CodeBlock({ title, language, children, copy, copyStatus }: UI.CodeBlockProps) {
  return <figure className="crab-code"><figcaption>{title || language}<Button onClick={copy}>{copyStatus === 'copied' ? 'Copied' : 'Copy code'}</Button></figcaption><pre><code>{children}</code></pre></figure>;
}
export function ApiTable({ symbol, properties, filter, setFilter, error }: UI.ApiTableProps) {
  return <section><h2>{symbol}</h2><LineEdit aria-label="Filter properties" value={filter} onChange={event => setFilter(event.target.value)} />
    {error && <p role="alert">{error}</p>}
    <table><thead><tr><th>Property</th><th>Type</th><th>Description</th></tr></thead><tbody>{properties.map(prop => <tr key={prop.name}><td>{prop.name}</td><td>{prop.type_text}</td><td>{prop.description}</td></tr>)}</tbody></table>
  </section>;
}
export function PageLoading() { return <p role="status">Loading page…</p>; }
export function PageError({ error, retry }: UI.PageErrorProps) { return <div role="alert"><p>{error}</p><Button onClick={retry}>Retry</Button></div>; }
export function NotFound({ site }: UI.NotFoundProps) { return <section><h1>Page not found</h1><a href={site.homeHref}>Home</a></section>; }
