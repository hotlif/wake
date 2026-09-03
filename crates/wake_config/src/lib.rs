//! # wake_config — 声明式项目配置（`wake.config.toml`）
//!
//! 旧实现使用 `unconfig` **执行** `executable TypeScript configuration`（含 JS 逻辑 / 正则 / `mods` 函数）；wake 是
//! 纯 Rust、无 JS 运行时，无法执行 TS 配置。故对齐方案是**声明式 TOML**（WAKE-COMPATIBILITY 决策①）：
//! 字段一一对应 legacy tool `Config`，正则类字段以字符串表达、`mods` 折叠为 `[hooks]` 声明项（决策④）。
//!
//! 加载：[`load`] 从项目根读 `wake.config.toml`（不存在 → [`Config::default`]，保持零配置可跑）。
//! 根发现：[`find_root`] 从入口向上找含 `wake.config.toml` / `package.json` 的目录。
//!
//! 本 crate **只做数据**：不依赖 wake_resolver / wake_bundler，避免依赖倒挂。别名表由
//! [`Config::resolver_aliases`] 产出 `(前缀, 绝对路径)`，由 CLI 桥接进 `ResolveOptions`。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer};

pub use wake_federation_contract::{
    ContainerName, ExposeConfig, ExposeKey, ExposeMode, FederationConfig, FederationOptions,
    RemoteConfig, ShadowMode, SharedConfig,
};

/// 配置文件名（固定）。
pub const CONFIG_FILE: &str = "wake.config.toml";

/// 零配置项目使用的现代浏览器基线。
///
/// 直接保存每个 family 的最低版本，不经 Browserslist 数据库展开，从而避免数据库更新时间
/// 改变零配置项目的目标集合或缓存身份。语义等价于对应 family 的 `>=` 查询。
pub const DEFAULT_BROWSER_TARGETS: [(&str, &str); 5] = [
    ("chrome", "120"),
    ("edge", "120"),
    ("firefox", "121"),
    ("safari", "17.2"),
    ("ios_saf", "17.2"),
];

/// 项目配置（保持既定行为 `Config`）。所有字段可省，缺省即零配置默认。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 项目根（相对配置文件所在目录）。缺省 = 配置所在目录 / cwd。
    pub root_dir: Option<String>,
    /// 静态资源公共路径（如 `/app/`）；Electron/file URL 使用 `./`。缺省 `/`。
    pub public_path: Option<String>,
    /// 路径别名（前缀 → 相对根的路径）。CLI 会叠加默认 `@`→`src`、`@@`→根。
    pub alias: BTreeMap<String, String>,
    /// 组件自动扫描规则（M2）。
    pub component_scan: Vec<ComponentScan>,
    /// 开发服务器配置（M3）。
    pub dev_server: DevServer,
    /// HTML / 入口生成（M2）。
    pub html: Html,
    /// 声明式 hook（提供声明式替代 `mods`，决策④）。
    pub hooks: Hooks,
    /// 全局常量替换（`process.env.NODE_ENV` 等）。dev/prod 由 CLI 注入默认值。
    #[serde(default, deserialize_with = "deserialize_defines")]
    pub define: BTreeMap<String, String>,
    /// 显式 Browserslist 查询。为空时依次查找 `.browserslistrc` 和
    /// `package.json#browserslist`，最终使用 [`DEFAULT_BROWSER_TARGETS`]。
    pub browserslist: Vec<String>,
    /// Browserslist 查询行为。
    pub browserslist_options: BrowserslistOptions,
    /// TypeScript 转换选项（`.ts`/`.tsx` 默认启用）。
    pub typescript: TypeScript,
    /// React JSX automatic runtime 选项。
    pub react: React,
    /// Wake-native browser Module Federation configuration. Disabled unless explicitly enabled.
    #[serde(default, deserialize_with = "deserialize_federation_config")]
    pub federation: FederationConfig,
    /// React 组件文档站配置。
    pub docs: Docs,
    /// Wake-owned React test runner configuration.
    pub test: Test,
    /// 手动强制启用/禁用特定 Babel 风格 transform 名。
    pub transforms: TransformControl,
}

fn deserialize_federation_config<'de, D>(deserializer: D) -> Result<FederationConfig, D::Error>
where
    D: Deserializer<'de>,
{
    FederationConfig::deserialize(deserializer)?
        .validate_and_normalize()
        .map_err(serde::de::Error::custom)
}

fn deserialize_defines<'de, D>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: Deserializer<'de>,
{
    let defines = BTreeMap::<String, String>::deserialize(deserializer)?;
    if defines.contains_key("import.meta.hot") {
        return Err(serde::de::Error::custom(
            "`import.meta.hot` is reserved and always false: Wake provides Live Reload, not a module HMR API",
        ));
    }
    Ok(defines)
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct BrowserslistOptions {
    /// 移动浏览器缺少对应版本数据时，使用桌面浏览器数据。
    pub mobile_to_desktop: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TransformControl {
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

/// Wake-owned test configuration. This is deliberately not a Jest configuration adapter: every
/// accepted key is snake_case and every nested table rejects unknown fields.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Test {
    pub environment: TestEnvironment,
    pub include: Vec<String>,
    pub browser_include: Vec<String>,
    pub exclude: Vec<String>,
    pub setup: Vec<String>,
    pub timeout_ms: u64,
    pub workers: WorkerCount,
    pub forbid_only: bool,
    pub leaks: TestLeakPolicy,
    pub react: TestReact,
    pub browser: TestBrowser,
    pub network: TestNetwork,
    pub snapshot: TestSnapshot,
    pub coverage: TestCoverage,
    #[serde(default, deserialize_with = "deserialize_test_projects")]
    pub projects: Vec<TestProject>,
}

impl Default for Test {
    fn default() -> Self {
        Self {
            environment: TestEnvironment::Auto,
            include: vec!["**/*.{test,spec}.{js,mjs,cjs,jsx,ts,mts,cts,tsx}".to_string()],
            browser_include: vec![
                "**/*.browser.{test,spec}.{js,mjs,cjs,jsx,ts,mts,cts,tsx}".to_string(),
            ],
            exclude: vec!["**/node_modules/**".to_string(), "**/dist/**".to_string()],
            setup: Vec::new(),
            timeout_ms: 5_000,
            workers: WorkerCount::Auto,
            forbid_only: false,
            leaks: TestLeakPolicy::Warn,
            react: TestReact::default(),
            browser: TestBrowser::default(),
            network: TestNetwork::default(),
            snapshot: TestSnapshot::default(),
            coverage: TestCoverage::default(),
            projects: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestEnvironment {
    #[default]
    Auto,
    Dom,
    Browser,
}

/// Number of suite workers. TOML accepts `"auto"`, a positive integer, or a percentage such as
/// `"50%"`; invalid and zero values fail while parsing the configuration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum WorkerCount {
    #[default]
    Auto,
    Count(usize),
    Percent(u8),
}

impl<'de> Deserialize<'de> for WorkerCount {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            Count(usize),
            Text(String),
        }

        match Value::deserialize(deserializer)? {
            Value::Count(0) => Err(serde::de::Error::custom(
                "workers must be greater than zero",
            )),
            Value::Count(count) => Ok(Self::Count(count)),
            Value::Text(value) if value == "auto" => Ok(Self::Auto),
            Value::Text(value) => {
                let percent = value
                    .strip_suffix('%')
                    .and_then(|value| value.parse::<u8>().ok())
                    .filter(|percent| (1..=100).contains(percent))
                    .ok_or_else(|| {
                        serde::de::Error::custom(
                            "workers must be \"auto\", a positive integer, or a percentage from 1% to 100%",
                        )
                    })?;
                Ok(Self::Percent(percent))
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestLeakPolicy {
    Off,
    #[default]
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestDiagnosticPolicy {
    Off,
    Warn,
    #[default]
    Error,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestReact {
    pub strict_mode: bool,
    pub cleanup: bool,
    pub act_warnings: TestDiagnosticPolicy,
    pub test_id_attribute: String,
}

impl Default for TestReact {
    fn default() -> Self {
        Self {
            strict_mode: false,
            cleanup: true,
            act_warnings: TestDiagnosticPolicy::Error,
            test_id_attribute: "data-testid".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestBrowser {
    pub headless: bool,
    pub sandbox: bool,
    pub viewport: TestViewport,
    pub locale: String,
    pub timezone: String,
    pub color_scheme: TestColorScheme,
}

impl Default for TestBrowser {
    fn default() -> Self {
        Self {
            headless: true,
            sandbox: true,
            viewport: TestViewport::default(),
            locale: "en-US".to_string(),
            timezone: "UTC".to_string(),
            color_scheme: TestColorScheme::Light,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestViewport {
    pub width: u32,
    pub height: u32,
    pub device_scale_factor: f64,
}

impl Default for TestViewport {
    fn default() -> Self {
        Self {
            width: 1_280,
            height: 720,
            device_scale_factor: 1.0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestColorScheme {
    #[default]
    Light,
    Dark,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestNetwork {
    pub mode: TestNetworkMode,
    pub allow_hosts: Vec<String>,
}

impl Default for TestNetwork {
    fn default() -> Self {
        Self {
            mode: TestNetworkMode::Deny,
            allow_hosts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestNetworkMode {
    #[default]
    Deny,
    Allow,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestSnapshot {
    pub directory: String,
    pub screenshot_directory: String,
}

impl Default for TestSnapshot {
    fn default() -> Self {
        Self {
            directory: "__snapshots__".to_string(),
            screenshot_directory: "__screenshots__".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestCoverage {
    pub enabled: bool,
    pub reporters: Vec<TestCoverageReporter>,
    pub threshold: TestCoverageThreshold,
    pub per_file: Vec<TestCoverageFileThreshold>,
}

impl Default for TestCoverage {
    fn default() -> Self {
        Self {
            enabled: false,
            reporters: vec![TestCoverageReporter::Text],
            threshold: TestCoverageThreshold::default(),
            per_file: Vec::new(),
        }
    }
}

/// Minimum percentages required from the aggregate coverage result. A missing metric is not
/// checked. Values are parsed as either TOML integers or floats and must stay within 0..=100.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TestCoverageThreshold {
    #[serde(default, deserialize_with = "deserialize_optional_coverage_percentage")]
    pub lines: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_coverage_percentage")]
    pub functions: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_coverage_percentage")]
    pub blocks: Option<f64>,
}

/// Per-file threshold selected with a Wake glob relative to the project root.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestCoverageFileThreshold {
    #[serde(deserialize_with = "deserialize_non_empty_coverage_pattern")]
    pub pattern: String,
    #[serde(default, deserialize_with = "deserialize_optional_coverage_percentage")]
    pub lines: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_coverage_percentage")]
    pub functions: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_coverage_percentage")]
    pub blocks: Option<f64>,
}

fn deserialize_optional_coverage_percentage<'de, D>(
    deserializer: D,
) -> Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Number {
        Integer(i64),
        Float(f64),
    }

    let value = Option::<Number>::deserialize(deserializer)?.map(|value| match value {
        Number::Integer(value) => value as f64,
        Number::Float(value) => value,
    });
    if value.is_some_and(|value| !value.is_finite() || !(0.0..=100.0).contains(&value)) {
        return Err(serde::de::Error::custom(
            "coverage threshold must be a percentage from 0 to 100",
        ));
    }
    Ok(value)
}

fn deserialize_non_empty_coverage_pattern<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    if value.trim().is_empty() {
        return Err(serde::de::Error::custom(
            "coverage per-file pattern must not be empty",
        ));
    }
    Ok(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TestCoverageReporter {
    Text,
    Json,
    Lcov,
    Html,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TestProject {
    pub name: String,
    #[serde(default = "default_test_project_root")]
    pub root: String,
    #[serde(default)]
    pub environment: TestEnvironment,
}

fn deserialize_test_projects<'de, D>(deserializer: D) -> Result<Vec<TestProject>, D::Error>
where
    D: Deserializer<'de>,
{
    let projects = Vec::<TestProject>::deserialize(deserializer)?;
    let mut names = BTreeSet::new();
    for project in &projects {
        if project.name.trim().is_empty() {
            return Err(serde::de::Error::custom(
                "test project name must not be empty",
            ));
        }
        if !names.insert(project.name.as_str()) {
            return Err(serde::de::Error::custom(format!(
                "test project name {:?} must be unique",
                project.name
            )));
        }
    }
    Ok(projects)
}

fn default_test_project_root() -> String {
    ".".to_string()
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TypeScript {
    pub enabled: bool,
    pub only_remove_type_imports: bool,
}

impl Default for TypeScript {
    fn default() -> Self {
        Self {
            enabled: true,
            only_remove_type_imports: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct React {
    pub enabled: bool,
    pub jsx_import_source: String,
}

impl Default for React {
    fn default() -> Self {
        Self {
            enabled: true,
            jsx_import_source: "react".to_string(),
        }
    }
}

/// Wake 原生 React 组件文档站配置。
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Docs {
    pub source_dir: String,
    pub title: String,
    pub description: String,
    pub locale: String,
    pub logo: Option<String>,
    pub repository_url: Option<String>,
    pub base_path: String,
    pub preview: Option<String>,
    pub theme_css: Option<String>,
    pub default_theme: String,
    pub accent_color: Option<String>,
    pub workspace: Vec<DocsWorkspace>,
}

impl Default for Docs {
    fn default() -> Self {
        Self {
            source_dir: "docs".to_string(),
            title: "Wake Docs".to_string(),
            description: String::new(),
            locale: "zh-CN".to_string(),
            logo: None,
            repository_url: None,
            base_path: "/".to_string(),
            preview: None,
            theme_css: None,
            default_theme: "system".to_string(),
            accent_color: None,
            workspace: Vec::new(),
        }
    }
}

/// A components documentation workspace mounted by a parent Docs site.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DocsWorkspace {
    /// Parent directory whose direct children are candidate workspaces.
    pub root: String,
    /// Case-sensitive wildcard patterns matched against the child directory name.
    pub include: Vec<String>,
    /// Site-relative public path template. Must contain `{name}`.
    pub base_path: String,
    /// Workspace Docs product. Aggregation currently supports components only.
    pub mode: DocsWorkspaceMode,
    /// Workbench presentation when mounted by the parent site.
    pub presentation: DocsWorkspacePresentation,
    /// Development loading policy. Production always builds every workspace.
    pub dev_loading: DocsWorkspaceDevLoading,
}

impl Default for DocsWorkspace {
    fn default() -> Self {
        Self {
            root: String::new(),
            include: Vec::new(),
            base_path: String::new(),
            mode: DocsWorkspaceMode::Components,
            presentation: DocsWorkspacePresentation::Embedded,
            dev_loading: DocsWorkspaceDevLoading::Lazy,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocsWorkspaceMode {
    #[default]
    Components,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocsWorkspacePresentation {
    #[default]
    Embedded,
    Standalone,
}

impl DocsWorkspacePresentation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Embedded => "embedded",
            Self::Standalone => "standalone",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocsWorkspaceDevLoading {
    #[default]
    Lazy,
    Eager,
}

impl DocsWorkspaceDevLoading {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lazy => "lazy",
            Self::Eager => "eager",
        }
    }
}

/// Browserslist 查询后的稳定 DTO。编译核心不依赖 browserslist 数据库。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct BrowserTarget {
    pub name: String,
    pub version: String,
}

/// 一条组件自动扫描规则（保持既定行为 `ComponentScanRule`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ComponentScan {
    /// 命名空间，经 `@@@/{namespace}` 导入。
    pub namespace: String,
    /// 扫描目录（相对根）。
    pub cwd: String,
    /// 是否在产物中包含源码字符（默认否）。
    pub generate_source: bool,
    /// 文件包含正则（字符串）。
    pub include: Option<String>,
    /// 文件排除正则（字符串）。
    pub exclude: Option<String>,
}

/// 开发服务器配置（保持既定行为 `DevServer`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DevServer {
    /// `"http"` | `"https"`。缺省 http。
    pub server: Option<String>,
    /// 监听端口。缺省由 CLI 决定（5173）。
    pub port: Option<u16>,
    /// 监听地址。缺省 `127.0.0.1`。
    pub host: Option<String>,
    /// 启动后自动打开浏览器。
    pub open: bool,
    /// 代理规则。
    pub proxy: Vec<Proxy>,
}

/// 代理配置（保持既定行为 `Proxy`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Proxy {
    /// 匹配的路径前缀（如 `["/api"]`）。
    pub context: Vec<String>,
    /// 转发目标（如 `http://localhost:8080`）。
    pub target: String,
    /// 是否代理 WebSocket（HTTP Upgrade 转发）。
    pub ws: bool,
    /// 是否把请求头 `Host` 改写为 target 的 host（跨域远端需开）。
    pub change_origin: bool,
    /// 路径改写（正则字符串 → 替换）。
    pub path_rewrite: BTreeMap<String, String>,
}

/// HTML / 入口生成配置。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Html {
    /// HTML 外壳模板路径（相对根）。缺省 `public/index.html`，无则用内置外壳。
    pub template: Option<String>,
    /// 虚拟入口目标（相对根）。缺省 `src/entry.tsx`（生成 `import("@/entry.tsx")`）。
    pub entry: Option<String>,
}

/// 声明式 hook（提供声明式替代 `Modification` 的 JS 函数，决策④）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Hooks {
    /// `bootstrap` 文件查找路径（相对根）。替代 `modifyBootstrapPath`。
    pub bootstrap_path: Option<String>,
}

/// 配置加载错误。
#[derive(Debug)]
pub enum ConfigError {
    /// 读文件失败（非「不存在」）。
    Io(PathBuf, String),
    /// TOML 解析失败。
    Parse(PathBuf, String),
    /// Browserslist 配置或查询无效。
    Browserslist(PathBuf, String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(p, e) => write!(f, "无法读取配置 `{}`：{e}", p.display()),
            ConfigError::Parse(p, e) => write!(f, "配置 `{}` 解析失败：{e}", p.display()),
            ConfigError::Browserslist(p, e) => {
                write!(f, "项目 `{}` 的 browserslist 解析失败：{e}", p.display())
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// 从 `root` 加载 `wake.config.toml`。文件不存在 → 默认配置（零配置可跑）。
pub fn load(root: &Path) -> Result<Config, ConfigError> {
    let path = root.join(CONFIG_FILE);
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).map_err(|e| ConfigError::Parse(path, e.to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(ConfigError::Io(path, e.to_string())),
    }
}

/// 从 `start` 向上查找含 `wake.config.toml` 或 `package.json` 的项目根；找不到回退 `start`。
pub fn find_root(start: &Path) -> PathBuf {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if d.join(CONFIG_FILE).is_file() || d.join("package.json").is_file() {
            return d.to_path_buf();
        }
        dir = d.parent();
    }
    start.to_path_buf()
}

impl Config {
    /// 按 wake 配置、`.browserslistrc`、`package.json`、现代默认基线的顺序解析目标。
    pub fn resolve_browser_targets(&self, root: &Path) -> Result<Vec<BrowserTarget>, ConfigError> {
        let queries = if !self.browserslist.is_empty() {
            self.browserslist.clone()
        } else if let Some(queries) = read_browserslist_rc(root)? {
            queries
        } else if let Some(queries) = read_package_browserslist(root)? {
            queries
        } else {
            return Ok(DEFAULT_BROWSER_TARGETS
                .into_iter()
                .map(|(name, version)| BrowserTarget {
                    name: name.to_string(),
                    version: version.to_string(),
                })
                .collect());
        };

        let opts = browserslist::Opts {
            mobile_to_desktop: self.browserslist_options.mobile_to_desktop,
            ..browserslist::Opts::default()
        };
        let mut targets = browserslist::resolve(&queries, &opts)
            .map_err(|e| ConfigError::Browserslist(root.to_path_buf(), e.to_string()))?
            .into_iter()
            .map(|d| BrowserTarget {
                name: d.name().to_ascii_lowercase(),
                version: d.version().to_string(),
            })
            .collect::<Vec<_>>();
        targets.sort();
        targets.dedup();
        Ok(targets)
    }

    /// 解析后的项目根（`root_dir` 相对 `config_dir`，缺省即 `config_dir`）。
    pub fn resolved_root(&self, config_dir: &Path) -> PathBuf {
        match &self.root_dir {
            Some(r) => config_dir.join(r),
            None => config_dir.to_path_buf(),
        }
    }

    /// 组装 resolver 别名表：`(前缀, 绝对路径)`。含默认 `@`→`root/src`、`@@`→`root`，
    /// 叠加 `[alias]` 配置项（同名覆盖，值相对 `root`）。CLI 桥接进 `ResolveOptions.alias`。
    pub fn resolver_aliases(&self, root: &Path) -> Vec<(String, PathBuf)> {
        let mut v: Vec<(String, PathBuf)> = vec![
            ("@".to_string(), root.join("src")),
            ("@@".to_string(), root.to_path_buf()),
        ];
        for (k, val) in &self.alias {
            let p = root.join(val);
            if let Some(slot) = v.iter_mut().find(|(kk, _)| kk == k) {
                slot.1 = p;
            } else {
                v.push((k.clone(), p));
            }
        }
        v
    }

    /// 公共路径（缺省 `/`）。
    pub fn public_path(&self) -> &str {
        self.public_path.as_deref().unwrap_or("/")
    }
}

fn read_browserslist_rc(root: &Path) -> Result<Option<Vec<String>>, ConfigError> {
    let path = root.join(".browserslistrc");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ConfigError::Io(path, e.to_string())),
    };
    let queries = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with('['))
        .map(str::to_string)
        .collect::<Vec<_>>();
    Ok((!queries.is_empty()).then_some(queries))
}

fn read_package_browserslist(root: &Path) -> Result<Option<Vec<String>>, ConfigError> {
    let path = root.join("package.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ConfigError::Io(path, e.to_string())),
    };
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| ConfigError::Parse(path.clone(), e.to_string()))?;
    let Some(value) = value.get("browserslist") else {
        return Ok(None);
    };
    let queries = match value {
        serde_json::Value::String(query) => vec![query.clone()],
        serde_json::Value::Array(items) => items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        serde_json::Value::Object(envs) => envs
            .get("production")
            .or_else(|| envs.get("defaults"))
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect(),
        _ => Vec::new(),
    };
    Ok((!queries.is_empty()).then_some(queries))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_default() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.alias.is_empty());
        assert_eq!(c.public_path(), "/");
        assert!(c.component_scan.is_empty());
        assert!(!c.federation.enabled);
        assert!(c.federation.name.as_str().is_empty());
        assert!(c.federation.remotes.is_empty());
        assert!(c.federation.exposes.is_empty());
        assert!(c.federation.shared.is_empty());
    }

    #[test]
    fn live_reload_contract_rejects_fake_hmr_configuration() {
        for source in [
            "[dev_server]\nhmr = true\n",
            "[dev_server]\nhot = true\n",
            "[dev_server]\nlive_reload = false\n",
            "[define]\n\"import.meta.hot\" = \"true\"\n",
        ] {
            let error = toml::from_str::<Config>(source).unwrap_err().to_string();
            assert!(
                error.contains("unknown field") || error.contains("reserved and always false"),
                "{source}\n{error}"
            );
        }
    }

    #[test]
    fn federation_config_parses_and_normalizes_browser_contract() {
        let config: Config = toml::from_str(
            r#"
[federation]
enabled = true
name = "shell"

[federation.remotes.catalog]
manifest_url = "https://catalog.example.test/assets/wake-federation.json"
allowed_origins = ["https://catalog.example.test/", "https://catalog.example.test"]
dev_follow = false

[federation.exposes."./Button"]
entry = "src/button.tsx"
mode = "host-rendered"
scope = "react18"
shadow = "none"
allow_global_css = true

[federation.exposes.LegacyCard]
entry = "src/legacy-card.tsx"
mode = "isolated"
scope = "react17"

[federation.shared.react]
scope = "react18"
required_version = "^18.3.0"
singleton = true
strict = true
fallback = false
coherence_group = "react18"
owner = "shell"

[federation.shared."react/jsx-runtime"]
scope = "react18"
required_version = "^18.3.0"
singleton = true
strict = true
coherence_group = "react18"
owner = "shell"

[federation.shared."react/jsx-dev-runtime"]
scope = "react18"
required_version = "^18.3.0"
singleton = true
strict = true
coherence_group = "react18"
owner = "shell"

[federation.shared.react-dom]
scope = "react18"
required_version = "^18.3.0"
singleton = true
strict = true
coherence_group = "react18"
owner = "shell"

[federation.shared."react-dom/client"]
scope = "react18"
required_version = "^18.3.0"
singleton = true
strict = true
coherence_group = "react18"
owner = "shell"
"#,
        )
        .unwrap();

        let federation = config.federation;
        assert!(federation.enabled);
        assert_eq!(federation.name.as_str(), "shell");
        let remote = &federation.remotes[&ContainerName::from("catalog")];
        assert!(!remote.dev_follow);
        assert_eq!(
            remote.allowed_origins,
            ["https://catalog.example.test".to_string()]
        );
        assert!(!federation.exposes.contains_key(&ExposeKey::from("Button")));
        assert!(
            federation
                .exposes
                .contains_key(&ExposeKey::from("./Button"))
        );
        assert_eq!(
            federation.exposes[&ExposeKey::from("./Button")].shadow,
            ShadowMode::None
        );
        assert!(federation.exposes[&ExposeKey::from("./Button")].allow_global_css);
        assert_eq!(
            federation.exposes[&ExposeKey::from("./LegacyCard")].shadow,
            ShadowMode::Open
        );
        assert_eq!(federation.shared["react"].scope, "react18");
        assert!(!federation.shared["react"].fallback);
    }

    #[test]
    fn federation_config_rejects_disabled_populated_or_unnamed_contract() {
        let disabled = r#"
[federation]
name = "shell"
"#;
        assert!(
            toml::from_str::<Config>(disabled)
                .unwrap_err()
                .to_string()
                .contains("requires enabled=true")
        );

        let unnamed = r#"
[federation]
enabled = true
"#;
        assert!(
            toml::from_str::<Config>(unnamed)
                .unwrap_err()
                .to_string()
                .contains("container names must")
        );
    }

    #[test]
    fn federation_config_rejects_invalid_names_specifiers_and_urls() {
        let invalid = [
            (
                "container name",
                r#"
[federation]
enabled = true
name = "bad/name"
"#,
            ),
            (
                "remote name",
                r#"
[federation]
enabled = true
name = "shell"
[federation.remotes."bad/name"]
manifest_url = "https://catalog.example.test/wake-federation.json"
"#,
            ),
            (
                "manifest URL scheme",
                r#"
[federation]
enabled = true
name = "shell"
[federation.remotes.catalog]
manifest_url = "file:///tmp/wake-federation.json"
"#,
            ),
            (
                "manifest URL credentials",
                r#"
[federation]
enabled = true
name = "shell"
[federation.remotes.catalog]
manifest_url = "https://user@catalog.example.test/wake-federation.json"
"#,
            ),
            (
                "origin path",
                r#"
[federation]
enabled = true
name = "shell"
[federation.remotes.catalog]
manifest_url = "https://catalog.example.test/wake-federation.json"
allowed_origins = ["https://catalog.example.test/assets"]
"#,
            ),
            (
                "expose traversal",
                r#"
[federation]
enabled = true
name = "shell"
[federation.exposes."../Button"]
entry = "src/button.tsx"
"#,
            ),
            (
                "shared relative specifier",
                r#"
[federation]
enabled = true
name = "shell"
[federation.shared."./react"]
"#,
            ),
        ];

        for (case, source) in invalid {
            assert!(
                toml::from_str::<Config>(source).is_err(),
                "{case} should be rejected"
            );
        }
    }

    #[test]
    fn federation_config_enforces_react_expose_boundaries() {
        let invalid = [
            r#"
[federation]
enabled = true
name = "shell"
[federation.exposes.Button]
entry = "src/button.tsx"
mode = "host-rendered"
"#,
            r#"
[federation]
enabled = true
name = "shell"
[federation.exposes.Button]
entry = "src/button.tsx"
mode = "host-rendered"
scope = "react18"
shadow = "open"
"#,
            r#"
[federation]
enabled = true
name = "shell"
[federation.exposes.Button]
entry = "src/button.tsx"
mode = "isolated"
scope = "default"
"#,
            r#"
[federation]
enabled = true
name = "shell"
[federation.exposes.Button]
entry = "src/button.tsx"
mode = "react-component"
"#,
        ];

        for source in invalid {
            assert!(toml::from_str::<Config>(source).is_err());
        }
    }

    #[test]
    fn federation_config_enforces_host_rendered_react_coherence_only() {
        let incomplete = r#"
[federation]
enabled = true
name = "shell"

[federation.exposes.Button]
entry = "src/button.tsx"
mode = "host-rendered"
scope = "react18"

[federation.shared.react]
scope = "react18"
singleton = true
coherence_group = "react18"
owner = "shell"
"#;
        let error = toml::from_str::<Config>(incomplete)
            .unwrap_err()
            .to_string();
        assert!(error.contains("react-dom/client"), "{error}");

        let isolated = r#"
[federation]
enabled = true
name = "legacy"

[federation.exposes.Widget]
entry = "src/widget.tsx"
mode = "isolated"
scope = "react17"

[federation.shared.react]
scope = "react17"
"#;
        toml::from_str::<Config>(isolated).unwrap();
    }

    #[test]
    fn federation_global_css_opt_in_is_rejected_outside_host_rendered_mode() {
        for mode in ["generic", "isolated"] {
            let scope = if mode == "isolated" {
                "scope = \"react17\""
            } else {
                ""
            };
            let source = format!(
                r#"
[federation]
enabled = true
name = "shell"

[federation.exposes.Styles]
entry = "src/styles.ts"
mode = "{mode}"
{scope}
allow_global_css = true
"#
            );
            let error = toml::from_str::<Config>(&source).unwrap_err().to_string();
            assert!(error.contains("allowGlobalCss"), "{error}");
        }
    }

    #[test]
    fn federation_config_rejects_ambiguous_shared_and_unknown_fields() {
        let invalid = [
            r#"
[federation]
enabled = true
name = "shell"
[federation.shared.react]
strict = true
"#,
            r#"
[federation]
enabled = true
name = "shell"
[federation.shared.react]
required_version = "^18"
singleton = false
coherence_group = "react18"
"#,
            r#"
[federation]
enabled = true
name = "shell"
strategy = "version-first"
"#,
        ];

        for source in invalid {
            assert!(toml::from_str::<Config>(source).is_err());
        }
    }

    #[test]
    fn test_config_defaults_are_wake_owned_and_react_focused() {
        let test = Config::default().test;
        assert_eq!(test.environment, TestEnvironment::Auto);
        assert_eq!(
            test.include,
            ["**/*.{test,spec}.{js,mjs,cjs,jsx,ts,mts,cts,tsx}"]
        );
        assert_eq!(
            test.browser_include,
            ["**/*.browser.{test,spec}.{js,mjs,cjs,jsx,ts,mts,cts,tsx}"]
        );
        assert_eq!(test.exclude, ["**/node_modules/**", "**/dist/**"]);
        assert!(test.setup.is_empty());
        assert_eq!(test.timeout_ms, 5_000);
        assert_eq!(test.workers, WorkerCount::Auto);
        assert!(!test.forbid_only);
        assert_eq!(test.leaks, TestLeakPolicy::Warn);
        assert!(!test.react.strict_mode);
        assert!(test.react.cleanup);
        assert_eq!(test.react.act_warnings, TestDiagnosticPolicy::Error);
        assert_eq!(test.react.test_id_attribute, "data-testid");
        assert!(test.browser.headless);
        assert!(test.browser.sandbox);
        assert_eq!(test.browser.viewport.width, 1_280);
        assert_eq!(test.browser.viewport.height, 720);
        assert_eq!(test.browser.viewport.device_scale_factor, 1.0);
        assert_eq!(test.browser.locale, "en-US");
        assert_eq!(test.browser.timezone, "UTC");
        assert_eq!(test.browser.color_scheme, TestColorScheme::Light);
        assert_eq!(test.network.mode, TestNetworkMode::Deny);
        assert!(test.network.allow_hosts.is_empty());
        assert_eq!(test.snapshot.directory, "__snapshots__");
        assert_eq!(test.snapshot.screenshot_directory, "__screenshots__");
        assert!(!test.coverage.enabled);
        assert_eq!(test.coverage.reporters, [TestCoverageReporter::Text]);
        assert_eq!(test.coverage.threshold, TestCoverageThreshold::default());
        assert!(test.coverage.per_file.is_empty());
        assert!(test.projects.is_empty());
    }

    #[test]
    fn parses_the_complete_wake_test_contract() {
        let config: Config = toml::from_str(
            r#"
                [test]
                environment = "dom"
                include = ["src/**/*.test.tsx"]
                browser_include = ["src/**/*.browser.test.tsx"]
                exclude = ["**/generated/**"]
                setup = ["test/setup.ts"]
                timeout_ms = 8000
                workers = "75%"
                forbid_only = true
                leaks = "error"

                [test.react]
                strict_mode = true
                cleanup = false
                act_warnings = "warn"
                test_id_attribute = "data-wake-id"

                [test.browser]
                headless = false
                sandbox = false
                viewport = { width = 1440, height = 900, device_scale_factor = 2.0 }
                locale = "zh-CN"
                timezone = "Asia/Singapore"
                color_scheme = "dark"

                [test.network]
                mode = "allow"
                allow_hosts = ["api.example.test"]

                [test.snapshot]
                directory = "snapshots"
                screenshot_directory = "screenshots"

                [test.coverage]
                enabled = true
                reporters = ["text", "json", "lcov", "html"]

                [test.coverage.threshold]
                lines = 85
                functions = 80.5
                blocks = 75

                [[test.coverage.per_file]]
                pattern = "src/components/**"
                lines = 95
                functions = 90
                blocks = 85.5

                [[test.projects]]
                name = "client"
                root = "packages/client"
                environment = "browser"
            "#,
        )
        .unwrap();

        let test = config.test;
        assert_eq!(test.environment, TestEnvironment::Dom);
        assert_eq!(test.workers, WorkerCount::Percent(75));
        assert!(test.forbid_only);
        assert_eq!(test.leaks, TestLeakPolicy::Error);
        assert!(test.react.strict_mode);
        assert_eq!(test.react.act_warnings, TestDiagnosticPolicy::Warn);
        assert_eq!(test.browser.viewport.device_scale_factor, 2.0);
        assert_eq!(test.browser.color_scheme, TestColorScheme::Dark);
        assert_eq!(test.network.mode, TestNetworkMode::Allow);
        assert_eq!(
            test.coverage.reporters,
            [
                TestCoverageReporter::Text,
                TestCoverageReporter::Json,
                TestCoverageReporter::Lcov,
                TestCoverageReporter::Html,
            ]
        );
        assert_eq!(test.coverage.threshold.lines, Some(85.0));
        assert_eq!(test.coverage.threshold.functions, Some(80.5));
        assert_eq!(test.coverage.threshold.blocks, Some(75.0));
        assert_eq!(test.coverage.per_file.len(), 1);
        assert_eq!(test.coverage.per_file[0].pattern, "src/components/**");
        assert_eq!(test.coverage.per_file[0].lines, Some(95.0));
        assert_eq!(test.coverage.per_file[0].functions, Some(90.0));
        assert_eq!(test.coverage.per_file[0].blocks, Some(85.5));
        assert_eq!(test.projects.len(), 1);
        assert_eq!(test.projects[0].name, "client");
        assert_eq!(test.projects[0].root, "packages/client");
        assert_eq!(test.projects[0].environment, TestEnvironment::Browser);
    }

    #[test]
    fn test_config_rejects_jest_and_unknown_fields_at_every_level() {
        for source in [
            "[test]\ntestMatch = []\n",
            "[test]\ntest_match = []\n",
            "[test.react]\ncleanup = true\nunknown = true\n",
            "[test.browser.viewport]\nwidth = 1\nheight = 1\ndevice_scale_factor = 1.0\ndpr = 1.0\n",
            "[test.coverage]\nenabled = false\nprovider = \"v8\"\n",
            "[test.coverage.threshold]\nlines = 100.01\n",
            "[test.coverage.threshold]\nfunctions = -0.1\n",
            "[[test.coverage.per_file]]\npattern = \"  \"\nlines = 80\n",
            "[test.coverage.threshold]\nstatements = 80\n",
            "[[test.projects]]\nname = \"client\"\nroot = \".\"\nenvironment = \"auto\"\nrunner = \"custom\"\n",
        ] {
            assert!(
                toml::from_str::<Config>(source).is_err(),
                "unexpectedly accepted {source:?}"
            );
        }
    }

    #[test]
    fn test_project_names_must_be_non_empty_and_unique() {
        for source in [
            "[[test.projects]]\nname = \"\"\n",
            "[[test.projects]]\nname = \"   \"\n",
            concat!(
                "[[test.projects]]\nname = \"client\"\nroot = \"packages/client\"\n",
                "[[test.projects]]\nname = \"client\"\nroot = \"packages/other\"\n",
            ),
        ] {
            let error = toml::from_str::<Config>(source).unwrap_err().to_string();
            assert!(error.contains("test project name"), "{error}");
        }
    }

    #[test]
    fn test_workers_reject_invalid_or_zero_values() {
        for value in ["0", "\"0%\"", "\"101%\"", "\"half\""] {
            let source = format!("[test]\nworkers = {value}\n");
            assert!(toml::from_str::<Config>(&source).is_err());
        }
        let count: Config = toml::from_str("[test]\nworkers = 3\n").unwrap();
        assert_eq!(count.test.workers, WorkerCount::Count(3));
    }

    #[test]
    fn parses_full_shape() {
        let src = r#"
            root_dir = "."
            public_path = "/app/"
            browserslist = ["chrome 80", "firefox 74"]

            [browserslist_options]
            mobile_to_desktop = true

            [typescript]
            enabled = true
            only_remove_type_imports = true

            [react]
            enabled = true
            jsx_import_source = "preact"

            [transforms]
            include = ["transform-arrow-functions"]
            exclude = ["transform-template-literals"]

            [alias]
            "@" = "app"
            "~" = "lib"

            [[component_scan]]
            namespace = "pages"
            cwd = "src/pages"
            generate_source = true
            include = "\\.page\\.tsx$"

            [dev_server]
            server = "https"
            port = 3000
            open = true

            [[dev_server.proxy]]
            context = ["/api"]
            target = "http://localhost:8080"
            ws = true

            [html]
            entry = "src/main.tsx"

            [hooks]
            bootstrap_path = "src/bootstrap.tsx"

            [define]
            "process.env.API" = "\"/api\""
        "#;
        let c: Config = toml::from_str(src).unwrap();
        assert_eq!(c.public_path(), "/app/");
        assert_eq!(c.browserslist, ["chrome 80", "firefox 74"]);
        assert!(c.browserslist_options.mobile_to_desktop);
        assert!(c.typescript.enabled);
        assert!(c.typescript.only_remove_type_imports);
        assert_eq!(c.react.jsx_import_source, "preact");
        assert_eq!(
            c.transforms.include,
            ["transform-arrow-functions".to_string()]
        );
        assert_eq!(
            c.transforms.exclude,
            ["transform-template-literals".to_string()]
        );
        assert_eq!(c.alias.get("@").map(String::as_str), Some("app"));
        assert_eq!(c.component_scan.len(), 1);
        assert_eq!(c.component_scan[0].namespace, "pages");
        assert!(c.component_scan[0].generate_source);
        assert_eq!(c.dev_server.server.as_deref(), Some("https"));
        assert_eq!(c.dev_server.port, Some(3000));
        assert!(c.dev_server.open);
        assert_eq!(c.dev_server.proxy.len(), 1);
        assert!(c.dev_server.proxy[0].ws);
        assert_eq!(c.html.entry.as_deref(), Some("src/main.tsx"));
        assert_eq!(c.hooks.bootstrap_path.as_deref(), Some("src/bootstrap.tsx"));
        assert_eq!(
            c.define.get("process.env.API").map(String::as_str),
            Some("\"/api\"")
        );
    }

    #[test]
    fn explicit_browserslist_resolves_and_sorts() {
        let c: Config =
            toml::from_str(r#"browserslist = ["firefox 74", "chrome 80", "chrome 80"]"#).unwrap();
        let targets = c.resolve_browser_targets(Path::new("/unused")).unwrap();
        assert_eq!(
            targets,
            vec![
                BrowserTarget {
                    name: "chrome".into(),
                    version: "80".into(),
                },
                BrowserTarget {
                    name: "firefox".into(),
                    version: "74".into(),
                },
            ]
        );
    }

    #[test]
    fn zero_config_resolves_the_fixed_modern_browser_baseline() {
        assert_eq!(
            DEFAULT_BROWSER_TARGETS,
            [
                ("chrome", "120"),
                ("edge", "120"),
                ("firefox", "121"),
                ("safari", "17.2"),
                ("ios_saf", "17.2"),
            ]
        );

        let root = std::env::temp_dir().join(format!(
            "wake_config_missing_default_baseline_{}",
            std::process::id()
        ));
        let targets = Config::default().resolve_browser_targets(&root).unwrap();
        assert_eq!(
            targets,
            DEFAULT_BROWSER_TARGETS
                .into_iter()
                .map(|(name, version)| BrowserTarget {
                    name: name.to_string(),
                    version: version.to_string(),
                })
                .collect::<Vec<_>>(),
            "zero config must not inherit or expand dynamic Browserslist defaults"
        );
    }

    #[test]
    fn browserslist_discovery_precedence_remains_explicit_rc_package_then_baseline() {
        let root = std::env::temp_dir().join(format!(
            "wake_config_browserslist_precedence_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{"browserslist":["chrome 81"]}"#,
        )
        .unwrap();
        std::fs::write(root.join(".browserslistrc"), "firefox 82\n").unwrap();

        let explicit: Config = toml::from_str(r#"browserslist = ["chrome 80"]"#).unwrap();
        assert_eq!(
            explicit.resolve_browser_targets(&root).unwrap(),
            [BrowserTarget {
                name: "chrome".into(),
                version: "80".into(),
            }]
        );

        let discovered = Config::default();
        assert_eq!(
            discovered.resolve_browser_targets(&root).unwrap(),
            [BrowserTarget {
                name: "firefox".into(),
                version: "82".into(),
            }]
        );

        std::fs::remove_file(root.join(".browserslistrc")).unwrap();
        assert_eq!(
            discovered.resolve_browser_targets(&root).unwrap(),
            [BrowserTarget {
                name: "chrome".into(),
                version: "81".into(),
            }]
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn docs_config_has_stable_defaults_and_parses_overrides() {
        let default = Config::default().docs;
        assert_eq!(default.source_dir, "docs");
        assert_eq!(default.base_path, "/");
        assert_eq!(default.default_theme, "system");
        assert_eq!(default.locale, "zh-CN");
        assert_eq!(default.accent_color, None);

        let config: Config = toml::from_str(
            r##"
            [docs]
            source_dir = "website"
            title = "Crab UI"
            locale = "en-US"
            base_path = "/crab/"
            preview = "docs/preview.tsx"
            theme_css = "docs/theme.css"
            default_theme = "dark"
            accent_color = "#7c3aed"
        "##,
        )
        .unwrap();
        assert_eq!(config.docs.source_dir, "website");
        assert_eq!(config.docs.title, "Crab UI");
        assert_eq!(config.docs.locale, "en-US");
        assert_eq!(config.docs.preview.as_deref(), Some("docs/preview.tsx"));
        assert_eq!(config.docs.default_theme, "dark");
        assert_eq!(config.docs.accent_color.as_deref(), Some("#7c3aed"));
    }

    #[test]
    fn docs_workspace_configuration_is_strict_and_defaults_to_lazy_embedded_components() {
        let config: Config = toml::from_str(
            r#"
            [[docs.workspace]]
            root = "../components"
            include = ["rc-*"]
            base_path = "/components/{name}/workbench/"
        "#,
        )
        .unwrap();
        let workspace = &config.docs.workspace[0];
        assert_eq!(workspace.root, "../components");
        assert_eq!(workspace.include, ["rc-*"]);
        assert_eq!(workspace.mode, DocsWorkspaceMode::Components);
        assert_eq!(workspace.presentation, DocsWorkspacePresentation::Embedded);
        assert_eq!(workspace.dev_loading, DocsWorkspaceDevLoading::Lazy);

        let eager: Config = toml::from_str(
            r#"
            [[docs.workspace]]
            root = "../components"
            include = ["rc-*"]
            base_path = "/components/{name}/workbench/"
            presentation = "standalone"
            dev_loading = "eager"
        "#,
        )
        .unwrap();
        assert_eq!(
            eager.docs.workspace[0].presentation,
            DocsWorkspacePresentation::Standalone
        );
        assert_eq!(
            eager.docs.workspace[0].dev_loading,
            DocsWorkspaceDevLoading::Eager
        );

        assert!(
            toml::from_str::<Config>(
                r#"
                [[docs.workspace]]
                root = "../components"
                include = ["*"]
                base_path = "/components/{name}/"
                typo = true
            "#
            )
            .is_err()
        );
    }

    #[test]
    fn resolver_aliases_defaults_and_override() {
        let src = r#"
            [alias]
            "@" = "app"
            "~" = "lib"
        "#;
        let c: Config = toml::from_str(src).unwrap();
        let root = Path::new("/proj");
        let aliases = c.resolver_aliases(root);
        // @ 被配置覆盖为 root/app；@@ 默认 root；~ 新增 root/lib。
        let get = |k: &str| {
            aliases
                .iter()
                .find(|(kk, _)| kk == k)
                .map(|(_, p)| p.clone())
        };
        assert_eq!(get("@"), Some(PathBuf::from("/proj/app")));
        assert_eq!(get("@@"), Some(PathBuf::from("/proj")));
        assert_eq!(get("~"), Some(PathBuf::from("/proj/lib")));
    }

    #[test]
    fn default_alias_when_no_config() {
        let c = Config::default();
        let aliases = c.resolver_aliases(Path::new("/x"));
        assert_eq!(aliases.len(), 2);
        assert_eq!(aliases[0], ("@".to_string(), PathBuf::from("/x/src")));
        assert_eq!(aliases[1], ("@@".to_string(), PathBuf::from("/x")));
    }
}

#[cfg(test)]
mod root_dir_tests {
    use super::*;

    #[test]
    fn resolved_root_defaults_to_config_dir() {
        let c = Config::default();
        assert_eq!(c.resolved_root(Path::new("/proj")), PathBuf::from("/proj"));
    }

    #[test]
    fn resolved_root_is_relative_to_config_dir() {
        let c: Config = toml::from_str("root_dir = \"app\"").unwrap();
        assert_eq!(
            c.resolved_root(Path::new("/proj")),
            PathBuf::from("/proj/app")
        );
    }

    #[test]
    fn absolute_root_dir_wins() {
        let c: Config = toml::from_str("root_dir = \"/elsewhere\"").unwrap();
        assert_eq!(
            c.resolved_root(Path::new("/proj")),
            PathBuf::from("/elsewhere")
        );
    }

    /// `root_dir` 必须真正改变别名基准——这是它此前作为「死字段」的核心症状。
    #[test]
    fn root_dir_shifts_alias_base() {
        let c: Config = toml::from_str("root_dir = \"app\"").unwrap();
        let root = c.resolved_root(Path::new("/proj"));
        let aliases = c.resolver_aliases(&root);
        let at = aliases
            .iter()
            .find(|(k, _)| k == "@")
            .expect("应有默认 @ 别名");
        assert_eq!(
            at.1,
            PathBuf::from("/proj/app/src"),
            "@ 应指向 root_dir/src"
        );
        let at2 = aliases
            .iter()
            .find(|(k, _)| k == "@@")
            .expect("应有默认 @@ 别名");
        assert_eq!(at2.1, PathBuf::from("/proj/app"), "@@ 应指向 root_dir");
    }
}
