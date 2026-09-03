import type { FederationDevUpdate, FederationRuntime } from './federation.mjs'

export { FEDERATION_ISOLATED_REMOUNT_EVENT } from './federation.mjs'

export type StructuredPrimitive = null | boolean | number | string
export type StructuredValue = StructuredPrimitive | readonly StructuredValue[] | { readonly [key: string]: StructuredValue }

export interface IsolatedStyle {
  readonly kind?: 'css'
  readonly url?: string
  readonly cssText?: string
  readonly integrity?: string
  readonly contentHash?: string
  readonly size?: number
  readonly mime?: string
}

export interface IsolatedLifecycleContext<
  Props extends StructuredValue = StructuredValue,
  Slots extends Readonly<Record<string, Node>> = Readonly<Record<string, Node>>,
> {
  readonly host: HTMLElement
  readonly shadowRoot: ShadowRoot
  readonly mountRoot: HTMLElement
  readonly portalRoot: HTMLElement
  readonly props: Props
  /** Own, plain-record DOM Node values; React elements, refs, functions, and accessors are rejected. */
  readonly slots: Slots
  emit(type: string, detail: StructuredValue): boolean
}

export interface IsolatedLifecycle<
  Props extends StructuredValue = StructuredValue,
  Instance = unknown,
  Slots extends Readonly<Record<string, Node>> = Readonly<Record<string, Node>>,
> {
  mount(context: IsolatedLifecycleContext<Props, Slots>): Instance | Promise<Instance>
  update(instance: Instance, context: IsolatedLifecycleContext<Props, Slots>): unknown | Promise<unknown>
  unmount(instance: Instance, context: IsolatedLifecycleContext<Props, Slots>): unknown | Promise<unknown>
}

export interface IsolatedStyleLoadContext {
  readonly root: ShadowRoot
  readonly document: Document
  readonly signal?: AbortSignal
  readonly nonce?: string
  readonly maxAssetSize: number
}

export interface IsolatedBridgeDevIdentity {
  readonly remote: string
  readonly expose: `./${string}`
  readonly eventTarget?: EventTarget
}

export interface IsolatedBridgeOptions<
  Props extends StructuredValue = StructuredValue,
  Instance = unknown,
  Slots extends Readonly<Record<string, Node>> = Readonly<Record<string, Node>>,
> {
  readonly load: () =>
    IsolatedLifecycle<Props, Instance, Slots> |
    { readonly default: IsolatedLifecycle<Props, Instance, Slots> } |
    Promise<IsolatedLifecycle<Props, Instance, Slots> | { readonly default: IsolatedLifecycle<Props, Instance, Slots> }>
  readonly styles?: readonly (string | IsolatedStyle)[]
  readonly shadowMode?: 'open'
  readonly nonce?: string
  readonly styleTimeoutMs?: number
  readonly maxAssetSize?: number
  readonly loadStyle?: (
    style: Readonly<IsolatedStyle>,
    context: IsolatedStyleLoadContext,
  ) => Node | void | Promise<Node | void>
  /** Opts this isolated root into stateless remounts from Wake's development update event. */
  readonly dev?: IsolatedBridgeDevIdentity
  readonly onDevRemountError?: (error: unknown, update: Readonly<FederationDevUpdate>) => void
}

export type IsolatedBridgeStatus =
  | 'idle'
  | 'mounting'
  | 'mounted'
  | 'unmounting'
  | 'unmounted'
  | 'failed'

export interface IsolatedBridgeController<
  Props extends StructuredValue = StructuredValue,
  Instance = unknown,
  Slots extends Readonly<Record<string, Node>> = Readonly<Record<string, Node>>,
> {
  readonly status: IsolatedBridgeStatus
  readonly shadowRoot?: ShadowRoot
  readonly mountRoot?: HTMLElement
  readonly portalRoot?: HTMLElement
  mount(host: HTMLElement, props?: Props, options?: Readonly<{ slots?: Slots }>): Promise<Instance>
  update(props: Props): Promise<unknown>
  unmount(): Promise<void>
}

export declare function createIsolatedBridge<
  Props extends StructuredValue = StructuredValue,
  Instance = unknown,
  Slots extends Readonly<Record<string, Node>> = Readonly<Record<string, Node>>,
>(options: IsolatedBridgeOptions<Props, Instance, Slots>): IsolatedBridgeController<Props, Instance, Slots>

export declare const createIsolatedReactBridge: typeof createIsolatedBridge

export type FederatedIsolatedBridgeOptions<
  Props extends StructuredValue = StructuredValue,
  Instance = unknown,
  Slots extends Readonly<Record<string, Node>> = Readonly<Record<string, Node>>,
> = Omit<IsolatedBridgeOptions<Props, Instance, Slots>, 'load' | 'dev'> & Readonly<{
  dev?: false | Readonly<{ eventTarget?: EventTarget }>
}>

export declare function createFederatedIsolatedBridge<
  Props extends StructuredValue = StructuredValue,
  Instance = unknown,
  Slots extends Readonly<Record<string, Node>> = Readonly<Record<string, Node>>,
>(
  /** The bridge attaches its open ShadowRoot before this runtime loads Manifest CSS or module code. */
  runtime: FederationRuntime,
  specifier: string,
  options?: FederatedIsolatedBridgeOptions<Props, Instance, Slots>,
): Promise<IsolatedBridgeController<Props, Instance, Slots>>

export declare const createFederatedIsolatedReactBridge: typeof createFederatedIsolatedBridge

export interface HostRenderedLazyOptions<Component, Namespace> {
  readonly exportName?: string
  readonly adapt?: (component: Component, namespace: Namespace) => Component | Promise<Component>
  readonly onResolved?: (component: Component, namespace: Namespace) => void | Promise<void>
}

export declare function createHostRenderedLazyFactory<
  Component,
  Namespace extends Readonly<Record<string, unknown>> = Readonly<Record<string, unknown>>,
>(
  loadModule: () => Namespace | Promise<Namespace>,
  options?: HostRenderedLazyOptions<Component, Namespace>,
): () => Promise<Readonly<{ default: Component }>>

export declare const createHostRenderedAdapter: typeof createHostRenderedLazyFactory
