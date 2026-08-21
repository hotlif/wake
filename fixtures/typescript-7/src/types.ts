import type { Row } from './shared.js'

type Equal<Left, Right> =
  (<Value>() => Value extends Left ? 1 : 2) extends
    (<Value>() => Value extends Right ? 1 : 2)
    ? true
    : false
type Expect<Value extends true> = Value

type HeadTail<Value extends string> =
  Value extends `${infer Head}${infer Tail}` ? [Head, Tail] : never

export type UnicodeCodePointInference = Expect<
  Equal<HeadTail<'😀abc'>, ['😀', 'abc']>
>

export type Producer<out Value> = () => Value
export type Consumer<in Value> = (value: Value) => void
export type AwaitedValue<Value> = Value extends Promise<infer Inner> ? Inner : Value
export type RowGetters<Value extends Row> = {
  [Key in keyof Value as `get${Capitalize<string & Key>}`]-?: () => Value[Key]
}
export type LabeledTuple = [head: string, count?: number, ...flags: boolean[]]
export type ImportedRow = import('./shared.js').Row

export const rows = [
  { id: 'a', label: 'Alpha' },
  { id: 'b', label: 'Beta' },
] as const satisfies readonly Row[]

export type RowId = (typeof rows)[number]['id']
