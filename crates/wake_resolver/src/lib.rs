//! # wake_resolver — 模块解析
//!
//! DESIGN §5.1：Node 解析算法的现代子集。v1 覆盖相对路径 + 扩展名补全 + 目录 index +
//! `node_modules` 逐级向上 + `package.json` 的 `main`/`module` 字段 + 结果缓存。
//! `exports`/`imports` 字段、tsconfig paths、browser 字段、symlink 是 resolver v2（DESIGN §4.7 / P4）。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use wake_common::{FileSystem, FxHashMap, fs::normalize};

pub mod pnp;
mod pnpfs;

pub use pnp::PnpManifest;
pub use pnpfs::PnpFileSystem;

/// 解析选项。
#[derive(Clone, Debug)]
pub struct ResolveOptions {
    /// 扩展名补全顺序。
    pub extensions: Vec<String>,
    /// `package.json` 入口字段优先级（现代优先 `module` 再 `main`）。
    pub main_fields: Vec<String>,
    /// 路径别名 `(前缀, 绝对目标)`（如 `@`→`<root>/src`、`@@`→`<root>`、`@@@/{ns}`→扫描产物）。
    /// 匹配规则：说明符 == 前缀 或以 `前缀/` 开头；命中最长前缀，重写后走文件/目录解析。
    /// 保持既定行为 webpack `resolve.alias`（WAKE-COMPATIBILITY §H）。默认空 → 行为与接入前逐字节一致。
    pub alias: Vec<(String, PathBuf)>,
}

impl Default for ResolveOptions {
    fn default() -> ResolveOptions {
        ResolveOptions {
            extensions: [
                ".ts", ".tsx", ".mts", ".cts", ".js", ".mjs", ".cjs", ".jsx", ".json", ".css",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            main_fields: vec!["module".to_string(), "main".to_string()],
            alias: Vec::new(),
        }
    }
}

/// 解析失败。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolveError {
    pub specifier: String,
    pub from: PathBuf,
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "无法从 `{}` 解析模块 `{}`",
            self.from.display(),
            self.specifier
        )
    }
}

impl std::error::Error for ResolveError {}

/// 模块解析器。结果缓存降低重复解析成本（DESIGN §5.1）。
pub struct Resolver {
    fs: Arc<dyn FileSystem>,
    options: ResolveOptions,
    /// `(from_dir, specifier)` → 解析结果（`None` = 未找到）。
    /// `Mutex` 而非 `RefCell`：使 `Resolver: Sync`，让打包器能经 `Arc<Resolver>` 在工作窃取
    /// 执行器上**并行 resolve**（临界区仅包住 cache 的 get/insert，昂贵的 FS 探测在锁外，竞争极小）。
    cache: Mutex<FxHashMap<(PathBuf, String), Option<PathBuf>>>,
    /// Yarn PnP 清单。`Some` 时裸说明符走 PnP 依赖图（不走 `node_modules` 上溯）。
    pnp: Option<Arc<PnpManifest>>,
}

impl Resolver {
    pub fn new(fs: Arc<dyn FileSystem>) -> Resolver {
        Resolver::with_options(fs, ResolveOptions::default())
    }

    pub fn with_options(fs: Arc<dyn FileSystem>, options: ResolveOptions) -> Resolver {
        Resolver {
            fs,
            options,
            cache: Mutex::new(FxHashMap::default()),
            pnp: None,
        }
    }

    /// PnP 模式解析器：裸说明符经 `manifest` 的依赖图定位（DESIGN §5.1）。
    ///
    /// `fs` 应为 [`PnpFileSystem`] 包裹后的 zip 感知文件系统，使 zip 内文件的
    /// `is_file`/`read` 透明工作。
    pub fn with_pnp(fs: Arc<dyn FileSystem>, manifest: Arc<PnpManifest>) -> Resolver {
        Resolver::with_pnp_options(fs, manifest, ResolveOptions::default())
    }

    /// PnP 模式解析器，附带解析选项（含别名）。PnP 与别名并存时用此构造，
    /// 使 `enable_pnp` 切换解析器后仍保留 CLI 配置的别名（否则会退回默认丢别名）。
    pub fn with_pnp_options(
        fs: Arc<dyn FileSystem>,
        manifest: Arc<PnpManifest>,
        options: ResolveOptions,
    ) -> Resolver {
        Resolver {
            fs,
            options,
            cache: Mutex::new(FxHashMap::default()),
            pnp: Some(manifest),
        }
    }

    /// 从 `from_dir` 解析 `specifier` 到一个规范文件路径。
    pub fn resolve(&self, specifier: &str, from_dir: &Path) -> Result<PathBuf, ResolveError> {
        let key = (from_dir.to_path_buf(), specifier.to_string());
        // 先取 cache（锁瞬间释放：`.cloned()` 拷出 Option 后 guard 即析构）——**关键**是别把锁
        // 持到 `resolve_uncached` 的 FS 探测期间，否则并行退化为串行。
        let cached = self.cache.lock().unwrap().get(&key).cloned();
        if let Some(resolved) = cached {
            return resolved.ok_or_else(|| self.err(specifier, from_dir));
        }
        // 未命中：昂贵的 FS 探测在锁外进行（并行的收益全在这里）。两个线程同 key 竞争时都会算一遍
        // 再各自 insert——幂等无害，换取零锁争用。
        let resolved = self.resolve_uncached(specifier, from_dir);
        self.cache.lock().unwrap().insert(key, resolved.clone());
        resolved.ok_or_else(|| self.err(specifier, from_dir))
    }

    /// 文件系统 generation 变化后清空路径解析结果。
    ///
    /// Resolver 会缓存成功与失败；watch 中新增、删除或重命名文件后，旧结果都可能失效。
    pub fn clear_cache(&self) {
        self.cache.lock().unwrap().clear();
    }

    fn err(&self, specifier: &str, from_dir: &Path) -> ResolveError {
        ResolveError {
            specifier: specifier.to_string(),
            from: from_dir.to_path_buf(),
        }
    }

    fn resolve_uncached(&self, specifier: &str, from_dir: &Path) -> Option<PathBuf> {
        // 别名优先：命中则重写为绝对目标后走文件/目录解析（扩展名补全 / index / package.json）。
        if let Some(aliased) = self.apply_alias(specifier) {
            return self.resolve_as_file_or_dir(&aliased);
        }
        if specifier.starts_with("./") || specifier.starts_with("../") || specifier.starts_with('/')
        {
            let base = if specifier.starts_with('/') {
                normalize(Path::new(specifier))
            } else {
                normalize(&from_dir.join(specifier))
            };
            self.resolve_as_file_or_dir(&base)
        } else if let Some(pnp) = &self.pnp {
            // PnP 模式：裸说明符经依赖图定位到「未限定」包路径，再走文件/目录解析补 main/index/扩展名。
            let unqualified = pnp.resolve_bare(specifier, from_dir).ok()?;
            self.resolve_as_file_or_dir(&unqualified)
        } else {
            self.resolve_node_modules(specifier, from_dir)
        }
    }

    /// 别名匹配：说明符 == 前缀 或以 `前缀/` 开头 → 重写为 `目标 [+ 余下子路径]`（命中最长前缀）。
    /// 无别名（默认）时零成本返回 `None`。
    fn apply_alias(&self, specifier: &str) -> Option<PathBuf> {
        if self.options.alias.is_empty() {
            return None;
        }
        let mut best: Option<(&str, &Path)> = None;
        for (key, target) in &self.options.alias {
            let hit = specifier == key
                || (specifier.len() > key.len()
                    && specifier.as_bytes()[key.len()] == b'/'
                    && specifier.starts_with(key.as_str()));
            if hit && best.is_none_or(|(bk, _)| key.len() > bk.len()) {
                best = Some((key, target));
            }
        }
        let (key, target) = best?;
        let rest = specifier[key.len()..].trim_start_matches('/');
        Some(if rest.is_empty() {
            normalize(target)
        } else {
            normalize(&target.join(rest))
        })
    }

    fn resolve_as_file_or_dir(&self, path: &Path) -> Option<PathBuf> {
        self.resolve_as_file(path)
            .or_else(|| self.resolve_as_directory(path))
    }

    /// 作为文件解析：原样、TS `.js`-孪生、或补各扩展名。
    fn resolve_as_file(&self, path: &Path) -> Option<PathBuf> {
        if self.fs.is_file(path) {
            return Some(normalize(path));
        }
        // TS 约定（moduleResolution nodenext）：`import "./x.js"` 而磁盘上其实是
        // `./x.ts`/`.tsx`。仅当字面 `.js` 不存在时才试孪生扩展名——真实 node_modules 的
        // `.js` 上面一步已命中，不多花 stat。
        if let Some(twins) = ts_twin_candidates(path) {
            for cand in twins {
                if self.fs.is_file(&cand) {
                    return Some(normalize(&cand));
                }
            }
        }
        for ext in &self.options.extensions {
            let cand = append_ext(path, ext);
            if self.fs.is_file(&cand) {
                return Some(normalize(&cand));
            }
        }
        None
    }

    /// 作为目录解析：package.json main/module，或 index.*。
    fn resolve_as_directory(&self, path: &Path) -> Option<PathBuf> {
        if !self.fs.is_dir(path) {
            return None;
        }
        let pkg = path.join("package.json");
        if self.fs.is_file(&pkg)
            && let Some(main) = self.read_pkg_entry(&pkg)
        {
            let main_path = normalize(&path.join(&main));
            if let Some(f) = self.resolve_as_file(&main_path) {
                return Some(f);
            }
            // main 指向目录时再找其 index。
            if let Some(f) = self.resolve_as_file(&main_path.join("index")) {
                return Some(f);
            }
        }
        self.resolve_as_file(&path.join("index"))
    }

    fn resolve_node_modules(&self, specifier: &str, from_dir: &Path) -> Option<PathBuf> {
        let (pkg_name, subpath) = split_package(specifier);
        let mut dir = Some(normalize(from_dir));
        while let Some(d) = dir {
            let pkg_dir = d.join("node_modules").join(&pkg_name);
            if self.fs.is_dir(&pkg_dir) {
                let target = if subpath.is_empty() {
                    if let Some(f) = self.resolve_as_directory(&pkg_dir) {
                        return Some(f);
                    }
                    pkg_dir.clone()
                } else {
                    pkg_dir.join(&subpath)
                };
                if let Some(r) = self.resolve_as_file_or_dir(&target) {
                    return Some(r);
                }
            }
            dir = d.parent().map(|p| p.to_path_buf());
        }
        None
    }

    /// 读 package.json 的入口字段（按 main_fields 优先级）。
    fn read_pkg_entry(&self, pkg: &Path) -> Option<String> {
        let text = self.fs.read_to_string(pkg).ok()?;
        let json: serde_json::Value = serde_json::from_str(&text).ok()?;
        for field in &self.options.main_fields {
            if let Some(s) = json.get(field).and_then(|v| v.as_str()) {
                return Some(s.to_string());
            }
        }
        None
    }
}

/// TS 的 `.js`-扩展名导入约定：`import "./x.js"` 磁盘上可能是 `./x.ts`/`.tsx`。
/// 返回把 JS 扩展名换成 TS 对应扩展名后的候选路径（按优先级）；非 JS 扩展名返回 `None`。
fn ts_twin_candidates(path: &Path) -> Option<Vec<PathBuf>> {
    let ext = path.extension()?.to_str()?;
    let twins: &[&str] = match ext {
        "js" => &["ts", "tsx"],
        "jsx" => &["tsx"],
        "mjs" => &["mts"],
        "cjs" => &["cts"],
        _ => return None,
    };
    Some(twins.iter().map(|t| path.with_extension(t)).collect())
}

/// 追加扩展名（`src/foo` + `.js` → `src/foo.js`）。
fn append_ext(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(ext);
    PathBuf::from(s)
}

/// 拆分裸说明符为 (包名, 子路径)。处理 scoped 包 `@scope/name/sub`。
pub(crate) fn split_package(specifier: &str) -> (String, String) {
    if let Some(rest) = specifier.strip_prefix('@') {
        // @scope/name[/sub]
        let mut parts = rest.splitn(3, '/');
        let scope = parts.next().unwrap_or("");
        let name = parts.next().unwrap_or("");
        let sub = parts.next().unwrap_or("");
        (format!("@{scope}/{name}"), sub.to_string())
    } else {
        match specifier.split_once('/') {
            Some((name, sub)) => (name.to_string(), sub.to_string()),
            None => (specifier.to_string(), String::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wake_common::MemoryFileSystem;

    fn resolver(files: &[(&str, &str)]) -> Resolver {
        let fs = MemoryFileSystem::new();
        for (p, c) in files {
            fs.insert(p, *c);
        }
        Resolver::new(Arc::new(fs))
    }

    #[test]
    fn relative_with_extension_completion() {
        let r = resolver(&[("src/a.js", "//a"), ("src/b.js", "//b")]);
        assert_eq!(
            r.resolve("./b", Path::new("src")).unwrap(),
            PathBuf::from("src/b.js")
        );
        assert_eq!(
            r.resolve("./a.js", Path::new("src")).unwrap(),
            PathBuf::from("src/a.js")
        );
        let r2 = resolver(&[("a.js", "x"), ("src/b.js", "y")]);
        assert_eq!(
            r2.resolve("../a", Path::new("src")).unwrap(),
            PathBuf::from("a.js")
        );
    }

    #[test]
    fn ts_js_extension_import() {
        // TS 约定：`import "./App.js"` 磁盘上是 `./App.tsx`（moduleResolution nodenext）。
        let r = resolver(&[("src/App.tsx", "//app"), ("src/util.ts", "//util")]);
        assert_eq!(
            r.resolve("./App.js", Path::new("src")).unwrap(),
            PathBuf::from("src/App.tsx")
        );
        assert_eq!(
            r.resolve("./util.js", Path::new("src")).unwrap(),
            PathBuf::from("src/util.ts")
        );
        // 真实 .js 存在时不被孪生规则劫持——字面命中优先。
        let r2 = resolver(&[("src/real.js", "//js"), ("src/real.ts", "//ts")]);
        assert_eq!(
            r2.resolve("./real.js", Path::new("src")).unwrap(),
            PathBuf::from("src/real.js")
        );
    }

    #[test]
    fn directory_index() {
        let r = resolver(&[("lib/index.js", "//idx")]);
        assert_eq!(
            r.resolve("./lib", Path::new(".")).unwrap(),
            PathBuf::from("lib/index.js")
        );
    }

    #[test]
    fn node_modules_main_and_module() {
        let r = resolver(&[
            (
                "node_modules/react/package.json",
                r#"{"name":"react","main":"index.js","module":"esm/react.js"}"#,
            ),
            ("node_modules/react/index.js", "//cjs"),
            ("node_modules/react/esm/react.js", "//esm"),
        ]);
        assert_eq!(
            r.resolve("react", Path::new("src")).unwrap(),
            PathBuf::from("node_modules/react/esm/react.js")
        );
    }

    #[test]
    fn node_modules_walk_up() {
        let r = resolver(&[("node_modules/lodash/index.js", "//lodash")]);
        assert_eq!(
            r.resolve("lodash", Path::new("src/features/auth")).unwrap(),
            PathBuf::from("node_modules/lodash/index.js")
        );
    }

    #[test]
    fn node_modules_subpath() {
        let r = resolver(&[("node_modules/react-dom/client.js", "//client")]);
        assert_eq!(
            r.resolve("react-dom/client", Path::new(".")).unwrap(),
            PathBuf::from("node_modules/react-dom/client.js")
        );
    }

    #[test]
    fn scoped_package() {
        let r = resolver(&[("node_modules/@babel/core/lib/index.js", "//babel")]);
        assert_eq!(
            r.resolve("@babel/core/lib", Path::new(".")).unwrap(),
            PathBuf::from("node_modules/@babel/core/lib/index.js")
        );
        assert_eq!(
            split_package("@scope/pkg/a/b"),
            ("@scope/pkg".to_string(), "a/b".to_string())
        );
        assert_eq!(split_package("react"), ("react".to_string(), String::new()));
    }

    #[test]
    fn not_found_errors() {
        let r = resolver(&[("a.js", "x")]);
        assert!(r.resolve("./missing", Path::new(".")).is_err());
        assert!(r.resolve("nonexistent-pkg", Path::new(".")).is_err());
    }

    #[test]
    fn alias_prefix_resolution() {
        let fs = MemoryFileSystem::new();
        fs.insert("proj/src/components/Button.tsx", "//btn");
        fs.insert("proj/src/index.ts", "//idx");
        fs.insert("proj/config.json", "{}");
        let opts = ResolveOptions {
            alias: vec![
                ("@".to_string(), PathBuf::from("proj/src")),
                ("@@".to_string(), PathBuf::from("proj")),
            ],
            ..ResolveOptions::default()
        };
        let r = Resolver::with_options(Arc::new(fs), opts);
        // `@` 精确匹配 → proj/src（补 index）。
        assert_eq!(
            r.resolve("@", Path::new("proj/whatever")).unwrap(),
            PathBuf::from("proj/src/index.ts")
        );
        // `@/components/Button` → proj/src/components/Button.tsx（扩展名补全 + ts-twin 无关）。
        assert_eq!(
            r.resolve("@/components/Button", Path::new("proj/deep/dir"))
                .unwrap(),
            PathBuf::from("proj/src/components/Button.tsx")
        );
        // 最长前缀：`@@/config.json` 命中 `@@` 而非 `@`。
        assert_eq!(
            r.resolve("@@/config.json", Path::new("proj/x")).unwrap(),
            PathBuf::from("proj/config.json")
        );
        // 非别名说明符不受影响。
        assert!(r.resolve("./missing", Path::new("proj/src")).is_err());
    }

    #[test]
    fn cache_returns_same() {
        let r = resolver(&[("src/a.js", "x")]);
        let first = r.resolve("./a", Path::new("src")).unwrap();
        let second = r.resolve("./a", Path::new("src")).unwrap();
        assert_eq!(first, second);
    }
}
