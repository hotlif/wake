import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import test from 'node:test'

const root = new URL('..', import.meta.url)
const readJson = async path => JSON.parse(await readFile(new URL(path, root), 'utf8'))

test('manifest exposes the stable Crab CSS contract', async () => {
  const manifest = await readJson('package.json')
  assert.equal(manifest.name, 'crab-css')
  assert.equal(manifest.publisher, 'crab-dev')
  assert.equal(manifest.version, '0.1.0')
  assert.equal(manifest.engines.vscode, '^1.96.0')
  assert.deepEqual(manifest.extensionKind, ['workspace'])
  assert.equal(manifest.contributes.configuration.properties['crabCss.validation.mode'].default, 'onType')
  assert.equal(manifest.contributes.configuration.properties['crabCss.format.enable'].default, true)
})

test('grammar injects canonical tags and delegates content to CSS', async () => {
  const grammar = await readJson('syntaxes/crab-css.injection.json')
  assert.match(grammar.patterns[0].begin, /css/)
  assert.match(grammar.patterns[0].begin, /keyframes/)
  assert.match(grammar.patterns[0].begin, /globalStyle/)
  assert.equal(grammar.patterns[0].patterns.at(-1).include, 'source.css')
})

test('client and manifest use only @crab-dev/css', async () => {
  const source = await readFile(new URL('src/extension.ts', root), 'utf8')
  assert.match(source, /@crab-dev\/css/)
  assert.doesNotMatch(source, /@wake\/css|@vanilla-extract/)
})
