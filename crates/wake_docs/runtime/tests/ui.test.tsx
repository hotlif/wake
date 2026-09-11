import React from 'react';
import { test, expect, afterEach } from '@crab-dev/wake/test';
import { render, screen } from '@crab-dev/wake/test/react';
import type { CommonProps, HeaderProps } from '@crab-dev/wake/docs';
import { Slot, UIProvider, UIErrorBoundary } from '../ui';
import { ui } from './config';

const common: CommonProps = {
  site: { title: 'Test docs', description: '', locale: 'en', basePath: '/', homeHref: '/' },
  theme: { theme: 'light', resolved: 'light', setTheme() {} },
  route: { path: '/', page: null, navigate() {}, href: slug => slug, focusContent() {} },
};
afterEach(() => { for (const key of Object.keys(ui)) delete ui[key as keyof typeof ui]; });

test('omitted, overridden and deliberately hidden slots use one controller value', async () => {
  const seen: unknown[] = [];
  const Default = ({ site }: HeaderProps) => <h1>Default {site.title}</h1>;
  const Custom = (props: HeaderProps) => { seen.push(props.route); return <h1>Custom {props.site.title}</h1>; };
  const tree = () => <UIProvider value={common} components={ui}><Slot name="Header" fallback={Default} /></UIProvider>;
  const view = await render(tree());
  expect(screen.getByRole('heading').textContent).toBe('Default Test docs');
  ui.Header = Custom;
  await view.rerender(tree());
  expect(screen.getByRole('heading').textContent).toBe('Custom Test docs');
  expect(seen[0]).toBe(common.route);
  ui.Header = () => null;
  await view.rerender(tree());
  expect(screen.queryByRole('heading')).toBeNull();
});

test('a custom Root provider reaches a nested custom Page through a default parent', async () => {
  const Context = React.createContext('missing');
  ui.Root = ({ children }) => <Context.Provider value="shared">{children}</Context.Provider>;
  ui.Page = ({ children }) => <section><p>{React.useContext(Context)}</p>{children}</section>;
  await render(<UIProvider value={common} components={ui}><Slot name="Root" fallback={() => null}>
    <Slot name="Layout" fallback={({ children }) => <main>{children}</main>}>
      <Slot name="Page" fallback={() => null}><b>Content</b></Slot>
    </Slot>
  </Slot></UIProvider>);
  expect(screen.getByText('shared').textContent).toBe('shared');
  expect(screen.getByText('Content').closest('[data-wake-custom="Page"]')).not.toBeNull();
  expect(screen.getByText('Content').closest('[data-wake-custom="Page"]')?.hasAttribute('data-wake-reset')).toBe(true);
  expect(screen.getByText('Content').closest('[data-wake-custom="Root"]')?.hasAttribute('data-wake-reset')).toBe(false);
});

test('custom rendering errors are visible and do not invoke the default renderer', async () => {
  let fallbackCalls = 0;
  ui.Header = () => { throw new Error('custom-header-failed'); };
  await render(<UIErrorBoundary><UIProvider value={common} components={ui}><Slot name="Header" fallback={() => { fallbackCalls++; return <p>Default</p>; }} /></UIProvider></UIErrorBoundary>, { onCaughtError() {} });
  expect(screen.getByRole('alert').textContent).toContain('custom-header-failed');
  expect(fallbackCalls).toBe(0);
});
