# @crab-dev/wake

Wake is a Rust-native web builder exposed as both a Node.js library and the
`wake` command.

```sh
npm install --save-dev @crab-dev/wake
npx wake build
```

```js
import { build, bundle, startDevServer } from '@crab-dev/wake'

await build({ cwd: process.cwd() })

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
})
```

`platform: 'node'` defaults to CommonJS and `node20`, so `format` and `target`
can be omitted. `bundle()` returns a dedicated result whose `code` is always a
string. With `sourceMap: true`, `sourceMap` is returned in memory; when
`outfile` is present Wake also writes `<outfile>.map`, exposes
`sourceMapFile`, and appends the matching `sourceMappingURL`.

Full documentation:

- [Node.js API](https://github.com/hotlif/wake/blob/canary/docs/reference/node-api.mdx)
- [Experimental API](https://github.com/hotlif/wake/blob/canary/docs/reference/experimental-api.mdx)
- [CLI reference](https://github.com/hotlif/wake/blob/canary/docs/reference/cli.mdx)

Wake supports Node.js 22.14 through 26 on Windows x64, Linux glibc x64/arm64,
and macOS x64/arm64. Installing the package never compiles Rust code and does
not run a postinstall script.
