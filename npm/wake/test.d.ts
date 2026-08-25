export type Awaitable<T> = T | PromiseLike<T>
export type TestCallback = () => Awaitable<void>

export interface TestCaseOptions {
  timeout: number
}

export interface Each {
  <T extends readonly unknown[]>(table: readonly T[]): (
    name: string,
    callback: (...values: T) => Awaitable<void>,
    options?: TestCaseOptions,
  ) => void
  <T>(table: readonly T[]): (
    name: string,
    callback: (value: T) => Awaitable<void>,
    options?: TestCaseOptions,
  ) => void
}

export interface TestApi {
  (name: string, callback: TestCallback, options?: TestCaseOptions): void
  readonly only: TestApi
  readonly skip: TestApi
  todo(name: string): void
  readonly each: Each
}

export interface DescribeApi {
  (name: string, callback: () => void): void
  readonly only: DescribeApi
  readonly skip: DescribeApi
  readonly each: Each
}

export interface AsymmetricMatcher {
  asymmetricMatch(value: unknown): boolean
  toString(): string
}

export interface MatcherResult {
  pass: boolean
  message(): string
}

export type MatcherFunction = (
  received: unknown,
  ...expected: unknown[]
) => Awaitable<MatcherResult>

/** Augment this interface to type matchers registered through `expect.extend()`. */
export interface CustomMatchers {}

export interface Matchers<R = void> extends CustomMatchers {
  readonly not: Matchers<R>
  readonly resolves: Matchers<Promise<void>>
  readonly rejects: Matchers<Promise<void>>
  toBe(expected: unknown): R
  toEqual(expected: unknown): R
  toStrictEqual(expected: unknown): R
  toBeDefined(): R
  toBeUndefined(): R
  toBeNull(): R
  toBeTruthy(): R
  toBeFalsy(): R
  toBeNaN(): R
  toBeGreaterThan(expected: number | bigint): R
  toBeGreaterThanOrEqual(expected: number | bigint): R
  toBeLessThan(expected: number | bigint): R
  toBeLessThanOrEqual(expected: number | bigint): R
  toBeCloseTo(expected: number, digits?: number): R
  toContain(expected: unknown): R
  toContainEqual(expected: unknown): R
  toHaveLength(expected: number): R
  toMatch(expected: string | RegExp): R
  toMatchObject(expected: object): R
  toHaveProperty(path: string | readonly (string | number)[], expected?: unknown): R
  toBeInstanceOf(expected: Function): R
  toThrow(expected?: string | RegExp | Function | Error): R
  toHaveBeenCalled(): R
  toHaveBeenCalledTimes(count: number): R
  toHaveBeenCalledWith(...expected: unknown[]): R
  toHaveBeenLastCalledWith(...expected: unknown[]): R
  toHaveBeenNthCalledWith(call: number, ...expected: unknown[]): R
  toHaveReturned(): R
  toHaveReturnedTimes(count: number): R
  toHaveReturnedWith(expected: unknown): R
  toHaveLastReturnedWith(expected: unknown): R
  toHaveNthReturnedWith(call: number, expected: unknown): R
  toMatchSnapshot(propertyMatchers?: object, hint?: string): R
  /** Browser-only exact visual snapshot for the current viewport or received Element. */
  toMatchScreenshot(hint?: string): Promise<void>
  toBeInTheDocument(): R
  toContainElement(element: Element | null): R
  toContainHTML(html: string): R
  toBeEmptyDOMElement(): R
  toBeVisible(): R
  toBeEnabled(): R
  toBeDisabled(): R
  toHaveAttribute(name: string, value?: string | RegExp): R
  toHaveClass(...classNames: string[]): R
  toHaveStyle(style: string | Record<string, string | number>): R
  toHaveTextContent(text: string | RegExp, options?: { normalizeWhitespace?: boolean }): R
  toHaveValue(value?: string | number | readonly string[]): R
  toHaveDisplayValue(value: string | RegExp | readonly (string | RegExp)[]): R
  toHaveFormValues(values: Record<string, unknown>): R
  toHaveFocus(): R
  toBeChecked(): R
  toBePartiallyChecked(): R
  toBeRequired(): R
  toBeInvalid(): R
  toBeValid(): R
  toHaveAccessibleName(name?: string | RegExp): R
  toHaveAccessibleDescription(description?: string | RegExp): R
  toHaveAccessibleErrorMessage(message?: string | RegExp): R
  toHaveRole(role: string): R
  toHaveSelection(selection: string): R
}

export interface Expect {
  <T>(received: T): Matchers
  extend(matchers: Readonly<Record<string, MatcherFunction>>): void
  addEqualityTesters(testers: readonly ((left: unknown, right: unknown) => boolean | undefined)[]): void
  addSnapshotSerializer(serializer: {
    test(value: unknown): boolean
    print(value: unknown, serialize: (value: unknown) => string): string
  }): void
  assertions(count: number): void
  hasAssertions(): void
  getState(): Readonly<Record<string, unknown>>
  setState(state: Readonly<Record<string, unknown>>): void
  anything(): AsymmetricMatcher
  any(constructor: Function): AsymmetricMatcher
  arrayContaining(sample: readonly unknown[]): AsymmetricMatcher
  objectContaining(sample: object): AsymmetricMatcher
  stringContaining(sample: string): AsymmetricMatcher
  stringMatching(sample: string | RegExp): AsymmetricMatcher
  closeTo(sample: number, digits?: number): AsymmetricMatcher
}

export type AnyFunction = (...args: any[]) => any

export interface MockResult {
  type: 'return' | 'throw' | 'incomplete'
  value: unknown
}

export interface MockState<T extends AnyFunction> {
  calls: Parameters<T>[]
  contexts: unknown[]
  instances: unknown[]
  invocationCallOrder: number[]
  results: MockResult[]
  lastCall?: Parameters<T>
}

export interface MockFunction<T extends AnyFunction = AnyFunction> {
  (...args: Parameters<T>): ReturnType<T>
  readonly isMockFunction: true
  readonly calls: Readonly<MockState<T>>
  clear(): this
  reset(): this
  restore(): void
  implement(implementation: T): this
  implementOnce(implementation: T): this
  return(value: ReturnType<T>): this
  returnOnce(value: ReturnType<T>): this
  resolve(value: Awaited<ReturnType<T>>): this
  resolveOnce(value: Awaited<ReturnType<T>>): this
  reject(reason: unknown): this
  rejectOnce(reason: unknown): this
  named(name: string): this
  readonly name: string
}

export interface ReplacedProperty<T> {
  replace(value: T): void
  restore(): void
}

export interface MockApi {
  fn<T extends AnyFunction = AnyFunction>(implementation?: T): MockFunction<T>
  spyOn<T extends object, K extends keyof T>(
    object: T,
    key: K,
    accessType?: 'get' | 'set',
  ): T[K] extends AnyFunction ? MockFunction<T[K]> : MockFunction
  replaceProperty<T extends object, K extends keyof T>(object: T, key: K, value: T[K]): ReplacedProperty<T[K]>
  module<T = unknown>(specifier: string, factory: () => Awaitable<T>): void
  import<T = unknown>(specifier: string): Promise<T>
  actual<T = unknown>(specifier: string): Promise<T>
  isolate<T>(callback: () => Awaitable<T>): Promise<T>
  clearAll(): void
  resetAll(): void
  restoreAll(): void
}

export interface FakeClockOptions {
  now?: number | Date
  timerLimit?: number
  exclude?: readonly ('date' | 'performance' | 'timeout' | 'interval' | 'immediate' | 'microtask' | 'animationFrame' | 'idleCallback')[]
}

export interface ClockApi {
  fake(options?: FakeClockOptions): Promise<ClockApi>
  restore(): Promise<ClockApi>
  advanceBy(milliseconds: number): Promise<void>
  advanceTo(timestamp: number | Date): Promise<void>
  runNext(): Promise<boolean>
  runAll(): Promise<void>
  flushMicrotasks(): Promise<void>
}

export interface NetworkRequest {
  readonly id: string
  readonly url: URL
  readonly method: string
  readonly headers: Headers
  readonly body: Uint8Array | null
}

export interface NetworkResponse {
  status?: number
  statusText?: string
  headers?: HeadersInit
  body?: BodyInit | object | null
  delayMs?: number
}

export type NetworkMatcher =
  | string
  | URL
  | RegExp
  | {
      method?: string
      url?: string | URL | RegExp
    }
  | ((request: NetworkRequest) => boolean)

export type NetworkHandler = (
  request: NetworkRequest,
) => Awaitable<NetworkResponse | Response>

export interface NetworkDisposer {
  (): void
}

export interface NetworkApi {
  route(matcher: NetworkMatcher, handler: NetworkHandler): NetworkDisposer
  allow(matcher: NetworkMatcher): NetworkDisposer
  requests(): readonly NetworkRequest[]
  reset(): void
}

export const test: TestApi
export const it: TestApi
export const describe: DescribeApi
export const beforeAll: (callback: TestCallback, options?: TestCaseOptions) => void
export const beforeEach: (callback: TestCallback, options?: TestCaseOptions) => void
export const afterAll: (callback: TestCallback, options?: TestCaseOptions) => void
export const afterEach: (callback: TestCallback, options?: TestCaseOptions) => void
export const expect: Expect
export const mock: MockApi
export const clock: ClockApi
export const network: NetworkApi
