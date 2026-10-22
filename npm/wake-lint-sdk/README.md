# @crab-dev/wake-lint-sdk

Wake native lint extension SDK. The package exposes the versioned
`wake.lint.extension.v1` read-only source and diagnostic protocol for independent rule packages.

Rules are created with `defineRule`, grouped with `definePlugin`, and exercised with `testRule`.
`runRule` freezes the source facts and catches rule failures as `WAKE_LINT_EXTENSION`; it validates
UTF-8 byte ranges and non-overlapping edits before returning diagnostics. The SDK does not execute
Wake configuration, expose compiler AST handles, or automatically enable a rule in `wake lint`.
`loadPlugin` accepts an explicitly imported package only when its declared protocol and SDK major
match this SDK; plugin, rule, and rule metadata fields outside the v1 manifest are rejected.

```js
import { defineRule, runRule } from '@crab-dev/wake-lint-sdk'

const rule = defineRule({
  id: 'demo/no-debugger',
  meta: { description: 'Find debugger statements', category: 'problem', fixable: false },
  create(context) {
    const index = context.source.indexOf('debugger')
    if (index >= 0) {
      const start = Buffer.byteLength(context.source.slice(0, index))
      context.report({ message: 'debugger is not allowed', messageId: 'found', start, end: start + 8 })
    }
  },
})

const result = await runRule(rule, { path: 'src/example.js', language: 'js', source: 'debugger;' })
```

## Using a rule package in `wake lint`

The first CLI/Node host phase requires a scoped package listed as a direct dependency of the lint
project. The package must declare static metadata in `package.json` and a JSON manifest. For the
example rule above, the package files include:

```json
{
  "name": "@demo/wake-lint-plugin",
  "version": "1.0.0",
  "type": "module",
  "exports": { "import": "./index.mjs" },
  "wakeLint": {
    "manifest": "./wake-lint.manifest.json",
    "module": "./index.mjs"
  }
}
```

```json
{
  "schema": "wake.lint.package.v1",
  "name": "@demo/wake-lint-plugin",
  "version": "1.0.0",
  "protocol": "wake.lint.extension.v1",
  "sdkMajor": 1,
  "rules": [{
    "id": "demo/no-debugger",
    "meta": { "description": "Find debugger statements", "category": "problem", "fixable": false },
    "messageIds": ["found"],
    "optionsSchema": { "type": "object", "additionalProperties": false, "properties": {} }
  }]
}
```

`index.mjs` must default export a `definePlugin({ name, version, rules: [rule] })` value with the
same package identity, rule order, IDs, and metadata. Reports sent to the host must include a
`messageId` from the manifest, such as `found`. The package's `wakeLint` paths stay within the
package. The first host phase accepts only read-only rules and empty option objects. Project
configuration must list the package in `[lint].extensions` and enable its rule in `[lint.rules]`;
each CLI call also requires `--allow-lint-extensions`, and each Node call requires
`allowExtensions: true`. The [lint reference](../../docs/reference/cli/lint.mdx) lists the current
fix, baseline, Markdown, watch, and LSP limits.
