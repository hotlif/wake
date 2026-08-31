# @crab-dev/wake

Wake is a Rust-native web builder exposed as both a Node.js library and the
`wake` command.

Interactive `dev` and `docs dev` sessions use the full-screen TUI when stdin
and stderr are terminals. The command line accepts `help`, `clear`, `open`,
and `quit` (with optional `/` prefixes), keeps in-memory history, supports
paste, and copies mouse-selected screen text to the clipboard. Use
`--ui plain` for stable non-interactive logs; Ctrl-C always interrupts.
Compiler failures include `path:line:column`, a numbered source line, and a caret when the diagnostic
has a valid source span. The same structured location is available from Node build errors and server
`diagnostic` events.

```sh
npm install --save-dev @crab-dev/wake
npx wake build
```

```js
import { build, buildLibrary, bundle, generateCssToken, generateDocgen, startDevServer } from '@crab-dev/wake'

await build({ cwd: process.cwd() })
await buildLibrary({ cwd: process.cwd(), entry: 'src/index.ts' })

await generateCssToken({ cwd: process.cwd(), configPath: 'token.toml' })
await generateDocgen({ cwd: process.cwd(), entry: 'src/button.tsx' })

const server = await startDevServer({ port: 5173 })
console.log(server.url)
await server.waitUntilClosed()
```

Compiler primitives are intentionally isolated under
`@crab-dev/wake/experimental`. A `ParsedModule` is an opaque, disposable
native handle and cannot be cloned, persisted, or transferred to a Worker.

The stable API also includes in-memory `bundle()`, incremental
`createBuildContext()`, documentation builds and development servers. Build
and server operations accept `AbortSignal`; long-lived contexts and servers
support explicit close methods and JavaScript disposal protocols.

Node-hosted tools can request an exact CommonJS artifact without Web output:

```js
await bundle({
  entry: 'src/extension.ts',
  outfile: 'dist/extension.js',
  platform: 'node',
  format: 'cjs',
  target: 'node20',
  external: ['vscode'],
  minify: true,
  sourceMap: true,
})
```

`platform: 'node'` defaults to CommonJS and `node20`, so `format` and `target`
can be omitted. `minify: true` and `sourceMap: true` can be combined; a source
map is emitted for optimized output without switching to an unminified pipeline.
Minification uses Wake's centralized Closure-style ordered-pass/fixed-point
pipeline; this does not expose Closure `ADVANCED`, externs, or whole-world
configuration.
`bundle()` returns a dedicated result whose `code` is always a string. With
`sourceMap: true`, `sourceMap` is returned in memory; when
`outfile` is present Wake also writes `<outfile>.map`, exposes
`sourceMapFile`, and appends the matching `sourceMappingURL`.

Component packages can generate design-token TypeScript without a Node-based
generator by running `wake library token` or calling `generateCssToken()`. The
generator supports recursive package imports in Yarn PnP and `node_modules`,
rejects missing references and cycles, and atomically writes only the output
declared by `build.output`.

`wake library build` and `buildLibrary()` emit `esm/index.mjs`,
`cjs/index.cjs`, `declarations/index.d.ts`, and optional `css/index.css` from
the native library graph. Outputs are staged and committed together; unsafe
public type inference or static-style failures preserve the previous build.

Component API metadata can be generated without `react-docgen` by running
`wake library docgen` or calling `generateDocgen()`. Entry resolution follows
the CLI override, package configuration, then the default export from
`src/index.ts`; failures leave the previous `public/docgen.json` untouched.

Full documentation:

- [Node.js API](https://github.com/hotlif/wake/blob/canary/docs/reference/node-api.mdx)
- [Experimental API](https://github.com/hotlif/wake/blob/canary/docs/reference/experimental-api.mdx)
- [CLI reference](https://github.com/hotlif/wake/blob/canary/docs/reference/cli.mdx)

Wake supports Node.js 22.14 through 26 on Windows x64, Linux glibc x64/arm64,
and macOS x64/arm64. Installing the package never compiles Rust code and does
not run a postinstall script.
