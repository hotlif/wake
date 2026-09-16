//! Source-bound dependency graph coordination over the resolver-owned installation environment.
mod run;
pub(super) use run::{Project, analyze, needs_project};

use super::{execution::Batch, module_snapshot::SnapshotFs};
use crate::{CancellationToken, WakeError};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wake_common::{FileSystem, fs::normalize};
use wake_lint_core::{
    ModuleFile, ModuleGraph, ModuleId, ModuleRequest, ModuleRequestKind, ModuleResolution,
    PackageDependency,
};
use wake_resolver::{
    ModuleIdentity, ResolutionContext, ResolutionEnvironment, ResolveErrorKind, ResolveOptions,
    ResolvedModule,
};

pub(super) struct GraphSource {
    pub path: PathBuf,
    pub text: Arc<str>,
}
pub(super) struct GraphRequest {
    pub root: PathBuf,
    pub sources: Vec<GraphSource>,
    pub overlays: BTreeMap<PathBuf, Arc<str>>,
    pub resolve: ResolveOptions,
}
pub(super) struct ReadyGraph {
    pub graph: ModuleGraph,
    pub selected: Vec<ModuleId>,
}
pub(super) struct BuiltGraph {
    pub result: Result<ReadyGraph, WakeError>,
    pub watch_paths: Vec<PathBuf>,
}

pub(super) fn build(
    base: Arc<dyn FileSystem>,
    request: GraphRequest,
    batch: &Batch<'_>,
    cancellation: &CancellationToken,
) -> BuiltGraph {
    let mut overlays = request.overlays;
    for source in &request.sources {
        overlays.insert(normalize(&source.path), source.text.clone());
    }
    let snapshot = match SnapshotFs::new(base, overlays, cancellation.clone()) {
        Ok(snapshot) => Arc::new(snapshot),
        Err(error) => {
            return BuiltGraph {
                result: Err(error),
                watch_paths: Vec::new(),
            };
        }
    };
    let profiles: [ResolveOptions; 4] = std::array::from_fn(|mode| {
        let mut resolve = request.resolve.clone();
        let commonjs = mode & 1 != 0;
        let types = mode & 2 != 0;
        resolve.conditions.retain(|condition| {
            !matches!(
                condition.as_str(),
                "import" | "require" | "types" | "module"
            ) || condition == "module" && !commonjs
        });
        resolve
            .conditions
            .push(if commonjs { "require" } else { "import" }.into());
        if !resolve
            .conditions
            .iter()
            .any(|condition| condition == "default")
        {
            resolve.conditions.push("default".into());
        }
        if commonjs {
            resolve.main_fields = vec!["main".into()];
        }
        if types {
            resolve.conditions.push("types".into());
            resolve
                .main_fields
                .splice(0..0, ["types".into(), "typings".into()]);
            let before_js = resolve
                .extensions
                .iter()
                .position(|extension| extension == ".js")
                .unwrap_or(0);
            resolve.extensions.splice(
                before_js..before_js,
                [".d.ts".into(), ".d.mts".into(), ".d.cts".into()],
            );
        }
        resolve
    });
    let environments = profiles
        .clone()
        .map(|resolve| ResolutionEnvironment::with_options(snapshot.clone(), resolve));
    let mut builder = Builder {
        root: normalize(&request.root),
        environments: vec![environments],
        profiles,
        snapshot: snapshot.clone(),
        contexts: vec![ResolutionContext::default()],
        files: Vec::new(),
        paths: Vec::new(),
        file_scopes: Vec::new(),
        identity_names: Vec::new(),
        identities: HashMap::new(),
        queue: VecDeque::new(),
        manifests: HashMap::new(),
        watched: BTreeSet::new(),
        bytes: 0,
        requests: 0,
        cancellation,
    };
    let mut result = builder.run(request.sources, batch);
    if let Err(error) = snapshot.check() {
        result = Err(error);
    }
    let mut watched: BTreeSet<_> = snapshot.observed_paths().into_iter().collect();
    watched.extend(builder.watched);
    BuiltGraph {
        result,
        watch_paths: watched.into_iter().collect(),
    }
}

struct Builder<'a> {
    root: PathBuf,
    environments: Vec<[ResolutionEnvironment; 4]>,
    profiles: [ResolveOptions; 4],
    snapshot: Arc<SnapshotFs>,
    contexts: Vec<ResolutionContext>,
    files: Vec<Option<ModuleFile>>,
    paths: Vec<PathBuf>,
    file_scopes: Vec<usize>,
    identity_names: Vec<String>,
    identities: HashMap<(ModuleIdentity, usize), ModuleId>,
    queue: VecDeque<ModuleId>,
    manifests: HashMap<PathBuf, Option<Arc<Manifest>>>,
    watched: BTreeSet<PathBuf>,
    bytes: usize,
    requests: usize,
    cancellation: &'a CancellationToken,
}

impl Builder<'_> {
    fn scope_for(&mut self, path: &Path, parent: usize) -> Result<usize, WakeError> {
        let issuer = path
            .parent()
            .ok_or_else(|| analysis("Module source has no issuer directory"))?;
        let context = self.environments[parent][0]
            .context_for_issuer(issuer)
            .map_err(|error| analysis(&error.to_string()))?;
        if let Some(index) = self.contexts.iter().position(|known| *known == context) {
            return Ok(index);
        }
        if self.contexts.len() >= 256 {
            return Err(analysis("Module resolution context budget exceeded"));
        }
        let environments = self.profiles.clone().map(|profile| {
            ResolutionEnvironment::with_context(self.snapshot.clone(), profile, context.clone())
        });
        let index = self.contexts.len();
        self.contexts.push(context);
        self.environments.push(environments);
        Ok(index)
    }

    fn intern(
        &mut self,
        module: ResolvedModule,
        parent_scope: usize,
    ) -> Result<ModuleId, WakeError> {
        let scope = self.scope_for(&module.path, parent_scope)?;
        let key = (module.identity, scope);
        if let Some(&id) = self.identities.get(&key) {
            return Ok(id);
        }
        if self.files.len() >= ModuleGraph::MAX_FILES {
            return Err(analysis("Module file budget exceeded"));
        }
        let id = ModuleId(self.files.len());
        self.identity_names
            .push(format!("{:?}@{:?}", key.0, self.contexts[scope]));
        self.identities.insert(key, id);
        self.paths.push(normalize(&module.path));
        self.file_scopes.push(scope);
        self.files.push(None);
        self.queue.push_back(id);
        Ok(id)
    }

    fn run(
        &mut self,
        sources: Vec<GraphSource>,
        batch: &Batch<'_>,
    ) -> Result<ReadyGraph, WakeError> {
        let mut selected = Vec::new();
        for source in sources {
            self.cancellation.check()?;
            let path = normalize(&source.path);
            let identity = self.environments[0][0].resolver().module_identity(&path);
            selected.push(self.intern(ResolvedModule { path, identity }, 0)?);
        }
        while !self.queue.is_empty() {
            self.cancellation.check()?;
            let mut jobs = Vec::new();
            let mut ids = Vec::new();
            for _ in 0..batch.batch_size() {
                let Some(id) = self.queue.pop_front() else {
                    break;
                };
                let path = &self.paths[id.0];
                let scope = self.file_scopes[id.0];
                let fs = self.environments[scope][0].file_system();
                self.watched
                    .insert(self.environments[scope][0].watch_path(path));
                let text = fs
                    .read_to_string(path)
                    .map_err(|error| super::io_error(path, error))?;
                if text.len() > ModuleGraph::MAX_SOURCE_BYTES.saturating_sub(self.bytes) {
                    return Err(analysis("Module source byte budget exceeded"));
                }
                self.bytes += text.len();
                let source_type = super::source_type(&path.to_string_lossy())?;
                let identity = self.identity_names[id.0].clone();
                let path = relative(&self.root, path);
                ids.push(id);
                jobs.push(move || {
                    ModuleFile::new(identity, path, Arc::from(text), source_type)
                        .map_err(|error| analysis(&error.to_string()))
                });
            }
            let parsed = batch.run_window(jobs, self.cancellation)?;
            for (id, file) in ids.into_iter().zip(parsed) {
                let mut file = file?;
                if file.requests().len() > ModuleGraph::MAX_REQUESTS.saturating_sub(self.requests) {
                    return Err(analysis("Module request budget exceeded"));
                }
                self.requests += file.requests().len();
                for (index, request) in file.requests().to_vec().into_iter().enumerate() {
                    self.cancellation.check()?;
                    let path = self.paths[id.0].clone();
                    let resolution = self.resolve(&path, &request, self.file_scopes[id.0])?;
                    file.resolve(index, resolution)
                        .map_err(|error| analysis(&error.to_string()))?;
                }
                self.files[id.0] = Some(file);
            }
        }
        let graph = ModuleGraph::new(
            std::mem::take(&mut self.files)
                .into_iter()
                .map(|file| file.expect("every queued file was parsed"))
                .collect(),
        )
        .map_err(|error| analysis(&error.to_string()))?;
        Ok(ReadyGraph { graph, selected })
    }

    fn resolve(
        &mut self,
        path: &Path,
        request: &ModuleRequest,
        scope: usize,
    ) -> Result<ModuleResolution, WakeError> {
        let Some(specifier) = &request.specifier else {
            return Ok(ModuleResolution::Unknown("nonliteral request".into()));
        };
        let Some(specifier) = specifier.as_str() else {
            return Ok(ModuleResolution::Unresolved {
                reason: "module specifier contains isolated UTF-16 surrogates unsupported by the UTF-8 resolver".into(),
                package: None,
            });
        };
        if !request.attributes_known {
            return Ok(ModuleResolution::Unknown(
                "dynamic import attributes".into(),
            ));
        }
        if let Some(name) = builtin(specifier) {
            return Ok(ModuleResolution::Builtin(name.into()));
        }
        if specifier.starts_with("node:") {
            return Ok(ModuleResolution::Unresolved {
                reason: "unknown Node builtin".into(),
                package: None,
            });
        }
        let mut commonjs = matches!(
            request.kind,
            ModuleRequestKind::Require | ModuleRequestKind::ImportEquals
        );
        if request.type_only {
            for (key, value) in &request.attributes {
                if key == "resolution-mode" {
                    commonjs = match value.as_str() {
                        Some("require") => true,
                        Some("import") => false,
                        _ => {
                            return Ok(ModuleResolution::Unknown(
                                "invalid type import resolution-mode".into(),
                            ));
                        }
                    };
                }
            }
        }
        let mode = commonjs as usize | (request.type_only as usize) << 1;
        let resolver = self.environments[scope][mode].resolver();
        let issuer = path
            .parent()
            .ok_or_else(|| analysis("Module source has no issuer directory"))?;
        let name = resolver
            .package_request(specifier, issuer)
            .map_err(|error| analysis(&error.to_string()))?;
        let package = name
            .map(|name| self.package(issuer, name, scope))
            .transpose()?;
        let target = match resolver.resolve_module(specifier, issuer) {
            Ok(target) => target,
            Err(error) => {
                for witness in error.witnesses() {
                    self.watched
                        .insert(self.environments[scope][mode].watch_path(witness));
                }
                if matches!(error.kind(), ResolveErrorKind::PnpManifest(_)) {
                    return Err(analysis(&error.to_string()));
                }
                return Ok(ModuleResolution::Unresolved {
                    reason: error.to_string(),
                    package,
                });
            }
        };
        self.watched
            .insert(self.environments[scope][mode].watch_path(&target.path));
        let normalized = relative(&self.root, &target.path);
        let extension = target
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("");
        if super::source_type(&normalized).is_ok() {
            Ok(ModuleResolution::File {
                target: self.intern(target, scope)?,
                package,
            })
        } else if matches!(extension, "json" | "css") {
            let target_scope = self.scope_for(&target.path, scope)?;
            Ok(ModuleResolution::Resource {
                identity: format!("{:?}@{:?}", target.identity, self.contexts[target_scope]),
                path: normalized,
                package,
            })
        } else {
            let target_scope = self.scope_for(&target.path, scope)?;
            Ok(ModuleResolution::Opaque {
                identity: format!("{:?}@{:?}", target.identity, self.contexts[target_scope]),
                path: normalized,
                package,
            })
        }
    }

    fn package(
        &mut self,
        issuer: &Path,
        name: &str,
        scope: usize,
    ) -> Result<PackageDependency, WakeError> {
        let fs = self.environments[scope][0].file_system();
        let mut traversed = Vec::new();
        let mut manifest = None;
        for directory in issuer.ancestors() {
            self.cancellation.check()?;
            if let Some(cached) = self.manifests.get(directory) {
                manifest = cached.clone();
                break;
            }
            traversed.push(directory.to_path_buf());
            let path = directory.join("package.json");
            self.watched
                .insert(self.environments[scope][0].watch_path(&path));
            if fs.is_file(&path) {
                let source = fs
                    .read_to_string(&path)
                    .map_err(|error| super::io_error(&path, error))?;
                manifest = Some(Arc::new(
                    Manifest::parse(&source).map_err(|error| error.at(&path))?,
                ));
                break;
            }
            if directory
                .file_name()
                .is_some_and(|name| name == "node_modules")
            {
                break;
            }
        }
        for directory in traversed {
            self.manifests.insert(directory, manifest.clone());
        }
        Ok(PackageDependency {
            name: name.into(),
            production: manifest
                .as_ref()
                .is_some_and(|m| m.production.contains(name)),
            development: manifest
                .as_ref()
                .is_some_and(|m| m.development.contains(name)),
            optional: manifest.as_ref().is_some_and(|m| m.optional.contains(name)),
            peer: manifest.as_ref().is_some_and(|m| m.peer.contains(name)),
            self_reference: manifest
                .as_ref()
                .is_some_and(|m| m.name.as_deref() == Some(name)),
        })
    }
}

struct Manifest {
    name: Option<String>,
    production: BTreeSet<String>,
    development: BTreeSet<String>,
    optional: BTreeSet<String>,
    peer: BTreeSet<String>,
}
impl Manifest {
    fn parse(source: &str) -> Result<Self, WakeError> {
        let value: serde_json::Value = serde_json::from_str(source)
            .map_err(|error| analysis(&format!("Invalid package manifest: {error}")))?;
        let object = value
            .as_object()
            .ok_or_else(|| analysis("Package manifest must be an object"))?;
        let section = |name: &str| -> Result<BTreeSet<String>, WakeError> {
            match object.get(name) {
                None => Ok(BTreeSet::new()),
                Some(value) => value
                    .as_object()
                    .map(|section| section.keys().cloned().collect())
                    .ok_or_else(|| analysis(&format!("Package {name} must be an object"))),
            }
        };
        Ok(Self {
            name: object
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            production: section("dependencies")?,
            development: section("devDependencies")?,
            optional: section("optionalDependencies")?,
            peer: section("peerDependencies")?,
        })
    }
}

fn relative(root: &Path, path: &Path) -> String {
    let path = normalize(path);
    let root_parts: Vec<_> = root.components().collect();
    let path_parts: Vec<_> = path.components().collect();
    let common = root_parts
        .iter()
        .zip(&path_parts)
        .take_while(|(a, b)| a == b)
        .count();
    if common == 0 {
        return path.to_string_lossy().replace('\\', "/");
    }
    let mut relative = PathBuf::new();
    for _ in common..root_parts.len() {
        relative.push("..");
    }
    for part in &path_parts[common..] {
        relative.push(part.as_os_str());
    }
    relative.to_string_lossy().replace('\\', "/")
}

fn analysis(message: &str) -> WakeError {
    WakeError::new("WAKE_LINT_ANALYSIS", message)
}

// Node 24.0.0 default builtin identities; names only, no third-party implementation or host probe.
fn builtin(specifier: &str) -> Option<&str> {
    let name = specifier.strip_prefix("node:").unwrap_or(specifier);
    if specifier.starts_with("node:")
        && matches!(name, "sea" | "sqlite" | "test" | "test/reporters")
    {
        return Some(name);
    }
    const NAMES: &[&str] = &[
        "_http_agent",
        "_http_client",
        "_http_common",
        "_http_incoming",
        "_http_outgoing",
        "_http_server",
        "_stream_duplex",
        "_stream_passthrough",
        "_stream_readable",
        "_stream_transform",
        "_stream_wrap",
        "_stream_writable",
        "_tls_common",
        "_tls_wrap",
        "assert",
        "assert/strict",
        "async_hooks",
        "buffer",
        "child_process",
        "cluster",
        "console",
        "constants",
        "crypto",
        "dgram",
        "diagnostics_channel",
        "dns",
        "dns/promises",
        "domain",
        "events",
        "fs",
        "fs/promises",
        "http",
        "http2",
        "https",
        "inspector",
        "inspector/promises",
        "module",
        "net",
        "os",
        "path",
        "path/posix",
        "path/win32",
        "perf_hooks",
        "process",
        "punycode",
        "querystring",
        "readline",
        "readline/promises",
        "repl",
        "stream",
        "stream/consumers",
        "stream/promises",
        "stream/web",
        "string_decoder",
        "sys",
        "timers",
        "timers/promises",
        "tls",
        "trace_events",
        "tty",
        "url",
        "util",
        "util/types",
        "v8",
        "vm",
        "wasi",
        "worker_threads",
        "zlib",
    ];
    NAMES.binary_search(&name).is_ok().then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wake_common::MemoryFileSystem;
    use wake_lint_core::{LintOptions, RuleConfiguration, RuleLevel, RuleSetting};

    fn root() -> PathBuf {
        PathBuf::from(if cfg!(windows) {
            "C:/graph-project"
        } else {
            "/graph-project"
        })
    }

    fn run(
        disk: Arc<MemoryFileSystem>,
        source: &str,
        overlays: BTreeMap<PathBuf, Arc<str>>,
    ) -> BuiltGraph {
        let root = root();
        let scheduler =
            super::super::execution::Scheduler::new(Arc::new(wake_turbo::Executor::new(2)));
        let cancellation = CancellationToken::default();
        let batch = scheduler.admit(&cancellation).unwrap();
        build(
            disk,
            GraphRequest {
                root: root.clone(),
                sources: vec![GraphSource {
                    path: root.join("src/a.ts"),
                    text: Arc::from(source),
                }],
                overlays,
                resolve: ResolveOptions {
                    alias: vec![("@".into(), root.join("src"))],
                    ..Default::default()
                },
            },
            &batch,
            &cancellation,
        )
    }

    fn lint(
        ready: &ReadyGraph,
        rule: &str,
        values: serde_json::Value,
    ) -> Vec<wake_lint_core::LintDiagnostic> {
        ready
            .graph
            .lint(
                ready.selected[0],
                &LintOptions {
                    recommended: false,
                    rules: [(
                        rule.into(),
                        RuleSetting::Options(RuleConfiguration {
                            level: RuleLevel::Error,
                            options: serde_json::from_value(values).unwrap(),
                        }),
                    )]
                    .into(),
                    ..Default::default()
                },
            )
            .unwrap()
            .diagnostics
    }

    #[test]
    fn native_graph_uses_request_profiles_aliases_and_transitive_source_snapshots() {
        let root = root();
        let disk = Arc::new(MemoryFileSystem::from_files([
            (
                root.join("package.json"),
                r#"{"name":"app","version":"1","dependencies":{"dual":"1"}}"#,
            ),
            (root.join("src/b.ts"), "import './a';"),
            (
                root.join("node_modules/dual/package.json"),
                r#"{"name":"dual","version":"1","exports":{"types":"./types.d.ts","import":"./import.js","require":"./require.cjs"}}"#,
            ),
            (
                root.join("node_modules/dual/import.js"),
                "export default 1;",
            ),
            (
                root.join("node_modules/dual/require.cjs"),
                "module.exports = 1;",
            ),
            (
                root.join("node_modules/dual/types.d.ts"),
                "export interface T {}",
            ),
        ]));
        let source = "import D from 'dual'; require('dual'); import type {T} from 'dual'; import '@/b'; import 'node:fs';";
        let built = run(disk.clone(), source, BTreeMap::new());
        assert!(
            built
                .watch_paths
                .contains(&root.join("node_modules/dual/package.json"))
        );
        let ready = built.result.unwrap();
        assert!(lint(&ready, "import/no-unresolved", serde_json::json!({})).is_empty());
        assert!(
            lint(
                &ready,
                "import/no-extraneous-dependencies",
                serde_json::json!({})
            )
            .is_empty()
        );
        assert_eq!(
            lint(&ready, "import/no-cycle", serde_json::json!({})).len(),
            1
        );
        let restricted = lint(
            &ready,
            "import/no-restricted-paths",
            serde_json::json!({"zones":[
                {"to":"^src/a", "from":"/import\\.js$", "message":"ESM entry"},
                {"to":"^src/a", "from":"/require\\.cjs$", "message":"CJS entry"},
                {"to":"^src/a", "from":"/types\\.d\\.ts$", "message":"Types entry"}
            ]}),
        );
        assert_eq!(
            restricted
                .iter()
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>(),
            ["ESM entry", "CJS entry", "Types entry"]
        );
        let next = run(
            disk.clone(),
            source,
            [(root.join("src/b.ts"), Arc::from("export {};"))].into(),
        )
        .result
        .unwrap();
        assert!(lint(&next, "import/no-cycle", serde_json::json!({})).is_empty());
        assert_eq!(
            disk.read_to_string(&root.join("src/b.ts")).unwrap(),
            "import './a';"
        );
    }

    #[test]
    fn invalid_pnp_is_authoritative_and_retains_watch_evidence_for_recovery() {
        let root = root();
        let disk = Arc::new(MemoryFileSystem::from_files([
            (root.join(".pnp.cjs"), "truncated"),
            (
                root.join("node_modules/ghost/index.js"),
                "export default 1;",
            ),
        ]));
        let result = run(disk, "import 'ghost';", BTreeMap::new());
        assert!(result.result.is_err());
        assert!(result.watch_paths.contains(&root.join(".pnp.cjs")));
    }

    #[test]
    fn builtin_names_are_exact_and_missing_packages_retain_declaration_facts() {
        let disk = Arc::new(MemoryFileSystem::from_files([(
            root().join("package.json"),
            "{}",
        )]));
        let result = run(disk, "import 'fs'; import 'node:fs/promises'; import 'node:test/reporters'; import 'fs/nonexistent'; import 'node:invented'; import 'test';", BTreeMap::new()).result.unwrap();
        assert_eq!(
            lint(&result, "import/no-unresolved", serde_json::json!({})).len(),
            3
        );
        assert_eq!(
            lint(
                &result,
                "import/no-extraneous-dependencies",
                serde_json::json!({})
            )
            .len(),
            2
        );
    }

    #[test]
    fn external_pnp_cache_dependencies_keep_the_owning_project_authority() {
        let root = root();
        let cache = root.parent().unwrap().join("graph-cache");
        let disk = Arc::new(MemoryFileSystem::from_files([
            (
                root.join(".pnp.cjs"),
                "module.exports = require('./.pnp.data.json');",
            ),
            (
                root.join("package.json"),
                r#"{"dependencies":{"first":"1"}}"#,
            ),
            (
                cache.join("first/package.json"),
                r#"{"name":"first","version":"1","main":"index.js","dependencies":{"second":"1"}}"#,
            ),
            (cache.join("first/index.js"), "import 'second';"),
            (
                cache.join("second/package.json"),
                r#"{"name":"second","version":"1","main":"index.js"}"#,
            ),
            (cache.join("second/index.js"), "export default 1;"),
        ]));
        disk.insert(root.join(".pnp.data.json"), serde_json::json!({
            "dependencyTreeRoots":[{"name":null,"reference":null}],
            "packageRegistryData":[
                [null,[[null,{"packageLocation":"./","packageDependencies":[["first","npm:1"]]}]]],
                ["first",[["npm:1",{"packageLocation":"../graph-cache/first/","packageDependencies":[["first","npm:1"],["second","npm:1"]]}]]],
                ["second",[["npm:1",{"packageLocation":"../graph-cache/second/","packageDependencies":[["second","npm:1"]]}]]]
            ]
        }).to_string());
        let ready = run(disk, "import 'first';", BTreeMap::new())
            .result
            .unwrap();
        assert!(lint(&ready, "import/no-cycle", serde_json::json!({})).is_empty());
    }

    #[test]
    fn nested_pnp_projects_do_not_merge_shared_cache_modules_with_different_dependency_graphs() {
        let root = root();
        let cache = root.parent().unwrap().join("graph-cache");
        let disk = Arc::new(MemoryFileSystem::from_files([
            (
                root.join("package.json"),
                r#"{"dependencies":{"first":"1"}}"#,
            ),
            (
                root.join("nested/package.json"),
                r#"{"dependencies":{"first":"1"}}"#,
            ),
            (root.join("nested/entry.ts"), "import 'first';"),
            (
                cache.join("shared-first/package.json"),
                r#"{"name":"first","version":"1","main":"index.js","dependencies":{"second":"1"}}"#,
            ),
            (cache.join("shared-first/index.js"), "import 'second';"),
            (
                cache.join("a/package.json"),
                r#"{"name":"second","version":"1","main":"index.js"}"#,
            ),
            (cache.join("a/index.js"), "export default 1;"),
            (
                cache.join("b/package.json"),
                r#"{"name":"second","version":"1","main":"index.js"}"#,
            ),
            (
                cache.join("b/index.js"),
                "import '../../graph-project/src/a.ts';",
            ),
        ]));
        for (project, prefix, second) in [
            (root.clone(), "../graph-cache", "a"),
            (root.join("nested"), "../../graph-cache", "b"),
        ] {
            disk.insert(
                project.join(".pnp.cjs"),
                "module.exports = require('./.pnp.data.json');",
            );
            disk.insert(project.join(".pnp.data.json"), serde_json::json!({
                "dependencyTreeRoots":[{"name":null,"reference":null}],
                "packageRegistryData":[
                    [null,[[null,{"packageLocation":"./","packageDependencies":[["first","npm:1"]]}]]],
                    ["first",[["npm:1",{"packageLocation":format!("{prefix}/shared-first/"),"packageDependencies":[["first","npm:1"],["second","npm:1"]]}]]],
                    ["second",[["npm:1",{"packageLocation":format!("{prefix}/{second}/"),"packageDependencies":[["second","npm:1"]]}]]]
                ]
            }).to_string());
        }
        let source = "import 'first'; import '../nested/entry';";
        let ready = run(disk, source, BTreeMap::new()).result.unwrap();
        let cycles = lint(&ready, "import/no-cycle", serde_json::json!({}));
        assert_eq!(
            cycles
                .iter()
                .map(|d| (
                    &source[d.start as usize..d.end as usize],
                    d.message_id.as_str()
                ))
                .collect::<Vec<_>>(),
            [("'../nested/entry'", "cycle")]
        );
    }
}
