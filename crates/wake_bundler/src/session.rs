use std::path::{Path, PathBuf};
use std::sync::Arc;

use wake_common::FileSystem;
use wake_ecma_transform::TargetEnv;

use crate::{BuildOutput, BuildPlatform, IncrementalBundler, ModuleFormat, ResolveOptions};

/// JSX automatic runtime options owned by a build session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JsxOptions {
    /// Emit development-runtime calls and source locations.
    pub development: bool,
    /// Package prefix used for `jsx-runtime` / `jsx-dev-runtime` imports.
    pub import_source: String,
}

impl Default for JsxOptions {
    fn default() -> Self {
        Self {
            development: false,
            import_source: "react".to_string(),
        }
    }
}

/// Federation identity published by the entry module of this build.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FederationEntryExport {
    /// Development/application entry published in the page-scoped registry.
    PageScoped { container: String, expose: String },
    /// Immutable producer entry published in the broker's build-scoped registry.
    BuildScoped { container: String, expose: String },
}

impl FederationEntryExport {
    pub fn page_scoped(container: impl Into<String>, expose: impl Into<String>) -> Self {
        Self::PageScoped {
            container: container.into(),
            expose: expose.into(),
        }
    }

    pub fn build_scoped(container: impl Into<String>, expose: impl Into<String>) -> Self {
        Self::BuildScoped {
            container: container.into(),
            expose: expose.into(),
        }
    }
}

/// Build-scoped federation inputs already resolved by the product layer.
///
/// Manifest I/O, origin policy, and public contract validation intentionally stay outside the
/// bundler. This plan contains only facts that alter graph construction or emitted runtime code.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FederationBuildPlan {
    pub remotes: Vec<String>,
    /// `(source request, public share key, share scope)`.
    pub shared: Vec<(String, String, String)>,
    pub shared_fallback_roots: Vec<PathBuf>,
    pub entry_export: Option<FederationEntryExport>,
    /// `(synthetic chunk name, canonical expose key)`.
    pub expose_roots: Vec<(String, String)>,
}

/// 一次构建使用的稳定配置。配置在 session 创建时归一化，避免构建过程中通过 setter
/// 改变任务语义。
#[derive(Clone, Debug)]
pub struct BuildOptions {
    /// 项目根，用于生成跨 checkout/跨平台稳定的 CSS module identity。
    pub project_root: Option<PathBuf>,
    pub resolve: ResolveOptions,
    pub platform: BuildPlatform,
    pub module_format: ModuleFormat,
    pub external_packages: Vec<String>,
    pub define: Vec<(String, String)>,
    pub extract_css: bool,
    pub asset_inline_limit: usize,
    pub public_path: String,
    pub minify: bool,
    pub dead_module_elimination: bool,
    pub source_map: bool,
    pub css_in_js: bool,
    pub drop_console: bool,
    pub drop_debugger: bool,
    pub tree_shaking: bool,
    pub code_splitting: bool,
    pub content_hash: bool,
    /// Give the entry chunk a product-owned logical name in both single- and multi-chunk builds.
    pub entry_chunk_name: Option<String>,
    /// Hash the single JavaScript entry filename instead of retaining the legacy `bundle.js`.
    pub single_chunk_content_hash: bool,
    pub persistent_cache: Option<PathBuf>,
    pub jsx: JsxOptions,
    pub federation: FederationBuildPlan,
    /// 已规范化的浏览器目标。
    pub target_env: TargetEnv,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            project_root: None,
            resolve: ResolveOptions::default(),
            platform: BuildPlatform::Browser,
            module_format: ModuleFormat::Iife,
            external_packages: Vec::new(),
            define: vec![
                (
                    "process.env.NODE_ENV".to_string(),
                    "\"production\"".to_string(),
                ),
                ("import.meta.hot".to_string(), "false".to_string()),
                (
                    "import.meta.url".to_string(),
                    "__wake_require__.metaUrl()".to_string(),
                ),
            ],
            extract_css: false,
            asset_inline_limit: usize::MAX,
            public_path: "/".to_string(),
            minify: false,
            dead_module_elimination: false,
            source_map: false,
            css_in_js: false,
            drop_console: false,
            drop_debugger: false,
            tree_shaking: false,
            code_splitting: false,
            content_hash: true,
            entry_chunk_name: None,
            single_chunk_content_hash: false,
            persistent_cache: None,
            jsx: JsxOptions::default(),
            federation: FederationBuildPlan::default(),
            target_env: TargetEnv::default(),
        }
    }
}

/// 单次构建请求。构建选项属于 [`BuildSession`]，请求只携带本次变化的输入。
#[derive(Clone, Debug)]
pub struct BuildRequest {
    pub entry: PathBuf,
}

impl BuildRequest {
    pub fn new(entry: impl Into<PathBuf>) -> Self {
        Self {
            entry: entry.into(),
        }
    }
}

/// 唯一构建会话入口。普通构建、增量构建和未来的 watch 构建应复用此类型。
pub struct BuildSession {
    bundler: IncrementalBundler,
    lifetime: SessionLifetime,
    generation: u64,
    committed: Option<CommittedBuild>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionLifetime {
    Retained,
    OneShot,
}

struct CommittedBuild {
    generation: u64,
    entry: PathBuf,
    output: BuildOutput,
}

impl BuildSession {
    pub fn new(fs: Arc<dyn FileSystem>, options: BuildOptions) -> Self {
        let mut bundler = IncrementalBundler::new(fs);
        apply_options(&mut bundler, options);
        bundler.enable_load_cache();
        Self {
            bundler,
            lifetime: SessionLifetime::Retained,
            generation: 0,
            committed: None,
        }
    }

    /// Create a session for exactly one owned build.
    ///
    /// Unlike retained sessions, this path does not retain loader snapshots or commit an output
    /// for future generations. Call [`BuildSession::build_once`] to consume it.
    pub fn new_one_shot(fs: Arc<dyn FileSystem>, options: BuildOptions) -> Self {
        let mut bundler = IncrementalBundler::new_one_shot(fs);
        apply_options(&mut bundler, options);
        Self {
            bundler,
            lifetime: SessionLifetime::OneShot,
            generation: 0,
            committed: None,
        }
    }

    /// Execute an owned one-shot build without committing and cloning its output.
    ///
    /// Taking `self` makes a second invocation impossible at the type level.
    ///
    /// ```compile_fail
    /// # use wake_bundler::{BuildRequest, BuildSession};
    /// # fn consume_twice(session: BuildSession) {
    /// let request = BuildRequest::new("src/index.js");
    /// let _ = session.build_once(request.clone());
    /// let _ = session.build_once(request);
    /// # }
    /// ```
    pub fn build_once(mut self, request: BuildRequest) -> BuildOutput {
        assert_eq!(
            self.lifetime,
            SessionLifetime::OneShot,
            "build_once requires BuildSession::new_one_shot"
        );
        self.bundler.build(&request.entry)
    }

    /// 强制执行一次构建。适合一次性 build；推进 generation，且不会使用已提交产物短路。
    pub fn build(&mut self, request: BuildRequest) -> BuildOutput {
        self.assert_retained_api();
        self.invalidate_filesystem();
        self.rebuild_and_commit(request).clone()
    }

    pub fn build_entry(&mut self, entry: &Path) -> BuildOutput {
        self.build(BuildRequest::new(entry))
    }

    /// 构建当前 generation。同一入口且没有新的文件事件时直接复用最后一次完整产物。
    pub fn build_current(&mut self, request: BuildRequest) -> BuildOutput {
        self.build_current_ref(request).clone()
    }

    /// `build_current` 的零拷贝形式，供 watch/dev server 直接读取会话内已提交产物。
    pub fn build_current_ref(&mut self, request: BuildRequest) -> &BuildOutput {
        self.assert_retained_api();
        let needs_build = self.committed.as_ref().is_none_or(|committed| {
            committed.generation != self.generation || committed.entry != request.entry
        });
        if needs_build {
            self.rebuild_and_commit(request);
        }
        &self.committed.as_ref().expect("build committed").output
    }

    fn rebuild_and_commit(&mut self, request: BuildRequest) -> &BuildOutput {
        let output = self.bundler.build(&request.entry);
        self.committed = Some(CommittedBuild {
            generation: self.generation,
            entry: request.entry,
            output,
        });
        &self.committed.as_ref().expect("build committed").output
    }

    /// 提交一批文件系统变化，推进 generation 并使 resolver 路径缓存失效。
    pub fn invalidate_filesystem(&mut self) -> u64 {
        self.assert_retained_api();
        self.generation = self.generation.wrapping_add(1);
        self.bundler.invalidate_filesystem();
        self.generation
    }

    /// 提交 watcher 合并后的一批具体变更。
    ///
    /// `structural=true` 用于 create/remove/rename，会额外清空 resolver 路径缓存；
    /// 单纯 modify 只失效对应 loader 快照。
    pub fn invalidate_paths(&mut self, paths: &[PathBuf], structural: bool) -> u64 {
        self.assert_retained_api();
        self.generation = self.generation.wrapping_add(1);
        self.bundler.invalidate_paths(paths, structural);
        self.generation
    }

    /// Return the exact decorated filesystem used by this compilation session.
    ///
    /// Product coordinators use this read-only capability when a sibling artifact must observe
    /// the same path projection and generation query cache as the runtime build.
    pub fn file_system_view(&self) -> Arc<dyn FileSystem> {
        self.bundler.file_system_view()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn task_exec_count(&self) -> u64 {
        self.bundler.task_exec_count()
    }

    pub fn load_exec_count(&self) -> u64 {
        self.bundler.load_exec_count()
    }

    pub fn resolve_exec_count(&self) -> u64 {
        self.bundler.resolve_exec_count()
    }

    pub fn topology_reuse_count(&self) -> u64 {
        self.bundler.topology_reuse_count()
    }

    pub fn link_plan_reuse_count(&self) -> u64 {
        self.bundler.link_plan_reuse_count()
    }

    fn assert_retained_api(&self) {
        assert_ne!(
            self.lifetime,
            SessionLifetime::OneShot,
            "one-shot sessions must be consumed with BuildSession::build_once"
        );
    }
}

fn apply_options(bundler: &mut IncrementalBundler, options: BuildOptions) {
    let BuildOptions {
        project_root,
        resolve,
        platform,
        module_format,
        external_packages,
        define,
        extract_css,
        asset_inline_limit,
        public_path,
        minify,
        dead_module_elimination,
        source_map,
        css_in_js,
        drop_console,
        drop_debugger,
        tree_shaking,
        code_splitting,
        content_hash,
        entry_chunk_name,
        single_chunk_content_hash,
        persistent_cache,
        jsx,
        federation,
        target_env,
    } = options;
    let FederationBuildPlan {
        remotes,
        shared,
        shared_fallback_roots,
        entry_export,
        expose_roots,
    } = federation;

    if let Some(root) = project_root {
        bundler.set_project_root(root);
    }
    bundler
        .set_resolve_options(resolve)
        .set_platform(platform)
        .set_module_format(module_format)
        .set_external_packages(external_packages)
        .set_federation_remotes(remotes)
        .set_federation_shared(shared)
        .set_federation_shared_fallback_roots(shared_fallback_roots)
        .set_federation_expose_roots(expose_roots)
        .set_define(define)
        .set_asset_inline_limit(asset_inline_limit)
        .set_public_path(public_path)
        .set_content_hash(content_hash)
        .set_jsx_runtime(jsx.development, jsx.import_source);
    bundler.set_target_env(target_env);

    if let Some(entry_export) = entry_export {
        match entry_export {
            FederationEntryExport::PageScoped { container, expose } => {
                bundler.set_federation_entry_export(container, expose);
            }
            FederationEntryExport::BuildScoped { container, expose } => {
                bundler.set_federation_build_scoped_entry_export(container, expose);
            }
        }
    }
    if let Some(name) = entry_chunk_name {
        bundler.set_entry_chunk_name(name);
    }
    if single_chunk_content_hash {
        bundler.enable_single_chunk_content_hash();
    }
    if extract_css {
        bundler.enable_css_extraction();
    }
    if minify {
        bundler.enable_minify();
    }
    if dead_module_elimination {
        bundler.enable_dead_module_elimination();
    }
    if source_map {
        bundler.enable_sourcemap();
    }
    if css_in_js {
        bundler.enable_css_in_js();
    }
    if drop_console {
        bundler.enable_drop_console();
    }
    if drop_debugger {
        bundler.enable_drop_debugger();
    }
    if tree_shaking {
        bundler.enable_tree_shaking();
    }
    if code_splitting {
        bundler.enable_code_splitting();
    }
    if let Some(path) = persistent_cache {
        bundler.enable_persistent_cache(path);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wake_common::MemoryFileSystem;

    use super::*;

    #[test]
    fn session_reuses_the_incremental_pipeline() {
        let fs = MemoryFileSystem::from_files([(
            "src/index.js",
            "export const mode = process.env.NODE_ENV;",
        )]);
        let options = BuildOptions {
            define: vec![(
                "process.env.NODE_ENV".to_string(),
                "\"development\"".to_string(),
            )],
            ..BuildOptions::default()
        };
        let mut session = BuildSession::new(Arc::new(fs), options);

        let first = session.build(BuildRequest::new("src/index.js"));
        let second = session.build(BuildRequest::new("src/index.js"));

        assert!(!first.has_errors(), "{:?}", first.diagnostics);
        assert_eq!(first.bundle, second.bundle);
        assert!(first.bundle.contains("\"development\""));
    }

    #[test]
    fn current_generation_reuses_committed_output_without_tasks() {
        let fs = MemoryFileSystem::from_files([("src/index.js", "export const value = 1;")]);
        let mut session = BuildSession::new(Arc::new(fs), BuildOptions::default());
        let request = BuildRequest::new("src/index.js");

        let first = session.build_current(request.clone());
        let tasks = session.task_exec_count();
        let second = session.build_current(request);

        assert_eq!(second.bundle, first.bundle);
        assert_eq!(session.task_exec_count(), tasks);
        assert_eq!(session.generation(), 0);
    }

    #[test]
    fn forced_build_commits_the_latest_output_for_build_current() {
        let fs = Arc::new(MemoryFileSystem::from_files([(
            "src/index.js",
            "export const value = 1;",
        )]));
        let mut session = BuildSession::new(fs.clone(), BuildOptions::default());
        let request = BuildRequest::new("src/index.js");

        let first = session.build_current(request.clone());
        assert!(!first.has_errors(), "{:?}", first.diagnostics);

        fs.insert("src/index.js", "export const value = 2;");
        let forced = session.build(request.clone());
        assert!(!forced.has_errors(), "{:?}", forced.diagnostics);
        assert_ne!(forced.bundle, first.bundle);
        let tasks = session.task_exec_count();

        let current = session.build_current(request);
        assert_eq!(current.bundle, forced.bundle);
        assert_eq!(session.task_exec_count(), tasks);
        assert_eq!(session.generation(), 1);
    }

    #[test]
    fn invalidation_clears_cached_resolver_miss() {
        let fs = Arc::new(MemoryFileSystem::from_files([(
            "src/index.js",
            "import { value } from './created.js'; export { value };",
        )]));
        let mut session = BuildSession::new(fs.clone(), BuildOptions::default());

        let missing = session.build_current(BuildRequest::new("src/index.js"));
        assert!(missing.has_errors());
        let resolves = session.resolve_exec_count();

        fs.insert("src/created.js", "export const value = 42;");
        assert_eq!(session.invalidate_filesystem(), 1);
        let rebuilt = session.build_current(BuildRequest::new("src/index.js"));

        assert!(!rebuilt.has_errors(), "{:?}", rebuilt.diagnostics);
        assert_eq!(rebuilt.module_count, 2);
        assert_eq!(session.resolve_exec_count() - resolves, 1);
    }

    #[test]
    fn content_edit_reloads_only_changed_module() {
        let fs = Arc::new(MemoryFileSystem::from_files([
            (
                "src/index.js",
                "import { value } from './dep.js'; export { value };",
            ),
            ("src/dep.js", "export const value = 1;"),
        ]));
        let mut session = BuildSession::new(fs.clone(), BuildOptions::default());
        let request = BuildRequest::new("src/index.js");
        let first = session.build_current(request.clone());
        assert!(!first.has_errors());
        assert_eq!(first.updated_module_count, first.module_count);
        assert_eq!(first.cached_module_count, 0);
        let loads = session.load_exec_count();
        let tasks = session.task_exec_count();
        let resolves = session.resolve_exec_count();
        let link_reuses = session.link_plan_reuse_count();
        assert_eq!(loads, 2);
        assert_eq!(resolves, 1);

        fs.insert("src/dep.js", "export const value = 2;");
        session.invalidate_paths(&[PathBuf::from("src/dep.js")], false);
        let rebuilt = session.build_current(request);

        assert!(!rebuilt.has_errors());
        assert_eq!(rebuilt.updated_module_count, 1);
        assert_eq!(rebuilt.cached_module_count, 1);
        assert_eq!(session.load_exec_count() - loads, 1);
        assert_eq!(
            session.task_exec_count() - tasks,
            3,
            "changed module reruns parse, optimize, and body emission only"
        );
        assert_eq!(
            session.resolve_exec_count(),
            resolves,
            "普通内容修改应复用稳定解析边"
        );
        assert_eq!(
            session.topology_reuse_count(),
            1,
            "依赖形状未变时应复用持久模块图"
        );
        assert_eq!(
            session.link_plan_reuse_count() - link_reuses,
            1,
            "绑定活跃性与依赖图未变时应复用 link/chunk 规划"
        );
        assert!(rebuilt.bundle.contains("2"));
    }

    #[test]
    fn changed_import_adds_only_the_new_resolve_edge() {
        let fs = Arc::new(MemoryFileSystem::from_files([
            (
                "src/index.js",
                "import { value } from './a.js'; export { value };",
            ),
            ("src/a.js", "export const value = 1;"),
            ("src/b.js", "export const value = 2;"),
        ]));
        let mut session = BuildSession::new(fs.clone(), BuildOptions::default());
        let request = BuildRequest::new("src/index.js");
        let first = session.build_current(request.clone());
        assert!(!first.has_errors());
        let resolves = session.resolve_exec_count();
        let topology_reuses = session.topology_reuse_count();
        let link_reuses = session.link_plan_reuse_count();

        fs.insert(
            "src/index.js",
            "import { value } from './b.js'; export { value };",
        );
        session.invalidate_paths(&[PathBuf::from("src/index.js")], false);
        let rebuilt = session.build_current(request);

        assert!(!rebuilt.has_errors(), "{:?}", rebuilt.diagnostics);
        assert_eq!(rebuilt.module_count, 2);
        assert_eq!(session.resolve_exec_count() - resolves, 1);
        assert_eq!(
            session.topology_reuse_count(),
            topology_reuses,
            "specifier 改变必须回退全图扫描"
        );
        assert_eq!(
            session.link_plan_reuse_count(),
            link_reuses,
            "依赖图改变必须重算 link/chunk 规划"
        );
        assert!(rebuilt.bundle.contains("2"));
    }

    #[test]
    fn removed_dependency_falls_back_and_drops_old_module() {
        let fs = Arc::new(MemoryFileSystem::from_files([
            (
                "src/index.js",
                "import { value } from './dep.js'; export default value;",
            ),
            ("src/dep.js", "export const value = 1;"),
        ]));
        let mut session = BuildSession::new(fs.clone(), BuildOptions::default());
        let request = BuildRequest::new("src/index.js");
        let first = session.build_current(request.clone());
        assert_eq!(first.module_count, 2);
        let topology_reuses = session.topology_reuse_count();

        fs.insert("src/index.js", "export default 7;");
        session.invalidate_paths(&[PathBuf::from("src/index.js")], false);
        let rebuilt = session.build_current(request);

        assert!(!rebuilt.has_errors(), "{:?}", rebuilt.diagnostics);
        assert_eq!(rebuilt.module_count, 1);
        assert!(rebuilt.bundle.contains("7"));
        assert_eq!(session.topology_reuse_count(), topology_reuses);
    }

    #[test]
    fn structural_event_re_resolves_unchanged_import() {
        let fs = Arc::new(MemoryFileSystem::from_files([
            (
                "src/index.js",
                "import value from './dep'; export default value;",
            ),
            ("src/dep.js", "export default 1;"),
        ]));
        let mut session = BuildSession::new(fs.clone(), BuildOptions::default());
        let request = BuildRequest::new("src/index.js");
        let first = session.build_current(request.clone());
        assert!(!first.has_errors());
        assert!(first.bundle.contains("1"));
        let topology_reuses = session.topology_reuse_count();

        fs.insert("src/dep", "export default 2;");
        session.invalidate_paths(&[PathBuf::from("src/dep")], true);
        let rebuilt = session.build_current(request);

        assert!(!rebuilt.has_errors(), "{:?}", rebuilt.diagnostics);
        assert!(rebuilt.bundle.contains("2"));
        assert_eq!(session.topology_reuse_count(), topology_reuses);
    }

    #[test]
    fn package_json_modify_invalidates_resolution_topology() {
        let fs = Arc::new(MemoryFileSystem::from_files([
            (
                "src/index.js",
                "import value from './pkg'; export default value;",
            ),
            ("src/pkg/package.json", r#"{"main":"a.js"}"#),
            ("src/pkg/a.js", "export default 1;"),
            ("src/pkg/b.js", "export default 2;"),
        ]));
        let mut session = BuildSession::new(fs.clone(), BuildOptions::default());
        let request = BuildRequest::new("src/index.js");
        let first = session.build_current(request.clone());
        assert!(!first.has_errors(), "{:?}", first.diagnostics);
        assert!(first.bundle.contains("1"));
        let topology_reuses = session.topology_reuse_count();

        fs.insert("src/pkg/package.json", r#"{"main":"b.js"}"#);
        session.invalidate_paths(&[PathBuf::from("src/pkg/package.json")], false);
        let rebuilt = session.build_current(request);

        assert!(!rebuilt.has_errors(), "{:?}", rebuilt.diagnostics);
        assert!(rebuilt.bundle.contains("2"));
        assert_eq!(session.topology_reuse_count(), topology_reuses);
    }

    #[test]
    fn error_build_does_not_poison_stable_graph() {
        let fs = Arc::new(MemoryFileSystem::from_files([
            (
                "src/index.js",
                "import { value } from './dep.js'; export default value;",
            ),
            ("src/dep.js", "export const value = 1;"),
        ]));
        let mut session = BuildSession::new(fs.clone(), BuildOptions::default());
        let request = BuildRequest::new("src/index.js");
        assert!(!session.build_current(request.clone()).has_errors());

        fs.insert("src/dep.js", "export const value = ;");
        session.invalidate_paths(&[PathBuf::from("src/dep.js")], false);
        assert!(session.build_current(request.clone()).has_errors());

        fs.insert("src/dep.js", "export const value = 3;");
        session.invalidate_paths(&[PathBuf::from("src/dep.js")], false);
        let recovered = session.build_current(request);
        assert!(!recovered.has_errors(), "{:?}", recovered.diagnostics);
        assert!(recovered.bundle.contains("3"));
    }

    #[test]
    fn stable_graph_replays_side_outputs() {
        let fs = Arc::new(MemoryFileSystem::new());
        fs.insert(
            "src/index.js",
            "import logo from './logo.png'; export default logo + 'a';",
        );
        fs.insert("src/logo.png", vec![0_u8, 1, 2, 3]);
        let options = BuildOptions {
            asset_inline_limit: 0,
            ..BuildOptions::default()
        };
        let mut session = BuildSession::new(fs.clone(), options);
        let request = BuildRequest::new("src/index.js");
        let first = session.build_current(request.clone());
        assert!(!first.has_errors(), "{:?}", first.diagnostics);
        assert_eq!(first.assets.len(), 1);
        let asset_name = first.assets[0].file_name.clone();
        let asset_bytes = first.assets[0].bytes.clone();
        let asset_owners = first.assets[0].owner_module_ids.clone();
        assert!(first.assets[0].unscoped_css_owner_module_ids.is_empty());

        fs.insert(
            "src/index.js",
            "import logo from './logo.png'; export default logo + 'b';",
        );
        session.invalidate_paths(&[PathBuf::from("src/index.js")], false);
        let rebuilt = session.build_current(request);

        assert!(!rebuilt.has_errors(), "{:?}", rebuilt.diagnostics);
        assert_eq!(session.topology_reuse_count(), 1);
        assert_eq!(rebuilt.assets.len(), 1);
        assert_eq!(rebuilt.assets[0].file_name, asset_name);
        assert_eq!(rebuilt.assets[0].bytes, asset_bytes);
        assert_eq!(rebuilt.assets[0].owner_module_ids, asset_owners);
        assert!(rebuilt.assets[0].unscoped_css_owner_module_ids.is_empty());
    }

    #[test]
    fn stable_graph_replays_unscoped_css_ownership() {
        let fs = Arc::new(MemoryFileSystem::from_files([
            (
                "src/index.js",
                "import './global.css'; export default 'first';",
            ),
            ("src/global.css", "body { color: rebeccapurple; }"),
        ]));
        let options = BuildOptions {
            extract_css: true,
            ..BuildOptions::default()
        };
        let mut session = BuildSession::new(fs.clone(), options);
        let request = BuildRequest::new("src/index.js");
        let first = session.build_current(request.clone());
        assert!(!first.has_errors(), "{:?}", first.diagnostics);
        let first_css = first
            .assets
            .iter()
            .find(|asset| asset.is_css)
            .expect("extracted CSS");
        assert_eq!(
            first_css.unscoped_css_owner_module_ids,
            first_css.owner_module_ids
        );
        assert!(!first_css.owner_module_ids.is_empty());
        let first_owners = first_css.owner_module_ids.clone();

        fs.insert(
            "src/index.js",
            "import './global.css'; export default 'second';",
        );
        session.invalidate_paths(&[PathBuf::from("src/index.js")], false);
        let rebuilt = session.build_current(request);
        assert!(!rebuilt.has_errors(), "{:?}", rebuilt.diagnostics);
        assert_eq!(session.topology_reuse_count(), 1);
        let rebuilt_css = rebuilt
            .assets
            .iter()
            .find(|asset| asset.is_css)
            .expect("replayed extracted CSS");
        assert_eq!(rebuilt_css.owner_module_ids, first_owners);
        assert_eq!(
            rebuilt_css.unscoped_css_owner_module_ids,
            rebuilt_css.owner_module_ids
        );
    }

    #[test]
    fn layout_only_edit_reuses_topology_and_matches_fresh_sourcemap() {
        let fs = Arc::new(MemoryFileSystem::from_files([
            (
                "src/index.js",
                "import { value } from './dep.js'; export default value;",
            ),
            ("src/dep.js", "export const value = 1;"),
        ]));
        let options = BuildOptions {
            source_map: true,
            ..BuildOptions::default()
        };
        let mut session = BuildSession::new(fs.clone(), options.clone());
        let request = BuildRequest::new("src/index.js");
        assert!(!session.build_current(request.clone()).has_errors());

        fs.insert(
            "src/index.js",
            "// span shift\n\nimport { value } from './dep.js';\nexport default value;",
        );
        session.invalidate_paths(&[PathBuf::from("src/index.js")], false);
        let incremental = session.build_current(request.clone()).clone();
        assert_eq!(session.topology_reuse_count(), 1);

        let mut fresh = BuildSession::new(fs, options);
        let cold = fresh.build_current(request);
        assert!(!incremental.has_errors(), "{:?}", incremental.diagnostics);
        assert!(!cold.has_errors(), "{:?}", cold.diagnostics);
        assert_eq!(incremental.bundle, cold.bundle);
        assert_eq!(incremental.module_count, cold.module_count);
        assert_eq!(
            incremental.chunks[incremental.entry_chunk].source_map,
            cold.chunks[cold.entry_chunk].source_map
        );
    }

    fn assert_build_outputs_equal(left: &BuildOutput, right: &BuildOutput) {
        assert_eq!(left.bundle, right.bundle);
        assert_eq!(left.module_count, right.module_count);
        assert_eq!(left.updated_module_count, right.updated_module_count);
        assert_eq!(left.cached_module_count, right.cached_module_count);
        assert_eq!(left.entry_chunk, right.entry_chunk);
        assert_eq!(
            format!("{:?}", left.diagnostics),
            format!("{:?}", right.diagnostics)
        );

        assert_eq!(left.chunks.len(), right.chunks.len());
        for (left, right) in left.chunks.iter().zip(&right.chunks) {
            assert_eq!(left.name, right.name);
            assert_eq!(left.file_name, right.file_name);
            assert_eq!(left.code, right.code);
            assert_eq!(left.kind, right.kind);
            assert_eq!(left.is_entry, right.is_entry);
            assert_eq!(left.chunk_id, right.chunk_id);
            assert_eq!(left.module_ids, right.module_ids);
            assert_eq!(left.imports, right.imports);
            assert_eq!(left.dynamic_imports, right.dynamic_imports);
            assert_eq!(left.styles, right.styles);
            assert_eq!(left.source_map, right.source_map);
        }

        assert_eq!(left.assets.len(), right.assets.len());
        for (left, right) in left.assets.iter().zip(&right.assets) {
            assert_eq!(left.file_name, right.file_name);
            assert_eq!(left.bytes, right.bytes);
            assert_eq!(left.is_css, right.is_css);
            assert_eq!(left.owner_module_ids, right.owner_module_ids);
            assert_eq!(
                left.unscoped_css_owner_module_ids,
                right.unscoped_css_owner_module_ids
            );
        }
    }

    #[test]
    fn one_shot_and_retained_sessions_produce_identical_complete_outputs() {
        let fs = Arc::new(MemoryFileSystem::new());
        fs.insert(
            "src/index.js",
            "import './global.css';import logo from './logo.png';export {logo};export const load=()=>import('./lazy.js');",
        );
        fs.insert(
            "src/lazy.js",
            "export const value='lazy';export const deeper=()=>import('./deeper.js');",
        );
        fs.insert("src/deeper.js", "export const value='deeper';");
        fs.insert("src/global.css", "body { color: rebeccapurple; }");
        fs.insert("src/logo.png", vec![0_u8, 1, 2, 3]);
        let options = BuildOptions {
            extract_css: true,
            asset_inline_limit: 0,
            minify: true,
            dead_module_elimination: true,
            source_map: true,
            tree_shaking: true,
            code_splitting: true,
            entry_chunk_name: Some("application".to_string()),
            ..BuildOptions::default()
        };
        let request = BuildRequest::new("src/index.js");

        let mut retained = BuildSession::new(fs.clone(), options.clone());
        let retained_output = retained.build_current(request.clone());
        let one_shot_output = BuildSession::new_one_shot(fs, options).build_once(request.clone());

        assert!(
            !retained_output.has_errors(),
            "{:?}",
            retained_output.diagnostics
        );
        assert!(
            !one_shot_output.has_errors(),
            "{:?}",
            one_shot_output.diagnostics
        );
        assert_build_outputs_equal(&retained_output, &one_shot_output);
    }

    #[test]
    fn one_shot_session_owns_jsx_options_and_skips_retained_load_cache() {
        let fs = Arc::new(MemoryFileSystem::from_files([(
            "src/index.jsx",
            "export const view=<section>hello</section>;",
        )]));
        let options = BuildOptions {
            external_packages: vec!["preact".to_string()],
            jsx: JsxOptions {
                development: true,
                import_source: String::from("preact"),
            },
            entry_chunk_name: Some("client".to_string()),
            single_chunk_content_hash: true,
            ..BuildOptions::default()
        };
        let retained = BuildSession::new(fs.clone(), options.clone());
        assert!(retained.bundler.load_cache_enabled_for_test());

        let one_shot = BuildSession::new_one_shot(fs, options);
        assert_eq!(one_shot.lifetime, SessionLifetime::OneShot);
        assert!(!one_shot.bundler.load_cache_enabled_for_test());
        let output = one_shot.build_once(BuildRequest::new("src/index.jsx"));

        assert!(!output.has_errors(), "{:?}", output.diagnostics);
        assert!(output.bundle.contains("preact/jsx-dev-runtime"));
        assert!(output.bundle.contains("jsxDEV"));
        assert!(output.entry().file_name.starts_with("client."));
        assert!(output.entry().file_name.ends_with(".js"));
    }

    #[test]
    #[should_panic(expected = "one-shot sessions must be consumed with BuildSession::build_once")]
    fn one_shot_session_rejects_retained_build_api() {
        let fs = Arc::new(MemoryFileSystem::from_files([(
            "src/index.js",
            "export const value=1;",
        )]));
        let mut session = BuildSession::new_one_shot(fs, BuildOptions::default());
        let _ = session.build(BuildRequest::new("src/index.js"));
    }

    #[test]
    fn federation_build_plan_applies_every_bundler_scoped_input() {
        let fs = MemoryFileSystem::from_files([
            (
                "src/index.js",
                "export const fallback=()=>import('./fallback.js');export const app=()=>import('./app.js');",
            ),
            (
                "src/fallback.js",
                "import {value} from 'shared-a';export {value};",
            ),
            (
                "src/app.js",
                "import {value} from 'shared-a';export {value};export const remote=()=>import('catalog/Other');",
            ),
            (
                "node_modules/shared-a/package.json",
                r#"{"name":"shared-a","version":"1.0.0","main":"index.js"}"#,
            ),
            (
                "node_modules/shared-a/index.js",
                "export const value='shared';",
            ),
        ]);
        let options = BuildOptions {
            code_splitting: true,
            federation: FederationBuildPlan {
                remotes: vec!["catalog".to_string()],
                shared: vec![(
                    "shared-a".to_string(),
                    "shared-a".to_string(),
                    "group".to_string(),
                )],
                shared_fallback_roots: vec![PathBuf::from("src/fallback.js")],
                entry_export: Some(FederationEntryExport::build_scoped(
                    "shell",
                    "./__wake_container__",
                )),
                expose_roots: vec![("app".to_string(), "./Widget".to_string())],
            },
            ..BuildOptions::default()
        };

        let output = BuildSession::new_one_shot(Arc::new(fs), options)
            .build_once(BuildRequest::new("src/index.js"));

        assert!(!output.has_errors(), "{:?}", output.diagnostics);
        let fallback = output
            .chunks
            .iter()
            .find(|chunk| chunk.name == "fallback")
            .expect("shared fallback chunk");
        assert!(!fallback.code.contains("__wake_require__.shared("));
        let app = output
            .chunks
            .iter()
            .find(|chunk| chunk.name == "app")
            .expect("expose application chunk");
        assert!(
            app.code
                .contains("__wake_require__.shared(\"shared-a\", \"group\")"),
            "{}",
            app.code
        );
        assert!(
            app.code
                .contains("runtimeImport(\"catalog/Other\", \"./Widget\")")
                || app
                    .code
                    .contains("runtimeImport(\"catalog/Other\",\"./Widget\")"),
            "{}",
            app.code
        );
        assert!(output.entry().code.contains("loadFederatedAsset"));
        assert!(
            output
                .entry()
                .code
                .contains("__wake_federation_asset_context__.buildId")
        );
    }
}
