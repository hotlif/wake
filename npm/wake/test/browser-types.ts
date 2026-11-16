import { createBrowserBuildContext, type BrowserBuildOptions } from '@crab-dev/wake/browser'

const options: BrowserBuildOptions = {
  wasmUrl: '/wake.wasm',
  federation: { enabled: true, name: 'preview', exposes: { './page': { entry: 'page.ts' } } },
}
void createBrowserBuildContext(options)
// @ts-expect-error Browser imports must not bring Node ambient globals into a DOM-only consumer.
void process
// @ts-expect-error Isolated exposes need a separate mounting lifecycle.
options.federation.exposes['./legacy'] = { entry: 'legacy.ts', mode: 'isolated', scope: 'legacy' }
// @ts-expect-error Browser shared modules must use registered host providers.
options.federation.shared = { react: { fallback: true } }
