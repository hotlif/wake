import { readFile } from 'node:fs/promises'
import { resolve } from 'node:path'

const nativeRules = new Set([
  'js/no-debugger', 'js/eqeqeq', 'js/no-empty', 'js/no-duplicate-case', 'js/no-dupe-keys',
  'js/no-constant-condition', 'js/no-unused-vars', 'js/no-undef', 'js/no-redeclare',
  'js/no-shadow', 'js/no-use-before-define', 'js/no-unreachable', 'js/consistent-return',
  'js/no-fallthrough', 'js/no-unsafe-finally', 'js/no-cond-assign', 'js/no-self-assign',
  'js/no-self-compare', 'js/no-constant-binary-expression', 'js/no-duplicate-imports', 'js/no-async-promise-executor',
  'js/no-promise-executor-return', 'js/no-sparse-arrays', 'js/valid-typeof', 'js/use-isnan',
  'js/prefer-const', 'js/no-var', 'js/no-console', 'ts/no-explicit-any',
  'ts/no-non-null-assertion', 'ts/consistent-type-imports', 'ts/consistent-type-exports',
  'ts/no-namespace', 'ts/no-empty-interface', 'ts/array-type', 'ts/ban-ts-comment',
  'ts/no-floating-promises', 'ts/no-misused-promises', 'ts/await-thenable',
  'ts/no-unnecessary-type-assertion', 'ts/no-unsafe-assignment', 'ts/no-unsafe-call',
  'ts/no-unsafe-member-access', 'ts/no-unsafe-return', 'ts/restrict-template-expressions',
  'ts/switch-exhaustiveness-check', 'import/no-unresolved', 'import/no-cycle',
  'import/no-duplicates', 'import/no-restricted-paths', 'import/no-extraneous-dependencies',
  'import/order', 'react-hooks/exhaustive-deps', 'react-hooks/rules-of-hooks',
  'react/jsx-uses-vars', 'react/jsx-no-undef', 'react/jsx-key', 'react/no-array-index-key',
  'react/jsx-no-duplicate-props', 'react/no-danger', 'react/no-children-prop',
  'react/self-closing-comp', 'a11y/click-events-have-key-events', 'a11y/interactive-supports-focus',
  'a11y/aria-props', 'a11y/aria-proptypes', 'a11y/aria-role', 'a11y/alt-text',
  'a11y/anchor-has-content', 'a11y/anchor-is-valid', 'a11y/label-has-associated-control',
  'style/indent', 'style/comma-dangle', 'style/semi', 'style/no-trailing-spaces',
  'style/quotes', 'style/eol-last',
])

const aliasCandidates = new Map()
for (const id of nativeRules) {
  const shortName = id.slice(id.indexOf('/') + 1)
  const candidates = aliasCandidates.get(shortName) ?? []
  candidates.push(id)
  aliasCandidates.set(shortName, candidates)
}

function nativeRuleId(id) {
  if (nativeRules.has(id)) return id
  if (id.includes('/') && !id.startsWith('@typescript-eslint/')) return undefined
  const candidates = aliasCandidates.get(id.split('/').at(-1))
  return candidates?.length === 1 ? candidates[0] : undefined
}

const presetAliases = new Map([
  ['eslint:recommended', 'recommended'],
  ['plugin:react/recommended', 'react'],
  ['plugin:@typescript-eslint/recommended-type-checked', 'typescript'],
])
const input = process.argv[2] ?? '.eslintrc.json'
const config = JSON.parse(await readFile(resolve(input), 'utf8'))
const rules = config.rules ?? {}
const migrated = Object.fromEntries(
  Object.entries(rules)
    .map(([id, setting]) => {
      const native = nativeRuleId(id)
      return native ? [native, setting] : undefined
    })
    .filter(Boolean)
    .sort(([left], [right]) => left.localeCompare(right)),
)
const unsupported = []
for (const [id, setting] of Object.entries(rules)) {
  const native = nativeRuleId(id)
  if (!native) unsupported.push(id)
}
const extendsValues = config.extends === undefined
  ? []
  : (Array.isArray(config.extends) ? config.extends : [config.extends])
const presets = extendsValues.map((value) => ({
  source: value,
  native: presetAliases.get(value) ?? null,
}))
const unsupportedConfig = Object.keys(config)
  .filter((key) => !['rules', 'extends'].includes(key))
  .sort()
const report = {
  schema: 'wake.lint.migration.v1',
  source: input,
  presets,
  migrated,
  unsupported: unsupported.sort(),
  unsupportedConfig,
  notes: [
    'Rule settings are copied as values; parameter names and fix semantics must be reviewed in wake.config.toml.',
    'Overrides, processors, parser options, plugins and environment globals require explicit Wake configuration review.',
  ],
}
process.stdout.write(`${JSON.stringify(report, null, 2)}\n`)
