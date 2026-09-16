import { mkdir, readFile, writeFile } from 'node:fs/promises'
import ts from 'typescript'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = resolve(fileURLToPath(new URL('..', import.meta.url)))
const source = await readFile(resolve(root, 'src/extension.ts'), 'utf8')
await mkdir(resolve(root, 'dist'), { recursive: true })
const output = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.CommonJS },
  fileName: 'extension.ts',
}).outputText
await writeFile(resolve(root, 'dist/extension.js'), output)
