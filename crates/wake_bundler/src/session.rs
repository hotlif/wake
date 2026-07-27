use std::path::{Path, PathBuf};
use std::sync::Arc;

use wake_common::FileSystem;

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
}

impl BuildSession {
    pub fn new(fs: Arc<dyn FileSystem>, options: BuildOptions) -> Self {
        let mut bundler = IncrementalBundler::new(fs);
        apply_options(&mut bundler, options);
        Self { bundler }
    }

    pub fn build(&mut self, request: BuildRequest) -> BuildOutput {
        self.bundler.build(&request.entry)
    }

    pub fn build_entry(&mut self, entry: &Path) -> BuildOutput {
        self.build(BuildRequest::new(entry))
    }
}

fn apply_options(bundler: &mut IncrementalBundler, options: BuildOptions) {
    bundler
        .set_resolve_options(options.resolve)
        .set_define(options.define)
        .set_asset_inline_limit(options.asset_inline_limit)
        .set_public_path(options.public_path)
        .set_content_hash(options.content_hash);

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
}
