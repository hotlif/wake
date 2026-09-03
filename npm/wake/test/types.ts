import {
  BuildContext,
  DevServer,
  WakeError,
  build,
  buildLibrary,
  buildDocs,
  bundle,
  createBuildContext,
  createTestContext,
  generateCssToken,
  generateDocgen,
  generateFederationLock,
  initializeFederation,
  runTests,
  startDevServer,
  startDocsDevServer,
  type DevServerFederationUpdatedEvent,
  type FederationExposeOptions,
  type FederationOptions,
  type OutputFileKind,
  type TestOptions,
  type TestRunResult,
  type TestWorkers,
  type WakeErrorCode,
  type WakeErrorOptions,
} from '@crab-dev/wake'
import * as wakeContract from '@crab-dev/wake'
import {
  ParsedModule,
  analyze,
  parse,
  tokenize,
  transform,
} from '@crab-dev/wake/experimental'
import {
  afterAll,
  afterEach,
  beforeAll,
  beforeEach,
  clock,
  describe,
  expect,
  it,
  mock,
  network,
  test,
} from '@crab-dev/wake/test'
import * as wakeTestContract from '@crab-dev/wake/test'
import {
  act,
  cleanup,
  fireEvent,
  prettyDOM,
  render,
  renderHook,
  screen,
  userEvent,
  waitFor,
  waitForElementToBeRemoved,
  within,
} from '@crab-dev/wake/test/react'
import type { ReactElement, ReactNode } from 'react'
import {
  Button,
  Tree,
  type TreeNode,
} from '@crab-dev/wake/internal/components-runtime'
import {
  FEDERATION_DEV_LEASE_SCHEMA,
  FEDERATION_DEV_MAX_BUILD_LEASES,
  createFederationRuntime,
  type FederationDevLeaseMessage,
  type FederationManifest,
  type FederationManifestWire,
  type FederationExpose,
  type FederationRemoteLock,
  type FederationTransportContext,
  type HostSharedProvider,
  type SharedRequest,
} from '@crab-dev/wake/federation'
import { createFederatedIsolatedBridge } from '@crab-dev/wake/federation/react'

const outputFileKinds: Record<OutputFileKind, true> = {
  asset: true,
  chunk: true,
  css: true,
  declaration: true,
  entry: true,
  'federation-bootstrap': true,
  'federation-chunk': true,
  'federation-entry': true,
  'federation-manifest': true,
  'federation-shared': true,
  types: true,
  html: true,
  manifest: true,
  map: true,
}
void outputFileKinds

const wakeErrorOptions: WakeErrorOptions = {
  path: 'wake.config.toml',
  diagnostics: [],
  cause: new Error('invalid config'),
}
const constructedWakeError = new WakeError('WAKE_CONFIG', 'invalid config', wakeErrorOptions)
constructedWakeError.code.toUpperCase()
// @ts-expect-error WakeError requires the product error code and message.
new WakeError()
// @ts-expect-error BuildContext instances are factory-owned native handles.
new BuildContext()
// @ts-expect-error DevServer instances are factory-owned native handles.
new DevServer()

const wireManifestSourceMap: FederationManifestWire['remoteEntrySourceMap'] = null
const wireExposeSourceMap: FederationManifestWire['exposes']['./Button']['sourceMap'] = null
const wireOfferAsset: FederationManifestWire['shared']['offers'][number]['asset'] = null
const wireRequirementFallback: FederationManifestWire['shared']['requirements'][number]['fallback'] = null
const wireTypes: FederationManifestWire['types'] = null
const wireDevelopment: FederationManifestWire['development'] = null
// @ts-expect-error Runtime-normalized manifests represent absence with undefined, not wire null.
const normalizedManifestSourceMap: FederationManifest['remoteEntrySourceMap'] = null
// @ts-expect-error Runtime-normalized share policies represent absence with undefined.
const normalizedShareOwner: FederationManifest['shared']['offers'][number]['policy']['owner'] = null
void [
  wireManifestSourceMap,
  wireExposeSourceMap,
  wireOfferAsset,
  wireRequirementFallback,
  wireTypes,
  wireDevelopment,
  normalizedManifestSourceMap,
  normalizedShareOwner,
]

async function api() {
  const federation: FederationOptions = {
    enabled: true,
    name: 'shell',
    remotes: {
      catalog: {
        manifestUrl: 'https://catalog.example.test/wake-federation.json',
        allowedOrigins: ['https://catalog.example.test'],
        devFollow: true,
      },
    },
    exposes: {
      Button: {
        entry: 'src/button.tsx',
        mode: 'host-rendered',
        scope: 'react18',
        shadow: 'none',
        allowGlobalCss: false,
      },
      LegacyCard: {
        entry: 'src/legacy-card.tsx',
        mode: 'isolated',
        scope: 'react17',
      },
    },
    shared: {
      react: {
        scope: 'react18',
        requiredVersion: '^18.3.0',
        singleton: true,
        strict: true,
        fallback: false,
        coherenceGroup: 'react18',
        owner: 'shell',
      },
    },
  }
  const federationRuntime = createFederationRuntime({
    devReconnectMs: 5_000,
    transport: {
      fetchManifest: () => ({}),
      loadScript: () => undefined,
    },
  })
  federationRuntime.applyDevUpdate({
    schemaVersion: 'wake.federation.dev-update.v1',
    remote: 'catalog',
    oldBuildId: 'catalog-1',
    newBuildId: 'catalog-2',
    changedExposes: ['./Button'],
    generation: 2,
    action: 'isolated-remount',
  })
  await federationRuntime.loadRemote('catalog/Button', {
    container: 'shell',
    buildId: 'shell-1',
  })
  await federationRuntime.loadFederatedAsset({
    name: 'catalog',
    buildId: 'catalog-2',
    expose: './Button',
    fileName: 'chunks/button.js',
    kind: 'javascript',
  })
  const detachIsolatedStyles = await federationRuntime.attachIsolatedStyleTarget(
    'catalog/Button',
    document.createElement('div').attachShadow({ mode: 'open' }),
  )
  detachIsolatedStyles()
  const remoteDescriptor = await federationRuntime.describeRemote('catalog/Button')
  const descriptorShadow: 'open' | 'none' = remoteDescriptor.shadow
  const isolatedBridge = await createFederatedIsolatedBridge(federationRuntime, 'catalog/Button', {
    dev: { eventTarget: new EventTarget() },
  })
  isolatedBridge.status.toUpperCase()
  void descriptorShadow
  const result = await build({
    cwd: '.',
    signal: new AbortController().signal,
    federation,
  })
  await build(undefined)
  // @ts-expect-error Node request objects never accept explicit null.
  await build(null)
  // @ts-expect-error Optional request fields must be omitted instead of set to null.
  await build({ cwd: null })
  // @ts-expect-error Abort signals must be omitted instead of set to null.
  await build({ signal: null })
  // @ts-expect-error Nested Federation request fields never accept explicit null.
  await build({ federation: { enabled: true, name: 'shell', remotes: { catalog: { manifestUrl: null } } } })
  // @ts-expect-error Nested Federation shared fields never accept explicit null.
  await build({ federation: { enabled: true, name: 'shell', shared: { react: { requiredVersion: null } } } })
  result.files.forEach((file) => console.log(file.path))
  const library = await buildLibrary({ entry: 'src/index.ts' })
  library.declarationEntry.toUpperCase()
  const initialized = await initializeFederation({ cwd: '.', signal: AbortSignal.timeout(1_000) })
  initialized.projectRoot.toUpperCase()
  const initStatus: 'created' | 'unchanged' = initialized.declaration
  void initStatus
  const locked = await generateFederationLock({ cwd: '.', signal: AbortSignal.timeout(15_000) })
  locked.lock.schemaVersion.toUpperCase()
  locked.lockPath.toUpperCase()
  locked.remotes.toFixed()
  const memoryBundle = await bundle({ entry: 'src/index.ts', sourceMap: true })
  memoryBundle.code.toUpperCase()
  memoryBundle.sourceMap?.toUpperCase()
  memoryBundle.sourceMapFile?.toUpperCase()
  const defaultBundle = await bundle()
  defaultBundle.code.toUpperCase()
  const nodeBundle = await bundle({
    entry: 'src/extension.ts',
    outfile: 'dist/extension.js',
    platform: 'node',
    format: 'cjs',
    target: 'node20',
    external: ['vscode'],
    minify: true,
  })
  nodeBundle.outputFile?.toUpperCase()
  const tokenResult = await generateCssToken({ configPath: 'token.toml' })
  tokenResult.outputFile.toUpperCase()
  const docgenResult = await generateDocgen({ entry: 'src/button.tsx' })
  docgenResult.entry.toUpperCase()
  const docs = await buildDocs({ basePath: '/docs/' })
  docs.workspaces.forEach((workspace) => console.log(workspace.name, workspace.presentation))
  docs.routes.forEach((route) => console.log(route.groupId, route.sectionId))
  const workbench = await buildDocs({ mode: 'components' })
  workbench.demos.forEach((demo) => console.log(demo.component, demo.controlCount))
  const docsServer = await startDocsDevServer({ mode: 'components' })
  // @ts-expect-error Docs servers do not accept application entry overrides.
  await startDocsDevServer({ entry: 'src/index.ts' })
  // @ts-expect-error Docs servers do not accept application Federation overrides.
  await startDocsDevServer({ federation })
  docsServer.on('workspaceState', (event) => console.log(event.loaded, event.failedNames))
  docsServer.on('rebuilt', (event) => console.log(event.workspace, event.basePath))
  await docsServer.close()


  const context: BuildContext = await createBuildContext()
  await context.rebuild(['src/index.ts'])
  await context.close()

  const server: DevServer = await startDevServer({ port: 5173, federation })
  // @ts-expect-error Application servers do not accept Docs rendering modes.
  await startDevServer({ mode: 'site' })
  // @ts-expect-error Wake provides always-on Live Reload, not a configurable module-HMR API.
  await startDevServer({ hmr: true })
  server.on('diagnostic', (diagnostic) => {
    console.log(diagnostic.message)
    diagnostic.location?.lineText.toUpperCase()
    diagnostic.location?.line.toFixed()
    diagnostic.location?.column.toFixed()
  })
  server.on('federationUpdated', (event: DevServerFederationUpdatedEvent) => {
    event.remote.toUpperCase()
    event.oldBuildId?.toUpperCase()
    event.newBuildId.toUpperCase()
    event.changedExposes.forEach((expose) => expose.toUpperCase())
    event.typesHash?.toUpperCase()
    event.action.toUpperCase()
  })
  server.unref()
  await server.close()

  const testOptions: TestOptions = {
    root: '.',
    patterns: ['src/**/*.test.tsx'],
    namePattern: 'renders',
    projects: ['components'],
    environment: 'auto',
    changed: false,
    related: ['src/button.tsx'],
    coverage: true,
    updateSnapshots: 'new',
    serial: false,
    workers: '50%',
    bail: 1,
    shard: '1/2',
    seed: 'wake-types',
    shuffle: true,
    allowNoTests: false,
    browserPath: '/browser',
    headful: false,
  }
  const workerBoundaries: TestWorkers[] = [1, 'auto', '1%', '9%', '10%', '99%', '100%']
  void workerBoundaries
  // @ts-expect-error Worker percentages begin at 1%.
  const zeroPercentWorkers: TestWorkers = '0%'
  // @ts-expect-error Worker percentages end at 100%.
  const excessivePercentWorkers: TestWorkers = '101%'
  // @ts-expect-error Worker percentages use one canonical decimal spelling.
  const paddedPercentWorkers: TestWorkers = '01%'
  // @ts-expect-error Worker percentages are integers.
  const fractionalPercentWorkers: TestWorkers = '1.5%'
  void [zeroPercentWorkers, excessivePercentWorkers, paddedPercentWorkers, fractionalPercentWorkers]
  const testResult: TestRunResult = await runTests({
    ...testOptions,
    signal: new AbortController().signal,
  })
  await runTests(undefined)
  await runTests({ environment: undefined, updateSnapshots: undefined, workers: undefined })
  // @ts-expect-error Node test request objects never accept explicit null.
  await runTests(null)
  // @ts-expect-error Test environments must be omitted instead of set to null.
  await runTests({ environment: null })
  // @ts-expect-error Snapshot update modes must be omitted instead of set to null.
  await runTests({ updateSnapshots: null })
  // @ts-expect-error Worker overrides must be omitted instead of set to null.
  await runTests({ workers: null })
  // @ts-expect-error Watching is controlled by TestContext methods, not a request option.
  await runTests({ watch: true })
  // @ts-expect-error Reporter presentation is owned by the CLI, not the Node request.
  await runTests({ reporter: 'json' })
  // @ts-expect-error CLI output destinations are not accepted by the Node request.
  await runTests({ output: 'artifacts/tests' })
  const schema: 'wake.test.v1' = testResult.schemaVersion
  testResult.runId.toUpperCase()
  testResult.counts.tests.todo.toFixed()
  testResult.coverage?.summary.lines.percent.toFixed()
  testResult.coverage?.summary.blocks.covered.toFixed()
  testResult.artifacts.forEach((artifact) => artifact.id.toUpperCase())
  testResult.leaks.forEach((leak) => leak.description.toUpperCase())
  // @ts-expect-error Suite results cannot use the case-only todo status.
  const todoSuite: TestRunResult['suites'][number]['status'] = 'todo'
  // @ts-expect-error Environment result kinds are closed to the implemented runtimes.
  const nodeEnvironment: TestRunResult['environment']['kind'] = 'node'
  // @ts-expect-error Leak kinds are a closed public contract.
  const handleLeak: TestRunResult['leaks'][number]['kind'] = 'handle'
  void [todoSuite, nodeEnvironment, handleLeak]
  const testContext = await createTestContext(testOptions)
  testContext.on('runStart', (event) => event.runId.toUpperCase())
  testContext.on('testCaseResult', (event) => event.result.attempts.toFixed())
  testContext.on('suiteResult', (event) => event.result.failures.length.toFixed())
  testContext.on('runComplete', (completed) => completed.seed.toUpperCase())
  await testContext.run()
  testContext.startWatch()
  testContext.watching.valueOf()
  testContext.stopWatch()
  await testContext.close()
  // @ts-expect-error Context options are frozen at creation; only runTests accepts a signal option.
  await testContext.run({ signal: new AbortController().signal })
  void schema

  describe('Wake test types', () => {
    beforeAll(() => undefined)
    beforeEach(async () => undefined)
    afterEach(() => undefined)
    afterAll(() => undefined)
    test('supports assertions', () => expect({ answer: 42 }).toEqual({ answer: 42 }))
    it('supports async cases', async () => expect(Promise.resolve(42)).resolves.toBe(42))
    test.each([[1, 2, 3] as const])('adds values', (left, right, sum) => {
      expect(left + right).toBe(sum)
    })
  })

  const sum = mock.fn((left: number, right: number) => left + right)
  sum.implementOnce((left, right) => left - right).returnOnce(42)
  expect(sum(1, 2)).toBe(42)
  mock.replaceProperty({ value: 1 }, 'value', 2).restore()
  clock.fake({ now: 0, exclude: ['performance'] })
  await clock.advanceBy(16)
  await clock.flushMicrotasks()
  clock.restore()
  const disposeRoute = network.route(
    { method: 'GET', url: '/api/button' },
    () => ({ status: 200, body: { label: 'Save' } }),
  )
  disposeRoute()
  network.allow(/\/assets\//)
  network.requests()

  const ui = null as unknown as ReactElement
  const Wrapper = ({ children }: { children: ReactNode }) => children as ReactElement
  const rendered = await render(ui, { strict: true, wrapper: Wrapper })
  await expect(rendered.container).toMatchScreenshot('rendered component')
  await rendered.rerender(ui)
  within(rendered.container).getByText('Save')
  screen.getByRole('button', { name: 'Save' })
  await userEvent.setup().click(rendered.container)
  await fireEvent.click(rendered.container)
  const hook = await renderHook(() => 42)
  hook.result.current.toFixed()
  await act(async () => undefined)
  await waitFor(() => screen.getByText('Saved'))
  await waitForElementToBeRemoved(() => screen.queryByText('Saving'))
  prettyDOM(rendered.container).toUpperCase()
  await rendered.unmount()
  await cleanup()

  const module: ParsedModule = parse('const value = 1')
  tokenize('const value = 1')
  transform(module)
  analyze(module)
  module.dispose()
}

void api()
void WakeError
void Button
void Tree
const treeNode: TreeNode | undefined = undefined
void treeNode
const testContextCode: WakeErrorCode = 'WAKE_TEST_CONTEXT'
void testContextCode
const applicationBoundaryCodes: WakeErrorCode[] = [
  'WAKE_OUTPUT_COLLISION',
  'WAKE_WATCH_SNAPSHOT_CHANGED',
]
void applicationBoundaryCodes
const testRuntimeCodes: WakeErrorCode[] = [
  'WAKE_TEST_REACT_VERSION',
  'WAKE_TEST_BUSY',
  'WAKE_TEST_UNKNOWN_RUN',
  'WAKE_TEST_UNKNOWN_WATCH',
]
void testRuntimeCodes
const federationCodes: WakeErrorCode[] = [
  'FED_LOCK_REQUIRED',
  'FED_TYPES_INVALID',
  'FED_CONTAINER_INIT',
  'WAKE_FED_INIT_CONFIG',
  'WAKE_FED_INIT_IO',
  'WAKE_FED_INIT_CONFLICT',
]
void federationCodes
const workerOverrides: TestWorkers[] = ['auto', '50%', 2]
void workerOverrides
const genericExpose: FederationExposeOptions = { entry: 'src/value.ts' }
void genericExpose
const sharedRequestWithFallback: SharedRequest = { shareKey: 'react', fallback: false }
void sharedRequestWithFallback
const devLeaseMessage: FederationDevLeaseMessage = {
  type: 'lease',
  schemaVersion: FEDERATION_DEV_LEASE_SCHEMA,
  remote: 'catalog',
  buildIds: ['build-a'],
}
void devLeaseMessage
const devLeaseLimit: 8 = FEDERATION_DEV_MAX_BUILD_LEASES
void devLeaseLimit
const invalidDevReload: FederationDevLeaseMessage = {
  type: 'full-reload',
  schemaVersion: FEDERATION_DEV_LEASE_SCHEMA,
  remote: 'catalog',
  currentBuildId: 'build-b',
  generation: 2,
  expiredBuildId: 'build-a',
  // @ts-expect-error Reload reasons are a closed wire enum.
  reason: 'unknown',
}
void invalidDevReload
const hostProviderWithFallback: HostSharedProvider = {
  shareKey: 'react',
  version: '18.2.0',
  fallback: false,
  module: {},
}
void hostProviderWithFallback
// @ts-expect-error Reconnect cadence belongs to runtime options, not individual transport calls.
const transportContextWithReconnect: FederationTransportContext = { devReconnectMs: 250 }
void transportContextWithReconnect
const sharedOnlyRemoteLock: FederationRemoteLock = {
  buildId: 'catalog-build-a',
  manifestIntegrity: `sha384-${'A'.repeat(64)}`,
  hasExposes: false,
  allowedAssets: {},
}
void sharedOnlyRemoteLock
const legacyNullSharedOnlyRemoteLock: FederationRemoteLock = {
  ...sharedOnlyRemoteLock,
  typesIntegrity: null,
}
void legacyNullSharedOnlyRemoteLock
// @ts-expect-error Expose presence is required in every production remote lock.
const ambiguousRemoteLock: FederationRemoteLock = {
  buildId: 'catalog-build-a',
  manifestIntegrity: `sha384-${'A'.repeat(64)}`,
  allowedAssets: {},
}
void ambiguousRemoteLock
// @ts-expect-error Exposed production remotes require locked declaration integrity.
const exposedRemoteWithoutTypes: FederationRemoteLock = {
  buildId: 'catalog-build-a',
  manifestIntegrity: `sha384-${'A'.repeat(64)}`,
  hasExposes: true,
  allowedAssets: {},
}
void exposedRemoteWithoutTypes
// @ts-expect-error Null is accepted only for legacy shared-only v1 locks.
const exposedRemoteWithNullTypes: FederationRemoteLock = {
  buildId: 'catalog-build-a',
  manifestIntegrity: `sha384-${'A'.repeat(64)}`,
  hasExposes: true,
  typesIntegrity: null,
  allowedAssets: {},
}
void exposedRemoteWithNullTypes
const manifestExposeShadow: FederationExpose['shadow'] = 'none'
void manifestExposeShadow
// @ts-expect-error Disabled federation options cannot retain active container configuration.
const disabledFederation: FederationOptions = { enabled: false, name: 'shell' }
void disabledFederation
// @ts-expect-error A host-rendered React expose never owns a shadow root.
const invalidHostExpose: FederationExposeOptions = {
  entry: 'src/button.tsx',
  mode: 'host-rendered',
  scope: 'react18',
  shadow: 'open',
}
void invalidHostExpose
const invalidGenericGlobalCss: FederationExposeOptions = {
  entry: 'src/value.ts',
  mode: 'generic',
  // @ts-expect-error Only host-rendered exposes can opt in to unscoped global CSS.
  allowGlobalCss: true,
}
void invalidGenericGlobalCss
// @ts-expect-error An isolated expose must name its independent share scope.
const unscopedIsolatedExpose: FederationExposeOptions = {
  entry: 'src/legacy.tsx',
  mode: 'isolated',
}
void unscopedIsolatedExpose
// @ts-expect-error Test configuration is declarative; there is no initialization API.
void wakeContract.initTestConfig
// @ts-expect-error Seeds cross the wire as deterministic strings.
const numericSeedOptions: TestOptions = { seed: 42 }
void numericSeedOptions
// @ts-expect-error The React-first runner only accepts auto, dom, or browser.
const nodeEnvironmentOptions: TestOptions = { environment: 'node' }
void nodeEnvironmentOptions
// @ts-expect-error Wake's test API intentionally has no compatibility namespace.
void wakeTestContract.jest
// @ts-expect-error Legacy focus aliases are not part of the Wake contract.
void wakeTestContract.fit
// @ts-expect-error Upstream timer aliases are intentionally not part of Wake clock.
void wakeTestContract.clock.useFake
// @ts-expect-error Wake module mocking does not expose legacy unmock/resetModules paths.
void wakeTestContract.mock.unmock
// @ts-expect-error Network interception is route-based rather than HTTP-verb sugar.
void wakeTestContract.network.get
// @ts-expect-error Tests within one Wake DOM are sequential.
void wakeTestContract.test.concurrent
// @ts-expect-error Wake snapshots are external artifacts, not inline source rewrites.
expect('Wake').toMatchInlineSnapshot('"Wake"')
