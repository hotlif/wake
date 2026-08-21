export interface Row {
  readonly id: string
  label: string
}

export type SharedValue = string | number

export const rowValue = {
  id: 'row-7',
  label: 'TypeScript 7',
} as const satisfies Row
