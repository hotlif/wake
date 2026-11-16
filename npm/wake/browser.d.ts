import type { FederationManifestWire, FederationPackageIdentity, FederationRuntime, ModuleNamespace, SharedPolicy } from './federation.mjs'

export type BrowserExposeOptions =
  | { entry: string; mode?: 'generic'; scope?: string; shadow?: 'none' }
  | { entry: string; mode: 'host-rendered'; scope: string; shadow?: 'none'; allowGlobalCss?: boolean }
export type BrowserSharedOptions = Partial<SharedPolicy> & { requiredVersion?: string; fallback: false }

export interface BrowserBuildOptions {
  /** Deploy the version-matched @crab-dev/wake/browser/wasm artifact on your origin. */
  wasmUrl: string | URL
  workerUrl?: string | URL
  /** Required for every shared key: actual registered host provider identities, not preview-generated labels. */
  hostShared?: Record<string, FederationPackageIdentity>
  federation: {
    enabled: true
    name: string
    exposes: Record<string, BrowserExposeOptions>
    shared?: Record<string, BrowserSharedOptions>
  }
  aliases?: Record<string, string>
  define?: Record<string, string>
  assetBase?: '/__wake_preview__/assets/'
}

export interface BrowserDiagnostic {
  severity: string
  code: string | null
  message: string
  path: string | null
  /** UTF-8 byte offsets, with an exclusive end. */
  start: number | null
  end: number | null
  /** One-based line and Unicode scalar column. */
  line: number | null
  column: number | null
  lineText: string | null
  notes: string[]
}

export interface BrowserBuildOutput {
  manifest: FederationManifestWire
  files: { path: string; bytes: Uint8Array; mime: string }[]
  diagnostics: BrowserDiagnostic[]
  moduleCount: number
  updatedModuleCount: number
  cachedModuleCount: number
  /** Worker wall time, including ABI serialization. */
  durationMs: number
}

export class BrowserBuildError extends Error {
  constructor(code: string, message: string, diagnostics?: BrowserDiagnostic[])
  readonly code: string
  readonly diagnostics: BrowserDiagnostic[]
}

export interface BrowserBuildContext {
  /** Copies the complete snapshot. Superseded or aborted requests reject with WAKE_CANCELLED. */
  build(files: Readonly<Record<string, string | Uint8Array>>, options?: { signal?: AbortSignal }): Promise<BrowserBuildOutput>
  dispose(): void
}
export function createBrowserBuildContext(options: BrowserBuildOptions): Promise<BrowserBuildContext>

export interface BrowserPreviewView {
  /** Synchronous, non-throwing host DOM swap after preparation succeeds. */
  commit(): void
  /** Dispose only this view; a later view may already be active. */
  dispose(): void | Promise<void>
}
export interface BrowserPreview {
  /** Prepare off-screen. An unchanged build does not call prepare again. */
  update(output: BrowserBuildOutput, prepare: (exports: ModuleNamespace) => BrowserPreviewView | Promise<BrowserPreviewView>): Promise<{ buildId: string; cleanupError?: string }>
  dispose(): Promise<void>
}
export function createBrowserPreview(options: {
  registration: ServiceWorkerRegistration
  name: string
  expose?: string
  runtime?: FederationRuntime
}): Promise<BrowserPreview>
