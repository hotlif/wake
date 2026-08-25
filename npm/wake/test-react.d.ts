import type { ReactElement, ReactNode } from 'react'

export * from '@crab-dev/wake/test'

export interface RenderOptions {
  container?: HTMLElement
  baseElement?: HTMLElement
  hydrate?: boolean
  strict?: boolean
  wrapper?: (props: { children: ReactNode }) => ReactElement
  identifierPrefix?: string
  onCaughtError?: (error: unknown, info: { componentStack: string }) => void
  onUncaughtError?: (error: unknown, info: { componentStack: string }) => void
  onRecoverableError?: (error: unknown, info: { componentStack: string }) => void
}

export interface RenderResult {
  readonly container: HTMLElement
  readonly baseElement: HTMLElement
  rerender(ui: ReactElement): Promise<void>
  unmount(): Promise<void>
  asFragment(): DocumentFragment
  debug(element?: Element | DocumentFragment): string
}

export interface RenderHookOptions<Props> {
  initialProps?: Props
  wrapper?: (props: { children: ReactNode }) => ReactElement
  strict?: boolean
}

export interface RenderHookResult<Result, Props> {
  readonly result: { readonly current: Result }
  rerender(props?: Props): Promise<void>
  unmount(): Promise<void>
}

export type TextMatcher = string | RegExp | ((content: string, element: Element) => boolean)

export interface RoleQueryOptions {
  name?: TextMatcher
  description?: TextMatcher
  hidden?: boolean
  selected?: boolean
  checked?: boolean
  pressed?: boolean
  expanded?: boolean
  level?: number
}

export interface QueryOptions {
  exact?: boolean
  timeout?: number
}

export interface Queries {
  getByRole(role: string, options?: RoleQueryOptions): HTMLElement
  getAllByRole(role: string, options?: RoleQueryOptions): HTMLElement[]
  queryByRole(role: string, options?: RoleQueryOptions): HTMLElement | null
  queryAllByRole(role: string, options?: RoleQueryOptions): HTMLElement[]
  findByRole(role: string, options?: RoleQueryOptions & QueryOptions): Promise<HTMLElement>
  findAllByRole(role: string, options?: RoleQueryOptions & QueryOptions): Promise<HTMLElement[]>
  getByText(text: TextMatcher, options?: QueryOptions): HTMLElement
  getAllByText(text: TextMatcher, options?: QueryOptions): HTMLElement[]
  queryByText(text: TextMatcher, options?: QueryOptions): HTMLElement | null
  queryAllByText(text: TextMatcher, options?: QueryOptions): HTMLElement[]
  findByText(text: TextMatcher, options?: QueryOptions): Promise<HTMLElement>
  findAllByText(text: TextMatcher, options?: QueryOptions): Promise<HTMLElement[]>
  getByLabelText(text: TextMatcher, options?: QueryOptions): HTMLElement
  getAllByLabelText(text: TextMatcher, options?: QueryOptions): HTMLElement[]
  queryByLabelText(text: TextMatcher, options?: QueryOptions): HTMLElement | null
  queryAllByLabelText(text: TextMatcher, options?: QueryOptions): HTMLElement[]
  findByLabelText(text: TextMatcher, options?: QueryOptions): Promise<HTMLElement>
  findAllByLabelText(text: TextMatcher, options?: QueryOptions): Promise<HTMLElement[]>
  getByDisplayValue(text: TextMatcher, options?: QueryOptions): HTMLElement
  getAllByDisplayValue(text: TextMatcher, options?: QueryOptions): HTMLElement[]
  queryByDisplayValue(text: TextMatcher, options?: QueryOptions): HTMLElement | null
  queryAllByDisplayValue(text: TextMatcher, options?: QueryOptions): HTMLElement[]
  findByDisplayValue(text: TextMatcher, options?: QueryOptions): Promise<HTMLElement>
  findAllByDisplayValue(text: TextMatcher, options?: QueryOptions): Promise<HTMLElement[]>
  getByPlaceholderText(text: TextMatcher, options?: QueryOptions): HTMLElement
  getAllByPlaceholderText(text: TextMatcher, options?: QueryOptions): HTMLElement[]
  queryByPlaceholderText(text: TextMatcher, options?: QueryOptions): HTMLElement | null
  queryAllByPlaceholderText(text: TextMatcher, options?: QueryOptions): HTMLElement[]
  findByPlaceholderText(text: TextMatcher, options?: QueryOptions): Promise<HTMLElement>
  findAllByPlaceholderText(text: TextMatcher, options?: QueryOptions): Promise<HTMLElement[]>
  getByAltText(text: TextMatcher, options?: QueryOptions): HTMLElement
  getAllByAltText(text: TextMatcher, options?: QueryOptions): HTMLElement[]
  queryByAltText(text: TextMatcher, options?: QueryOptions): HTMLElement | null
  queryAllByAltText(text: TextMatcher, options?: QueryOptions): HTMLElement[]
  findByAltText(text: TextMatcher, options?: QueryOptions): Promise<HTMLElement>
  findAllByAltText(text: TextMatcher, options?: QueryOptions): Promise<HTMLElement[]>
  getByTitle(text: TextMatcher, options?: QueryOptions): HTMLElement
  getAllByTitle(text: TextMatcher, options?: QueryOptions): HTMLElement[]
  queryByTitle(text: TextMatcher, options?: QueryOptions): HTMLElement | null
  queryAllByTitle(text: TextMatcher, options?: QueryOptions): HTMLElement[]
  findByTitle(text: TextMatcher, options?: QueryOptions): Promise<HTMLElement>
  findAllByTitle(text: TextMatcher, options?: QueryOptions): Promise<HTMLElement[]>
  getByTestId(id: TextMatcher, options?: QueryOptions): HTMLElement
  getAllByTestId(id: TextMatcher, options?: QueryOptions): HTMLElement[]
  queryByTestId(id: TextMatcher, options?: QueryOptions): HTMLElement | null
  queryAllByTestId(id: TextMatcher, options?: QueryOptions): HTMLElement[]
  findByTestId(id: TextMatcher, options?: QueryOptions): Promise<HTMLElement>
  findAllByTestId(id: TextMatcher, options?: QueryOptions): Promise<HTMLElement[]>
  debug(element?: Element | DocumentFragment): string
}

export interface UserEventOptions {
  delayMs?: number
  document?: Document
}

export interface UserEventController {
  click(element: Element): Promise<void>
  dblClick(element: Element): Promise<void>
  type(element: Element, text: string): Promise<void>
  clear(element: Element): Promise<void>
  keyboard(input: string): Promise<void>
  tab(options?: { shift?: boolean }): Promise<void>
  hover(element: Element): Promise<void>
  unhover(element: Element): Promise<void>
  selectOptions(element: Element, values: string | readonly string[] | Element | readonly Element[]): Promise<void>
  upload(element: HTMLInputElement, files: File | readonly File[]): Promise<void>
}

export interface UserEventApi {
  setup(options?: UserEventOptions): UserEventController
}

export interface FireEventApi {
  (element: Element, event: Event): Promise<boolean>
  change(element: Element, init?: EventInit & { target?: Record<string, unknown> }): Promise<boolean>
  click(element: Element, init?: MouseEventInit): Promise<boolean>
  input(element: Element, init?: InputEventInit & { target?: Record<string, unknown> }): Promise<boolean>
  keyDown(element: Element, init?: KeyboardEventInit): Promise<boolean>
  keyUp(element: Element, init?: KeyboardEventInit): Promise<boolean>
  submit(element: Element, init?: SubmitEventInit): Promise<boolean>
}

export interface WaitForOptions {
  container?: HTMLElement
  timeout?: number
  interval?: number
}

export function render(ui: ReactElement, options?: RenderOptions): Promise<RenderResult>
export function renderHook<Result, Props = void>(
  callback: (props: Props) => Result,
  options?: RenderHookOptions<Props>,
): Promise<RenderHookResult<Result, Props>>
export function cleanup(): Promise<void>
export function act<T>(callback: () => T | PromiseLike<T>): Promise<Awaited<T>>
export function waitFor<T>(callback: () => T | PromiseLike<T>, options?: WaitForOptions): Promise<T>
export function waitForElementToBeRemoved(
  callback: () => Element | readonly Element[] | null,
  options?: WaitForOptions,
): Promise<void>
export function prettyDOM(element?: Element | DocumentFragment, maxLength?: number): string
export function within(element: HTMLElement): Queries
export const screen: Queries
export const userEvent: UserEventApi
export const fireEvent: FireEventApi
