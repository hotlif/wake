export const PROTOCOL_VERSION = 'wake.lint.extension.v1'
export const SDK_MAJOR = 1

const RULE_ID = /^[a-z][a-z0-9-]*(?:\/[a-z][a-z0-9-]*)+$/
const PLUGIN_NAME = /^@[a-z0-9][a-z0-9._-]*\/[a-z0-9][a-z0-9._-]*$/
const VERSION = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?$/
const LANGUAGES = new Set(['js', 'jsx', 'ts', 'tsx'])
const CATEGORIES = new Set(['problem', 'suggestion', 'layout'])

function extensionError(message) {
  const error = new Error(message)
  error.code = 'WAKE_LINT_EXTENSION'
  return error
}

function deepFreeze(value, seen = new WeakSet()) {
  if (value === null || typeof value !== 'object' || seen.has(value)) return value
  seen.add(value)
  for (const child of Object.values(value)) deepFreeze(child, seen)
  return Object.freeze(value)
}

function requireString(value, label, maxLength = 4096) {
  if (typeof value !== 'string' || value.length === 0 || value.length > maxLength) {
    throw extensionError(`${label} must be a non-empty string of at most ${maxLength} characters`)
  }
  return value
}

function requireText(value, label, maxLength) {
  if (typeof value !== 'string' || value.length > maxLength) {
    throw extensionError(`${label} must be a string of at most ${maxLength} characters`)
  }
  return value
}

function validateMeta(meta) {
  if (!meta || typeof meta !== 'object' || Array.isArray(meta)) throw extensionError('rule meta must be an object')
  const description = requireString(meta.description, 'rule meta.description')
  if (!CATEGORIES.has(meta.category)) throw extensionError('rule meta.category is invalid')
  if (typeof meta.fixable !== 'boolean') throw extensionError('rule meta.fixable must be boolean')
  return Object.freeze({ description, category: meta.category, fixable: meta.fixable })
}

export function defineRule(definition) {
  if (!definition || typeof definition !== 'object') throw extensionError('rule definition must be an object')
  if (!RULE_ID.test(definition.id)) throw extensionError(`invalid rule id: ${definition.id}`)
  const meta = validateMeta(definition.meta)
  if (typeof definition.create !== 'function') throw extensionError(`${definition.id} create must be a function`)
  return Object.freeze({
    protocol: PROTOCOL_VERSION,
    id: definition.id,
    meta,
    create: definition.create,
  })
}

export function definePlugin(definition) {
  if (!definition || typeof definition !== 'object') throw extensionError('plugin definition must be an object')
  if (!PLUGIN_NAME.test(definition.name)) throw extensionError(`invalid plugin name: ${definition.name}`)
  if (!VERSION.test(definition.version)) throw extensionError(`invalid plugin version: ${definition.version}`)
  if (!Array.isArray(definition.rules) || definition.rules.length === 0) throw extensionError('plugin rules must be a non-empty array')
  const rules = definition.rules.map(defineRule)
  const ids = new Set()
  for (const rule of rules) {
    if (ids.has(rule.id)) throw extensionError(`duplicate rule id: ${rule.id}`)
    ids.add(rule.id)
  }
  return Object.freeze({ protocol: PROTOCOL_VERSION, sdkMajor: SDK_MAJOR, name: definition.name, version: definition.version, rules: Object.freeze(rules) })
}

export function assertCompatible(plugin, { sdkMajor = SDK_MAJOR } = {}) {
  if (!plugin || plugin.protocol !== PROTOCOL_VERSION) throw extensionError('extension protocol version is incompatible')
  if (plugin.sdkMajor !== sdkMajor) throw extensionError(`extension SDK major ${plugin.sdkMajor} is incompatible with SDK major ${sdkMajor}`)
  return plugin
}

export async function loadPlugin(specifier) {
  try {
    const module = await import(specifier)
    return assertCompatible(definePlugin(module.default ?? module.plugin ?? module))
  } catch (error) {
    if (error?.code === 'WAKE_LINT_EXTENSION') throw error
    throw extensionError(`failed to load extension ${specifier}: ${error instanceof Error ? error.message : String(error)}`)
  }
}

function sourceBoundaries(source) {
  const boundaries = new Set([0])
  let offset = 0
  for (const character of source) {
    offset += new TextEncoder().encode(character).byteLength
    boundaries.add(offset)
  }
  return boundaries
}

function validateInput(input) {
  if (!input || typeof input !== 'object') throw extensionError('rule input must be an object')
  const path = requireString(input.path, 'input.path')
  const language = requireString(input.language, 'input.language')
  if (!LANGUAGES.has(language)) throw extensionError(`unsupported input language: ${language}`)
  const source = requireText(input.source, 'input.source', 16 * 1024 * 1024)
  return { path, language, source, syntax: input.syntax && typeof input.syntax === 'object' ? input.syntax : {} }
}

function validateDiagnostic(diagnostic, boundaries, fixable) {
  if (!diagnostic || typeof diagnostic !== 'object') throw extensionError('reported diagnostic must be an object')
  const message = requireString(diagnostic.message, 'diagnostic.message')
  if (!Number.isSafeInteger(diagnostic.start) || !Number.isSafeInteger(diagnostic.end)
    || diagnostic.start < 0 || diagnostic.end < diagnostic.start
    || !boundaries.has(diagnostic.start) || !boundaries.has(diagnostic.end)) {
    throw extensionError('diagnostic range must use UTF-8 character boundaries')
  }
  if (diagnostic.fix !== undefined) {
    if (!fixable || !diagnostic.fix || !Array.isArray(diagnostic.fix.edits)) throw extensionError('diagnostic fix is not permitted')
    let previousEnd = -1
    for (const edit of diagnostic.fix.edits) {
      if (typeof edit.text !== 'string' || !Number.isSafeInteger(edit.start) || !Number.isSafeInteger(edit.end)
        || edit.start < previousEnd || edit.end < edit.start
        || !boundaries.has(edit.start) || !boundaries.has(edit.end)) {
        throw extensionError('diagnostic fix contains an invalid or overlapping edit')
      }
      previousEnd = edit.end
    }
  }
  return {
    message,
    start: diagnostic.start,
    end: diagnostic.end,
    ...(diagnostic.messageId === undefined ? {} : { messageId: requireString(diagnostic.messageId, 'diagnostic.messageId', 256) }),
    ...(diagnostic.fix === undefined ? {} : { fix: { edits: diagnostic.fix.edits.map((edit) => ({ start: edit.start, end: edit.end, text: edit.text })) } }),
  }
}

export async function runRule(rule, input) {
  try {
    const checkedRule = defineRule(rule)
    const checkedInput = validateInput(input)
    const boundaries = sourceBoundaries(checkedInput.source)
    const diagnostics = []
    const context = {
      path: checkedInput.path,
      language: checkedInput.language,
      source: checkedInput.source,
      syntax: deepFreeze(structuredClone(checkedInput.syntax)),
      options: deepFreeze(structuredClone(input.options ?? {})),
      report(diagnostic) {
        diagnostics.push(validateDiagnostic(diagnostic, boundaries, checkedRule.meta.fixable))
      },
    }
    deepFreeze(context)
    const returned = await checkedRule.create(context)
    if (Array.isArray(returned)) for (const diagnostic of returned) context.report(diagnostic)
    diagnostics.sort((left, right) => left.start - right.start || left.end - right.end || left.message.localeCompare(right.message))
    return { protocol: PROTOCOL_VERSION, diagnostics }
  } catch (error) {
    return {
      protocol: PROTOCOL_VERSION,
      diagnostics: [],
      error: { code: 'WAKE_LINT_EXTENSION', message: error instanceof Error ? error.message : String(error) },
    }
  }
}

export async function testRule(rule, fixtures) {
  for (const fixture of fixtures) {
    const result = await runRule(rule, fixture.input)
    if (result.error) throw extensionError(`fixture failed: ${result.error.message}`)
    if (JSON.stringify(result.diagnostics) !== JSON.stringify(fixture.diagnostics)) {
      throw extensionError(`fixture diagnostics did not match for ${fixture.input.path}`)
    }
  }
  return true
}
