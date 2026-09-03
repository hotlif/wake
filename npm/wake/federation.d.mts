export declare const FEDERATION_RUNTIME_ABI: 'wake.federation.v1'
export declare const FEDERATION_MANIFEST_SCHEMA: 'wake.federation.manifest.v1'
export declare const FEDERATION_DEV_UPDATE_SCHEMA: 'wake.federation.dev-update.v1'
export declare const FEDERATION_DEV_LEASE_SCHEMA: 'wake.federation.dev-lease.v1'
export declare const FEDERATION_DEV_MAX_BUILD_LEASES: 8
export declare const FEDERATION_ISOLATED_REMOUNT_EVENT: 'wake:federation:isolated-remount'

export declare const FEDERATION_ERROR_CODES: Readonly<{
  INVALID_SPECIFIER: 'FED_INVALID_SPECIFIER'
  CONFIG_INVALID: 'FED_CONFIG_INVALID'
  LOCK_REQUIRED: 'FED_LOCK_REQUIRED'
  LOCK_INVALID: 'FED_LOCK_INVALID'
  LOCK_MISMATCH: 'FED_LOCK_MISMATCH'
  UNKNOWN_REMOTE: 'FED_UNKNOWN_REMOTE'
  MANIFEST_FETCH: 'FED_MANIFEST_FETCH'
  MANIFEST_SCHEMA: 'FED_MANIFEST_SCHEMA'
  RUNTIME_ABI: 'FED_RUNTIME_ABI'
  ORIGIN_DENIED: 'FED_ORIGIN_DENIED'
  MANIFEST_INTEGRITY: 'FED_MANIFEST_INTEGRITY'
  ASSET_INTEGRITY: 'FED_ASSET_INTEGRITY'
  ASSET_MIME: 'FED_ASSET_MIME'
  ASSET_SIZE: 'FED_ASSET_SIZE'
  UNKNOWN_EXPOSE: 'FED_UNKNOWN_EXPOSE'
  CONTAINER_INIT: 'FED_CONTAINER_INIT'
  CONTAINER_GET: 'FED_CONTAINER_GET'
  SHARE_UNSATISFIABLE: 'FED_SHARE_UNSATISFIABLE'
  SHARE_SINGLETON_CONFLICT: 'FED_SHARE_SINGLETON_CONFLICT'
  COHERENCE_CONFLICT: 'FED_COHERENCE_CONFLICT'
  TYPE_BUILD_MISMATCH: 'FED_TYPE_BUILD_MISMATCH'
  TYPES_INVALID: 'FED_TYPES_INVALID'
  TIMEOUT: 'FED_TIMEOUT'
  NETWORK: 'FED_NETWORK'
  STATIC_REMOTE_UNSUPPORTED: 'FED_STATIC_REMOTE_UNSUPPORTED'
  REMOTE_CYCLE: 'FED_REMOTE_CYCLE'
  REMOTE_CONFLICT: 'FED_REMOTE_CONFLICT'
  UNSUPPORTED_ENVIRONMENT: 'FED_UNSUPPORTED_ENVIRONMENT'
  CONTAINER_REGISTRATION: 'FED_CONTAINER_REGISTRATION'
  BRIDGE_LIFECYCLE: 'FED_BRIDGE_LIFECYCLE'
  BRIDGE_PROPS: 'FED_BRIDGE_PROPS'
  STYLE_LOAD: 'FED_STYLE_LOAD'
}>

export type FederationErrorCode = typeof FEDERATION_ERROR_CODES[keyof typeof FEDERATION_ERROR_CODES]
export type FederationMode = 'development' | 'production'
export type FederationDevUpdateAction = 'types-only' | 'isolated-remount' | 'full-reload'
export type FederationDevLeaseReloadReason = 'build-gone' | 'invalid-lease' | 'lease-limit' | 'update-lagged'
export type ExposeMode = 'generic' | 'host-rendered' | 'isolated'
export type AssetKind = 'javascript' | 'css' | 'source-map' | 'other'
export type ModuleNamespace = Readonly<Record<string, unknown>>

/**
 * A monotonic development control-plane update. `types-only` advances the
 * build/generation cursor without replacing already active code. Canonically
 * identical messages for the current generation are idempotent.
 */
export interface FederationDevUpdate {
  readonly schemaVersion: 'wake.federation.dev-update.v1'
  readonly remote: string
  readonly oldBuildId?: string | null
  readonly newBuildId: string
  readonly changedExposes: readonly `./${string}`[]
  readonly typesHash?: string | null
  readonly generation: number
  readonly action: FederationDevUpdateAction
}

/** Complete replacement of the build snapshots retained for one development browser socket. */
export interface FederationDevBuildLease {
  readonly type: 'lease'
  readonly schemaVersion: 'wake.federation.dev-lease.v1'
  readonly remote: string
  /** Canonically sorted unique build IDs; length is between 1 and 8. */
  readonly buildIds: readonly string[]
}

export interface FederationDevBuildLeaseAck {
  readonly type: 'lease-ack'
  readonly schemaVersion: 'wake.federation.dev-lease.v1'
  readonly remote: string
  readonly buildIds: readonly string[]
  readonly currentBuildId: string
  readonly generation: number
}

export interface FederationDevFullReload {
  readonly type: 'full-reload'
  readonly schemaVersion: 'wake.federation.dev-lease.v1'
  readonly remote: string
  readonly currentBuildId: string
  readonly generation: number
  readonly expiredBuildId: string | null
  readonly reason: FederationDevLeaseReloadReason
}

export type FederationDevLeaseMessage =
  | FederationDevBuildLease
  | FederationDevBuildLeaseAck
  | FederationDevFullReload

export interface FederatedAssetRequest {
  readonly name: string
  readonly buildId: string
  readonly fileName: string
  readonly kind: 'javascript' | 'css'
  readonly expose?: `./${string}`
}

export interface FederationAssetExecutionContext {
  readonly name: string
  readonly buildId: string
  readonly generation: number
  readonly development?: boolean
  readonly expose?: `./${string}`
}

export interface FederationRequesterIdentity {
  readonly name?: string
  readonly container?: string | Readonly<{
    name: string
    buildId?: string
    expose?: `./${string}`
  }>
  readonly buildId?: string
  readonly expose?: `./${string}`
}

export interface FederationErrorOptions {
  phase?: string
  retryable?: boolean
  details?: Readonly<Record<string, unknown>>
  cause?: unknown
}

export declare class FederationError extends Error {
  constructor(code: FederationErrorCode | (string & {}), message: string, options?: FederationErrorOptions)
  readonly name: 'FederationError'
  readonly code: FederationErrorCode | (string & {})
  readonly phase: string
  readonly retryable: boolean
  readonly details: Readonly<Record<string, unknown>>
  toJSON(): Readonly<{
    name: string
    code: string
    message: string
    phase: string
    retryable: boolean
    details: Readonly<Record<string, unknown>>
  }>
}

export interface FederationAsset {
  readonly kind: AssetKind
  readonly url: string
  readonly contentHash: string
  readonly integrity: `sha384-${string}` | string
  readonly size: number
  readonly mime: string
}

export interface FederationExpose {
  readonly mode: ExposeMode
  readonly scope: string
  readonly shadow: 'open' | 'none'
  readonly entry: FederationAsset
  readonly css: readonly FederationAsset[]
  readonly sourceMap?: FederationAsset
  readonly synchronousAssets: readonly FederationAsset[]
  readonly asynchronousAssets: readonly FederationAsset[]
}

export interface FederationPackageIdentity {
  readonly name: string
  readonly version: string
  readonly packageContext: string
  readonly buildVariant: string
}

export interface SharedPolicy {
  readonly scope: string
  readonly singleton: boolean
  readonly strict: boolean
  readonly fallback: boolean
  readonly coherenceGroup?: string
  readonly owner?: string
}

export interface SharedRequirement {
  readonly shareKey: string
  readonly requiredVersion: string
  readonly packageContext: string
  readonly buildVariant: string
  readonly policy: SharedPolicy
  readonly fallback?: FederationAsset
}

export interface SharedRequest {
  readonly shareKey: string
  readonly requiredVersion?: string
  readonly scope?: string
  readonly singleton?: boolean
  readonly strict?: boolean
  readonly packageContext?: string
  readonly buildVariant?: string
  readonly coherenceGroup?: string
  readonly owner?: string
  readonly fallback?: boolean
}

export interface SharedOffer {
  readonly shareKey: string
  readonly package: FederationPackageIdentity
  readonly provider: string
  readonly policy: SharedPolicy
  readonly asset?: FederationAsset
}

export interface FederationTypeArtifact {
  readonly buildId: string
  readonly url: string
  readonly contentHash: string
  readonly integrity: `sha384-${string}` | string
  readonly size: number
  readonly format: 'declaration-bundle'
}

export interface FederationManifest {
  readonly schemaVersion: 'wake.federation.manifest.v1'
  readonly runtimeAbi: 'wake.federation.v1'
  readonly name: string
  readonly buildId: string
  readonly browserTarget: string
  readonly remoteEntry: FederationAsset
  readonly remoteEntrySourceMap?: FederationAsset
  readonly exposes: Readonly<Record<`./${string}`, FederationExpose>>
  readonly shared: Readonly<{
    offers: readonly SharedOffer[]
    requirements: readonly SharedRequirement[]
  }>
  readonly types?: FederationTypeArtifact
  readonly development?: Readonly<{
    updatesUrl: string
    generation: number
  }>
}

/** The canonical JSON shape emitted by Wake before runtime validation and normalization. */
export type FederationExposeWire = Omit<FederationExpose, 'sourceMap'> & Readonly<{
  sourceMap: FederationAsset | null
}>

export type SharedPolicyWire = Omit<SharedPolicy, 'coherenceGroup' | 'owner'> & Readonly<{
  coherenceGroup: string | null
  owner: string | null
}>

export type SharedOfferWire = Omit<SharedOffer, 'policy' | 'asset'> & Readonly<{
  policy: SharedPolicyWire
  asset: FederationAsset | null
}>

export type SharedRequirementWire = Omit<SharedRequirement, 'policy' | 'fallback'> & Readonly<{
  policy: SharedPolicyWire
  fallback: FederationAsset | null
}>

export type FederationManifestWire = Omit<
  FederationManifest,
  'remoteEntrySourceMap' | 'exposes' | 'shared' | 'types' | 'development'
> & Readonly<{
  remoteEntrySourceMap: FederationAsset | null
  exposes: Readonly<Record<`./${string}`, FederationExposeWire>>
  shared: Readonly<{
    offers: readonly SharedOfferWire[]
    requirements: readonly SharedRequirementWire[]
  }>
  types: FederationTypeArtifact | null
  development: Readonly<{
    updatesUrl: string
    generation: number
  }> | null
}>

export interface ReadonlyShareContext {
  readonly runtimeAbi: 'wake.federation.v1'
  readonly container: Readonly<{ name: string; buildId: string }>
  readonly resolved: Readonly<Record<string, unknown>>
  resolve<T = unknown>(request: string | SharedRequest | SharedRequirement): Promise<T>
  getSync<T = unknown>(shareKey: string, scope?: string): T
}

export interface WakeContainerV1 {
  init(scope: ReadonlyShareContext): void | Promise<void>
  get(expose: string): (() => unknown | Promise<unknown>) | Promise<() => unknown | Promise<unknown>>
  getShared?(shareKey: string): unknown | Promise<unknown>
}

export interface WakeContainerRegistration {
  readonly name: string
  readonly buildId: string
  readonly container: WakeContainerV1
}

export interface HostSharedProvider<T = unknown> {
  readonly shareKey: string
  readonly version: string
  readonly scope?: string
  readonly singleton?: boolean
  readonly strict?: boolean
  readonly packageContext?: string
  readonly buildVariant?: string
  readonly coherenceGroup?: string
  readonly owner?: string
  readonly fallback?: boolean
  readonly module?: T
  readonly get?: () => T | Promise<T>
}

export interface FederationLockAsset {
  readonly url: string
  readonly integrity: string
}

export interface FederationRemoteLockBase {
  readonly manifestUrl?: string
  readonly buildId: string
  readonly manifestIntegrity: `sha384-${string}` | string
  readonly allowedAssets?: Readonly<Record<string, string>>
  /** Accepted aliases for callers that construct a lock programmatically. */
  readonly typesHash?: string
  readonly assets?: readonly FederationLockAsset[] | Readonly<Record<string, string>>
  readonly assetClosure?: readonly FederationLockAsset[] | Readonly<Record<string, string>>
}

/** Expose presence is bound to the reviewed Manifest and determines whether types are mandatory. */
export type FederationRemoteLock = FederationRemoteLockBase &
  (
    | {
        readonly hasExposes: false
        /** Legacy v1 locks may contain null; canonical writers omit this field. */
        readonly typesIntegrity?: `sha384-${string}` | string | null
      }
    | {
        readonly hasExposes: true
        readonly typesIntegrity: `sha384-${string}` | string
      }
  )

export interface FederationRemoteDefinition {
  readonly name: string
  readonly manifestUrl: string
  readonly allowedOrigins?: readonly string[]
  readonly mode?: FederationMode
  readonly expectedBuildId?: string
  readonly buildId?: string
  readonly manifestIntegrity?: `sha384-${string}` | string
  readonly typesIntegrity?: `sha384-${string}` | string
  readonly typesHash?: string
  readonly lock?: FederationRemoteLock
  /** Positive safe integer, at most 300,000 milliseconds. */
  readonly timeoutMs?: number
  /** Positive safe integer, at most 16 MiB. */
  readonly maxManifestSize?: number
  /** Positive safe integer, at most 512 MiB. */
  readonly maxAssetSize?: number
}

export interface FederationTransportManifestResult {
  /** Untrusted decoded JSON. Wake validates and normalizes it into FederationManifest. */
  readonly manifest: unknown
  readonly rawBytes?: Uint8Array | ArrayBuffer | string
  readonly contentType?: string
  readonly verifiedIntegrity?: boolean
}

export interface FederationTransportContext {
  readonly signal?: AbortSignal
  readonly mode?: FederationMode
  readonly runtime?: FederationRuntime
  readonly manifest?: FederationManifest
  readonly maxManifestSize?: number
  readonly maxAssetSize?: number
  readonly assetContext?: Readonly<FederationAssetExecutionContext>
  /** Present only when an isolated expose owns stylesheet placement in an open ShadowRoot. */
  readonly styleTarget?: ShadowRoot
}

export interface FederationTransport {
  fetchManifest(
    url: string,
    context: FederationTransportContext,
  ): unknown | FederationTransportManifestResult | Promise<unknown | FederationTransportManifestResult>
  loadScript(
    asset: FederationAsset,
    context: FederationTransportContext,
  ): void | WakeContainerV1 | { readonly container: WakeContainerV1 } |
    Promise<void | WakeContainerV1 | { readonly container: WakeContainerV1 }>
  loadStyle?(asset: FederationAsset, context: FederationTransportContext): Node | void | Promise<Node | void>
}

/**
 * The default browser transport bounds streamed Manifest and failure-diagnostic bodies. Native
 * module/style success requires bounded HEAD Content-Length metadata together with browser-enforced
 * SHA-384 SRI. Identity transfers require the exact Manifest size; compressed transfers use their
 * distinct bounded wire size. HEAD is not proof of the final GET origin, so CSP/CORS must constrain
 * origins too.
 */
export interface FederationRuntimeOptions {
  readonly global?: Window & typeof globalThis
  readonly mode?: FederationMode
  /** Positive safe integer, at most 300,000 milliseconds. */
  readonly timeoutMs?: number
  /** Positive safe integer, at most 16 MiB. */
  readonly maxManifestSize?: number
  /** Positive safe integer, at most 512 MiB. */
  readonly maxAssetSize?: number
  /** Positive safe integer, at most 5,000 milliseconds. */
  readonly devReconnectMs?: number
  readonly nonce?: string
  readonly transport?: FederationTransport
}

export interface FederationDecision {
  readonly specifier: string
  readonly remote?: string
  readonly expose?: string
  readonly status: 'unregistered' | 'registered' | 'manifest-loaded' | 'ready' | 'loaded' | 'error'
  readonly manifestUrl?: string
  readonly container?: Readonly<{ name: string; buildId: string; generation: number }>
  readonly cache?: Readonly<{ manifest: boolean; container: boolean; module: boolean }>
  readonly shared?: readonly Readonly<Record<string, unknown>>[]
  readonly error?: Readonly<Record<string, unknown>>
  readonly trace: readonly Readonly<Record<string, unknown>>[]
}

export interface FederationRemoteDescriptor {
  readonly specifier: string
  readonly name: string
  readonly buildId: string
  readonly generation: number
  readonly development: boolean
  readonly expose: `./${string}`
  readonly mode: ExposeMode
  readonly scope: string
  readonly shadow: 'open' | 'none'
  readonly css: readonly FederationAsset[]
}

export declare class FederationRuntime {
  constructor(options?: FederationRuntimeOptions)
  readonly runtimeAbi: 'wake.federation.v1'
  readonly mode: FederationMode

  registerRemote(definition: FederationRemoteDefinition): this
  registerRemote(name: string, definition: Omit<FederationRemoteDefinition, 'name'>): this
  registerRemote(name: string, manifestUrl: string): this
  registerContainer(registration: WakeContainerRegistration): this
  registerHostShared<T>(provider: HostSharedProvider<T>): this
  registerHostShared(providers: readonly HostSharedProvider[]): this
  registerHostShared(providers: Readonly<Record<string, Omit<HostSharedProvider, 'shareKey'>>>): this
  registerHostShared<T>(shareKey: string, provider: Omit<HostSharedProvider<T>, 'shareKey'>): this
  resolveShared<T = unknown>(request: string | SharedRequest | SharedRequirement): Promise<T>
  /** Accepts a dev update and returns the canonical frozen update object. */
  applyDevUpdate(update: unknown): Readonly<FederationDevUpdate>
  /** Attaches an isolated expose's ordered initial and lazy styles to one open ShadowRoot. */
  attachIsolatedStyleTarget(specifier: string, root: ShadowRoot): Promise<() => void>
  /** Loads an asset only from the previously accepted immutable Manifest for `request.buildId`. */
  loadFederatedAsset(request: FederatedAssetRequest): Promise<void>
  prepareRemote(name: string): Promise<Readonly<{ name: string; buildId: string; generation: number }>>
  describeRemote(specifier: string): Promise<Readonly<FederationRemoteDescriptor>>
  connectDevUpdates(name: string): Promise<boolean>
  preloadRemote(specifier: string): Promise<void>
  loadRemote<T = ModuleNamespace>(specifier: string, requester?: FederationRequesterIdentity): Promise<T>
  explain(specifier: string): FederationDecision
}

export declare function createFederationRuntime(options?: FederationRuntimeOptions): FederationRuntime
export declare const createFederationBroker: typeof createFederationRuntime
export declare function getFederationRuntime(options?: FederationRuntimeOptions): FederationRuntime
export declare const getFederationBroker: typeof getFederationRuntime
