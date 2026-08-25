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
  runTests,
  startDevServer,
  startDocsDevServer,
  type TestOptions,
  type TestRunResult,
  type TestWorkers,
  type WakeErrorCode,
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

async function api() {
  const result = await build({ cwd: '.', signal: new AbortController().signal })
  result.files.forEach((file) => console.log(file.path))
  const library = await buildLibrary({ entry: 'src/index.ts' })
  library.declarationEntry.toUpperCase()
  const memoryBundle = await bundle({ entry: 'src/index.ts', sourceMap: true })
  memoryBundle.code.toUpperCase()
  memoryBundle.sourceMap?.toUpperCase()
  memoryBundle.sourceMapFile?.toUpperCase()
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
  const workbench = await buildDocs({ mode: 'components' })
  workbench.demos.forEach((demo) => console.log(demo.component, demo.controlCount))
  const docsServer = await startDocsDevServer({ mode: 'components' })
  docsServer.on('workspaceState', (event) => console.log(event.loaded, event.failedNames))
  docsServer.on('rebuilt', (event) => console.log(event.workspace, event.basePath))
  await docsServer.close()


  const context: BuildContext = await createBuildContext()
  await context.rebuild(['src/index.ts'])
  await context.close()

  const server: DevServer = await startDevServer({ port: 5173 })
  server.on('diagnostic', (diagnostic) => {
    console.log(diagnostic.message)
    diagnostic.location?.lineText.toUpperCase()
    diagnostic.location?.line.toFixed()
    diagnostic.location?.column.toFixed()
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
    reporter: 'json',
    output: 'artifacts/tests',
    allowNoTests: false,
    browserPath: '/browser',
    headful: false,
  }
  const testResult: TestRunResult = await runTests({
    ...testOptions,
    signal: new AbortController().signal,
  })
  const schema: 'wake.test.v1' = testResult.schemaVersion
  testResult.runId.toUpperCase()
  testResult.counts.tests.todo.toFixed()
  testResult.coverage?.summary.lines.percent.toFixed()
  testResult.coverage?.summary.blocks.covered.toFixed()
  testResult.artifacts.forEach((artifact) => artifact.id.toUpperCase())
  testResult.leaks.forEach((leak) => leak.description.toUpperCase())
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
const testRuntimeCodes: WakeErrorCode[] = [
  'WAKE_TEST_REACT_VERSION',
  'WAKE_TEST_BUSY',
  'WAKE_TEST_UNKNOWN_RUN',
  'WAKE_TEST_UNKNOWN_WATCH',
]
void testRuntimeCodes
const workerOverrides: TestWorkers[] = ['auto', '50%', 2]
void workerOverrides
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
