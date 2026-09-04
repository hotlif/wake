//! Product-edge orchestration for Wake-native browser federation artifacts.
//!
//! The compiler only classifies runtime-owned requests. This module owns the I/O boundary:
//! building one multi-root container graph, binding immutable assets into a manifest, installing the
//! browser broker, and translating the production lock into host registrations.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ring::digest::{SHA384, digest};
use wake_bundler::{
    BuildGeneration, BuildOptions as BundlerBuildOptions, BuildOutput, BuildRequest,
    FederationEntryExport,
};
#[cfg(test)]
use wake_bundler::{BuildSession, FederationBuildPlan, JsxOptions, ResolveOptions};
use wake_common::{
    FileSystem, OsFileSystem, OwnedFileTree, OwnedFileTreeBuilder, ProjectedRelativePath,
};
use wake_federation_contract::{
    Asset, AssetKind, BuildId, ErrorCode as FederationErrorCode, ExposeConfig, ExposeKey,
    ExposeMode, ExposedModule, FederationLock, Manifest, PackageKey, SharedManifest, SharedOffer,
    SharedPolicy, SharedRequirement, TypeArtifact,
};
use wake_resolver::{ModuleIdentity, ResolutionEnvironment};

use super::{
    BuildOptions, CancellationToken, OutputFile, OutputFileKind, PreparedBuild, WakeError,
    absolute_from, create_bundler_options, diagnostic_infos,
};

const MANIFEST_FILE: &str = "wake-federation.json";
const LOCK_FILE: &str = "wake-federation.lock";
const BOOTSTRAP_FILE: &str = "wake-federation-bootstrap.mjs";
const SHARED_EXPOSE: &str = "./__wake_shared__";
const CONTAINER_EXPOSE: &str = "./__wake_container__";
pub(super) const HOST_EXPOSE: &str = "./__wake_host__";
const RUNTIME_SOURCE: &str = include_str!("../assets/federation-runtime.mjs");
const RUNTIME_BOOTSTRAP_SOURCE: &str = r#"
const __wake_federation_runtime_options_symbol__=Symbol.for('wake.federation.runtime-options.v1');
const __wake_federation_nonce_pattern__=/^[A-Za-z0-9+/_-]+={0,2}$/u;
function __wake_federation_runtime_options_error__(message){const error=new TypeError(message);error.code='FED_CONFIG_INVALID';return error}
function __wake_federation_runtime_options__(){
  const explicitDescriptor=Object.getOwnPropertyDescriptor(globalThis,__wake_federation_runtime_options_symbol__);
  let nonce;
  if(explicitDescriptor!==undefined){
    if(!Object.prototype.hasOwnProperty.call(explicitDescriptor,'value'))throw __wake_federation_runtime_options_error__('Wake Federation runtime options symbol must be a data property');
    const explicit=explicitDescriptor.value;
    if(explicit===null||typeof explicit!=='object'||Array.isArray(explicit))throw __wake_federation_runtime_options_error__('Wake Federation runtime options must be a plain object containing only nonce');
    const prototype=Object.getPrototypeOf(explicit);
    const keys=Reflect.ownKeys(explicit);
    const descriptor=Object.getOwnPropertyDescriptor(explicit,'nonce');
    if((prototype!==Object.prototype&&prototype!==null)||keys.length!==1||keys[0]!=='nonce'||descriptor===undefined||!Object.prototype.hasOwnProperty.call(descriptor,'value'))throw __wake_federation_runtime_options_error__('Wake Federation runtime options must contain only a nonce data property');
    nonce=descriptor.value;
    if(typeof nonce!=='string'||!__wake_federation_nonce_pattern__.test(nonce))throw __wake_federation_runtime_options_error__('Wake Federation runtime nonce must be a non-empty CSP base64 value');
  }else if(typeof document!=='undefined'&&typeof document.querySelectorAll==='function'){
    for(const script of document.querySelectorAll('script[type="module"][src]')){
      if(script.src!==import.meta.url)continue;
      const candidate=script.nonce;
      if(candidate==='')break;
      if(typeof candidate!=='string'||!__wake_federation_nonce_pattern__.test(candidate))throw __wake_federation_runtime_options_error__('Wake Federation bootstrap script has an invalid CSP nonce');
      nonce=candidate;
      break;
    }
  }
  return nonce===undefined?{global:globalThis}:{global:globalThis,nonce};
}
const __wake_federation_broker__=getFederationRuntime(__wake_federation_runtime_options__());
"#;
const DEV_APP_ALIAS: &str = "@@@/__wake_federation_app_entry";
const DEV_STANDALONE_ALIAS: &str = "@@@/__wake_federation_standalone_entry";
const APPLICATION_LOADER_EXPORT: &str = "__wakeApp";
const SHARED_LOADER_EXPORT: &str = "__wakeShared";

pub(super) struct DevFederationSetup {
    pub(super) entry: PathBuf,
    pub(super) aliases: Vec<(String, PathBuf)>,
    pub(super) build: wake_dev_server::FederationBuildOptions,
    pub(super) generated_inputs: OwnedFileTree,
}

#[derive(Default)]
pub(super) struct FederationArtifacts {
    files: Vec<GeneratedFile>,
    hidden_source_maps: Vec<GeneratedFile>,
    pub(super) bootstrap_file: Option<String>,
    pub(super) module_count: usize,
    pub(super) updated_module_count: usize,
    pub(super) cached_module_count: usize,
}

impl FederationArtifacts {
    pub(super) fn output_files(&self) -> impl Iterator<Item = OutputFile> + '_ {
        self.files.iter().map(|file| OutputFile {
            path: file.path.clone(),
            kind: file.kind,
            bytes: file.bytes.len(),
        })
    }

    pub(super) fn write_public_to(&self, outdir: &Path) -> Result<(), WakeError> {
        for file in &self.files {
            let path = super::output_file_path(outdir, &file.path)?;
            super::atomic_write(&path, &file.bytes)?;
        }
        Ok(())
    }

    pub(super) fn write_hidden_source_maps_to(&self, project_root: &Path) -> Result<(), WakeError> {
        for file in &self.hidden_source_maps {
            let root = project_root.join(".wake");
            let path = super::output_file_path(&root, &file.path)?;
            super::atomic_write(&path, &file.bytes)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProductionSourceMapMode {
    Hidden,
    Public,
}

impl ProductionSourceMapMode {
    fn from_public(public: bool) -> Self {
        if public { Self::Public } else { Self::Hidden }
    }

    fn is_public(self) -> bool {
        self == Self::Public
    }
}

/// Render and bind the generated inputs for a retained development `BuildSession`.
///
/// Shared providers are first compiled into one self-contained module. The retained session then
/// imports that provider before resolving/fixing the host share context and only afterwards loads
/// the real application entry. This keeps application imports asynchronous without introducing a
/// second long-lived bundler path or evaluating a shared consumer before its context exists.
pub(super) fn prepare_dev(
    prepared: &PreparedBuild,
    _options: &BuildOptions,
    fs: Arc<dyn FileSystem>,
    captured_lock: Option<Arc<FederationLock>>,
) -> Result<DevFederationSetup, WakeError> {
    let federation = &prepared.config.federation;
    if !federation.enabled {
        return Ok(DevFederationSetup {
            entry: prepared.entry.clone(),
            aliases: Vec::new(),
            build: wake_dev_server::FederationBuildOptions::default(),
            generated_inputs: OwnedFileTreeBuilder::new().seal(),
        });
    }
    validate_configured_shared_owners(federation)?;

    let shared = resolve_shared_descriptors(prepared, true, Arc::clone(&fs))?;
    let mut generated_inputs = OwnedFileTreeBuilder::new();
    let mut aliases = Vec::new();
    let application = if shared.is_empty() {
        prepared.entry.clone()
    } else {
        let wrapper = development_host_entry(prepared, &shared);
        let wrapper_path = projected_federation_path(
            PathBuf::from("federation/generated")
                .join(format!("dev-host-{}.mjs", short_hash(wrapper.as_bytes()))),
        )?;
        insert_federation_input(
            &mut generated_inputs,
            wrapper_path.clone(),
            wrapper.as_bytes(),
        )?;
        aliases.push((DEV_APP_ALIAS.to_owned(), prepared.entry.clone()));
        logical_federation_path(prepared, &wrapper_path)
    };
    let synthetic = prepare_synthetic_container_entry_with_application(
        prepared,
        Some(&application),
        &shared,
        &mut generated_inputs,
    )?;
    aliases.extend(synthetic.aliases.clone());

    let expose_loaders = synthetic
        .exposes
        .iter()
        .map(|expose| (expose.key.clone(), expose.loader_export.clone()))
        .collect::<BTreeMap<_, _>>();
    let remote_entry_template = remote_entry_protocol_source(
        federation.name.as_str(),
        wake_dev_server::FEDERATION_BUILD_ID_PLACEHOLDER,
        &expose_loaders,
        &shared,
        synthetic
            .shared
            .as_ref()
            .map(|entry| entry.loader_export.as_str()),
    );
    let root = prepared.root.clone();
    let container = federation.name.clone();
    let type_entries = type_entries(prepared);
    let shared_fallback_root = synthetic.shared.as_ref().map(|entry| entry.entry.clone());
    let type_emitter = (!type_entries.is_empty()).then(|| {
        wake_dev_server::FederationTypeEmitter::new(move |generation_fs| {
            let file_system =
                super::federation_types::GenerationDeclarationFileSystem::new(generation_fs);
            let prepared = Arc::new(
                super::federation_types::prepare_federation_types_with_file_system(
                    &root,
                    &container,
                    &type_entries,
                    &file_system,
                )
                .map_err(|error| error.to_string())?,
            );
            let identity_bytes = prepared.identity_bytes().to_vec();
            Ok(wake_dev_server::FederationTypeGeneration::new(
                identity_bytes,
                move |build_id| {
                    prepared
                        .bind(build_id)
                        .map(|output| wake_dev_server::FederationTypeOutput {
                            bundle_json: output.bundle_json,
                            ambient_declaration: output.ambient_declaration,
                        })
                        .map_err(|error| error.to_string())
                },
            ))
        })
    });

    Ok(DevFederationSetup {
        entry: synthetic.entry,
        aliases,
        build: wake_dev_server::FederationBuildOptions {
            enabled: true,
            container_name: federation.name.as_str().to_owned(),
            remotes: federation
                .remotes
                .keys()
                .map(|name| name.as_str().to_owned())
                .collect(),
            shared: shared_mappings(prepared),
            entry_export: None,
            bootstrap: Some(development_bootstrap_source(
                prepared,
                captured_lock.as_deref(),
            )?),
            browser_target: browser_target(prepared)?,
            exposes: synthetic
                .exposes
                .iter()
                .map(|expose| wake_dev_server::FederationExposeBuild {
                    key: expose.key.clone(),
                    mode: expose.config.mode,
                    scope: expose.config.scope.clone(),
                    shadow: expose.config.shadow,
                    allow_global_css: expose.config.allow_global_css,
                    chunk_name: expose.chunk_name.clone(),
                    loader_export: expose.loader_export.clone(),
                })
                .collect(),
            shared_manifest: shared_manifest(federation, &shared, None),
            shared_fallback: synthetic
                .shared
                .map(|entry| wake_dev_server::FederationSharedBuild {
                    chunk_name: entry.chunk_name,
                    loader_export: entry.loader_export,
                }),
            shared_fallback_root,
            application_loader_export: Some(APPLICATION_LOADER_EXPORT.to_owned()),
            remote_entry_template: Some(remote_entry_template),
            type_emitter,
        },
        generated_inputs: generated_inputs.seal(),
    })
}

fn development_bootstrap_source(
    prepared: &PreparedBuild,
    lock: Option<&FederationLock>,
) -> Result<String, WakeError> {
    let federation = &prepared.config.federation;
    let mut source = String::with_capacity(RUNTIME_SOURCE.len() + 2048);
    source.push_str(RUNTIME_SOURCE);
    source.push_str(RUNTIME_BOOTSTRAP_SOURCE);
    for (name, remote) in &federation.remotes {
        let mut registration = serde_json::Map::new();
        registration.insert("name".to_owned(), serde_json::json!(name.as_str()));
        registration.insert(
            "manifestUrl".to_owned(),
            serde_json::json!(remote.manifest_url),
        );
        registration.insert(
            "mode".to_owned(),
            serde_json::json!(if remote.dev_follow {
                "development"
            } else {
                "production"
            }),
        );
        if !remote.allowed_origins.is_empty() {
            registration.insert(
                "allowedOrigins".to_owned(),
                serde_json::json!(remote.allowed_origins),
            );
        }
        if !remote.dev_follow {
            let locked = lock
                .and_then(|lock| lock.remotes.get(name))
                .ok_or_else(|| {
                    WakeError::new(
                        "FED_LOCK_MISMATCH",
                        format!("development-pinned remote `{name}` is missing from the lock"),
                    )
                })?;
            registration.insert(
                "lock".to_owned(),
                serde_json::to_value(locked).expect("remote lock serialization"),
            );
        }
        source.push_str("__wake_federation_broker__.registerRemote(");
        source.push_str(
            &serde_json::to_string(&registration).expect("remote registration serialization"),
        );
        source.push_str(");\n");
    }
    // Register the local development container as a followed remote as well. The dev coordinator
    // can then apply an isolated remount update to the preview page instead of treating its own
    // container as unknown and forcing an unconditional full reload.
    source.push_str("__wake_federation_broker__.registerRemote({name:");
    source.push_str(&serde_json::to_string(federation.name.as_str()).unwrap());
    source.push_str(",manifestUrl:new URL('../../wake-federation.json',import.meta.url).href,mode:'development'});\n");
    Ok(source)
}

fn development_host_entry(prepared: &PreparedBuild, shared: &[SharedDescriptor]) -> String {
    let federation = &prepared.config.federation;
    let name = serde_json::to_string(federation.name.as_str()).unwrap();
    let requester = serde_json::to_string(&format!("{}\0$host", federation.name)).unwrap();
    let mut source = format!(
        "const __wake_build_id__=__wake__?.federation?.buildId;\nif(typeof __wake_build_id__!=='string')throw Object.assign(new Error('Wake Federation build context is missing'),{{code:'FED_RUNTIME_ABI'}});\nconst __wake_shared_registry__=globalThis[Symbol.for('wake.federation.exposes.v1')];\nconst __wake_shared_loader__=__wake_shared_registry__?.[{name}]?.[__wake_build_id__]?.['./__wake_container__']?.[{loader}];\nif(typeof __wake_shared_loader__!=='function')throw Object.assign(new Error('Wake host shared fallback loader is unavailable'),{{code:'FED_CONTAINER_GET'}});\nconst __wake_local_shared__=await __wake_shared_loader__();\nconst __wake_broker__=globalThis[Symbol.for('wake.federation.v1')];\nif(!__wake_broker__)throw Object.assign(new Error('Wake Federation broker is not installed'),{{code:'FED_RUNTIME_ABI'}});\n",
        loader = serde_json::to_string(SHARED_LOADER_EXPORT).unwrap(),
    );
    for descriptor in shared {
        let provider = serde_json::json!({
            "shareKey": descriptor.share_key,
            "version": descriptor.package.version,
            "scope": descriptor.config.scope,
            "singleton": descriptor.config.singleton,
            "strict": descriptor.config.strict,
            "packageContext": descriptor.package.package_context,
            "buildVariant": descriptor.package.build_variant,
            "coherenceGroup": descriptor.config.coherence_group,
            "fallback": descriptor.config.fallback,
            "owner": federation.name.as_str(),
        });
        source.push_str("__wake_broker__.registerHostShared(Object.assign(");
        source.push_str(&provider.to_string());
        source.push_str(",{module:__wake_local_shared__[");
        source.push_str(&serde_json::to_string(&descriptor.export_name).unwrap());
        source.push_str("]}));\n");
    }
    source.push_str("await Promise.all([");
    for (index, owner) in remote_singleton_owners(federation, shared)
        .iter()
        .enumerate()
    {
        if index != 0 {
            source.push(',');
        }
        source.push_str("__wake_broker__.prepareRemote(");
        source.push_str(&serde_json::to_string(owner).unwrap());
        source.push(')');
    }
    source
        .push_str("]);\nconst __wake_resolved_shared__=Object.create(null);\nawait Promise.all([");
    for (index, descriptor) in shared.iter().enumerate() {
        if index != 0 {
            source.push(',');
        }
        let requirement = serde_json::json!({
            "shareKey": descriptor.share_key,
            "requiredVersion": descriptor.config.required_version.as_deref().unwrap_or(&descriptor.package.version),
            "packageContext": descriptor.package.package_context,
            "buildVariant": descriptor.package.build_variant,
            "scope": descriptor.config.scope,
            "singleton": descriptor.config.singleton,
            "strict": descriptor.config.strict,
            "coherenceGroup": descriptor.config.coherence_group,
            "fallback": descriptor.config.fallback,
            "owner": descriptor.config.owner.as_ref().map(wake_federation_contract::ContainerName::as_str),
        });
        let key = serde_json::to_string(&format!(
            "{}:{}",
            descriptor.config.scope, descriptor.share_key
        ))
        .unwrap();
        source.push_str("__wake_broker__.resolveShared(");
        source.push_str(&requirement.to_string());
        source.push_str(",{requester:");
        source.push_str(&requester);
        source.push_str("}).then(value=>{__wake_resolved_shared__[");
        source.push_str(&key);
        source.push_str("]=value})");
    }
    source.push_str("]);\nconst __wake_context__=Object.freeze({runtimeAbi:'wake.federation.v1',container:Object.freeze({name:");
    source.push_str(&name);
    source.push_str(",buildId:__wake_build_id__}),resolved:Object.freeze(__wake_resolved_shared__),resolve:(request)=>__wake_broker__.resolveShared(request,{requester:");
    source.push_str(&requester);
    source.push_str("}),getSync:(shareKey,scope='default')=>{const key=scope+':'+shareKey;if(!Object.prototype.hasOwnProperty.call(__wake_resolved_shared__,key))throw Object.assign(new Error('Wake host shared dependency was not initialized'),{code:'FED_SHARE_UNSATISFIABLE',details:{shareKey,scope}});return __wake_resolved_shared__[key]}});let contexts=globalThis[Symbol.for('wake.federation.share-contexts.v1')];if(!contexts){contexts=Object.create(null);Object.defineProperty(globalThis,Symbol.for('wake.federation.share-contexts.v1'),{value:contexts,configurable:false})}const containerContexts=contexts[");
    source.push_str(&name);
    source.push_str("]||(contexts[");
    source.push_str(&name);
    source.push_str("]=Object.create(null));const previous=containerContexts[__wake_build_id__];if(previous!==undefined&&previous!==__wake_context__)throw Object.assign(new Error('Wake host build already owns another share context'),{code:'FED_CONTAINER_INIT'});containerContexts[__wake_build_id__]=__wake_context__;await import(");
    source.push_str(&serde_json::to_string(DEV_APP_ALIAS).unwrap());
    source.push_str(");\n");
    source
}

struct GeneratedFile {
    path: String,
    bytes: Vec<u8>,
    kind: OutputFileKind,
}

struct BuiltExposeRoot {
    config: ExposeConfig,
    chunk_name: String,
    loader_export: String,
}

struct BuiltContainer {
    directory: String,
    output: BuildOutput,
    exposes: BTreeMap<ExposeKey, BuiltExposeRoot>,
}

struct BuiltShared {
    directory: String,
    output: BuildOutput,
}

/// Immutable, unbound Federation sources for one production configuration.
///
/// The files are rooted at the project's logical `.wake` tree. Rendering owns no filesystem and
/// therefore cannot observe or mutate a physical generation namespace.
pub(super) struct ProductionFederationInputs {
    files: OwnedFileTree,
    plan: Option<UnboundProductionFederationPlan>,
}

impl ProductionFederationInputs {
    pub(super) fn files(&self) -> &OwnedFileTree {
        &self.files
    }
}

struct UnboundProductionFederationPlan {
    source_map_mode: ProductionSourceMapMode,
    shared_slots: Vec<UnboundSharedSlot>,
    container: UnboundContainerBuild,
    shared_provider: Option<UnboundSharedBuild>,
}

struct UnboundSharedSlot {
    share_key: String,
    export_name: String,
}

struct UnboundContainerBuild {
    entry: ProjectedRelativePath,
    exposes: Vec<UnboundExposeRoot>,
}

struct UnboundExposeRoot {
    key: ExposeKey,
    config: ExposeConfig,
    configured_entry: PathBuf,
    alias: String,
    chunk_name: String,
    loader_export: String,
}

struct UnboundSharedBuild {
    entry: ProjectedRelativePath,
}

/// Generation-bound Federation facts shared by every production compilation view.
///
/// Synthetic sources were sealed by [`ProductionFederationInputs`] before this value was bound.
/// Package identities and the remote lock are captured through the generation filesystem, so the
/// application, container, provider, and bootstrap consume one stable observation set without
/// collapsing their deliberately different bundler profiles.
pub(super) struct PreparedFederationGeneration {
    source_map_mode: ProductionSourceMapMode,
    shared: Vec<SharedDescriptor>,
    container: PreparedContainerBuild,
    shared_provider: Option<PreparedSharedBuild>,
    types: Option<super::federation_types::FrozenFederationTypes>,
    lock: Option<Arc<FederationLock>>,
}

struct PreparedContainerBuild {
    directory: String,
    entry: PathBuf,
    options: BundlerBuildOptions,
    exposes: BTreeMap<ExposeKey, BuiltExposeRoot>,
}

struct PreparedSharedBuild {
    directory: String,
    entry: PathBuf,
    options: BundlerBuildOptions,
}

struct MaterializedContainer {
    exposes: BTreeMap<ExposeKey, ExposedModule>,
    files: Vec<GeneratedFile>,
    hidden_source_maps: Vec<GeneratedFile>,
}

#[derive(Clone)]
struct SharedDescriptor {
    share_key: String,
    export_name: String,
    source: PathBuf,
    package: PackageKey,
    config: wake_federation_contract::SharedConfig,
}

#[derive(Clone)]
struct AssetBytes {
    asset: Asset,
    path: String,
}

pub(super) fn render_production_inputs(
    prepared: &PreparedBuild,
    options: &BuildOptions,
) -> Result<ProductionFederationInputs, WakeError> {
    let config = &prepared.config.federation;
    if !config.enabled {
        return Ok(ProductionFederationInputs {
            files: OwnedFileTreeBuilder::new().seal(),
            plan: None,
        });
    }

    validate_production_shared_policy(config)?;
    let mut files = OwnedFileTreeBuilder::new();
    let shared_slots = config
        .shared
        .keys()
        .enumerate()
        .map(|(index, share_key)| UnboundSharedSlot {
            share_key: share_key.clone(),
            export_name: format!("s{index}"),
        })
        .collect::<Vec<_>>();
    let shared_provider = (!shared_slots.is_empty())
        .then(|| render_production_shared_provider(&shared_slots, &mut files))
        .transpose()?;
    let container = render_production_container(prepared, &mut files)?;
    Ok(ProductionFederationInputs {
        files: files.seal(),
        plan: Some(UnboundProductionFederationPlan {
            source_map_mode: ProductionSourceMapMode::from_public(options.source_map),
            shared_slots,
            container,
            shared_provider,
        }),
    })
}

pub(super) fn bind_production_generation(
    prepared: &PreparedBuild,
    options: &BuildOptions,
    inputs: &ProductionFederationInputs,
    generation_fs: Arc<dyn FileSystem>,
) -> Result<Option<PreparedFederationGeneration>, WakeError> {
    let Some(plan) = &inputs.plan else {
        return Ok(None);
    };
    let shared = resolve_shared_descriptors(prepared, false, Arc::clone(&generation_fs))?;
    validate_bound_shared_slots(&plan.shared_slots, &shared)?;
    let shared_provider = plan
        .shared_provider
        .as_ref()
        .map(|provider| bind_shared_provider_build(prepared, options, provider, &shared, true))
        .transpose()?;
    let container = bind_container_build(prepared, options, &plan.container)?;
    let types = prepare_federation_types(prepared, Arc::clone(&generation_fs))?;
    let lock = load_production_lock_from_fs(prepared, generation_fs)?;
    Ok(Some(PreparedFederationGeneration {
        source_map_mode: plan.source_map_mode,
        shared,
        container,
        shared_provider,
        types,
        lock,
    }))
}

fn validate_bound_shared_slots(
    slots: &[UnboundSharedSlot],
    shared: &[SharedDescriptor],
) -> Result<(), WakeError> {
    let matches = slots.len() == shared.len()
        && slots.iter().zip(shared).all(|(slot, descriptor)| {
            slot.share_key == descriptor.share_key && slot.export_name == descriptor.export_name
        });
    if !matches {
        return Err(WakeError::new(
            "WAKE_INTERNAL",
            "rendered Federation shared slots do not match the bound configuration",
        ));
    }
    Ok(())
}

fn projected_federation_path(path: PathBuf) -> Result<ProjectedRelativePath, WakeError> {
    ProjectedRelativePath::new(&path).map_err(|error| {
        WakeError::new(
            "WAKE_INTERNAL",
            format!("generated Federation input has an invalid path: {error}"),
        )
    })
}

fn insert_federation_input(
    files: &mut OwnedFileTreeBuilder,
    path: ProjectedRelativePath,
    contents: &[u8],
) -> Result<(), WakeError> {
    files
        .insert(path.clone(), contents.to_vec())
        .map_err(|error| {
            WakeError::new(
                "WAKE_INTERNAL",
                format!("generated Federation input inventory is invalid: {error}"),
            )
        })
}

fn logical_federation_path(prepared: &PreparedBuild, relative: &ProjectedRelativePath) -> PathBuf {
    wake_common::fs::normalize(&prepared.root.join(".wake").join(relative.as_path()))
}

pub(super) fn build_artifacts(
    prepared: &PreparedBuild,
    application: &BuildOutput,
    prepared_generation: Option<PreparedFederationGeneration>,
    generation: &mut BuildGeneration,
    cancellation: &CancellationToken,
) -> Result<FederationArtifacts, WakeError> {
    let Some(prepared_generation) = prepared_generation else {
        return Ok(FederationArtifacts::default());
    };
    let PreparedFederationGeneration {
        source_map_mode,
        shared,
        container,
        shared_provider,
        types,
        lock,
    } = prepared_generation;
    let config = &prepared.config.federation;

    cancellation.check()?;
    let built_shared = shared_provider
        .map(|provider| build_shared_provider(prepared, provider, generation))
        .transpose()?;
    cancellation.check()?;
    let container = build_container(prepared, container, generation)?;
    cancellation.check()?;

    let mut federation_module_count = container.output.module_count;
    let mut federation_updated_module_count = container.output.updated_module_count;
    let mut federation_cached_module_count = container.output.cached_module_count;
    if let Some(provider) = &built_shared {
        federation_module_count += provider.output.module_count;
        federation_updated_module_count += provider.output.updated_module_count;
        federation_cached_module_count += provider.output.cached_module_count;
    }

    let browser_target = browser_target(prepared)?;
    let mut identity_inputs = Vec::<(String, Vec<u8>)>::new();
    collect_output_identity(
        &container.directory,
        &container.output,
        &mut identity_inputs,
    );
    if let Some(provider) = &built_shared {
        collect_output_identity(&provider.directory, &provider.output, &mut identity_inputs);
    }
    identity_inputs.push((
        "federation-policy.json".to_owned(),
        canonical_federation_identity_bytes(config, &shared, &browser_target),
    ));
    if let Some(types) = &types {
        identity_inputs.push((
            "federation-types.json".to_owned(),
            types.identity_bytes().to_vec(),
        ));
    }
    let build_id = build_id_from_identity_inputs(&identity_inputs);

    let mut generated = Vec::new();
    let MaterializedContainer {
        exposes: manifest_exposes,
        files: mut container_files,
        mut hidden_source_maps,
    } = materialize_container(config.name.as_str(), &build_id, &container, source_map_mode)?;
    generated.append(&mut container_files);

    let (shared_manifest, shared_asset, mut shared_files, mut hidden_shared_maps) =
        match built_shared {
            Some(provider) => {
                let (asset, files, hidden_maps) =
                    materialize_shared(config.name.as_str(), &build_id, provider, source_map_mode)?;
                (
                    shared_manifest(config, &shared, Some(&asset.asset)),
                    Some(asset),
                    files,
                    hidden_maps,
                )
            }
            None => (
                shared_manifest(config, &shared, None),
                None,
                Vec::new(),
                Vec::new(),
            ),
        };
    generated.append(&mut shared_files);
    hidden_source_maps.append(&mut hidden_shared_maps);

    let remote_entry_source = remote_entry_source(
        config.name.as_str(),
        build_id.as_str(),
        &container.output.entry().code,
        &container.exposes,
        &shared,
    );
    let remote_entry_name = format!(
        "federation/{}/{}/remoteEntry.{}.mjs",
        config.name,
        build_id,
        short_hash(remote_entry_source.as_bytes())
    );
    let remote_entry = asset_for(
        AssetKind::JavaScript,
        format!("./{remote_entry_name}"),
        remote_entry_source.as_bytes(),
        "text/javascript",
    );
    generated.push(GeneratedFile {
        path: remote_entry_name.clone(),
        bytes: remote_entry_source.into_bytes(),
        kind: OutputFileKind::FederationEntry,
    });

    let mut manifest = Manifest::new(
        config.name.clone(),
        build_id.clone(),
        browser_target,
        remote_entry,
    );
    manifest.exposes = manifest_exposes;
    manifest.shared = shared_manifest;
    if let Some(source_map) = &container.output.entry().source_map {
        let rewritten = rewrite_remote_source_map(
            source_map,
            config.name.as_str(),
            build_id.as_str(),
            &remote_entry_name,
        )?;
        let map_path = format!("{remote_entry_name}.map");
        let map_bytes = rewritten.into_bytes();
        if source_map_mode.is_public() {
            manifest.remote_entry_source_map = Some(asset_for(
                AssetKind::SourceMap,
                format!("./{map_path}"),
                &map_bytes,
                "application/source-map+json",
            ));
            generated.push(GeneratedFile {
                path: map_path,
                bytes: map_bytes,
                kind: OutputFileKind::SourceMap,
            });
        } else {
            let file_name = remote_entry_name
                .rsplit('/')
                .next()
                .expect("remote entry has a file name");
            hidden_source_maps.push(GeneratedFile {
                path: hidden_source_map_path(
                    config.name.as_str(),
                    &build_id,
                    &format!("{file_name}.map"),
                ),
                bytes: map_bytes,
                kind: OutputFileKind::SourceMap,
            });
        }
    }

    let type_artifact = materialize_type_artifact(prepared, &build_id, types)?;
    if let Some((artifact, mut files)) = type_artifact {
        manifest.types = Some(artifact);
        generated.append(&mut files);
    }

    manifest.validate().map_err(|error| {
        WakeError::new(
            "FED_MANIFEST_SCHEMA",
            format!("generated federation manifest is invalid: {error}"),
        )
    })?;
    let manifest_bytes = serde_json::to_vec_pretty(&manifest).map_err(|error| {
        WakeError::new(
            "WAKE_INTERNAL",
            format!("could not serialize federation manifest: {error}"),
        )
    })?;
    generated.push(GeneratedFile {
        path: MANIFEST_FILE.to_owned(),
        bytes: manifest_bytes,
        kind: OutputFileKind::FederationManifest,
    });

    let bootstrap = bootstrap_source(
        prepared,
        application.entry().file_name.as_str(),
        &build_id,
        &shared,
        shared_asset.as_ref(),
        lock.as_deref(),
    )?;
    generated.push(GeneratedFile {
        path: BOOTSTRAP_FILE.to_owned(),
        bytes: bootstrap.into_bytes(),
        kind: OutputFileKind::FederationBootstrap,
    });

    let artifacts = FederationArtifacts {
        files: generated,
        hidden_source_maps,
        bootstrap_file: Some(BOOTSTRAP_FILE.to_owned()),
        module_count: federation_module_count,
        updated_module_count: federation_updated_module_count,
        cached_module_count: federation_cached_module_count,
    };
    Ok(artifacts)
}

fn validate_production_shared_policy(
    config: &wake_federation_contract::FederationConfig,
) -> Result<(), WakeError> {
    validate_configured_shared_owners(config)?;
    for (share_key, shared) in &config.shared {
        if shared.singleton && shared.owner.is_none() {
            return Err(WakeError::new(
                "FED_CONFIG_INVALID",
                format!(
                    "production singleton `{share_key}` requires an explicit owner to avoid navigation-order selection"
                ),
            ));
        }
    }
    Ok(())
}

fn validate_configured_shared_owners(
    config: &wake_federation_contract::FederationConfig,
) -> Result<(), WakeError> {
    for (share_key, shared) in &config.shared {
        if let Some(owner) = &shared.owner
            && owner != &config.name
            && !config.remotes.contains_key(owner)
        {
            return Err(WakeError::new(
                "FED_CONFIG_INVALID",
                format!(
                    "singleton owner `{owner}` for `{share_key}` is neither this host nor a configured remote"
                ),
            ));
        }
    }
    Ok(())
}

fn resolve_shared_descriptors(
    prepared: &PreparedBuild,
    development: bool,
    fs: Arc<dyn FileSystem>,
) -> Result<Vec<SharedDescriptor>, WakeError> {
    let config = &prepared.config.federation;
    if config.shared.is_empty() {
        return Ok(Vec::new());
    }
    let environment = ResolutionEnvironment::with_options(
        fs,
        wake_resolver::ResolveOptions {
            alias: prepared.aliases.clone(),
            conditions: shared_resolution_conditions(development),
            ..wake_resolver::ResolveOptions::default()
        },
    );
    let resolver = environment.resolver();
    let build_variant = shared_build_variant(prepared, development)?;
    let mut descriptors = Vec::with_capacity(config.shared.len());
    for (index, (share_key, shared)) in config.shared.iter().enumerate() {
        let resolved = resolver
            .resolve_module(share_key, &prepared.root)
            .map_err(|error| {
                WakeError::new(
                    "FED_SHARE_UNSATISFIABLE",
                    format!("cannot resolve shared dependency `{share_key}`: {error}"),
                )
                .at(&prepared.root)
            })?;
        let ModuleIdentity::Package { package, .. } = resolved.identity else {
            return Err(WakeError::new(
                "FED_CONFIG_INVALID",
                format!(
                    "shared dependency `{share_key}` must resolve to a package identity, not a project file"
                ),
            ));
        };
        descriptors.push(SharedDescriptor {
            share_key: share_key.clone(),
            export_name: format!("s{index}"),
            source: wake_common::fs::normalize(&resolved.path),
            package: PackageKey {
                name: package.name,
                version: package.version,
                package_context: package.context.unwrap_or_else(|| "default".to_owned()),
                build_variant: build_variant.clone(),
            },
            config: shared.clone(),
        });
    }
    Ok(descriptors)
}

fn shared_resolution_conditions(development: bool) -> Vec<String> {
    if development {
        ["browser", "development", "import", "module", "default"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    } else {
        wake_bundler::ResolveOptions::default().conditions
    }
}

fn shared_build_variant(prepared: &PreparedBuild, development: bool) -> Result<String, WakeError> {
    let mut transform_include = prepared.config.transforms.include.clone();
    let mut transform_exclude = prepared.config.transforms.exclude.clone();
    transform_include.sort();
    transform_include.dedup();
    transform_exclude.sort();
    transform_exclude.dedup();
    let identity = serde_json::json!({
        "runtimeAbi": wake_federation_contract::FEDERATION_RUNTIME_ABI,
        "browserTarget": browser_target(prepared)?,
        "resolutionConditions": shared_resolution_conditions(development),
        "defines": super::build_defines(&prepared.config, development),
        "typescript": {
            "enabled": prepared.config.typescript.enabled,
            "onlyRemoveTypeImports": prepared.config.typescript.only_remove_type_imports,
        },
        "jsx": {
            "enabled": prepared.config.react.enabled,
            "importSource": prepared.config.react.jsx_import_source,
            "developmentRuntime": development,
        },
        "transforms": {
            "include": transform_include,
            "exclude": transform_exclude,
        },
        "emit": {
            "platform": "browser",
            "format": "wake-iife",
            "treeShaking": !development,
            "deadModuleElimination": !development,
            "minify": !development,
            "cssInJs": true,
        },
    });
    let bytes = serde_json::to_vec(&identity).expect("shared build variant serialization");
    Ok(format!("wake-browser-{}", &digest_bytes(&bytes).0[..24]))
}

#[derive(Clone)]
pub(super) struct SyntheticExposeEntry {
    pub(super) key: ExposeKey,
    pub(super) config: ExposeConfig,
    pub(super) chunk_name: String,
    pub(super) loader_export: String,
}

pub(super) struct SyntheticContainerEntry {
    pub(super) entry: PathBuf,
    pub(super) aliases: Vec<(String, PathBuf)>,
    pub(super) exposes: Vec<SyntheticExposeEntry>,
    pub(super) shared: Option<SyntheticSharedEntry>,
}

#[derive(Clone)]
pub(super) struct SyntheticSharedEntry {
    pub(super) chunk_name: String,
    pub(super) loader_export: String,
    pub(super) entry: PathBuf,
}

fn prepare_synthetic_container_entry_with_application(
    prepared: &PreparedBuild,
    application: Option<&Path>,
    shared: &[SharedDescriptor],
    generated_inputs: &mut OwnedFileTreeBuilder,
) -> Result<SyntheticContainerEntry, WakeError> {
    let generated_relative = PathBuf::from("federation/generated/container");

    let mut source = String::new();
    let mut aliases = Vec::new();
    let mut exposes = Vec::with_capacity(prepared.config.federation.exposes.len());
    if let Some(application) = application {
        aliases.push((DEV_STANDALONE_ALIAS.to_owned(), application.to_path_buf()));
        source.push_str("export const ");
        source.push_str(APPLICATION_LOADER_EXPORT);
        source.push_str("=()=>import(");
        source.push_str(
            &serde_json::to_string(DEV_STANDALONE_ALIAS)
                .expect("synthetic application serialization"),
        );
        source.push_str(");\n");
    }
    let shared_entry = if shared.is_empty() {
        None
    } else {
        let chunk_name = "shared-fallback".to_owned();
        let wrapper_file = format!("{chunk_name}.mjs");
        let wrapper_path = projected_federation_path(generated_relative.join(&wrapper_file))?;
        let mut wrapper = String::new();
        for (index, descriptor) in shared.iter().enumerate() {
            let alias = format!("@@@/__wake_federation_shared_{index:04}");
            wrapper.push_str("import * as ");
            wrapper.push_str(&descriptor.export_name);
            wrapper.push_str(" from ");
            wrapper.push_str(&serde_json::to_string(&alias).unwrap());
            wrapper.push_str(";\n");
            aliases.push((alias, descriptor.source.clone()));
        }
        wrapper.push_str("export {");
        for (index, descriptor) in shared.iter().enumerate() {
            if index != 0 {
                wrapper.push(',');
            }
            wrapper.push_str(&descriptor.export_name);
        }
        wrapper.push_str("};\n");
        insert_federation_input(generated_inputs, wrapper_path.clone(), wrapper.as_bytes())?;
        let loader_export = SHARED_LOADER_EXPORT.to_owned();
        source.push_str("export const __wakeShared=()=>import(");
        source.push_str(&serde_json::to_string(&format!("./{wrapper_file}")).unwrap());
        source.push_str(");\n");
        Some(SyntheticSharedEntry {
            chunk_name,
            loader_export,
            entry: logical_federation_path(prepared, &wrapper_path),
        })
    };
    for (index, (key, expose)) in prepared.config.federation.exposes.iter().enumerate() {
        let configured = absolute_from(&prepared.root, Path::new(&expose.entry));
        let entry = configured.canonicalize().map_err(|error| {
            WakeError::new(
                "WAKE_IO",
                format!("cannot resolve federation expose `{key}`: {error}"),
            )
            .at(&configured)
        })?;
        let entry = wake_common::fs::normalize(&entry);
        if !entry.starts_with(&prepared.root) {
            return Err(WakeError::new(
                "FED_CONFIG_INVALID",
                format!("federation expose `{key}` resolves outside the project root"),
            )
            .at(&entry));
        }

        let safe = safe_expose_name(key.as_str());
        let wrapper_stem = format!("expose-{index:04}-{safe}");
        let wrapper_file = format!("{wrapper_stem}.mjs");
        let alias = format!("@@@/__wake_federation_expose_{index:04}");
        let wrapper = format!(
            "import * as namespace from {};\nexport default namespace;\n",
            serde_json::to_string(&alias).expect("synthetic expose alias serialization")
        );
        let wrapper_path = projected_federation_path(generated_relative.join(&wrapper_file))?;
        insert_federation_input(generated_inputs, wrapper_path, wrapper.as_bytes())?;
        aliases.push((alias, entry));

        let loader_export = format!("e{index}");
        source.push_str("export const ");
        source.push_str(&loader_export);
        source.push_str("=()=>import(");
        source.push_str(
            &serde_json::to_string(&format!("./{wrapper_file}"))
                .expect("synthetic wrapper serialization"),
        );
        source.push_str(").then(module=>module.default);\n");
        exposes.push(SyntheticExposeEntry {
            key: key.clone(),
            config: expose.clone(),
            chunk_name: wrapper_stem,
            loader_export,
        });
    }

    let entry = projected_federation_path(
        generated_relative.join(format!("container-{}.mjs", short_hash(source.as_bytes()))),
    )?;
    insert_federation_input(generated_inputs, entry.clone(), source.as_bytes())?;
    Ok(SyntheticContainerEntry {
        entry: logical_federation_path(prepared, &entry),
        aliases,
        exposes,
        shared: shared_entry,
    })
}

fn render_production_container(
    prepared: &PreparedBuild,
    generated_inputs: &mut OwnedFileTreeBuilder,
) -> Result<UnboundContainerBuild, WakeError> {
    let generated_relative = PathBuf::from("federation/generated/container");
    let mut source = String::new();
    let mut exposes = Vec::with_capacity(prepared.config.federation.exposes.len());
    for (index, (key, expose)) in prepared.config.federation.exposes.iter().enumerate() {
        let safe = safe_expose_name(key.as_str());
        let wrapper_stem = format!("expose-{index:04}-{safe}");
        let wrapper_file = format!("{wrapper_stem}.mjs");
        let alias = format!("@@@/__wake_federation_expose_{index:04}");
        let wrapper = format!(
            "import * as namespace from {};\nexport default namespace;\n",
            serde_json::to_string(&alias).expect("synthetic expose alias serialization")
        );
        let wrapper_path = projected_federation_path(generated_relative.join(&wrapper_file))?;
        insert_federation_input(generated_inputs, wrapper_path, wrapper.as_bytes())?;

        let loader_export = format!("e{index}");
        source.push_str("export const ");
        source.push_str(&loader_export);
        source.push_str("=()=>import(");
        source.push_str(
            &serde_json::to_string(&format!("./{wrapper_file}"))
                .expect("synthetic wrapper serialization"),
        );
        source.push_str(").then(module=>module.default);\n");
        exposes.push(UnboundExposeRoot {
            key: key.clone(),
            config: expose.clone(),
            configured_entry: absolute_from(&prepared.root, Path::new(&expose.entry)),
            alias,
            chunk_name: wrapper_stem,
            loader_export,
        });
    }
    let entry = projected_federation_path(
        generated_relative.join(format!("container-{}.mjs", short_hash(source.as_bytes()))),
    )?;
    insert_federation_input(generated_inputs, entry.clone(), source.as_bytes())?;
    Ok(UnboundContainerBuild { entry, exposes })
}

fn bind_container_build(
    prepared: &PreparedBuild,
    options: &BuildOptions,
    unbound: &UnboundContainerBuild,
) -> Result<PreparedContainerBuild, WakeError> {
    let mut aliases = prepared.aliases.clone();
    let mut exposes = BTreeMap::new();
    for expose in &unbound.exposes {
        let entry = expose.configured_entry.canonicalize().map_err(|error| {
            WakeError::new(
                "WAKE_IO",
                format!("cannot resolve federation expose `{}`: {error}", expose.key),
            )
            .at(&expose.configured_entry)
        })?;
        let entry = wake_common::fs::normalize(&entry);
        if !entry.starts_with(&prepared.root) {
            return Err(WakeError::new(
                "FED_CONFIG_INVALID",
                format!(
                    "federation expose `{}` resolves outside the project root",
                    expose.key
                ),
            )
            .at(&entry));
        }
        aliases.push((expose.alias.clone(), entry));
        exposes.insert(
            expose.key.clone(),
            BuiltExposeRoot {
                config: expose.config.clone(),
                chunk_name: expose.chunk_name.clone(),
                loader_export: expose.loader_export.clone(),
            },
        );
    }
    let expose_roots = unbound
        .exposes
        .iter()
        .map(|expose| (expose.chunk_name.clone(), expose.key.as_str().to_owned()))
        .collect();
    let mut bundler_options = create_bundler_options(prepared, options, true)?;
    bundler_options.resolve.alias = aliases;
    bundler_options.federation.entry_export = Some(FederationEntryExport::build_scoped(
        prepared.config.federation.name.as_str(),
        CONTAINER_EXPOSE,
    ));
    bundler_options.federation.expose_roots = expose_roots;
    bundler_options.federation.shared = shared_mappings(prepared);
    bundler_options.entry_chunk_name = Some("container".to_owned());
    bundler_options.source_map = true;
    Ok(PreparedContainerBuild {
        directory: format!("federation/{}/container", prepared.config.federation.name),
        entry: logical_federation_path(prepared, &unbound.entry),
        options: bundler_options,
        exposes,
    })
}

fn build_container(
    prepared: &PreparedBuild,
    build: PreparedContainerBuild,
    generation: &mut BuildGeneration,
) -> Result<BuiltContainer, WakeError> {
    let PreparedContainerBuild {
        directory,
        entry,
        options,
        exposes,
    } = build;
    let output = generation.build_once(options, BuildRequest::new(entry));
    ensure_output(
        prepared,
        &output,
        "federation container",
        generation.file_system_view().as_ref(),
    )?;
    Ok(BuiltContainer {
        directory,
        output,
        exposes,
    })
}

fn render_production_shared_provider(
    shared: &[UnboundSharedSlot],
    generated_inputs: &mut OwnedFileTreeBuilder,
) -> Result<UnboundSharedBuild, WakeError> {
    let mut source = String::new();
    for descriptor in shared {
        source.push_str("import * as ");
        source.push_str(&descriptor.export_name);
        source.push_str(" from ");
        source.push_str(
            &serde_json::to_string(&descriptor.share_key).expect("share key serialization"),
        );
        source.push_str(";\n");
    }
    source.push_str("export {");
    for (index, descriptor) in shared.iter().enumerate() {
        if index != 0 {
            source.push(',');
        }
        source.push_str(&descriptor.export_name);
    }
    source.push_str("};\n");

    let entry = projected_federation_path(
        PathBuf::from("federation/generated")
            .join(format!("shared-{}.mjs", short_hash(source.as_bytes()))),
    )?;
    insert_federation_input(generated_inputs, entry.clone(), source.as_bytes())?;
    Ok(UnboundSharedBuild { entry })
}

fn bind_shared_provider_build(
    prepared: &PreparedBuild,
    options: &BuildOptions,
    unbound: &UnboundSharedBuild,
    shared: &[SharedDescriptor],
    build_scoped: bool,
) -> Result<PreparedSharedBuild, WakeError> {
    let mut bundler_options = create_bundler_options(prepared, options, false)?;
    bundler_options.resolve.alias.extend(
        shared
            .iter()
            .map(|descriptor| (descriptor.share_key.clone(), descriptor.source.clone())),
    );
    // `project_defaults=false` selects the self-contained provider emission shape; it must not
    // accidentally select development defines. Production container, application and shared
    // provider identities all use the same production replacement set hashed below.
    bundler_options.define = super::build_defines(&prepared.config, false);
    bundler_options.federation.remotes.clear();
    bundler_options.federation.shared.clear();
    bundler_options.entry_chunk_name = Some("shared".to_owned());
    bundler_options.asset_inline_limit = usize::MAX;
    bundler_options.tree_shaking = true;
    bundler_options.dead_module_elimination = true;
    bundler_options.minify = true;
    bundler_options.source_map = true;
    bundler_options.federation.entry_export = Some(if build_scoped {
        FederationEntryExport::build_scoped(prepared.config.federation.name.as_str(), SHARED_EXPOSE)
    } else {
        FederationEntryExport::page_scoped(prepared.config.federation.name.as_str(), SHARED_EXPOSE)
    });
    Ok(PreparedSharedBuild {
        directory: format!("federation/{}/shared", prepared.config.federation.name),
        entry: logical_federation_path(prepared, &unbound.entry),
        options: bundler_options,
    })
}

fn build_shared_provider(
    prepared: &PreparedBuild,
    build: PreparedSharedBuild,
    generation: &mut BuildGeneration,
) -> Result<BuiltShared, WakeError> {
    let PreparedSharedBuild {
        directory,
        entry,
        options,
    } = build;
    let output = generation.build_once(options, BuildRequest::new(entry));
    ensure_output(
        prepared,
        &output,
        "federation shared provider",
        generation.file_system_view().as_ref(),
    )?;
    Ok(BuiltShared { directory, output })
}

fn ensure_output(
    prepared: &PreparedBuild,
    output: &BuildOutput,
    label: &str,
    diagnostic_file_system: &dyn FileSystem,
) -> Result<(), WakeError> {
    if output.has_errors() {
        return Err(
            WakeError::new("WAKE_BUILD", format!("failed to build {label}")).with_diagnostic_infos(
                diagnostic_infos(&output.diagnostics, &prepared.root, diagnostic_file_system),
            ),
        );
    }
    Ok(())
}

fn shared_mappings(prepared: &PreparedBuild) -> Vec<(String, String, String)> {
    prepared
        .config
        .federation
        .shared
        .iter()
        .map(|(share_key, config)| (share_key.clone(), share_key.clone(), config.scope.clone()))
        .collect()
}

fn remote_singleton_owners(
    federation: &wake_federation_contract::FederationConfig,
    shared: &[SharedDescriptor],
) -> Vec<String> {
    shared
        .iter()
        .filter(|descriptor| descriptor.config.singleton)
        .filter_map(|descriptor| descriptor.config.owner.as_ref())
        .filter(|owner| *owner != &federation.name)
        .map(|owner| owner.as_str().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn collect_output_identity(
    directory: &str,
    output: &BuildOutput,
    destination: &mut Vec<(String, Vec<u8>)>,
) {
    for chunk in &output.chunks {
        destination.push((
            format!("{directory}/{}", chunk.file_name),
            chunk.code.as_bytes().to_vec(),
        ));
    }
    for asset in &output.assets {
        destination.push((
            format!("{directory}/{}", asset.file_name),
            asset.bytes.clone(),
        ));
        let owner_chunks = output
            .chunks
            .iter()
            .filter(|chunk| {
                chunk
                    .module_ids
                    .iter()
                    .any(|module_id| asset.owner_module_ids.contains(module_id))
            })
            .map(|chunk| chunk.file_name.as_str())
            .collect::<BTreeSet<_>>();
        destination.push((
            format!("{directory}/{}.owners", asset.file_name),
            serde_json::to_vec(&owner_chunks).expect("asset owner chunk serialization"),
        ));
        let unscoped_owner_chunks = output
            .chunks
            .iter()
            .filter(|chunk| {
                chunk
                    .module_ids
                    .iter()
                    .any(|module_id| asset.unscoped_css_owner_module_ids.contains(module_id))
            })
            .map(|chunk| chunk.file_name.as_str())
            .collect::<BTreeSet<_>>();
        destination.push((
            format!("{directory}/{}.unscoped-css-owners", asset.file_name),
            serde_json::to_vec(&unscoped_owner_chunks)
                .expect("unscoped CSS owner chunk serialization"),
        ));
    }
}

fn canonical_federation_identity_bytes(
    config: &wake_federation_contract::FederationConfig,
    shared: &[SharedDescriptor],
    browser_target: &str,
) -> Vec<u8> {
    let exposes = config
        .exposes
        .iter()
        .map(|(key, expose)| {
            (
                key.as_str().to_owned(),
                serde_json::json!({
                    "mode": expose.mode,
                    "scope": expose.scope,
                    "shadow": expose.shadow,
                }),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let identity = serde_json::json!({
        "schemaVersion": wake_federation_contract::FEDERATION_SCHEMA_VERSION,
        "runtimeAbi": wake_federation_contract::FEDERATION_RUNTIME_ABI,
        "container": config.name,
        "browserTarget": browser_target,
        "exposes": exposes,
        "shared": shared_manifest(config, shared, None),
    });

    // Remote URLs, origin allowlists, and dev-follow policy belong to deployment/bootstrap state,
    // not to this producer's immutable container. Expose entry paths are represented by emitted
    // bytes, while allowGlobalCss is a producer validation gate and has no Manifest wire field.
    serde_json::to_vec(&identity).expect("federation identity serialization")
}

fn build_id_from_identity_inputs(inputs: &[(String, Vec<u8>)]) -> BuildId {
    BuildId::new(format!("sha384-{}", &stable_digest(inputs).0[..32]))
}

fn stable_digest(inputs: &[(String, Vec<u8>)]) -> (String, String) {
    let mut ordered = inputs.to_vec();
    ordered.sort_by(|left, right| left.0.cmp(&right.0));
    let mut framed = Vec::new();
    for (path, bytes) in ordered {
        framed.extend_from_slice(&(path.len() as u64).to_be_bytes());
        framed.extend_from_slice(path.as_bytes());
        framed.extend_from_slice(&(bytes.len() as u64).to_be_bytes());
        framed.extend_from_slice(&bytes);
    }
    digest_bytes(&framed)
}

fn digest_bytes(bytes: &[u8]) -> (String, String) {
    let digest = digest(&SHA384, bytes);
    let raw = digest.as_ref();
    let mut hex = String::with_capacity(raw.len() * 2);
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for byte in raw {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    (hex, format!("sha384-{}", BASE64.encode(raw)))
}

fn short_hash(bytes: &[u8]) -> String {
    digest_bytes(bytes).0[..16].to_owned()
}

fn safe_expose_name(key: &str) -> String {
    let value = key.strip_prefix("./").unwrap_or(key);
    let mut safe = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if safe.is_empty() {
        safe.push_str("expose");
    }
    safe
}

fn asset_for(kind: AssetKind, url: String, bytes: &[u8], mime: &str) -> Asset {
    let (content_hash, integrity) = digest_bytes(bytes);
    Asset::new(kind, url, content_hash, integrity, mime, bytes.len() as u64)
}

fn materialize_container(
    container_name: &str,
    build_id: &BuildId,
    container: &BuiltContainer,
    source_map_mode: ProductionSourceMapMode,
) -> Result<MaterializedContainer, WakeError> {
    let deployment_directory = format!("federation/{container_name}/{build_id}/container");
    let mut files = Vec::new();
    let mut hidden_source_maps = Vec::new();
    let mut chunk_assets = BTreeMap::<String, Asset>::new();
    let mut map_assets = BTreeMap::<String, Asset>::new();

    for chunk in container
        .output
        .chunks
        .iter()
        .filter(|chunk| !chunk.is_entry)
    {
        let path = format!("{deployment_directory}/{}", chunk.file_name);
        let url = format!("./{path}");
        let bytes = chunk.code.as_bytes().to_vec();
        chunk_assets.insert(
            chunk.file_name.clone(),
            asset_for(AssetKind::JavaScript, url, &bytes, "text/javascript"),
        );
        files.push(GeneratedFile {
            path,
            bytes,
            kind: OutputFileKind::FederationChunk,
        });
        if let Some(source_map) = &chunk.source_map {
            let map = rewrite_remote_source_map(
                source_map,
                container_name,
                build_id.as_str(),
                &chunk.file_name,
            )?;
            let map_path = format!("{deployment_directory}/{}.map", chunk.file_name);
            let map_bytes = map.into_bytes();
            if source_map_mode.is_public() {
                map_assets.insert(
                    chunk.file_name.clone(),
                    asset_for(
                        AssetKind::SourceMap,
                        format!("./{map_path}"),
                        &map_bytes,
                        "application/source-map+json",
                    ),
                );
                files.push(GeneratedFile {
                    path: map_path,
                    bytes: map_bytes,
                    kind: OutputFileKind::SourceMap,
                });
            } else {
                hidden_source_maps.push(GeneratedFile {
                    path: hidden_source_map_path(
                        container_name,
                        build_id,
                        &format!("container/{}.map", chunk.file_name),
                    ),
                    bytes: map_bytes,
                    kind: OutputFileKind::SourceMap,
                });
            }
        }
    }

    let mut asset_lookup = BTreeMap::<String, Asset>::new();
    for output_asset in &container.output.assets {
        let kind = if output_asset.is_css {
            AssetKind::Css
        } else {
            AssetKind::Other
        };
        let mime = if output_asset.is_css {
            "text/css"
        } else {
            mime_for_path(&output_asset.file_name)
        };
        let path = format!("{deployment_directory}/{}", output_asset.file_name);
        let asset = asset_for(kind, format!("./{path}"), &output_asset.bytes, mime);
        asset_lookup.insert(output_asset.file_name.clone(), asset);
        files.push(GeneratedFile {
            path,
            bytes: output_asset.bytes.clone(),
            kind: if output_asset.is_css {
                OutputFileKind::Css
            } else {
                OutputFileKind::Asset
            },
        });
    }

    let chunks = container
        .output
        .chunks
        .iter()
        .filter(|chunk| !chunk.is_entry)
        .map(|chunk| (chunk.file_name.clone(), chunk))
        .collect::<BTreeMap<_, _>>();
    let mut exposed_modules = BTreeMap::new();
    for (key, root) in &container.exposes {
        let entry_chunk = container
            .output
            .chunks
            .iter()
            .find(|chunk| !chunk.is_entry && chunk.name == root.chunk_name)
            .ok_or_else(|| {
                WakeError::new(
                    "WAKE_INTERNAL",
                    format!(
                        "federation expose `{key}` has no independently loadable `{}` chunk",
                        root.chunk_name
                    ),
                )
            })?;
        let initial_files = dependency_first_chunk_closure(&entry_chunk.file_name, &chunks)?;
        let initial_set = initial_files.iter().cloned().collect::<BTreeSet<_>>();
        let all_files = transitive_dynamic_chunk_closure(&initial_files, &chunks)?;
        let initial_module_ids = initial_files
            .iter()
            .flat_map(|file| chunks[file].module_ids.iter().copied())
            .collect::<BTreeSet<_>>();
        let all_module_ids = all_files
            .iter()
            .flat_map(|file| chunks[file].module_ids.iter().copied())
            .collect::<BTreeSet<_>>();
        validate_host_rendered_css_policy(
            key,
            &root.config,
            &container.output.assets,
            &all_module_ids,
        )?;

        let entry = chunk_assets
            .get(&entry_chunk.file_name)
            .cloned()
            .expect("expose entry chunk asset exists");
        let source_map = map_assets.get(&entry_chunk.file_name).cloned();
        let mut synchronous_assets = initial_files
            .iter()
            .filter(|file| *file != &entry_chunk.file_name)
            .filter_map(|file| chunk_assets.get(file).cloned())
            .collect::<Vec<_>>();
        synchronous_assets.extend(
            container
                .output
                .assets
                .iter()
                .filter(|asset| !asset.is_css)
                .filter(|asset| {
                    asset
                        .owner_module_ids
                        .iter()
                        .any(|owner| initial_module_ids.contains(owner))
                })
                .filter_map(|asset| asset_lookup.get(&asset.file_name).cloned()),
        );

        let mut css = Vec::new();
        let mut seen_css = BTreeSet::new();
        for file in &initial_files {
            for style in &chunks[file].styles {
                if seen_css.insert(style.clone())
                    && let Some(asset) = asset_lookup.get(style)
                {
                    css.push(asset.clone());
                }
            }
        }

        let mut asynchronous_assets = Vec::new();
        let mut seen_async_assets = BTreeSet::new();
        for file in &all_files {
            if !initial_set.contains(file)
                && let Some(asset) = chunk_assets.get(file)
                && seen_async_assets.insert(asset.clone())
            {
                asynchronous_assets.push(asset.clone());
            }
            if file != &entry_chunk.file_name
                && let Some(map) = map_assets.get(file)
                && seen_async_assets.insert(map.clone())
            {
                asynchronous_assets.push(map.clone());
            }
            if !initial_set.contains(file) {
                for style in &chunks[file].styles {
                    if let Some(asset) = asset_lookup.get(style)
                        && seen_async_assets.insert(asset.clone())
                    {
                        asynchronous_assets.push(asset.clone());
                    }
                }
            }
        }
        asynchronous_assets.extend(
            container
                .output
                .assets
                .iter()
                .filter(|asset| !asset.is_css)
                .filter(|asset| {
                    !asset
                        .owner_module_ids
                        .iter()
                        .any(|owner| initial_module_ids.contains(owner))
                        && asset
                            .owner_module_ids
                            .iter()
                            .any(|owner| all_module_ids.contains(owner))
                })
                .filter_map(|asset| asset_lookup.get(&asset.file_name).cloned())
                .filter(|asset| seen_async_assets.insert(asset.clone())),
        );

        exposed_modules.insert(
            key.clone(),
            ExposedModule {
                mode: root.config.mode,
                scope: root.config.scope.clone(),
                shadow: root.config.shadow,
                entry,
                css,
                source_map,
                synchronous_assets,
                asynchronous_assets,
            },
        );
    }

    Ok(MaterializedContainer {
        exposes: exposed_modules,
        files,
        hidden_source_maps,
    })
}

fn validate_host_rendered_css_policy(
    expose: &ExposeKey,
    config: &ExposeConfig,
    assets: &[wake_bundler::OutputAsset],
    reachable_module_ids: &BTreeSet<u32>,
) -> Result<(), WakeError> {
    if config.mode != ExposeMode::HostRendered || config.allow_global_css {
        return Ok(());
    }
    let unscoped = assets
        .iter()
        .filter(|asset| asset.is_css)
        .filter(|asset| {
            asset
                .unscoped_css_owner_module_ids
                .iter()
                .any(|owner| reachable_module_ids.contains(owner))
        })
        .map(|asset| asset.file_name.as_str())
        .collect::<Vec<_>>();
    if unscoped.is_empty() {
        return Ok(());
    }
    Err(WakeError::new(
        FederationErrorCode::ConfigInvalid.as_str(),
        format!(
            "host-rendered expose `{expose}` reaches unscoped CSS {}; use CSS Modules or Wake CSS-in-JS, or explicitly set allow_global_css=true",
            unscoped
                .iter()
                .map(|file| format!("`{file}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    ))
}

fn dependency_first_chunk_closure(
    root: &str,
    chunks: &BTreeMap<String, &wake_bundler::OutputChunk>,
) -> Result<Vec<String>, WakeError> {
    fn visit(
        file: &str,
        chunks: &BTreeMap<String, &wake_bundler::OutputChunk>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        ordered: &mut Vec<String>,
    ) -> Result<(), WakeError> {
        if visited.contains(file) {
            return Ok(());
        }
        if !visiting.insert(file.to_owned()) {
            return Err(WakeError::new(
                "WAKE_INTERNAL",
                format!("federation chunk dependency cycle includes `{file}`"),
            ));
        }
        let chunk = chunks.get(file).ok_or_else(|| {
            WakeError::new(
                "WAKE_INTERNAL",
                format!("federation chunk `{file}` imports a missing output chunk"),
            )
        })?;
        for dependency in &chunk.imports {
            visit(dependency, chunks, visiting, visited, ordered)?;
        }
        visiting.remove(file);
        visited.insert(file.to_owned());
        ordered.push(file.to_owned());
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut ordered = Vec::new();
    visit(root, chunks, &mut visiting, &mut visited, &mut ordered)?;
    Ok(ordered)
}

fn transitive_dynamic_chunk_closure(
    initial: &[String],
    chunks: &BTreeMap<String, &wake_bundler::OutputChunk>,
) -> Result<Vec<String>, WakeError> {
    let mut ordered = initial.to_vec();
    let mut visited = initial.iter().cloned().collect::<BTreeSet<_>>();
    let mut scan = 0;
    while scan < ordered.len() {
        let file = ordered[scan].clone();
        let dynamic_targets = chunks[&file].dynamic_imports.clone();
        for target in dynamic_targets {
            let closure = dependency_first_chunk_closure(&target, chunks)?;
            for dependency in closure {
                if visited.insert(dependency.clone()) {
                    ordered.push(dependency);
                }
            }
        }
        scan += 1;
    }
    Ok(ordered)
}

fn materialize_shared(
    container: &str,
    build_id: &BuildId,
    shared: BuiltShared,
    source_map_mode: ProductionSourceMapMode,
) -> Result<(AssetBytes, Vec<GeneratedFile>, Vec<GeneratedFile>), WakeError> {
    if shared.output.chunks.len() != 1 || !shared.output.assets.is_empty() {
        return Err(WakeError::new(
            "FED_CONFIG_INVALID",
            "shared fallback must compile to one self-contained JavaScript asset",
        ));
    }
    let chunk = shared.output.entry();
    let path = format!(
        "federation/{container}/{build_id}/shared/{}",
        chunk.file_name
    );
    let bytes = chunk.code.as_bytes().to_vec();
    let asset = asset_for(
        AssetKind::JavaScript,
        format!("./{path}"),
        &bytes,
        "text/javascript",
    );
    let mut files = vec![GeneratedFile {
        path: path.clone(),
        bytes: bytes.clone(),
        kind: OutputFileKind::FederationShared,
    }];
    let mut hidden_source_maps = Vec::new();
    if let Some(map) = &chunk.source_map {
        let rewritten =
            rewrite_remote_source_map(map, container, build_id.as_str(), &chunk.file_name)?;
        let bytes = rewritten.into_bytes();
        if source_map_mode.is_public() {
            files.push(GeneratedFile {
                path: format!("{path}.map"),
                bytes,
                kind: OutputFileKind::SourceMap,
            });
        } else {
            hidden_source_maps.push(GeneratedFile {
                path: hidden_source_map_path(
                    container,
                    build_id,
                    &format!("shared/{}.map", chunk.file_name),
                ),
                bytes,
                kind: OutputFileKind::SourceMap,
            });
        }
    }
    Ok((AssetBytes { asset, path }, files, hidden_source_maps))
}

fn hidden_source_map_path(container: &str, build_id: &BuildId, file: &str) -> String {
    format!("federation/source-maps/{container}/{build_id}/{file}")
}

fn shared_manifest(
    federation: &wake_federation_contract::FederationConfig,
    shared: &[SharedDescriptor],
    fallback: Option<&Asset>,
) -> SharedManifest {
    let mut manifest = SharedManifest::default();
    for descriptor in shared {
        let policy = SharedPolicy {
            scope: descriptor.config.scope.clone(),
            singleton: descriptor.config.singleton,
            strict: descriptor.config.strict,
            fallback: descriptor.config.fallback,
            coherence_group: descriptor.config.coherence_group.clone(),
            owner: descriptor.config.owner.clone(),
        };
        manifest.requirements.push(SharedRequirement {
            share_key: descriptor.share_key.clone(),
            required_version: descriptor
                .config
                .required_version
                .clone()
                .unwrap_or_else(|| descriptor.package.version.clone()),
            package_context: descriptor.package.package_context.clone(),
            build_variant: descriptor.package.build_variant.clone(),
            policy: policy.clone(),
            fallback: descriptor
                .config
                .fallback
                .then(|| fallback.cloned())
                .flatten(),
        });
        if descriptor.config.fallback {
            manifest.offers.push(SharedOffer {
                share_key: descriptor.share_key.clone(),
                package: descriptor.package.clone(),
                provider: federation.name.clone(),
                policy,
                asset: fallback.cloned(),
            });
        }
    }
    manifest
}

fn rewrite_remote_source_map(
    map: &str,
    container: &str,
    build_id: &str,
    file_name: &str,
) -> Result<String, WakeError> {
    let mut value = serde_json::from_str::<serde_json::Value>(map).map_err(|error| {
        WakeError::new(
            "WAKE_INTERNAL",
            format!("Wake generated an invalid federation source map: {error}"),
        )
    })?;
    let object = value.as_object_mut().ok_or_else(|| {
        WakeError::new(
            "WAKE_INTERNAL",
            "Wake generated a federation source map whose root is not an object",
        )
    })?;
    object.insert(
        "file".to_owned(),
        serde_json::Value::String(file_name.to_owned()),
    );
    if let Some(sources) = object
        .get_mut("sources")
        .and_then(|value| value.as_array_mut())
    {
        for source in sources {
            if let Some(name) = source.as_str() {
                let normalized = name
                    .replace('\\', "/")
                    .trim_start_matches("../")
                    .trim_start_matches("./")
                    .to_owned();
                *source = serde_json::Value::String(format!(
                    "wake://{container}@{build_id}/{normalized}"
                ));
            }
        }
    }
    serde_json::to_string(&value).map_err(|error| {
        WakeError::new(
            "WAKE_INTERNAL",
            format!("could not serialize federation source map: {error}"),
        )
    })
}

fn browser_target(prepared: &PreparedBuild) -> Result<String, WakeError> {
    let targets = prepared
        .config
        .resolve_browser_targets(&prepared.root)
        .map_err(|error| WakeError::new("WAKE_CONFIG", error.to_string()).at(&prepared.root))?;
    Ok(targets
        .into_iter()
        .map(|target| format!("{}>={}", target.name, target.version))
        .collect::<Vec<_>>()
        .join(","))
}

fn mime_for_path(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn type_entries(prepared: &PreparedBuild) -> Vec<(ExposeKey, PathBuf)> {
    prepared
        .config
        .federation
        .exposes
        .iter()
        .map(|(key, expose)| {
            (
                key.clone(),
                absolute_from(&prepared.root, Path::new(&expose.entry)),
            )
        })
        .collect()
}

fn prepare_federation_types(
    prepared: &PreparedBuild,
    generation_fs: Arc<dyn FileSystem>,
) -> Result<Option<super::federation_types::FrozenFederationTypes>, WakeError> {
    let entries = type_entries(prepared);
    if entries.is_empty() {
        return Ok(None);
    }
    let file_system = super::federation_types::GenerationDeclarationFileSystem::new(generation_fs);
    let types = super::federation_types::prepare_federation_types_with_file_system(
        &prepared.root,
        &prepared.config.federation.name,
        &entries,
        &file_system,
    )
    .map_err(|error| WakeError::new("FED_TYPES_INVALID", error.to_string()))?;
    Ok(Some(types))
}

fn materialize_type_artifact(
    prepared: &PreparedBuild,
    build_id: &BuildId,
    prepared_types: Option<super::federation_types::FrozenFederationTypes>,
) -> Result<Option<(TypeArtifact, Vec<GeneratedFile>)>, WakeError> {
    let Some(prepared_types) = prepared_types else {
        return Ok(None);
    };
    let output = prepared_types
        .bind(build_id)
        .map_err(|error| WakeError::new("FED_TYPES_INVALID", error.to_string()))?;
    let file_name = format!(
        "federation/{}/{}/types/types.{}.json",
        prepared.config.federation.name,
        build_id,
        short_hash(&output.bundle_json)
    );
    let (content_hash, integrity) = digest_bytes(&output.bundle_json);
    let artifact = TypeArtifact {
        build_id: build_id.clone(),
        url: format!("./{file_name}"),
        content_hash,
        integrity,
        size: output.bundle_json.len() as u64,
        format: wake_federation_contract::TypeArtifactFormat::DeclarationBundle,
    };
    let ambient_file = format!(
        "federation/{}/{}/types/index.d.ts",
        prepared.config.federation.name, build_id
    );
    Ok(Some((
        artifact,
        vec![
            GeneratedFile {
                path: file_name,
                bytes: output.bundle_json,
                kind: OutputFileKind::FederationTypes,
            },
            GeneratedFile {
                path: ambient_file,
                bytes: output.ambient_declaration.into_bytes(),
                kind: OutputFileKind::FederationTypes,
            },
        ],
    )))
}

fn remote_entry_source(
    container: &str,
    build_id: &str,
    container_entry: &str,
    exposes: &BTreeMap<ExposeKey, BuiltExposeRoot>,
    shared: &[SharedDescriptor],
) -> String {
    let expose_loaders = exposes
        .iter()
        .map(|(key, expose)| (key.clone(), expose.loader_export.clone()))
        .collect::<BTreeMap<_, _>>();
    let protocol = remote_entry_protocol_source(container, build_id, &expose_loaders, shared, None);
    format!("{container_entry}\n{protocol}")
}

fn remote_entry_protocol_source(
    container: &str,
    build_id: &str,
    expose_loaders: &BTreeMap<ExposeKey, String>,
    shared: &[SharedDescriptor],
    shared_loader_export: Option<&str>,
) -> String {
    let shared_exports = shared
        .iter()
        .filter(|descriptor| descriptor.config.fallback)
        .map(|descriptor| (&descriptor.share_key, &descriptor.export_name))
        .collect::<BTreeMap<_, _>>();
    let name = serde_json::to_string(container).expect("container serialization");
    let build = serde_json::to_string(build_id).expect("build id serialization");
    let expose_json = serde_json::to_string(&expose_loaders).expect("expose serialization");
    let shared_json = serde_json::to_string(&shared_exports).expect("shared serialization");
    let shared_loader_json =
        serde_json::to_string(&shared_loader_export).expect("shared loader serialization");
    format!(
        r#"const NAME={name};
const BUILD_ID={build};
const EXPOSE_LOADERS={expose_json};
const SHARED={shared_json};
const SHARED_LOADER={shared_loader_json};
const ABI=Symbol.for('wake.federation.v1');
const CONTEXTS=Symbol.for('wake.federation.share-contexts.v1');
const MODULES=Symbol.for('wake.federation.exposes.v1');
const fail=(code,message,details)=>{{const error=new Error(`[${{code}}] ${{message}}`);error.code=code;error.details=details;throw error}};
let initializedContext;
const container={{
  async init(context){{
    if(!context||context.runtimeAbi!=='wake.federation.v1'||typeof context.getSync!=='function')fail('FED_RUNTIME_ABI','container received an incompatible share context',{{name:NAME,buildId:BUILD_ID}});
    if(initializedContext!==undefined){{if(initializedContext!==context)fail('FED_CONTAINER_INIT','container was initialized with a different share context',{{name:NAME,buildId:BUILD_ID}});return}}
    let contexts=globalThis[CONTEXTS];
    if(contexts===undefined){{contexts=Object.create(null);Object.defineProperty(globalThis,CONTEXTS,{{value:contexts,configurable:false}})}}
    const containerContexts=contexts[NAME]||(contexts[NAME]=Object.create(null));
    const previous=containerContexts[BUILD_ID];
    if(previous!==undefined&&previous!==context)fail('FED_CONTAINER_INIT','this build already owns another share context',{{name:NAME,buildId:BUILD_ID}});
    containerContexts[BUILD_ID]=context;
    initializedContext=context;
  }},
  async get(expose){{
    if(initializedContext===undefined)fail('FED_CONTAINER_INIT','container.get() was called before init()',{{name:NAME,buildId:BUILD_ID,expose}});
    const loaderExport=EXPOSE_LOADERS[expose];
    if(loaderExport===undefined)fail('FED_UNKNOWN_EXPOSE','container does not expose the requested module',{{name:NAME,buildId:BUILD_ID,expose}});
    const loaders=globalThis[MODULES]?.[NAME]?.[BUILD_ID]?.['./__wake_container__'];
    const loader=loaders?.[loaderExport];
    if(typeof loader!=='function')fail('FED_CONTAINER_GET','container entry did not publish the expose loader',{{name:NAME,buildId:BUILD_ID,expose,loaderExport}});
    return ()=>loader();
  }},
  async getShared(shareKey){{
    const exportName=SHARED[shareKey];
    if(exportName===undefined)fail('FED_SHARE_UNSATISFIABLE','container has no fallback for this shared dependency',{{name:NAME,buildId:BUILD_ID,shareKey}});
    let namespace;
    if(SHARED_LOADER!==null){{
      const loaders=globalThis[MODULES]?.[NAME]?.[BUILD_ID]?.['./__wake_container__'];
      const loader=loaders?.[SHARED_LOADER];
      if(typeof loader!=='function')fail('FED_SHARE_UNSATISFIABLE','container entry did not publish its shared fallback loader',{{name:NAME,buildId:BUILD_ID,shareKey,loaderExport:SHARED_LOADER}});
      namespace=await loader();
    }}else{{
      namespace=globalThis[MODULES]?.[NAME]?.[BUILD_ID]?.['./__wake_shared__'];
    }}
    if(namespace===undefined)fail('FED_SHARE_UNSATISFIABLE','shared fallback asset did not publish its namespace',{{name:NAME,buildId:BUILD_ID,shareKey}});
    return namespace[exportName];
  }}
}};
const broker=globalThis[ABI];
if(!broker||typeof broker.registerContainer!=='function')fail('FED_RUNTIME_ABI','Wake Federation broker is not installed',{{name:NAME,buildId:BUILD_ID}});
broker.registerContainer({{name:NAME,buildId:BUILD_ID,container}});
export {{container}};
export default container;
//# sourceURL=wake://{container}@{build_id}/remoteEntry.mjs
"#
    )
}

pub(super) fn load_production_lock(
    prepared: &PreparedBuild,
) -> Result<Option<FederationLock>, WakeError> {
    load_production_lock_from_fs(prepared, Arc::new(OsFileSystem))
        .map(|lock| lock.map(|lock| (*lock).clone()))
}

pub(super) fn load_production_lock_from_fs(
    prepared: &PreparedBuild,
    fs: Arc<dyn FileSystem>,
) -> Result<Option<Arc<FederationLock>>, WakeError> {
    let remotes = &prepared.config.federation.remotes;
    if remotes.is_empty() {
        return Ok(None);
    }
    let path = prepared.root.join(LOCK_FILE);
    let bytes = fs.read(&path).map_err(|error| {
        WakeError::new(
            "FED_LOCK_REQUIRED",
            format!(
                "production federation build requires `{LOCK_FILE}`: {error}; generate and review the remote lock before building"
            ),
        )
        .at(&path)
    })?;
    let lock = serde_json::from_slice::<FederationLock>(&bytes).map_err(|error| {
        WakeError::new(
            "FED_LOCK_INVALID",
            format!("invalid federation lock: {error}"),
        )
        .at(&path)
    })?;
    lock.validate().map_err(|error| {
        let code = if error
            .violations
            .iter()
            .any(|violation| violation.code == FederationErrorCode::TypeBuildMismatch)
        {
            FederationErrorCode::TypeBuildMismatch.as_str()
        } else {
            "FED_LOCK_INVALID"
        };
        WakeError::new(code, format!("invalid federation lock: {error}")).at(&path)
    })?;
    for (name, remote) in remotes {
        let locked = lock.remotes.get(name).ok_or_else(|| {
            WakeError::new(
                "FED_LOCK_MISMATCH",
                format!("federation lock is missing configured remote `{name}`"),
            )
            .at(&path)
        })?;
        if locked.manifest_url != remote.manifest_url {
            return Err(WakeError::new(
                "FED_LOCK_MISMATCH",
                format!(
                    "locked manifest URL for `{name}` does not match configuration: `{}` != `{}`",
                    locked.manifest_url, remote.manifest_url
                ),
            )
            .at(&path));
        }
        if locked.allowed_assets.is_empty() {
            return Err(WakeError::new(
                "FED_LOCK_INVALID",
                format!("production remote `{name}` has an empty asset closure"),
            )
            .at(&path));
        }
        if locked.has_exposes && locked.types_integrity.is_none() {
            return Err(WakeError::new(
                "FED_TYPE_BUILD_MISMATCH",
                format!("production remote `{name}` has no locked declaration artifact"),
            )
            .at(&path));
        }
    }
    Ok(Some(Arc::new(lock)))
}

fn bootstrap_source(
    prepared: &PreparedBuild,
    application_entry: &str,
    build_id: &BuildId,
    shared: &[SharedDescriptor],
    shared_asset: Option<&AssetBytes>,
    lock: Option<&FederationLock>,
) -> Result<String, WakeError> {
    let federation = &prepared.config.federation;
    let mut source = String::with_capacity(RUNTIME_SOURCE.len() + 4096);
    source.push_str(RUNTIME_SOURCE);
    source.push_str(RUNTIME_BOOTSTRAP_SOURCE);

    for (name, remote) in &federation.remotes {
        let locked = lock
            .and_then(|lock| lock.remotes.get(name))
            .ok_or_else(|| {
                WakeError::new(
                    "FED_LOCK_MISMATCH",
                    format!("production lock is missing remote `{name}`"),
                )
            })?;
        let mut registration = serde_json::Map::new();
        registration.insert("name".to_owned(), serde_json::json!(name.as_str()));
        registration.insert(
            "manifestUrl".to_owned(),
            serde_json::json!(remote.manifest_url),
        );
        registration.insert("mode".to_owned(), serde_json::json!("production"));
        if !remote.allowed_origins.is_empty() {
            registration.insert(
                "allowedOrigins".to_owned(),
                serde_json::json!(remote.allowed_origins),
            );
        }
        registration.insert(
            "lock".to_owned(),
            serde_json::to_value(locked).expect("remote lock serialization"),
        );
        source.push_str("__wake_federation_broker__.registerRemote(");
        source.push_str(
            &serde_json::to_string(&registration).expect("remote registration serialization"),
        );
        source.push_str(");\n");
    }

    if let Some(asset) = shared_asset {
        source.push_str("const __wake_shared_url__=new URL(");
        source.push_str(&serde_json::to_string(&format!("./{}", asset.path)).unwrap());
        source.push_str(",import.meta.url).href;\nlet __wake_asset_contexts__=globalThis[Symbol.for('wake.federation.asset-contexts.v1')];\nif(__wake_asset_contexts__===undefined){__wake_asset_contexts__=new Map();Object.defineProperty(globalThis,Symbol.for('wake.federation.asset-contexts.v1'),{value:__wake_asset_contexts__,configurable:false})}\nif(!(__wake_asset_contexts__ instanceof Map))throw Object.assign(new Error('Wake federation asset context registry is incompatible'),{code:'FED_RUNTIME_ABI'});\n__wake_asset_contexts__.set(__wake_shared_url__,Object.freeze({name:");
        source.push_str(&serde_json::to_string(federation.name.as_str()).unwrap());
        source.push_str(",buildId:");
        source.push_str(&serde_json::to_string(build_id.as_str()).unwrap());
        source.push_str(",generation:0}));\nawait import(__wake_shared_url__);\n");
        source.push_str(
            "const __wake_local_shared__=globalThis[Symbol.for('wake.federation.exposes.v1')]?.[",
        );
        source.push_str(&serde_json::to_string(federation.name.as_str()).unwrap());
        source.push_str("]?.[");
        source.push_str(&serde_json::to_string(build_id.as_str()).unwrap());
        source.push_str("]?.['./__wake_shared__'];\nif(!__wake_local_shared__)throw Object.assign(new Error('Wake host shared provider did not register'),{code:'FED_SHARE_UNSATISFIABLE'});\n");
        for descriptor in shared {
            let provider = serde_json::json!({
                "shareKey": descriptor.share_key,
                "version": descriptor.package.version,
                "scope": descriptor.config.scope,
                "singleton": descriptor.config.singleton,
                "strict": descriptor.config.strict,
                "packageContext": descriptor.package.package_context,
                "buildVariant": descriptor.package.build_variant,
                "coherenceGroup": descriptor.config.coherence_group,
                "fallback": descriptor.config.fallback,
                "owner": federation.name.as_str(),
            });
            source.push_str("__wake_federation_broker__.registerHostShared(Object.assign(");
            source.push_str(&provider.to_string());
            source.push_str(",{module:__wake_local_shared__[");
            source.push_str(&serde_json::to_string(&descriptor.export_name).unwrap());
            source.push_str("]}));\n");
        }

        for owner in remote_singleton_owners(federation, shared) {
            source.push_str("await __wake_federation_broker__.prepareRemote(");
            source.push_str(&serde_json::to_string(&owner).unwrap());
            source.push_str(");\n");
        }

        source.push_str("const __wake_resolved_shared__=Object.create(null);\n");
        for descriptor in shared {
            let requirement = serde_json::json!({
                "shareKey": descriptor.share_key,
                "requiredVersion": descriptor.config.required_version.as_deref().unwrap_or(&descriptor.package.version),
                "packageContext": descriptor.package.package_context,
                "buildVariant": descriptor.package.build_variant,
                "scope": descriptor.config.scope,
                "singleton": descriptor.config.singleton,
                "strict": descriptor.config.strict,
                "coherenceGroup": descriptor.config.coherence_group,
                "fallback": descriptor.config.fallback,
                "owner": descriptor.config.owner.as_ref().map(wake_federation_contract::ContainerName::as_str),
            });
            let resolved_key = format!("{}:{}", descriptor.config.scope, descriptor.share_key);
            source.push_str("__wake_resolved_shared__[");
            source.push_str(&serde_json::to_string(&resolved_key).unwrap());
            source.push_str("]=await __wake_federation_broker__.resolveShared(");
            source.push_str(&requirement.to_string());
            source.push_str(",{requester:");
            source
                .push_str(&serde_json::to_string(&format!("{}\0$host", federation.name)).unwrap());
            source.push_str("});\n");
        }
        let name = serde_json::to_string(federation.name.as_str()).unwrap();
        let id = serde_json::to_string(build_id.as_str()).unwrap();
        source.push_str(&format!(
            "const __wake_host_context__=Object.freeze({{runtimeAbi:'wake.federation.v1',container:Object.freeze({{name:{name},buildId:{id}}}),resolved:Object.freeze(__wake_resolved_shared__),resolve:(request)=>__wake_federation_broker__.resolveShared(request,{{requester:{name}+'\\0$host'}}),getSync:(shareKey,scope='default')=>{{const key=scope+':'+shareKey;if(!Object.prototype.hasOwnProperty.call(__wake_resolved_shared__,key))throw Object.assign(new Error('Wake host shared dependency was not initialized'),{{code:'FED_SHARE_UNSATISFIABLE',details:{{shareKey,scope}}}});return __wake_resolved_shared__[key]}}}});\nlet __wake_contexts__=globalThis[Symbol.for('wake.federation.share-contexts.v1')];if(!__wake_contexts__){{__wake_contexts__=Object.create(null);Object.defineProperty(globalThis,Symbol.for('wake.federation.share-contexts.v1'),{{value:__wake_contexts__,configurable:false}})}}__wake_contexts__[{name}]=__wake_host_context__;\n"
        ));
    }

    source.push_str("await import(new URL(");
    source.push_str(&serde_json::to_string(&format!("./{application_entry}")).unwrap());
    source.push_str(",import.meta.url));\n");
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev_setup_file_system(root: &Path, setup: &DevFederationSetup) -> Arc<dyn FileSystem> {
        let root = super::super::canonical_project_root(root).unwrap();
        Arc::new(
            wake_common::OwnedOverlayFileSystem::try_new(
                Arc::new(OsFileSystem),
                root.join(".wake"),
                setup.generated_inputs.clone(),
            )
            .unwrap(),
        )
    }

    fn assert_csp_runtime_bootstrap(source: &str) {
        let options = source
            .find("Symbol.for('wake.federation.runtime-options.v1')")
            .expect("stable runtime options symbol");
        let broker = source
            .find("getFederationRuntime(__wake_federation_runtime_options__())")
            .expect("broker created from the resolved runtime options");
        assert!(
            options < broker,
            "runtime options must be resolved before broker creation"
        );
        assert!(
            source.contains("document.querySelectorAll('script[type=\"module\"][src]')"),
            "{source}"
        );
        assert!(!source.contains("getFederationRuntime({global:globalThis})"));
    }

    #[test]
    fn runtime_bootstrap_nonce_source_is_strict_and_explicit_options_take_precedence() {
        assert_csp_runtime_bootstrap(RUNTIME_BOOTSTRAP_SOURCE);
        let node_available = std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success());
        if !node_available {
            eprintln!("node unavailable; structural CSP bootstrap assertions completed");
            return;
        }

        let directory = tempfile::tempdir().unwrap();
        let execute = |name: &str, setup: &str| {
            let path = directory.path().join(format!("{name}.mjs"));
            let source = format!(
                r#"{setup}
let observed=null,error=null;
function getFederationRuntime(options){{observed=options;return {{}}}}
try {{
{RUNTIME_BOOTSTRAP_SOURCE}
}} catch(cause) {{error={{code:cause?.code??null,message:String(cause?.message??cause)}}}}
process.stdout.write(JSON.stringify({{nonce:observed?.nonce??null,hasNonce:observed!==null&&Object.prototype.hasOwnProperty.call(observed,'nonce'),usesPageGlobal:observed?.global===globalThis,selector:globalThis.__wakeSelector??null,error}}));
"#
            );
            std::fs::write(&path, source).unwrap();
            let executed = std::process::Command::new("node")
                .arg(&path)
                .output()
                .expect("node must execute the generated federation runtime bootstrap");
            assert!(
                executed.status.success(),
                "{}",
                String::from_utf8_lossy(&executed.stderr)
            );
            serde_json::from_slice::<serde_json::Value>(&executed.stdout).unwrap()
        };

        let explicit = execute(
            "explicit",
            r#"const optionsSymbol=Symbol.for('wake.federation.runtime-options.v1');
globalThis[optionsSymbol]=Object.freeze({nonce:'explicit-Nonce_123'});
globalThis.document={querySelectorAll(selector){globalThis.__wakeSelector=selector;return [{src:import.meta.url,nonce:'script-Nonce_456'}]}};"#,
        );
        assert_eq!(explicit["nonce"], "explicit-Nonce_123");
        assert_eq!(explicit["hasNonce"], true);
        assert_eq!(explicit["usesPageGlobal"], true);
        assert_eq!(explicit["selector"], serde_json::Value::Null);
        assert_eq!(explicit["error"], serde_json::Value::Null);

        let script = execute(
            "script",
            r#"globalThis.document={querySelectorAll(selector){globalThis.__wakeSelector=selector;return [{src:'https://other.test/bootstrap.mjs',nonce:'other-Nonce_1'},{src:import.meta.url,nonce:'script-Nonce_456'}]}};"#,
        );
        assert_eq!(script["nonce"], "script-Nonce_456");
        assert_eq!(script["selector"], "script[type=\"module\"][src]");
        assert_eq!(script["error"], serde_json::Value::Null);

        let missing = execute(
            "missing",
            r#"globalThis.document={querySelectorAll(){return [{src:import.meta.url,nonce:''}]}};"#,
        );
        assert_eq!(missing["nonce"], serde_json::Value::Null);
        assert_eq!(missing["hasNonce"], false);
        assert_eq!(missing["usesPageGlobal"], true);
        assert_eq!(missing["error"], serde_json::Value::Null);

        for (name, setup) in [
            (
                "extra-transport",
                "globalThis[Symbol.for('wake.federation.runtime-options.v1')]={nonce:'valid-Nonce_1',transport:{}};",
            ),
            (
                "missing-nonce",
                "globalThis[Symbol.for('wake.federation.runtime-options.v1')]={};",
            ),
            (
                "invalid-nonce",
                "globalThis[Symbol.for('wake.federation.runtime-options.v1')]={nonce:'not a nonce'};",
            ),
            (
                "accessor-nonce",
                "globalThis[Symbol.for('wake.federation.runtime-options.v1')]=Object.defineProperty({},'nonce',{get(){throw new Error('getter executed')}});",
            ),
            (
                "accessor-options-symbol",
                "Object.defineProperty(globalThis,Symbol.for('wake.federation.runtime-options.v1'),{get(){throw new Error('getter executed')}});",
            ),
        ] {
            let invalid = execute(name, setup);
            assert_eq!(invalid["nonce"], serde_json::Value::Null, "{invalid:#}");
            assert_eq!(
                invalid["error"]["code"], "FED_CONFIG_INVALID",
                "{invalid:#}"
            );
            assert!(
                !invalid["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains("getter executed"),
                "{invalid:#}"
            );
        }
    }

    fn write(path: &Path, value: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, value).unwrap();
    }

    fn build_project(root: &Path, outdir: &str, source_map: bool) -> super::super::BuildResult {
        super::super::build(
            BuildOptions {
                project: super::super::ProjectOptions {
                    cwd: Some(root.to_path_buf()),
                    config_path: None,
                },
                entry: Some(PathBuf::from("src/main.ts")),
                outdir: Some(PathBuf::from(outdir)),
                source_map,
                ..BuildOptions::default()
            },
            &super::super::CancellationToken::default(),
        )
        .unwrap()
    }

    fn read_manifest(root: &Path, outdir: &str) -> Manifest {
        serde_json::from_slice(&std::fs::read(root.join(outdir).join(MANIFEST_FILE)).unwrap())
            .unwrap()
    }

    fn files_under(root: &Path) -> Vec<PathBuf> {
        fn visit(directory: &Path, files: &mut Vec<PathBuf>) {
            if !directory.is_dir() {
                return;
            }
            let mut entries = std::fs::read_dir(directory)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            entries.sort();
            for path in entries {
                if path.is_dir() {
                    visit(&path, files);
                } else {
                    files.push(path);
                }
            }
        }

        let mut files = Vec::new();
        visit(root, &mut files);
        files
    }

    fn build_options_for(root: &Path) -> BuildOptions {
        BuildOptions {
            project: super::super::ProjectOptions {
                cwd: Some(root.to_path_buf()),
                config_path: None,
            },
            entry: Some(PathBuf::from("src/main.ts")),
            write: false,
            ..BuildOptions::default()
        }
    }

    fn owned_tree_snapshot(tree: &OwnedFileTree) -> BTreeMap<PathBuf, Vec<u8>> {
        tree.iter()
            .map(|(path, contents)| (path.as_path().to_path_buf(), contents.to_vec()))
            .collect()
    }

    fn valid_remote_lock(manifest_url: &str, build_id: &str) -> FederationLock {
        let mut lock = FederationLock::new();
        lock.remotes.insert(
            wake_federation_contract::ContainerName::new("catalog"),
            wake_federation_contract::RemoteRef {
                manifest_url: manifest_url.to_owned(),
                build_id: BuildId::new(build_id),
                manifest_integrity: digest_bytes(b"manifest").1,
                has_exposes: false,
                types_integrity: None,
                allowed_assets: BTreeMap::from([(
                    "https://catalog.example/assets/remote.mjs".to_owned(),
                    digest_bytes(b"remote").1,
                )]),
            },
        );
        lock
    }

    struct CountingFileSystem {
        lock_path: PathBuf,
        lock_reads: std::sync::atomic::AtomicUsize,
    }

    impl CountingFileSystem {
        fn new(lock_path: PathBuf) -> Self {
            let lock_path = wake_common::OsFileSystem
                .canonicalize(&lock_path)
                .unwrap_or(lock_path);
            Self {
                lock_path,
                lock_reads: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn lock_reads(&self) -> usize {
            self.lock_reads.load(std::sync::atomic::Ordering::Acquire)
        }
    }

    impl FileSystem for CountingFileSystem {
        fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
            wake_common::OsFileSystem.canonicalize(path)
        }

        fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
            std::fs::read_to_string(path)
        }

        fn read(&self, path: &Path) -> std::io::Result<Vec<u8>> {
            if wake_common::OsFileSystem
                .canonicalize(path)
                .is_ok_and(|path| path == self.lock_path)
            {
                self.lock_reads
                    .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
            }
            std::fs::read(path)
        }

        fn exists(&self, path: &Path) -> bool {
            path.exists()
        }

        fn is_file(&self, path: &Path) -> bool {
            path.is_file()
        }

        fn is_dir(&self, path: &Path) -> bool {
            path.is_dir()
        }

        fn read_dir(&self, path: &Path) -> std::io::Result<Vec<PathBuf>> {
            std::fs::read_dir(path)?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect()
        }
    }

    #[test]
    fn production_render_is_pure_complete_and_slot_ordered() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        let config = r#"[federation]
enabled = true
name = "catalog"

[federation.exposes.Zeta]
entry = "src/zeta.ts"
mode = "generic"

[federation.exposes.Alpha]
entry = "src/alpha.ts"
mode = "generic"

[federation.shared.zod]

[federation.shared.react]
"#;
        write(&root.join("wake.config.toml"), config);
        write(&root.join("src/main.ts"), "export const main = true;\n");
        let options = build_options_for(root);
        let prepared = super::super::prepare_build(&options).unwrap();
        let physical_wake = root.join(".wake");
        assert!(!physical_wake.exists());

        let rendered = render_production_inputs(&prepared, &options).unwrap();
        assert!(
            !physical_wake.exists(),
            "render created the physical .wake tree"
        );
        let second_project = tempfile::tempdir().unwrap();
        write(&second_project.path().join("wake.config.toml"), config);
        write(
            &second_project.path().join("src/main.ts"),
            "export const main = true;\n",
        );
        let second_options = build_options_for(second_project.path());
        let second_prepared = super::super::prepare_build(&second_options).unwrap();
        let second_rendered = render_production_inputs(&second_prepared, &second_options).unwrap();
        assert_eq!(
            owned_tree_snapshot(rendered.files()),
            owned_tree_snapshot(second_rendered.files()),
            "production wrappers must not encode their physical project root"
        );
        let inventory = rendered
            .files()
            .inventory()
            .map(|path| path.as_path().to_path_buf())
            .collect::<Vec<_>>();
        assert_eq!(inventory.len(), 4, "{inventory:?}");
        assert!(
            inventory
                .iter()
                .any(|path| path.ends_with("container/expose-0000-Alpha.mjs"))
        );
        assert!(
            inventory
                .iter()
                .any(|path| path.ends_with("container/expose-0001-Zeta.mjs"))
        );
        let container_path = rendered
            .files()
            .inventory()
            .find(|path| {
                path.as_path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("container-"))
            })
            .unwrap();
        let container = std::str::from_utf8(rendered.files().get(container_path).unwrap()).unwrap();
        assert!(
            container.find("./expose-0000-Alpha.mjs").unwrap()
                < container.find("./expose-0001-Zeta.mjs").unwrap()
        );
        let shared_path = rendered
            .files()
            .inventory()
            .find(|path| {
                path.as_path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("shared-"))
            })
            .unwrap();
        let shared = std::str::from_utf8(rendered.files().get(shared_path).unwrap()).unwrap();
        assert!(
            shared.find("s0 from \"react\"").unwrap() < shared.find("s1 from \"zod\"").unwrap()
        );

        let rogue = physical_wake.join("federation/generated/rogue.mjs");
        write(&rogue, "physical rogue");
        let before_paths = files_under(&physical_wake);
        let before_bytes = std::fs::read(&rogue).unwrap();
        let rerendered = render_production_inputs(&prepared, &options).unwrap();
        assert_eq!(files_under(&physical_wake), before_paths);
        assert_eq!(std::fs::read(&rogue).unwrap(), before_bytes);
        let overlay: Arc<dyn FileSystem> = Arc::new(
            wake_common::OwnedOverlayFileSystem::try_new(
                Arc::new(OsFileSystem),
                root.join(".wake"),
                rerendered.files().clone(),
            )
            .unwrap(),
        );
        assert!(!overlay.exists(&rogue));
        for path in rerendered.files().inventory() {
            assert!(overlay.is_file(&root.join(".wake").join(path.as_path())));
        }

        write(&root.join("src/alpha.ts"), "export const alpha = true;\n");
        write(&root.join("src/zeta.ts"), "export const zeta = true;\n");
        for package in ["react", "zod"] {
            write(
                &root.join("node_modules").join(package).join("package.json"),
                &format!(r#"{{"name":"{package}","version":"1.0.0","module":"index.js"}}"#),
            );
            write(
                &root.join("node_modules").join(package).join("index.js"),
                "export const value = true;\n",
            );
        }
        let bound = bind_production_generation(&prepared, &options, &rerendered, overlay)
            .unwrap()
            .unwrap();
        let generated_aliases = &bound.container.options.resolve.alias[prepared.aliases.len()..];
        assert_eq!(
            generated_aliases
                .iter()
                .map(|(alias, _)| alias.as_str())
                .collect::<Vec<_>>(),
            [
                "@@@/__wake_federation_expose_0000",
                "@@@/__wake_federation_expose_0001"
            ]
        );
        assert!(generated_aliases[0].1.ends_with("src/alpha.ts"));
        assert!(generated_aliases[1].1.ends_with("src/zeta.ts"));
        let shared_provider = bound.shared_provider.as_ref().unwrap();
        let shared_aliases = &shared_provider.options.resolve.alias[prepared.aliases.len()..];
        assert_eq!(
            shared_aliases
                .iter()
                .map(|(alias, _)| alias.as_str())
                .collect::<Vec<_>>(),
            ["react", "zod"]
        );
    }

    #[test]
    fn disabled_production_render_has_no_files_or_bound_generation() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        write(&root.join("src/main.ts"), "export const main = true;\n");
        let options = build_options_for(root);
        let prepared = super::super::prepare_build(&options).unwrap();
        let rendered = render_production_inputs(&prepared, &options).unwrap();
        assert!(rendered.files().is_empty());
        assert!(
            bind_production_generation(
                &prepared,
                &options,
                &rendered,
                prepared.generation.file_system(),
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn enabled_production_render_without_shared_omits_the_provider_input() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        write(
            &root.join("wake.config.toml"),
            r#"[federation]
enabled = true
name = "catalog"

[federation.exposes.Button]
entry = "src/button.ts"
mode = "generic"
"#,
        );
        write(&root.join("src/main.ts"), "export const main = true;\n");
        let options = build_options_for(root);
        let prepared = super::super::prepare_build(&options).unwrap();
        let rendered = render_production_inputs(&prepared, &options).unwrap();
        let inventory = rendered
            .files()
            .inventory()
            .map(|path| path.as_path().to_path_buf())
            .collect::<Vec<_>>();
        assert_eq!(inventory.len(), 2, "{inventory:?}");
        assert!(inventory.iter().all(|path| {
            !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("shared-"))
        }));
        assert!(!root.join(".wake").exists());
    }

    #[test]
    fn production_bind_reads_and_captures_the_lock_once() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        let manifest_url = "https://catalog.example/wake-federation.json";
        write(
            &root.join("wake.config.toml"),
            &format!(
                r#"[federation]
enabled = true
name = "shell"

[federation.remotes.catalog]
manifest_url = "{manifest_url}"
"#
            ),
        );
        write(&root.join("src/main.ts"), "export const main = true;\n");
        let lock_path = root.join(LOCK_FILE);
        let first_lock = valid_remote_lock(manifest_url, "remote-build-a");
        write(
            &lock_path,
            &serde_json::to_string_pretty(&first_lock).unwrap(),
        );
        let options = build_options_for(root);
        let prepared = super::super::prepare_build(&options).unwrap();
        let rendered = render_production_inputs(&prepared, &options).unwrap();
        let counted = Arc::new(CountingFileSystem::new(lock_path.clone()));
        let generation_fs: Arc<dyn FileSystem> = counted.clone();
        let bound = bind_production_generation(&prepared, &options, &rendered, generation_fs)
            .unwrap()
            .unwrap();
        assert_eq!(counted.lock_reads(), 1);

        let second_lock = valid_remote_lock(manifest_url, "remote-build-b");
        write(
            &lock_path,
            &serde_json::to_string_pretty(&second_lock).unwrap(),
        );
        let captured = bound.lock.as_deref().unwrap();
        let bootstrap = bootstrap_source(
            &prepared,
            "bundle.js",
            &BuildId::new("local-build"),
            &[],
            None,
            Some(captured),
        )
        .unwrap();
        assert!(bootstrap.contains("remote-build-a"), "{bootstrap}");
        assert!(!bootstrap.contains("remote-build-b"), "{bootstrap}");
        assert_eq!(counted.lock_reads(), 1);
    }

    #[test]
    fn all_follow_development_bootstrap_requires_no_lock_snapshot() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        write(
            &root.join("wake.config.toml"),
            r#"[federation]
enabled = true
name = "shell"

[federation.remotes.catalog]
manifest_url = "http://127.0.0.1:4201/wake-federation.json"
dev_follow = true
"#,
        );
        write(&root.join("src/main.ts"), "export const main = true;\n");
        let options = build_options_for(root);
        let prepared = super::super::prepare_build(&options).unwrap();
        let counted = Arc::new(CountingFileSystem::new(root.join(LOCK_FILE)));
        let fs: Arc<dyn FileSystem> = counted.clone();
        let setup = prepare_dev(&prepared, &options, fs, None).unwrap();
        assert_eq!(counted.lock_reads(), 0);
        assert!(
            setup
                .build
                .bootstrap
                .as_deref()
                .unwrap()
                .contains("\"mode\":\"development\"")
        );
    }

    #[test]
    fn pinned_development_bootstrap_uses_the_callers_lock_snapshot() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        let manifest_url = "https://catalog.example/wake-federation.json";
        write(
            &root.join("wake.config.toml"),
            &format!(
                r#"[federation]
enabled = true
name = "shell"

[federation.remotes.catalog]
manifest_url = "{manifest_url}"
dev_follow = false
"#
            ),
        );
        write(&root.join("src/main.ts"), "export const main = true;\n");
        let options = build_options_for(root);
        let prepared = super::super::prepare_build(&options).unwrap();
        let captured = Arc::new(valid_remote_lock(manifest_url, "remote-build-a"));
        write(
            &root.join(LOCK_FILE),
            &serde_json::to_string_pretty(&valid_remote_lock(manifest_url, "remote-build-b"))
                .unwrap(),
        );
        let counted = Arc::new(CountingFileSystem::new(root.join(LOCK_FILE)));
        let fs: Arc<dyn FileSystem> = counted.clone();

        let setup = prepare_dev(&prepared, &options, fs, Some(captured)).unwrap();
        let bootstrap = setup.build.bootstrap.as_deref().unwrap();
        assert!(bootstrap.contains("remote-build-a"), "{bootstrap}");
        assert!(!bootstrap.contains("remote-build-b"), "{bootstrap}");
        assert_eq!(counted.lock_reads(), 0);
    }

    fn css_policy_container(
        mode: ExposeMode,
        allow_global_css: bool,
        css_owner: u32,
        unscoped: bool,
    ) -> BuiltContainer {
        let entry = wake_bundler::OutputChunk {
            name: "container".to_owned(),
            file_name: "container.js".to_owned(),
            code: "globalThis.__container = true;".to_owned(),
            kind: wake_bundler::ChunkKind::Initial,
            is_entry: true,
            chunk_id: 0,
            module_ids: vec![0],
            imports: Vec::new(),
            dynamic_imports: vec!["button.js".to_owned()],
            styles: Vec::new(),
            source_map: None,
        };
        let expose = wake_bundler::OutputChunk {
            name: "expose-0000-Button".to_owned(),
            file_name: "button.js".to_owned(),
            code: "globalThis.__button = true;".to_owned(),
            kind: wake_bundler::ChunkKind::Async,
            is_entry: false,
            chunk_id: 1,
            module_ids: vec![1],
            imports: Vec::new(),
            dynamic_imports: vec!["lazy.js".to_owned()],
            styles: (css_owner == 1)
                .then(|| "button.css".to_owned())
                .into_iter()
                .collect(),
            source_map: None,
        };
        let lazy = wake_bundler::OutputChunk {
            name: "lazy".to_owned(),
            file_name: "lazy.js".to_owned(),
            code: "globalThis.__lazy = true;".to_owned(),
            kind: wake_bundler::ChunkKind::Async,
            is_entry: false,
            chunk_id: 2,
            module_ids: vec![2],
            imports: Vec::new(),
            dynamic_imports: Vec::new(),
            styles: (css_owner == 2)
                .then(|| "button.css".to_owned())
                .into_iter()
                .collect(),
            source_map: None,
        };
        BuiltContainer {
            directory: "federation/catalog/container".to_owned(),
            output: BuildOutput {
                bundle: entry.code.clone(),
                module_count: 3,
                updated_module_count: 3,
                cached_module_count: 0,
                diagnostics: Vec::new(),
                chunks: vec![entry, expose, lazy],
                entry_chunk: 0,
                assets: vec![wake_bundler::OutputAsset {
                    file_name: "button.css".to_owned(),
                    bytes: b"body{color:red}".to_vec(),
                    is_css: true,
                    owner_module_ids: vec![css_owner],
                    unscoped_css_owner_module_ids: unscoped
                        .then_some(css_owner)
                        .into_iter()
                        .collect(),
                }],
            },
            exposes: BTreeMap::from([(
                ExposeKey::new("./Button"),
                BuiltExposeRoot {
                    config: ExposeConfig {
                        entry: "src/button.tsx".to_owned(),
                        mode,
                        scope: if mode == ExposeMode::Isolated {
                            "react17".to_owned()
                        } else {
                            "react18".to_owned()
                        },
                        shadow: if mode == ExposeMode::Isolated {
                            wake_federation_contract::ShadowMode::Open
                        } else {
                            wake_federation_contract::ShadowMode::None
                        },
                        allow_global_css,
                    },
                    chunk_name: "expose-0000-Button".to_owned(),
                    loader_export: "e0".to_owned(),
                },
            )]),
        }
    }

    #[test]
    fn host_rendered_css_policy_is_fail_closed_across_the_full_chunk_closure() {
        for owner in [1, 2] {
            let error = match materialize_container(
                "catalog",
                &BuildId::new("build-css-policy"),
                &css_policy_container(ExposeMode::HostRendered, false, owner, true),
                ProductionSourceMapMode::Hidden,
            ) {
                Ok(_) => panic!("unscoped CSS must fail the host-rendered boundary"),
                Err(error) => error,
            };
            assert_eq!(error.code, FederationErrorCode::ConfigInvalid.as_str());
            assert!(error.message.contains("button.css"), "{}", error.message);
        }

        for container in [
            css_policy_container(ExposeMode::HostRendered, true, 1, true),
            css_policy_container(ExposeMode::HostRendered, false, 1, false),
            css_policy_container(ExposeMode::Isolated, false, 1, true),
        ] {
            materialize_container(
                "catalog",
                &BuildId::new("build-css-policy"),
                &container,
                ProductionSourceMapMode::Hidden,
            )
            .expect("allowed CSS boundary");
        }
    }

    #[test]
    fn remote_build_emits_immutable_manifest_types_and_namespaced_maps() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        write(
            &root.join("wake.config.toml"),
            r#"[federation]
enabled = true
name = "catalog"

[federation.exposes.Button]
entry = "src/button.ts"
mode = "generic"
"#,
        );
        write(&root.join("src/main.ts"), "globalThis.__host = true;\n");
        write(
            &root.join("src/button.ts"),
            "export const label: string = 'federated'; export const load: () => Promise<typeof import('./lazy')> = () => import('./lazy');\n",
        );
        write(
            &root.join("src/lazy.ts"),
            "export const lazyLabel: string = 'lazy-federated';\n",
        );

        let mapped = build_project(root, "dist-mapped", true);
        let mapped_manifest = read_manifest(root, "dist-mapped");
        mapped_manifest.validate().unwrap();
        let button = &mapped_manifest.exposes[&ExposeKey::from("./Button")];
        let map = button.source_map.as_ref().expect("entry source map");
        let map_text = std::fs::read_to_string(
            root.join("dist-mapped")
                .join(map.url.trim_start_matches("./")),
        )
        .unwrap();
        assert!(map_text.contains(&format!("wake://catalog@{}/", mapped_manifest.build_id)));
        let remote_map = mapped_manifest
            .remote_entry_source_map
            .as_ref()
            .expect("embedded container entry source map");
        let remote_map_text = std::fs::read_to_string(
            root.join("dist-mapped")
                .join(remote_map.url.trim_start_matches("./")),
        )
        .unwrap();
        assert!(remote_map_text.contains(&format!("wake://catalog@{}/", mapped_manifest.build_id)));
        let async_map = button
            .asynchronous_assets
            .iter()
            .filter(|asset| asset.kind == AssetKind::SourceMap)
            .find_map(|asset| {
                let text = std::fs::read_to_string(
                    root.join("dist-mapped")
                        .join(asset.url.trim_start_matches("./")),
                )
                .unwrap();
                text.contains("src/lazy.ts").then_some((asset, text))
            })
            .expect("lazy chunk source map");
        let types = mapped_manifest.types.as_ref().expect("declaration bundle");
        assert_eq!(types.build_id, mapped_manifest.build_id);
        assert!(
            root.join("dist-mapped")
                .join(format!(
                    "federation/catalog/{}/types/index.d.ts",
                    mapped_manifest.build_id
                ))
                .is_file()
        );
        let remote_entry = std::fs::read_to_string(
            root.join("dist-mapped")
                .join(mapped_manifest.remote_entry.url.trim_start_matches("./")),
        )
        .unwrap();
        assert!(remote_entry.contains("registerContainer"));
        assert!(remote_entry.contains("Symbol.for('wake.federation.v1')"));
        let html = std::fs::read_to_string(root.join("dist-mapped/index.html")).unwrap();
        assert!(html.contains("type=\"module\" src=\"/wake-federation-bootstrap.mjs\""));
        assert!(!html.contains(&format!(
                "src=\"/{}\"",
                mapped_manifest.exposes[&ExposeKey::from("./Button")]
                    .entry
                    .url
            )));
        assert!(
            mapped
                .files
                .iter()
                .any(|file| file.kind == OutputFileKind::FederationManifest)
        );

        let hidden_root = root.join(format!(
            ".wake/federation/source-maps/catalog/{}",
            mapped_manifest.build_id
        ));
        assert!(
            !hidden_root.exists(),
            "public source maps must not also populate the hidden sink"
        );

        let plain = build_project(root, "dist-plain", false);
        let plain_manifest = read_manifest(root, "dist-plain");
        let plain_button = &plain_manifest.exposes[&ExposeKey::from("./Button")];
        assert_eq!(mapped_manifest.build_id, plain_manifest.build_id);
        assert_eq!(button.entry.content_hash, plain_button.entry.content_hash);
        assert!(plain_button.source_map.is_none());
        assert!(plain_manifest.remote_entry_source_map.is_none());
        assert!(
            plain_button
                .asynchronous_assets
                .iter()
                .all(|asset| asset.kind != AssetKind::SourceMap)
        );
        assert!(
            files_under(&root.join("dist-plain"))
                .iter()
                .all(|path| path.extension().and_then(|value| value.to_str()) != Some("map")),
            "hidden source maps must not be copied to the public outdir"
        );
        assert!(
            plain.files.iter().all(|file| {
                file.kind != OutputFileKind::SourceMap && !file.path.ends_with(".map")
            }),
            "hidden source maps must not be reported as public BuildResult files"
        );

        let hidden_maps = files_under(&hidden_root);
        assert!(hidden_maps.len() >= 3, "{hidden_maps:?}");
        let hidden_remote = hidden_root.join(
            mapped_manifest
                .remote_entry
                .url
                .rsplit('/')
                .next()
                .map(|name| format!("{name}.map"))
                .unwrap(),
        );
        let hidden_expose = hidden_root.join("container").join(format!(
            "{}.map",
            button.entry.url.rsplit('/').next().unwrap()
        ));
        assert_eq!(
            std::fs::read_to_string(hidden_remote).unwrap(),
            remote_map_text
        );
        assert_eq!(std::fs::read_to_string(hidden_expose).unwrap(), map_text);
        assert!(hidden_maps.iter().any(|path| {
            std::fs::read_to_string(path)
                .unwrap()
                .contains("src/lazy.ts")
        }));
        assert!(hidden_maps.iter().all(|path| {
            std::fs::read_to_string(path)
                .unwrap()
                .contains(&format!("wake://catalog@{}/", plain_manifest.build_id))
        }));

        let javascript_assets = |manifest: &Manifest| {
            let mut assets = BTreeMap::new();
            assets.insert(
                manifest.remote_entry.url.clone(),
                manifest.remote_entry.content_hash.clone(),
            );
            for expose in manifest.exposes.values() {
                for asset in std::iter::once(&expose.entry)
                    .chain(&expose.synchronous_assets)
                    .chain(&expose.asynchronous_assets)
                    .filter(|asset| asset.kind == AssetKind::JavaScript)
                {
                    assets.insert(asset.url.clone(), asset.content_hash.clone());
                }
            }
            assets
        };
        let mapped_javascript = javascript_assets(&mapped_manifest);
        assert_eq!(mapped_javascript, javascript_assets(&plain_manifest));
        for url in mapped_javascript.keys() {
            assert_eq!(
                std::fs::read(root.join("dist-mapped").join(url.trim_start_matches("./"))).unwrap(),
                std::fs::read(root.join("dist-plain").join(url.trim_start_matches("./"))).unwrap(),
                "hidden/public source map modes must not change JavaScript `{url}`"
            );
            let source =
                std::fs::read_to_string(root.join("dist-plain").join(url.trim_start_matches("./")))
                    .unwrap();
            assert!(!source.contains("sourceMappingURL"), "{url}: {source}");
        }
        assert!(async_map.1.contains("src/lazy.ts"));
    }

    #[test]
    fn canonical_build_identity_ignores_deployment_metadata_but_tracks_runtime_policy() {
        let remote_name = wake_federation_contract::ContainerName::new("shell");
        let expose_key = ExposeKey::new("./Button");
        let baseline = wake_federation_contract::FederationConfig {
            enabled: true,
            name: wake_federation_contract::ContainerName::new("catalog"),
            remotes: BTreeMap::from([(
                remote_name.clone(),
                wake_federation_contract::RemoteConfig {
                    manifest_url: "https://shell-a.example/wake-federation.json".to_owned(),
                    allowed_origins: vec!["https://shell-a.example".to_owned()],
                    dev_follow: true,
                },
            )]),
            exposes: BTreeMap::from([(
                expose_key.clone(),
                ExposeConfig {
                    entry: "src/button.ts".to_owned(),
                    mode: ExposeMode::Generic,
                    scope: "default".to_owned(),
                    shadow: wake_federation_contract::ShadowMode::None,
                    allow_global_css: false,
                },
            )]),
            shared: BTreeMap::new(),
        };
        let projected_build_id = |config: &wake_federation_contract::FederationConfig| {
            build_id_from_identity_inputs(&[
                ("container/output.mjs".to_owned(), b"same-output".to_vec()),
                (
                    "federation-policy.json".to_owned(),
                    canonical_federation_identity_bytes(config, &[], "chrome>=120"),
                ),
            ])
        };

        let mut deployment_only = baseline.clone();
        let remote = deployment_only.remotes.get_mut(&remote_name).unwrap();
        remote.manifest_url = "https://shell-b.example/cdn/manifest.json".to_owned();
        remote.allowed_origins = vec![
            "https://backup.example".to_owned(),
            "https://shell-b.example".to_owned(),
        ];
        remote.dev_follow = false;
        assert_eq!(
            projected_build_id(&baseline),
            projected_build_id(&deployment_only),
            "deployment URLs, origin policy, and development follow mode are not producer identity"
        );

        let mut runtime_policy = baseline.clone();
        runtime_policy.exposes.get_mut(&expose_key).unwrap().scope = "isolated".to_owned();
        assert_ne!(
            projected_build_id(&baseline),
            projected_build_id(&runtime_policy),
            "an exposed module's runtime scope is canonical build identity"
        );
    }

    #[test]
    fn production_host_fails_closed_without_remote_lock() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        write(
            &root.join("wake.config.toml"),
            r#"[federation]
enabled = true
name = "shell"

[federation.remotes.catalog]
manifest_url = "https://catalog.example/wake-federation.json"
"#,
        );
        write(&root.join("src/main.ts"), "globalThis.__host = true;\n");
        let error = super::super::build(
            BuildOptions {
                project: super::super::ProjectOptions {
                    cwd: Some(root.to_path_buf()),
                    config_path: None,
                },
                entry: Some(PathBuf::from("src/main.ts")),
                ..BuildOptions::default()
            },
            &super::super::CancellationToken::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "FED_LOCK_REQUIRED");
    }

    #[test]
    fn type_only_build_changes_keep_identical_javascript_on_distinct_build_scoped_urls() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        write(
            &root.join("wake.config.toml"),
            r#"[federation]
enabled = true
name = "catalog"

[federation.exposes.Button]
entry = "src/button.ts"
mode = "generic"
"#,
        );
        write(&root.join("src/main.ts"), "globalThis.__host = true;\n");
        write(
            &root.join("src/button.ts"),
            "export interface Props { label: string } export const value = 1;\n",
        );
        build_project(root, "dist-one", false);
        let first = read_manifest(root, "dist-one");

        write(
            &root.join("src/button.ts"),
            "export interface Props { label: string; count?: number } export const value = 1;\n",
        );
        build_project(root, "dist-two", false);
        let second = read_manifest(root, "dist-two");

        assert_ne!(first.build_id, second.build_id);
        let first_button = &first.exposes[&ExposeKey::from("./Button")];
        let second_button = &second.exposes[&ExposeKey::from("./Button")];
        assert_eq!(
            first_button.entry.content_hash, second_button.entry.content_hash,
            "type-only edits must leave the expose JavaScript bytes unchanged"
        );
        assert_ne!(first_button.entry.url, second_button.entry.url);
        assert!(first_button.entry.url.contains(first.build_id.as_str()));
        assert!(second_button.entry.url.contains(second.build_id.as_str()));
        assert_ne!(first.remote_entry.url, second.remote_entry.url);
        assert!(first.remote_entry.url.contains(first.build_id.as_str()));
        assert!(second.remote_entry.url.contains(second.build_id.as_str()));
        assert!(
            first
                .types
                .as_ref()
                .unwrap()
                .url
                .contains(first.build_id.as_str())
        );
        assert!(
            second
                .types
                .as_ref()
                .unwrap()
                .url
                .contains(second.build_id.as_str())
        );
    }

    #[test]
    fn application_only_change_keeps_the_federation_build_identity() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        write(
            &root.join("wake.config.toml"),
            r#"[federation]
enabled = true
name = "catalog"

[federation.exposes.Button]
entry = "src/button.ts"
mode = "generic"
"#,
        );
        write(
            &root.join("src/main.ts"),
            "globalThis.__application = 'first';\n",
        );
        write(
            &root.join("src/button.ts"),
            "export const button: string = 'stable';\n",
        );
        build_project(root, "dist-one", false);
        let first = read_manifest(root, "dist-one");
        let first_application = std::fs::read(root.join("dist-one/bundle.js")).unwrap();

        write(
            &root.join("src/main.ts"),
            "globalThis.__application = 'second';\n",
        );
        build_project(root, "dist-two", false);
        let second = read_manifest(root, "dist-two");
        let second_application = std::fs::read(root.join("dist-two/bundle.js")).unwrap();

        assert_eq!(first.build_id, second.build_id);
        assert_ne!(first_application, second_application);
    }

    #[test]
    fn failed_container_view_preserves_the_complete_last_published_generation() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        write(
            &root.join("wake.config.toml"),
            r#"[federation]
enabled = true
name = "catalog"

[federation.exposes.Button]
entry = "src/button.ts"
mode = "generic"
"#,
        );
        write(
            &root.join("src/main.ts"),
            "globalThis.__generation = 'application-v1';\n",
        );
        write(
            &root.join("src/button.ts"),
            "export const button = 'container-v1';\n",
        );
        build_project(root, "dist", false);
        let published = files_under(&root.join("dist"))
            .into_iter()
            .map(|path| {
                let relative = path.strip_prefix(root.join("dist")).unwrap().to_path_buf();
                (relative, std::fs::read(path).unwrap())
            })
            .collect::<BTreeMap<_, _>>();

        write(
            &root.join("src/main.ts"),
            "globalThis.__generation = 'application-v2-must-not-publish';\n",
        );
        write(
            &root.join("src/button.ts"),
            "import './broken.scss'; export const button: string = 'container-v2';\n",
        );
        write(&root.join("src/broken.scss"), "$color: red;\n");
        let error = super::super::build(
            BuildOptions {
                project: super::super::ProjectOptions {
                    cwd: Some(root.to_path_buf()),
                    config_path: None,
                },
                entry: Some(PathBuf::from("src/main.ts")),
                outdir: Some(PathBuf::from("dist")),
                ..BuildOptions::default()
            },
            &super::super::CancellationToken::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, "WAKE_BUILD");

        let after = files_under(&root.join("dist"))
            .into_iter()
            .map(|path| {
                let relative = path.strip_prefix(root.join("dist")).unwrap().to_path_buf();
                (relative, std::fs::read(path).unwrap())
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(after, published);
        assert!(
            after
                .values()
                .any(|bytes| { String::from_utf8_lossy(bytes).contains("application-v1") })
        );
        assert!(after.values().all(|bytes| {
            !String::from_utf8_lossy(bytes).contains("application-v2-must-not-publish")
        }));
    }

    #[test]
    fn two_exposes_share_one_common_chunk_with_independent_initial_closures() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        write(
            &root.join("wake.config.toml"),
            r#"[federation]
enabled = true
name = "catalog"

[federation.exposes.Alpha]
entry = "src/alpha.ts"
mode = "generic"

[federation.exposes.Beta]
entry = "src/beta.ts"
mode = "generic"
"#,
        );
        write(&root.join("src/main.ts"), "globalThis.__host = true;\n");
        write(
            &root.join("src/common.ts"),
            "export const common: string = 'COMMON_IMPLEMENTATION';\n",
        );
        write(
            &root.join("src/alpha.ts"),
            "import alphaImage from './alpha.png'; import sharedFont from './shared.woff2'; import { common } from './common'; export const alpha: string = 'A:' + common + alphaImage + sharedFont;\n",
        );
        write(
            &root.join("src/beta.ts"),
            "import betaImage from './beta.png'; import sharedFont from './shared.woff2'; import { common } from './common'; export const beta: string = 'B:' + common + betaImage + sharedFont;\n",
        );
        std::fs::write(root.join("src/alpha.png"), vec![1_u8; 5000]).unwrap();
        std::fs::write(root.join("src/beta.png"), vec![2_u8; 5000]).unwrap();
        std::fs::write(root.join("src/shared.woff2"), vec![3_u8; 5000]).unwrap();

        build_project(root, "dist", false);
        let manifest = read_manifest(root, "dist");
        let alpha = &manifest.exposes[&ExposeKey::from("./Alpha")];
        let beta = &manifest.exposes[&ExposeKey::from("./Beta")];
        assert_ne!(alpha.entry.url, beta.entry.url);
        assert!(!alpha.synchronous_assets.contains(&beta.entry));
        assert!(!beta.synchronous_assets.contains(&alpha.entry));
        assert!(!alpha.asynchronous_assets.contains(&beta.entry));
        assert!(!beta.asynchronous_assets.contains(&alpha.entry));

        let alpha_js = alpha
            .synchronous_assets
            .iter()
            .filter(|asset| asset.kind == AssetKind::JavaScript)
            .collect::<Vec<_>>();
        let beta_js = beta
            .synchronous_assets
            .iter()
            .filter(|asset| asset.kind == AssetKind::JavaScript)
            .collect::<Vec<_>>();
        let common = alpha_js
            .iter()
            .find(|asset| beta_js.iter().any(|candidate| candidate.url == asset.url))
            .expect("both expose closures must preload one common JavaScript chunk");
        let common_code =
            std::fs::read_to_string(root.join("dist").join(common.url.trim_start_matches("./")))
                .unwrap();
        assert!(common_code.contains("COMMON_IMPLEMENTATION"));
        let hidden_container_maps = files_under(&root.join(format!(
            ".wake/federation/source-maps/catalog/{}/container",
            manifest.build_id
        )));
        assert!(
            hidden_container_maps.iter().any(|path| {
                std::fs::read_to_string(path)
                    .unwrap()
                    .contains("src/common.ts")
            }),
            "the common shared chunk must have a hidden source map: {hidden_container_maps:?}"
        );

        let other_names = |expose: &ExposedModule| {
            expose
                .synchronous_assets
                .iter()
                .chain(&expose.asynchronous_assets)
                .filter(|asset| asset.kind == AssetKind::Other)
                .map(|asset| asset.url.rsplit('/').next().unwrap().to_owned())
                .collect::<BTreeSet<_>>()
        };
        let alpha_other = other_names(alpha);
        let beta_other = other_names(beta);
        assert!(
            alpha_other.iter().any(|name| name.starts_with("alpha.")),
            "{alpha_other:?}"
        );
        assert!(
            !alpha_other.iter().any(|name| name.starts_with("beta.")),
            "{alpha_other:?}"
        );
        assert!(
            beta_other.iter().any(|name| name.starts_with("beta.")),
            "{beta_other:?}"
        );
        assert!(
            !beta_other.iter().any(|name| name.starts_with("alpha.")),
            "{beta_other:?}"
        );
        let shared_alpha = alpha_other
            .iter()
            .find(|name| name.ends_with(".woff2"))
            .expect("alpha authorizes shared font");
        let shared_beta = beta_other
            .iter()
            .find(|name| name.ends_with(".woff2"))
            .expect("beta authorizes shared font");
        assert_eq!(shared_alpha, shared_beta);

        let remote_entry = std::fs::read_to_string(
            root.join("dist")
                .join(manifest.remote_entry.url.trim_start_matches("./")),
        )
        .unwrap();
        assert!(remote_entry.contains("loadFederatedAsset"));
        assert!(remote_entry.contains("'./__wake_container__'"));
        assert!(remote_entry.contains("[BUILD_ID]"));
    }

    #[test]
    fn explicit_shared_package_becomes_host_provider_and_remote_fallback() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        write(
            &root.join("wake.config.toml"),
            r#"[federation]
enabled = true
name = "catalog"

[federation.exposes.Button]
entry = "src/button.ts"
mode = "generic"

[federation.shared.react]
required_version = "18.2.0"
singleton = true
strict = true
fallback = true
coherence_group = "react18"
owner = "catalog"

[define]
WAKE_SHARED_SENTINEL = '"configured-provider"'
"#,
        );
        write(
            &root.join("node_modules/react/package.json"),
            r#"{"name":"react","version":"18.2.0","module":"index.js","exports":{".":"./index.js"}}"#,
        );
        write(
            &root.join("node_modules/react/index.js"),
            "export const version = '18.2.0'; export const environment = process.env.NODE_ENV; export const sentinel = WAKE_SHARED_SENTINEL; export default { version, environment, sentinel };\n",
        );
        write(
            &root.join("src/main.ts"),
            "import React from 'react'; globalThis.__react = React;\n",
        );
        write(
            &root.join("src/button.ts"),
            "import React from 'react'; export const version: string = React.version;\n",
        );

        let build = build_project(root, "dist", false);
        let manifest = read_manifest(root, "dist");
        assert_eq!(manifest.shared.offers.len(), 1);
        assert_eq!(manifest.shared.requirements.len(), 1);
        assert_eq!(manifest.shared.offers[0].package.version, "18.2.0");
        assert!(manifest.shared.offers[0].asset.is_some());
        let provider = manifest.shared.offers[0].asset.as_ref().unwrap();
        let provider_code = std::fs::read_to_string(
            root.join("dist")
                .join(provider.url.trim_start_matches("./")),
        )
        .unwrap();
        assert!(provider_code.contains("production"), "{provider_code}");
        assert!(
            provider_code.contains("configured-provider"),
            "{provider_code}"
        );
        assert!(!provider_code.contains("development"), "{provider_code}");
        assert!(
            !provider_code.contains("sourceMappingURL"),
            "{provider_code}"
        );
        assert!(
            !root
                .join("dist")
                .join(format!("{}.map", provider.url.trim_start_matches("./")))
                .is_file(),
            "the default shared fallback map must not be publicly deployed"
        );
        let hidden_shared_maps = files_under(&root.join(format!(
            ".wake/federation/source-maps/catalog/{}/shared",
            manifest.build_id
        )));
        assert_eq!(hidden_shared_maps.len(), 1, "{hidden_shared_maps:?}");
        let shared_map = std::fs::read_to_string(&hidden_shared_maps[0]).unwrap();
        assert!(
            shared_map.contains("node_modules/react/index.js"),
            "{shared_map}"
        );
        assert!(shared_map.contains(&format!("wake://catalog@{}/", manifest.build_id)));
        assert!(build.files.iter().all(|file| {
            file.kind != OutputFileKind::SourceMap && !file.path.ends_with(".map")
        }));
        let bootstrap = std::fs::read_to_string(root.join("dist").join(BOOTSTRAP_FILE)).unwrap();
        assert_csp_runtime_bootstrap(&bootstrap);
        assert!(bootstrap.contains("registerHostShared"));
        assert!(bootstrap.contains("__wake_resolved_shared__"));
        let expose = &manifest.exposes[&ExposeKey::from("./Button")];
        let code = std::fs::read_to_string(
            root.join("dist")
                .join(expose.entry.url.trim_start_matches("./")),
        )
        .unwrap();
        assert!(!code.contains("18.2.0"));
        let remote_entry = std::fs::read_to_string(
            root.join("dist")
                .join(manifest.remote_entry.url.trim_start_matches("./")),
        )
        .unwrap();
        assert!(remote_entry.contains("wake.federation.share-contexts.v1"));
    }

    #[test]
    fn development_bootstrap_registers_remotes_before_a_shared_host_entry() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        write(
            &root.join("wake.config.toml"),
            r#"[federation]
enabled = true
name = "shell"

[federation.remotes.catalog]
manifest_url = "http://127.0.0.1:4201/wake-federation.json"
allowed_origins = ["http://127.0.0.1:4201"]

[federation.shared.react]
required_version = "18.2.0"
singleton = true
strict = true
fallback = true
coherence_group = "react18"
"#,
        );
        write(
            &root.join("node_modules/react/package.json"),
            r#"{"name":"react","version":"18.2.0","module":"index.js","exports":{".":"./index.js"}}"#,
        );
        write(
            &root.join("node_modules/react/index.js"),
            "export const version = '18.2.0'; export default { version };\n",
        );
        write(
            &root.join("src/main.ts"),
            "import React from 'react'; import('catalog/Button').then(value => globalThis.__remote = value); globalThis.__react = React;\n",
        );

        let options = BuildOptions {
            project: super::super::ProjectOptions {
                cwd: Some(root.to_path_buf()),
                config_path: None,
            },
            entry: Some(PathBuf::from("src/main.ts")),
            write: false,
            ..BuildOptions::default()
        };
        let prepared = super::super::prepare_build(&options).unwrap();
        let setup =
            prepare_dev(&prepared, &options, prepared.generation.file_system(), None).unwrap();
        let generated_fs = dev_setup_file_system(root, &setup);
        assert!(setup.build.enabled);
        assert_eq!(setup.build.remotes, ["catalog"]);
        assert_eq!(setup.build.entry_export, None);
        assert_eq!(
            setup.build.application_loader_export.as_deref(),
            Some(APPLICATION_LOADER_EXPORT)
        );
        assert!(setup.build.shared_fallback.is_some());
        let bootstrap = setup.build.bootstrap.as_deref().unwrap();
        assert_csp_runtime_bootstrap(bootstrap);
        assert!(bootstrap.contains("getFederationRuntime"));
        assert!(bootstrap.contains("\"mode\":\"development\""));
        assert!(bootstrap.contains("http://127.0.0.1:4201/wake-federation.json"));
        assert!(bootstrap.contains("../../wake-federation.json"));
        let container_entry = generated_fs.read_to_string(&setup.entry).unwrap();
        assert!(container_entry.contains("export const __wakeApp=()=>import("));
        assert!(!container_entry.contains("__wakeApp();"));
        let wrapper_path = &setup
            .aliases
            .iter()
            .find(|(alias, _)| alias == DEV_STANDALONE_ALIAS)
            .expect("standalone application wrapper alias")
            .1;
        let wrapper = generated_fs.read_to_string(wrapper_path).unwrap();
        assert!(wrapper.contains("registerHostShared"));
        assert!(wrapper.contains("resolveShared"));
        assert!(wrapper.contains("wake.federation.exposes.v1"));
        assert!(wrapper.contains("__wake_shared_loader__"));
        assert!(!wrapper.contains("import * as __wake_local_shared__"));
        assert!(wrapper.contains(DEV_APP_ALIAS));
        assert!(
            setup
                .aliases
                .iter()
                .all(|(_, path)| generated_fs.is_file(path))
        );
    }

    #[test]
    fn development_shared_fallback_remains_one_self_contained_dynamic_root() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        write(
            &root.join("wake.config.toml"),
            r#"[federation]
enabled = true
name = "catalog"

[federation.exposes.Button]
entry = "src/button.ts"
mode = "generic"

[federation.shared.singleton-lib]
required_version = "1.0.0"
singleton = true
strict = true
fallback = true

[federation.shared.peer-lib]
required_version = "1.0.0"
singleton = true
strict = true
fallback = true
"#,
        );
        write(
            &root.join("node_modules/singleton-lib/package.json"),
            r#"{"name":"singleton-lib","version":"1.0.0","module":"index.js","exports":{".":"./index.js"}}"#,
        );
        write(
            &root.join("node_modules/singleton-lib/index.js"),
            "import {peer} from 'peer-lib'; export const value = 'local-provider-' + peer;\n",
        );
        write(
            &root.join("node_modules/peer-lib/package.json"),
            r#"{"name":"peer-lib","version":"1.0.0","module":"index.js","exports":{".":"./index.js"}}"#,
        );
        write(
            &root.join("node_modules/peer-lib/index.js"),
            "import * as singleton from 'singleton-lib'; export const peer = typeof singleton;\n",
        );
        write(
            &root.join("src/main.ts"),
            "import {value} from 'singleton-lib'; import {peer} from 'peer-lib'; globalThis.__hostValue = value + peer;\n",
        );
        write(
            &root.join("src/button.ts"),
            "import {value} from 'singleton-lib'; export const buttonValue = value;\n",
        );

        let options = BuildOptions {
            project: super::super::ProjectOptions {
                cwd: Some(root.to_path_buf()),
                config_path: None,
            },
            entry: Some(PathBuf::from("src/main.ts")),
            write: false,
            ..BuildOptions::default()
        };
        let prepared = super::super::prepare_build(&options).unwrap();
        let setup =
            prepare_dev(&prepared, &options, prepared.generation.file_system(), None).unwrap();
        let fallback = setup
            .build
            .shared_fallback
            .as_ref()
            .expect("configured shared fallback");
        let mut aliases = prepared.aliases.clone();
        aliases.extend(setup.aliases.clone());
        let bundler_options = BundlerBuildOptions {
            project_root: Some(prepared.root.clone()),
            resolve: ResolveOptions {
                alias: aliases,
                conditions: shared_resolution_conditions(true),
                ..ResolveOptions::default()
            },
            define: super::super::build_defines(&prepared.config, true),
            extract_css: true,
            public_path: prepared.config.public_path().to_owned(),
            source_map: true,
            css_in_js: true,
            code_splitting: true,
            jsx: JsxOptions {
                development: true,
                import_source: prepared.config.react.jsx_import_source.clone(),
            },
            federation: FederationBuildPlan {
                remotes: setup.build.remotes.clone(),
                shared: setup.build.shared.clone(),
                shared_fallback_roots: vec![
                    setup
                        .build
                        .shared_fallback_root
                        .clone()
                        .expect("configured shared fallback root"),
                ],
                entry_export: Some(FederationEntryExport::build_scoped(
                    &setup.build.container_name,
                    CONTAINER_EXPOSE,
                )),
                expose_roots: setup
                    .build
                    .exposes
                    .iter()
                    .map(|expose| (expose.chunk_name.clone(), expose.key.as_str().to_owned()))
                    .collect(),
            },
            target_env: super::super::resolve_target_env(&prepared.config, &prepared.root).unwrap(),
            ..BundlerBuildOptions::default()
        };
        let mut session = BuildSession::new(dev_setup_file_system(root, &setup), bundler_options);
        let output = session.build_entry(&setup.entry);

        assert!(!output.has_errors(), "{:?}", output.diagnostics);
        let fallback_chunk = output
            .chunks
            .iter()
            .find(|chunk| chunk.name == fallback.chunk_name)
            .expect("shared fallback retains its declared dynamic-root name");
        assert!(!fallback_chunk.is_entry);
        assert!(
            !fallback_chunk.code.contains("__wake_require__.shared("),
            "SharedFallback coherence members must resolve inside their own static closure: {}",
            fallback_chunk.code
        );
        assert!(
            output
                .chunks
                .iter()
                .filter(|chunk| chunk.name != fallback.chunk_name)
                .any(|chunk| chunk.code.contains("__wake_require__.shared(")),
            "the standalone application/expose must retain broker-owned shared imports"
        );
        assert!(
            fallback_chunk.imports.is_empty(),
            "fallback imports: {:?}",
            fallback_chunk.imports
        );
        assert!(
            fallback_chunk.dynamic_imports.is_empty(),
            "fallback dynamic imports: {:?}",
            fallback_chunk.dynamic_imports
        );
        assert!(
            fallback_chunk.styles.is_empty(),
            "fallback styles: {:?}",
            fallback_chunk.styles
        );
        assert!(
            output.chunks.iter().all(|chunk| chunk.name != "shared"),
            "the provider must not be reclassified as a multi-owner shared bucket: {:?}",
            output
                .chunks
                .iter()
                .map(|chunk| (&chunk.name, &chunk.file_name))
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn remote_singleton_owner_is_prepared_after_host_offers_and_before_resolution() {
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        write(
            &root.join("wake.config.toml"),
            r#"[federation]
enabled = true
name = "shell"

[federation.remotes.catalog]
manifest_url = "http://127.0.0.1:4201/wake-federation.json"

[federation.shared.react]
required_version = "18.2.0"
singleton = true
strict = true
fallback = true
owner = "catalog"
"#,
        );
        write(
            &root.join("node_modules/react/package.json"),
            r#"{"name":"react","version":"18.2.0","module":"index.js","exports":{".":"./index.js"}}"#,
        );
        write(
            &root.join("node_modules/react/index.js"),
            "export const version = '18.2.0'; export default { version };\n",
        );
        write(
            &root.join("src/main.ts"),
            "import React from 'react'; globalThis.__react = React;\n",
        );
        let options = BuildOptions {
            project: super::super::ProjectOptions {
                cwd: Some(root.to_path_buf()),
                config_path: None,
            },
            entry: Some(PathBuf::from("src/main.ts")),
            write: false,
            ..BuildOptions::default()
        };
        let prepared = super::super::prepare_build(&options).unwrap();
        let setup =
            prepare_dev(&prepared, &options, prepared.generation.file_system(), None).unwrap();
        let generated_fs = dev_setup_file_system(root, &setup);
        let wrapper_path = &setup
            .aliases
            .iter()
            .find(|(alias, _)| alias == DEV_STANDALONE_ALIAS)
            .expect("standalone application wrapper alias")
            .1;
        let wrapper = generated_fs.read_to_string(wrapper_path).unwrap();
        let offers = wrapper.find("registerHostShared").unwrap();
        let prepare = wrapper.find("prepareRemote(\"catalog\")").unwrap();
        let resolve = wrapper.find("resolveShared").unwrap();
        assert!(offers < prepare && prepare < resolve, "{wrapper}");
    }

    #[test]
    fn shared_build_variant_is_path_independent_and_changes_with_compiler_inputs() {
        let make = |root: &Path, define: Option<&str>| {
            let mut config = String::from("[federation]\nenabled = true\nname = \"shell\"\n");
            if let Some(value) = define {
                config.push_str("\n[define]\nFEATURE = ");
                config.push_str(&serde_json::to_string(value).unwrap());
                config.push('\n');
            }
            write(&root.join("wake.config.toml"), &config);
            write(&root.join("src/main.ts"), "export const value = 1;\n");
            let options = BuildOptions {
                project: super::super::ProjectOptions {
                    cwd: Some(root.to_path_buf()),
                    config_path: None,
                },
                entry: Some(PathBuf::from("src/main.ts")),
                write: false,
                ..BuildOptions::default()
            };
            let prepared = super::super::prepare_build(&options).unwrap();
            shared_build_variant(&prepared, false).unwrap()
        };
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let changed = tempfile::tempdir().unwrap();
        assert_eq!(make(first.path(), None), make(second.path(), None));
        assert_ne!(make(first.path(), None), make(changed.path(), Some("1")));
        let options = BuildOptions {
            project: super::super::ProjectOptions {
                cwd: Some(first.path().to_path_buf()),
                config_path: None,
            },
            entry: Some(PathBuf::from("src/main.ts")),
            write: false,
            ..BuildOptions::default()
        };
        let prepared = super::super::prepare_build(&options).unwrap();
        assert_ne!(
            shared_build_variant(&prepared, false).unwrap(),
            shared_build_variant(&prepared, true).unwrap(),
            "development resolution/JSX/minify variants must not reuse production providers"
        );
    }

    #[test]
    fn embedded_browser_runtime_matches_the_public_npm_runtime() {
        assert_eq!(
            RUNTIME_SOURCE,
            include_str!("../../../npm/wake/federation.mjs")
        );
    }
}
