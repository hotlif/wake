// Wake-owned test kernel. This source executes inside wake_ecma_vm and exposes only the explicit
// authoring primitives and result schema owned by Wake.
(() => {
  'use strict'

  const runtimeResultSchema = 'wake.test.runtime.v1'
  const schedulerSchema = 'wake.test.scheduler.v1'
  const realDateNow = Date.now.bind(Date)
  const realRequestAnimationFrame = typeof globalThis.requestAnimationFrame === 'function'
    ? globalThis.requestAnimationFrame.bind(globalThis)
    : callback => globalThis.setTimeout(callback, 0)
  let realSetTimeout
  let realClearTimeout
  const state = {
    root: null,
    current: null,
    activeTest: null,
    focused: false,
    defaultTimeout: 5000,
    seed: 0,
    namePattern: null,
    mocks: new Set(),
    spies: new Set(),
    moduleMocks: new Map(),
    moduleDefinitions: new Map(),
    moduleCache: new Map(),
    modulePromises: new Set(),
    builtins: new Map(),
    snapshotSerializers: [],
    snapshots: [],
    expectedSnapshots: Object.create(null),
    updateSnapshots: 'new',
    timerState: null,
    realTimerTracking: null,
    leaks: [],
    diagnostics: [],
    networkRoutes: [],
    networkRequests: [],
    networkFetchOriginal: null,
    networkRequestId: 0,
    networkMode: 'deny',
    networkAllowHosts: [],
    browserOperationId: 0,
    browserInputElementId: 0,
    browserOperationPending: new Map(),
    environment: 'dom',
    forbidOnly: false,
    reactStrictMode: false,
    reactCleanup: true,
    reactActWarnings: 'error',
    reactActWarningKeys: new Set(),
    pendingActFailures: [],
    reactRuntimeOverride: null,
    reactRecoveryRuntime: null,
    testIdAttribute: 'data-testid',
    scheduler: null,
  }

  function suite(name, parent, mode = 'run') {
    return {
      name: String(name), parent, mode, entries: [],
      hooks: { beforeAll: [], beforeEach: [], afterEach: [], afterAll: [] },
    }
  }
  state.root = suite('', null)
  state.current = state.root

  function formatName(template, values, rowIndex) {
    let index = 0
    return String(template)
      .replace(/%#/g, String(rowIndex))
      .replace(/%\$|%[sdifjo]/g, token => {
        if (token === '%$') return String(rowIndex + 1)
        const value = values[index++]
        if (token === '%j') {
          try { return JSON.stringify(value) } catch { return '[Circular]' }
        }
        if (token === '%d' || token === '%i') return String(parseInt(value, 10))
        if (token === '%f') return String(Number(value))
        if (token === '%o') return pretty(value)
        return String(value)
      })
  }

  function tableRows(table) {
    if (!Array.isArray(table)) throw new TypeError('each table must be an array')
    return table.map(row => Array.isArray(row) ? row : [row])
  }

  function timeoutOption(value) {
    if (value === undefined) return undefined
    if (typeof value === 'number') throw new TypeError('Wake test timeouts use { timeout: milliseconds }')
    if (value && typeof value === 'object' && value.timeout === undefined && Reflect.ownKeys(value).length === 0) return undefined
    if (!value || typeof value !== 'object' || !Number.isFinite(value.timeout) || value.timeout <= 0) {
      throw new TypeError('Wake test timeout must be a positive { timeout } option')
    }
    return Number(value.timeout)
  }

  function captureRegistrationStack() {
    const error = new Error()
    return error.stack ? String(error.stack) : null
  }

  function registerTest(name, fn, mode = 'run', options = {}) {
    if (mode === 'only') {
      if (state.forbidOnly) throw Object.assign(new Error('Focused tests are forbidden by test.forbid_only'), { code: 'WAKE_TEST_CONFIG' })
      state.focused = true
    }
    const record = {
      name: String(name), fn, mode, timeout: timeoutOption(options),
      registrationStack: captureRegistrationStack(),
    }
    state.current.entries.push({ type: 'test', value: record })
    return record
  }

  function makeTest(mode = 'run', options = {}) {
    const value = (name, fn, timeout) => registerTest(name, fn, mode, timeout || options)
    value.todo = name => registerTest(name, undefined, 'todo', options)
    value.each = table => {
      const rows = tableRows(table)
      return (name, fn, timeout) => rows.forEach((row, index) =>
        registerTest(formatName(name, row, index), () => fn(...row), mode, timeout || options))
    }
    return value
  }

  const test = makeTest()
  test.only = makeTest('only')
  test.skip = makeTest('skip')
  const it = test

  function registerDescribe(name, fn, mode = 'run') {
    if (mode === 'only') {
      if (state.forbidOnly) throw Object.assign(new Error('Focused suites are forbidden by test.forbid_only'), { code: 'WAKE_TEST_CONFIG' })
      state.focused = true
    }
    const parent = state.current
    const child = suite(name, parent, mode)
    parent.entries.push({ type: 'suite', value: child })
    state.current = child
    try { fn() } finally { state.current = parent }
  }

  function makeDescribe(mode = 'run') {
    const value = (name, fn) => registerDescribe(name, fn, mode)
    value.each = table => {
      const rows = tableRows(table)
      return (name, fn) => rows.forEach((row, index) =>
        registerDescribe(formatName(name, row, index), () => fn(...row), mode))
    }
    return value
  }

  const describe = makeDescribe()
  describe.only = makeDescribe('only')
  describe.skip = makeDescribe('skip')

  function hook(kind, fn, options) {
    if (typeof fn !== 'function') throw new TypeError(`${kind} requires a function`)
    state.current.hooks[kind].push({
      fn,
      timeout: timeoutOption(options),
      registrationStack: captureRegistrationStack(),
    })
  }
  const beforeAll = (fn, options) => hook('beforeAll', fn, options)
  const beforeEach = (fn, options) => hook('beforeEach', fn, options)
  const afterEach = (fn, options) => hook('afterEach', fn, options)
  const afterAll = (fn, options) => hook('afterAll', fn, options)

  const htmlVoidElements = new Set(['area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'param', 'source', 'track', 'wbr'])
  const escapeDomText = value => String(value).replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
  const escapeDomAttribute = value => escapeDomText(value).replace(/"/g, '&quot;')

  function canonicalDOM(node) {
    if (!node) return ''
    if (node.nodeType === 9 || node.nodeType === 11) {
      const children = node.nodeType === 1 && node.localName === 'template' && node.content
        ? node.content.childNodes
        : node.childNodes
      return [...children || []].map(canonicalDOM).join('')
    }
    if (node.nodeType === 3) return escapeDomText(node.data ?? node.textContent ?? '')
    if (node.nodeType === 8) return `<!--${String(node.data ?? node.textContent ?? '')}-->`
    if (node.nodeType !== 1) return `[${node.nodeName || 'Node'}]`
    const name = String(node.localName || node.tagName || node.nodeName).toLowerCase()
    const attributes = [...node.attributes || []]
      .map(attribute => [String(attribute.name), String(attribute.value)])
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([key, value]) => ` ${key}="${escapeDomAttribute(value)}"`)
      .join('')
    if (htmlVoidElements.has(name) && (!node.namespaceURI || node.namespaceURI === 'http://www.w3.org/1999/xhtml')) {
      return `<${name}${attributes}>`
    }
    const children = name === 'template' && node.content ? node.content.childNodes : node.childNodes
    return `<${name}${attributes}>${[...children || []].map(canonicalDOM).join('')}</${name}>`
  }

  function pretty(value, seen = new Set()) {
    if (value && value.$$wakeSnapshotProperty === true) return value.text
    for (const serializer of state.snapshotSerializers) {
      if (serializer.test(value)) {
        return serializer.print(value, child => pretty(child, seen), String, {}, {})
      }
    }
    if (typeof value === 'string') return JSON.stringify(value)
    if (typeof value === 'number') return Object.is(value, -0) ? '-0' : String(value)
    if (typeof value === 'bigint') return `${value}n`
    if (typeof value === 'boolean') return String(value)
    if (typeof value === 'symbol') return value.toString()
    if (typeof value === 'function') return `[Function ${value.name || 'anonymous'}]`
    if (value === null) return 'null'
    if (value === undefined) return 'undefined'
    if (value instanceof Error) return `[${value.name}: ${value.message}]`
    if (value === globalThis || (typeof globalThis.window !== 'undefined' && value === globalThis.window)) return '[Window]'
    if (typeof ArrayBuffer !== 'undefined' && value instanceof ArrayBuffer) return `ArrayBuffer { byteLength: ${value.byteLength} }`
    if (ArrayBuffer.isView(value)) return `${value.constructor.name} [${[...value].join(', ')}]`
    if (typeof value.nodeType === 'number') {
      return canonicalDOM(value)
    }
    if (seen.has(value)) return '[Circular]'
    seen.add(value)
    try {
      if (Array.isArray(value)) return `[${value.map(item => pretty(item, seen)).join(', ')}]`
      if (value instanceof Date) return `Date { ${JSON.stringify(value.toISOString())} }`
      if (value instanceof RegExp) return value.toString()
      if (value instanceof Map) return `Map { ${[...value].map(([k, v]) => `${pretty(k, seen)} => ${pretty(v, seen)}`).join(', ')} }`
      if (value instanceof Set) return `Set { ${[...value].map(item => pretty(item, seen)).join(', ')} }`
      const entries = Reflect.ownKeys(value).sort((a, b) => String(a).localeCompare(String(b)))
        .map(key => {
          try { return `${String(key)}: ${pretty(value[key], seen)}` }
          catch { return `${String(key)}: [Unavailable]` }
        })
      return `{ ${entries.join(', ')} }`
    } finally {
      seen.delete(value)
    }
  }

  function asymmetric(match, label, sample) {
    return {
      $$wakeAsymmetric: true,
      asymmetricMatch: match,
      toString: () => label,
      getExpectedType: () => typeof sample,
      toAsymmetricMatcher: () => `${label}<${pretty(sample)}>`,
    }
  }

  function equals(received, expected, strict = false, seen = new Map()) {
    for (const tester of expect._equalityTesters || []) {
      const outcome = tester.call({ equals }, received, expected)
      if (outcome !== undefined) return Boolean(outcome)
    }
    if (expected && expected.$$wakeAsymmetric) return Boolean(expected.asymmetricMatch(received))
    if (Object.is(received, expected)) return true
    if (!strict && typeof received === 'number' && typeof expected === 'number' && Number.isNaN(received) && Number.isNaN(expected)) return true
    if (typeof received !== 'object' || received === null || typeof expected !== 'object' || expected === null) return false
    if (strict && Object.getPrototypeOf(received) !== Object.getPrototypeOf(expected)) return false
    if (seen.get(received) === expected) return true
    seen.set(received, expected)
    if (received instanceof Date || expected instanceof Date) return received instanceof Date && expected instanceof Date && received.getTime() === expected.getTime()
    if (received instanceof RegExp || expected instanceof RegExp) return received instanceof RegExp && expected instanceof RegExp && String(received) === String(expected)
    if (received instanceof Set || expected instanceof Set) {
      if (!(received instanceof Set && expected instanceof Set) || received.size !== expected.size) return false
      return [...expected].every(item => [...received].some(candidate => equals(candidate, item, strict, seen)))
    }
    if (received instanceof Map || expected instanceof Map) {
      if (!(received instanceof Map && expected instanceof Map) || received.size !== expected.size) return false
      return [...expected].every(([key, value]) => [...received].some(([candidate, actual]) => equals(candidate, key, strict, seen) && equals(actual, value, strict, seen)))
    }
    const actualKeys = Reflect.ownKeys(received).filter(key => !(!strict && received[key] === undefined))
    const expectedKeys = Reflect.ownKeys(expected).filter(key => !(!strict && expected[key] === undefined))
    if (actualKeys.length !== expectedKeys.length) return false
    return expectedKeys.every(key => Object.prototype.hasOwnProperty.call(received, key) && equals(received[key], expected[key], strict, seen))
  }

  function subset(received, expected) {
    if (expected && expected.$$wakeAsymmetric) return expected.asymmetricMatch(received)
    if (typeof expected !== 'object' || expected === null) return equals(received, expected)
    if (typeof received !== 'object' || received === null) return false
    return Reflect.ownKeys(expected).every(key => Object.prototype.hasOwnProperty.call(received, key) && subset(received[key], expected[key]))
  }

  function property(object, path) {
    const parts = Array.isArray(path) ? path : String(path).replace(/\[(\d+)\]/g, '.$1').split('.').filter(Boolean)
    let value = object
    for (const part of parts) {
      if (value == null || !(part in Object(value))) return { found: false, value: undefined }
      value = value[part]
    }
    return { found: true, value }
  }

  const matchers = Object.create(null)
  function result(pass, message) { return { pass: Boolean(pass), message: () => message } }
  matchers.toBe = (received, expected) => result(Object.is(received, expected), `Expected ${pretty(received)} to be ${pretty(expected)}`)
  matchers.toEqual = (received, expected) => result(equals(received, expected), `Expected ${pretty(received)} to equal ${pretty(expected)}`)
  matchers.toStrictEqual = (received, expected) => result(equals(received, expected, true), `Expected ${pretty(received)} to strictly equal ${pretty(expected)}`)
  matchers.toBeDefined = received => result(received !== undefined, 'Expected value to be defined')
  matchers.toBeUndefined = received => result(received === undefined, `Expected ${pretty(received)} to be undefined`)
  matchers.toBeNull = received => result(received === null, `Expected ${pretty(received)} to be null`)
  matchers.toBeTruthy = received => result(Boolean(received), `Expected ${pretty(received)} to be truthy`)
  matchers.toBeFalsy = received => result(!received, `Expected ${pretty(received)} to be falsy`)
  matchers.toBeNaN = received => result(Number.isNaN(received), `Expected ${pretty(received)} to be NaN`)
  matchers.toBeGreaterThan = (received, expected) => result(received > expected, `Expected ${received} to be greater than ${expected}`)
  matchers.toBeGreaterThanOrEqual = (received, expected) => result(received >= expected, `Expected ${received} to be greater than or equal to ${expected}`)
  matchers.toBeLessThan = (received, expected) => result(received < expected, `Expected ${received} to be less than ${expected}`)
  matchers.toBeLessThanOrEqual = (received, expected) => result(received <= expected, `Expected ${received} to be less than or equal to ${expected}`)
  matchers.toBeCloseTo = (received, expected, digits = 2) => result(Math.abs(received - expected) < 0.5 * 10 ** -digits, `Expected ${received} to be close to ${expected}`)
  matchers.toContain = (received, expected) => result(received != null && typeof received.includes === 'function' ? received.includes(expected) : [...received].includes(expected), `Expected ${pretty(received)} to contain ${pretty(expected)}`)
  matchers.toContainEqual = (received, expected) => result([...received].some(value => equals(value, expected)), `Expected ${pretty(received)} to contain an equal value`)
  matchers.toHaveLength = (received, expected) => result(received != null && received.length === expected, `Expected length ${expected}, received ${received && received.length}`)
  matchers.toMatch = (received, expected) => result(typeof received === 'string' && (expected instanceof RegExp ? expected.test(received) : received.includes(String(expected))), `Expected ${pretty(received)} to match ${expected}`)
  matchers.toMatchObject = (received, expected) => result(subset(received, expected), `Expected ${pretty(received)} to match object ${pretty(expected)}`)
  matchers.toHaveProperty = (received, path, ...expected) => {
    const found = property(received, path)
    return result(found.found && (!expected.length || equals(found.value, expected[0])), `Expected ${pretty(received)} to have property ${pretty(path)}`)
  }
  matchers.toBeInstanceOf = (received, expected) => result(received instanceof expected, `Expected value to be instance of ${expected && expected.name}`)
  matchers.toThrow = (received, expected) => {
    if (typeof received !== 'function') return result(false, 'toThrow requires a function')
    let thrown
    try { received() } catch (error) { thrown = error }
    if (thrown === undefined) return result(false, 'Expected function to throw')
    if (expected === undefined) return result(true, '')
    if (typeof expected === 'string') return result(String(thrown.message || thrown).includes(expected), `Expected thrown message to contain ${expected}`)
    if (expected instanceof RegExp) return result(expected.test(String(thrown.message || thrown)), `Expected thrown message to match ${expected}`)
    if (typeof expected === 'function') return result(thrown instanceof expected, `Expected thrown value to be ${expected.name}`)
    return result(equals(thrown, expected), `Expected thrown value to equal ${pretty(expected)}`)
  }

  function mockState(value) { return value && value.isMockFunction === true && value.calls }
  matchers.toHaveBeenCalled = received => result(Boolean(mockState(received) && mockState(received).calls.length), 'Expected mock to have been called')
  matchers.toHaveBeenCalledTimes = (received, count) => result(Boolean(mockState(received)) && mockState(received).calls.length === count, `Expected mock to be called ${count} times`)
  matchers.toHaveBeenCalledWith = (received, ...args) => result(Boolean(mockState(received)) && mockState(received).calls.some(call => equals(call, args)), `Expected mock to be called with ${pretty(args)}`)
  matchers.toHaveBeenLastCalledWith = (received, ...args) => result(Boolean(mockState(received)) && equals(mockState(received).lastCall, args), `Expected last mock call to equal ${pretty(args)}`)
  matchers.toHaveBeenNthCalledWith = (received, nth, ...args) => result(Boolean(mockState(received)) && equals(mockState(received).calls[nth - 1], args), `Expected mock call ${nth} to equal ${pretty(args)}`)
  matchers.toHaveReturned = received => result(Boolean(mockState(received)) && mockState(received).results.some(item => item.type === 'return'), 'Expected mock to have returned')
  matchers.toHaveReturnedTimes = (received, count) => result(Boolean(mockState(received)) && mockState(received).results.filter(item => item.type === 'return').length === count, `Expected mock to return ${count} times`)
  matchers.toHaveReturnedWith = (received, expected) => result(Boolean(mockState(received)) && mockState(received).results.some(item => item.type === 'return' && equals(item.value, expected)), `Expected mock to return ${pretty(expected)}`)
  matchers.toHaveLastReturnedWith = (received, expected) => {
    const item = mockState(received) && mockState(received).results.at(-1)
    return result(Boolean(item && item.type === 'return' && equals(item.value, expected)), `Expected last mock result to equal ${pretty(expected)}`)
  }
  matchers.toHaveNthReturnedWith = (received, nth, expected) => {
    const item = mockState(received) && mockState(received).results[nth - 1]
    return result(Boolean(item && item.type === 'return' && equals(item.value, expected)), `Expected mock result ${nth} to equal ${pretty(expected)}`)
  }

  function snapshotKey(hint) {
    const active = state.activeTest
    active.snapshotIndex += 1
    return `${active.fullName}${hint ? `: ${hint}` : ''} ${active.snapshotIndex}`
  }
  function snapshotPropertyMarker(expected) {
    const text = expected && expected.$$wakeAsymmetric
      ? (typeof expected.toAsymmetricMatcher === 'function' ? expected.toAsymmetricMatcher() : expected.toString())
      : pretty(expected)
    return Object.freeze({$$wakeSnapshotProperty: true, text})
  }
  function snapshotWithProperties(received, expected) {
    if (expected && expected.$$wakeAsymmetric) return snapshotPropertyMarker(expected)
    if (typeof expected !== 'object' || expected === null) return snapshotPropertyMarker(expected)
    if (typeof received !== 'object' || received === null) return received
    const clone = Array.isArray(received)
      ? [...received]
      : Object.assign(Object.create(Object.getPrototypeOf(received)), received)
    for (const key of Reflect.ownKeys(expected)) {
      clone[key] = snapshotWithProperties(received[key], expected[key])
    }
    return clone
  }
  matchers.toMatchSnapshot = (received, propertyMatchers, hint) => {
    if (typeof propertyMatchers === 'string') { hint = propertyMatchers; propertyMatchers = undefined }
    if (propertyMatchers && !subset(received, propertyMatchers)) return result(false, 'Snapshot property matchers failed')
    const key = snapshotKey(hint)
    const value = pretty(propertyMatchers ? snapshotWithProperties(received, propertyMatchers) : received)
    state.snapshots.push({ key, value })
    if (Object.prototype.hasOwnProperty.call(state.expectedSnapshots, key)) {
      if (state.expectedSnapshots[key] === value || state.updateSnapshots === 'all') return result(true, '')
      return {
        pass: false,
        message: () => `Snapshot ${key} did not match\nExpected: ${state.expectedSnapshots[key]}\nReceived: ${value}`,
        code: 'WAKE_TEST_SNAPSHOT',
        diff: {
          expected: state.expectedSnapshots[key],
          received: value,
          unified: `- ${state.expectedSnapshots[key]}\n+ ${value}`,
        },
      }
    }
    const canAdd = state.updateSnapshots === 'new' || state.updateSnapshots === 'all'
    return {pass: canAdd, message: () => `Snapshot ${key} does not exist`, code: canAdd ? undefined : 'WAKE_TEST_SNAPSHOT'}
  }

  function screenshotClip(received) {
    if (received === globalThis || received === document || received === document.documentElement || received === document.body) return null
    if (!(received instanceof Element)) throw browserInputError('toMatchScreenshot requires document, window, or an Element')
    browserInputTarget(received)
    const rect = received.getBoundingClientRect()
    const x = Math.max(0, rect.left)
    const y = Math.max(0, rect.top)
    const width = Math.min(innerWidth, rect.right) - x
    const height = Math.min(innerHeight, rect.bottom) - y
    if (width <= 0 || height <= 0) throw browserInputError('Screenshot target has no visible layout box')
    return {x, y, width, height, scale: 1}
  }

  matchers.toMatchScreenshot = async function (received, hint) {
    if (this.isNot) throw Object.assign(new Error('toMatchScreenshot does not support .not'), {code: 'WAKE_TEST_SNAPSHOT'})
    if (state.environment !== 'browser') {
      return {pass: false, message: () => 'toMatchScreenshot requires the browser environment', code: 'WAKE_TEST_BROWSER'}
    }
    if (hint !== undefined && typeof hint !== 'string') {
      return {pass: false, message: () => 'toMatchScreenshot hint must be a string', code: 'WAKE_TEST_SNAPSHOT'}
    }
    const active = state.activeTest
    if (!active) throw Object.assign(new Error('toMatchScreenshot must be called inside a test'), {code: 'WAKE_TEST_SNAPSHOT'})
    const key = snapshotKey(hint)
    await optionalReactAct(async () => {
      if (document.fonts && document.fonts.ready) await document.fonts.ready
      await new Promise(resolve => realRequestAnimationFrame(resolve))
      await new Promise(resolve => realRequestAnimationFrame(resolve))
      await Promise.resolve()
    })
    const clip = screenshotClip(received)
    const outcome = await enqueueBrowserOperation('screenshot', {
      key,
      testFullName: active.fullName,
      clip,
    })
    return {
      pass: Boolean(outcome && outcome.pass),
      message: () => outcome && outcome.message || `Screenshot ${key} did not match`,
      code: outcome && outcome.code || 'WAKE_TEST_SNAPSHOT',
      diff: outcome && outcome.diff,
    }
  }

  function applyMatcher(name, received, expected, isNot) {
    const matcher = matchers[name]
    if (!matcher) throw new Error(`Unknown matcher ${name}`)
    if (state.activeTest) state.activeTest.assertionCalls += 1
    const context = {
      isNot,
      equals,
      utils: { stringify: pretty, printExpected: pretty, printReceived: pretty },
    }
    const evaluate = outcome => {
      if (!outcome || typeof outcome.pass !== 'boolean') throw new TypeError(`Matcher ${name} must return {pass, message}`)
      const pass = isNot ? !outcome.pass : outcome.pass
      if (!pass) {
        const prefix = isNot ? 'Expected matcher not to pass. ' : ''
        const message = typeof outcome.message === 'function' ? outcome.message() : outcome.message
        const error = Object.assign(new Error(prefix + (message || `Matcher ${name} failed`)), {
          name: 'AssertionError',
          code: outcome.code || 'WAKE_TEST_ASSERTION',
        })
        if (outcome.diff) error.__wakeDiff = outcome.diff
        else if (expected.length) {
          const expectedText = pretty(expected[0])
          const receivedText = pretty(received)
          error.__wakeDiff = {
            expected: expectedText,
            received: receivedText,
            unified: `--- Expected\n+++ Received\n- ${expectedText}\n+ ${receivedText}`,
          }
        }
        throw error
      }
    }
    const outcome = Reflect.apply(matcher, context, [received, ...expected])
    return outcome && typeof outcome.then === 'function' ? outcome.then(evaluate) : evaluate(outcome)
  }

  function expectation(received, isNot = false, promiseMode = null) {
    const object = {}
    Object.defineProperty(object, 'not', { get: () => expectation(received, !isNot, promiseMode) })
    Object.defineProperty(object, 'resolves', { get: () => expectation(received, isNot, 'resolves') })
    Object.defineProperty(object, 'rejects', { get: () => expectation(received, isNot, 'rejects') })
    for (const name of Object.keys(matchers)) {
      object[name] = (...expected) => {
        if (!promiseMode) return applyMatcher(name, received, expected, isNot)
        if (!received || typeof received.then !== 'function') throw new Error(`expect(...).${promiseMode} requires a Promise`)
        if (promiseMode === 'resolves') {
          return Promise.resolve(received).then(value => applyMatcher(name, value, expected, isNot), error => { throw new Error(`Expected Promise to resolve, but it rejected with ${pretty(error)}`) })
        }
        return Promise.resolve(received).then(
          value => { throw new Error(`Expected Promise to reject, but it resolved with ${pretty(value)}`) },
          error => applyMatcher(name, name === 'toThrow' ? () => { throw error } : error, expected, isNot),
        )
      }
    }
    return object
  }

  const expect = received => expectation(received)
  expect.extend = additions => {
    if (!additions || typeof additions !== 'object') throw new TypeError('expect.extend() requires a matcher object')
    for (const [name, matcher] of Object.entries(additions)) {
      if (typeof matcher !== 'function') throw new TypeError(`Custom matcher ${name} must be a function`)
      matchers[name] = matcher
    }
  }
  expect.addEqualityTesters = testers => {
    if (!Array.isArray(testers) || testers.some(tester => typeof tester !== 'function')) {
      throw new TypeError('expect.addEqualityTesters() requires an array of functions')
    }
    expect._equalityTesters.push(...testers)
  }
  expect._equalityTesters = []
  expect.addSnapshotSerializer = serializer => {
    if (!serializer || typeof serializer.test !== 'function' || typeof serializer.print !== 'function') {
      throw new TypeError('expect.addSnapshotSerializer() requires test() and print() functions')
    }
    state.snapshotSerializers.unshift(serializer)
  }
  expect.assertions = count => { if (!state.activeTest) throw new Error('expect.assertions must be called inside a test'); state.activeTest.expectedAssertions = count }
  expect.hasAssertions = () => { if (!state.activeTest) throw new Error('expect.hasAssertions must be called inside a test'); state.activeTest.hasAssertions = true }
  expect.getState = () => ({ ...state.activeTest, snapshotState: { added: state.snapshots.length } })
  expect.setState = values => { if (state.activeTest) Object.assign(state.activeTest, values) }
  expect.anything = () => asymmetric(value => value !== null && value !== undefined, 'Anything')
  expect.any = constructor => asymmetric(value => {
    if (constructor === String) return typeof value === 'string' || value instanceof String
    if (constructor === Number) return typeof value === 'number' || value instanceof Number
    if (constructor === Boolean) return typeof value === 'boolean' || value instanceof Boolean
    if (constructor === BigInt) return typeof value === 'bigint'
    if (constructor === Symbol) return typeof value === 'symbol'
    return value instanceof constructor
  }, 'Any', constructor)
  expect.stringContaining = sample => asymmetric(value => typeof value === 'string' && value.includes(String(sample)), 'StringContaining', sample)
  expect.stringMatching = sample => asymmetric(value => typeof value === 'string' && (sample instanceof RegExp ? sample.test(value) : new RegExp(sample).test(value)), 'StringMatching', sample)
  expect.arrayContaining = sample => asymmetric(value => Array.isArray(value) && sample.every(item => value.some(candidate => equals(candidate, item))), 'ArrayContaining', sample)
  expect.objectContaining = sample => asymmetric(value => subset(value, sample), 'ObjectContaining', sample)
  expect.closeTo = (sample, digits = 2) => asymmetric(value => typeof value === 'number' && Math.abs(value - sample) < 0.5 * 10 ** -digits, 'CloseTo', sample)
  expect.not = {
    stringContaining: sample => asymmetric(value => !(typeof value === 'string' && value.includes(String(sample))), 'NotStringContaining', sample),
    stringMatching: sample => asymmetric(value => !(typeof value === 'string' && (sample instanceof RegExp ? sample.test(value) : new RegExp(sample).test(value))), 'NotStringMatching', sample),
    arrayContaining: sample => asymmetric(value => !(Array.isArray(value) && sample.every(item => value.some(candidate => equals(candidate, item)))), 'NotArrayContaining', sample),
    objectContaining: sample => asymmetric(value => !subset(value, sample), 'NotObjectContaining', sample),
  }

  function createMock(implementation) {
    let currentImplementation = implementation
    let once = []
    let name = 'wake.mock.fn()'
    let record = { calls: [], contexts: [], instances: [], invocationCallOrder: [], results: [], lastCall: undefined }
    const mock = function (...args) {
      record.calls.push(args)
      record.contexts.push(this)
      record.instances.push(new.target ? this : undefined)
      record.lastCall = args
      record.invocationCallOrder.push(createMock.order++)
      const resultRecord = { type: 'incomplete', value: undefined }
      record.results.push(resultRecord)
      const next = once.length ? once.shift() : currentImplementation
      try {
        const value = next ? Reflect.apply(next, this, args) : undefined
        resultRecord.type = 'return'; resultRecord.value = value
        return value
      } catch (error) {
        resultRecord.type = 'throw'; resultRecord.value = error
        throw error
      }
    }
    Object.defineProperties(mock, {
      calls: { get: () => record },
      isMockFunction: { value: true, enumerable: true },
      name: { get: () => name, configurable: true },
    })
    mock.clear = () => { record = { calls: [], contexts: [], instances: [], invocationCallOrder: [], results: [], lastCall: undefined }; return mock }
    mock.reset = () => { mock.clear(); currentImplementation = undefined; once = []; return mock }
    mock.restore = () => { mock.reset(); if (mock._restore) mock._restore(); return undefined }
    mock.implement = value => { currentImplementation = value; return mock }
    mock.implementOnce = value => { once.push(value); return mock }
    mock.return = value => mock.implement(() => value)
    mock.returnOnce = value => mock.implementOnce(() => value)
    mock.resolve = value => mock.implement(() => Promise.resolve(value))
    mock.resolveOnce = value => mock.implementOnce(() => Promise.resolve(value))
    mock.reject = value => mock.implement(() => Promise.reject(value))
    mock.rejectOnce = value => mock.implementOnce(() => Promise.reject(value))
    mock.named = value => { name = String(value); return mock }
    state.mocks.add(mock)
    return mock
  }
  createMock.order = 1

  function spyOn(object, key, accessType) {
    if (object == null) throw new TypeError('spyOn requires an object')
    const descriptor = Object.getOwnPropertyDescriptor(object, key)
    if (!descriptor) throw new Error(`Property ${String(key)} does not exist`)
    const original = accessType ? descriptor[accessType] : object[key]
    if (typeof original !== 'function') throw new TypeError(`Property ${String(key)} is not a function`)
    const mock = createMock(function (...args) { return Reflect.apply(original, this, args) })
    const restore = () => Object.defineProperty(object, key, descriptor)
    Object.defineProperty(mock, '_restore', { value: restore, configurable: true })
    if (accessType) Object.defineProperty(object, key, { ...descriptor, [accessType]: mock })
    else object[key] = mock
    state.spies.add(mock)
    return mock
  }

  function replaceProperty(object, key, value) {
    const descriptor = Object.getOwnPropertyDescriptor(object, key)
    if (!descriptor || !descriptor.configurable) throw new Error(`Property ${String(key)} is not replaceable`)
    Object.defineProperty(object, key, { ...descriptor, value })
    const replaced = {
      replace(next) { Object.defineProperty(object, key, { ...descriptor, value: next }); return undefined },
      restore() { Object.defineProperty(object, key, descriptor) },
    }
    state.spies.add(replaced)
    return replaced
  }

  function clearAllMocks() { state.mocks.forEach(value => value.clear()); return mock }
  function resetAllMocks() { state.mocks.forEach(value => value.reset()); return mock }
  function restoreAllMocks() { state.spies.forEach(value => value.restore()); state.spies.clear(); return mock }

  function installFakeTimers(options = {}) {
    if (state.timerState) return
    if (!options || typeof options !== 'object') throw new TypeError('clock.fake() options must be an object')
    const now = Number(options.now ?? realDateNow())
    const limit = options.timerLimit === undefined ? 100000 : Number(options.timerLimit)
    if (!Number.isFinite(now)) throw new TypeError('clock.fake() now must be a finite timestamp or Date')
    if (!Number.isSafeInteger(limit) || limit <= 0) throw new TypeError('clock.fake() timerLimit must be a positive safe integer')
    const clock = { now, nextId: 1, timers: new Map(), running: null, limit }
    const exclusions = {
      date: ['Date'], performance: ['performance'], timeout: ['setTimeout', 'clearTimeout'],
      interval: ['setInterval', 'clearInterval'], immediate: ['setImmediate', 'clearImmediate'],
      microtask: ['queueMicrotask'], animationFrame: ['requestAnimationFrame', 'cancelAnimationFrame'],
      idleCallback: ['requestIdleCallback', 'cancelIdleCallback'],
    }
    const excluded = options.exclude || []
    if (!Array.isArray(excluded) || excluded.some(value => !Object.prototype.hasOwnProperty.call(exclusions, value))) {
      throw new TypeError(`clock.fake() exclude must contain only ${Object.keys(exclusions).join(', ')}`)
    }
    const doNotFake = new Set(excluded.flatMap(value => exclusions[value]))
    const schedule = (kind, callback, delay = 0, interval = null, args = []) => {
      const id = clock.nextId++
      const normalizedDelay = Math.max(0, Number(delay) || 0)
      clock.timers.set(id, { id, kind, callback, due: clock.now + normalizedDelay, interval, args, stack: new Error().stack || null })
      return id
    }
    state.timerState = { clock, originals: { Date: globalThis.Date, performance: globalThis.performance, setTimeout: globalThis.setTimeout, clearTimeout: globalThis.clearTimeout, setInterval: globalThis.setInterval, clearInterval: globalThis.clearInterval, setImmediate: globalThis.setImmediate, clearImmediate: globalThis.clearImmediate, queueMicrotask: globalThis.queueMicrotask, requestAnimationFrame: globalThis.requestAnimationFrame, cancelAnimationFrame: globalThis.cancelAnimationFrame, requestIdleCallback: globalThis.requestIdleCallback, cancelIdleCallback: globalThis.cancelIdleCallback } }
    const clear = id => {
      if (clock.running === id) clock.running = -id
      else clock.timers.delete(id)
    }
    if (!doNotFake.has('setTimeout')) globalThis.setTimeout = (fn, delay, ...args) => schedule('timeout', fn, delay, null, args)
    if (!doNotFake.has('setInterval')) globalThis.setInterval = (fn, delay, ...args) => schedule('interval', fn, delay, Math.max(1, Number(delay) || 0), args)
    if (!doNotFake.has('clearTimeout')) globalThis.clearTimeout = clear
    if (!doNotFake.has('clearInterval')) globalThis.clearInterval = clear
    if (!doNotFake.has('setImmediate')) globalThis.setImmediate = (fn, ...args) => schedule('immediate', fn, 0, null, args)
    if (!doNotFake.has('clearImmediate')) globalThis.clearImmediate = clear
    if (!doNotFake.has('queueMicrotask')) globalThis.queueMicrotask = fn => schedule('microtask', fn, 0)
    if (!doNotFake.has('requestAnimationFrame')) globalThis.requestAnimationFrame = fn => schedule('animation-frame', () => fn(clock.now), 16)
    if (!doNotFake.has('cancelAnimationFrame')) globalThis.cancelAnimationFrame = id => clock.timers.delete(id)
    if (!doNotFake.has('requestIdleCallback')) globalThis.requestIdleCallback = fn => schedule('idle-callback', () => fn({ didTimeout: false, timeRemaining: () => 50 }), 1)
    if (!doNotFake.has('cancelIdleCallback')) globalThis.cancelIdleCallback = id => clock.timers.delete(id)
    if (!doNotFake.has('Date')) {
      const OriginalDate = state.timerState.originals.Date
      class FakeDate extends OriginalDate {
        constructor(...args) { super(...(args.length ? args : [clock.now])) }
        static now() { return clock.now }
      }
      FakeDate.parse = OriginalDate.parse
      FakeDate.UTC = OriginalDate.UTC
      globalThis.Date = FakeDate
    }
    if (!doNotFake.has('performance')) globalThis.performance = { now: () => clock.now }
  }

  function nextTimer(end = Infinity) {
    const timers = [...state.timerState.clock.timers.values()].filter(timer => timer.due <= end)
    timers.sort((a, b) => a.due - b.due || a.id - b.id)
    return timers[0]
  }

  async function drainFakeMicrotasks(clock) {
    let count = 0
    for (;;) {
      const timers = [...clock.timers.values()]
        .filter(timer => timer.kind === 'microtask')
        .sort((left, right) => left.id - right.id)
      const timer = timers[0]
      if (!timer) return
      if (++count > clock.limit) throw new Error(`Aborting after running ${clock.limit} microtasks`)
      await executeFakeTimer(clock, timer, false)
    }
  }

  async function executeFakeTimer(clock, timer, drainMicrotasks = true) {
    clock.timers.delete(timer.id)
    clock.running = timer.id
    let thrown
    try { Reflect.apply(timer.callback, undefined, timer.args) } catch (error) { thrown = error }
    const cancelled = clock.running === -timer.id
    clock.running = null
    if (timer.interval !== null && !cancelled && state.timerState?.clock === clock) {
      timer.due = clock.now + timer.interval
      clock.timers.set(timer.id, timer)
    }
    await clockCheckpoint()
    if (drainMicrotasks && state.timerState?.clock === clock) await drainFakeMicrotasks(clock)
    if (thrown !== undefined) throw thrown
  }

  async function advanceTimers(ms = Infinity) {
    if (!state.timerState) throw new Error('Fake timers are not enabled')
    const clock = state.timerState.clock
    const end = ms === Infinity ? Infinity : clock.now + Number(ms)
    let count = 0
    for (;;) {
      const timer = nextTimer(end)
      if (!timer) break
      if (++count > clock.limit) throw new Error(`Aborting after running ${clock.limit} timers`)
      clock.now = timer.due
      await executeFakeTimer(clock, timer)
      if (state.timerState?.clock !== clock) return
    }
    if (end !== Infinity) clock.now = end
  }

  function restoreClock() {
    if (!state.timerState) return clock
    Object.assign(globalThis, state.timerState.originals)
    state.timerState = null
    return clock
  }

  function installRealTimerTracking() {
    if (state.realTimerTracking) return
    const originals = {
      setTimeout: globalThis.setTimeout,
      clearTimeout: globalThis.clearTimeout,
      setInterval: globalThis.setInterval,
      clearInterval: globalThis.clearInterval,
      setImmediate: globalThis.setImmediate,
      clearImmediate: globalThis.clearImmediate,
      requestAnimationFrame: globalThis.requestAnimationFrame,
      cancelAnimationFrame: globalThis.cancelAnimationFrame,
      requestIdleCallback: globalThis.requestIdleCallback,
      cancelIdleCallback: globalThis.cancelIdleCallback,
    }
    const pending = new Map()
    let nextHandle = 0
    const cancelNative = timer => {
      if (timer.nativeHandle === undefined) return
      const handle = timer.nativeHandle
      timer.nativeHandle = undefined
      try { Reflect.apply(timer.nativeClear, globalThis, [handle]) } catch {}
    }
    const arm = timer => {
      if (!pending.has(timer.handle) || timer.nativeHandle !== undefined) return
      const wait = Math.max(0, timer.due - realDateNow())
      const invoke = function (...nativeArgs) {
        timer.nativeHandle = undefined
        if (!pending.has(timer.handle)) return
        if (!timer.repeating) pending.delete(timer.handle)
        try {
          const args = timer.callbackArgs === null ? nativeArgs : timer.callbackArgs
          return Reflect.apply(timer.callback, this, args)
        } finally {
          if (timer.repeating && pending.has(timer.handle)) {
            timer.due = realDateNow() + timer.delay
            arm(timer)
          }
        }
      }
      if (timer.kind === 'timeout' || timer.kind === 'interval') {
        timer.nativeHandle = Reflect.apply(originals.setTimeout, globalThis, [invoke, wait])
      } else {
        timer.nativeHandle = Reflect.apply(timer.nativeSchedule, globalThis, [invoke, ...timer.scheduleArgs])
      }
    }
    const cancel = handle => {
      const timer = pending.get(handle)
      if (!timer) return false
      pending.delete(handle)
      cancelNative(timer)
      return true
    }
    const wrap = (setName, clearName, kind, repeating = false) => {
      const schedule = originals[setName]
      const clear = originals[clearName]
      if (typeof schedule !== 'function' || typeof clear !== 'function') return
      globalThis[setName] = function (callback, ...args) {
        if (typeof callback !== 'function') return Reflect.apply(schedule, this, [callback, ...args])
        const handle = ++nextHandle
        const owner = state.activeTest ? state.activeTest.fullName : null
        const stack = new Error(`Pending ${kind} scheduled here`).stack || null
        const hasDelay = kind === 'timeout' || kind === 'interval'
        const delay = hasDelay ? Math.max(0, Number(args[0]) || 0) : 0
        const timer = {
          handle,
          kind,
          owner,
          stack,
          callback,
          callbackArgs: hasDelay || kind === 'immediate' ? args.slice(hasDelay ? 1 : 0) : null,
          scheduleArgs: hasDelay || kind === 'immediate' ? [] : args,
          delay: repeating ? Math.max(1, delay) : delay,
          due: realDateNow() + delay,
          repeating,
          nativeSchedule: schedule,
          nativeClear: hasDelay ? originals.clearTimeout : clear,
          nativeHandle: undefined,
        }
        pending.set(handle, timer)
        arm(timer)
        return handle
      }
      globalThis[clearName] = function (handle) {
        if (cancel(handle)) return
        return Reflect.apply(clear, this, [handle])
      }
    }
    wrap('setTimeout', 'clearTimeout', 'timeout')
    wrap('setInterval', 'clearInterval', 'interval', true)
    wrap('setImmediate', 'clearImmediate', 'immediate')
    wrap('requestAnimationFrame', 'cancelAnimationFrame', 'animation frame')
    wrap('requestIdleCallback', 'cancelIdleCallback', 'idle callback')
    state.realTimerTracking = {
      originals,
      pending,
      pause() { for (const timer of pending.values()) cancelNative(timer) },
      resume() { for (const timer of pending.values()) arm(timer) },
      cancel,
    }
  }

  function pauseRealTimerTracking() { state.realTimerTracking?.pause() }
  function resumeRealTimerTracking() { state.realTimerTracking?.resume() }

  function recordTimerLeak(timer, owner, fake) {
    const scope = owner ? `Test ${JSON.stringify(owner)}` : 'The test suite'
    state.leaks.push({
      kind: 'timer',
      description: `${scope} left a pending ${fake ? 'fake ' : ''}${timer.kind}`,
      stack: timer.stack || null,
    })
  }

  function collectFakeTimerLeaks(owner) {
    if (!state.timerState) return
    for (const timer of state.timerState.clock.timers.values()) {
      if (timer.kind !== 'microtask') recordTimerLeak(timer, owner, true)
    }
    state.timerState.clock.timers.clear()
  }

  function collectRealTimerLeaks(owner, all = false) {
    const tracking = state.realTimerTracking
    if (!tracking) return
    for (const [handle, timer] of [...tracking.pending]) {
      if (!all && timer.owner !== owner) continue
      recordTimerLeak(timer, timer.owner || owner, false)
      tracking.cancel(handle)
    }
  }

  const mock = {
    fn: createMock, spyOn, replaceProperty,
    clearAll: clearAllMocks, resetAll: resetAllMocks, restoreAll: restoreAllMocks,
    module(moduleName, factory) {
      if (typeof factory !== 'function') throw new TypeError('mock.module() requires a factory')
      const {specifier} = normalizeModuleRequest(moduleName)
      state.moduleMocks.set(specifier, {
        factory,
        evaluated: false,
        evaluating: null,
        value: undefined,
      })
      return mock
    },
    async import(moduleName) { return await importWakeModule(moduleName, true, false) },
    async actual(moduleName) { return await importWakeModule(moduleName, false, true) },
    async isolate(fn) {
      if (typeof fn !== 'function') throw new TypeError('mock.isolate() requires a function')
      const previousCache = state.moduleCache
      const previousMocks = state.moduleMocks
      state.moduleCache = new Map()
      state.moduleMocks = new Map([...previousMocks].map(([specifier, entry]) => [specifier, {
        factory: entry.factory,
        evaluated: false,
        evaluating: null,
        value: undefined,
      }]))
      try { return await fn() } finally {
        state.moduleCache = previousCache
        state.moduleMocks = previousMocks
      }
    },
  }

  async function clockCheckpoint() {
    await Promise.resolve()
    await Promise.resolve()
  }

  async function clockAct(callback) {
    if (reactRoots.size) return reactAct(async () => { await callback(); await clockCheckpoint() })
    await callback()
    await clockCheckpoint()
  }

  const clock = {
    async fake(options) { installFakeTimers(options); await clockCheckpoint(); return clock },
    async restore() { restoreClock(); await clockCheckpoint(); return clock },
    async advanceBy(milliseconds) { await clockAct(() => advanceTimers(Number(milliseconds))) },
    async advanceTo(value) {
      if (!state.timerState) installFakeTimers()
      const target = value instanceof Date ? value.getTime() : Number(value)
      await clockAct(() => advanceTimers(Math.max(0, target - state.timerState.clock.now)))
    },
    async runNext() {
      if (!state.timerState) installFakeTimers()
      const timer = nextTimer()
      if (timer) await clockAct(async () => {
        const timerState = state.timerState
        if (!timerState) return
        timerState.clock.now = timer.due
        await executeFakeTimer(timerState.clock, timer)
      })
      else await clockCheckpoint()
      return Boolean(timer)
    },
    async runAll() { if (!state.timerState) installFakeTimers(); await clockAct(() => advanceTimers()) },
    async flushMicrotasks() {
      if (state.timerState) await clockAct(() => drainFakeMicrotasks(state.timerState.clock))
      else await clockCheckpoint()
    },
  }

  function urlMatches(pattern, request) {
    if (pattern instanceof URL) return pattern.href === request.url.href
    if (pattern instanceof RegExp) {
      const matches = pattern.test(request.url.href)
      pattern.lastIndex = 0
      return matches
    }
    const text = String(pattern)
    if (text.includes('*')) {
      const expression = text.split('*').map(value => value.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('.*')
      return new RegExp('^' + expression + '$').test(request.url.href)
    }
    return text === request.url.href
  }

  function routeMatches(pattern, request) {
    if (typeof pattern === 'function') return Boolean(pattern(request))
    if (pattern && typeof pattern === 'object' && !(pattern instanceof URL) && !(pattern instanceof RegExp)) {
      if (pattern.method && String(pattern.method).toUpperCase() !== request.method) return false
      return pattern.url === undefined || urlMatches(pattern.url, request)
    }
    return urlMatches(pattern, request)
  }

  function configuredHostAllowed(url) {
    if (state.networkMode === 'allow') return true
    const hostname = String(url.hostname || '').toLowerCase()
    return state.networkAllowHosts.some(value => {
      value = String(value).toLowerCase()
      if (value.startsWith('*.')) {
        const suffix = value.slice(1)
        return hostname.endsWith(suffix) && hostname.length > suffix.length
      }
      return hostname === value
    })
  }

  async function normalizeNetworkRequest(input, init) {
    const value = new Request(input, init)
    let body = null
    if (value.method !== 'GET' && value.method !== 'HEAD') {
      try {
        const bytes = new Uint8Array(await value.clone().arrayBuffer())
        if (bytes.byteLength) body = bytes
      } catch {}
    }
    return Object.freeze({
      id: `wake-request-${++state.networkRequestId}`,
      url: new URL(value.url),
      method: String(value.method || 'GET').toUpperCase(),
      headers: new Headers(value.headers),
      body,
    })
  }

  function isNativeResponseBody(value) {
    return typeof value === 'string'
      || value instanceof ArrayBuffer
      || ArrayBuffer.isView(value)
      || (typeof Blob !== 'undefined' && value instanceof Blob)
      || (typeof FormData !== 'undefined' && value instanceof FormData)
      || (typeof URLSearchParams !== 'undefined' && value instanceof URLSearchParams)
      || (typeof ReadableStream !== 'undefined' && value instanceof ReadableStream)
  }

  async function normalizeNetworkResponse(value) {
    if (value instanceof Response) return value
    if (!value || typeof value !== 'object') {
      throw new TypeError('network.route() handlers must return a Response or NetworkResponse object')
    }
    const headers = new Headers(value.headers || {})
    let body = value.body === undefined ? null : value.body
    if (body !== null && typeof body === 'object' && !isNativeResponseBody(body)) {
      body = JSON.stringify(body)
      if (!headers.has('content-type')) headers.set('content-type', 'application/json')
    }
    if (value.delayMs !== undefined) {
      const delay = Number(value.delayMs)
      if (!Number.isFinite(delay) || delay < 0) throw new TypeError('network response delayMs must be a non-negative number')
      if (delay > 0) await new Promise(resolve => setTimeout(resolve, delay))
    }
    return new Response(body, {
      status: value.status === undefined ? 200 : Number(value.status),
      statusText: value.statusText === undefined ? '' : String(value.statusText),
      headers,
    })
  }

  async function decideNetworkRequest(request) {
    for (let index = state.networkRoutes.length - 1; index >= 0; index--) {
      const route = state.networkRoutes[index]
      if (!routeMatches(route.pattern, request)) continue
      if (route.allow) return { action: 'continue' }
      return { action: 'fulfill', response: await normalizeNetworkResponse(await route.handler(request)) }
    }
    return configuredHostAllowed(request.url) ? { action: 'continue' } : { action: 'fail' }
  }

  function normalizeBrowserNetworkRequest(value) {
    if (!value || typeof value !== 'object' || !Array.isArray(value.headers)) {
      throw new TypeError('Wake browser network bridge requires an owned request object')
    }
    const headers = new Headers()
    for (const entry of value.headers) {
      if (!entry || typeof entry !== 'object') throw new TypeError('Wake browser request headers must be owned name/value objects')
      headers.append(String(entry.name), String(entry.value))
    }
    if (value.body !== null && value.body !== undefined && !Array.isArray(value.body)) {
      throw new TypeError('Wake browser request body must be an owned byte array')
    }
    const bytes = value.body === null || value.body === undefined
      ? null
      : Uint8Array.from(value.body, byte => {
          const value = Number(byte)
          if (!Number.isInteger(value) || value < 0 || value > 255) throw new TypeError('Wake browser request body contains an invalid byte')
          return value
        })
    return Object.freeze({
      id: `wake-request-${++state.networkRequestId}`,
      url: new URL(String(value.url)),
      method: String(value.method || 'GET').toUpperCase(),
      headers,
      body: bytes && bytes.byteLength ? bytes : null,
    })
  }

  async function handleBrowserNetworkRequest(value) {
    const request = normalizeBrowserNetworkRequest(value)
    state.networkRequests.push(request)
    try {
      const decision = await decideNetworkRequest(request)
      if (decision.action === 'continue') return JSON.stringify({ action: 'continue' })
      if (decision.action === 'fail') return JSON.stringify({ action: 'fail', errorReason: 'BlockedByClient' })
      const response = decision.response
      const headers = [...response.headers.entries()]
        .map(([name, headerValue]) => ({ name: String(name), value: String(headerValue) }))
        .sort((left, right) => left.name < right.name ? -1 : left.name > right.name ? 1 : left.value < right.value ? -1 : left.value > right.value ? 1 : 0)
      const body = Array.from(new Uint8Array(await response.arrayBuffer()))
      return JSON.stringify({
        action: 'fulfill',
        status: response.status,
        statusText: response.statusText,
        headers,
        body,
      })
    } catch (error) {
      return JSON.stringify({
        action: 'fail',
        errorReason: 'Failed',
        message: String(error && error.message || error),
      })
    }
  }

  function isRedirectStatus(status) {
    return status === 301 || status === 302 || status === 303 || status === 307 || status === 308
  }

  function domNetworkTransport(request) {
    const result = host('httpRequest', {
      url: request.url.href,
      method: request.method,
      headers: [...request.headers.entries()].map(([name, value]) => ({name, value})),
      body: request.body ? Array.from(request.body) : [],
      timeoutMs: state.defaultTimeout,
    })
    if (!result || typeof result !== 'object' || !Array.isArray(result.headers) || !Array.isArray(result.body)) {
      throw Object.assign(new Error('Wake DOM HTTP transport returned an invalid response'), { code: 'WAKE_TEST_NETWORK' })
    }
    const headers = new Headers()
    for (const header of result.headers) headers.append(String(header.name), String(header.value))
    const body = Uint8Array.from(result.body, byte => {
      const value = Number(byte)
      if (!Number.isInteger(value) || value < 0 || value > 255) throw new TypeError('Wake DOM HTTP response contains an invalid byte')
      return value
    })
    return new Response(request.method === 'HEAD' || body.byteLength === 0 ? null : body, {
      status: Number(result.status),
      statusText: String(result.statusText || ''),
      headers,
    })
  }

  async function performDomFetch(input, init) {
    let current = new Request(input, init)
    let redirected = false
    for (let redirectCount = 0; redirectCount <= 20; redirectCount++) {
      const request = await normalizeNetworkRequest(current)
      state.networkRequests.push(request)
      const decision = await decideNetworkRequest(request)
      if (decision.action === 'fulfill') return decision.response
      if (decision.action === 'fail') {
        throw Object.assign(new Error('Network request denied: ' + request.url.href), { code: 'WAKE_TEST_NETWORK' })
      }
      const response = domNetworkTransport(request)
      const location = response.headers.get('location')
      if (!isRedirectStatus(response.status) || !location || current.redirect === 'manual') {
        try {
          Object.defineProperties(response, {
            redirected: { value: redirected, configurable: true },
            url: { value: request.url.href, configurable: true },
          })
        } catch {}
        return response
      }
      if (current.redirect === 'error') {
        throw Object.assign(new Error('Network redirect is forbidden for ' + request.url.href), { code: 'WAKE_TEST_NETWORK' })
      }
      if (redirectCount === 20) {
        throw Object.assign(new Error('Network request exceeded 20 redirects'), { code: 'WAKE_TEST_NETWORK' })
      }
      redirected = true
      const target = new URL(location, request.url)
      const rewriteToGet = response.status === 303
        ? current.method !== 'GET' && current.method !== 'HEAD'
        : (response.status === 301 || response.status === 302) && current.method === 'POST'
      const headers = new Headers(current.headers)
      if (rewriteToGet) {
        headers.delete('content-length')
        headers.delete('content-type')
      }
      current = new Request(target, {
        method: rewriteToGet ? 'GET' : current.method,
        headers,
        body: rewriteToGet || request.body === null ? undefined : request.body,
        redirect: current.redirect,
      })
    }
    throw Object.assign(new Error('Network redirect loop did not terminate'), { code: 'WAKE_TEST_NETWORK' })
  }

  function installNetworkFetch() {
    if (globalThis.fetch && globalThis.fetch.__wakeNetworkFetch) return
    const original = globalThis.fetch
    state.networkFetchOriginal ||= original
    const controlled = async (input, init = {}) => {
      if (state.environment === 'browser') {
        if (typeof state.networkFetchOriginal === 'function') {
          return await state.networkFetchOriginal.call(globalThis, input, init)
        }
        throw Object.assign(new Error('The browser environment has no native fetch transport'), { code: 'WAKE_TEST_NETWORK' })
      }
      return await performDomFetch(input, init)
    }
    controlled.__wakeNetworkFetch = true
    globalThis.fetch = controlled
  }

  const network = {
    route(pattern, handler) {
      if (typeof handler !== 'function') throw new TypeError('network.route() requires a handler')
      installNetworkFetch()
      const route = { pattern, handler, allow: false }
      state.networkRoutes.push(route)
      return () => { const index = state.networkRoutes.indexOf(route); if (index >= 0) state.networkRoutes.splice(index, 1) }
    },
    allow(pattern) {
      installNetworkFetch()
      const route = { pattern, allow: true }
      state.networkRoutes.push(route)
      return () => { const index = state.networkRoutes.indexOf(route); if (index >= 0) state.networkRoutes.splice(index, 1) }
    },
    requests() { return state.networkRequests.slice() },
    reset() { state.networkRoutes.length = 0; state.networkRequests.length = 0 },
  }

  const reactRoots = new Map()

  function formatConsoleArguments(values) {
    if (!values.length) return ''
    let index = 1
    let message = String(values[0])
    message = message.replace(/%[sdifoO]/g, token => {
      if (index >= values.length) return token
      const value = values[index++]
      if (token === '%d' || token === '%i' || token === '%f') return String(Number(value))
      if ((token === '%o' || token === '%O') && value && typeof value === 'object') {
        try { return JSON.stringify(value) } catch {}
      }
      return String(value)
    })
    if (index < values.length) message += ' ' + values.slice(index).map(String).join(' ')
    return message
  }

  function isReactActWarning(message) {
    return message.includes('not wrapped in act(')
      || message.includes('not configured to support act(')
      || message.includes('overlapping act() calls')
      || (message.includes('act(') && message.includes('without await'))
  }

  function recordReactActWarning(message) {
    if (state.reactActWarnings === 'off') return
    const scheduler = state.scheduler
    const step = scheduler && scheduler.currentStep
    const identity = `${step ? step.id : 'suite'}\0${message}`
    if (state.reactActWarningKeys.has(identity)) return
    state.reactActWarningKeys.add(identity)
    const stack = step && step.registrationStack ? step.registrationStack : new Error(message).stack || null
    const diagnostic = {code: 'WAKE_TEST_ACT', message, stack}
    state.diagnostics.push(diagnostic)
    if (state.reactActWarnings !== 'error') return
    const failure = {message, code: 'WAKE_TEST_ACT', stack, diff: null}
    if (scheduler && step) schedulerRecordFailure(step, failure)
    else if (scheduler) scheduler.suiteFailures.push(failure)
    else state.pendingActFailures.push(failure)
  }

  function installReactActWarningCapture() {
    if (!globalThis.console || globalThis.console.__wakeActWarningCapture) return
    const target = globalThis.console
    for (const method of ['error', 'warn']) {
      const original = target[method]
      if (typeof original !== 'function') continue
      target[method] = function (...values) {
        const message = formatConsoleArguments(values)
        if (isReactActWarning(message)) {
          recordReactActWarning(message)
          return
        }
        return Reflect.apply(original, this, values)
      }
    }
    Object.defineProperty(target, '__wakeActWarningCapture', {value: true})
  }

  function reactRuntime() {
    if (state.reactRuntimeOverride) return state.reactRuntimeOverride
    const React = wakeRequire('react')
    const ReactDOM = wakeRequire('react-dom/client')
    if (!React || typeof React.act !== 'function' || !ReactDOM || typeof ReactDOM.createRoot !== 'function') {
      throw Object.assign(new Error('React 19.2 and react-dom/client are required by @crab-dev/wake/test/react'), { code: 'WAKE_TEST_REACT_VERSION' })
    }
    return { React, ReactDOM }
  }

  function prepareReactRuntimeRecovery() {
    const previousCache = state.moduleCache
    const previousReact = wakeRequire('react', false, true)
    const isolatedCache = new Map()
    state.moduleCache = isolatedCache
    try {
      const React = wakeRequire('react', false, true)
      const ReactDOM = wakeRequire('react-dom/client', false, true)
      return {React, ReactDOM, previousReact}
    } finally {
      state.moduleCache = previousCache
    }
  }

  function bridgeInterruptedReactRuntime(recovery) {
    const key = '__CLIENT_INTERNALS_DO_NOT_USE_OR_WARN_USERS_THEY_CANNOT_UPGRADE'
    const previous = recovery.previousReact && recovery.previousReact[key]
    const replacement = recovery.React && recovery.React[key]
    if (!previous || !replacement) return
    // User modules retain their original React hook functions for the lifetime of the suite realm.
    // Point only their dispatcher/context slots at the replacement React 19.2 runtime; the old
    // act implementation and the interrupted ReactDOM graph are never used again.
    for (const field of ['H', 'A', 'T', 'S', 'getCurrentStack']) {
      Object.defineProperty(previous, field, {
        get() { return replacement[field] },
        set(value) { replacement[field] = value },
        configurable: true,
      })
    }
  }

  async function reactAct(callback) {
    const { React } = reactRuntime()
    let value
    try {
      await React.act(async () => { value = await callback() })
    } catch (error) {
      // React can report an uncaught root error in the checkpoint immediately following act's
      // rejected Promise. Drain it before preserving the original render failure for the caller.
      await clockCheckpoint()
      throw error
    }
    return value
  }

  async function optionalReactAct(callback) {
    let React
    try { React = wakeRequire('react') } catch {}
    if (!React || typeof React.act !== 'function') return callback()
    let value
    await React.act(async () => { value = await callback() })
    return value
  }

  async function reactCleanup() {
    if (!state.reactCleanup) return
    if (reactRoots.size) state.reactRecoveryRuntime = prepareReactRuntimeRecovery()
    let completed = false
    try {
      for (const [container, root] of [...reactRoots]) {
        await reactAct(async () => root.unmount())
        reactRoots.delete(container)
        if (container.parentNode) container.parentNode.removeChild(container)
      }
      completed = true
    } catch (error) {
      state.reactRecoveryRuntime = null
      throw error
    } finally {
      // V8 termination deliberately bypasses this finally block. The scheduler then activates the
      // prebuilt ReactDOM graph so later cases never reuse a reconciler interrupted during unmount.
      if (completed || !reactRoots.size) state.reactRecoveryRuntime = null
    }
  }

  function textMatches(value, matcher, exact = true) {
    value = String(value || '').replace(/\s+/g, ' ').trim()
    if (matcher instanceof RegExp) return matcher.test(value)
    if (typeof matcher === 'function') return Boolean(matcher(value))
    const expected = String(matcher).replace(/\s+/g, ' ').trim()
    return exact ? value === expected : value.toLowerCase().includes(expected.toLowerCase())
  }

  function elementRole(element) {
    const explicit = element.getAttribute && element.getAttribute('role')
    if (explicit) return explicit
    const tag = String(element.localName || '').toLowerCase()
    if (tag === 'button') return 'button'
    if (tag === 'a' && element.hasAttribute('href')) return 'link'
    if (/^h[1-6]$/.test(tag)) return 'heading'
    if (tag === 'img') return 'img'
    if (tag === 'select') return element.multiple ? 'listbox' : 'combobox'
    if (tag === 'textarea') return 'textbox'
    if (tag === 'input') {
      const type = String(element.type || 'text').toLowerCase()
      if (type === 'checkbox') return 'checkbox'
      if (type === 'radio') return 'radio'
      if (['button', 'submit', 'reset'].includes(type)) return 'button'
      return 'textbox'
    }
    if (tag === 'form') return 'form'
    if (tag === 'table') return 'table'
    if (tag === 'ul' || tag === 'ol') return 'list'
    if (tag === 'li') return 'listitem'
    return null
  }

  function accessibleName(element) {
    const direct = element.getAttribute && element.getAttribute('aria-label')
    if (direct) return direct
    const labelled = element.getAttribute && element.getAttribute('aria-labelledby')
    if (labelled) return labelled.split(/\s+/).map(id => document.getElementById(id)?.textContent || '').join(' ').trim()
    if (element.labels && element.labels.length) return [...element.labels].map(label => label.textContent).join(' ').trim()
    if (element.alt) return element.alt
    if (element.title) return element.title
    if (element.value && ['button', 'submit', 'reset'].includes(String(element.type))) return element.value
    return element.textContent || ''
  }

  function accessibleDescription(element) {
    const described = element.getAttribute && element.getAttribute('aria-describedby')
    if (described) return described.split(/\s+/).map(id => document.getElementById(id)?.textContent || '').join(' ').trim()
    return element.getAttribute && (element.getAttribute('aria-description') || element.getAttribute('title')) || ''
  }

  function displayedValue(element) {
    if (element && element.options) { const values = [...element.options].filter(option => option.selected).map(option => option.textContent); return element.multiple ? values : values[0] }
    return element && element.value
  }

  function candidates(container) {
    const values = []
    if (container && container.nodeType === 1) values.push(container)
    if (container && typeof container.querySelectorAll === 'function') values.push(...container.querySelectorAll('*'))
    return values
  }

  function queryValues(container, kind, matcher, options = {}) {
    const exact = options.exact !== false
    return candidates(container).filter(element => {
      if (options.hidden !== true && (element.hidden || element.getAttribute('aria-hidden') === 'true')) return false
      switch (kind) {
        case 'Role': return elementRole(element) === String(matcher) && (!options.name || textMatches(accessibleName(element), options.name, exact))
        case 'LabelText': return textMatches(accessibleName(element), matcher, exact)
        case 'Text': return textMatches(element.textContent, matcher, exact) && ![...element.children].some(child => textMatches(child.textContent, matcher, exact))
        case 'DisplayValue': return textMatches(element.value, matcher, exact)
        case 'PlaceholderText': return textMatches(element.getAttribute('placeholder'), matcher, exact)
        case 'AltText': return textMatches(element.getAttribute('alt'), matcher, exact)
        case 'Title': return textMatches(element.getAttribute('title'), matcher, exact)
        case 'TestId': return textMatches(element.getAttribute(state.testIdAttribute), matcher, exact)
        default: return false
      }
    })
  }

  async function waitFor(callback, options = {}) {
    const timeout = Number(options.timeout || 1000)
    const interval = Math.max(1, Number(options.interval || 20))
    const attempts = Math.max(1, Math.ceil(timeout / interval))
    let lastError
    for (let attempt = 0; attempt < attempts; attempt++) {
      try { return await callback() } catch (error) { lastError = error }
      await new Promise(resolve => setTimeout(resolve, interval))
    }
    throw lastError || new Error(`waitFor timed out after ${timeout} ms`)
  }

  async function waitForElementToBeRemoved(value, options) {
    const read = typeof value === 'function' ? value : () => value
    return waitFor(() => {
      const current = read()
      const values = Array.isArray(current) ? current : [current]
      if (values.some(element => element && element.isConnected)) throw new Error('Element is still present')
      return undefined
    }, options)
  }

  function within(container) {
    const queries = {}
    for (const kind of ['Role', 'LabelText', 'Text', 'DisplayValue', 'PlaceholderText', 'AltText', 'Title', 'TestId']) {
      queries[`queryAllBy${kind}`] = (matcher, options) => queryValues(container, kind, matcher, options)
      queries[`queryBy${kind}`] = (matcher, options) => {
        const values = queryValues(container, kind, matcher, options)
        if (values.length > 1) throw new Error(`Found multiple elements by ${kind}`)
        return values[0] || null
      }
      queries[`getAllBy${kind}`] = (matcher, options) => {
        const values = queryValues(container, kind, matcher, options)
        if (!values.length) throw new Error(`Unable to find an element by ${kind}`)
        return values
      }
      queries[`getBy${kind}`] = (matcher, options) => {
        const values = queries[`getAllBy${kind}`](matcher, options)
        if (values.length > 1) throw new Error(`Found multiple elements by ${kind}`)
        return values[0]
      }
      queries[`findAllBy${kind}`] = (matcher, options, waitOptions) => waitFor(() => queries[`getAllBy${kind}`](matcher, options), waitOptions)
      queries[`findBy${kind}`] = (matcher, options, waitOptions) => waitFor(() => queries[`getBy${kind}`](matcher, options), waitOptions)
    }
    queries.debug = (element = container) => prettyDOM(element)
    return queries
  }

  const screen = new Proxy({}, { get(_target, key) { return within(document.body)[key] } })

  async function render(ui, options = {}) {
    const { React, ReactDOM } = reactRuntime()
    const container = options.container || document.body.appendChild(document.createElement('div'))
    const rootOptions = { ...options }
    for (const key of ['container', 'baseElement', 'hydrate', 'strict', 'wrapper', 'initialProps']) delete rootOptions[key]
    const strict = options.strict === undefined ? state.reactStrictMode : Boolean(options.strict)
    const wrap = next => {
      const wrapped = options.wrapper ? React.createElement(options.wrapper, null, next) : next
      return strict ? React.createElement(React.StrictMode, null, wrapped) : wrapped
    }
    let root
    if (options.hydrate) await reactAct(async () => { root = ReactDOM.hydrateRoot(container, wrap(ui), rootOptions) })
    else root = ReactDOM.createRoot(container, rootOptions)
    reactRoots.set(container, root)
    if (!options.hydrate) await reactAct(async () => root.render(wrap(ui)))
    return {
      container,
      baseElement: options.baseElement || document.body,
      ...within(options.baseElement || container),
      async rerender(next) { await reactAct(async () => root.render(wrap(next))) },
      async unmount() { await reactAct(async () => root.unmount()); reactRoots.delete(container) },
      asFragment() { const fragment = document.createDocumentFragment(); for (const child of [...container.childNodes]) fragment.appendChild(child.cloneNode(true)); return fragment },
      debug(element = container) { return prettyDOM(element) },
    }
  }

  async function renderHook(callback, options = {}) {
    const { React } = reactRuntime()
    const result = { current: undefined }
    let props = options.initialProps
    function Hook() { result.current = callback(props); return null }
    const rendered = await render(React.createElement(Hook), options)
    return {
      result,
      async rerender(nextProps = props) { props = nextProps; await rendered.rerender(React.createElement(Hook)) },
      unmount: rendered.unmount,
    }
  }

  async function dispatch(element, event) { return reactAct(async () => element.dispatchEvent(event)) }
  const fireEvent = async (element, event) => dispatch(element, event)
  for (const [name, Constructor, type] of [
    ['click', 'MouseEvent', 'click'], ['mouseDown', 'MouseEvent', 'mousedown'], ['mouseUp', 'MouseEvent', 'mouseup'],
    ['input', 'InputEvent', 'input'], ['change', 'Event', 'change'], ['submit', 'SubmitEvent', 'submit'],
    ['focus', 'FocusEvent', 'focus'], ['blur', 'FocusEvent', 'blur'], ['keyDown', 'KeyboardEvent', 'keydown'], ['keyUp', 'KeyboardEvent', 'keyup'],
  ]) fireEvent[name] = (element, init = {}) => {
    const values = init.target || {}
    for (const [key, value] of Object.entries(values)) {
      try { element[key] = value } catch { Object.defineProperty(element, key, { value, configurable: true }) }
    }
    const eventInit = { bubbles: true, cancelable: true, ...init }
    delete eventInit.target
    return dispatch(element, new globalThis[Constructor](type, eventInit))
  }

  function domUserEvent(options) {
    const delay = () => options.delayMs == null ? Promise.resolve() : new Promise(resolve => setTimeout(resolve, Number(options.delayMs)))
    return {
      async click(element) { await fireEvent.mouseDown(element); if (element.focus) element.focus(); await fireEvent.mouseUp(element); await fireEvent.click(element) },
      async dblClick(element) { await this.click(element); await this.click(element); await dispatch(element, new MouseEvent('dblclick', {bubbles: true, cancelable: true, detail: 2})) },
      async type(element, text) { if (element.focus) element.focus(); for (const character of String(text)) { await delay(); await fireEvent.keyDown(element, { key: character }); element.value = String(element.value || '') + character; await fireEvent.input(element, { data: character, inputType: 'insertText' }); await fireEvent.keyUp(element, { key: character }) } },
      async clear(element) { if (element.focus) element.focus(); element.value = ''; await fireEvent.input(element, {data: null, inputType: 'deleteContentBackward'}); await fireEvent.change(element) },
      async keyboard(text) { const target = document.activeElement || document.body; return this.type(target, text) },
      async tab() { const values = candidates(document.body).filter(value => !value.disabled && (value.tabIndex >= 0 || ['button', 'input', 'select', 'textarea', 'a'].includes(value.localName))); const index = values.indexOf(document.activeElement); values[(index + 1) % values.length]?.focus() },
      async pointer(actions) { for (const action of Array.isArray(actions) ? actions : [actions]) if (action.target) await this.click(action.target) },
      async hover(element) { await dispatch(element, new MouseEvent('mouseover', {bubbles: true})); await dispatch(element, new MouseEvent('mouseenter')) },
      async unhover(element) { await dispatch(element, new MouseEvent('mouseout', {bubbles: true})); await dispatch(element, new MouseEvent('mouseleave')) },
      async selectOptions(element, values) { const requested = (Array.isArray(values) ? values : [values]).map(value => typeof value === 'string' ? value : value.value); for (const option of [...element.options]) option.selected = requested.includes(option.value); await fireEvent.input(element); await fireEvent.change(element) },
      async upload(element, files) { Object.defineProperty(element, 'files', { value: Array.isArray(files) ? files : [files], configurable: true }); await fireEvent.change(element) },
      advanceTimers: options.advanceTimers,
    }
  }

  function browserInputError(message) {
    return Object.assign(new Error(String(message)), { code: 'WAKE_TEST_BROWSER' })
  }

  function browserInputTarget(element) {
    if (!(element instanceof Element)) throw browserInputError('Browser userEvent target must be an Element')
    if (!element.isConnected) throw browserInputError('Browser userEvent target is detached from the document')
    element.scrollIntoView?.({ block: 'center', inline: 'center' })
    const style = getComputedStyle(element)
    if (element.hidden || style.display === 'none' || style.visibility === 'hidden' || Number(style.opacity) === 0) {
      throw browserInputError('Browser userEvent target is not visible')
    }
    const rect = element.getBoundingClientRect()
    if (!Number.isFinite(rect.left) || !Number.isFinite(rect.top) || rect.width <= 0 || rect.height <= 0) {
      throw browserInputError('Browser userEvent target has no layout box')
    }
    const x = rect.left + rect.width / 2
    const y = rect.top + rect.height / 2
    if (x < 0 || y < 0 || x >= innerWidth || y >= innerHeight) {
      throw browserInputError('Browser userEvent target is outside the viewport')
    }
    return { x, y }
  }

  function enqueueBrowserOperation(action, payload = {}) {
    const binding = globalThis.__wakeBrowserOperationBinding
    if (typeof binding !== 'function') throw browserInputError('Wake browser operation binding is unavailable')
    const id = String(++state.browserOperationId)
    return new Promise((resolve, reject) => {
      state.browserOperationPending.set(id, { resolve, reject })
      try {
        binding(JSON.stringify({ schemaVersion: 'wake.browser.operation.v1', id, action, ...payload }))
      } catch (error) {
        state.browserOperationPending.delete(id)
        reject(browserInputError(error && error.message || error))
      }
    })
  }

  // Browser user events travel through Chromium's CDP input pipeline. This is higher fidelity than
  // synthetic dispatch, but it intentionally does not claim complete OS/IME, drag/drop, or native
  // file-chooser simulation; multiple select remains an explicit browser-realm fallback.
  function browserUserEvent(options) {
    const delay = () => options.delayMs == null ? Promise.resolve() : new Promise(resolve => setTimeout(resolve, Number(options.delayMs)))
    const perform = (action, payload, after) => reactAct(async () => {
      await delay()
      await enqueueBrowserOperation(action, payload)
      if (after) await after()
      await Promise.resolve()
    })
    const controller = {
      async click(element) { return perform('click', { target: browserInputTarget(element) }) },
      async dblClick(element) { return perform('doubleClick', { target: browserInputTarget(element) }) },
      async type(element, text) { return perform('type', { target: browserInputTarget(element), text: String(text) }) },
      async clear(element) { return perform('clear', { target: browserInputTarget(element) }) },
      async keyboard(text) { return perform('keyboard', { text: String(text) }) },
      async tab(tabOptions = {}) { return perform('tab', { shift: Boolean(tabOptions.shift) }) },
      async pointer(actions) { for (const action of Array.isArray(actions) ? actions : [actions]) if (action.target) await controller.click(action.target) },
      async hover(element) { return perform('hover', { target: browserInputTarget(element) }) },
      async unhover(element) { return perform('unhover', { target: browserInputTarget(element) }) },
      async selectOptions(element, values) {
        const target = browserInputTarget(element)
        if (element.localName !== 'select') throw browserInputError('selectOptions target must be a select element')
        const requested = (Array.isArray(values) ? values : [values]).map(value => typeof value === 'string' ? value : value && value.value)
        const indexes = requested.map(value => [...element.options].findIndex(option => option.value === String(value)))
        if (indexes.some(index => index < 0)) throw browserInputError('selectOptions value does not match an option')
        const multiple = Boolean(element.multiple)
        return perform('selectOptions', { target, indexes, multiple }, multiple ? async () => {
          for (let index = 0; index < element.options.length; index++) element.options[index].selected = indexes.includes(index)
          element.dispatchEvent(new Event('input', { bubbles: true }))
          element.dispatchEvent(new Event('change', { bubbles: true }))
        } : null)
      },
      async upload(element, files) {
        const target = browserInputTarget(element)
        if (element.localName !== 'input' || String(element.type).toLowerCase() !== 'file') throw browserInputError('upload target must be a file input')
        const values = Array.isArray(files) ? files : [files]
        const serialized = []
        for (const file of values) {
          if (!(file instanceof File)) throw browserInputError('upload requires File values')
          serialized.push({ name: file.name, bytes: Array.from(new Uint8Array(await file.arrayBuffer())) })
        }
        const token = String(++state.browserInputElementId)
        element.setAttribute('data-wake-browser-input', token)
        if (element.focus) element.focus()
        try {
          return await perform('upload', {
            target,
            selector: `[data-wake-browser-input="${token}"]`,
            files: serialized,
          })
        } finally {
          if (element.getAttribute('data-wake-browser-input') === token) element.removeAttribute('data-wake-browser-input')
        }
      },
      advanceTimers: options.advanceTimers,
    }
    return controller
  }

  const userEvent = {
    setup(options = {}) {
      return state.environment === 'browser' ? browserUserEvent(options) : domUserEvent(options)
    },
  }

  function prettyDOM(node = document.body) { return node && (node.outerHTML || node.innerHTML || node.textContent) || '' }

  Object.assign(matchers, {
    toBeInTheDocument: received => result(Boolean(received && received.isConnected), 'Expected element to be in the document'),
    toContainElement: (received, child) => result(Boolean(received && received.contains(child)), 'Expected element to contain child'),
    toContainHTML: (received, html) => result(Boolean(received && String(received.innerHTML).includes(String(html))), `Expected element HTML to contain ${html}`),
    toBeEmptyDOMElement: received => result(Boolean(received && !String(received.textContent).trim() && !received.children.length), 'Expected element to be empty'),
    toHaveTextContent: (received, expected) => result(Boolean(received && textMatches(received.textContent, expected, false)), `Expected element text to match ${expected}`),
    toHaveAttribute: (received, name, expected) => result(Boolean(received && received.hasAttribute(name) && (expected === undefined || equals(received.getAttribute(name), String(expected)))), `Expected element to have attribute ${name}`),
    toHaveClass: (received, ...classes) => result(Boolean(received && classes.every(value => received.classList.contains(value))), `Expected element to have classes ${classes.join(' ')}`),
    toHaveStyle: (received, expected) => { const entries = typeof expected === 'string' ? expected.split(';').map(value => value.split(':').map(part => part.trim())).filter(value => value[0]) : Object.entries(expected || {}); const style = received && getComputedStyle(received); return result(Boolean(style && entries.every(([key, value]) => String(style.getPropertyValue ? style.getPropertyValue(key) || style[key] : style[key]).trim() === String(value).trim())), `Expected element style to contain ${pretty(expected)}`) },
    toHaveValue: (received, expected) => result(Boolean(received && equals(received.value, expected)), `Expected element value to equal ${pretty(expected)}`),
    toHaveDisplayValue: (received, expected) => result(Boolean(received && equals(displayedValue(received), expected)), `Expected displayed value to equal ${pretty(expected)}`),
    toHaveFormValues: (received, expected) => { const values = {}; for (const control of [...received.elements || []]) { if (!control.name || control.disabled) continue; if ((control.type === 'checkbox' || control.type === 'radio') && !control.checked) continue; const value = control.type === 'checkbox' ? true : control.value; if (Object.prototype.hasOwnProperty.call(values, control.name)) values[control.name] = [].concat(values[control.name], value); else values[control.name] = value } return result(subset(values, expected), `Expected form values ${pretty(expected)}, received ${pretty(values)}`) },
    toBeChecked: received => result(Boolean(received && received.checked), 'Expected element to be checked'),
    toBePartiallyChecked: received => result(Boolean(received && (received.indeterminate || received.getAttribute?.('aria-checked') === 'mixed')), 'Expected element to be partially checked'),
    toBeDisabled: received => result(Boolean(received && (received.disabled || received.closest?.('[disabled]'))), 'Expected element to be disabled'),
    toBeEnabled: received => result(Boolean(received && !received.disabled && !received.closest?.('[disabled]')), 'Expected element to be enabled'),
    toBeRequired: received => result(Boolean(received && (received.required || received.getAttribute?.('aria-required') === 'true')), 'Expected element to be required'),
    toBeInvalid: received => result(Boolean(received && (received.getAttribute?.('aria-invalid') === 'true' || received.checkValidity?.() === false)), 'Expected element to be invalid'),
    toBeValid: received => result(Boolean(received && received.getAttribute?.('aria-invalid') !== 'true' && received.checkValidity?.() !== false), 'Expected element to be valid'),
    toHaveFocus: received => result(document.activeElement === received, 'Expected element to have focus'),
    toBeVisible: received => { const style = received && getComputedStyle(received); return result(Boolean(received && received.isConnected && !received.hidden && style?.display !== 'none' && style?.visibility !== 'hidden'), 'Expected element to be visible') },
    toHaveAccessibleName: (received, expected) => result(Boolean(received && textMatches(accessibleName(received), expected)), `Expected accessible name to match ${expected}`),
    toHaveAccessibleDescription: (received, expected = '') => result(Boolean(received && textMatches(accessibleDescription(received), expected)), `Expected accessible description to match ${expected}`),
    toHaveAccessibleErrorMessage: (received, expected = '') => { const id = received && received.getAttribute?.('aria-errormessage'); const value = id && document.getElementById(id)?.textContent || ''; return result(Boolean(received && textMatches(value, expected)), `Expected accessible error message to match ${expected}`) },
    toHaveRole: (received, role) => result(Boolean(received && elementRole(received) === String(role)), `Expected element to have role ${role}`),
    toHaveSelection: (received, expected) => { const value = received && typeof received.selectionStart === 'number' ? String(received.value).slice(received.selectionStart, received.selectionEnd) : String(globalThis.getSelection?.() || ''); return result(textMatches(value, expected), `Expected selection to match ${expected}`) },
  })

  const wakeModule = exports => {
    Object.defineProperty(exports, '__esModule', { value: true })
    return exports
  }
  const api = wakeModule({ test, it, describe, beforeAll, beforeEach, afterEach, afterAll, expect, mock, clock, network })
  const reactApi = wakeModule({ ...api, render, renderHook, screen, within, prettyDOM, fireEvent, userEvent, waitFor, waitForElementToBeRemoved, act: reactAct, cleanup: reactCleanup })

  function normalizeModuleRequest(moduleName, parent = null) {
    const edgeKey = String(moduleName)
    const definition = parent && state.moduleDefinitions.get(parent)
    const specifier = definition && Object.prototype.hasOwnProperty.call(definition.requestSpecifiers, edgeKey)
      ? definition.requestSpecifiers[edgeKey]
      : edgeKey
    return {edgeKey, specifier}
  }

  function resolveModuleId(request, parent) {
    const parentDefinition = parent && state.moduleDefinitions.get(parent)
    let id = parentDefinition ? parentDefinition.resolutions[request.edgeKey] : request.specifier
    if (!state.moduleDefinitions.has(id) && !parent) {
      const candidates = new Set()
      for (const definition of state.moduleDefinitions.values()) {
        if (definition.resolutions[request.edgeKey]) candidates.add(definition.resolutions[request.edgeKey])
      }
      if (candidates.size === 1) id = [...candidates][0]
      else if (candidates.size > 1) {
        throw Object.assign(new Error(`mock.import(${JSON.stringify(request.specifier)}) is ambiguous; use a project-root alias`), { code: 'WAKE_TEST_RUNTIME' })
      }
    }
    return id
  }

  async function evaluateModuleMock(entry) {
    if (entry.evaluated) return entry.value
    if (!entry.evaluating) {
      entry.evaluating = Promise.resolve()
        .then(() => entry.factory())
        .then(value => {
          entry.value = value
          entry.evaluated = true
          return value
        })
        .catch(error => {
          entry.evaluating = null
          throw error
        })
    }
    return await entry.evaluating
  }

  async function prepareModuleMocks(moduleName, parent = null, visited = new Set()) {
    const request = normalizeModuleRequest(moduleName, parent)
    const direct = state.moduleMocks.get(request.specifier)
    if (direct) {
      await evaluateModuleMock(direct)
      return
    }
    const id = resolveModuleId(request, parent)
    if (!state.moduleDefinitions.has(id) || visited.has(id)) return
    visited.add(id)
    const definition = state.moduleDefinitions.get(id)
    for (const [edgeKey, target] of Object.entries(definition.resolutions)) {
      const {specifier} = normalizeModuleRequest(edgeKey, id)
      const entry = state.moduleMocks.get(specifier)
      if (entry) await evaluateModuleMock(entry)
      else await prepareModuleMocks(target, null, visited)
    }
  }

  async function importWakeModule(moduleName, useMock, actual) {
    moduleName = String(moduleName)
    if (useMock && !actual) await prepareModuleMocks(moduleName)
    wakeRequire(moduleName, useMock, actual)
    await Promise.all([...state.modulePromises])
    return wakeRequire(moduleName, useMock, actual)
  }

  function wakeRequire(moduleName, useMock = true, actual = false) {
    return wakeLoadFrom(moduleName, null, useMock, actual)
  }

  function wakeLoadFrom(moduleName, parent, useMock = true, actual = false) {
    const request = normalizeModuleRequest(moduleName, parent)
    const {specifier} = request
    if (specifier === '@crab-dev/wake/test/react') return reactApi
    if (specifier === '@crab-dev/wake/test') return api
    if (useMock && !actual && state.moduleMocks.has(specifier)) {
      const entry = state.moduleMocks.get(specifier)
      if (!entry.evaluated) {
        const value = entry.factory()
        if (value && typeof value.then === 'function') {
          entry.evaluating = Promise.resolve(value).then(resolved => {
            entry.value = resolved
            entry.evaluated = true
            return resolved
          })
          throw Object.assign(new Error(`Async mock factory for ${specifier} must be loaded through await mock.import()`), { code: 'WAKE_TEST_RUNTIME' })
        }
        entry.value = value
        entry.evaluated = true
      }
      return entry.value
    }
    if (state.builtins.has(specifier)) return state.builtins.get(specifier)
    if (!specifier.startsWith('node:') && state.builtins.has(`node:${specifier}`)) return state.builtins.get(`node:${specifier}`)
    const id = resolveModuleId(request, parent)
    if (state.moduleDefinitions.has(id)) {
      if (state.moduleCache.has(id)) return state.moduleCache.get(id).exports
      const definition = state.moduleDefinitions.get(id)
      const module = { exports: {} }
      state.moduleCache.set(id, module)
      try {
        const ready = definition.factory(
          module,
          module.exports,
          name => wakeLoadFrom(name, id),
          id,
          id.includes('/') ? id.slice(0, id.lastIndexOf('/')) : '.',
        )
        if (ready && typeof ready.then === 'function') {
          module.ready = Promise.resolve(ready).catch(error => {
            state.moduleCache.delete(id)
            throw error
          })
          state.modulePromises.add(module.ready)
        }
      } catch (error) {
        state.moduleCache.delete(id)
        throw error
      }
      return module.exports
    }
    throw Object.assign(new Error(`Wake runtime cannot resolve module ${specifier}`), { code: 'WAKE_TEST_UNSUPPORTED' })
  }

  function defineModule(id, factory, resolutions, requestSpecifiers) {
    state.moduleDefinitions.set(String(id), {
      factory,
      resolutions: resolutions || {},
      requestSpecifiers: requestSpecifiers || {},
    })
  }

  function host(op, values = {}) {
    return JSON.parse(globalThis.__wakeHostCall(JSON.stringify({ op, ...values })))
  }

  function installBuiltins() {
    const utf8Bytes = value => {
      const bytes = []
      for (const character of String(value)) {
        const point = character.codePointAt(0)
        if (point < 0x80) bytes.push(point)
        else if (point < 0x800) bytes.push(0xc0 | point >> 6, 0x80 | point & 0x3f)
        else if (point < 0x10000) bytes.push(0xe0 | point >> 12, 0x80 | point >> 6 & 0x3f, 0x80 | point & 0x3f)
        else bytes.push(0xf0 | point >> 18, 0x80 | point >> 12 & 0x3f, 0x80 | point >> 6 & 0x3f, 0x80 | point & 0x3f)
      }
      return bytes
    }
    const utf8String = bytes => {
      let result = ''
      for (let index = 0; index < bytes.length;) {
        const first = bytes[index++]
        let point
        if (first < 0x80) point = first
        else if (first < 0xe0) point = (first & 0x1f) << 6 | bytes[index++] & 0x3f
        else if (first < 0xf0) point = (first & 0x0f) << 12 | (bytes[index++] & 0x3f) << 6 | bytes[index++] & 0x3f
        else point = (first & 7) << 18 | (bytes[index++] & 0x3f) << 12 | (bytes[index++] & 0x3f) << 6 | bytes[index++] & 0x3f
        result += String.fromCodePoint(point)
      }
      return result
    }
    const base64 = bytes => {
      const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
      let result = ''
      for (let index = 0; index < bytes.length; index += 3) {
        const first = bytes[index], second = bytes[index + 1], third = bytes[index + 2]
        const value = first << 16 | (second || 0) << 8 | third || 0
        result += alphabet[value >> 18 & 63] + alphabet[value >> 12 & 63]
        result += second === undefined ? '=' : alphabet[value >> 6 & 63]
        result += third === undefined ? '=' : alphabet[value & 63]
      }
      return result
    }
    class WakeBuffer extends Uint8Array {
      static from(value, encoding = 'utf8') {
        if (value instanceof WakeBuffer) return new WakeBuffer(value)
        if (typeof value === 'string') {
          if (encoding === 'hex') {
            const bytes = []
            for (let index = 0; index + 1 < value.length; index += 2) bytes.push(Number.parseInt(value.slice(index, index + 2), 16))
            return new WakeBuffer(bytes)
          }
          return new WakeBuffer(utf8Bytes(value))
        }
        if (value instanceof ArrayBuffer) return new WakeBuffer(new Uint8Array(value))
        return new WakeBuffer(value && typeof value[Symbol.iterator] === 'function' ? [...value] : [])
      }
      static isBuffer(value) { return value instanceof WakeBuffer }
      static isView(value) { return ArrayBuffer.isView(value) }
      static alloc(size, fill = 0) { const value = new WakeBuffer(Number(size)); value.fill(fill); return value }
      static byteLength(value) { return typeof value === 'string' ? utf8Bytes(value).length : value && value.byteLength || 0 }
      static concat(values, length) {
        const size = length === undefined ? values.reduce((total, value) => total + value.length, 0) : Number(length)
        const result = WakeBuffer.alloc(size)
        let offset = 0
        for (const value of values) { const bytes = WakeBuffer.from(value); result.set(bytes.subarray(0, Math.max(0, size - offset)), offset); offset += bytes.length; if (offset >= size) break }
        return result
      }
      static compare(left, right) {
        left = WakeBuffer.from(left); right = WakeBuffer.from(right)
        const length = Math.min(left.length, right.length)
        for (let index = 0; index < length; index++) if (left[index] !== right[index]) return left[index] < right[index] ? -1 : 1
        return Math.sign(left.length - right.length)
      }
      toString(encoding = 'utf8') {
        if (encoding === 'base64') return base64(this)
        if (encoding === 'binary' || encoding === 'latin1') return [...this].map(value => String.fromCharCode(value)).join('')
        if (encoding === 'hex') return [...this].map(value => value.toString(16).padStart(2, '0')).join('')
        return utf8String(this)
      }
    }
    globalThis.Buffer = WakeBuffer
    if (typeof globalThis.TextEncoder === 'undefined') {
      globalThis.TextEncoder = class TextEncoder { encode(value = '') { return new Uint8Array(utf8Bytes(value)) } }
    }
    if (typeof globalThis.TextDecoder === 'undefined') {
      globalThis.TextDecoder = class TextDecoder { decode(value = new Uint8Array()) { return utf8String(new Uint8Array(value.buffer || value, value.byteOffset || 0, value.byteLength)) } }
    }
    let timerId = 0
    const cancelledTimers = new Set()
    const scheduleTask = (callback, args, delay = 0, repeat = false) => {
      const id = ++timerId
      const run = () => {
        if (cancelledTimers.has(id)) return
        const ready = typeof globalThis.__wakeVmSleep === 'function'
          ? globalThis.__wakeVmSleep(id, delay)
          : Promise.resolve()
        Promise.resolve(ready).then(() => {
          if (cancelledTimers.has(id)) return
          callback(...args)
          if (repeat && !cancelledTimers.has(id)) run()
        })
      }
      run()
      return id
    }
    const cancelTask = id => {
      cancelledTimers.add(id)
      if (typeof globalThis.__wakeVmCancelSleep === 'function') globalThis.__wakeVmCancelSleep(id)
    }
    if (typeof globalThis.setTimeout === 'undefined') globalThis.setTimeout = (callback, delay, ...args) => scheduleTask(callback, args, Number(delay || 0))
    if (typeof globalThis.clearTimeout === 'undefined') globalThis.clearTimeout = cancelTask
    if (typeof globalThis.setInterval === 'undefined') globalThis.setInterval = (callback, delay, ...args) => scheduleTask(callback, args, Number(delay || 0), true)
    if (typeof globalThis.clearInterval === 'undefined') globalThis.clearInterval = cancelTask
    if (typeof globalThis.setImmediate === 'undefined') globalThis.setImmediate = (callback, ...args) => scheduleTask(callback, args)
    if (typeof globalThis.clearImmediate === 'undefined') globalThis.clearImmediate = cancelTask
    if (typeof globalThis.queueMicrotask === 'undefined') globalThis.queueMicrotask = callback => { Promise.resolve().then(callback) }
    if (typeof globalThis.MessageChannel === 'undefined') {
      class WakeMessagePort {
        constructor() { this._peer = null; this._listeners = new Set(); this.onmessage = null; this._closed = false }
        postMessage(data) {
          const peer = this._peer
          Promise.resolve().then(() => {
            if (!peer || peer._closed) return
            const event = { data, target: peer, currentTarget: peer }
            if (typeof peer.onmessage === 'function') peer.onmessage(event)
            for (const listener of peer._listeners) listener.call(peer, event)
          })
        }
        addEventListener(type, listener) { if (type === 'message') this._listeners.add(listener) }
        removeEventListener(type, listener) { if (type === 'message') this._listeners.delete(listener) }
        start() {}
        close() { this._closed = true; this._listeners.clear(); this.onmessage = null }
      }
      globalThis.MessageChannel = class MessageChannel {
        constructor() { this.port1 = new WakeMessagePort(); this.port2 = new WakeMessagePort(); this.port1._peer = this.port2; this.port2._peer = this.port1 }
      }
      globalThis.MessagePort = WakeMessagePort
    }
    if (typeof globalThis.performance === 'undefined') globalThis.performance = { now: () => Date.now() }
    if (typeof globalThis.URL === 'undefined') {
      globalThis.URL = class URL {
        constructor(input, base) {
          let value = String(input)
          if (!/^[A-Za-z][A-Za-z\d+.-]*:/.test(value)) {
            if (base === undefined) throw new TypeError('Invalid URL')
            const parent = base instanceof globalThis.URL ? base : new globalThis.URL(String(base))
            if (value.startsWith('//')) value = `${parent.protocol}${value}`
            else {
              const suffixIndex = value.search(/[?#]/)
              const suffix = suffixIndex >= 0 ? value.slice(suffixIndex) : ''
              const reference = suffixIndex >= 0 ? value.slice(0, suffixIndex) : value
              let pathname
              if (!reference) pathname = parent.pathname
              else if (reference.startsWith('/')) pathname = reference
              else pathname = `${parent.pathname.slice(0, parent.pathname.lastIndexOf('/') + 1)}${reference}`
              const normalized = []
              for (const part of pathname.split('/')) {
                if (part === '..') normalized.pop()
                else if (part && part !== '.') normalized.push(part)
              }
              const trailingSlash = pathname.endsWith('/') || reference === '..' || reference.endsWith('/..')
              pathname = `/${normalized.join('/')}${trailingSlash ? '/' : ''}`
              value = `${parent.protocol}//${parent.host}${pathname}${suffix}`
            }
          }
          const match = value.match(/^([A-Za-z][A-Za-z\d+.-]*:)(?:\/\/([^/]*))?([^?#]*)(\?[^#]*)?(#.*)?$/)
          if (!match) throw new TypeError('Invalid URL')
          this.protocol = match[1]
          const authority = match[2] || ''
          const at = authority.lastIndexOf('@')
          const credentials = at >= 0 ? authority.slice(0, at) : ''
          this.host = at >= 0 ? authority.slice(at + 1) : authority
          const colon = this.host.startsWith('[') ? this.host.indexOf(']') + 1 : this.host.lastIndexOf(':')
          this.hostname = colon > 0 && this.host[colon] === ':' ? this.host.slice(0, colon) : this.host
          this.port = colon > 0 && this.host[colon] === ':' ? this.host.slice(colon + 1) : ''
          const separator = credentials.indexOf(':')
          this.username = separator >= 0 ? credentials.slice(0, separator) : credentials
          this.password = separator >= 0 ? credentials.slice(separator + 1) : ''
          this.pathname = match[3] || '/'
          this.search = match[4] || ''
          this.hash = match[5] || ''
          this.origin = this.protocol === 'http:' || this.protocol === 'https:' ? `${this.protocol}//${this.host}` : 'null'
          const serializedCredentials = credentials ? `${credentials}@` : ''
          const slashes = match[2] !== undefined ? '//' : ''
          this.href = `${this.protocol}${slashes}${serializedCredentials}${this.host}${this.pathname}${this.search}${this.hash}`
          this.searchParams = new globalThis.URLSearchParams(this.search)
        }
        static canParse(input, base) { try { new this(input, base); return true } catch { return false } }
        static parse(input, base) { try { return new this(input, base) } catch { return null } }
        toString() { return this.href }
        toJSON() { return this.href }
      }
    }
    if (typeof globalThis.URLSearchParams === 'undefined') {
      globalThis.URLSearchParams = class URLSearchParams {
        constructor(input = '') {
          this._values = []
          if (typeof input === 'string') {
            for (const part of input.replace(/^\?/, '').split('&')) {
              if (!part) continue
              const [key, value = ''] = part.split('=', 2)
              this._values.push([decodeURIComponent(key.replaceAll('+', ' ')), decodeURIComponent(value.replaceAll('+', ' '))])
            }
          } else if (input && typeof input[Symbol.iterator] === 'function') this._values.push(...input)
          else if (input) for (const key of Object.keys(input)) this._values.push([key, String(input[key])])
        }
        append(key, value) { this._values.push([String(key), String(value)]) }
        set(key, value) { this.delete(key); this.append(key, value) }
        get(key) { const item = this._values.find(value => value[0] === String(key)); return item ? item[1] : null }
        getAll(key) { return this._values.filter(value => value[0] === String(key)).map(value => value[1]) }
        has(key) { return this._values.some(value => value[0] === String(key)) }
        delete(key) { this._values = this._values.filter(value => value[0] !== String(key)) }
        entries() { return this._values[Symbol.iterator]() }
        keys() { return this._values.map(value => value[0])[Symbol.iterator]() }
        values() { return this._values.map(value => value[1])[Symbol.iterator]() }
        forEach(callback, thisArg) { for (const [key, value] of this._values) callback.call(thisArg, value, key, this) }
        [Symbol.iterator]() { return this.entries() }
        toString() { return this._values.map(([key, value]) => `${encodeURIComponent(key)}=${encodeURIComponent(value)}`).join('&') }
      }
    }
    class WakeReadableStream {
      constructor(source = {}) {
        this._queue = []
        this._pending = []
        this._closed = false
        this._error = null
        this._source = source
        const controller = {
          enqueue: value => {
            const pending = this._pending.shift()
            if (pending) pending.resolve({ value, done: false })
            else this._queue.push(value)
          },
          close: () => {
            this._closed = true
            for (const pending of this._pending.splice(0)) pending.resolve({ value: undefined, done: true })
          },
          error: error => {
            this._error = error
            for (const pending of this._pending.splice(0)) pending.reject(error)
          },
          get desiredSize() { return 1 },
        }
        this._controller = controller
        if (typeof source.start === 'function') Promise.resolve(source.start(controller)).catch(controller.error)
      }
      get locked() { return false }
      getReader() {
        const stream = this
        return {
          async read() {
            if (stream._error) throw stream._error
            if (stream._queue.length) return { value: stream._queue.shift(), done: false }
            if (stream._closed) return { value: undefined, done: true }
            if (typeof stream._source.pull === 'function') await stream._source.pull(stream._controller)
            if (stream._queue.length) return { value: stream._queue.shift(), done: false }
            if (stream._closed) return { value: undefined, done: true }
            return await new Promise((resolve, reject) => stream._pending.push({ resolve, reject }))
          },
          async cancel(reason) { return stream.cancel(reason) },
          releaseLock() {},
        }
      }
      async cancel(reason) { this._closed = true; if (typeof this._source.cancel === 'function') await this._source.cancel(reason) }
      async *[Symbol.asyncIterator]() { const reader = this.getReader(); while (true) { const item = await reader.read(); if (item.done) return; yield item.value } }
      tee() {
        const values = this._queue.slice()
        return [WakeReadableStream.from(values), WakeReadableStream.from(values)]
      }
      static from(iterable) {
        return new WakeReadableStream({ async start(controller) { for await (const value of iterable) controller.enqueue(value); controller.close() } })
      }
    }
    class WakeWritableStream {
      constructor(sink = {}) { this._sink = sink }
      getWriter() {
        const sink = this._sink
        return {
          ready: Promise.resolve(), closed: Promise.resolve(),
          write: value => Promise.resolve(typeof sink.write === 'function' ? sink.write(value) : undefined),
          close: () => Promise.resolve(typeof sink.close === 'function' ? sink.close() : undefined),
          abort: reason => Promise.resolve(typeof sink.abort === 'function' ? sink.abort(reason) : undefined),
          releaseLock() {},
        }
      }
    }
    class WakeTransformStream {
      constructor(transformer = {}) {
        let controller
        this.readable = new WakeReadableStream({ start(value) { controller = value } })
        this.writable = new WakeWritableStream({
          write: value => transformer.transform ? transformer.transform(value, controller) : controller.enqueue(value),
          close: () => { if (transformer.flush) transformer.flush(controller); controller.close() },
        })
      }
    }
    globalThis.ReadableStream ||= WakeReadableStream
    globalThis.WritableStream ||= WakeWritableStream
    globalThis.TransformStream ||= WakeTransformStream

    let randomState = (realDateNow() ^ 0x9e3779b9) >>> 0
    const randomByte = () => { randomState ^= randomState << 13; randomState ^= randomState >>> 17; randomState ^= randomState << 5; return randomState & 255 }
    const randomUUID = () => {
      const bytes = Array.from({ length: 16 }, randomByte)
      bytes[6] = bytes[6] & 15 | 64
      bytes[8] = bytes[8] & 63 | 128
      const hex = bytes.map(value => value.toString(16).padStart(2, '0')).join('')
      return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`
    }
    const webcrypto = {
      getRandomValues(value) { for (let index = 0; index < value.length; index++) value[index] = randomByte(); return value },
      randomUUID,
      subtle: {},
    }
    globalThis.crypto ||= webcrypto
    const slash = value => String(value).replaceAll('\\', '/')
    const pathApi = {
      sep: '/', delimiter: ';',
      normalize(value) {
        const input = slash(value)
        const prefix = /^[A-Za-z]:\//.test(input) ? input.slice(0, 3) : input.startsWith('/') ? '/' : ''
        const values = []
        for (const part of input.slice(prefix.length).split('/')) {
          if (!part || part === '.') continue
          if (part === '..' && values.length && values.at(-1) !== '..') values.pop()
          else if (part === '..' && !prefix) values.push(part)
          else if (part !== '..') values.push(part)
        }
        return prefix + values.join('/') || '.'
      },
      isAbsolute(value) { return slash(value).startsWith('/') || /^[A-Za-z]:\//.test(slash(value)) },
      join(...values) { return pathApi.normalize(values.filter(Boolean).join('/')) },
      resolve(...values) {
        let result = ''
        for (const value of [host('cwd'), ...values]) {
          if (pathApi.isAbsolute(value)) result = slash(value)
          else result += `/${value}`
        }
        return pathApi.normalize(result)
      },
      dirname(value) {
        value = pathApi.normalize(value)
        const index = value.lastIndexOf('/')
        return index <= 0 ? (value.startsWith('/') ? '/' : '.') : value.slice(0, index)
      },
      basename(value, suffix) {
        let result = slash(value).slice(slash(value).lastIndexOf('/') + 1)
        if (suffix && result.endsWith(suffix)) result = result.slice(0, -String(suffix).length)
        return result
      },
      extname(value) {
        const base = pathApi.basename(value)
        const index = base.lastIndexOf('.')
        return index <= 0 ? '' : base.slice(index)
      },
      relative(from, to) {
        const left = pathApi.resolve(from).split('/')
        const right = pathApi.resolve(to).split('/')
        while (left.length && right.length && left[0].toLowerCase() === right[0].toLowerCase()) { left.shift(); right.shift() }
        return [...left.map(() => '..'), ...right].join('/') || ''
      },
      parse(value) {
        const dir = pathApi.dirname(value), base = pathApi.basename(value), ext = pathApi.extname(value)
        return { root: pathApi.isAbsolute(value) ? slash(value).match(/^(?:[A-Za-z]:\/|\/)/)[0] : '', dir, base, ext, name: ext ? base.slice(0, -ext.length) : base }
      },
      format(value) { return pathApi.join(value.dir || value.root || '', value.base || `${value.name || ''}${value.ext || ''}`) },
      posix: null, win32: null,
    }
    pathApi.posix = pathApi
    pathApi.win32 = { ...pathApi, sep: '\\', delimiter: ';' }

    class AssertionError extends Error { constructor(message) { super(message); this.name = 'AssertionError'; this.code = 'ERR_ASSERTION' } }
    const fail = message => { throw new AssertionError(message || 'Assertion failed') }
    const assert = value => { if (!value) fail(`Expected ${pretty(value)} to be truthy`) }
    assert.ok = assert
    assert.equal = (actual, expected, message) => { if (actual != expected) fail(message || `${pretty(actual)} == ${pretty(expected)}`) }
    assert.notEqual = (actual, expected, message) => { if (actual == expected) fail(message || `${pretty(actual)} != ${pretty(expected)}`) }
    assert.strictEqual = (actual, expected, message) => { if (!Object.is(actual, expected)) fail(message || `${pretty(actual)} === ${pretty(expected)}`) }
    assert.notStrictEqual = (actual, expected, message) => { if (Object.is(actual, expected)) fail(message || `${pretty(actual)} !== ${pretty(expected)}`) }
    assert.deepEqual = assert.deepStrictEqual = (actual, expected, message) => { if (!equals(actual, expected, true)) fail(message || `Expected ${pretty(actual)} to deeply equal ${pretty(expected)}`) }
    assert.notDeepEqual = assert.notDeepStrictEqual = (actual, expected, message) => { if (equals(actual, expected, true)) fail(message || 'Values were deeply equal') }
    assert.match = (actual, expected, message) => { if (!expected.test(String(actual))) fail(message || `${actual} did not match ${expected}`) }
    assert.doesNotMatch = (actual, expected, message) => { if (expected.test(String(actual))) fail(message || `${actual} matched ${expected}`) }
    assert.throws = (fn, expected, message) => {
      let thrown
      try { fn() } catch (error) { thrown = error }
      if (thrown === undefined) fail(message || 'Expected function to throw')
      if (expected instanceof RegExp && !expected.test(String(thrown && thrown.message || thrown))) fail(message || 'Thrown error did not match')
      else if (typeof expected === 'function') {
        const errorConstructor = expected === Error || Boolean(expected.prototype && expected.prototype instanceof Error)
        if (errorConstructor ? !(thrown instanceof expected) : expected(thrown) !== true) fail(message || 'Thrown error did not satisfy expectation')
      } else if (expected && typeof expected === 'object') {
        for (const key of Object.keys(expected)) if (!equals(thrown[key], expected[key], true)) fail(message || `Thrown error property ${key} did not match`)
      }
      return thrown
    }
    assert.doesNotThrow = (fn, message) => { try { fn() } catch (error) { fail(message || `Unexpected throw: ${error}`) } }
    assert.rejects = async (promise, expected, message) => { try { await (typeof promise === 'function' ? promise() : promise); fail(message || 'Expected rejection') } catch (error) { if (error instanceof AssertionError && error.message === 'Expected rejection') throw error; if (expected instanceof RegExp && !expected.test(String(error && error.message || error))) fail(message || 'Rejection did not match') } }
    assert.doesNotReject = async (promise, message) => { try { await (typeof promise === 'function' ? promise() : promise) } catch (error) { fail(message || `Unexpected rejection: ${error}`) } }
    assert.fail = fail
    assert.AssertionError = AssertionError

    class EventEmitter {
      constructor() { this._events = new Map() }
      on(name, listener) { const values = this._events.get(name) || []; values.push(listener); this._events.set(name, values); return this }
      addListener(name, listener) { return this.on(name, listener) }
      once(name, listener) { const wrapper = (...args) => { this.off(name, wrapper); listener(...args) }; return this.on(name, wrapper) }
      off(name, listener) { const values = this._events.get(name) || []; this._events.set(name, values.filter(value => value !== listener)); return this }
      removeListener(name, listener) { return this.off(name, listener) }
      removeAllListeners(name) { if (name === undefined) this._events.clear(); else this._events.delete(name); return this }
      emit(name, ...args) { const values = [...(this._events.get(name) || [])]; for (const listener of values) listener.apply(this, args); return values.length > 0 }
      listeners(name) { return [...(this._events.get(name) || [])] }
      listenerCount(name) { return (this._events.get(name) || []).length }
    }
    EventEmitter.EventEmitter = EventEmitter
    class PassThrough extends EventEmitter {
      write(value) { this.emit('data', value); return true }
      end(value) { if (value !== undefined) this.write(value); this.emit('end'); this.emit('finish') }
      pipe(destination) { this.on('data', value => destination.write(value)); this.on('end', () => destination.end()); return destination }
    }
    class Writable extends PassThrough {}
    class Transform extends PassThrough {}
    const streamApi = {
      PassThrough, Writable, Transform,
      Readable: class Readable extends PassThrough { static from(values) { const stream = new this(); Promise.resolve().then(() => { for (const value of values) stream.write(value); stream.end() }); return stream } },
      pipeline(source, destination, callback = () => {}) {
        try { source.pipe(destination); Promise.resolve().then(() => callback()); return destination }
        catch (error) { Promise.resolve().then(() => callback(error)); return destination }
      },
    }

    const fsPath = value => {
      if (value instanceof URL && value.protocol === 'file:') {
        let pathname = decodeURIComponent(value.pathname)
        if (/^\/[A-Za-z]:\//.test(pathname)) pathname = pathname.slice(1)
        return pathname
      }
      return String(value)
    }
    const stat = value => { const result = host('stat', { path: fsPath(value) }); return { ...result, isFile: () => result.isFile, isDirectory: () => result.isDirectory } }
    const fsApi = {
      readFileSync(path, encoding) { const value = host('readTextFile', { path: fsPath(path) }); return encoding ? value : value },
      writeFileSync(path, content) { return host('writeTextFile', { path: fsPath(path), content: String(content) }) },
      existsSync(path) { return host('exists', { path: fsPath(path) }) },
      accessSync(path) { return host('access', { path: fsPath(path) }) },
      mkdirSync(path, options = {}) { return host('mkdir', { path: fsPath(path), recursive: Boolean(options && options.recursive) }) },
      rmSync(path, options = {}) { return host('remove', { path: fsPath(path), recursive: Boolean(options && options.recursive), force: Boolean(options && options.force) }) },
      readdirSync(path) { return host('readdir', { path: fsPath(path) }) },
      statSync: stat,
      copyFileSync(from, to) { return host('copyFile', { from: String(from), to: String(to) }) },
      renameSync(from, to) { return host('rename', { from: String(from), to: String(to) }) },
    }
    const fsPromises = {
      readFile: async (path, encoding) => fsApi.readFileSync(path, encoding),
      writeFile: async (path, content) => fsApi.writeFileSync(path, content),
      access: async path => fsApi.accessSync(path),
      mkdir: async (path, options) => fsApi.mkdirSync(path, options),
      mkdtemp: async prefix => host('mkdtemp', { path: String(prefix) }),
      rm: async (path, options) => fsApi.rmSync(path, options),
      readdir: async path => fsApi.readdirSync(path),
      stat: async path => stat(path),
      copyFile: async (from, to) => fsApi.copyFileSync(from, to),
      rename: async (from, to) => fsApi.renameSync(from, to),
    }
    fsApi.promises = fsPromises

    const osApi = { tmpdir: () => host('tmpdir'), platform: () => host('platform'), homedir: () => host('env').HOME || host('env').USERPROFILE || '', EOL: host('platform') === 'win32' ? '\r\n' : '\n' }
    const urlApi = {
      URL: globalThis.URL,
      URLSearchParams: globalThis.URLSearchParams,
      pathToFileURL(path) { const pathname = slash(pathApi.resolve(path)); return new URL(`file://${pathname.startsWith('/') ? '' : '/'}${pathname.replaceAll(' ', '%20')}`) },
      fileURLToPath(value) { const url = value instanceof URL ? value : new URL(value); if (url.protocol !== 'file:') throw new TypeError('URL must use file:'); let pathname = decodeURIComponent(url.pathname); if (/^\/[A-Za-z]:\//.test(pathname)) pathname = pathname.slice(1); return pathname },
    }
    const moduleApi = {
      createRequire(value) {
        const parent = urlApi.fileURLToPath(value)
        return name => wakeLoadFrom(String(name), parent)
      },
    }
    class DisabledScript {
      constructor(code) { this.code = String(code) }
      runInContext() { throw Object.assign(new Error('Happy DOM script evaluation is disabled by Wake'), { code: 'WAKE_TEST_UNSUPPORTED' }) }
      runInNewContext() { return this.runInContext() }
      runInThisContext() { return this.runInContext() }
    }
    const vmApi = {
      Script: DisabledScript,
      createContext(value) { return value },
      isContext() { return true },
      runInNewContext() { throw Object.assign(new Error('node:vm is disabled in Wake tests'), { code: 'WAKE_TEST_UNSUPPORTED' }) },
      runInContext() { throw Object.assign(new Error('node:vm is disabled in Wake tests'), { code: 'WAKE_TEST_UNSUPPORTED' }) },
      runInThisContext() { throw Object.assign(new Error('node:vm is disabled in Wake tests'), { code: 'WAKE_TEST_UNSUPPORTED' }) },
    }
    const utilApi = {
      TextEncoder: globalThis.TextEncoder,
      TextDecoder: globalThis.TextDecoder,
      promisify: fn => (...args) => new Promise((resolve, reject) => fn(...args, (error, value) => error ? reject(error) : resolve(value))),
      types: {},
    }
    const cryptoApi = {
      webcrypto,
      randomUUID,
      randomBytes(size) { return WakeBuffer.from(Array.from({ length: Number(size) }, randomByte)) },
      createHash() {
        let hash = 0x811c9dc5
        return {
          update(value) { for (const byte of WakeBuffer.from(value)) { hash ^= byte; hash = Math.imul(hash, 0x01000193) >>> 0 } return this },
          digest(encoding) { const bytes = WakeBuffer.from(hash.toString(16).padStart(8, '0'), 'hex'); return encoding ? bytes.toString(encoding) : bytes },
        }
      },
    }
    class PerformanceEntry { constructor(name = '', entryType = '', startTime = 0, duration = 0) { Object.assign(this, { name, entryType, startTime, duration }) } }
    class PerformanceObserver { constructor(callback) { this.callback = callback } observe() {} disconnect() {} takeRecords() { return [] } }
    const performanceApi = { performance: globalThis.performance, PerformanceEntry, PerformanceObserver }
    const deniedTransport = new Proxy({}, { get(_target, key) { if (key === 'default') return deniedTransport; return () => { throw Object.assign(new Error('Direct transport access is disabled by Wake'), { code: 'WAKE_TEST_NETWORK' }) } } })
    const zlibApi = new Proxy({}, { get() { return () => { throw Object.assign(new Error('Compressed Node transport is disabled by Wake'), { code: 'WAKE_TEST_NETWORK' }) } } })
    const netApi = { isIP(value) { const text = String(value); return /^\d{1,3}(?:\.\d{1,3}){3}$/.test(text) ? 4 : text.includes(':') ? 6 : 0 } }
    class StringDecoder {
      write(value) { return WakeBuffer.from(value).toString('utf8') }
      end(value) { return value === undefined ? '' : this.write(value) }
    }
    const childProcessApi = {
      spawnSync(command, args = [], options = {}) {
        try {
          const result = host('spawnSync', { command: String(command), args: args.map(String), cwd: options.cwd, env: options.env })
          return { ...result, pid: 0, output: [null, result.stdout, result.stderr], error: undefined }
        } catch (error) {
          return { status: null, signal: null, stdout: '', stderr: '', pid: 0, output: [null, '', ''], error }
        }
      },
      execFileSync(command, args = [], options = {}) {
        const result = childProcessApi.spawnSync(command, args, options)
        if (result.error) throw result.error
        if (result.status !== 0) {
          const error = new Error(`Command failed: ${command}\n${result.stderr}`)
          Object.assign(error, result)
          throw error
        }
        return result.stdout
      },
    }
    state.builtins.set('node:assert', assert)
    state.builtins.set('node:assert/strict', assert)
    state.builtins.set('node:path', pathApi)
    state.builtins.set('node:path/posix', pathApi.posix)
    state.builtins.set('node:path/win32', pathApi.win32)
    state.builtins.set('node:fs', fsApi)
    state.builtins.set('node:fs/promises', fsPromises)
    state.builtins.set('node:os', osApi)
    state.builtins.set('node:url', urlApi)
    state.builtins.set('node:events', EventEmitter)
    state.builtins.set('node:util', utilApi)
    state.builtins.set('node:child_process', childProcessApi)
    state.builtins.set('node:string_decoder', { StringDecoder })
    state.builtins.set('node:buffer', { Buffer: WakeBuffer, default: WakeBuffer })
    state.builtins.set('node:crypto', cryptoApi)
    state.builtins.set('node:stream', streamApi)
    state.builtins.set('node:stream/web', { ReadableStream: globalThis.ReadableStream, WritableStream: globalThis.WritableStream, TransformStream: globalThis.TransformStream })
    state.builtins.set('node:perf_hooks', performanceApi)
    state.builtins.set('node:http', deniedTransport)
    state.builtins.set('node:https', deniedTransport)
    state.builtins.set('node:net', netApi)
    state.builtins.set('node:zlib', zlibApi)
    state.builtins.set('node:module', moduleApi)
    state.builtins.set('node:vm', vmApi)
    const execPath = host('execPath')
    const platform = host('platform')
    globalThis.process = { argv: [execPath], env: host('env'), cwd: () => host('cwd'), execPath, exitCode: 0, platform, arch: 'x64', version: 'wake-v8', versions: { wake: '0.1', v8: 'embedded' }, nextTick: callback => Promise.resolve().then(callback) }
  }

  function ancestors(target) {
    const values = []
    for (let current = target; current; current = current.parent) values.unshift(current)
    return values
  }

  function pathFocused(target) {
    for (let current = target; current; current = current.parent) if (current.mode === 'only') return true
    return false
  }

  async function invoke(fn, timeout = state.defaultTimeout) {
    if (typeof fn !== 'function') return
    if (fn.length > 0) {
      throw Object.assign(new Error('Wake tests do not support done callbacks; return or await a Promise'), { code: 'WAKE_TEST_UNSUPPORTED' })
    }
    const value = fn()
    if (!value || typeof value.then !== 'function') return value
    let timer
    try {
      return await Promise.race([
        value,
        new Promise((_resolve, reject) => {
          timer = realSetTimeout(
            () => reject(Object.assign(new Error(`Test callback exceeded ${timeout} ms`), { code: 'WAKE_TEST_TIMEOUT' })),
            timeout,
          )
        }),
      ])
    } finally {
      if (timer !== undefined) realClearTimeout(timer)
    }
  }

  function failureDetails(error) {
    const rawMessage = String(error && error.message || error)
    const loopTimeout = rawMessage.includes('Maximum loop iteration limit')
    return {
      message: loopTimeout ? `Test execution timed out: ${rawMessage}` : rawMessage,
      code: loopTimeout ? 'WAKE_TEST_TIMEOUT' : error && error.code ? String(error.code) : null,
      stack: error && error.stack ? String(error.stack) : null,
      diff: error && error.__wakeDiff ? error.__wakeDiff : null,
    }
  }

  function schedulerCaseResult(plan, status, failures = [], assertions = 0, durationMs = 0) {
    return {
      name: plan.name,
      fullName: plan.fullName,
      status,
      durationMs,
      failures,
      assertions,
      registrationStack: plan.registrationStack,
    }
  }

  function schedulerRuntimeResult(includeActive = true) {
    const scheduler = state.scheduler
    const cases = scheduler.results.slice()
    if (includeActive && scheduler.activeCase) {
      const active = scheduler.activeCase
      cases.push(schedulerCaseResult(
        active.plan,
        active.failures.length ? 'failed' : 'passed',
        active.failures.slice(),
        active.expectation.assertionCalls,
        Math.max(0, realDateNow() - active.started),
      ))
    }
    const failed = cases.some(value => value.status === 'failed') || scheduler.suiteFailures.length > 0
    return {
      schemaVersion: runtimeResultSchema,
      status: failed ? 'failed' : 'passed',
      cases,
      failures: scheduler.suiteFailures.slice(),
      snapshots: state.snapshots.slice(),
      leaks: state.leaks.slice(),
      diagnostics: state.diagnostics.slice(),
    }
  }

  function schedulerAddStep(scheduler, values) {
    const step = { id: `step-${++scheduler.stepId}`, ...values }
    scheduler.steps.push(step)
    scheduler.stepsById.set(step.id, step)
    return step
  }

  function schedulerCaseStatus(record, owner, staticallySkipped) {
    if (record.mode === 'todo') return 'todo'
    const fullName = [...ancestors(owner).map(value => value.name).filter(Boolean), record.name]
      .filter(Boolean)
      .join(' ')
    const nameFiltered = state.namePattern && !state.namePattern.test(fullName)
    if (state.namePattern) state.namePattern.lastIndex = 0
    if (
      staticallySkipped
      || nameFiltered
      || record.mode === 'skip'
      || (state.focused && record.mode !== 'only' && !pathFocused(owner))
    ) return 'skipped'
    return 'run'
  }

  function buildScheduler() {
    const scheduler = {
      stepId: 0,
      suiteId: 0,
      steps: [],
      stepsById: new Map(),
      suiteIds: new Map(),
      cases: [],
      results: [],
      suiteFailures: state.pendingActFailures.splice(0),
      enteredSuites: new Set(),
      blockedSuites: new Set(),
      beforeAllStopped: new Set(),
      afterAllStopped: new Set(),
      activeCase: null,
      currentStep: null,
      index: 0,
    }

    const appendSuite = (target, parentSuitePath, inheritedSkip) => {
      const suiteId = `suite-${++scheduler.suiteId}`
      scheduler.suiteIds.set(target, suiteId)
      const suitePath = [...parentSuitePath, suiteId]
      const staticallySkipped = inheritedSkip || target.mode === 'skip'
      scheduler.steps.push({ kind: 'suiteStart', suiteId, suitePath, staticallySkipped })
      if (!staticallySkipped) {
        for (const item of target.hooks.beforeAll) {
          schedulerAddStep(scheduler, {
            kind: 'beforeAll', suiteId, suitePath, caseIndex: null,
            timeoutMs: item.timeout || state.defaultTimeout,
            registrationStack: item.registrationStack,
            fn: item.fn,
          })
        }
      }
      for (const entry of target.entries) {
        if (entry.type === 'suite') {
          appendSuite(entry.value, suitePath, staticallySkipped)
          continue
        }
        const record = entry.value
        const chain = ancestors(target)
        const fullName = [...chain.map(value => value.name).filter(Boolean), record.name]
          .filter(Boolean)
          .join(' ')
        const plan = {
          id: `case-${scheduler.cases.length + 1}`,
          index: scheduler.cases.length,
          name: record.name,
          fullName,
          status: schedulerCaseStatus(record, target, staticallySkipped),
          registrationStack: record.registrationStack,
          suitePath,
        }
        scheduler.cases.push(plan)
        scheduler.steps.push({ kind: 'caseStart', plan })
        if (plan.status === 'run') {
          for (const owner of chain) {
            for (const item of owner.hooks.beforeEach) {
              schedulerAddStep(scheduler, {
                kind: 'beforeEach',
                suiteId: scheduler.suiteIds.get(owner),
                suitePath,
                caseIndex: plan.index,
                timeoutMs: item.timeout || state.defaultTimeout,
                registrationStack: item.registrationStack,
                fn: item.fn,
              })
            }
          }
          schedulerAddStep(scheduler, {
            kind: 'test', suiteId, suitePath, caseIndex: plan.index,
            timeoutMs: record.timeout || state.defaultTimeout,
            registrationStack: record.registrationStack,
            fn: record.fn,
          })
          for (const owner of [...chain].reverse()) {
            for (const item of owner.hooks.afterEach) {
              schedulerAddStep(scheduler, {
                kind: 'afterEach',
                suiteId: scheduler.suiteIds.get(owner),
                suitePath,
                caseIndex: plan.index,
                timeoutMs: item.timeout || state.defaultTimeout,
                registrationStack: item.registrationStack,
                fn: item.fn,
              })
            }
          }
          schedulerAddStep(scheduler, {
            kind: 'cleanup', suiteId, suitePath, caseIndex: plan.index,
            timeoutMs: state.defaultTimeout,
            registrationStack: record.registrationStack,
            fn: async () => {
              await reactCleanup()
              collectFakeTimerLeaks(plan.fullName)
              await clock.restore()
            },
          })
        }
        scheduler.steps.push({ kind: 'caseEnd', plan })
      }
      if (!staticallySkipped) {
        for (const item of target.hooks.afterAll) {
          schedulerAddStep(scheduler, {
            kind: 'afterAll', suiteId, suitePath, caseIndex: null,
            timeoutMs: item.timeout || state.defaultTimeout,
            registrationStack: item.registrationStack,
            fn: item.fn,
          })
        }
      }
      scheduler.steps.push({ kind: 'suiteEnd', suiteId, suitePath, staticallySkipped })
    }

    appendSuite(state.root, [], false)
    schedulerAddStep(scheduler, {
      kind: 'finalize', suiteId: 'suite-1', suitePath: ['suite-1'], caseIndex: null,
      timeoutMs: state.defaultTimeout,
      registrationStack: null,
      fn: async () => {
        collectFakeTimerLeaks(null)
        await clock.restore()
        collectRealTimerLeaks(null, true)
        if (globalThis.happyDOM) await globalThis.happyDOM.close()
      },
    })
    state.scheduler = scheduler
    globalThis.__wakeSerializedSchedulerPlan = JSON.stringify({
      schemaVersion: schedulerSchema,
      cases: scheduler.cases.map(plan => ({
        id: plan.id,
        index: plan.index,
        name: plan.name,
        fullName: plan.fullName,
        status: plan.status,
        registrationStack: plan.registrationStack,
      })),
    })
  }

  function buildFailedScheduler(error) {
    buildScheduler()
    const scheduler = state.scheduler
    scheduler.steps = []
    scheduler.stepsById.clear()
    scheduler.cases = []
    scheduler.suiteFailures.push(failureDetails(error))
    globalThis.__wakeSerializedSchedulerPlan = JSON.stringify({
      schemaVersion: schedulerSchema,
      cases: [],
    })
  }

  function schedulerSuiteIsEntered(step) {
    return step.suitePath.every(suiteId => state.scheduler.enteredSuites.has(suiteId))
  }

  function schedulerFinishCase(plan) {
    const scheduler = state.scheduler
    const active = scheduler.activeCase
    if (!active || active.plan.index !== plan.index) return
    collectFakeTimerLeaks(plan.fullName)
    restoreClock()
    collectRealTimerLeaks(plan.fullName)
    network.reset()
    restoreAllMocks()
    scheduler.results.push(schedulerCaseResult(
      plan,
      active.failures.length ? 'failed' : 'passed',
      active.failures.slice(),
      active.expectation.assertionCalls,
      Math.max(0, realDateNow() - active.started),
    ))
    scheduler.activeCase = null
    state.activeTest = null
  }

  function schedulerStepCursor(step) {
    const scheduler = state.scheduler
    const casePlan = step.caseIndex === null ? null : scheduler.cases[step.caseIndex]
    return {
      schemaVersion: schedulerSchema,
      status: 'step',
      step: {
        id: step.id,
        kind: step.kind,
        suiteId: step.suiteId,
        caseIndex: step.caseIndex,
        caseName: casePlan && casePlan.name,
        caseFullName: casePlan && casePlan.fullName,
        timeoutMs: step.timeoutMs,
        registrationStack: step.registrationStack,
      },
      partialResult: schedulerRuntimeResult(true),
    }
  }

  function schedulerNext() {
    const scheduler = state.scheduler
    if (!scheduler) throw Object.assign(new Error('Wake scheduler has not been prepared'), {code: 'WAKE_TEST_HOST'})
    if (scheduler.currentStep) throw Object.assign(new Error(`Wake scheduler step ${scheduler.currentStep.id} is still active`), {code: 'WAKE_TEST_HOST'})
    while (scheduler.index < scheduler.steps.length) {
      const step = scheduler.steps[scheduler.index++]
      if (step.kind === 'suiteStart') {
        const parentEntered = step.suitePath.slice(0, -1).every(id => scheduler.enteredSuites.has(id) && !scheduler.blockedSuites.has(id))
        if (!step.staticallySkipped && parentEntered) scheduler.enteredSuites.add(step.suiteId)
        continue
      }
      if (step.kind === 'suiteEnd') {
        scheduler.enteredSuites.delete(step.suiteId)
        continue
      }
      if (step.kind === 'caseStart') {
        const plan = step.plan
        if (plan.status !== 'run') {
          scheduler.results.push(schedulerCaseResult(plan, plan.status))
        } else if (!plan.suitePath.every(id => scheduler.enteredSuites.has(id) && !scheduler.blockedSuites.has(id))) {
          scheduler.results.push(schedulerCaseResult(plan, 'skipped'))
        } else {
          const expectation = { fullName: plan.fullName, assertionCalls: 0, expectedAssertions: null, hasAssertions: false, snapshotIndex: 0 }
          scheduler.activeCase = { plan, expectation, failures: [], primaryFailed: false, started: realDateNow() }
          state.activeTest = expectation
          clearAllMocks()
          network.reset()
        }
        continue
      }
      if (step.kind === 'caseEnd') {
        schedulerFinishCase(step.plan)
        continue
      }
      if (step.kind === 'beforeAll') {
        if (!schedulerSuiteIsEntered(step) || scheduler.beforeAllStopped.has(step.suiteId) || scheduler.blockedSuites.has(step.suiteId)) continue
      } else if (step.kind === 'afterAll') {
        if (!schedulerSuiteIsEntered(step) || scheduler.afterAllStopped.has(step.suiteId)) continue
      } else if (step.caseIndex !== null) {
        const active = scheduler.activeCase
        if (!active || active.plan.index !== step.caseIndex) continue
        if ((step.kind === 'beforeEach' || step.kind === 'test') && active.primaryFailed) continue
      }
      scheduler.currentStep = step
      const cursor = schedulerStepCursor(step)
      globalThis.__wakeSerializedSchedulerCursor = JSON.stringify(cursor)
      return globalThis.__wakeSerializedSchedulerCursor
    }
    const cursor = {
      schemaVersion: schedulerSchema,
      status: 'complete',
      result: schedulerRuntimeResult(false),
    }
    globalThis.__wakeSerializedSchedulerCursor = JSON.stringify(cursor)
    return globalThis.__wakeSerializedSchedulerCursor
  }

  function schedulerRecordFailure(step, failure) {
    const scheduler = state.scheduler
    if (step.kind === 'beforeAll') {
      scheduler.suiteFailures.push(failure)
      scheduler.blockedSuites.add(step.suiteId)
      scheduler.beforeAllStopped.add(step.suiteId)
      return
    }
    if (step.kind === 'afterAll') {
      scheduler.suiteFailures.push(failure)
      scheduler.afterAllStopped.add(step.suiteId)
      return
    }
    if (step.kind === 'finalize') {
      scheduler.suiteFailures.push(failure)
      return
    }
    const active = scheduler.activeCase
    if (!active || active.plan.index !== step.caseIndex) {
      scheduler.suiteFailures.push(failure)
      return
    }
    active.failures.push(failure)
    if (step.kind === 'beforeEach' || step.kind === 'test') active.primaryFailed = true
  }

  function schedulerTimeoutFailure(step) {
    const label = step.kind === 'test' ? 'Test callback' : `${step.kind} phase`
    return {
      message: `${label} exceeded ${step.timeoutMs} ms`,
      code: 'WAKE_TEST_TIMEOUT',
      stack: step.registrationStack || null,
      diff: null,
    }
  }

  function schedulerAcknowledgeStep(step) {
    const scheduler = state.scheduler
    step.completed = true
    if (scheduler.currentStep === step) scheduler.currentStep = null
    globalThis.__wakeSerializedSchedulerStep = JSON.stringify({
      schemaVersion: schedulerSchema,
      stepId: step.id,
      timedOut: step.timedOut === true,
    })
    return globalThis.__wakeSerializedSchedulerStep
  }

  async function schedulerRunStep(id) {
    const scheduler = state.scheduler
    const step = scheduler && scheduler.currentStep
    if (!step || step.id !== String(id)) throw Object.assign(new Error(`Wake scheduler step ${id} is not active`), {code: 'WAKE_TEST_HOST'})
    resumeRealTimerTracking()
    try {
      const execution = Promise.resolve().then(async () => {
        if (typeof step.fn !== 'function') return
        if (step.fn.length > 0) throw Object.assign(new Error('Wake tests do not support done callbacks; return or await a Promise'), { code: 'WAKE_TEST_UNSUPPORTED' })
        await step.fn()
        if (step.kind === 'test') {
          const active = scheduler.activeCase.expectation
          if (active.expectedAssertions !== null && active.assertionCalls !== active.expectedAssertions) {
            throw new Error(`Expected ${active.expectedAssertions} assertions, but received ${active.assertionCalls}`)
          }
          if (active.hasAssertions && active.assertionCalls === 0) throw new Error('Expected at least one assertion')
        }
      })
      const guard = new Promise((_resolve, reject) => {
        // The engine deadline interrupts synchronous instruction streams. This real timer owns
        // the same absolute budget for a Promise whose event loop is otherwise idle.
        step.timeoutHandle = realSetTimeout(
          () => reject(Object.assign(new Error(`${step.kind} phase exceeded ${step.timeoutMs} ms`), {code: 'WAKE_TEST_TIMEOUT'})),
          step.timeoutMs,
        )
      })
      await Promise.race([execution, guard])
    } catch (error) {
      step.timedOut = error && error.code === 'WAKE_TEST_TIMEOUT'
      schedulerRecordFailure(
        step,
        step.timedOut ? schedulerTimeoutFailure(step) : failureDetails(error),
      )
    } finally {
      if (step.timeoutHandle !== undefined) realClearTimeout(step.timeoutHandle)
      step.timeoutHandle = undefined
      pauseRealTimerTracking()
    }
    return schedulerAcknowledgeStep(step)
  }

  function schedulerRecordTimeout(id) {
    const scheduler = state.scheduler
    const step = scheduler && scheduler.stepsById.get(String(id))
    if (!step) throw Object.assign(new Error(`Wake scheduler step ${id} is unknown`), {code: 'WAKE_TEST_HOST'})
    if (step.completed) return schedulerAcknowledgeStep(step)
    if (scheduler.currentStep !== step) throw Object.assign(new Error(`Wake scheduler step ${id} is not active`), {code: 'WAKE_TEST_HOST'})
    if (step.timeoutHandle !== undefined) realClearTimeout(step.timeoutHandle)
    step.timeoutHandle = undefined
    step.timedOut = true
    pauseRealTimerTracking()
    schedulerRecordFailure(step, schedulerTimeoutFailure(step))
    if (step.kind === 'cleanup') {
      // V8 termination bypasses React cleanup's finally path. Do not re-enter React after a
      // timeout. Activate the prebuilt ReactDOM graph before detaching the interrupted roots, so
      // the suite keeps one realm while never reusing the contaminated reconciler.
      if (state.reactRecoveryRuntime) {
        bridgeInterruptedReactRuntime(state.reactRecoveryRuntime)
        state.reactRuntimeOverride = state.reactRecoveryRuntime
        state.reactRecoveryRuntime = null
      }
      for (const [container] of [...reactRoots]) {
        reactRoots.delete(container)
        if (container.parentNode) container.parentNode.removeChild(container)
      }
    }
    return schedulerAcknowledgeStep(step)
  }

  async function runCase(record, owner) {
    const chain = ancestors(owner)
    const fullName = [...chain.map(value => value.name).filter(Boolean), record.name].filter(Boolean).join(' ')
    if (record.mode === 'todo') return { name: record.name, fullName, status: 'todo', durationMs: 0, failures: [], assertions: 0, registrationStack: record.registrationStack }
    const nameFiltered = state.namePattern && !state.namePattern.test(fullName)
    if (state.namePattern) state.namePattern.lastIndex = 0
    const skipped = nameFiltered || record.mode === 'skip' || chain.some(value => value.mode === 'skip') || (state.focused && record.mode !== 'only' && !pathFocused(owner))
    if (skipped) return { name: record.name, fullName, status: 'skipped', durationMs: 0, failures: [], assertions: 0, registrationStack: record.registrationStack }
    const active = { fullName, assertionCalls: 0, expectedAssertions: null, hasAssertions: false, snapshotIndex: 0 }
    state.activeTest = active
    const started = realDateNow()
    const failures = []
    try {
      clearAllMocks()
      network.reset()
      for (const value of chain) for (const item of value.hooks.beforeEach) await invoke(item.fn, item.timeout || state.defaultTimeout)
      await invoke(record.fn, record.timeout || state.defaultTimeout)
      if (active.expectedAssertions !== null && active.assertionCalls !== active.expectedAssertions) throw new Error(`Expected ${active.expectedAssertions} assertions, but received ${active.assertionCalls}`)
      if (active.hasAssertions && active.assertionCalls === 0) throw new Error('Expected at least one assertion')
    } catch (error) {
      failures.push(failureDetails(error))
    } finally {
      for (const value of [...chain].reverse()) {
        for (const item of value.hooks.afterEach) {
          try { await invoke(item.fn, item.timeout || state.defaultTimeout) } catch (error) { failures.push(failureDetails(error)) }
        }
      }
      try { await reactCleanup() } catch (error) { failures.push(failureDetails(error)) }
      collectFakeTimerLeaks(fullName)
      await clock.restore()
      collectRealTimerLeaks(fullName)
      network.reset()
      restoreAllMocks()
    }
    state.activeTest = null
    return {
      name: record.name, fullName,
      status: failures.length ? 'failed' : 'passed', durationMs: Math.max(0, realDateNow() - started),
      failures, assertions: active.assertionCalls, registrationStack: record.registrationStack,
    }
  }

  async function runSuite(target, results, suiteFailures) {
    if (target.mode === 'skip') {
      for (const entry of target.entries) {
        if (entry.type === 'test') results.push(await runCase({ ...entry.value, mode: entry.value.mode === 'todo' ? 'todo' : 'skip' }, target))
        else await runSuite({ ...entry.value, mode: 'skip' }, results, suiteFailures)
      }
      return
    }
    try { for (const item of target.hooks.beforeAll) await invoke(item.fn, item.timeout || state.defaultTimeout) }
    catch (error) { suiteFailures.push(failureDetails(error)) }
    if (!suiteFailures.length) {
      for (const entry of target.entries) {
        if (entry.type === 'test') results.push(await runCase(entry.value, target))
        else await runSuite(entry.value, results, suiteFailures)
      }
    }
    try { for (const item of target.hooks.afterAll) await invoke(item.fn, item.timeout || state.defaultTimeout) }
    catch (error) { suiteFailures.push(failureDetails(error)) }
  }

  async function run() {
    const cases = []
    const failures = []
    await runSuite(state.root, cases, failures)
    collectFakeTimerLeaks(null)
    await clock.restore()
    collectRealTimerLeaks(null, true)
    const failed = cases.some(value => value.status === 'failed') || failures.length > 0
    return { schemaVersion: runtimeResultSchema, status: failed ? 'failed' : 'passed', cases, failures, snapshots: state.snapshots, leaks: state.leaks, diagnostics: state.diagnostics }
  }

  const failedRun = error => {
    collectFakeTimerLeaks(null)
    restoreClock()
    collectRealTimerLeaks(null, true)
    return {
      schemaVersion: runtimeResultSchema,
      status: 'failed',
      cases: [],
      failures: [failureDetails(error)],
      snapshots: [],
      leaks: state.leaks,
      diagnostics: state.diagnostics,
    }
  }

  installBuiltins()
  realSetTimeout = globalThis.setTimeout.bind(globalThis)
  realClearTimeout = globalThis.clearTimeout.bind(globalThis)
  globalThis.__wakeTest = api
  globalThis.__wakeRequire = wakeRequire
  globalThis.__wakeDefineModule = defineModule
  globalThis.__wakeLoadModule = id => wakeLoadFrom(String(id), null)
  globalThis.__wakeWhenModulesReady = () => Promise.all([...state.modulePromises])
  Object.defineProperty(globalThis, '__wakeHandleBrowserNetworkRequest', {
    value: handleBrowserNetworkRequest,
    configurable: false,
    enumerable: false,
    writable: false,
  })
  Object.defineProperty(globalThis, '__wakeCompleteBrowserOperation', {
    value: response => {
      if (!response || typeof response !== 'object' || response.schemaVersion !== 'wake.browser.operation.v1') {
        throw browserInputError('Wake browser operation completion has an invalid schema')
      }
      const id = String(response.id)
      const pending = state.browserOperationPending.get(id)
      if (!pending) return false
      state.browserOperationPending.delete(id)
      if (response.ok) pending.resolve(response.value)
      else pending.reject(Object.assign(
        new Error(response.message || 'Wake browser operation failed'),
        {code: response.code || 'WAKE_TEST_BROWSER'},
      ))
      return true
    },
    configurable: false,
    enumerable: false,
    writable: false,
  })
  globalThis.require = wakeRequire
  globalThis.module = { exports: {} }
  globalThis.exports = globalThis.module.exports
  globalThis.__wakeConfigureTest = options => {
    options = options || {}
    state.seed = String(options.seed || '')
    state.defaultTimeout = Number(options.timeoutMs || 5000)
    state.forbidOnly = Boolean(options.forbidOnly)
    state.reactStrictMode = Boolean(options.reactStrictMode)
    state.reactCleanup = options.reactCleanup !== false
    state.reactActWarnings = ['off', 'warn', 'error'].includes(options.reactActWarnings)
      ? options.reactActWarnings
      : 'error'
    state.testIdAttribute = String(options.testIdAttribute || 'data-testid')
    state.environment = options.environment === 'browser' ? 'browser' : 'dom'
    state.networkMode = options.networkMode === 'allow' ? 'allow' : 'deny'
    state.networkAllowHosts = Array.isArray(options.networkAllowHosts) ? options.networkAllowHosts.map(String) : []
    state.namePattern = options.namePattern ? new RegExp(String(options.namePattern), 'u') : null
    state.expectedSnapshots = options.snapshots || Object.create(null)
    state.updateSnapshots = String(options.updateSnapshots || 'new')
    installReactActWarningCapture()
    installNetworkFetch()
  }
  globalThis.__wakeStartTestRun = () => {
    installNetworkFetch()
    installRealTimerTracking()
    globalThis.__wakeSerializedTestResult = undefined
    globalThis.__wakeTestRunPromise = run().then(
      async result => {
        globalThis.__wakeSerializedTestResult = JSON.stringify(result)
        if (globalThis.happyDOM) await globalThis.happyDOM.close()
      },
      error => { globalThis.__wakeSerializedTestResult = JSON.stringify(failedRun(error)) },
    )
  }
  globalThis.__wakeStartTestRunAfterModules = promise => {
    if (promise === undefined) {
      globalThis.__wakeStartTestRun()
      return
    }
    Promise.resolve(promise).then(
      () => globalThis.__wakeStartTestRun(),
      error => { globalThis.__wakeSerializedTestResult = JSON.stringify(failedRun(error)) },
    )
  }
  globalThis.__wakePrepareTestRun = () => {
    installNetworkFetch()
    installRealTimerTracking()
    buildScheduler()
  }
  globalThis.__wakePrepareTestRunAfterModules = promise => {
    globalThis.__wakeSchedulerPreparationPromise = Promise.resolve(promise).then(
      () => globalThis.__wakePrepareTestRun(),
      error => buildFailedScheduler(error),
    )
  }
  globalThis.__wakeSchedulerNext = schedulerNext
  globalThis.__wakeSchedulerRunStep = schedulerRunStep
  globalThis.__wakeSchedulerRecordTimeout = schedulerRecordTimeout
})()
