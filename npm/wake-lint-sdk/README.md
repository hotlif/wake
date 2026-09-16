# @crab-dev/wake-lint-sdk

Wake native lint extension SDK. The package exposes the versioned
`wake.lint.extension.v1` read-only source and diagnostic protocol for independent rule packages.

Rules are created with `defineRule`, grouped with `definePlugin`, and exercised with `testRule`.
`runRule` freezes the source facts and catches rule failures as `WAKE_LINT_EXTENSION`; it validates
UTF-8 byte ranges and non-overlapping edits before returning diagnostics. The SDK does not execute
Wake configuration, expose compiler AST handles, or automatically enable a rule in `wake lint`.

```js
import { defineRule, runRule } from '@crab-dev/wake-lint-sdk'

const rule = defineRule({
  id: 'demo/no-debugger',
  meta: { description: 'Find debugger statements', category: 'problem', fixable: false },
  create(context) {
    const start = context.source.indexOf('debugger')
    if (start >= 0) context.report({ message: 'debugger is not allowed', start, end: start + 8 })
  },
})

const result = await runRule(rule, { path: 'src/example.js', language: 'js', source: 'debugger;' })
```
