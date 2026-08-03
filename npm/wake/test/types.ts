import {
  BuildContext,
  DevServer,
  WakeError,
  build,
  buildDocs,
  bundle,
  createBuildContext,
  startDevServer,
} from '@crab-dev/wake'
import {
  ParsedModule,
  analyze,
  parse,
  tokenize,
  transform,
} from '@crab-dev/wake/experimental'

async function api() {
  const result = await build({ cwd: '.', signal: new AbortController().signal })
  result.files.forEach((file) => console.log(file.path))
  await bundle({ entry: 'src/index.ts' })
  await buildDocs({ basePath: '/docs/' })

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
