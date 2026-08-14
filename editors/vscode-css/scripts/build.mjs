import { build } from 'esbuild'
import { fileURLToPath } from 'node:url'

await build({
  entryPoints: [fileURLToPath(new URL('../src/extension.ts', import.meta.url))],
  outfile: fileURLToPath(new URL('../dist/extension.js', import.meta.url)),
  bundle: true,
  external: ['vscode'],
  format: 'cjs',
  minify: true,
  platform: 'node',
  sourcemap: false,
  target: 'node20',
  logLevel: 'info',
})

await build({
  entryPoints: [fileURLToPath(new URL('../test/suite/index.ts', import.meta.url))],
  outfile: fileURLToPath(new URL('../.test-dist/suite/index.js', import.meta.url)),
  bundle: true,
  external: ['vscode'],
  format: 'cjs',
  platform: 'node',
  target: 'node20',
  logLevel: 'info',
})
