export const PROTOCOL_VERSION: 'wake.lint.extension.v1'
export const SDK_MAJOR: 1
export type ExtensionLanguage = 'js' | 'jsx' | 'ts' | 'tsx'
export type ExtensionCategory = 'problem' | 'suggestion' | 'layout'
export interface ExtensionDiagnosticEdit { start: number; end: number; text: string }
export interface ExtensionDiagnostic {
  message: string
  start: number
  end: number
  messageId?: string
  fix?: { edits: ExtensionDiagnosticEdit[] }
}
export interface ExtensionSourceFacts { tokens?: unknown[]; comments?: unknown[]; [key: string]: unknown }
export interface ExtensionContext {
  readonly path: string
  readonly language: ExtensionLanguage
  readonly source: string
  readonly syntax: Readonly<ExtensionSourceFacts>
  readonly options: Readonly<Record<string, unknown>>
  report(diagnostic: ExtensionDiagnostic): void
}
export interface ExtensionRuleMeta { description: string; category: ExtensionCategory; fixable: boolean }
export interface ExtensionRuleDefinition {
  id: string
  meta: ExtensionRuleMeta
  create(context: ExtensionContext): void | ExtensionDiagnostic[] | Promise<void | ExtensionDiagnostic[]>
}
export interface ExtensionRule extends ExtensionRuleDefinition { readonly protocol: typeof PROTOCOL_VERSION }
export interface ExtensionPlugin { readonly protocol: typeof PROTOCOL_VERSION; readonly sdkMajor: 1; readonly name: string; readonly version: string; readonly rules: readonly ExtensionRule[] }
export interface RuleInput { path: string; language: ExtensionLanguage; source: string; syntax?: ExtensionSourceFacts; options?: Record<string, unknown> }
export interface RuleResult { protocol: typeof PROTOCOL_VERSION; diagnostics: ExtensionDiagnostic[]; error?: { code: 'WAKE_LINT_EXTENSION'; message: string } }
export function defineRule(definition: ExtensionRuleDefinition): ExtensionRule
export function definePlugin(definition: { name: string; version: string; rules: ExtensionRuleDefinition[] }): ExtensionPlugin
export function assertCompatible(plugin: ExtensionPlugin, options?: { sdkMajor?: number }): ExtensionPlugin
export function loadPlugin(specifier: string): Promise<ExtensionPlugin>
export function runRule(rule: ExtensionRule, input: RuleInput): Promise<RuleResult>
export function testRule(rule: ExtensionRule, fixtures: Array<{ input: RuleInput; diagnostics: ExtensionDiagnostic[] }>): Promise<true>
