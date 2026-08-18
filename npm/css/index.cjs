'use strict'

const variableCounterKey = Symbol.for('@crab-dev/css.createVar.counter')

function compilerOnly(name) {
  const error = new Error(
    `@crab-dev/css: ${name} is a compile-time template tag, but this call reached runtime. ` +
      'Build this module with Wake so the CSS can be extracted, and do not call the tag indirectly.',
  )

  error.code = 'ERR_WAKE_CSS_NOT_COMPILED'
  throw error
}

/** @returns {never} */
function css() {
  return compilerOnly('css')
}

/** @returns {never} */
function keyframes() {
  return compilerOnly('keyframes')
}

/** @returns {never} */
function globalStyle() {
  return compilerOnly('globalStyle')
}

function defineTokens(value) {
  if (value === null || typeof value !== 'object') {
    throw new TypeError('@crab-dev/css: defineTokens expects a plain object or array.')
  }

  const pending = [value]
  const seen = new WeakSet()
  while (pending.length > 0) {
    const current = pending.pop()
    if (seen.has(current)) continue
    seen.add(current)

    const prototype = Object.getPrototypeOf(current)
    const plainObject = prototype === null || Object.getPrototypeOf(prototype) === null
    if (!Array.isArray(current) && !plainObject) {
      throw new TypeError('@crab-dev/css: defineTokens accepts only plain objects and arrays.')
    }

    const descriptors = Object.getOwnPropertyDescriptors(current)
    for (const key of Reflect.ownKeys(descriptors)) {
      if (typeof key === 'symbol') {
        throw new TypeError('@crab-dev/css: defineTokens does not accept symbol keys.')
      }
      const descriptor = descriptors[key]
      if ('get' in descriptor || 'set' in descriptor) {
        throw new TypeError('@crab-dev/css: defineTokens does not accept accessors.')
      }
      const item = descriptor.value
      if (item === null) {
        continue
      } else if (typeof item === 'object') {
        pending.push(item)
      } else if (
        !['string', 'number', 'boolean', 'undefined'].includes(typeof item) ||
        (typeof item === 'number' && !Number.isFinite(item))
      ) {
        throw new TypeError('@crab-dev/css: defineTokens contains an unsupported value.')
      }
    }
    Object.freeze(current)
  }
  return value
}

function cx(...values) {
  const classes = []
  const pending = values.slice().reverse()

  while (pending.length > 0) {
    const value = pending.pop()

    if (!value) continue

    if (typeof value === 'string') {
      const normalized = value.trim()
      if (normalized) classes.push(normalized)
      continue
    }

    if (Array.isArray(value)) {
      for (let index = value.length - 1; index >= 0; index -= 1) {
        pending.push(value[index])
      }
      continue
    }

    if (typeof value === 'object') {
      for (const className of Object.keys(value)) {
        if (value[className]) classes.push(className)
      }
    }
  }

  return classes.join(' ')
}

function createVar(debugName) {
  if (debugName !== undefined && typeof debugName !== 'string') {
    throw new TypeError('@crab-dev/css: createVar debugName must be a string when provided.')
  }

  const label = normalizeDebugName(debugName)
  const current = globalThis[variableCounterKey]
  const id = typeof current === 'bigint' && current >= 0n ? current : 0n
  globalThis[variableCounterKey] = id + 1n

  return `var(--crab-css-${label}-${id.toString(36)})`
}

function assignVars(variables) {
  if (variables === null || typeof variables !== 'object' || Array.isArray(variables)) {
    throw new TypeError('@crab-dev/css: assignVars expects a CSS variable map.')
  }

  const styles = {}

  for (const [variable, value] of Object.entries(variables)) {
    const property = customPropertyFromReference(variable)

    if (property === undefined) {
      throw new TypeError(
        `@crab-dev/css: assignVars key ${JSON.stringify(variable)} is not a CSS variable reference.`,
      )
    }

    if (
      (typeof value !== 'string' && typeof value !== 'number') ||
      (typeof value === 'number' && !Number.isFinite(value))
    ) {
      throw new TypeError(
        `@crab-dev/css: assignVars value for ${JSON.stringify(property)} must be a string or finite number.`,
      )
    }

    styles[property] = value
  }

  return styles
}

function customPropertyFromReference(variable) {
  if (!variable.startsWith('var(--') || !variable.endsWith(')')) return undefined

  const property = variable.slice(4, -1)
  if (property.length <= 2) return undefined
  for (const character of property) {
    if (character === '(' || character === ')' || character.trim() === '') return undefined
  }
  return property
}

function normalizeDebugName(debugName) {
  if (!debugName) return 'var'

  let normalized = ''
  let invalidRun = false
  for (const character of debugName.trim()) {
    const code = character.codePointAt(0)
    const valid =
      (code >= 48 && code <= 57) ||
      (code >= 65 && code <= 90) ||
      (code >= 97 && code <= 122) ||
      character === '_' ||
      character === '-'
    if (valid) {
      if (invalidRun) normalized += '-'
      normalized += character
      invalidRun = false
    } else {
      invalidRun = true
    }
  }
  while (normalized.startsWith('-')) normalized = normalized.slice(1)
  while (normalized.endsWith('-')) normalized = normalized.slice(0, -1)
  normalized = normalized.slice(0, 48)

  return normalized || 'var'
}

exports.assignVars = assignVars
exports.createVar = createVar
exports.css = css
exports.cx = cx
exports.defineTokens = defineTokens
exports.globalStyle = globalStyle
exports.keyframes = keyframes
