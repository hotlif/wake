import type { Diagnostic } from './index.js'

export interface ParseOptions {
  sourceType?: 'module' | 'script' | 'commonjs'
}

export interface Token {
  kind: string
  start: number
  end: number
  newlineBefore: boolean
  text: string
}

export interface TokenizeResult {
  tokens: Token[]
  diagnostics: Diagnostic[]
}

export interface ParseSummary {
  sourceBytes: number
  statementCount: number
  dependencies: number
  hasTopLevelAwait: boolean
  diagnostics: Diagnostic[]
}

export interface TransformResult {
  code: string
  diagnostics: Diagnostic[]
}

export interface SemanticScope {
  id: number
  kind: string
  parent: number | null
  bindings: Array<{ name: string; symbol: number }>
}

export interface SemanticSymbol {
  id: number
  name: string
  declarationKind: string
  scope: number
  start: number
  end: number
}

export interface SemanticReference {
  id: number
  name: string
  scope: number
  resolved: number | null
  start: number
  end: number
}

export interface SemanticResult {
  schemaVersion: 'wake.semantic.v1'
  scopes: SemanticScope[]
  symbols: SemanticSymbol[]
  references: SemanticReference[]
}

export class ParsedModule {
  readonly disposed: boolean
  readonly summary: ParseSummary
  dispose(): void
  [Symbol.dispose](): void
}

export function tokenize(source: string, options?: ParseOptions): TokenizeResult
export function parse(source: string, options?: ParseOptions): ParsedModule
export function transform(
  sourceOrModule: string | ParsedModule,
  options?: ParseOptions,
): TransformResult
export function analyze(
  sourceOrModule: string | ParsedModule,
  options?: ParseOptions,
): SemanticResult

declare const experimental: {
  ParsedModule: typeof ParsedModule
  analyze: typeof analyze
  parse: typeof parse
  tokenize: typeof tokenize
  transform: typeof transform
}

export default experimental
