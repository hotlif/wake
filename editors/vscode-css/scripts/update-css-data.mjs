import { writeFile } from 'node:fs/promises'
import { resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

import { getDefaultCSSDataProvider } from 'vscode-css-languageservice'

const provider = getDefaultCSSDataProvider()
const text = value => typeof value === 'string' ? value : value?.value ?? ''
const facts = {
  source: 'MDN data via vscode-css-languageservice',
  sourceRevision: 'vscode-css-languageservice@6.3.10',
  properties: provider.provideProperties()
    .map(property => ({
      name: property.name,
      description: text(property.description),
      values: (property.values ?? []).map(value => value.name),
    }))
    .sort((left, right) => left.name.localeCompare(right.name)),
  atRules: provider.provideAtDirectives().map(value => value.name).sort(),
  pseudos: [
    ...provider.providePseudoClasses().map(value => value.name),
    ...provider.providePseudoElements().map(value => value.name),
  ].sort(),
}
const output = resolve(
  fileURLToPath(new URL('../../..', import.meta.url)),
  'crates/wake_css_language/data/css-facts.json',
)
await writeFile(output, `${JSON.stringify(facts, null, 2)}\n`)
console.log(`Updated ${facts.properties.length} CSS properties in ${output}`)
