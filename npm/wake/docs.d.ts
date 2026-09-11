import type { ComponentType, ReactNode } from 'react';

export type Theme = 'light' | 'dark' | 'system';
export type ResolvedTheme = 'light' | 'dark';
export type Viewport = 'responsive' | 'tablet' | 'mobile';
export interface SiteInfo {
  readonly title: string;
  readonly description: string;
  readonly locale: string;
  readonly logo?: string | null;
  readonly repositoryUrl?: string | null;
  readonly basePath: string;
  readonly homeHref: string;
  readonly accentColor?: string | null;
}
export interface ThemeState {
  readonly theme: Theme;
  readonly resolved: ResolvedTheme;
  setTheme(theme: Theme): void;
}
export interface PageLink { readonly slug: string; readonly title: string; readonly href: string }
export interface Heading { readonly id: string; readonly title: string; readonly depth: number; readonly href: string }
export interface PageInfo extends PageLink {
  readonly description: string;
  readonly status: string;
  readonly draft: boolean;
  readonly headings: readonly Heading[];
}
export interface RouteState {
  readonly path: string;
  readonly page: PageInfo | null;
  /** A docs slug, optionally including a heading fragment. */
  navigate(slug: string): void;
  href(slug: string): string;
  /** Restore page focus after a custom dialog completes its close transition. */
  focusContent(): void;
}
export interface SearchResult { readonly slug: string; readonly title: string; readonly detail: string; readonly kind: string; readonly href: string }
export interface SearchState {
  readonly open: boolean;
  readonly query: string;
  readonly results: readonly SearchResult[];
  readonly activeIndex: number;
  readonly loading: boolean;
  readonly error: string | null;
  readonly shortcut: string;
  setOpen(open: boolean): void;
  setQuery(query: string): void;
  setActiveIndex(index: number): void;
  select(slug: string): void;
}
export interface CommonProps { readonly site: SiteInfo; readonly theme: ThemeState; readonly route: RouteState }
export interface RootProps extends CommonProps { readonly children: ReactNode }
export interface LayoutProps extends CommonProps {
  readonly header: ReactNode;
  readonly navigation: ReactNode;
  readonly mobileNavigation: ReactNode;
  readonly tableOfContents: ReactNode;
  readonly searchDialog: ReactNode;
  /** Includes Wake's accessible content target, loading and error boundary. Render once. */
  readonly children: ReactNode;
}
export interface HeaderProps extends CommonProps {
  readonly search: SearchState;
  readonly mobileNavigation: { readonly open: boolean; setOpen(open: boolean): void };
}
export interface NavigationSection { readonly id: string; readonly title: string; readonly pages: readonly PageLink[]; readonly expanded: boolean; readonly active: boolean }
export interface NavigationGroup { readonly id: string; readonly title: string; readonly pages: readonly PageLink[]; readonly sections: readonly NavigationSection[] }
export interface NavigationProps extends CommonProps {
  readonly groups: readonly NavigationGroup[];
  readonly current: string;
  toggleSection(id: string): void;
  /** Closes mobile navigation when appropriate; route navigation remains Wake-owned. */
  onNavigate(slug: string): void;
}
export interface MobileNavigationProps extends CommonProps {
  readonly open: boolean;
  readonly navigation: ReactNode;
  setOpen(open: boolean): void;
}
export interface SearchDialogProps extends CommonProps { readonly search: SearchState }
export interface TableOfContentsProps extends CommonProps {
  readonly headings: readonly Heading[];
  readonly activeId: string;
  readonly variant: 'desktop' | 'mobile';
  navigate(id: string): void;
}
export interface PageProps extends CommonProps {
  readonly page: PageInfo;
  readonly breadcrumbs: readonly string[];
  readonly previous: PageLink | null;
  readonly next: PageLink | null;
  readonly children: ReactNode;
}
export interface CodeBlockProps extends CommonProps {
  readonly language: string;
  readonly code: string;
  readonly title?: string;
  readonly children: ReactNode;
  readonly copyStatus: 'idle' | 'copied' | 'error';
  copy(): Promise<void>;
}
export interface ApiProperty {
  readonly name: string;
  readonly description: string;
  readonly type_text: string;
  readonly default_value?: string | null;
  readonly required: boolean;
  readonly deprecated: boolean;
  readonly since?: string | null;
}
export interface ApiTableProps extends CommonProps {
  readonly symbol: string;
  readonly description: string;
  readonly properties: readonly ApiProperty[];
  readonly total: number;
  readonly inherited: readonly { readonly name: string; readonly source: string; readonly type_text: string }[];
  readonly warnings: readonly string[];
  readonly filter: string;
  readonly error: string | null;
  setFilter(filter: string): void;
}
export interface DemoProps extends CommonProps {
  readonly id: string;
  readonly title: string;
  readonly description: string;
  readonly loading: boolean;
  readonly background: string;
  readonly padding: string;
  readonly error: string | null;
  readonly source: string;
  readonly highlightedSource: ReactNode;
  readonly sourceLanguage: string;
  readonly codeOpen: boolean;
  readonly fullscreen: boolean;
  readonly viewport: Viewport;
  readonly copied: boolean;
  /** Render once; place in the full-screen surface while fullscreen is true. */
  readonly preview: ReactNode;
  setCodeOpen(open: boolean): void;
  setFullscreen(open: boolean): void;
  setViewport(viewport: Viewport): void;
  copy(): Promise<void>;
}
export interface PageLoadingProps extends CommonProps {}
export interface PageErrorProps extends CommonProps { readonly error: string; retry(): void }
export interface NotFoundProps extends CommonProps {}
export interface DocsUI {
  readonly Root?: ComponentType<RootProps>;
  readonly Layout?: ComponentType<LayoutProps>;
  readonly Header?: ComponentType<HeaderProps>;
  readonly Navigation?: ComponentType<NavigationProps>;
  readonly MobileNavigation?: ComponentType<MobileNavigationProps>;
  readonly SearchDialog?: ComponentType<SearchDialogProps>;
  readonly TableOfContents?: ComponentType<TableOfContentsProps>;
  readonly Page?: ComponentType<PageProps>;
  readonly Demo?: ComponentType<DemoProps>;
  readonly CodeBlock?: ComponentType<CodeBlockProps>;
  readonly ApiTable?: ComponentType<ApiTableProps>;
  readonly PageLoading?: ComponentType<PageLoadingProps>;
  readonly PageError?: ComponentType<PageErrorProps>;
  readonly NotFound?: ComponentType<NotFoundProps>;
}
export function defineDocsUI<T extends DocsUI>(components: T & Record<Exclude<keyof T, keyof DocsUI>, never>): Readonly<T>;
