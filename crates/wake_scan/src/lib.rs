//! # wake_scan — 组件自动扫描（WAKE-COMPATIBILITY §M2）
//!
//! 保持既定行为 `AutoScanWebpackPlugin`：递归扫描一个目录，为每个文件生成**懒加载**条目
//! （动态 `import()`），并提取 **TOML frontmatter**——`.ts/.tsx/.js/.jsx` 取文件首个块注释、
//! `.md/.mdx` 取 `+++ … +++` frontmatter。产出一段 JS 模块源码，经 `@@@/{namespace}` 别名被入口引用：
//!
//! ```ts
//! const ___src_pages_home_tsx = import("@@/src/pages/home.tsx");
//! const components = [
//!   { name: "___src_pages_home_tsx", component: ___src_pages_home_tsx,
//!     path: "/src/pages/home.tsx", frontmatter: {"title":"Home"}, source: null },
//! ];
//! export default components;
//! ```
//!
//! `import("@@/…")` 中的 `@@` 是项目根别名（wake_resolver 解析），动态 import 让每个组件独立成
//! async chunk（开启代码分割时）。本 crate **只做数据 + 字符串**：不依赖任何 wake crate。

use std::path::{Path, PathBuf};

use regex::Regex;

/// 扫描规则（路径已解析为绝对）。保持既定行为 `ComponentScanRule`。
pub struct ScanRule<'a> {
    /// 命名空间（经 `@@@/{namespace}` 导入）。
    pub namespace: &'a str,
    /// 要扫描的目录（绝对）。
    pub scan_dir: &'a Path,
    /// 项目根（绝对）——生成 `@@/relpath` 与 `path` 字段的基准。
    pub root: &'a Path,
    /// 是否在条目中内联源码字符串（默认否 → `source: null`）。
    pub generate_source: bool,
    /// 文件包含正则（对完整正斜杠路径匹配）。`None` = 不过滤。
    pub include: Option<&'a str>,
    /// 文件排除正则。命中则剔除。`None` = 不排除。
    pub exclude: Option<&'a str>,
}

/// 扫描失败。
#[derive(Debug)]
pub enum ScanError {
    /// include/exclude 正则非法。
    Regex(String),
}

impl std::fmt::Display for ScanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanError::Regex(e) => write!(f, "组件扫描正则非法：{e}"),
        }
    }
}

impl std::error::Error for ScanError {}

/// 一个扫描条目（内部，供模块生成）。
struct ScanEntry {
    /// 消毒后的合法标识符（导入变量名）。
    name: String,
    /// 动态 import 说明符 `@@/relpath`。
    import_url: String,
    /// `path` 字段：`/relpath`（前导斜杠）。
    rel_path: String,
    /// frontmatter 的 JSON 文本（无则 `"null"`）。
    frontmatter_json: String,
    /// 内联源码（`generate_source` 时 `Some`）。
    source: Option<String>,
}

/// 扫描 `rule.scan_dir` 并生成 `@@@/{namespace}` 懒加载模块源码。
///
/// 目录不存在 → 生成空组件列表（不报错，保持既定行为 的 ENOENT 处理）。文件读取失败者跳过。
/// 输出确定（文件按路径排序），便于缓存与快照。
pub fn scan(rule: &ScanRule) -> Result<String, ScanError> {
    let include = compile(rule.include)?;
    let exclude = compile(rule.exclude)?;

    let mut files: Vec<PathBuf> = Vec::new();
    walk(rule.scan_dir, &include, &exclude, &mut files);
    files.sort();

    let mut entries: Vec<ScanEntry> = Vec::with_capacity(files.len());
    for file in &files {
        let source = std::fs::read_to_string(file).unwrap_or_default();
        let rel = rel_path(rule.root, file);
        let import_url = format!("@@/{rel}");
        let name = sanitize(&import_url);
        let frontmatter_json = extract_frontmatter(file, &source)
            .and_then(|v| serde_json::to_string(&v).ok())
            .unwrap_or_else(|| "null".to_string());
        entries.push(ScanEntry {
            name,
            import_url,
            rel_path: format!("/{rel}"),
            frontmatter_json,
            source: rule.generate_source.then_some(source),
        });
    }

    Ok(generate_module(&entries))
}

/// 编译可选正则。
fn compile(pat: Option<&str>) -> Result<Option<Regex>, ScanError> {
    match pat {
        Some(p) => Regex::new(p)
            .map(Some)
            .map_err(|e| ScanError::Regex(e.to_string())),
        None => Ok(None),
    }
}

/// 递归收集文件：include（若设）须匹配、exclude（若设）不得匹配。对完整正斜杠路径判定。
fn walk(dir: &Path, include: &Option<Regex>, exclude: &Option<Regex>, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return; // 目录不存在 / 不可读 → 空（保持既定行为 ENOENT）
    };
    for ent in rd.flatten() {
        let path = ent.path();
        let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            walk(&path, include, exclude, out);
        } else {
            let s = to_slash(&path);
            if include.as_ref().is_some_and(|re| !re.is_match(&s)) {
                continue;
            }
            if exclude.as_ref().is_some_and(|re| re.is_match(&s)) {
                continue;
            }
            out.push(path);
        }
    }
}

/// 项目根到文件的相对路径（正斜杠）。文件不在根下时回退文件名。
fn rel_path(root: &Path, file: &Path) -> String {
    match file.strip_prefix(root) {
        Ok(rel) => to_slash(rel),
        Err(_) => file
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

/// 路径 → 正斜杠字符串（Windows `\` → `/`）。
fn to_slash(p: &Path) -> String {
    let value = p.to_string_lossy().replace('\\', "/");
    if let Some(rest) = value.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = value.strip_prefix("//?/") {
        rest.to_string()
    } else {
        value
    }
}

/// 消毒为合法 JS 标识符：非字母数字 → `_`（保持既定行为 `replace(/[^a-zA-Z0-9]/g, "_")`）。
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// 按扩展名提取 frontmatter：`.ts/.tsx/.js/.jsx/.mts/.cts` 取首个块注释、`.md/.mdx` 取 `+++` frontmatter。
pub fn extract_frontmatter(path: &Path, source: &str) -> Option<toml::Value> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("ts" | "tsx" | "js" | "jsx" | "mts" | "cts") => {
            let body = first_block_comment(source)?;
            toml::from_str(&body).ok()
        }
        Some("md" | "mdx") => {
            let body = toml_frontmatter(source)?;
            toml::from_str(&body).ok()
        }
        _ => None,
    }
}

/// 取源码首个块注释 `/* … */` 的内容（跳过前导空白与 `//` 行注释），逐行剥离前导 `*` 装饰。
/// 保持既定行为 `getTypeScriptComment`。返回适合 TOML 解析的文本；无块注释 → `None`。
fn first_block_comment(src: &str) -> Option<String> {
    let bytes = src.as_bytes();
    let mut i = 0;
    let n = bytes.len();
    loop {
        // 跳过空白。
        while i < n && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i + 1 >= n {
            return None;
        }
        if bytes[i] == b'/' && bytes[i + 1] == b'*' {
            // 找到块注释起点。
            let start = i + 2;
            let end = src[start..].find("*/")? + start;
            let inner = &src[start..end];
            // 逐行剥离前导空白 + `*`。
            let cleaned: Vec<String> = inner
                .lines()
                .map(|line| {
                    let t = line.trim_start();
                    let t = t.strip_prefix('*').unwrap_or(t);
                    t.trim().to_string()
                })
                .collect();
            return Some(cleaned.join("\n").trim().to_string());
        }
        if bytes[i] == b'/' && bytes[i + 1] == b'/' {
            // 行注释：跳到行尾后继续。
            let off = src[i..].find('\n')?;
            i += off + 1;
            continue;
        }
        // 遇到非注释、非空白 token → 首块注释必须在最前，放弃。
        return None;
    }
}

/// 取 `.md/.mdx` 的 `+++ … +++` TOML frontmatter（对齐 remark-frontmatter 'toml'）。
fn toml_frontmatter(src: &str) -> Option<String> {
    let s = src.strip_prefix('\u{feff}').unwrap_or(src);
    let s = s.trim_start_matches(['\r', '\n']);
    let rest = s.strip_prefix("+++")?;
    // 跳过 `+++` 所在行剩余到换行。
    let rest = rest.trim_start_matches('\r');
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let end = rest.find("\n+++")?;
    Some(rest[..end].to_string())
}

/// 渲染懒加载模块源码。
fn generate_module(entries: &[ScanEntry]) -> String {
    let mut out = String::new();
    // 逐条 `const <name> = import("<url>");`
    for e in entries {
        out.push_str("const ");
        out.push_str(&e.name);
        out.push_str(" = import(");
        push_js_string(&mut out, &e.import_url);
        out.push_str(");\n");
    }
    out.push_str("\nconst components = [\n");
    for e in entries {
        out.push_str("  { name: ");
        push_js_string(&mut out, &e.name);
        out.push_str(", component: ");
        out.push_str(&e.name);
        out.push_str(", path: ");
        push_js_string(&mut out, &e.rel_path);
        out.push_str(", frontmatter: ");
        out.push_str(&e.frontmatter_json);
        out.push_str(", source: ");
        match &e.source {
            Some(s) => push_js_string(&mut out, s),
            None => out.push_str("null"),
        }
        out.push_str(" },\n");
    }
    out.push_str("];\n\nexport default components;\n");
    out
}

/// 追加双引号 JS 字符串字面量（转义控制字符与行分隔符）。
fn push_js_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_paths_strip_windows_verbatim_prefixes() {
        assert_eq!(
            to_slash(Path::new(r"\\?\C:\proj\src\button.tsx")),
            "C:/proj/src/button.tsx"
        );
        assert_eq!(
            to_slash(Path::new(r"\\?\UNC\server\share\src\button.tsx")),
            "//server/share/src/button.tsx"
        );
    }

    #[test]
    fn sanitize_import_name() {
        assert_eq!(sanitize("@@/src/pages/home.tsx"), "___src_pages_home_tsx");
        assert_eq!(sanitize("@@/a-b.c"), "___a_b_c");
    }

    #[test]
    fn ts_first_block_comment_toml() {
        let src = "/*\n index = true\n title = \"Home\"\n */\nexport default 1;";
        let fm = extract_frontmatter(Path::new("x.tsx"), src).unwrap();
        assert_eq!(fm.get("index").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(fm.get("title").and_then(|v| v.as_str()), Some("Home"));
    }

    #[test]
    fn ts_line_comment_before_block() {
        // 首块注释前允许 `//` 行注释与空白。
        let src = "// banner\n\n/* order = 3 */\nexport const x = 1;";
        let fm = extract_frontmatter(Path::new("x.ts"), src).unwrap();
        assert_eq!(fm.get("order").and_then(|v| v.as_integer()), Some(3));
    }

    #[test]
    fn ts_no_comment_is_none() {
        assert!(extract_frontmatter(Path::new("x.ts"), "export const x = 1;").is_none());
        // 代码在前、注释在后 → 不算 frontmatter。
        assert!(extract_frontmatter(Path::new("x.ts"), "const a=1;/* k=1 */").is_none());
    }

    #[test]
    fn md_toml_frontmatter() {
        let src = "+++\ntitle = \"Post\"\ndraft = false\n+++\n# Heading\n";
        let fm = extract_frontmatter(Path::new("p.md"), src).unwrap();
        assert_eq!(fm.get("title").and_then(|v| v.as_str()), Some("Post"));
        assert_eq!(fm.get("draft").and_then(|v| v.as_bool()), Some(false));
    }

    #[test]
    fn md_without_frontmatter_is_none() {
        assert!(extract_frontmatter(Path::new("p.md"), "# just heading\n").is_none());
    }

    #[test]
    fn generate_module_shape() {
        let entries = vec![
            ScanEntry {
                name: "___a_tsx".to_string(),
                import_url: "@@/a.tsx".to_string(),
                rel_path: "/a.tsx".to_string(),
                frontmatter_json: "{\"index\":true}".to_string(),
                source: None,
            },
            ScanEntry {
                name: "___b_tsx".to_string(),
                import_url: "@@/b.tsx".to_string(),
                rel_path: "/b.tsx".to_string(),
                frontmatter_json: "null".to_string(),
                source: Some("hi".to_string()),
            },
        ];
        let m = generate_module(&entries);
        assert!(m.contains("const ___a_tsx = import(\"@@/a.tsx\");"));
        assert!(m.contains("component: ___a_tsx"));
        assert!(m.contains("path: \"/a.tsx\""));
        assert!(m.contains("frontmatter: {\"index\":true}"));
        assert!(m.contains("source: null"));
        assert!(m.contains("source: \"hi\""));
        assert!(m.trim_end().ends_with("export default components;"));
    }
}
