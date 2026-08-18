# @crab-dev/css

`@crab-dev/css` is Wake's type-safe, zero-runtime CSS authoring API. Wake
extracts `css`, `keyframes`, and `globalStyle` templates at build time; the
browser receives static CSS plus only the small runtime helpers you actually
use.

```sh
npm install @crab-dev/css
```

## Scoped styles and keyframes

```ts
import { css, cx, keyframes } from '@crab-dev/css'

const enter = keyframes`
  from { opacity: 0; transform: translateY(4px); }
  to { opacity: 1; transform: translateY(0); }
`

const card = css`
  display: grid;
  gap: 1rem;
  animation: ${enter} 160ms ease-out;

  &:hover {
    translate: 0 -2px;
  }
`

const className = cx(card, ['surface', { interactive: true, disabled: false }])
```

`cx` has no dependencies. It accepts strings, falsy values, conditional
objects, and arrays nested to any depth while preserving source order.

## Global rules

`globalStyle` is a template tag whose template contains complete CSS rules:

```ts
import { globalStyle } from '@crab-dev/css'

globalStyle`
  :root {
    color-scheme: light dark;
  }

  *, *::before, *::after {
    box-sizing: border-box;
  }
`
```

## Branded CSS variables

`createVar` returns a branded `var(--custom-property)` reference.
Names are deterministic for call order and unique within one JavaScript realm;
ESM and CommonJS loads share the same counter. Interpolate the reference
directly in CSS and pass the direct variable/value map to `assignVars`; it
emits the raw `--...` keys required by inline styles:

```ts
import { assignVars, createVar, css } from '@crab-dev/css'

const accent = createVar('accent')
const gap = createVar('gap')

const panel = css`
  color: ${accent};
  gap: ${gap};
`

const style = assignVars({
  [accent]: 'rebeccapurple',
  [gap]: 12,
})
```

## Immutable design tokens

Use `defineTokens` for nested token structures that are interpolated from another module. Wake
recognizes only a direct top-level `const` initialized through this imported helper; the argument
must contain statically evaluable plain objects, arrays, and primitive values.

```ts
import { css, defineTokens } from '@crab-dev/css'

export const tokens = defineTokens({
  color: { accent: 'rebeccapurple' },
})

export const button = css`
  color: ${tokens.color.accent};
`
```

The return type is deeply readonly and the runtime value is deeply frozen. Ordinary mutable
objects do not acquire this cross-module compiler guarantee.

## Compile-time contract

`css`, `keyframes`, and `globalStyle` must be used as direct tagged templates
in code compiled by Wake. If an untransformed call reaches runtime, it throws
an `ERR_WAKE_CSS_NOT_COMPILED` error explaining how to fix the build. This is
intentional: silently generating placeholder class names would ship missing
styles.

The package provides native ESM and CommonJS entry points, bundled TypeScript
declarations, no runtime dependencies, and is marked side-effect free.

## License

Licensed under either the MIT License or Apache License 2.0, at your option.
