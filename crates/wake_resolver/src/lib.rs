//! # wake_resolver — 模块解析
//!
//! DESIGN §5.1：Node 解析算法的现代子集。v1 覆盖相对路径 + 扩展名补全 + 目录 index +
//! `node_modules` / Yarn PnP + `package.json` 的 `exports`/`module`/`main` 字段 + 结果缓存。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use wake_common::{FileSystem, FxHashMap, fs::normalize};

pub mod pnp;
mod pnpfs;

pub use pnp::{PnpError, PnpManifest};
pub use pnpfs::PnpFileSystem;

/// 一份 npm 包内容的逻辑身份。普通 registry 包按 `name@version` 扁平去重；
/// `context` 仅用于 Yarn PnP/pnpm peer 虚拟实例和本地 workspace，避免把解析上下文不同的
/// 同名同版本包错误合并。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PackageKey {
    pub name: String,
    pub version: String,
    pub context: Option<String>,
}

impl PackageKey {
    pub fn display_name(&self) -> String {
        match &self.context {
            Some(context) => format!("{}@{}#{context}", self.name, self.version),
            None => format!("{}@{}", self.name, self.version),
        }
    }
}

/// 打包器使用的稳定逻辑模块身份。安装器的物理布局只用于读取源码，不参与普通 npm 包的去重。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ModuleIdentity {
    Package {
        package: PackageKey,
        subpath: String,
    },
    File(PathBuf),
}

/// 路径解析与逻辑身份解析的组合结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedModule {
    pub path: PathBuf,
    pub identity: ModuleIdentity,
}

type ResolutionCache = FxHashMap<PathBuf, FxHashMap<String, Option<PathBuf>>>;
type PackageRootCache = FxHashMap<PathBuf, FxHashMap<String, Arc<[PathBuf]>>>;

/// 一条只在 Yarn PnP 依赖边界错误时生效的定向 fallback。
///
/// 当 `issuer_package_prefix` 命中导入方包名、导入的裸包名等于 `dependency`，
/// 且 issuer 自身及 Yarn 顶层 fallback 都因未声明或未满足 peer 而失败时，
/// 改从 `provider_issuer` 所属 locator 的依赖图中解析。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PnpDependencyFallback {
    pub issuer_package_prefix: String,
    pub dependency: String,
    pub provider_issuer: PathBuf,
}

/// 解析选项。
#[derive(Clone, Debug)]
pub struct ResolveOptions {
    /// 扩展名补全顺序。
    pub extensions: Vec<String>,
    /// `package.json` 入口字段优先级（现代优先 `module` 再 `main`）。
    pub main_fields: Vec<String>,
    /// `package.json#exports` 条件优先级。`default` 始终作为最终回退。
    pub conditions: Vec<String>,
    /// 路径别名 `(前缀, 绝对目标)`（如 `@`→`<root>/src`、`@@`→`<root>`、`@@@/{ns}`→扫描产物）。
    /// 匹配规则：说明符 == 前缀 或以 `前缀/` 开头；命中最长前缀，重写后走文件/目录解析。
    /// 保持既定行为 webpack `resolve.alias`（WAKE-COMPATIBILITY §H）。默认空 → 行为与接入前逐字节一致。
    pub alias: Vec<(String, PathBuf)>,
    /// 只在 PnP `Undeclared`/`UnfulfilledPeer` 上应用的 issuer-scoped 依赖 fallback。
    /// 普通 `node_modules` 解析忽略此项。
    pub pnp_dependency_fallbacks: Vec<PnpDependencyFallback>,
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
            conditions: ["browser", "import", "module", "default"]
                .iter()
                .map(|value| value.to_string())
                .collect(),
            alias: Vec::new(),
            pnp_dependency_fallbacks: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct PackageConfig {
    entry: Option<String>,
    exports: Option<serde_json::Value>,
    name: Option<String>,
    version: Option<String>,
    has_peer_dependencies: bool,
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
    /// 两级缓存：`from_dir → specifier → 解析结果`（`None` = 未找到）。
    /// `Mutex` 而非 `RefCell`：使 `Resolver: Sync`，让打包器能经 `Arc<Resolver>` 在工作窃取
    /// 执行器上**并行 resolve**（临界区仅包住 cache 的 get/insert，昂贵的 FS 探测在锁外，竞争极小）。
    cache: Mutex<ResolutionCache>,
    /// package.json 路径 → 入口与 exports。失败也缓存，避免不同 issuer 重复读取清单。
    package_configs: Mutex<FxHashMap<PathBuf, Option<PackageConfig>>>,
    /// 物理文件路径 → 扁平逻辑模块身份。
    module_identities: Mutex<FxHashMap<PathBuf, ModuleIdentity>>,
    /// issuer 目录 → 包名 → 向上搜索到的全部 package root。
    package_roots: Mutex<PackageRootCache>,
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
            package_configs: Mutex::new(FxHashMap::default()),
            module_identities: Mutex::new(FxHashMap::default()),
            package_roots: Mutex::new(FxHashMap::default()),
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
            package_configs: Mutex::new(FxHashMap::default()),
            module_identities: Mutex::new(FxHashMap::default()),
            package_roots: Mutex::new(FxHashMap::default()),
            pnp: Some(manifest),
        }
    }

    /// 从 `from_dir` 解析 `specifier` 到一个规范文件路径。
    pub fn resolve(&self, specifier: &str, from_dir: &Path) -> Result<PathBuf, ResolveError> {
        let cached = self
            .cache
            .lock()
            .unwrap()
            .get(from_dir)
            .and_then(|by_specifier| by_specifier.get(specifier))
            .cloned();
        // 先取 cache（锁瞬间释放：`.cloned()` 拷出 Option 后 guard 即析构）——**关键**是别把锁
        // 持到 `resolve_uncached` 的 FS 探测期间，否则并行退化为串行。
        if let Some(resolved) = cached {
            return resolved.ok_or_else(|| self.err(specifier, from_dir));
        }
        // 未命中：昂贵的 FS 探测在锁外进行（并行的收益全在这里）。两个线程同 key 竞争时都会算一遍
        // 再各自 insert——幂等无害，换取零锁争用。
        let resolved = self.resolve_uncached(specifier, from_dir);
        self.cache
            .lock()
            .unwrap()
            .entry(from_dir.to_path_buf())
            .or_default()
            .insert(specifier.to_owned(), resolved.clone());
        resolved.ok_or_else(|| self.err(specifier, from_dir))
    }

    /// 同时返回物理路径和按 npm 包名、版本、子路径归一后的逻辑身份。
    pub fn resolve_module(
        &self,
        specifier: &str,
        from_dir: &Path,
    ) -> Result<ResolvedModule, ResolveError> {
        let path = self.resolve(specifier, from_dir)?;
        let identity = self.module_identity(&path);
        Ok(ResolvedModule { path, identity })
    }

    /// 将一个已解析文件归一为逻辑模块身份。
    pub fn module_identity(&self, path: &Path) -> ModuleIdentity {
        let path = normalize(path);
        if let Some(identity) = self.module_identities.lock().unwrap().get(&path).cloned() {
            return identity;
        }
        let identity = self.module_identity_uncached(&path);
        self.module_identities
            .lock()
            .unwrap()
            .insert(path, identity.clone());
        identity
    }

    fn module_identity_uncached(&self, path: &Path) -> ModuleIdentity {
        let Some(root) = Self::find_package_root(self.fs.as_ref(), path) else {
            return ModuleIdentity::File(path.to_path_buf());
        };
        let Some(config) = self.read_package_config(&root.join("package.json")) else {
            return ModuleIdentity::File(path.to_path_buf());
        };
        let (Some(name), Some(version)) = (config.name, config.version) else {
            return ModuleIdentity::File(path.to_path_buf());
        };
        let subpath = path
            .strip_prefix(&root)
            .ok()
            .map(Self::path_to_logical_string)
            .unwrap_or_else(|| Self::path_to_logical_string(path));
        let context = Self::package_context(&root, config.has_peer_dependencies);
        ModuleIdentity::Package {
            package: PackageKey {
                name,
                version,
                context,
            },
            subpath,
        }
    }

    /// 文件系统 generation 变化后清空路径解析结果。
    ///
    /// Resolver 会缓存成功与失败；watch 中新增、删除或重命名文件后，旧结果都可能失效。
    pub fn clear_cache(&self) {
        self.cache.lock().unwrap().clear();
        self.package_configs.lock().unwrap().clear();
        self.module_identities.lock().unwrap().clear();
        self.package_roots.lock().unwrap().clear();
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
            // PnP 先定位包根，再由统一包入口逻辑处理 exports 与子路径。
            let (package, subpath) = split_package_ref(specifier);
            let package_root = match pnp.resolve_bare(package, from_dir) {
                Ok(root) => root,
                Err(pnp::PnpError::Undeclared | pnp::PnpError::UnfulfilledPeer) => {
                    self.resolve_pnp_dependency_fallback(pnp, package, from_dir)?
                }
                Err(_) => return None,
            };
            self.resolve_package(&package_root, subpath)
        } else {
            self.resolve_node_modules(specifier, from_dir)
        }
    }

    /// 严格限定的 PnP 依赖 fallback。调用方只会在 issuer 正常解析与
    /// Yarn 顶层 fallback 都返回依赖边界错误后进入此逻辑。
    fn resolve_pnp_dependency_fallback(
        &self,
        pnp: &PnpManifest,
        dependency: &str,
        issuer_dir: &Path,
    ) -> Option<PathBuf> {
        let issuer_package = pnp.issuer_package_name(issuer_dir).ok()??;
        self.options
            .pnp_dependency_fallbacks
            .iter()
            .find(|fallback| {
                fallback.dependency == dependency
                    && issuer_package.starts_with(&fallback.issuer_package_prefix)
            })
            .and_then(|fallback| pnp.resolve_bare(dependency, &fallback.provider_issuer).ok())
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

    fn resolve_package(&self, package_root: &Path, subpath: &str) -> Option<PathBuf> {
        if !self.fs.is_dir(package_root) {
            return None;
        }
        let package_json = package_root.join("package.json");
        if self.fs.is_file(&package_json)
            && let Some(config) = self.read_package_config(&package_json)
        {
            if let Some(exports) = config.exports {
                let key = if subpath.is_empty() {
                    ".".to_string()
                } else {
                    format!("./{subpath}")
                };
                let target = resolve_exports_target(&exports, &key, &self.options.conditions)?;
                let relative = target.strip_prefix("./")?;
                return self.resolve_as_file_or_dir(&normalize(&package_root.join(relative)));
            }
            if subpath.is_empty()
                && let Some(entry) = config.entry
            {
                return self.resolve_as_file_or_dir(&normalize(&package_root.join(entry)));
            }
        }
        let target = if subpath.is_empty() {
            package_root.join("index")
        } else {
            package_root.join(subpath)
        };
        self.resolve_as_file_or_dir(&target)
    }

    fn resolve_node_modules(&self, specifier: &str, from_dir: &Path) -> Option<PathBuf> {
        let (pkg_name, subpath) = split_package_ref(specifier);
        for pkg_dir in self.package_roots(pkg_name, from_dir).iter() {
            if let Some(resolved) = self.resolve_package(pkg_dir, subpath) {
                return Some(resolved);
            }
        }
        None
    }

    fn find_package_root(fs: &dyn FileSystem, path: &Path) -> Option<PathBuf> {
        let start = path.parent()?;
        for dir in start.ancestors() {
            let parent = dir.parent();
            let direct = parent
                .and_then(Path::file_name)
                .is_some_and(|name| name == "node_modules");
            let scoped = parent
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with('@'))
                && parent
                    .and_then(Path::parent)
                    .and_then(Path::file_name)
                    .is_some_and(|name| name == "node_modules");
            if direct || scoped {
                return Some(normalize(dir));
            }
        }
        start
            .ancestors()
            .find(|dir| fs.is_file(&dir.join("package.json")))
            .map(normalize)
    }

    fn path_to_logical_string(path: &Path) -> String {
        path.to_string_lossy().replace('\\', "/")
    }

    fn package_context(root: &Path, has_peer_dependencies: bool) -> Option<String> {
        let logical = Self::path_to_logical_string(root);
        if logical.contains("/__virtual__/") || logical.contains("/$$virtual/") {
            return Some(format!("pnp:{logical}"));
        }
        let components = root
            .components()
            .filter_map(|component| component.as_os_str().to_str())
            .collect::<Vec<_>>();
        if let Some(index) = components
            .iter()
            .position(|component| *component == ".pnpm")
            && let Some(locator) = components.get(index + 1)
            && (locator.contains('_') || locator.contains('('))
        {
            return Some(format!("pnpm:{locator}"));
        }
        let installed = components.iter().any(|component| {
            matches!(
                *component,
                "node_modules" | ".pnp-store" | ".yarn" | ".pnpm"
            )
        });
        if has_peer_dependencies || !installed {
            Some(format!("root:{logical}"))
        } else {
            None
        }
    }

    fn package_roots(&self, pkg_name: &str, from_dir: &Path) -> Arc<[PathBuf]> {
        if let Some(roots) = self
            .package_roots
            .lock()
            .unwrap()
            .get(from_dir)
            .and_then(|by_package| by_package.get(pkg_name))
            .cloned()
        {
            return roots;
        }
        let mut roots = Vec::new();
        let mut dir = Some(normalize(from_dir));
        while let Some(d) = dir {
            let pkg_dir = d.join("node_modules").join(pkg_name);
            if self.fs.is_dir(&pkg_dir) {
                roots.push(pkg_dir);
            }
            dir = d.parent().map(Path::to_path_buf);
        }
        let roots: Arc<[PathBuf]> = roots.into();
        self.package_roots
            .lock()
            .unwrap()
            .entry(from_dir.to_path_buf())
            .or_default()
            .insert(pkg_name.to_owned(), Arc::clone(&roots));
        roots
    }

    /// 读 package.json 的入口字段（按 main_fields 优先级）。
    fn read_pkg_entry(&self, pkg: &Path) -> Option<String> {
        self.read_package_config(pkg)?.entry
    }

    fn read_package_config(&self, pkg: &Path) -> Option<PackageConfig> {
        if let Some(config) = self.package_configs.lock().unwrap().get(pkg) {
            return config.clone();
        }
        let config = self.fs.read_to_string(pkg).ok().and_then(|text| {
            let json: serde_json::Value = serde_json::from_str(&text).ok()?;
            let entry = self.options.main_fields.iter().find_map(|field| {
                json.get(field)
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
            });
            Some(PackageConfig {
                entry,
                exports: json.get("exports").cloned(),
                name: json
                    .get("name")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                version: json
                    .get("version")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned),
                has_peer_dependencies: json
                    .get("peerDependencies")
                    .and_then(|value| value.as_object())
                    .is_some_and(|peers| !peers.is_empty()),
            })
        });
        self.package_configs
            .lock()
            .unwrap()
            .insert(pkg.to_path_buf(), config.clone());
        config
    }
}

fn resolve_exports_target(
    exports: &serde_json::Value,
    key: &str,
    conditions: &[String],
) -> Option<String> {
    let Some(map) = exports.as_object() else {
        return (key == ".").then(|| resolve_conditional_target(exports, conditions))?;
    };
    if !map.keys().any(|candidate| candidate.starts_with('.')) {
        return (key == ".").then(|| resolve_conditional_target(exports, conditions))?;
    }
    if let Some(value) = map.get(key) {
        return resolve_conditional_target(value, conditions);
    }

    let mut patterns = map
        .iter()
        .filter_map(|(pattern, value)| {
            let (prefix, suffix) = pattern.split_once('*')?;
            let capture = key.strip_prefix(prefix)?.strip_suffix(suffix)?;
            Some((prefix.len() + suffix.len(), capture, value))
        })
        .collect::<Vec<_>>();
    patterns.sort_by_key(|item| std::cmp::Reverse(item.0));
    let (_, capture, value) = patterns.into_iter().next()?;
    resolve_conditional_target(value, conditions).map(|target| target.replace('*', capture))
}

fn resolve_conditional_target(value: &serde_json::Value, conditions: &[String]) -> Option<String> {
    match value {
        serde_json::Value::String(target) => Some(target.clone()),
        serde_json::Value::Array(targets) => targets
            .iter()
            .find_map(|target| resolve_conditional_target(target, conditions)),
        serde_json::Value::Object(targets) => {
            for condition in conditions {
                if let Some(target) = targets.get(condition)
                    && let Some(resolved) = resolve_conditional_target(target, conditions)
                {
                    return Some(resolved);
                }
            }
            if !conditions.iter().any(|condition| condition == "default") {
                targets
                    .get("default")
                    .and_then(|target| resolve_conditional_target(target, conditions))
            } else {
                None
            }
        }
        _ => None,
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

fn split_package_ref(specifier: &str) -> (&str, &str) {
    if specifier.starts_with('@') {
        let package_end = specifier
            .char_indices()
            .filter_map(|(index, ch)| (ch == '/').then_some(index))
            .nth(1)
            .unwrap_or(specifier.len());
        (
            &specifier[..package_end],
            specifier[package_end..].trim_start_matches('/'),
        )
    } else {
        specifier
            .split_once('/')
            .map_or((specifier, ""), |(name, subpath)| (name, subpath))
    }
}

/// 拆分裸说明符为 (包名, 子路径)。处理 scoped 包 `@scope/name/sub`。
pub(crate) fn split_package(specifier: &str) -> (String, String) {
    let (package, subpath) = split_package_ref(specifier);
    (package.to_owned(), subpath.to_owned())
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
    fn module_identity_flattens_same_name_and_version_across_install_paths() {
        let r = resolver(&[
            (
                "node_modules/a/node_modules/shared/package.json",
                r#"{"name":"shared","version":"1.2.0"}"#,
            ),
            ("node_modules/a/node_modules/shared/index.js", "// a"),
            (
                "node_modules/b/node_modules/shared/package.json",
                r#"{"name":"shared","version":"1.2.0"}"#,
            ),
            ("node_modules/b/node_modules/shared/index.js", "// b"),
        ]);
        let a = r.module_identity(Path::new("node_modules/a/node_modules/shared/index.js"));
        let b = r.module_identity(Path::new("node_modules/b/node_modules/shared/index.js"));
        assert_eq!(a, b);
    }

    #[test]
    fn module_identity_keeps_versions_and_peer_contexts_separate() {
        let r = resolver(&[
            (
                "node_modules/a/node_modules/shared/package.json",
                r#"{"name":"shared","version":"1.2.0"}"#,
            ),
            ("node_modules/a/node_modules/shared/index.js", "// v1"),
            (
                "node_modules/b/node_modules/shared/package.json",
                r#"{"name":"shared","version":"2.0.0"}"#,
            ),
            ("node_modules/b/node_modules/shared/index.js", "// v2"),
            (
                "node_modules/x/node_modules/widget/package.json",
                r#"{"name":"widget","version":"1.0.0","peerDependencies":{"react":"*"}}"#,
            ),
            ("node_modules/x/node_modules/widget/index.js", "// react 18"),
            (
                "node_modules/y/node_modules/widget/package.json",
                r#"{"name":"widget","version":"1.0.0","peerDependencies":{"react":"*"}}"#,
            ),
            ("node_modules/y/node_modules/widget/index.js", "// react 19"),
        ]);
        assert_ne!(
            r.module_identity(Path::new("node_modules/a/node_modules/shared/index.js")),
            r.module_identity(Path::new("node_modules/b/node_modules/shared/index.js"))
        );
        assert_ne!(
            r.module_identity(Path::new("node_modules/x/node_modules/widget/index.js")),
            r.module_identity(Path::new("node_modules/y/node_modules/widget/index.js"))
        );
    }

    #[test]
    fn module_identity_keeps_yarn_pnp_virtual_instances_separate() {
        let r = resolver(&[
            (
                ".yarn/__virtual__/widget-react18/0/cache/widget/node_modules/widget/package.json",
                r#"{"name":"widget","version":"1.0.0"}"#,
            ),
            (
                ".yarn/__virtual__/widget-react18/0/cache/widget/node_modules/widget/index.js",
                "// react 18",
            ),
            (
                ".yarn/__virtual__/widget-react19/0/cache/widget/node_modules/widget/package.json",
                r#"{"name":"widget","version":"1.0.0"}"#,
            ),
            (
                ".yarn/__virtual__/widget-react19/0/cache/widget/node_modules/widget/index.js",
                "// react 19",
            ),
        ]);
        assert_ne!(
            r.module_identity(Path::new(
                ".yarn/__virtual__/widget-react18/0/cache/widget/node_modules/widget/index.js"
            )),
            r.module_identity(Path::new(
                ".yarn/__virtual__/widget-react19/0/cache/widget/node_modules/widget/index.js"
            ))
        );
    }

    #[test]
    fn package_exports_select_import_condition_and_subpaths() {
        let r = resolver(&[
            (
                "node_modules/modern/package.json",
                r#"{
                    "exports": {
                        ".": {
                            "types": "./types/index.d.ts",
                            "import": "./esm/index.mjs",
                            "default": "./cjs/index.cjs"
                        },
                        "./feature": "./esm/feature.js",
                        "./icons/*": "./esm/icons/*.js"
                    }
                }"#,
            ),
            ("node_modules/modern/esm/index.mjs", "// esm"),
            ("node_modules/modern/cjs/index.cjs", "// cjs"),
            ("node_modules/modern/esm/feature.js", "// feature"),
            ("node_modules/modern/esm/icons/add.js", "// icon"),
        ]);
        assert_eq!(
            r.resolve("modern", Path::new("src")).unwrap(),
            PathBuf::from("node_modules/modern/esm/index.mjs")
        );
        assert_eq!(
            r.resolve("modern/feature", Path::new("src")).unwrap(),
            PathBuf::from("node_modules/modern/esm/feature.js")
        );
        assert_eq!(
            r.resolve("modern/icons/add", Path::new("src")).unwrap(),
            PathBuf::from("node_modules/modern/esm/icons/add.js")
        );
    }

    #[test]
    fn package_exports_honor_development_condition() {
        let fs = MemoryFileSystem::new();
        fs.insert(
            "node_modules/router/package.json",
            r#"{"exports":{".":{"development":"./dev.js","default":"./prod.js"}}}"#,
        );
        fs.insert("node_modules/router/dev.js", "// dev");
        fs.insert("node_modules/router/prod.js", "// prod");
        let options = ResolveOptions {
            conditions: vec!["development".to_string(), "import".to_string()],
            ..ResolveOptions::default()
        };
        let r = Resolver::with_options(Arc::new(fs), options);
        assert_eq!(
            r.resolve("router", Path::new("src")).unwrap(),
            PathBuf::from("node_modules/router/dev.js")
        );
    }

    #[test]
    fn pnp_packages_use_exports_from_the_package_root() {
        let fs = MemoryFileSystem::new();
        fs.insert(
            "project/.pnp.data.json",
            r#"{
                "enableTopLevelFallback": true,
                "fallbackExclusionList": [],
                "fallbackPool": [],
                "packageRegistryData": [
                    [null, [[null, {
                        "packageLocation": "./",
                        "packageDependencies": [["modern", "npm:1.0.0"]],
                        "linkType": "SOFT"
                    }]]],
                    ["modern", [["npm:1.0.0", {
                        "packageLocation": "../cache/modern/node_modules/modern/",
                        "packageDependencies": [["modern", "npm:1.0.0"]],
                        "linkType": "HARD"
                    }]]]
                ]
            }"#,
        );
        fs.insert("project/src/entry.js", "import 'modern'");
        fs.insert(
            "cache/modern/node_modules/modern/package.json",
            r#"{"exports":{".":{"import":"./esm/index.mjs"}}}"#,
        );
        fs.insert("cache/modern/node_modules/modern/esm/index.mjs", "// esm");
        let manifest = PnpManifest::load(&fs, Path::new("project")).unwrap();
        let resolver = Resolver::with_pnp(Arc::new(fs), Arc::new(manifest));
        assert_eq!(
            resolver
                .resolve("modern", Path::new("project/src"))
                .unwrap(),
            PathBuf::from("cache/modern/node_modules/modern/esm/index.mjs")
        );
    }

    fn pnp_scoped_fallback_resolver(
        component_dependency: Option<Option<&str>>,
        provider_has_dependency: bool,
        enable_top_level_fallback: bool,
        top_level_dependency: Option<&str>,
        alias: Vec<(String, PathBuf)>,
    ) -> Resolver {
        let mut top_dependencies = vec![
            serde_json::json!(["@crab-dev/wake", "npm:0.1.16"]),
            serde_json::json!(["@crab-dev/rc-button", "npm:1.0.0"]),
        ];
        if let Some(reference) = top_level_dependency {
            top_dependencies.push(serde_json::json!(["@linaria/core", reference]));
        }

        let mut wake_dependencies = vec![serde_json::json!(["@crab-dev/wake", "npm:0.1.16"])];
        if provider_has_dependency {
            wake_dependencies.push(serde_json::json!(["@linaria/core", "npm:1.0.0"]));
        }

        let mut component_dependencies =
            vec![serde_json::json!(["@crab-dev/rc-button", "npm:1.0.0"])];
        match component_dependency {
            Some(Some(reference)) => {
                component_dependencies.push(serde_json::json!(["@linaria/core", reference]))
            }
            Some(None) => component_dependencies.push(serde_json::json!(["@linaria/core", null])),
            None => {}
        }

        let manifest_value = serde_json::json!({
            "enableTopLevelFallback": enable_top_level_fallback,
            "fallbackExclusionList": [],
            "fallbackPool": [],
            "packageRegistryData": [
                [null, [[null, {
                    "packageLocation": "./",
                    "packageDependencies": top_dependencies,
                    "linkType": "SOFT"
                }]]],
                ["@crab-dev/wake", [["npm:0.1.16", {
                    "packageLocation": "../cache/wake/node_modules/@crab-dev/wake/",
                    "packageDependencies": wake_dependencies,
                    "linkType": "HARD"
                }]]],
                ["@crab-dev/rc-button", [["npm:1.0.0", {
                    "packageLocation": "../cache/button/node_modules/@crab-dev/rc-button/",
                    "packageDependencies": component_dependencies,
                    "linkType": "HARD"
                }]]],
                ["@linaria/core", [
                    ["npm:1.0.0", {
                        "packageLocation": "../cache/linaria-v1/node_modules/@linaria/core/",
                        "packageDependencies": [["@linaria/core", "npm:1.0.0"]],
                        "linkType": "HARD"
                    }],
                    ["npm:2.0.0", {
                        "packageLocation": "../cache/linaria-v2/node_modules/@linaria/core/",
                        "packageDependencies": [["@linaria/core", "npm:2.0.0"]],
                        "linkType": "HARD"
                    }]
                ]]
            ]
        });

        let fs = MemoryFileSystem::new();
        fs.insert(
            "project/.pnp.data.json",
            manifest_value.to_string().as_str(),
        );
        fs.insert("project/src/index.js", "// project");
        fs.insert(
            "cache/wake/node_modules/@crab-dev/wake/package.json",
            r#"{"name":"@crab-dev/wake","main":"index.js"}"#,
        );
        fs.insert("cache/wake/node_modules/@crab-dev/wake/index.js", "// wake");
        fs.insert(
            "cache/button/node_modules/@crab-dev/rc-button/package.json",
            r#"{"name":"@crab-dev/rc-button","main":"index.js"}"#,
        );
        fs.insert(
            "cache/button/node_modules/@crab-dev/rc-button/index.js",
            "// button",
        );
        fs.insert(
            "cache/linaria-v1/node_modules/@linaria/core/package.json",
            r#"{"name":"@linaria/core","exports":{".":"./index.js","./private":"./private.js"}}"#,
        );
        fs.insert(
            "cache/linaria-v1/node_modules/@linaria/core/index.js",
            "// provider v1",
        );
        fs.insert(
            "cache/linaria-v1/node_modules/@linaria/core/private.js",
            "// provider private",
        );
        fs.insert(
            "cache/linaria-v2/node_modules/@linaria/core/package.json",
            r#"{"name":"@linaria/core","exports":{".":"./index.js"}}"#,
        );
        fs.insert(
            "cache/linaria-v2/node_modules/@linaria/core/index.js",
            "// component v2",
        );
        fs.insert("alias/linaria.js", "// alias");

        let manifest = PnpManifest::load(&fs, Path::new("project")).unwrap();
        Resolver::with_pnp_options(
            Arc::new(fs),
            Arc::new(manifest),
            ResolveOptions {
                alias,
                pnp_dependency_fallbacks: vec![PnpDependencyFallback {
                    issuer_package_prefix: "@crab-dev/rc-".to_string(),
                    dependency: "@linaria/core".to_string(),
                    provider_issuer: PathBuf::from(
                        "cache/wake/node_modules/@crab-dev/wake/internal",
                    ),
                }],
                ..ResolveOptions::default()
            },
        )
    }

    #[test]
    fn pnp_scoped_fallback_resolves_undeclared_dependency_from_provider() {
        let resolver = pnp_scoped_fallback_resolver(None, true, false, None, Vec::new());
        assert_eq!(
            resolver
                .resolve(
                    "@linaria/core",
                    Path::new("cache/button/node_modules/@crab-dev/rc-button/esm")
                )
                .unwrap(),
            PathBuf::from("cache/linaria-v1/node_modules/@linaria/core/index.js")
        );
    }

    #[test]
    fn pnp_scoped_fallback_resolves_unfulfilled_peer_from_provider() {
        let resolver = pnp_scoped_fallback_resolver(Some(None), true, false, None, Vec::new());
        assert_eq!(
            resolver
                .resolve(
                    "@linaria/core",
                    Path::new("cache/button/node_modules/@crab-dev/rc-button/esm")
                )
                .unwrap(),
            PathBuf::from("cache/linaria-v1/node_modules/@linaria/core/index.js")
        );
    }

    #[test]
    fn pnp_issuer_dependency_wins_over_scoped_fallback() {
        let resolver =
            pnp_scoped_fallback_resolver(Some(Some("npm:2.0.0")), true, false, None, Vec::new());
        assert_eq!(
            resolver
                .resolve(
                    "@linaria/core",
                    Path::new("cache/button/node_modules/@crab-dev/rc-button/esm")
                )
                .unwrap(),
            PathBuf::from("cache/linaria-v2/node_modules/@linaria/core/index.js")
        );
    }

    #[test]
    fn pnp_top_level_fallback_wins_over_scoped_fallback() {
        let resolver =
            pnp_scoped_fallback_resolver(None, true, true, Some("npm:2.0.0"), Vec::new());
        assert_eq!(
            resolver
                .resolve(
                    "@linaria/core",
                    Path::new("cache/button/node_modules/@crab-dev/rc-button/esm")
                )
                .unwrap(),
            PathBuf::from("cache/linaria-v2/node_modules/@linaria/core/index.js")
        );
    }

    #[test]
    fn pnp_scoped_fallback_does_not_apply_to_project_sources() {
        let resolver = pnp_scoped_fallback_resolver(None, true, false, None, Vec::new());
        assert!(
            resolver
                .resolve("@linaria/core", Path::new("project/src"))
                .is_err()
        );
    }

    #[test]
    fn pnp_scoped_fallback_fails_when_provider_does_not_declare_dependency() {
        let resolver = pnp_scoped_fallback_resolver(None, false, false, None, Vec::new());
        assert!(
            resolver
                .resolve(
                    "@linaria/core",
                    Path::new("cache/button/node_modules/@crab-dev/rc-button/esm")
                )
                .is_err()
        );
    }

    #[test]
    fn pnp_alias_wins_over_normal_and_scoped_dependency_resolution() {
        let resolver = pnp_scoped_fallback_resolver(
            Some(Some("npm:2.0.0")),
            true,
            false,
            None,
            vec![(
                "@linaria/core".to_string(),
                PathBuf::from("alias/linaria.js"),
            )],
        );
        assert_eq!(
            resolver
                .resolve(
                    "@linaria/core",
                    Path::new("cache/button/node_modules/@crab-dev/rc-button/esm")
                )
                .unwrap(),
            PathBuf::from("alias/linaria.js")
        );
    }

    #[test]
    fn pnp_invalid_export_does_not_retry_scoped_fallback() {
        let resolver =
            pnp_scoped_fallback_resolver(Some(Some("npm:2.0.0")), true, false, None, Vec::new());
        assert!(
            resolver
                .resolve(
                    "@linaria/core/private",
                    Path::new("cache/button/node_modules/@crab-dev/rc-button/esm")
                )
                .is_err()
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
