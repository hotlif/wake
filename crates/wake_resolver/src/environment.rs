//! Resolver-owned Yarn PnP discovery, registry, filesystem and invalidation.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use wake_common::{FileSystem, FxHashMap, fs::normalize};

use crate::{PnpFileSystem, ResolveOptions, Resolver, pnp::PnpLoadError, pnp::PnpManifest};

#[derive(Clone)]
pub(crate) enum PnpRoute {
    NoManifest,
    Classic,
    Managed(Arc<PnpManifest>),
}

/// 按 issuer 发现最近 PnP 根，并缓存成功及失败清单。
pub(crate) struct PnpRegistry {
    fs: Arc<dyn FileSystem>,
    roots: Mutex<FxHashMap<PathBuf, Option<PathBuf>>>,
    manifests: Mutex<FxHashMap<PathBuf, Result<Arc<PnpManifest>, PnpLoadError>>>,
}

impl PnpRegistry {
    fn new(fs: Arc<dyn FileSystem>) -> Self {
        Self {
            fs,
            roots: Mutex::new(FxHashMap::default()),
            manifests: Mutex::new(FxHashMap::default()),
        }
    }

    pub(crate) fn discover_root(&self, issuer_dir: &Path) -> Option<PathBuf> {
        let issuer_dir = normalize(issuer_dir);
        if let Some(root) = self.roots.lock().unwrap().get(&issuer_dir).cloned() {
            return root;
        }
        let root = PnpManifest::discover_root(self.fs.as_ref(), &issuer_dir);
        self.roots.lock().unwrap().insert(issuer_dir, root.clone());
        root
    }

    fn manifest(&self, root: &Path) -> Result<Arc<PnpManifest>, PnpLoadError> {
        let root = normalize(root);
        if let Some(manifest) = self.manifests.lock().unwrap().get(&root).cloned() {
            return manifest;
        }
        let manifest = PnpManifest::load(self.fs.as_ref(), &root).map(Arc::new);
        self.manifests
            .lock()
            .unwrap()
            .insert(root, manifest.clone());
        manifest
    }

    pub(crate) fn route(&self, issuer_dir: &Path) -> Result<PnpRoute, PnpLoadError> {
        let Some(root) = self.discover_root(issuer_dir) else {
            return Ok(PnpRoute::NoManifest);
        };
        let manifest = self.manifest(&root)?;
        if manifest.is_ignored(issuer_dir) || !manifest.owns_issuer(issuer_dir) {
            Ok(PnpRoute::Classic)
        } else {
            Ok(PnpRoute::Managed(manifest))
        }
    }

    fn clear(&self) {
        self.roots.lock().unwrap().clear();
        self.manifests.lock().unwrap().clear();
    }
}

/// JavaScript 解析的唯一运行环境。
///
/// 它统一持有基础文件系统、PnP/zip 投影、按 issuer 的清单 registry、Resolver 与失效状态；
/// Bundler、Library、Test runtime 和 CSS LSP 不再各自发现或加载 `.pnp.*`。
pub struct ResolutionEnvironment {
    base_fs: Arc<dyn FileSystem>,
    pnp_fs: Arc<PnpFileSystem>,
    registry: Arc<PnpRegistry>,
    resolver: Arc<Resolver>,
}

impl ResolutionEnvironment {
    pub fn new(base_fs: Arc<dyn FileSystem>) -> Self {
        Self::with_options(base_fs, ResolveOptions::default())
    }

    pub fn with_options(base_fs: Arc<dyn FileSystem>, options: ResolveOptions) -> Self {
        let pnp_fs = Arc::new(PnpFileSystem::new(Arc::clone(&base_fs)));
        let registry = Arc::new(PnpRegistry::new(Arc::clone(&base_fs)));
        let fs: Arc<dyn FileSystem> = pnp_fs.clone();
        let resolver = Arc::new(Resolver::with_registry(fs, Arc::clone(&registry), options));
        Self {
            base_fs,
            pnp_fs,
            registry,
            resolver,
        }
    }

    pub fn resolver(&self) -> Arc<Resolver> {
        Arc::clone(&self.resolver)
    }

    pub fn file_system(&self) -> Arc<dyn FileSystem> {
        self.pnp_fs.clone()
    }

    pub fn base_file_system(&self) -> Arc<dyn FileSystem> {
        Arc::clone(&self.base_fs)
    }

    pub fn watch_path(&self, path: &Path) -> PathBuf {
        self.pnp_fs.watch_path(path)
    }

    pub fn has_pnp_root(&self, issuer_dir: &Path) -> bool {
        self.registry.discover_root(issuer_dir).is_some()
    }

    pub fn invalidate_all(&self) {
        self.resolver.clear_cache();
        self.registry.clear();
        self.pnp_fs.clear_cache();
    }

    /// 文件 generation 变化后使解析、清单、zip 和模块拓扑同时失效。
    pub fn invalidate_paths<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) {
        let mut reload_pnp = false;
        for path in paths {
            if matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".pnp.cjs" | ".pnp.data.json" | "yarn.lock")
            ) {
                reload_pnp = true;
            }
        }
        self.resolver.clear_cache();
        if reload_pnp {
            self.registry.clear();
            self.pnp_fs.clear_cache();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PnpError, ResolveErrorKind};
    use wake_common::MemoryFileSystem;

    fn manifest(package_location: &str, ignore_pattern: Option<&str>) -> String {
        let mut value = serde_json::json!({
            "enableTopLevelFallback": false,
            "dependencyTreeRoots": [[null, null]],
            "packageRegistryData": [
                [null, [[null, {
                    "packageLocation": "./",
                    "packageDependencies": [["ghost", "npm:1.0.0"]]
                }]]],
                ["ghost", [["npm:1.0.0", {
                    "packageLocation": package_location,
                    "packageDependencies": [["ghost", "npm:1.0.0"]]
                }]]]
            ]
        });
        if let Some(pattern) = ignore_pattern {
            value["ignorePatternData"] = serde_json::Value::String(pattern.to_string());
        }
        value.to_string()
    }

    fn external_loader(fs: &MemoryFileSystem, root: &str, data: String) {
        fs.insert(
            Path::new(root).join(".pnp.cjs"),
            "module.exports = require('./.pnp.data.json');",
        );
        fs.insert(Path::new(root).join(".pnp.data.json"), data);
    }

    #[test]
    fn malformed_loader_is_final_even_when_node_modules_exists() {
        let fs = Arc::new(MemoryFileSystem::new());
        fs.insert("project/.pnp.cjs", "this is truncated");
        fs.insert("project/node_modules/ghost/index.js", "module.exports = 1");
        let base: Arc<dyn FileSystem> = fs;
        let environment = ResolutionEnvironment::new(base);

        let error = environment
            .resolver()
            .resolve("ghost", Path::new("project/src"))
            .unwrap_err();
        assert!(matches!(error.kind(), ResolveErrorKind::PnpManifest(_)));
    }

    #[test]
    fn pnp_dependency_rejection_is_final() {
        let fs = Arc::new(MemoryFileSystem::new());
        let data = manifest("../cache/ghost/node_modules/ghost/", None)
            .replace("[[\"ghost\",\"npm:1.0.0\"]]", "[]");
        external_loader(&fs, "project", data);
        fs.insert("project/node_modules/ghost/index.js", "module.exports = 1");
        let base: Arc<dyn FileSystem> = fs;
        let environment = ResolutionEnvironment::new(base);

        let error = environment
            .resolver()
            .resolve("ghost", Path::new("project/src"))
            .unwrap_err();
        assert_eq!(
            error.kind(),
            &ResolveErrorKind::PnpDependency(PnpError::Undeclared)
        );
    }

    #[test]
    fn ignore_pattern_routes_to_classic_node_resolution() {
        let fs = Arc::new(MemoryFileSystem::new());
        external_loader(
            &fs,
            "project",
            manifest(
                "../cache/ghost/node_modules/ghost/",
                Some(r"^ignored(?:/|$)"),
            ),
        );
        fs.insert(
            "project/ignored/node_modules/ghost/index.js",
            "module.exports = 1",
        );
        let base: Arc<dyn FileSystem> = fs;
        let environment = ResolutionEnvironment::new(base);

        assert_eq!(
            environment
                .resolver()
                .resolve("ghost", Path::new("project/ignored/src"))
                .unwrap(),
            PathBuf::from("project/ignored/node_modules/ghost/index.js")
        );
    }

    #[test]
    fn npm_project_uses_installed_tree_and_package_lock_only_invalidates() {
        let fs = Arc::new(MemoryFileSystem::new());
        fs.insert("project/package-lock.json", "{ definitely not valid json");
        // Neither an orphan data file nor a Yarn lock activates PnP.
        fs.insert("project/.pnp.data.json", "{ also not a manifest");
        fs.insert("project/yarn.lock", "not a real Yarn lock");
        let base: Arc<dyn FileSystem> = fs.clone();
        let environment = ResolutionEnvironment::new(base);
        let resolver = environment.resolver();

        let missing = resolver
            .resolve("installed", Path::new("project/src"))
            .unwrap_err();
        assert_eq!(missing.kind(), &ResolveErrorKind::NotFound);

        fs.insert(
            "project/node_modules/installed/package.json",
            r#"{"exports":"./index.js"}"#,
        );
        fs.insert(
            "project/node_modules/installed/index.js",
            "module.exports = 42",
        );
        environment.invalidate_paths([Path::new("project/package-lock.json")]);

        assert_eq!(
            resolver
                .resolve("installed", Path::new("project/src"))
                .unwrap(),
            PathBuf::from("project/node_modules/installed/index.js")
        );
    }

    #[test]
    fn npm_project_keeps_wake_alias_precedence() {
        let fs = Arc::new(MemoryFileSystem::new());
        fs.insert("project/alias/react.js", "module.exports = 'alias'");
        fs.insert(
            "project/node_modules/react/index.js",
            "module.exports = 'installed'",
        );
        let base: Arc<dyn FileSystem> = fs;
        let environment = ResolutionEnvironment::with_options(
            base,
            ResolveOptions {
                alias: vec![("react".to_string(), PathBuf::from("project/alias/react.js"))],
                ..ResolveOptions::default()
            },
        );

        assert_eq!(
            environment
                .resolver()
                .resolve("react", Path::new("project/src"))
                .unwrap(),
            PathBuf::from("project/alias/react.js")
        );
    }

    #[test]
    fn absolute_issuer_outside_the_cwd_pnp_tree_stays_classic() {
        let fs = Arc::new(MemoryFileSystem::new());
        external_loader(&fs, "", manifest("cache/ghost/node_modules/ghost/", None));
        let base: Arc<dyn FileSystem> = fs;
        let environment = ResolutionEnvironment::new(base);
        let issuer = std::env::temp_dir().join("wake-unmanaged-issuer/src");

        assert!(!environment.has_pnp_root(&issuer));
        assert_eq!(
            environment
                .resolver()
                .resolve("ghost", &issuer)
                .unwrap_err()
                .kind(),
            &ResolveErrorKind::NotFound
        );
    }

    #[test]
    fn pnp_preserves_non_package_wake_internal_aliases() {
        let fs = Arc::new(MemoryFileSystem::new());
        external_loader(
            &fs,
            "project",
            manifest("../cache/ghost/node_modules/ghost/", None),
        );
        fs.insert(
            "project/.wake/docs/generated/runtime/app.tsx",
            "export const App = 1",
        );
        let base: Arc<dyn FileSystem> = fs;
        let environment = ResolutionEnvironment::with_options(
            base,
            ResolveOptions {
                alias: vec![(
                    "@@wake/docs".to_string(),
                    PathBuf::from("project/.wake/docs/generated"),
                )],
                ..ResolveOptions::default()
            },
        );

        assert_eq!(
            environment
                .resolver()
                .resolve("@@wake/docs/runtime/app.tsx", Path::new("project/src"))
                .unwrap(),
            PathBuf::from("project/.wake/docs/generated/runtime/app.tsx")
        );
    }

    #[test]
    fn nearest_nested_pnp_root_owns_resolution() {
        let fs = Arc::new(MemoryFileSystem::new());
        external_loader(
            &fs,
            "project",
            manifest("../cache/v1/node_modules/ghost/", None),
        );
        external_loader(
            &fs,
            "project/nested",
            manifest("../../cache/v2/node_modules/ghost/", None),
        );
        fs.insert("cache/v1/node_modules/ghost/index.js", "module.exports = 1");
        fs.insert("cache/v2/node_modules/ghost/index.js", "module.exports = 2");
        let base: Arc<dyn FileSystem> = fs;
        let environment = ResolutionEnvironment::new(base);

        assert_eq!(
            environment
                .resolver()
                .resolve("ghost", Path::new("project/nested/src"))
                .unwrap(),
            PathBuf::from("cache/v2/node_modules/ghost/index.js")
        );
    }

    #[test]
    fn manifest_failure_and_version_switch_recover_after_invalidation() {
        let fs = Arc::new(MemoryFileSystem::new());
        fs.insert("project/.pnp.cjs", "broken");
        fs.insert("cache/v1/node_modules/ghost/index.js", "module.exports = 1");
        fs.insert("cache/v2/node_modules/ghost/index.js", "module.exports = 2");
        let base: Arc<dyn FileSystem> = fs.clone();
        let environment = ResolutionEnvironment::new(base);
        let resolver = environment.resolver();
        assert!(matches!(
            resolver
                .resolve("ghost", Path::new("project/src"))
                .unwrap_err()
                .kind(),
            ResolveErrorKind::PnpManifest(_)
        ));

        external_loader(
            &fs,
            "project",
            manifest("../cache/v1/node_modules/ghost/", None),
        );
        environment.invalidate_paths([Path::new("project/.pnp.cjs")]);
        assert_eq!(
            resolver.resolve("ghost", Path::new("project/src")).unwrap(),
            PathBuf::from("cache/v1/node_modules/ghost/index.js")
        );

        fs.insert(
            "project/.pnp.data.json",
            manifest("../cache/v2/node_modules/ghost/", None),
        );
        environment.invalidate_paths([Path::new("project/.pnp.data.json")]);
        assert_eq!(
            resolver.resolve("ghost", Path::new("project/src")).unwrap(),
            PathBuf::from("cache/v2/node_modules/ghost/index.js")
        );
    }
}
