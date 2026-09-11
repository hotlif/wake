import React, { createContext, useContext } from 'react';
import type { CommonProps, DocsUI } from '@crab-dev/wake/docs';

const CommonContext = createContext<CommonProps | null>(null);
const RegistryContext = createContext<DocsUI>({});
const DefaultAncestorContext = createContext(false);
export function UIProvider({ value, components = {}, children }: { value: CommonProps; components?: DocsUI; children: React.ReactNode }) {
  return <CommonContext.Provider value={value}><RegistryContext.Provider value={components}>{children}</RegistryContext.Provider></CommonContext.Provider>;
}
export function useCommon(): CommonProps {
  const value = useContext(CommonContext);
  if (!value) throw new Error('Wake Docs UI requires the application controller');
  return value;
}

/** A slot boundary owns presentation only. Controllers remain above this component. */
export function Slot({ name, fallback: Fallback, ...props }: {
  name: keyof DocsUI;
  fallback: React.ComponentType<any>;
  [key: string]: any;
}) {
  const common = useCommon();
  const ui = useContext(RegistryContext);
  const hasDefaultAncestor = useContext(DefaultAncestorContext);
  const Custom = ui[name] as React.ComponentType<any> | undefined;
  const Component = Custom || Fallback;
  return Custom
    ? <DefaultAncestorContext.Provider value={false}><span data-wake-custom={name} data-wake-reset={hasDefaultAncestor ? '' : undefined}
        style={hasDefaultAncestor ? { all: 'initial', display: 'contents' } : { display: 'contents' }}><Component {...common} {...props} /></span></DefaultAncestorContext.Provider>
    : <DefaultAncestorContext.Provider value={true}><span data-wake-default={name} data-theme={common.theme.resolved}
        style={{ display: 'contents', ...(common.site.accentColor ? { '--wake-accent': common.site.accentColor } : {}) }}><Component {...common} {...props} /></span></DefaultAncestorContext.Provider>;
}

/** Protects the entire custom shell, including its own error renderer. */
export class UIErrorBoundary extends React.Component<{ children: React.ReactNode }, { error: string }> {
  state = { error: '' };
  static getDerivedStateFromError(error: unknown) { return { error: String(error) }; }
  render() {
    return this.state.error
      ? <div role="alert"><h1>Wake Docs UI error</h1><pre>{this.state.error}</pre><button onClick={() => location.reload()}>Retry</button></div>
      : this.props.children;
  }
}
