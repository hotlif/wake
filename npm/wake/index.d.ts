import { EventEmitter } from 'node:events'
import type { FederationErrorCode as FederationContractErrorCode } from './federation.mjs'

export type WakeErrorCode =
  | 'WAKE_CONFIG'
  | 'WAKE_PARSE'
  | 'WAKE_BUILD'
  | 'WAKE_IO'
  | 'WAKE_CANCELLED'
  | 'WAKE_UNSUPPORTED_PLATFORM'
  | 'WAKE_INTERNAL'
  | 'WAKE_OUTPUT_COLLISION'
  | 'WAKE_WATCH_SNAPSHOT_CHANGED'
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
  | 'WAKE_TEST_CONFIG'
  | 'WAKE_TEST_DISCOVERY'
  | 'WAKE_TEST_RUNTIME'
  | 'WAKE_TEST_TIMEOUT'
  | 'WAKE_TEST_SNAPSHOT'
  | 'WAKE_TEST_COVERAGE'
  | 'WAKE_TEST_HOST'
  | 'WAKE_TEST_UNSUPPORTED'
  | 'WAKE_TEST_CONTEXT'
  | 'WAKE_TEST_DOM'
  | 'WAKE_TEST_BROWSER'
  | 'WAKE_TEST_REACT_VERSION'
  | 'WAKE_TEST_NETWORK'
  | 'WAKE_TEST_LEAK'
  | 'WAKE_TEST_BUSY'
  | 'WAKE_TEST_UNKNOWN_RUN'
  | 'WAKE_TEST_UNKNOWN_WATCH'
  | 'WAKE_FED_INIT_CONFIG'
  | 'WAKE_FED_INIT_IO'
  | 'WAKE_FED_INIT_CONFLICT'
  | FederationContractErrorCode

export interface DiagnosticLocation {
  /** One-based source line. */
  line: number
  /** One-based Unicode-scalar column. */
  column: number
  /** One-based line containing the exclusive end position. */
  endLine: number
  /** One-based Unicode-scalar column of the exclusive end position. */
  endColumn: number
  /** Exact source line without its line terminator. */
  lineText: string
  label?: string
}

export interface Diagnostic {
  severity: 'error' | 'warning' | 'note' | 'help'
  code?: string
  message: string
  path?: string
  start?: number
  end?: number
  location?: DiagnosticLocation
  notes?: string[]
}

export interface WakeErrorOptions {
  path?: string
  diagnostics?: Diagnostic[]
  cause?: unknown
}

export class WakeError extends Error {
  constructor(code: WakeErrorCode, message: string, options?: WakeErrorOptions)
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

export interface FederationControlOptions {
  cwd?: string
  signal?: AbortSignal
}

export type FederationInitFileStatus = 'created' | 'unchanged'

export interface FederationInitResult {
  projectRoot: string
  declarationPath: string
  typesIndexPath: string
  declaration: FederationInitFileStatus
  typesIndex: FederationInitFileStatus
}

export interface FederationLockRemoteBase {
  manifestUrl: string
  buildId: string
  manifestIntegrity: string
  allowedAssets: Record<string, string>
}

export type FederationLockRemote = FederationLockRemoteBase &
  (
    | { hasExposes: false; typesIntegrity?: string }
    | { hasExposes: true; typesIntegrity: string }
  )

export interface FederationLock {
  schemaVersion: 'wake.federation.lock.v1'
  remotes: Record<string, FederationLockRemote>
}

export interface FederationLockResult {
  projectRoot: string
  lockPath: string
  remotes: number
  lock: FederationLock
}

/** A remote Wake container resolved through its immutable federation manifest. */
export interface FederationRemoteOptions {
  manifestUrl: string
  allowedOrigins?: string[]
  /** Follow new remote build IDs in development. Production builds remain lock-bound. */
  devFollow?: boolean
}

export type FederationExposeMode = 'generic' | 'host-rendered' | 'isolated'
export type FederationShadowMode = 'none' | 'open'

export interface FederationGenericExposeOptions {
  entry: string
  mode?: 'generic'
  scope?: string
  shadow?: 'none'
}

export interface FederationHostRenderedExposeOptions {
  entry: string
  mode: 'host-rendered'
  /** Share scope containing the host-owned React coherence group. */
  scope: string
  shadow?: 'none'
  /** Permit ordinary unscoped CSS to enter the host document. Defaults to false. */
  allowGlobalCss?: boolean
}

export interface FederationIsolatedExposeOptions {
  entry: string
  mode: 'isolated'
  /** A non-default scope owned by the isolated component root. */
  scope: string
  /** Defaults to an open shadow root. */
  shadow?: 'open'
}

export type FederationExposeOptions =
  | FederationGenericExposeOptions
  | FederationHostRenderedExposeOptions
  | FederationIsolatedExposeOptions

export interface FederationSharedOptions {
  scope?: string
  requiredVersion?: string
  singleton?: boolean
  strict?: boolean
  fallback?: boolean
  coherenceGroup?: string
  /** Required for deterministic singleton selection in production builds. */
  owner?: string
}

export interface FederationEnabledOptions {
  enabled: true
  name: string
  remotes?: Record<string, FederationRemoteOptions>
  exposes?: Record<string, FederationExposeOptions>
  shared?: Record<string, FederationSharedOptions>
}

export interface FederationDisabledOptions {
  enabled?: false
  name?: never
  remotes?: never
  exposes?: never
  shared?: never
}

/** Wake-native browser federation options. Omitted or `{ enabled: false }` disables federation. */
export type FederationOptions = FederationEnabledOptions | FederationDisabledOptions

export interface BuildOptions extends ProjectOptions {
  entry?: string
  outdir?: string
  cache?: boolean
  sourceMap?: boolean
  federation?: FederationOptions
}

export type TestEnvironment = 'auto' | 'dom' | 'browser'
export type SnapshotUpdateMode = 'none' | 'new' | 'all'
type TestWorkerNonZeroDigit = '1' | '2' | '3' | '4' | '5' | '6' | '7' | '8' | '9'
type TestWorkerDigit = '0' | TestWorkerNonZeroDigit
export type TestWorkerPercentage =
  | `${TestWorkerNonZeroDigit}%`
  | `${TestWorkerNonZeroDigit}${TestWorkerDigit}%`
  | '100%'
export type TestWorkers = number | 'auto' | TestWorkerPercentage

export interface TestOptions {
  root?: string
  patterns?: string[]
  namePattern?: string
  projects?: string[]
  environment?: TestEnvironment
  changed?: boolean
  related?: string[]
  coverage?: boolean
  updateSnapshots?: SnapshotUpdateMode
  serial?: boolean
  /** A positive integer, `"auto"`, or an integer percentage from `"1%"` through `"100%"`. */
  workers?: TestWorkers
  /** A non-negative integer no greater than 4,294,967,295. */
  bail?: number
  shard?: `${number}/${number}`
  seed?: string
  shuffle?: boolean
  allowNoTests?: boolean
  browserPath?: string
  headful?: boolean
}

export type TestCaseStatus = 'passed' | 'failed' | 'skipped' | 'todo'
export type TestSuiteStatus = 'passed' | 'failed' | 'skipped'
export type TestTerminationReason =
  | 'completed'
  | 'cancelled'
  | 'bail'
  | 'watch-restart'
  | 'host-crash'
  | 'timeout'
  | 'oom'
  | 'internal-error'

export interface TestLocation {
  path: string
  line: number
  column: number
  endLine: number | null
  endColumn: number | null
}

export interface TestDiff {
  expected: string | null
  received: string | null
  unified: string | null
}

export interface TestFailure {
  message: string
  code: string | null
  stack: string | null
  location: TestLocation | null
  diff: TestDiff | null
}

export interface TestCaseResult {
  id: string
  name: string
  fullName: string
  status: TestCaseStatus
  durationMs: number
  assertions: number
  attempts: number
  location: TestLocation | null
  failures: TestFailure[]
}

export interface SnapshotSummary {
  added: number
  matched: number
  unmatched: number
  updated: number
  obsolete: number
  filesRemoved: number
}

export interface CoverageMetric {
  covered: number
  total: number
  percent: number
}

export interface CoverageMetrics {
  lines: CoverageMetric
  functions: CoverageMetric
  blocks: CoverageMetric
}

export interface CoverageFile extends CoverageMetrics {
  path: string
}

export interface CoverageResult {
  summary: CoverageMetrics
  files: CoverageFile[]
  reportArtifactIds: string[]
}

export interface BrowserEnvironmentInfo {
  name: string
  version: string
  headless: boolean
}

export interface TestEnvironmentInfo {
  kind: 'dom' | 'browser'
  react: string | null
  reactDom: string | null
  v8: string
  browser: BrowserEnvironmentInfo | null
}

export type TestMetadataValue =
  | null
  | boolean
  | number
  | string
  | TestMetadataValue[]
  | { [key: string]: TestMetadataValue }

export interface TestArtifact {
  id: string
  kind: string
  path: string
  suiteId: string | null
  testId: string | null
  metadata: Record<string, TestMetadataValue>
}

export interface TestDiagnostic {
  severity: 'error' | 'warning' | 'note' | 'help'
  code: string
  message: string
  path: string | null
  location: TestLocation | null
  notes?: string[]
}

export interface TestLeak {
  kind: 'timer' | 'listener' | 'task' | 'socket' | 'network' | 'other'
  description: string
  location: TestLocation | null
  stack: string | null
}

export interface TestSuiteResult {
  id: string
  path: string
  name: string | null
  project: string | null
  environment: TestEnvironmentInfo | null
  status: TestSuiteStatus
  durationMs: number
  tests: TestCaseResult[]
  failures: TestFailure[]
  snapshot: SnapshotSummary | null
}

export interface TestStatusCounts {
  total: number
  passed: number
  failed: number
  skipped: number
}

export interface TestCaseStatusCounts extends TestStatusCounts {
  todo: number
}

export interface TestRunCounts {
  suites: TestStatusCounts
  tests: TestCaseStatusCounts
}

export interface TestRunResult {
  schemaVersion: 'wake.test.v1'
  runId: string
  success: boolean
  seed: string
  durationMs: number
  terminationReason: TestTerminationReason
  environment: TestEnvironmentInfo
  suites: TestSuiteResult[]
  counts: TestRunCounts
  snapshot: SnapshotSummary
  coverage: CoverageResult | null
  leaks: TestLeak[]
  artifacts: TestArtifact[]
  diagnostics: TestDiagnostic[]
}

export interface TestRunStartEvent {
  runId: string
  watching: boolean
}

export interface TestCaseResultEvent {
  runId: string
  suiteId: string
  result: TestCaseResult
}

export interface TestSuiteResultEvent {
  runId: string
  result: TestSuiteResult
}

export class TestContext extends EventEmitter {
  private constructor()
  readonly watching: boolean
  readonly closed: boolean
  run(): Promise<TestRunResult>
  startWatch(): this
  stopWatch(): this
  close(): Promise<void>
  [Symbol.asyncDispose](): Promise<void>
  on(event: 'runStart', listener: (event: TestRunStartEvent) => void): this
  on(event: 'testCaseResult', listener: (event: TestCaseResultEvent) => void): this
  on(event: 'suiteResult', listener: (event: TestSuiteResultEvent) => void): this
  on(event: 'runComplete', listener: (result: TestRunResult) => void): this
  on(event: 'diagnostic', listener: (diagnostic: TestDiagnostic) => void): this
  on(event: 'closed', listener: () => void): this
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

export type OutputFileKind =
  | 'asset'
  | 'chunk'
  | 'css'
  | 'declaration'
  | 'entry'
  | 'federation-bootstrap'
  | 'federation-chunk'
  | 'federation-entry'
  | 'federation-manifest'
  | 'federation-shared'
  | 'types'
  | 'html'
  | 'manifest'
  | 'map'

export interface OutputFile {
  path: string
  kind: OutputFileKind
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

export interface DocsWorkspaceBuildInfo {
  name: string
  root: string
  basePath: string
  mode: 'components'
  presentation: 'embedded' | 'standalone'
  demos: number
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
  workspaces: DocsWorkspaceBuildInfo[]
}

export interface DevServerOptions extends ProjectOptions {
  entry?: string
  host?: string
  /** An integer from 0 through 65,535. */
  port?: number
  open?: boolean
  federation?: FederationOptions
}
export interface DocsDevServerOptions extends ProjectOptions {
  host?: string
  /** An integer from 0 through 65,535. */
  port?: number
  open?: boolean
  mode?: DocsMode
}

export interface DevServerRebuildStartEvent {
  type: 'rebuildStart'
  changedPaths: string[]
  workspace?: string
  basePath?: string
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
  workspace?: string
  basePath?: string
}

export interface DevServerWorkspaceStateEvent {
  type: 'workspaceState'
  total: number
  loaded: number
  failed: number
  current?: string
  failedNames: string[]
}

export interface DevServerFederationUpdatedEvent {
  type: 'federationUpdated'
  remote: string
  oldBuildId?: string
  newBuildId: string
  changedExposes: string[]
  typesHash?: string
  action: 'types-only' | 'isolated-remount' | 'full-reload'
}

export class BuildContext {
  private constructor()
  readonly closed: boolean
  rebuild(changedPaths?: string[], options?: { signal?: AbortSignal }): Promise<BuildResult>
  rebuild(options?: { signal?: AbortSignal }): Promise<BuildResult>
  close(): Promise<void>
  [Symbol.asyncDispose](): Promise<void>
}

export class DevServer extends EventEmitter {
  private constructor()
  readonly url: string
  close(): Promise<void>
  waitUntilClosed(): Promise<void>
  unref(): this
  [Symbol.asyncDispose](): Promise<void>
  on(event: 'rebuildStart', listener: (event: DevServerRebuildStartEvent) => void): this
  on(event: 'rebuilt', listener: (event: DevServerRebuiltEvent) => void): this
  on(event: 'workspaceState', listener: (event: DevServerWorkspaceStateEvent) => void): this
  on(event: 'federationUpdated', listener: (event: DevServerFederationUpdatedEvent) => void): this
  on(event: 'diagnostic', listener: (diagnostic: Diagnostic) => void): this
  on(event: 'closed', listener: () => void): this
}

export function version(): string
export function bundle(options?: BundleOptions): Promise<BundleResult>
export function build(options?: BuildOptions): Promise<BuildResult>
export function buildLibrary(options?: LibraryBuildOptions): Promise<LibraryBuildResult>
export function generateCssToken(options?: GenerateCssTokenOptions): Promise<GenerateCssTokenResult>
export function generateDocgen(options?: GenerateDocgenOptions): Promise<GenerateDocgenResult>
export function initializeFederation(options?: FederationControlOptions): Promise<FederationInitResult>
export function generateFederationLock(options?: FederationControlOptions): Promise<FederationLockResult>
export function createBuildContext(options?: BuildOptions): Promise<BuildContext>
export function runTests(options?: TestOptions & { signal?: AbortSignal }): Promise<TestRunResult>
export function createTestContext(options?: TestOptions): Promise<TestContext>
export function startDevServer(options?: DevServerOptions): Promise<DevServer>
export function buildDocs(options?: DocsBuildOptions): Promise<DocsBuildResult>
export function startDocsDevServer(options?: DocsDevServerOptions): Promise<DevServer>

declare const wake: {
  BuildContext: typeof BuildContext
  DevServer: typeof DevServer
  TestContext: typeof TestContext
  WakeError: typeof WakeError
  build: typeof build
  buildLibrary: typeof buildLibrary
  buildDocs: typeof buildDocs
  bundle: typeof bundle
  generateCssToken: typeof generateCssToken
  generateDocgen: typeof generateDocgen
  initializeFederation: typeof initializeFederation
  generateFederationLock: typeof generateFederationLock
  createBuildContext: typeof createBuildContext
  runTests: typeof runTests
  createTestContext: typeof createTestContext
  startDevServer: typeof startDevServer
  startDocsDevServer: typeof startDocsDevServer
  version: typeof version
}

export default wake
