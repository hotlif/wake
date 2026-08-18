import { EventEmitter } from 'node:events'

export type WakeErrorCode =
  | 'WAKE_CONFIG'
  | 'WAKE_PARSE'
  | 'WAKE_BUILD'
  | 'WAKE_IO'
  | 'WAKE_CANCELLED'
  | 'WAKE_UNSUPPORTED_PLATFORM'
  | 'WAKE_INTERNAL'
  | 'WAKE_TOKEN_IO'
  | 'WAKE_TOKEN_CONFIG'
  | 'WAKE_TOKEN_IMPORT'
  | 'WAKE_TOKEN_CYCLE'
  | 'WAKE_TOKEN_REF'
  | 'WAKE_DOCGEN_IO'
  | 'WAKE_DOCGEN_CONFIG'
  | 'WAKE_DOCGEN_ENTRY'
  | 'WAKE_DOCGEN_TYPE'
  | 'WAKE_LIBRARY_BUILD'
  | 'WAKE_LIBRARY_TYPE'
  | 'WAKE_LIBRARY_OUTPUT'

export interface Diagnostic {
  severity: 'error' | 'warning' | 'note' | 'help'
  code?: string
  message: string
  path?: string
  start?: number
  end?: number
  notes?: string[]
}

export class WakeError extends Error {
  readonly code: WakeErrorCode
  readonly path?: string
  readonly diagnostics?: Diagnostic[]
  override readonly cause?: unknown
}

export interface ProjectOptions {
  cwd?: string
  configPath?: string
  signal?: AbortSignal
}

export interface BuildOptions extends ProjectOptions {
  entry?: string
  outdir?: string
  cache?: boolean
  sourceMap?: boolean
}

export type BundlePlatform = 'browser' | 'node'
export type BundleFormat = 'iife' | 'cjs'
export type NodeTarget = `node${number}` | `node${number}.${number}`

export interface BundleOptions extends ProjectOptions {
  entry?: string
  outfile?: string
  platform?: BundlePlatform
  format?: BundleFormat
  target?: NodeTarget
  external?: string[]
  minify?: boolean
  sourceMap?: boolean
  cache?: boolean
}

export interface OutputFile {
  path: string
  kind: 'entry' | 'chunk' | 'css' | 'declaration' | 'asset' | 'html' | 'map'
  bytes: number
}

export interface BuildResult {
  success: true
  moduleCount: number
  updatedModuleCount: number
  cachedModuleCount: number
  durationMs: number
  outputDir?: string
  code?: string
  files: OutputFile[]
  diagnostics: Diagnostic[]
}

export interface BundleResult {
  success: true
  moduleCount: number
  updatedModuleCount: number
  cachedModuleCount: number
  durationMs: number
  outputFile?: string
  code: string
  sourceMap?: string
  sourceMapFile?: string
  files: OutputFile[]
  diagnostics: Diagnostic[]
}

export interface LibraryBuildOptions {
  cwd?: string
  entry?: string
  signal?: AbortSignal
}

export interface LibraryBuildResult extends BuildResult {
  outputDir: string
  esmEntry: string
  cjsEntry: string
  declarationEntry: string
  cssEntry?: string
}

export interface GenerateCssTokenOptions {
  cwd?: string
  configPath?: string
  signal?: AbortSignal
}

export interface GenerateCssTokenResult {
  success: true
  durationMs: number
  outputFile: string
  files: OutputFile[]
}

export interface GenerateDocgenOptions {
  cwd?: string
  entry?: string
  signal?: AbortSignal
}

export interface GenerateDocgenResult {
  success: true
  durationMs: number
  entry: string
  outputFile: string
  files: OutputFile[]
}

export type DocsMode = 'site' | 'components'

  export interface DocsRoute {
    id: string
    file: string
    title: string
    description: string
    kind: 'overview' | 'tutorial' | 'guide' | 'reference' | 'component'
    group: string
    groupId: string
    section: string
    sectionId: string
    slug: string
    status: string
    draft: boolean
    hidden: boolean
    headings: Array<{ depth: number; title: string; id: string }>
  }
export interface DocsDemo {
  id: string
  title: string
  group: string
  component: string
  order: number
  controlCount: number
  warnings: string[]
}

export interface DocsBuildOptions extends ProjectOptions {
  outdir?: string
  basePath?: string
  mode?: DocsMode
}

export interface DocsBuildResult extends BuildResult {
  routes: DocsRoute[]
  mode: DocsMode
  demos: DocsDemo[]
}

export interface DevServerOptions extends ProjectOptions {
  entry?: string
  host?: string
  port?: number
  open?: boolean
}
export interface DocsDevServerOptions extends DevServerOptions {
  mode?: DocsMode
}

export interface DevServerRebuildStartEvent {
  type: 'rebuildStart'
  changedPaths: string[]
}

export interface DevServerRebuiltEvent {
  type: 'rebuilt'
  initial: boolean
  modules: number
  updatedModules: number
  cachedModules: number
  chunks: number
  assets: number
  durationMs: number
}

export class BuildContext {
  readonly closed: boolean
  rebuild(changedPaths?: string[], options?: { signal?: AbortSignal }): Promise<BuildResult>
  rebuild(options?: { signal?: AbortSignal }): Promise<BuildResult>
  close(): Promise<void>
  [Symbol.asyncDispose](): Promise<void>
}

export class DevServer extends EventEmitter {
  readonly url: string
  close(): Promise<void>
  waitUntilClosed(): Promise<void>
  unref(): this
  [Symbol.asyncDispose](): Promise<void>
  on(event: 'rebuildStart', listener: (event: DevServerRebuildStartEvent) => void): this
  on(event: 'rebuilt', listener: (event: DevServerRebuiltEvent) => void): this
  on(event: 'diagnostic', listener: (diagnostic: Diagnostic) => void): this
  on(event: 'closed', listener: () => void): this
}

export function version(): string
export function bundle(options: BundleOptions): Promise<BundleResult>
export function build(options?: BuildOptions): Promise<BuildResult>
export function buildLibrary(options?: LibraryBuildOptions): Promise<LibraryBuildResult>
export function generateCssToken(options?: GenerateCssTokenOptions): Promise<GenerateCssTokenResult>
export function generateDocgen(options?: GenerateDocgenOptions): Promise<GenerateDocgenResult>
export function createBuildContext(options?: BuildOptions): Promise<BuildContext>
export function startDevServer(options?: DevServerOptions): Promise<DevServer>
export function buildDocs(options?: DocsBuildOptions): Promise<DocsBuildResult>
export function startDocsDevServer(options?: DocsDevServerOptions): Promise<DevServer>

declare const wake: {
  BuildContext: typeof BuildContext
  DevServer: typeof DevServer
  WakeError: typeof WakeError
  build: typeof build
  buildLibrary: typeof buildLibrary
  buildDocs: typeof buildDocs
  bundle: typeof bundle
  generateCssToken: typeof generateCssToken
  generateDocgen: typeof generateDocgen
  createBuildContext: typeof createBuildContext
  startDevServer: typeof startDevServer
  startDocsDevServer: typeof startDocsDevServer
  version: typeof version
}

export default wake
