//! Browser-owned builds from immutable virtual project snapshots.
//!
//! The browser product reuses Wake's resolver and BuildSession. It owns no second compiler,
//! dependency downloader, operating-system filesystem, or development server.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use wake_bundler::{
    BuildGeneration, BuildOptions, BuildRequest, BuildSession, FederationEntryExport,
};
use wake_common::{Diagnostic, FileSystem, MemoryFileSystem};
use wake_federation_build::{self as artifacts, ArtifactError, BuiltContainer, BuiltExposeRoot};
use wake_federation_contract::{
    AssetKind, BuildId, FederationConfig, Manifest, SharedManifest, SharedPolicy, SharedRequirement,
};
use wake_resolver::{ModuleIdentity, ResolveOptions, Resolver};

const ROOT: &str = "/__wake_browser__";
const GENERATED: &str = ".wake-browser";
const MAX_FILES: usize = 20_000;
const MAX_BYTES: usize = 128 * 1024 * 1024;

/// Immutable browser build configuration. Dependencies are supplied as package files in each snapshot.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserBuildOptions {
    pub federation: FederationConfig,
    /// Exact identities of the host's already registered shared providers.
    pub host_shared: BTreeMap<String, wake_federation_contract::PackageKey>,
    pub aliases: BTreeMap<String, String>,
    pub define: BTreeMap<String, String>,
    /// Reserved same-origin URL prefix served by the browser artifact store.
    pub asset_base: String,
}

impl Default for BrowserBuildOptions {
    fn default() -> Self {
        Self {
            federation: FederationConfig::default(),
            host_shared: BTreeMap::new(),
            aliases: BTreeMap::new(),
            define: BTreeMap::new(),
            asset_base: "/__wake_preview__/assets/".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub mime: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserDiagnostic {
    pub severity: String,
    pub code: Option<String>,
    pub message: String,
    pub path: Option<String>,
    pub start: Option<u32>,
    pub end: Option<u32>,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub line_text: Option<String>,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserBuildOutput {
    pub manifest: Manifest,
    pub files: Vec<BrowserFile>,
    pub diagnostics: Vec<BrowserDiagnostic>,
    pub module_count: usize,
    pub updated_module_count: usize,
    pub cached_module_count: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct BrowserBuildError {
    pub code: String,
    pub message: String,
    pub diagnostics: Vec<BrowserDiagnostic>,
}

impl BrowserBuildError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            diagnostics: Vec::new(),
        }
    }
}
impl std::fmt::Display for BrowserBuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}
impl std::error::Error for BrowserBuildError {}
impl From<ArtifactError> for BrowserBuildError {
    fn from(error: ArtifactError) -> Self {
        Self::new(error.code, error.message)
    }
}

/// Owns accepted compilation state. Failed candidates never replace the accepted session or artifacts.
pub struct BrowserBuildContext {
    options: BrowserBuildOptions,
    session: Option<BuildSession>,
    files: BTreeMap<String, Vec<u8>>,
    current: Option<BrowserBuildOutput>,
}

impl BrowserBuildContext {
    pub fn new(mut options: BrowserBuildOptions) -> Result<Self, BrowserBuildError> {
        options.federation = options
            .federation
            .validate_and_normalize()
            .map_err(|error| BrowserBuildError::new("FED_CONFIG_INVALID", error.to_string()))?;
        if !options.federation.enabled || options.federation.exposes.is_empty() {
            return Err(BrowserBuildError::new(
                "WAKE_CONFIG",
                "browser builds require at least one enabled Federation expose",
            ));
        }
        if !options.federation.remotes.is_empty()
            || options
                .federation
                .shared
                .values()
                .any(|shared| shared.fallback)
        {
            return Err(BrowserBuildError::new(
                "WAKE_CONFIG",
                "browser preview requires host-owned shared dependencies with fallback=false; remote dependencies are not supported",
            ));
        }
        if options
            .host_shared
            .keys()
            .ne(options.federation.shared.keys())
        {
            return Err(BrowserBuildError::new(
                "FED_CONFIG_INVALID",
                "hostShared must provide the actual registered package identity for every configured shared module",
            ));
        }
        if options.asset_base != "/__wake_preview__/assets/" {
            return Err(BrowserBuildError::new(
                "WAKE_CONFIG",
                "assetBase is reserved as /__wake_preview__/assets/ by the preview store",
            ));
        }
        for expose in options.federation.exposes.values() {
            validate_path(&expose.entry)?;
            if expose.mode == wake_federation_contract::ExposeMode::Isolated {
                return Err(BrowserBuildError::new(
                    "WAKE_CONFIG",
                    "browser preview supports generic and host-rendered exposes; isolated roots require a separate mounting lifecycle",
                ));
            }
        }
        for target in options.aliases.values() {
            validate_path(target)?;
        }
        if options
            .aliases
            .keys()
            .any(|key| key.starts_with("@@@/__wake_browser"))
            || options.define.contains_key("import.meta.hot")
            || options.define.contains_key("import.meta.url")
        {
            return Err(BrowserBuildError::new(
                "WAKE_CONFIG",
                "browser-owned aliases and import.meta definitions are reserved",
            ));
        }
        Ok(Self {
            options,
            session: None,
            files: BTreeMap::new(),
            current: None,
        })
    }

    pub fn current(&self) -> Option<&BrowserBuildOutput> {
        self.current.as_ref()
    }

    pub fn build(
        &mut self,
        files: BTreeMap<String, Vec<u8>>,
    ) -> Result<BrowserBuildOutput, BrowserBuildError> {
        validate_files(&files)?;
        if self.current.is_some() && files == self.files {
            let mut output = self.current.clone().expect("checked current");
            output.updated_module_count = 0;
            output.cached_module_count = output.module_count;
            return Ok(output);
        }
        let fs =
            Arc::new(MemoryFileSystem::from_files(files.iter().map(
                |(path, bytes)| (Path::new(ROOT).join(path), bytes.clone()),
            )));
        let mut resolve = ResolveOptions {
            conditions: ["browser", "development", "import", "module", "default"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            alias: self
                .options
                .aliases
                .iter()
                .map(|(key, target)| (key.clone(), Path::new(ROOT).join(target)))
                .collect(),
            ..ResolveOptions::default()
        };
        let shared = shared_requirements(&self.options, fs.clone(), &resolve)?;
        let mut source = String::new();
        let mut exposes = BTreeMap::new();
        for (index, (key, config)) in self.options.federation.exposes.iter().enumerate() {
            let chunk_name = format!(
                "expose-{index:04}-{}",
                artifacts::safe_expose_name(key.as_str())
            );
            let alias = format!("@@@/__wake_browser_expose_{index:04}");
            resolve
                .alias
                .push((alias.clone(), Path::new(ROOT).join(&config.entry)));
            let wrapper = format!(
                "import * as namespace from {};\nexport default namespace;\n",
                json_string(&alias)
            );
            fs.insert(
                Path::new(ROOT)
                    .join(GENERATED)
                    .join(format!("{chunk_name}.mjs")),
                wrapper.into_bytes(),
            );
            let loader_export = format!("e{index}");
            source.push_str(&format!(
                "export const {loader_export}=()=>import({}).then(module=>module.default);\n",
                json_string(&format!("./{chunk_name}.mjs"))
            ));
            exposes.insert(
                key.clone(),
                BuiltExposeRoot {
                    config: config.clone(),
                    chunk_name,
                    loader_export,
                },
            );
        }
        let entry = Path::new(ROOT).join(GENERATED).join("container.mjs");
        fs.insert(&entry, source.into_bytes());
        let mut options = BuildOptions {
            project_root: Some(ROOT.into()),
            resolve,
            extract_css: true,
            css_in_js: true,
            source_map: true,
            asset_inline_limit: 0,
            public_path: self.options.asset_base.clone(),
            code_splitting: true,
            entry_chunk_name: Some("container".into()),
            ..BuildOptions::default()
        };
        options.jsx.development = true;
        options
            .define
            .retain(|(key, _)| key != "process.env.NODE_ENV");
        options
            .define
            .push(("process.env.NODE_ENV".into(), "\"development\"".into()));
        for (key, value) in &self.options.define {
            options.define.retain(|(existing, _)| existing != key);
            options.define.push((key.clone(), value.clone()));
        }
        options.federation.entry_export = Some(FederationEntryExport::build_scoped(
            self.options.federation.name.as_str(),
            "./__wake_container__",
        ));
        options.federation.expose_roots = exposes
            .iter()
            .map(|(key, root)| (root.chunk_name.clone(), key.as_str().into()))
            .collect();
        options.federation.shared = self
            .options
            .federation
            .shared
            .iter()
            .map(|(key, shared)| (key.clone(), key.clone(), shared.scope.clone()))
            .collect();
        let generation = BuildGeneration::new(fs.clone());
        let changed = files
            .keys()
            .chain(self.files.keys())
            .filter(|path| files.get(*path) != self.files.get(*path))
            .map(|path| Path::new(ROOT).join(path))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let structural = files.keys().ne(self.files.keys());
        let mut candidate = match self.session.as_mut() {
            Some(accepted) => {
                generation.candidate_session(accepted, options, Some((&changed, structural)))
            }
            None => generation.retained_session(options),
        };
        let output = candidate.build_current(BuildRequest::new(entry));
        let diagnostics = output
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic_info(diagnostic, fs.as_ref()))
            .collect::<Vec<_>>();
        if output.has_errors() {
            return Err(BrowserBuildError {
                code: "WAKE_BUILD".into(),
                message: "browser project compilation failed".into(),
                diagnostics,
            });
        }
        let identity = serde_json::to_vec(&serde_json::json!({
            "compiler": env!("CARGO_PKG_VERSION"), "options": self.options, "shared": shared,
            "chunks": output.chunks.iter().map(|chunk| (&chunk.file_name, &chunk.code, &chunk.source_map)).collect::<Vec<_>>(),
            "assets": output.assets.iter().map(|asset| (&asset.file_name, &asset.bytes)).collect::<Vec<_>>()
        })).expect("serializable identity");
        let build_id = BuildId::new(format!("b{}", &artifacts::digest_bytes(&identity).0[..32]));
        let container = BuiltContainer {
            directory: String::new(),
            output,
            exposes,
        };
        let assembled = artifacts::materialize_container(
            self.options.federation.name.as_str(),
            &build_id,
            &container,
            artifacts::ProductionSourceMapMode::Public,
        )?;
        let mut generated = assembled
            .files
            .into_iter()
            .map(|file| BrowserFile {
                mime: match file.kind {
                    artifacts::ArtifactKind::Css => "text/css",
                    artifacts::ArtifactKind::FederationChunk => "text/javascript",
                    artifacts::ArtifactKind::SourceMap => "application/source-map+json",
                    artifacts::ArtifactKind::Asset => artifacts::mime_for_path(&file.path),
                }
                .into(),
                path: file.path,
                bytes: file.bytes,
            })
            .collect::<Vec<_>>();
        let loaders = container
            .exposes
            .iter()
            .map(|(key, expose)| (key.clone(), expose.loader_export.clone()))
            .collect();
        let protocol = artifacts::remote_entry_protocol_source(
            self.options.federation.name.as_str(),
            build_id.as_str(),
            &loaders,
            &BTreeMap::new(),
            None,
        );
        let remote_path = format!(
            "federation/{}/{}/remoteEntry.mjs",
            self.options.federation.name, build_id
        );
        let remote_code = format!("{}\n{}", container.output.entry().code, protocol);
        let mut manifest = Manifest::new(
            self.options.federation.name.clone(),
            build_id.clone(),
            "esnext",
            artifacts::asset_for(
                AssetKind::JavaScript,
                format!("./{remote_path}"),
                remote_code.as_bytes(),
                "text/javascript",
            ),
        );
        manifest.exposes = assembled.exposes;
        manifest.shared = SharedManifest {
            offers: Vec::new(),
            requirements: shared,
        };
        generated.push(BrowserFile {
            path: remote_path.clone(),
            bytes: remote_code.into_bytes(),
            mime: "text/javascript".into(),
        });
        if let Some(map) = &container.output.entry().source_map {
            let bytes = artifacts::rewrite_remote_source_map(
                map,
                self.options.federation.name.as_str(),
                build_id.as_str(),
                &remote_path,
            )?
            .into_bytes();
            let path = format!("{remote_path}.map");
            manifest.remote_entry_source_map = Some(artifacts::asset_for(
                AssetKind::SourceMap,
                format!("./{path}"),
                &bytes,
                "application/source-map+json",
            ));
            generated.push(BrowserFile {
                path,
                bytes,
                mime: "application/source-map+json".into(),
            });
        }
        manifest
            .validate()
            .map_err(|error| BrowserBuildError::new("FED_MANIFEST_SCHEMA", error.to_string()))?;
        generated.push(BrowserFile {
            path: "wake-federation.json".into(),
            bytes: serde_json::to_vec(&manifest).expect("manifest serialization"),
            mime: "application/json".into(),
        });
        generated.sort_by(|left, right| left.path.cmp(&right.path));
        let output = BrowserBuildOutput {
            manifest,
            files: generated,
            diagnostics,
            module_count: container.output.module_count,
            updated_module_count: container.output.updated_module_count,
            cached_module_count: container.output.cached_module_count,
        };
        self.session = Some(candidate);
        self.files = files;
        self.current = Some(output.clone());
        Ok(output)
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization")
}

fn validate_path(path: &str) -> Result<(), BrowserBuildError> {
    if path.is_empty()
        || path.contains(['\\', ':', '%', '?', '#', '\0'])
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || path.split('/').next() == Some(GENERATED)
    {
        return Err(BrowserBuildError::new(
            "WAKE_CONFIG",
            format!("invalid or reserved virtual path: {path}"),
        ));
    }
    Ok(())
}

fn validate_files(files: &BTreeMap<String, Vec<u8>>) -> Result<(), BrowserBuildError> {
    if files.len() > MAX_FILES
        || files
            .values()
            .try_fold(0usize, |size, bytes| size.checked_add(bytes.len()))
            .is_none_or(|size| size > MAX_BYTES)
    {
        return Err(BrowserBuildError::new(
            "WAKE_CONFIG",
            "virtual project exceeds the 20,000 file or 128 MiB limit",
        ));
    }
    for path in files.keys() {
        validate_path(path)?;
        let mut parent = Path::new(path).parent();
        while let Some(directory) = parent {
            if files.contains_key(directory.to_string_lossy().as_ref()) {
                return Err(BrowserBuildError::new(
                    "WAKE_CONFIG",
                    format!("file/directory collision: {path}"),
                ));
            }
            parent = directory.parent();
        }
    }
    Ok(())
}

fn shared_requirements(
    options: &BrowserBuildOptions,
    fs: Arc<dyn FileSystem>,
    resolve: &ResolveOptions,
) -> Result<Vec<SharedRequirement>, BrowserBuildError> {
    let resolver = Resolver::with_options(fs, resolve.clone());
    options.federation.shared.iter().map(|(key, config)| {
        let resolved = resolver.resolve_module(key, Path::new(ROOT)).map_err(|error| BrowserBuildError::new("FED_SHARE_UNSATISFIABLE", format!("{key}: {error}")))?;
        let ModuleIdentity::Package { package, .. } = resolved.identity else {
            return Err(BrowserBuildError::new("FED_CONFIG_INVALID", format!("shared module {key} has no package identity")));
        };
        let host = &options.host_shared[key];
        let context = package.context.unwrap_or_else(|| "default".into());
        if package.name != host.name || package.version != host.version || context != host.package_context {
            return Err(BrowserBuildError::new("FED_SHARE_UNSATISFIABLE", format!("virtual dependency {key} does not match the host provider's package name, version or context")));
        }
        Ok(SharedRequirement { share_key: key.clone(), required_version: config.required_version.clone().unwrap_or(package.version),
            package_context: context, build_variant: host.build_variant.clone(),
            policy: SharedPolicy { scope: config.scope.clone(), singleton: config.singleton, strict: config.strict, fallback: false,
                coherence_group: config.coherence_group.clone(), owner: config.owner.clone() }, fallback: None })
    }).collect()
}

fn diagnostic_info(diagnostic: &Diagnostic, fs: &dyn FileSystem) -> BrowserDiagnostic {
    let span = diagnostic.primary_span();
    let text = diagnostic
        .path
        .as_ref()
        .and_then(|path| fs.read_to_string(Path::new(path)).ok());
    let position = span.and_then(|span| {
        text.as_deref().and_then(|source| {
            let prefix = source.get(..span.lo as usize)?;
            let start = prefix.rfind('\n').map_or(0, |index| index + 1);
            let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
            let column = source[start..span.lo as usize].chars().count() + 1;
            let line_text = source[start..]
                .split(['\r', '\n'])
                .next()
                .unwrap_or_default()
                .to_owned();
            Some((line, column, line_text))
        })
    });
    BrowserDiagnostic {
        severity: diagnostic.severity.as_str().into(),
        code: diagnostic.code.as_ref().map(ToString::to_string),
        message: diagnostic.message.clone(),
        path: diagnostic.path.as_ref().map(|path| {
            path.replace('\\', "/")
                .trim_start_matches(&format!("{ROOT}/"))
                .to_owned()
        }),
        start: span.map(|span| span.lo),
        end: span.map(|span| span.hi),
        line: position.as_ref().map(|p| p.0),
        column: position.as_ref().map(|p| p.1),
        line_text: position.map(|p| p.2),
        notes: diagnostic.notes.clone(),
    }
}

#[cfg(target_arch = "wasm32")]
mod wasm;
