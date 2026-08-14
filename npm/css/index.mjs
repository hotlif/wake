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
export function css() {
  return compilerOnly('css')
}

/** @returns {never} */
export function keyframes() {
  return compilerOnly('keyframes')
}

/** @returns {never} */
export function globalStyle() {
  return compilerOnly('globalStyle')
}

export function cx(...values) {
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

export function createVar(debugName) {
  if (debugName !== undefined && typeof debugName !== 'string') {
    throw new TypeError('@crab-dev/css: createVar debugName must be a string when provided.')
  }

  const label = normalizeDebugName(debugName)
  const current = globalThis[variableCounterKey]
  const id = typeof current === 'bigint' && current >= 0n ? current : 0n
  globalThis[variableCounterKey] = id + 1n

  return `var(--crab-css-${label}-${id.toString(36)})`
}

export function assignVars(variables) {
  if (variables === null || typeof variables !== 'object' || Array.isArray(variables)) {
    throw new TypeError('@crab-dev/css: assignVars expects a CSS variable map.')
  }

  const styles = {}

  for (const [variable, value] of Object.entries(variables)) {
    const match = /^var\((--[^()\s]+)\)$/.exec(variable)

    if (!match) {
      throw new TypeError(
        `@crab-dev/css: assignVars key ${JSON.stringify(variable)} is not a CSS variable reference.`,
      )
    }

    const property = match[1]

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

function normalizeDebugName(debugName) {
  if (!debugName) return 'var'

  const normalized = debugName
    .trim()
    .replace(/[^a-zA-Z0-9_-]+/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 48)

  return normalized || 'var'
}
