import type { Row } from './shared.js'

export function isRow(value: unknown): value is Row {
  return typeof value === 'object' && value !== null && 'id' in value && 'label' in value
}

export function assertRow(value: unknown): asserts value is Row {
  if (!isRow(value)) throw new TypeError('Expected a row')
}

export function format(value: string): string
export function format(value: number): string
export function format(value: string | number): string {
  return String(value)
}

export function prefixed(this: { prefix: string }, value: string): string {
  return `${this.prefix}${value}`
}

export const identity = <Value>(value: Value): Value => value
export const trailing = <Value,>(value: Value): Value => value
export const constrained = <Value extends Row>(value: Value): string => value.id
export const defaulted = <Value = object>(value: Value): Value => value
export const constant = <const Value extends readonly unknown[]>(value: Value): Value => value
export const asyncIdentity = async <Value,>(value: Value): Promise<Value> => value
