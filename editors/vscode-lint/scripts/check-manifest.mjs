import { readFile } from 'node:fs/promises'
const manifest = JSON.parse(await readFile(new URL('../package.json', import.meta.url), 'utf8'))
const workspace = JSON.parse(await readFile(new URL('../../../package.json', import.meta.url), 'utf8'))
if (manifest.name !== 'wake-lint' || manifest.main !== './dist/extension.js') throw new Error('invalid Wake lint extension manifest')
if (manifest.version !== workspace.version) throw new Error('Wake lint extension version must match the workspace')
if (!manifest.activationEvents.some((event) => event === 'onLanguage:typescript')) throw new Error('missing TypeScript activation')
for (const language of ['javascript', 'javascriptreact', 'typescript', 'typescriptreact']) {
  if (!manifest.activationEvents.includes(`onLanguage:${language}`)) throw new Error(`missing ${language} activation`)
}
if (manifest.contributes?.configuration?.properties?.['wakeLint.serverPath']?.type !== 'string') {
  throw new Error('wakeLint.serverPath must be a string setting')
}
if (manifest.dependencies?.['vscode-languageclient'] !== '10.1.0') {
  throw new Error('Wake lint extension must pin vscode-languageclient')
}
console.log('wake-lint extension manifest ok')
