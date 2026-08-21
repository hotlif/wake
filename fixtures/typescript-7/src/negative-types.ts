interface ExactOptions {
  optional?: string
}

// @ts-expect-error exactOptionalPropertyTypes rejects an explicit undefined value.
export const invalidOptional: ExactOptions = { optional: undefined }

const numbers: number[] = []
// @ts-expect-error noUncheckedIndexedAccess keeps the possible undefined value.
export const invalidIndexedValue: number = numbers[0]

// @ts-expect-error satisfies checks the value without changing its inferred type.
export const invalidSatisfaction = { kind: 'wake' } satisfies { kind: 'typescript' }
