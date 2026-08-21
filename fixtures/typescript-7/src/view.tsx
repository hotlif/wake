import type { JSX, ReactNode } from 'react'
import type { Row } from './shared.js'

interface FormProps<Value> {
  value: Value
  children?: ReactNode
}

function Form<Value>({ value, children }: FormProps<Value>): JSX.Element {
  return <section data-value={JSON.stringify(value)}>{children}</section>
}

export const trailing = <Value,>(value: Value): Value => value
export const constrained = <Value extends Row>(value: Value): string => value.label
export const defaulted = <Value = Row>(value: Value): Value => value
export const constant = <const Value extends Row>(value: Value): Value => value
export const asyncIdentity = async <Value,>(value: Value): Promise<Value> => value

export const view = (
  <>
    <Form<Row> value={{ id: 'row', label: 'TSX' }}>
      <span {...{ title: 'TypeScript 7' }}>Wake</span>
    </Form>
  </>
)
