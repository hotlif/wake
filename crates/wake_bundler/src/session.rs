use std::path::{Path, PathBuf};
use std::sync::Arc;

use wake_common::FileSystem;
use wake_ecma_transform::TargetEnv;

use crate::{BuildOutput, IncrementalBundler, ResolveOptions};

/// 一次构建使用的稳定配置。配置在 session 创建时归一化，避免构建过程中通过 setter
/// 改变任务语义。
#[derive(Clone, Debug)]
pub struct BuildOptions {
    pub resolve: ResolveOptions,
    pub define: Vec<(String, String)>,
    pub extract_css: bool,
    pub asset_inline_limit: usize,
    pub public_path: String,
    pub minify: bool,
    pub dead_module_elimination: bool,
    pub mangle: bool,
    pub source_map: bool,
    pub css_in_js: bool,
    pub drop_console: bool,
    pub drop_debugger: bool,
    pub tree_shaking: bool,
    pub code_splitting: bool,
    pub content_hash: bool,
    pub persistent_cache: Option<PathBuf>,
    /// 已规范化的浏览器目标。
    pub target_env: TargetEnv,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            resolve: ResolveOptions::default(),
            define: vec![(
                "process.env.NODE_ENV".to_string(),
                "\"production\"".to_string(),
            )],
            extract_css: false,
            asset_inline_limit: usize::MAX,
            public_path: "/".to_string(),
            minify: false,
            dead_module_elimination: false,
            mangle: false,
            source_map: false,
            css_in_js: false,
            drop_console: false,
            drop_debugger: false,
            tree_shaking: false,
            code_splitting: false,
            content_hash: true,
            persistent_cache: None,
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
    generation: u64,
    committed: Option<CommittedBuild>,
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
            generation: 0,
            committed: None,
        }
    }

    /// 兼容迁移入口：接管一个已配置完成的增量打包器。
    pub fn from_incremental(mut bundler: IncrementalBundler) -> Self {
        bundler.enable_load_cache();
        Self {
            bundler,
            generation: 0,
            committed: None,
        }
    }

    /// 强制执行一次构建。适合一次性 build；不会使用 generation 产物短路。
    pub fn build(&mut self, request: BuildRequest) -> BuildOutput {
        self.bundler.invalidate_filesystem();
        self.bundler.build(&request.entry)
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
        let needs_build = self.committed.as_ref().is_none_or(|committed| {
            committed.generation != self.generation || committed.entry != request.entry
        });
        if needs_build {
            let output = self.bundler.build(&request.entry);
            self.committed = Some(CommittedBuild {
                generation: self.generation,
                entry: request.entry,
                output,
            });
        }
        &self.committed.as_ref().expect("build committed").output
    }

    /// 提交一批文件系统变化，推进 generation 并使 resolver 路径缓存失效。
    pub fn invalidate_filesystem(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.bundler.invalidate_filesystem();
        self.generation
    }

    /// 提交 watcher 合并后的一批具体变更。
    ///
    /// `structural=true` 用于 create/remove/rename，会额外清空 resolver 路径缓存；
    /// 单纯 modify 只失效对应 loader 快照。
    pub fn invalidate_paths(&mut self, paths: &[PathBuf], structural: bool) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.bundler.invalidate_paths(paths, structural);
        self.generation
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
}

fn apply_options(bundler: &mut IncrementalBundler, options: BuildOptions) {
    bundler
        .set_resolve_options(options.resolve)
        .set_define(options.define)
        .set_asset_inline_limit(options.asset_inline_limit)
        .set_public_path(options.public_path)
        .set_content_hash(options.content_hash);
    bundler.set_target_env(options.target_env);

    if options.extract_css {
        bundler.enable_css_extraction();
    }
    if options.minify {
        bundler.enable_minify();
    }
    if options.dead_module_elimination {
        bundler.enable_dead_module_elimination();
    }
    if options.mangle {
        bundler.enable_mangle();
    }
    if options.source_map {
        bundler.enable_sourcemap();
    }
    if options.css_in_js {
        bundler.enable_css_in_js();
    }
    if options.drop_console {
        bundler.enable_drop_console();
    }
    if options.drop_debugger {
        bundler.enable_drop_debugger();
    }
    if options.tree_shaking {
        bundler.enable_tree_shaking();
    }
    if options.code_splitting {
        bundler.enable_code_splitting();
    }
    if let Some(path) = options.persistent_cache {
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
        let loads = session.load_exec_count();
        let tasks = session.task_exec_count();
        let resolves = session.resolve_exec_count();
        assert_eq!(loads, 2);
        assert_eq!(resolves, 1);

        fs.insert("src/dep.js", "export const value = 2;");
        session.invalidate_paths(&[PathBuf::from("src/dep.js")], false);
        let rebuilt = session.build_current(request);

        assert!(!rebuilt.has_errors());
        assert_eq!(session.load_exec_count() - loads, 1);
        assert_eq!(session.task_exec_count() - tasks, 2);
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
}
