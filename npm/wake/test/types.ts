import {
  BuildContext,
  DevServer,
  WakeError,
  build,
  buildLibrary,
  buildDocs,
  bundle,
  createBuildContext,
  generateCssToken,
  generateDocgen,
  startDevServer,
  startDocsDevServer,
} from '@crab-dev/wake'
import {
  ParsedModule,
  analyze,
  parse,
  tokenize,
  transform,
} from '@crab-dev/wake/experimental'
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
  await buildDocs({ basePath: '/docs/' })
  const workbench = await buildDocs({ mode: 'components' })
  workbench.demos.forEach((demo) => console.log(demo.component, demo.controlCount))
  const docsServer = await startDocsDevServer({ mode: 'components' })
  await docsServer.close()


  const context: BuildContext = await createBuildContext()
  await context.rebuild(['src/index.ts'])
  await context.close()

  const server: DevServer = await startDevServer({ port: 5173 })
  server.on('diagnostic', (diagnostic) => console.log(diagnostic.message))
  server.unref()
  await server.close()

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
