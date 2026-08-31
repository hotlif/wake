//! # wake_resolver — 模块解析
//!
//! DESIGN §5.1：Node 解析算法的现代子集。v1 覆盖相对路径 + 扩展名补全 + 目录 index +
//! `node_modules` / Yarn PnP + `package.json` 的 `exports`/`module`/`main` 字段 + 结果缓存。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use wake_common::{FileSystem, FxHashMap, fs::normalize};

mod environment;
pub mod pnp;
mod pnpfs;

pub use environment::ResolutionEnvironment;
use environment::{PnpRegistry, PnpRoute};
pub use pnp::{PnpError, PnpLoadError, PnpManifest};
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ResolutionKey {
    specifier: String,
    conditions: Vec<String>,
    main_fields: Vec<String>,
}

/// 一次解析的完整包入口语义。条件是活动集合；条件对象自身的声明顺序决定优先级。
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ResolutionProfile {
    pub conditions: Vec<String>,
    pub main_fields: Vec<String>,
}

type ResolutionCache =
    FxHashMap<PathBuf, FxHashMap<ResolutionKey, Result<PathBuf, ResolveErrorKind>>>;
type PackageRootCache = FxHashMap<PathBuf, FxHashMap<String, Arc<[PathBuf]>>>;

/// 解析选项。
#[derive(Clone, Debug)]
pub struct ResolveOptions {
    /// 扩展名补全顺序。
    pub extensions: Vec<String>,
    /// `package.json` 入口字段优先级（现代优先 `module` 再 `main`）。
    pub main_fields: Vec<String>,
    /// `package.json#exports` 活动条件。条件对象中的声明顺序决定优先级。
    pub conditions: Vec<String>,
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
            conditions: ["browser", "import", "module", "default"]
                .iter()
                .map(|value| value.to_string())
                .collect(),
            alias: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
struct PackageConfig {
    string_fields: FxHashMap<String, String>,
    exports: Option<OrderedJsonValue>,
    name: Option<String>,
    version: Option<String>,
    has_peer_dependencies: bool,
}

impl PackageConfig {
    fn entry(&self, main_fields: &[String]) -> Option<String> {
        main_fields
            .iter()
            .find_map(|field| self.string_fields.get(field).cloned())
    }
}

#[derive(Clone, Debug)]
enum OrderedJsonValue {
    Null,
    Bool,
    Number,
    String(String),
    Array(Vec<OrderedJsonValue>),
    Object(Vec<(String, OrderedJsonValue)>),
}

impl OrderedJsonValue {
    fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(entries) => entries
                .iter()
                .find_map(|(candidate, value)| (candidate == key).then_some(value)),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    fn as_object(&self) -> Option<&[(String, OrderedJsonValue)]> {
        match self {
            Self::Object(entries) => Some(entries),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for OrderedJsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct OrderedValueVisitor;

        impl<'de> Visitor<'de> for OrderedValueVisitor {
            type Value = OrderedJsonValue;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a JSON value")
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(OrderedJsonValue::Null)
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(OrderedJsonValue::Null)
            }

            fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
                Ok(OrderedJsonValue::Bool)
            }

            fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
                Ok(OrderedJsonValue::Number)
            }

            fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
                Ok(OrderedJsonValue::Number)
            }

            fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
                Ok(OrderedJsonValue::Number)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(OrderedJsonValue::String(value.to_owned()))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
                Ok(OrderedJsonValue::String(value))
            }

            fn visit_seq<A>(self, mut values: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut items = Vec::new();
                while let Some(value) = values.next_element()? {
                    items.push(value);
                }
                Ok(OrderedJsonValue::Array(items))
            }

            fn visit_map<A>(self, mut values: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut entries = Vec::new();
                while let Some((key, value)) = values.next_entry()? {
                    entries.push((key, value));
                }
                Ok(OrderedJsonValue::Object(entries))
            }
        }

        deserializer.deserialize_any(OrderedValueVisitor)
    }
}

/// 解析失败。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolveErrorKind {
    NotFound,
    PnpManifest(PnpLoadError),
    PnpDependency(PnpError),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolveError {
    pub specifier: String,
    pub from: PathBuf,
    kind: Box<ResolveErrorKind>,
    witnesses: Vec<PathBuf>,
}

impl ResolveError {
    pub fn kind(&self) -> &ResolveErrorKind {
        self.kind.as_ref()
    }

    /// Logical filesystem locations whose mutation may make this exact resolution succeed.
    ///
    /// These are resolver-owned candidates, not diagnostics guessed by a caller. A PnP-aware
    /// product must project them through [`PnpFileSystem::watch_path`] before registering a
    /// physical watcher.
    pub fn witnesses(&self) -> &[PathBuf] {
        &self.witnesses
    }
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.kind.as_ref() {
            ResolveErrorKind::NotFound => write!(
                f,
                "无法从 `{}` 解析模块 `{}`",
                self.from.display(),
                self.specifier
            ),
            ResolveErrorKind::PnpManifest(error) => write!(f, "{error}"),
            ResolveErrorKind::PnpDependency(error) => write!(
                f,
                "Yarn PnP 拒绝从 `{}` 解析 `{}`：{error}",
                self.from.display(),
                self.specifier
            ),
        }
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
    /// 文件所在目录 → 最近的 package root；`None` 表示祖先链上没有。
    ///
    /// 模块身份是按文件路径缓存的，冷构建中每个文件仍是唯一 miss。这个目录级
    /// 路径压缩缓存使同目录与相邻目录的文件共用一次向上 `package.json` 探测。
    package_owners: Mutex<FxHashMap<PathBuf, Option<PathBuf>>>,
    /// issuer 目录 → 包名 → 向上搜索到的全部 package root。
    package_roots: Mutex<PackageRootCache>,
    /// Yarn PnP 清单。`Some` 时裸说明符走 PnP 依赖图（不走 `node_modules` 上溯）。
    pnp: Option<Arc<PnpManifest>>,
    /// `ResolutionEnvironment` 所有的按 issuer registry；支持嵌套 PnP 根与重新加载。
    pnp_registry: Option<Arc<PnpRegistry>>,
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
            package_owners: Mutex::new(FxHashMap::default()),
            package_roots: Mutex::new(FxHashMap::default()),
            pnp: None,
            pnp_registry: None,
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
            package_owners: Mutex::new(FxHashMap::default()),
            package_roots: Mutex::new(FxHashMap::default()),
            pnp: Some(manifest),
            pnp_registry: None,
        }
    }

    pub(crate) fn with_registry(
        fs: Arc<dyn FileSystem>,
        registry: Arc<PnpRegistry>,
        options: ResolveOptions,
    ) -> Resolver {
        Resolver {
            fs,
            options,
            cache: Mutex::new(FxHashMap::default()),
            package_configs: Mutex::new(FxHashMap::default()),
            module_identities: Mutex::new(FxHashMap::default()),
            package_owners: Mutex::new(FxHashMap::default()),
            package_roots: Mutex::new(FxHashMap::default()),
            pnp: None,
            pnp_registry: Some(registry),
        }
    }

    /// 从 `from_dir` 解析 `specifier` 到一个规范文件路径。
    pub fn resolve(&self, specifier: &str, from_dir: &Path) -> Result<PathBuf, ResolveError> {
        self.resolve_with_profile(
            specifier,
            from_dir,
            &ResolutionProfile {
                conditions: self.options.conditions.clone(),
                main_fields: self.options.main_fields.clone(),
            },
        )
    }

    /// 使用调用方提供的条件集合解析模块。条件属于解析身份的一部分，Node/Browser 与
    /// import/require 不得共享成功或失败缓存。
    pub fn resolve_with_conditions(
        &self,
        specifier: &str,
        from_dir: &Path,
        conditions: &[String],
    ) -> Result<PathBuf, ResolveError> {
        self.resolve_with_profile(
            specifier,
            from_dir,
            &ResolutionProfile {
                conditions: conditions.to_vec(),
                main_fields: self.options.main_fields.clone(),
            },
        )
    }

    /// 使用完整的包解析 profile。profile 的全部字段进入成功与失败缓存身份。
    pub fn resolve_with_profile(
        &self,
        specifier: &str,
        from_dir: &Path,
        profile: &ResolutionProfile,
    ) -> Result<PathBuf, ResolveError> {
        let key = ResolutionKey {
            specifier: specifier.to_string(),
            conditions: profile.conditions.clone(),
            main_fields: profile.main_fields.clone(),
        };
        let cached = self
            .cache
            .lock()
            .unwrap()
            .get(from_dir)
            .and_then(|by_specifier| by_specifier.get(&key))
            .cloned();
        // 先取 cache（锁瞬间释放：`.cloned()` 拷出 Option 后 guard 即析构）——**关键**是别把锁
        // 持到 `resolve_uncached` 的 FS 探测期间，否则并行退化为串行。
        if let Some(resolved) = cached {
            return resolved.map_err(|kind| self.err(specifier, from_dir, kind));
        }
        // 未命中：昂贵的 FS 探测在锁外进行（并行的收益全在这里）。两个线程同 key 竞争时都会算一遍
        // 再各自 insert——幂等无害，换取零锁争用。
        let resolved = self.resolve_uncached(specifier, from_dir, profile);
        self.cache
            .lock()
            .unwrap()
            .entry(from_dir.to_path_buf())
            .or_default()
            .insert(key, resolved.clone());
        resolved.map_err(|kind| self.err(specifier, from_dir, kind))
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

    /// 条件感知的逻辑模块解析。
    pub fn resolve_module_with_conditions(
        &self,
        specifier: &str,
        from_dir: &Path,
        conditions: &[String],
    ) -> Result<ResolvedModule, ResolveError> {
        let path = self.resolve_with_conditions(specifier, from_dir, conditions)?;
        let identity = self.module_identity(&path);
        Ok(ResolvedModule { path, identity })
    }

    /// 完整 profile 感知的逻辑模块解析。
    pub fn resolve_module_with_profile(
        &self,
        specifier: &str,
        from_dir: &Path,
        profile: &ResolutionProfile,
    ) -> Result<ResolvedModule, ResolveError> {
        let path = self.resolve_with_profile(specifier, from_dir, profile)?;
        let identity = self.module_identity(&path);
        Ok(ResolvedModule { path, identity })
    }

    /// 解析一个包名到未限定包根，供 token/docgen 等读取包内非入口元数据。
    pub fn resolve_package_root(
        &self,
        package: &str,
        issuer_dir: &Path,
    ) -> Result<PathBuf, ResolveError> {
        let kind = if !is_valid_bare_package_specifier(package)
            || !split_package_ref(package).1.is_empty()
        {
            Err(ResolveErrorKind::NotFound)
        } else {
            match self
                .pnp_route(issuer_dir)
                .map_err(ResolveErrorKind::PnpManifest)
            {
                Ok(PnpRoute::Managed(manifest)) => manifest
                    .resolve_bare(package, issuer_dir)
                    .map_err(ResolveErrorKind::PnpDependency),
                Ok(PnpRoute::Classic | PnpRoute::NoManifest) => self
                    .package_roots(package, issuer_dir)
                    .first()
                    .cloned()
                    .ok_or(ResolveErrorKind::NotFound),
                Err(error) => Err(error),
            }
        };
        kind.map_err(|kind| self.err(package, issuer_dir, kind))
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
        let Some(root) = self.find_package_root(path) else {
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
        self.package_owners.lock().unwrap().clear();
        self.package_roots.lock().unwrap().clear();
    }

    fn err(&self, specifier: &str, from_dir: &Path, kind: ResolveErrorKind) -> ResolveError {
        ResolveError {
            specifier: specifier.to_string(),
            from: from_dir.to_path_buf(),
            kind: Box::new(kind),
            witnesses: self.resolution_witnesses(specifier, from_dir),
        }
    }

    fn resolution_witnesses(&self, specifier: &str, from_dir: &Path) -> Vec<PathBuf> {
        let mut witnesses = std::collections::BTreeSet::from([normalize(from_dir)]);
        if !is_valid_bare_package_specifier(specifier)
            && let Some(aliased) = self.apply_alias(specifier)
        {
            witnesses.insert(aliased.clone());
            if let Some(parent) = aliased.parent() {
                witnesses.insert(parent.to_path_buf());
            }
            return witnesses.into_iter().collect();
        }

        let specifier_path = Path::new(specifier);
        if specifier.starts_with("./")
            || specifier.starts_with("../")
            || specifier_path.is_absolute()
        {
            let candidate = if specifier_path.is_absolute() {
                normalize(specifier_path)
            } else {
                normalize(&from_dir.join(specifier))
            };
            if let Some(parent) = candidate.parent() {
                witnesses.insert(parent.to_path_buf());
            }
            witnesses.insert(candidate);
            return witnesses.into_iter().collect();
        }

        let (package, subpath) = split_package_ref(specifier);
        let route = self.pnp_route(from_dir);
        if let Ok(PnpRoute::Managed(pnp)) = route {
            witnesses.insert(pnp.root().join(".pnp.cjs"));
            witnesses.insert(pnp.root().join(".pnp.data.json"));
            witnesses.insert(pnp.root().join("yarn.lock"));
            if let Ok(package_root) = pnp.resolve_bare(package, from_dir) {
                let candidate = if subpath.is_empty() {
                    package_root
                } else {
                    normalize(&package_root.join(subpath))
                };
                if let Some(parent) = candidate.parent() {
                    witnesses.insert(parent.to_path_buf());
                }
                witnesses.insert(candidate);
            }
            return witnesses.into_iter().collect();
        }
        if let Err(error) = route {
            witnesses.insert(error.path().to_path_buf());
            return witnesses.into_iter().collect();
        }

        let mut current = Some(normalize(from_dir));
        while let Some(directory) = current {
            let package_root = directory.join("node_modules").join(package);
            let candidate = if subpath.is_empty() {
                package_root
            } else {
                package_root.join(subpath)
            };
            if let Some(parent) = candidate.parent() {
                witnesses.insert(parent.to_path_buf());
            }
            witnesses.insert(candidate);
            current = directory.parent().map(Path::to_path_buf);
        }
        witnesses.into_iter().collect()
    }

    fn resolve_uncached(
        &self,
        specifier: &str,
        from_dir: &Path,
        profile: &ResolutionProfile,
    ) -> Result<PathBuf, ResolveErrorKind> {
        let specifier_path = Path::new(specifier);
        if specifier.starts_with("./")
            || specifier.starts_with("../")
            || specifier_path.is_absolute()
        {
            let base = if specifier_path.is_absolute() {
                normalize(specifier_path)
            } else {
                normalize(&from_dir.join(specifier))
            };
            return self
                .resolve_as_file_or_dir(&base)
                .ok_or(ResolveErrorKind::NotFound);
        }

        if is_valid_bare_package_specifier(specifier) {
            match self
                .pnp_route(from_dir)
                .map_err(ResolveErrorKind::PnpManifest)?
            {
                PnpRoute::Managed(pnp) => {
                    let (package, subpath) = split_package_ref(specifier);
                    let package_root = pnp
                        .resolve_bare(package, from_dir)
                        .map_err(ResolveErrorKind::PnpDependency)?;
                    return self
                        .resolve_package(&package_root, subpath, profile)
                        .ok_or(ResolveErrorKind::NotFound);
                }
                // Yarn 的 ignored / unmanaged 结果正式选择经典 Node 解析。
                PnpRoute::Classic => {
                    return self
                        .resolve_node_modules(specifier, from_dir, profile)
                        .ok_or(ResolveErrorKind::NotFound);
                }
                PnpRoute::NoManifest => {}
            }
        }

        // 只有未受 PnP 管理的包，或不构成 npm 包名的 Wake 路径 alias，才能命中 alias。
        if let Some(aliased) = self.apply_alias(specifier) {
            return self
                .resolve_as_file_or_dir(&aliased)
                .ok_or(ResolveErrorKind::NotFound);
        }

        self.resolve_node_modules(specifier, from_dir, profile)
            .ok_or(ResolveErrorKind::NotFound)
    }

    fn pnp_route(&self, from_dir: &Path) -> Result<PnpRoute, PnpLoadError> {
        if let Some(registry) = &self.pnp_registry {
            return registry.route(from_dir);
        }
        let Some(manifest) = &self.pnp else {
            return Ok(PnpRoute::NoManifest);
        };
        if manifest.is_ignored(from_dir) || !manifest.owns_issuer(from_dir) {
            Ok(PnpRoute::Classic)
        } else {
            Ok(PnpRoute::Managed(Arc::clone(manifest)))
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

    fn resolve_package(
        &self,
        package_root: &Path,
        subpath: &str,
        profile: &ResolutionProfile,
    ) -> Option<PathBuf> {
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
                let target = resolve_exports_target(&exports, &key, &profile.conditions)?;
                let relative = target.strip_prefix("./")?;
                return self.resolve_as_file_or_dir(&normalize(&package_root.join(relative)));
            }
            if subpath.is_empty()
                && let Some(entry) = config.entry(&profile.main_fields)
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

    fn resolve_node_modules(
        &self,
        specifier: &str,
        from_dir: &Path,
        profile: &ResolutionProfile,
    ) -> Option<PathBuf> {
        let (pkg_name, subpath) = split_package_ref(specifier);
        for pkg_dir in self.package_roots(pkg_name, from_dir).iter() {
            if let Some(resolved) = self.resolve_package(pkg_dir, subpath, profile) {
                return Some(resolved);
            }
        }
        None
    }

    fn find_package_root(&self, path: &Path) -> Option<PathBuf> {
        // `module_identity` already normalized `path`, so cache hits can probe the borrowed parent
        // directly without allocating another normalized `PathBuf` per module.
        let start = path.parent()?;
        let mut visited = Vec::new();
        for dir in start.ancestors() {
            // 只在短临界区读缓存；真实 FS 探测始终在锁外，保持并行 resolver
            // 的锁粒度。同 key 首次竞争时可能重复少量幂等探测，结果一致。
            let cached = self.package_owners.lock().unwrap().get(dir).cloned();
            if let Some(root) = cached {
                self.remember_package_owner(&visited, root.clone());
                return root;
            }
            visited.push(dir.to_path_buf());

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
                let root = Some(dir.to_path_buf());
                self.remember_package_owner(&visited, root.clone());
                return root;
            }
            if self.fs.is_file(&dir.join("package.json")) {
                let root = Some(dir.to_path_buf());
                self.remember_package_owner(&visited, root.clone());
                return root;
            }
        }
        self.remember_package_owner(&visited, None);
        None
    }

    fn remember_package_owner(&self, directories: &[PathBuf], root: Option<PathBuf>) {
        if directories.is_empty() {
            return;
        }
        let mut owners = self.package_owners.lock().unwrap();
        for directory in directories {
            owners.insert(directory.clone(), root.clone());
        }
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
        self.read_package_config(pkg)?
            .entry(&self.options.main_fields)
    }

    fn read_package_config(&self, pkg: &Path) -> Option<PackageConfig> {
        if let Some(config) = self.package_configs.lock().unwrap().get(pkg) {
            return config.clone();
        }
        let config = self.fs.read_to_string(pkg).ok().and_then(|text| {
            let json: OrderedJsonValue = serde_json::from_str(&text).ok()?;
            let root = json.as_object()?;
            let string_fields = root
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_owned()))
                })
                .collect();
            Some(PackageConfig {
                string_fields,
                exports: json.get("exports").cloned(),
                name: json
                    .get("name")
                    .and_then(OrderedJsonValue::as_str)
                    .map(str::to_owned),
                version: json
                    .get("version")
                    .and_then(OrderedJsonValue::as_str)
                    .map(str::to_owned),
                has_peer_dependencies: json
                    .get("peerDependencies")
                    .and_then(OrderedJsonValue::as_object)
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
    exports: &OrderedJsonValue,
    key: &str,
    conditions: &[String],
) -> Option<String> {
    let Some(map) = exports.as_object() else {
        return (key == ".").then(|| resolve_conditional_target(exports, conditions))?;
    };
    if !map.iter().any(|(candidate, _)| candidate.starts_with('.')) {
        return (key == ".").then(|| resolve_conditional_target(exports, conditions))?;
    }
    if let Some(value) = map
        .iter()
        .find_map(|(candidate, value)| (candidate == key).then_some(value))
    {
        return resolve_conditional_target(value, conditions);
    }

    let mut patterns = map
        .iter()
        .filter_map(|(pattern, value)| {
            let (prefix, suffix) = pattern.split_once('*')?;
            let capture = key.strip_prefix(prefix)?.strip_suffix(suffix)?;
            Some((prefix.len(), pattern.len(), capture, value))
        })
        .collect::<Vec<_>>();
    // Node's PATTERN_KEY_COMPARE prefers the longer base before the `*`, then the longer key.
    // Comparing only total literal length can select a broader pattern with a long suffix.
    patterns.sort_by_key(|item| std::cmp::Reverse((item.0, item.1)));
    let (_, _, capture, value) = patterns.into_iter().next()?;
    resolve_conditional_target(value, conditions).map(|target| target.replace('*', capture))
}

fn resolve_conditional_target(value: &OrderedJsonValue, conditions: &[String]) -> Option<String> {
    match value {
        OrderedJsonValue::String(target) => Some(target.clone()),
        OrderedJsonValue::Array(targets) => targets
            .iter()
            .find_map(|target| resolve_conditional_target(target, conditions)),
        OrderedJsonValue::Object(targets) => {
            for (condition, target) in targets {
                if (condition == "default" || conditions.iter().any(|active| active == condition))
                    && let Some(resolved) = resolve_conditional_target(target, conditions)
                {
                    return Some(resolved);
                }
            }
            None
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

/// 是否为 Yarn/Node 应作为 npm 包处理的裸说明符。
///
/// `@/`、`@@/`、`@@@/` 等 Wake 路径前缀故意返回 false，从而保留 alias；
/// `react`、`react/jsx-runtime`、`@scope/pkg/subpath` 返回 true，使 PnP 先于 alias。
fn is_valid_bare_package_specifier(specifier: &str) -> bool {
    fn valid_part(part: &str) -> bool {
        !part.is_empty()
            && !part.starts_with(['.', '_'])
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    }

    let (package, _) = split_package_ref(specifier);
    if let Some(scoped) = package.strip_prefix('@') {
        let Some((scope, name)) = scoped.split_once('/') else {
            return false;
        };
        valid_part(scope) && valid_part(name)
    } else {
        valid_part(package)
    }
}

/// 拆分裸说明符为 (包名, 子路径)。处理 scoped 包 `@scope/name/sub`。
pub(crate) fn split_package(specifier: &str) -> (String, String) {
    let (package, subpath) = split_package_ref(specifier);
    (package.to_owned(), subpath.to_owned())
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use wake_common::MemoryFileSystem;

    #[derive(Default)]
    struct CountingFileSystem {
        inner: MemoryFileSystem,
        package_json_probes: AtomicUsize,
    }

    impl CountingFileSystem {
        fn insert(&self, path: impl AsRef<Path>, contents: impl Into<Vec<u8>>) {
            self.inner.insert(path, contents);
        }

        fn package_json_probes(&self) -> usize {
            self.package_json_probes.load(Ordering::Relaxed)
        }
    }

    impl FileSystem for CountingFileSystem {
        fn read_to_string(&self, path: &Path) -> io::Result<String> {
            self.inner.read_to_string(path)
        }

        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            self.inner.read(path)
        }

        fn exists(&self, path: &Path) -> bool {
            self.inner.exists(path)
        }

        fn is_file(&self, path: &Path) -> bool {
            if path.file_name().is_some_and(|name| name == "package.json") {
                self.package_json_probes.fetch_add(1, Ordering::Relaxed);
            }
            self.inner.is_file(path)
        }

        fn is_dir(&self, path: &Path) -> bool {
            self.inner.is_dir(path)
        }

        fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
            self.inner.read_dir(path)
        }
    }

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

    #[cfg(windows)]
    #[test]
    fn windows_absolute_specifiers_use_the_same_file_resolver() {
        let r = resolver(&[("C:/project/src/value.ts", "export const value = 42")]);
        assert_eq!(
            r.resolve("C:/project/src/value", Path::new("C:/project/tests"))
                .unwrap(),
            PathBuf::from("C:/project/src/value.ts")
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
        let node = ResolutionProfile {
            conditions: vec!["node".into(), "require".into()],
            main_fields: vec!["main".into(), "module".into()],
        };
        assert_eq!(
            r.resolve_with_profile("react", Path::new("src"), &node)
                .unwrap(),
            PathBuf::from("node_modules/react/index.js")
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
    fn module_identity_compresses_package_root_lookups_by_directory() {
        let fs = Arc::new(CountingFileSystem::default());
        fs.insert(
            "project/package.json",
            r#"{"name":"app","version":"1.0.0"}"#,
        );
        for index in 0..64 {
            fs.insert(
                format!("project/src/pages/page-{index}.js"),
                "export default 1",
            );
            fs.insert(
                format!("project/src/utils/util-{index}.js"),
                "export default 1",
            );
        }
        let resolver = Resolver::new(fs.clone());

        for (directory, prefix) in [("pages", "page"), ("utils", "util")] {
            for index in 0..64 {
                let path = format!("project/src/{directory}/{prefix}-{index}.js");
                assert!(matches!(
                    resolver.module_identity(Path::new(&path)),
                    ModuleIdentity::Package { package, .. } if package.name == "app"
                ));
            }
        }

        // pages 首次：pages → src → project；utils 首次：utils → 已缓存 src。
        assert_eq!(fs.package_json_probes(), 4);
    }

    #[test]
    fn package_owner_path_compression_is_safe_under_parallel_discovery() {
        const THREADS: usize = 8;
        const FILES_PER_THREAD: usize = 8;
        let fs = Arc::new(CountingFileSystem::default());
        fs.insert(
            "project/package.json",
            r#"{"name":"app","version":"1.0.0"}"#,
        );
        for index in 0..=THREADS * FILES_PER_THREAD {
            fs.insert(
                format!("project/src/pages/page-{index}.js"),
                "export default 1",
            );
        }
        let resolver = Arc::new(Resolver::new(fs.clone()));
        let barrier = Arc::new(Barrier::new(THREADS));
        let mut handles = Vec::new();
        for thread in 0..THREADS {
            let resolver = Arc::clone(&resolver);
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for offset in 0..FILES_PER_THREAD {
                    let index = thread * FILES_PER_THREAD + offset;
                    let path = format!("project/src/pages/page-{index}.js");
                    assert!(matches!(
                        resolver.module_identity(Path::new(&path)),
                        ModuleIdentity::Package { package, .. } if package.name == "app"
                    ));
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let probes = fs.package_json_probes();
        assert!(probes <= THREADS * 3);
        assert!(matches!(
            resolver.module_identity(Path::new("project/src/pages/page-64.js")),
            ModuleIdentity::Package { package, .. } if package.name == "app"
        ));
        assert_eq!(fs.package_json_probes(), probes);
    }

    #[test]
    fn cached_parent_owner_does_not_hide_a_nested_package_boundary() {
        let fs = Arc::new(CountingFileSystem::default());
        fs.insert(
            "project/package.json",
            r#"{"name":"outer","version":"1.0.0"}"#,
        );
        fs.insert("project/src/outer.js", "export default 1");
        fs.insert(
            "project/src/vendor/nested/package.json",
            r#"{"name":"nested","version":"2.0.0"}"#,
        );
        fs.insert("project/src/vendor/nested/index.js", "export default 2");
        fs.insert(
            "project/src/vendor/nested/lib/feature.js",
            "export default 3",
        );
        let resolver = Resolver::new(fs);

        assert!(matches!(
            resolver.module_identity(Path::new("project/src/outer.js")),
            ModuleIdentity::Package { package, .. } if package.name == "outer"
        ));
        for path in [
            "project/src/vendor/nested/index.js",
            "project/src/vendor/nested/lib/feature.js",
        ] {
            assert!(matches!(
                resolver.module_identity(Path::new(path)),
                ModuleIdentity::Package { package, .. } if package.name == "nested"
            ));
        }
    }

    #[test]
    fn clear_cache_invalidates_package_owner_path_compression() {
        let fs = Arc::new(CountingFileSystem::default());
        fs.insert("project/src/value.js", "export default 1");
        let resolver = Resolver::new(fs.clone());
        let path = Path::new("project/src/value.js");

        assert!(matches!(
            resolver.module_identity(path),
            ModuleIdentity::File(_)
        ));
        fs.insert(
            "project/package.json",
            r#"{"name":"app","version":"1.0.0"}"#,
        );
        resolver.clear_cache();
        assert!(matches!(
            resolver.module_identity(path),
            ModuleIdentity::Package { package, .. } if package.name == "app"
        ));
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
    fn package_export_patterns_prefer_the_longer_base_before_total_key_length() {
        let r = resolver(&[
            (
                "node_modules/patterns/package.json",
                r#"{"exports":{"./a*def":"./broad/*.js","./abc*":"./specific/*.js"}}"#,
            ),
            ("node_modules/patterns/broad/bc.js", "// broad"),
            ("node_modules/patterns/specific/def.js", "// specific"),
        ]);

        assert_eq!(
            r.resolve("patterns/abcdef", Path::new("src")).unwrap(),
            PathBuf::from("node_modules/patterns/specific/def.js")
        );
    }

    #[test]
    fn conditional_exports_cache_isolated_by_platform_and_edge_kind() {
        let r = resolver(&[
            (
                "node_modules/dual/package.json",
                r#"{"exports":{".":{"node":{"import":"./node-import.js","require":"./node-require.js"},"browser":{"import":"./browser-import.js","require":"./browser-require.js"},"default":"./default.js"}}}"#,
            ),
            ("node_modules/dual/node-import.js", "// node import"),
            ("node_modules/dual/node-require.js", "// node require"),
            ("node_modules/dual/browser-import.js", "// browser import"),
            ("node_modules/dual/browser-require.js", "// browser require"),
            ("node_modules/dual/default.js", "// default"),
        ]);
        let node_import = ["node", "import", "default"].map(str::to_string);
        let node_require = ["node", "require", "default"].map(str::to_string);
        let browser_import = ["browser", "import", "module", "default"].map(str::to_string);
        let browser_require = ["browser", "require", "default"].map(str::to_string);
        assert_eq!(
            r.resolve_with_conditions("dual", Path::new("src"), &node_import)
                .unwrap(),
            PathBuf::from("node_modules/dual/node-import.js")
        );
        assert_eq!(
            r.resolve_with_conditions("dual", Path::new("src"), &node_require)
                .unwrap(),
            PathBuf::from("node_modules/dual/node-require.js")
        );
        assert_eq!(
            r.resolve_with_conditions("dual", Path::new("src"), &browser_import)
                .unwrap(),
            PathBuf::from("node_modules/dual/browser-import.js")
        );
        assert_eq!(
            r.resolve_with_conditions("dual", Path::new("src"), &browser_require)
                .unwrap(),
            PathBuf::from("node_modules/dual/browser-require.js")
        );
    }

    #[test]
    fn conditional_exports_follow_package_declaration_order() {
        let r = resolver(&[
            (
                "node_modules/ordered/package.json",
                r#"{"exports":{".":{"default":"./default.js","node":"./node.js"}}}"#,
            ),
            ("node_modules/ordered/default.js", "// default first"),
            ("node_modules/ordered/node.js", "// node second"),
        ]);
        let node = ResolutionProfile {
            conditions: vec!["node".into(), "import".into()],
            main_fields: vec!["main".into(), "module".into()],
        };
        assert_eq!(
            r.resolve_with_profile("ordered", Path::new("src"), &node)
                .unwrap(),
            PathBuf::from("node_modules/ordered/default.js")
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
            "project/.pnp.cjs",
            "module.exports = require('./.pnp.data.json');",
        );
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

    #[test]
    fn failed_pnp_exports_keep_resolver_owned_manifest_and_package_witnesses() {
        let fs = MemoryFileSystem::new();
        fs.insert(
            "project/.pnp.cjs",
            "module.exports = require('./.pnp.data.json');",
        );
        fs.insert(
            "project/.pnp.data.json",
            r#"{
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
        fs.insert(
            "cache/modern/node_modules/modern/package.json",
            r#"{"exports":{".":"./index.js"}}"#,
        );
        let manifest = PnpManifest::load(&fs, Path::new("project")).unwrap();
        let resolver = Resolver::with_pnp(Arc::new(fs), Arc::new(manifest));

        let error = resolver
            .resolve("modern/private", Path::new("project/src"))
            .unwrap_err();

        assert!(
            error
                .witnesses()
                .contains(&PathBuf::from("project/.pnp.cjs"))
        );
        assert!(
            error
                .witnesses()
                .contains(&PathBuf::from("cache/modern/node_modules/modern/private"))
        );
        assert!(
            error
                .witnesses()
                .contains(&PathBuf::from("cache/modern/node_modules/modern"))
        );
    }

    fn pnp_authoritative_resolver(component_declares_css: bool) -> Resolver {
        let mut component_dependencies =
            vec![serde_json::json!(["@crab-dev/rc-button", "npm:1.0.0"])];
        if component_declares_css {
            component_dependencies.push(serde_json::json!(["@crab-dev/css", "npm:2.0.0"]));
        }
        let manifest_value = serde_json::json!({
            "enableTopLevelFallback": false,
            "packageRegistryData": [
                [null, [[null, {
                    "packageLocation": "./",
                    "packageDependencies": [["@crab-dev/rc-button", "npm:1.0.0"]]
                }]]],
                ["@crab-dev/rc-button", [["npm:1.0.0", {
                    "packageLocation": "../cache/button/node_modules/@crab-dev/rc-button/",
                    "packageDependencies": component_dependencies
                }]]],
                ["@crab-dev/css", [["npm:2.0.0", {
                    "packageLocation": "../cache/css/node_modules/@crab-dev/css/",
                    "packageDependencies": [["@crab-dev/css", "npm:2.0.0"]]
                }]]]
            ]
        });
        let fs = MemoryFileSystem::new();
        fs.insert(
            "project/.pnp.cjs",
            "module.exports = require('./.pnp.data.json');",
        );
        fs.insert("project/.pnp.data.json", manifest_value.to_string());
        fs.insert(
            "cache/css/node_modules/@crab-dev/css/package.json",
            r#"{"exports":{".":"./index.js"}}"#,
        );
        fs.insert("cache/css/node_modules/@crab-dev/css/index.js", "// css");
        fs.insert("alias/crab-css.js", "// alias");
        let manifest = PnpManifest::load(&fs, Path::new("project")).unwrap();
        Resolver::with_pnp_options(
            Arc::new(fs),
            Arc::new(manifest),
            ResolveOptions {
                alias: vec![(
                    "@crab-dev/css".to_string(),
                    PathBuf::from("alias/crab-css.js"),
                )],
                ..ResolveOptions::default()
            },
        )
    }

    #[test]
    fn pnp_dependency_wins_over_valid_bare_package_alias() {
        let resolver = pnp_authoritative_resolver(true);
        assert_eq!(
            resolver
                .resolve(
                    "@crab-dev/css",
                    Path::new("cache/button/node_modules/@crab-dev/rc-button/esm")
                )
                .unwrap(),
            PathBuf::from("cache/css/node_modules/@crab-dev/css/index.js")
        );
    }

    #[test]
    fn pnp_rejection_is_not_overridden_by_alias_or_components_bridge() {
        let resolver = pnp_authoritative_resolver(false);
        let error = resolver
            .resolve(
                "@crab-dev/css",
                Path::new("cache/button/node_modules/@crab-dev/rc-button/esm"),
            )
            .unwrap_err();
        assert_eq!(
            error.kind(),
            &ResolveErrorKind::PnpDependency(PnpError::Undeclared)
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
        assert_eq!(
            r.resolve("./missing", Path::new(".")).unwrap_err().kind(),
            &ResolveErrorKind::NotFound
        );
        assert_eq!(
            r.resolve("nonexistent-pkg", Path::new("."))
                .unwrap_err()
                .kind(),
            &ResolveErrorKind::NotFound
        );
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
