//! Resolver-owned Yarn PnP discovery, registry, filesystem and invalidation.
mod node_modules;
pub use node_modules::{NodeModulesPath, NodeModulesView};

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

/// Resolver-owned authority retained when an installed package lives outside its PnP project.
/// A new filesystem generation revalidates the referenced manifest; callers cannot forge roots.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ResolutionContext {
    pnp_root: Option<PathBuf>,
}

/// 按 issuer 发现最近 PnP 根，并缓存成功及失败清单。
pub(crate) struct PnpRegistry {
    fs: Arc<dyn FileSystem>,
    context: ResolutionContext,
    roots: Mutex<FxHashMap<PathBuf, Option<PathBuf>>>,
    manifests: Mutex<FxHashMap<PathBuf, Result<Arc<PnpManifest>, PnpLoadError>>>,
}

impl PnpRegistry {
    fn new(fs: Arc<dyn FileSystem>, context: ResolutionContext) -> Self {
        Self {
            fs,
            context,
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

    fn context_for_issuer(&self, issuer_dir: &Path) -> Result<ResolutionContext, PnpLoadError> {
        self.context_for_issuer_with(issuer_dir, &self.context)
    }

    fn context_for_issuer_with(
        &self,
        issuer_dir: &Path,
        inherited: &ResolutionContext,
    ) -> Result<ResolutionContext, PnpLoadError> {
        if let Some(root) = self.discover_root(issuer_dir) {
            // Nearest physical authority is final, including ignored/unmanaged or invalid input.
            self.manifest(&root)?;
            return Ok(ResolutionContext {
                pnp_root: Some(root),
            });
        }
        if let Some(root) = &inherited.pnp_root
            && self.manifest(root)?.owns_issuer(issuer_dir)
        {
            return Ok(inherited.clone());
        }
        Ok(ResolutionContext::default())
    }

    pub(crate) fn route(&self, issuer_dir: &Path) -> Result<PnpRoute, PnpLoadError> {
        self.route_with(issuer_dir, &self.context)
    }

    fn route_with(
        &self,
        issuer_dir: &Path,
        inherited: &ResolutionContext,
    ) -> Result<PnpRoute, PnpLoadError> {
        let Some(root) = self
            .context_for_issuer_with(issuer_dir, inherited)?
            .pnp_root
        else {
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
        Self::with_context(base_fs, options, ResolutionContext::default())
    }

    /// Create an isolated resolver cache for a previously observed authority and new filesystem.
    /// Actual ancestor PnP roots still win; inherited authority only owns declared package paths.
    pub fn with_context(
        base_fs: Arc<dyn FileSystem>,
        options: ResolveOptions,
        context: ResolutionContext,
    ) -> Self {
        let pnp_fs = Arc::new(PnpFileSystem::new(Arc::clone(&base_fs)));
        let registry = Arc::new(PnpRegistry::new(Arc::clone(&base_fs), context));
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

    pub fn context_for_issuer(&self, issuer_dir: &Path) -> Result<ResolutionContext, PnpLoadError> {
        self.registry.context_for_issuer(&normalize(issuer_dir))
    }

    /// Read-only node_modules lookup projection for tools that query an installation filesystem.
    /// Package visibility and physical/logical target selection remain resolver-owned.
    pub fn node_modules_view(&self, issuer: &Path) -> Result<NodeModulesView, PnpLoadError> {
        Ok(NodeModulesView::new(
            self.registry.clone(),
            self.context_for_issuer(issuer)?,
        ))
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

    /// 文件 generation 变化后使解析状态失效。
    ///
    /// PnP manifest/lock 变化清除全部 registry 与 zip；物理 archive watcher 路径只驱逐
    /// 完全匹配的 zip cache key。
    pub fn invalidate_paths<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) {
        let mut reload_pnp = false;
        let mut archive_paths = Vec::new();
        for path in paths {
            if matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".pnp.cjs" | ".pnp.data.json" | "yarn.lock")
            ) {
                reload_pnp = true;
            } else {
                archive_paths.push(path);
            }
        }
        self.resolver.clear_cache();
        if reload_pnp {
            self.registry.clear();
            self.pnp_fs.clear_cache();
        } else {
            for path in archive_paths {
                self.pnp_fs.invalidate_archive(path);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PnpError, ResolveErrorKind};
    use wake_common::MemoryFileSystem;

    #[test]
    fn node_modules_view_keeps_installed_package_directory_boundaries_literal() {
        let root = normalize(&std::env::temp_dir().join("installed-type-view"));
        let disk = Arc::new(MemoryFileSystem::new());
        disk.insert(
            root.join(".pnp.cjs"),
            "module.exports = require('./.pnp.data.json');",
        );
        let location = "./.yarn/__virtual__/pkg-one/0/cache/pkg.zip/node_modules/pkg/";
        disk.insert(root.join(".pnp.data.json"), serde_json::json!({"packageRegistryData":[
            [null,[[null,{"packageLocation":"./","packageDependencies":[["alias",["pkg","virtual:one"]]]}]]],
            ["pkg",[["virtual:one",{"packageLocation":location,"packageDependencies":[]}]]]
        ]}).to_string());
        let environment = ResolutionEnvironment::new(disk);
        let view = environment.node_modules_view(&root).unwrap();
        for suffix in ["", "index.d.ts"] {
            let path = normalize(&root.join(location).join(suffix));
            assert_eq!(view.resolve(&path).unwrap(), NodeModulesPath::Native(path));
        }
    }

    #[test]
    fn node_modules_view_uses_pnp_visibility_aliases_scopes_and_transitive_authority() {
        let root = normalize(&std::env::temp_dir().join("type-view-project"));
        let cache = root.parent().unwrap().join("type-view-cache");
        let fs = Arc::new(MemoryFileSystem::new());
        fs.insert(
            root.join(".pnp.cjs"),
            "module.exports = require('./.pnp.data.json');",
        );
        fs.insert(root.join(".pnp.data.json"), serde_json::json!({
            "ignorePatternData":"(^|/)ignored/", "enableTopLevelFallback":true,
            "fallbackPool":[["fallback","npm:1"]],
            "packageRegistryData":[
                [null,[[null,{"packageLocation":"./","packageDependencies":[["first",["pkg","npm:1"]],["@scope/tool","npm:1"],["missing",null]]}]]],
                ["pkg",[["npm:1",{"packageLocation":"../type-view-cache/pkg/","packageDependencies":[["peer","npm:1"]]}]]],
                ["peer",[["npm:1",{"packageLocation":"../type-view-cache/peer/","packageDependencies":[]}]]],
                ["@scope/tool",[["npm:1",{"packageLocation":"../type-view-cache/tool/","packageDependencies":[]}]]],
                ["fallback",[["npm:1",{"packageLocation":"../type-view-cache/fallback/","packageDependencies":[]}]]]
            ]
        }).to_string());
        fs.insert(
            root.join("node_modules/undeclared/index.d.ts"),
            "physical shadow",
        );
        let environment = ResolutionEnvironment::new(fs.clone());
        let view = environment.node_modules_view(&root).unwrap();
        assert_eq!(
            view.resolve(&root.join("node_modules")).unwrap(),
            NodeModulesPath::Directory(vec!["@scope".into(), "fallback".into(), "first".into()])
        );
        assert_eq!(
            view.resolve(&root.join("node_modules/@scope")).unwrap(),
            NodeModulesPath::Directory(vec!["tool".into()])
        );
        for (request, expected) in [
            (
                "node_modules/first/index.d.ts",
                cache.join("pkg/index.d.ts"),
            ),
            (
                "src/node_modules/first/node_modules/peer/index.d.ts",
                cache.join("peer/index.d.ts"),
            ),
            (
                "node_modules/@scope/tool/index.d.ts",
                cache.join("tool/index.d.ts"),
            ),
            (
                "node_modules/fallback/index.d.ts",
                cache.join("fallback/index.d.ts"),
            ),
        ] {
            let NodeModulesPath::Projected { path, context } =
                view.resolve(&root.join(request)).unwrap()
            else {
                panic!("{request}");
            };
            assert_eq!(path, expected);
            assert_eq!(context, environment.context_for_issuer(&root).unwrap());
        }
        for request in [
            "node_modules/undeclared/index.d.ts",
            "node_modules/missing/index.d.ts",
            "node_modules/@absent",
            "node_modules/@scope/absent",
        ] {
            assert_eq!(
                view.resolve(&root.join(request)).unwrap(),
                NodeModulesPath::Missing
            );
        }
        let ordinary = root.join("ignored/node_modules/undeclared/index.d.ts");
        assert_eq!(
            view.resolve(&ordinary).unwrap(),
            NodeModulesPath::Native(ordinary)
        );
        let outside = root
            .parent()
            .unwrap()
            .join("unmanaged/node_modules/pkg/file.ts");
        assert_eq!(
            view.resolve(&outside).unwrap(),
            NodeModulesPath::Native(outside)
        );
        let context = view.context_for_issuer(&cache.join("pkg")).unwrap();
        assert_eq!(context, environment.context_for_issuer(&root).unwrap());
        let retained = view.in_context(context);
        assert!(
            matches!(retained.resolve(&cache.join("pkg/node_modules/peer/index.d.ts")).unwrap(),
            NodeModulesPath::Projected { path, .. } if path == cache.join("peer/index.d.ts"))
        );
        for ancestor in [&cache, root.parent().unwrap()] {
            assert_eq!(
                retained
                    .resolve(&ancestor.join("node_modules/undeclared/index.d.ts"))
                    .unwrap(),
                NodeModulesPath::Missing,
                "ancestor node_modules cannot bypass a PnP rejection"
            );
        }
        fs.insert(root.join("nested/.pnp.cjs"), "broken");
        assert!(
            view.resolve(&root.join("nested/node_modules/first/index.d.ts"))
                .is_err()
        );
    }

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
    fn package_request_identity_reuses_alias_and_pnp_routing_without_requiring_installation() {
        for mode in ["absent", "managed", "ignored", "malformed"] {
            let fs = Arc::new(MemoryFileSystem::new());
            if mode == "malformed" {
                fs.insert("project/.pnp.cjs", "truncated");
            } else if mode != "absent" {
                external_loader(
                    &fs,
                    "project",
                    manifest(
                        "../cache/ghost/node_modules/ghost/",
                        (mode == "ignored").then_some("^src(?:/|$)"),
                    ),
                );
            }
            let environment = ResolutionEnvironment::with_options(
                fs,
                ResolveOptions {
                    alias: vec![
                        ("ghost".into(), "project/local.js".into()),
                        ("@".into(), "project/src".into()),
                    ],
                    ..Default::default()
                },
            );
            let resolver = environment.resolver();
            let issuer = Path::new("project/src");
            assert_eq!(
                resolver.package_request("./relative", issuer).unwrap(),
                None
            );
            assert_eq!(resolver.package_request("#imports", issuer).unwrap(), None);
            assert_eq!(resolver.package_request("@/alias", issuer).unwrap(), None);
            assert_eq!(resolver.package_request("@bad", issuer).unwrap(), None);
            if mode == "malformed" {
                assert!(matches!(
                    resolver
                        .package_request("ghost/sub", issuer)
                        .unwrap_err()
                        .kind(),
                    ResolveErrorKind::PnpManifest(_)
                ));
            } else {
                assert_eq!(
                    resolver.package_request("ghost/sub", issuer).unwrap(),
                    (mode != "absent").then_some("ghost")
                );
                assert_eq!(
                    resolver
                        .package_request("@scope/missing/deep", issuer)
                        .unwrap(),
                    Some("@scope/missing")
                );
            }
        }
    }

    #[test]
    fn explicit_context_owns_external_cache_issuers_without_capturing_other_projects() {
        let fs = Arc::new(MemoryFileSystem::new());
        external_loader(
            &fs,
            "project",
            manifest("../cache/ghost/node_modules/ghost/", None),
        );
        external_loader(
            &fs,
            "nested",
            manifest("../cache/other/node_modules/ghost/", None),
        );
        fs.insert(
            "cache/ghost/node_modules/ghost/index.js",
            "export default 1;",
        );
        fs.insert(
            "cache/other/node_modules/ghost/index.js",
            "export default 2;",
        );
        let environment = ResolutionEnvironment::new(fs.clone());
        let context = environment
            .context_for_issuer(Path::new("project/src"))
            .unwrap();
        assert_ne!(context, ResolutionContext::default());
        let retained =
            ResolutionEnvironment::with_context(fs, ResolveOptions::default(), context.clone());
        assert_eq!(
            retained
                .context_for_issuer(Path::new("cache/ghost/node_modules/ghost"))
                .unwrap(),
            context
        );
        assert_eq!(
            retained
                .resolver()
                .resolve("ghost", Path::new("cache/ghost/node_modules/ghost"))
                .unwrap(),
            PathBuf::from("cache/ghost/node_modules/ghost/index.js")
        );
        assert_eq!(
            retained
                .context_for_issuer(Path::new("unrelated/src"))
                .unwrap(),
            ResolutionContext::default()
        );
        assert_ne!(
            retained
                .context_for_issuer(Path::new("nested/src"))
                .unwrap(),
            context
        );
        assert_eq!(
            retained
                .resolver()
                .resolve("ghost", Path::new("nested/src"))
                .unwrap(),
            PathBuf::from("cache/other/node_modules/ghost/index.js")
        );
        assert_eq!(
            environment
                .context_for_issuer(Path::new("cache/ghost/node_modules/ghost"))
                .unwrap(),
            ResolutionContext::default()
        );
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

    #[test]
    fn physical_archive_change_invalidates_only_that_zip_cache() {
        let fs = Arc::new(MemoryFileSystem::new());
        let first = Path::new("cache/first.zip");
        let second = Path::new("cache/second.zip");
        fs.insert(
            first,
            super::super::pnpfs::tests::one_entry_zip("pkg/index.js", b"first-v1"),
        );
        fs.insert(
            second,
            super::super::pnpfs::tests::one_entry_zip("pkg/index.js", b"second-v1"),
        );
        let base: Arc<dyn FileSystem> = fs.clone();
        let environment = ResolutionEnvironment::new(base);
        let projected = environment.file_system();

        assert_eq!(
            projected
                .read(Path::new("cache/first.zip/pkg/index.js"))
                .unwrap(),
            b"first-v1"
        );
        assert_eq!(
            projected
                .read(Path::new("cache/second.zip/pkg/index.js"))
                .unwrap(),
            b"second-v1"
        );

        fs.insert(
            first,
            super::super::pnpfs::tests::one_entry_zip("pkg/index.js", b"first-v2"),
        );
        fs.insert(
            second,
            super::super::pnpfs::tests::one_entry_zip("pkg/index.js", b"second-v2"),
        );
        environment.invalidate_paths([first]);

        assert_eq!(
            projected
                .read(Path::new("cache/first.zip/pkg/index.js"))
                .unwrap(),
            b"first-v2"
        );
        assert_eq!(
            projected
                .read(Path::new("cache/second.zip/pkg/index.js"))
                .unwrap(),
            b"second-v1"
        );
    }

    #[test]
    fn yarn_lock_change_invalidates_every_zip_cache() {
        let fs = Arc::new(MemoryFileSystem::new());
        for archive in ["cache/first.zip", "cache/second.zip"] {
            fs.insert(
                archive,
                super::super::pnpfs::tests::one_entry_zip("pkg/index.js", b"v1"),
            );
        }
        let base: Arc<dyn FileSystem> = fs.clone();
        let environment = ResolutionEnvironment::new(base);
        let projected = environment.file_system();
        for archive in ["cache/first.zip", "cache/second.zip"] {
            assert_eq!(
                projected
                    .read(&Path::new(archive).join("pkg/index.js"))
                    .unwrap(),
                b"v1"
            );
            fs.insert(
                archive,
                super::super::pnpfs::tests::one_entry_zip("pkg/index.js", b"v2"),
            );
        }

        environment.invalidate_paths([Path::new("project/yarn.lock")]);

        for archive in ["cache/first.zip", "cache/second.zip"] {
            assert_eq!(
                projected
                    .read(&Path::new(archive).join("pkg/index.js"))
                    .unwrap(),
                b"v2"
            );
        }
    }
}
