//! # wake_config — 声明式项目配置（`wake.config.toml`）
//!
//! crustify 用 `unconfig` **执行** `.crustify.ts`（含 JS 逻辑 / 正则 / `mods` 函数）；wake 是
//! 纯 Rust、无 JS 运行时，无法执行 TS 配置。故对齐方案是**声明式 TOML**（CRUSTIFY-PARITY 决策①）：
//! 字段一一对应 crustify `Config`，正则类字段以字符串表达、`mods` 折叠为 `[hooks]` 声明项（决策④）。
//!
//! 加载：[`load`] 从项目根读 `wake.config.toml`（不存在 → [`Config::default`]，保持零配置可跑）。
//! 根发现：[`find_root`] 从入口向上找含 `wake.config.toml` / `package.json` 的目录。
//!
//! 本 crate **只做数据**：不依赖 wake_resolver / wake_bundler，避免依赖倒挂。别名表由
//! [`Config::resolver_aliases`] 产出 `(前缀, 绝对路径)`，由 CLI 桥接进 `ResolveOptions`。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// 配置文件名（固定）。
pub const CONFIG_FILE: &str = "wake.config.toml";

/// 项目配置（对齐 crustify `Config`）。所有字段可省，缺省即零配置默认。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// 项目根（相对配置文件所在目录）。缺省 = 配置所在目录 / cwd。
    pub root_dir: Option<String>,
    /// 静态资源公共路径，部署到子路径时设置（如 `/app/`）。缺省 `/`。
    pub public_path: Option<String>,
    /// 路径别名（前缀 → 相对根的路径）。CLI 会叠加默认 `@`→`src`、`@@`→根。
    pub alias: BTreeMap<String, String>,
    /// 组件自动扫描规则（M2）。
    pub component_scan: Vec<ComponentScan>,
    /// 开发服务器配置（M3）。
    pub dev_server: DevServer,
    /// HTML / 入口生成（M2）。
    pub html: Html,
    /// 声明式 hook（替代 crustify `mods`，决策④）。
    pub hooks: Hooks,
    /// 全局常量替换（`process.env.NODE_ENV` 等）。dev/prod 由 CLI 注入默认值。
    pub define: BTreeMap<String, String>,
}

/// 一条组件自动扫描规则（对齐 crustify `ComponentScanRule`）。
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

/// 开发服务器配置（对齐 crustify `DevServer`）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
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

/// 代理配置（对齐 crustify `Proxy`）。
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

/// 声明式 hook（替代 crustify `Modification` 的 JS 函数，决策④）。
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
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::Io(p, e) => write!(f, "无法读取配置 `{}`：{e}", p.display()),
            ConfigError::Parse(p, e) => write!(f, "配置 `{}` 解析失败：{e}", p.display()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_config_is_default() {
        let c: Config = toml::from_str("").unwrap();
        assert!(c.alias.is_empty());
        assert_eq!(c.public_path(), "/");
        assert!(c.component_scan.is_empty());
    }

    #[test]
    fn parses_full_shape() {
        let src = r#"
            root_dir = "."
            public_path = "/app/"

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
