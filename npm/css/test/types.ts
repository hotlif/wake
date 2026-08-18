import {
  assignVars,
  createVar,
  css,
  cx,
  defineTokens,
  globalStyle,
  keyframes,
  type CSSVar,
  type ClassName,
  type KeyframesName,
} from '@crab-dev/css'

const accent = createVar('accent')
const spacing = createVar('spacing')
const defaultVariable = createVar()

const fadeIn: KeyframesName = keyframes`
  from { opacity: 0; }
  to { opacity: 1; }
`

const button: ClassName = css`
  color: ${accent};
  padding: ${spacing};
  animation: ${fadeIn} 150ms ease-out;
`

globalStyle`
  :root {
    color-scheme: light dark;
  }
`

const combined: ClassName = cx(
  button,
  false,
  ['local', [null, { active: true, disabled: false }]],
)

const styles: Record<string, string | number> = assignVars({
  [accent]: 'red',
  [spacing]: 8,
  [defaultVariable]: '1rem',
})

const variable: CSSVar = spacing

const tokens = defineTokens({
  color: 'red',
  nested: { gap: 8 },
  steps: ['sm', 'lg'],
})
const tokenColor: 'red' = tokens.color
const tokenGap: 8 = tokens.nested.gap

// @ts-expect-error defineTokens returns a deeply readonly structure.
tokens.nested.gap = 12

// @ts-expect-error Functions are outside the static token value contract.
defineTokens({ color: () => 'red' })

// @ts-expect-error CSS variables are branded custom-property names.
const invalidVariable: CSSVar = 'var(--ordinary-variable)'

// @ts-expect-error A raw string is not a compiler-generated class name.
const invalidClassName: ClassName = 'button'

// @ts-expect-error Variable values are limited to CSS-compatible primitives.
assignVars({ [accent]: true })

// @ts-expect-error Only branded variables created by createVar are accepted as keys.
assignVars({ 'var(--ordinary-variable)': 'red' })

void combined
void styles
void variable
void tokenColor
void tokenGap
void invalidVariable
void invalidClassName
