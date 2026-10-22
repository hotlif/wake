import { build, type BuildOptions, type WakeErrorCode } from '@crab-dev/wake'
import { expect, test, type TestApi } from '@crab-dev/wake/test'
import { css, type ClassName } from '@crab-dev/css'

declare const options: BuildOptions
void build(options)

const className: ClassName = css`color: red;`
void className

test('packed declarations', () => {
  expect(42).toBe(42)
})

// @ts-expect-error WakeErrorCode must come from the published declaration entry.
const invalidCode: WakeErrorCode = 'WAKE_NOT_REAL'
// @ts-expect-error TestApi must come from the published subpath declaration entry.
const invalidTest: TestApi = 42
// @ts-expect-error CSS ClassName is branded in the published declaration entry.
const invalidClass: ClassName = 'raw-class'

declare const maybeText: string | undefined
// @ts-expect-error strictNullChecks is inherited from the zipped shared config.
const requiredText: string = maybeText
const values = ['first']
// @ts-expect-error noUncheckedIndexedAccess is inherited from the zipped shared config.
const first: string = values[0]

void [invalidCode, invalidTest, invalidClass, requiredText, first]
