import assert from 'node:assert/strict'
import { readFile } from 'node:fs/promises'
import { test } from '@crab-dev/wake/test'

const root = new URL('..', import.meta.url)
const readJson = async path => JSON.parse(await readFile(new URL(path, root), 'utf8'))

test('manifest exposes the stable Crab CSS contract', async () => {
  const manifest = await readJson('package.json')
  assert.equal(manifest.name, 'crab-css')
  assert.equal(manifest.private, true)
  assert.equal(manifest.publisher, 'crab-dev')
  assert.equal(manifest.version, '0.1.3')
  assert.equal(manifest.engines.vscode, '^1.96.0')
  assert.deepEqual(manifest.extensionKind, ['workspace'])
  assert.deepEqual(manifest.contributes.semanticTokenTypes, [
    {
      id: 'crabCssValue',
      description: 'A CSS identifier used as a value inside an @crab-dev/css template.',
    },
  ])
  assert.deepEqual(manifest.contributes.semanticTokenScopes, [
    {
      scopes: {
        crabCssValue: ['support.constant.property-value.css'],
      },
    },
  ])
  assert.equal(manifest.contributes.semanticTokenTypes[0].superType, undefined)
  assert.equal(manifest.contributes.configuration.properties['crabCss.validation.mode'].default, 'onType')
  assert.equal(manifest.contributes.configuration.properties['crabCss.format.enable'].default, true)
})

test('manifest delegates highlighting exclusively to semantic analysis', async () => {
  const manifest = await readJson('package.json')
  assert.equal(manifest.contributes.grammars, undefined)
  assert.ok(!manifest.files.includes('syntaxes/**'))
})

test('client and manifest use only @crab-dev/css', async () => {
  const source = await readFile(new URL('src/extension.ts', root), 'utf8')
  assert.ok(!source.includes('getText().includes'))
  assert.ok(!source.includes('workspaceUsesCrabCss'))
  assert.ok(!source.includes('manifestSectionHasCrabCss'))
  assert.ok(!source.includes('@wake/css'))
  assert.ok(!source.includes('@vanilla-extract'))
})

test('compiled client preserves the automatic suggestion command', async () => {
  const compiled = await readFile(new URL('dist/extension.js', root), 'utf8')
  assert.ok(compiled.includes('editor.action.triggerSuggest'))
  assert.ok(compiled.includes('onDidChangeTextEditorSelection'))
})

test('language highlighting has no spelling or byte-scanner fallback', async () => {
  const language = await readFile(new URL('../../crates/wake_css_language/src/lib.rs', root), 'utf8')
  const syntax = await readFile(new URL('../../crates/wake_css/src/syntax.rs', root), 'utf8')
  const nesting = await readFile(new URL('../../crates/wake_css_in_js/src/nesting.rs', root), 'utf8')
  assert.ok(language.includes('use wake_css::syntax::'))
  assert.ok(language.includes('CssSyntaxTree::parse'))
  assert.ok(syntax.includes('pub struct CssSyntaxTree'))
  assert.ok(nesting.includes('use wake_css::syntax::'))
  assert.ok(!language.includes('semantic_tokens_in_segment'))
  assert.ok(!language.includes('unknown_property_diagnostics'))
  assert.ok(!nesting.includes('as_bytes()'))
})

test('CSS syntax consumers contain no regular-expression fallback', async () => {
  const paths = [
    '../../crates/wake_css/src/lib.rs',
    '../../crates/wake_css/src/syntax.rs',
    '../../crates/wake_css_in_js/src/lib.rs',
    '../../crates/wake_css_in_js/src/nesting.rs',
    '../../crates/wake_css_language/src/lib.rs',
    '../../npm/css/index.mjs',
    '../../npm/css/index.cjs',
    'src/extension.ts',
    'scripts/build.mjs',
    'scripts/check-vsix.mjs',
    'scripts/clean.mjs',
    'scripts/package-vsix.mjs',
    'scripts/update-css-data.mjs',
    'test/run-extension-host.mjs',
    'test/suite/index.ts',
  ]
  const forbidden = [
    'regex::',
    'Regex::',
    'new RegExp',
    'RegExp(',
    '.match(',
    '.matchAll(',
    '.replace(/',
    '.split(/',
    '.search(',
  ]
  for (const path of paths) {
    const source = await readFile(new URL(path, root), 'utf8')
    for (const marker of forbidden) {
      assert.ok(!source.includes(marker), `${path} contains forbidden syntax matcher ${marker}`)
    }
  }
})
