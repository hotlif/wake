//! Portable artifact assembly shared by the native and browser build products.
//!
//! This layer owns no filesystem, configuration discovery, network, or session lifetime.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use sha2::{Digest, Sha384};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use wake_bundler::BuildOutput;
use wake_federation_contract::{
    Asset, AssetKind, BuildId, ErrorCode as FederationErrorCode, ExposeConfig, ExposeKey,
    ExposeMode, ExposedModule,
};

#[derive(Clone, Debug)]
pub struct ArtifactError {
    pub code: &'static str,
    pub message: String,
}
impl ArtifactError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}
impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}
impl std::error::Error for ArtifactError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactKind {
    Css,
    Asset,
    SourceMap,
    FederationChunk,
}
pub struct GeneratedFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub kind: ArtifactKind,
}
#[derive(Clone)]
pub struct BuiltExposeRoot {
    pub config: ExposeConfig,
    pub chunk_name: String,
    pub loader_export: String,
}
pub struct BuiltContainer {
    pub directory: String,
    pub output: BuildOutput,
    pub exposes: BTreeMap<ExposeKey, BuiltExposeRoot>,
}
pub struct MaterializedContainer {
    pub exposes: BTreeMap<ExposeKey, ExposedModule>,
    pub files: Vec<GeneratedFile>,
    pub hidden_source_maps: Vec<GeneratedFile>,
}
#[derive(Clone, Copy)]
pub enum ProductionSourceMapMode {
    Hidden,
    Public,
}
impl ProductionSourceMapMode {
    pub fn is_public(self) -> bool {
        matches!(self, Self::Public)
    }
}

pub fn digest_bytes(bytes: &[u8]) -> (String, String) {
    let digest = Sha384::digest(bytes);
    let raw = digest.as_slice();
    let mut hex = String::with_capacity(raw.len() * 2);
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for byte in raw {
        hex.push(char::from(DIGITS[usize::from(byte >> 4)]));
        hex.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    (hex, format!("sha384-{}", BASE64.encode(raw)))
}

pub fn short_hash(bytes: &[u8]) -> String {
    digest_bytes(bytes).0[..16].to_owned()
}

pub fn safe_expose_name(key: &str) -> String {
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

pub fn asset_for(kind: AssetKind, url: String, bytes: &[u8], mime: &str) -> Asset {
    let (content_hash, integrity) = digest_bytes(bytes);
    Asset::new(kind, url, content_hash, integrity, mime, bytes.len() as u64)
}

pub fn materialize_container(
    container_name: &str,
    build_id: &BuildId,
    container: &BuiltContainer,
    source_map_mode: ProductionSourceMapMode,
) -> Result<MaterializedContainer, ArtifactError> {
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
            kind: ArtifactKind::FederationChunk,
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
                    kind: ArtifactKind::SourceMap,
                });
            } else {
                hidden_source_maps.push(GeneratedFile {
                    path: hidden_source_map_path(
                        container_name,
                        build_id,
                        &format!("container/{}.map", chunk.file_name),
                    ),
                    bytes: map_bytes,
                    kind: ArtifactKind::SourceMap,
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
                ArtifactKind::Css
            } else {
                ArtifactKind::Asset
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
                ArtifactError::new(
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

pub fn validate_host_rendered_css_policy(
    expose: &ExposeKey,
    config: &ExposeConfig,
    assets: &[wake_bundler::OutputAsset],
    reachable_module_ids: &BTreeSet<u32>,
) -> Result<(), ArtifactError> {
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
    Err(ArtifactError::new(
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

pub fn dependency_first_chunk_closure(
    root: &str,
    chunks: &BTreeMap<String, &wake_bundler::OutputChunk>,
) -> Result<Vec<String>, ArtifactError> {
    fn visit(
        file: &str,
        chunks: &BTreeMap<String, &wake_bundler::OutputChunk>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        ordered: &mut Vec<String>,
    ) -> Result<(), ArtifactError> {
        if visited.contains(file) {
            return Ok(());
        }
        if !visiting.insert(file.to_owned()) {
            return Err(ArtifactError::new(
                "WAKE_INTERNAL",
                format!("federation chunk dependency cycle includes `{file}`"),
            ));
        }
        let chunk = chunks.get(file).ok_or_else(|| {
            ArtifactError::new(
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

pub fn transitive_dynamic_chunk_closure(
    initial: &[String],
    chunks: &BTreeMap<String, &wake_bundler::OutputChunk>,
) -> Result<Vec<String>, ArtifactError> {
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

pub fn hidden_source_map_path(container: &str, build_id: &BuildId, file: &str) -> String {
    format!("federation/source-maps/{container}/{build_id}/{file}")
}

pub fn rewrite_remote_source_map(
    map: &str,
    container: &str,
    build_id: &str,
    file_name: &str,
) -> Result<String, ArtifactError> {
    let mut value = serde_json::from_str::<serde_json::Value>(map).map_err(|error| {
        ArtifactError::new(
            "WAKE_INTERNAL",
            format!("Wake generated an invalid federation source map: {error}"),
        )
    })?;
    let object = value.as_object_mut().ok_or_else(|| {
        ArtifactError::new(
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
        ArtifactError::new(
            "WAKE_INTERNAL",
            format!("could not serialize federation source map: {error}"),
        )
    })
}

pub fn mime_for_path(path: &str) -> &'static str {
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

pub fn remote_entry_protocol_source(
    container: &str,
    build_id: &str,
    expose_loaders: &BTreeMap<ExposeKey, String>,
    shared_exports: &BTreeMap<String, String>,
    shared_loader_export: Option<&str>,
) -> String {
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
