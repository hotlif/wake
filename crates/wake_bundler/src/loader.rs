//! # 非 JS 资源 loader —— 把 CSS / JSON / 静态资源转成 JS 模块源码
//!
//! DESIGN §8：CSS 与静态资源不是 JS，但打包器只认 JS 模块。策略与 css-loader / Vite 一致：
//! **把它们「翻译」成一段 JS 模块源码**，再走完全相同的 parse → link → codegen 管线——
//! 引擎、codegen、linker 零改动。CSS 的 `@import` 翻译成 ESM `import`（进模块图去重排序），
//! 样式经 `<style>` 注入（dev 形态，可 HMR）。
//!
//! Phase 6.2 dev 切片：`import './x.css'` 端到端可用。prod 抽取 `.css`（6.2 后半）后置。

use std::path::Path;

use wake_common::FileSystem;
use wake_ecma_ast::SourceType;

/// 加载选项（prod/dev 差异，CRUSTIFY-PARITY §M3）。
pub(crate) struct LoadOptions {
    /// prod CSS 抽取：CSS 模块不再运行时注入 `<style>`，CSS 文本经 [`Loaded::css`] 带出聚合为 `.css`。
    pub extract_css: bool,
    /// 资源内联字节上限：`> limit` 的资源写为独立文件（模块导出其 URL）；`<= limit` 内联 base64。
    /// `usize::MAX` = 全内联（dev / 默认，行为与阈值接入前一致）。
    pub asset_inline_limit: usize,
    /// 资源 URL 的 `publicPath` 前缀（如 `/` 或 `/app/`）。
    pub public_path: String,
}

impl Default for LoadOptions {
    fn default() -> LoadOptions {
        LoadOptions {
            extract_css: false,
            asset_inline_limit: usize::MAX,
            public_path: "/".to_string(),
        }
    }
}

/// 一次加载结果：JS 模块源码 + 源类型 + 可选带外产物。
pub(crate) struct Loaded {
    pub source: String,
    pub source_type: SourceType,
    /// 超阈值资源独立产物：`(文件名, 字节)`。
    pub asset: Option<(String, Vec<u8>)>,
    /// prod 抽取的 CSS 文本（供聚合为 `.css`）。
    pub css: Option<String>,
}

/// 读取并「加载」一个模块为 JS 源码 + 源类型（可能附带带外产物）。
///
/// 非 JS 资源在此翻译为等价 JS 模块（DESIGN §8），JS/TS/JSX 原样返回并据扩展名定源类型。
/// 二进制资源（图片/字体）走 `read`（字节）：超阈值写独立文件、模块导出 URL；否则内联 base64。
/// CSS 在 `extract_css` 下不注入 `<style>`、CSS 文本带出（供聚合 `.css`）。
pub(crate) fn load_source(
    fs: &dyn FileSystem,
    path: &Path,
    opts: &LoadOptions,
) -> std::io::Result<Loaded> {
    if is_asset_path(path) {
        let bytes = fs.read(path)?;
        if bytes.len() > opts.asset_inline_limit {
            // 超阈值：独立产物 + 模块导出 URL（`publicPath` + hash 文件名）。
            let name = asset_file_name(path, &bytes);
            let url = join_public_path(&opts.public_path, &name);
            let mut source = String::from("export default ");
            push_js_string(&mut source, &url);
            source.push_str(";\n");
            Ok(Loaded {
                source,
                source_type: SourceType::Module,
                asset: Some((name, bytes)),
                css: None,
            })
        } else {
            Ok(Loaded {
                source: asset_to_js_module(path, &bytes),
                source_type: SourceType::Module,
                asset: None,
                css: None,
            })
        }
    } else {
        let text = fs.read_to_string(path)?;
        if is_css_path(path) {
            if opts.extract_css {
                // prod 抽取：JS 模块不注入 `<style>`，CSS 文本带出。
                let (source, css) = if is_css_module_path(path) {
                    css_module_extract(&text, path)
                } else {
                    css_extract(&text)
                };
                Ok(Loaded {
                    source,
                    source_type: SourceType::Module,
                    asset: None,
                    css: Some(css),
                })
            } else {
                let source = if is_css_module_path(path) {
                    css_module_to_js(&text, path)
                } else {
                    css_to_js_module(&text)
                };
                Ok(Loaded {
                    source,
                    source_type: SourceType::Module,
                    asset: None,
                    css: None,
                })
            }
        } else if is_json_path(path) {
            Ok(Loaded {
                source: json_to_js_module(&text),
                source_type: SourceType::Module,
                asset: None,
                css: None,
            })
        } else if is_raw_path(path) {
            Ok(Loaded {
                source: raw_to_js_module(&text),
                source_type: SourceType::Module,
                asset: None,
                css: None,
            })
        } else {
            Ok(Loaded {
                source: text,
                source_type: source_type_for(path),
                asset: None,
                css: None,
            })
        }
    }
}

/// 超阈值资源的写盘文件名：`<stem>.<hash8>.<ext>`（内容 hash）。
fn asset_file_name(path: &Path, bytes: &[u8]) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("asset");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("bin");
    format!(
        "{stem}.{:08x}.{ext}",
        xxhash_rust::xxh3::xxh3_64(bytes) as u32
    )
}

/// 拼接 `publicPath` 与文件名（规范化斜杠）。
fn join_public_path(public_path: &str, name: &str) -> String {
    if public_path.is_empty() {
        name.to_string()
    } else if public_path.ends_with('/') {
        format!("{public_path}{name}")
    } else {
        format!("{public_path}/{name}")
    }
}

/// prod 抽取普通 CSS：返回 `(JS 模块源码, CSS 文本)`。JS 只保留 `@import` 的 ESM 依赖（不注入 `<style>`）。
fn css_extract(src: &str) -> (String, String) {
    let css = wake_css::analyze(src);
    split_css_imports(&css.imports, &css.code)
}

/// prod 抽取 CSS Modules：JS 导出 `{局部名: 作用域名}` 映射（不注入 `<style>`），CSS 文本带出。
fn css_module_extract(src: &str, path: &Path) -> (String, String) {
    let seed = path.to_string_lossy();
    let m = wake_css::transform_modules(src, &seed);
    let (mut js, css_text) = split_css_imports(&m.imports, &m.code);
    js.push_str("export default {");
    for (local, scoped) in &m.exports {
        js.push_str(&format!(" {local:?}: {scoped:?},"));
    }
    js.push_str(" };\n");
    (js, css_text)
}

/// 拆分 CSS `@import`：相对 → ESM `import`（进模块图，其 CSS 各自抽取）；外部 URL → 回填 CSS 顶部。
/// 返回 `(JS import 语句, CSS 文本)`。
fn split_css_imports(imports: &[wake_css::CssImport], code: &str) -> (String, String) {
    let mut js = String::new();
    let mut head = String::new();
    for imp in imports {
        if is_external_url(&imp.specifier) {
            head.push_str("@import \"");
            head.push_str(&imp.specifier);
            head.push('"');
            if let Some(media) = &imp.media {
                head.push(' ');
                head.push_str(media);
            }
            head.push_str(";\n");
        } else {
            js.push_str("import ");
            push_js_string(&mut js, &normalize_css_import(&imp.specifier));
            js.push_str(";\n");
        }
    }
    let css = if head.is_empty() {
        code.to_string()
    } else {
        format!("{head}{code}")
    };
    (js, css)
}

/// 按扩展名选择源类型：`.tsx` → TS+JSX，`.jsx` → JS+JSX，`.ts`/`.mts`/`.cts` → TS，其余 → ESM 模块。
pub(crate) fn source_type_for(path: &Path) -> SourceType {
    match path.extension().and_then(|e| e.to_str()) {
        Some("tsx") => SourceType::Tsx,
        Some("jsx") => SourceType::Jsx,
        Some("ts" | "mts" | "cts") => SourceType::TypeScript,
        _ => SourceType::Module,
    }
}

/// 路径是否为 CSS 模块（`.css` / `.module.css` 等）。
pub(crate) fn is_css_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("css" | "scss" | "sass" | "less")
    )
}

/// 路径是否为 CSS Modules（`*.module.css` / `*.module.scss` …）。
fn is_css_module_path(path: &Path) -> bool {
    is_css_path(path)
        && path
            .file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(|stem| {
                Path::new(stem).extension().and_then(|e| e.to_str()) == Some("module")
            })
}

/// 路径是否为 JSON。
fn is_json_path(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()), Some("json"))
}

/// 路径是否为 `.raw`（原样文本，`asset/source`，对齐 crustify）。
fn is_raw_path(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()), Some("raw"))
}

/// `.raw` → JS 模块：把文件文本原样作默认导出字符串（webpack `asset/source` 等价，DESIGN §8.2）。
fn raw_to_js_module(text: &str) -> String {
    let mut js = String::from("export default ");
    push_js_string(&mut js, text);
    js.push_str(";\n");
    js
}

/// 路径是否为二进制静态资源（图片/字体/媒体）。
fn is_asset_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some(
            "png"
                | "jpg"
                | "jpeg"
                | "gif"
                | "svg"
                | "webp"
                | "avif"
                | "ico"
                | "bmp"
                | "woff"
                | "woff2"
                | "ttf"
                | "otf"
                | "eot"
                | "mp4"
                | "webm"
                | "mp3"
                | "wav"
                | "ogg"
        )
    )
}

/// JSON → JS 模块：JSON 是 JS 表达式子集，直接作默认导出（DESIGN §8.2；具名导出 tree-shaking 后置）。
fn json_to_js_module(text: &str) -> String {
    let trimmed = text.trim();
    let body = if trimmed.is_empty() { "null" } else { trimmed };
    format!("export default {body};\n")
}

/// 静态资源 → JS 模块：内联为 base64 data URI，默认导出该 URL 字符串（DESIGN §8.2）。
///
/// 第一版所有尺寸都内联；「超阈值(4KB)则 hash 拷贝为独立产物文件」需多产物输出基建
/// （与 6.5 代码分割共用），后置到该基建落地时接入。
fn asset_to_js_module(path: &Path, bytes: &[u8]) -> String {
    let uri = format!("data:{};base64,{}", mime_for(path), base64(bytes));
    let mut js = String::from("export default ");
    push_js_string(&mut js, &uri);
    js.push_str(";\n");
    js
}

/// 按扩展名猜 MIME 类型（data URI 用）。
fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("avif") => "image/avif",
        Some("ico") => "image/x-icon",
        Some("bmp") => "image/bmp",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("ttf") => "font/ttf",
        Some("otf") => "font/otf",
        Some("eot") => "application/vnd.ms-fontobject",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        _ => "application/octet-stream",
    }
}

/// 标准 base64 编码（RFC 4648，带 `=` 填充）。无外部依赖。
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            T[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// 把 CSS 源码翻译成一段等价 JS 模块源码（dev 形态：运行时注入 `<style>`）。
///
/// - `@import`（相对）→ ESM `import "..."`：进模块图，依赖先注入，天然去重排序；
/// - `@import`（`http(s):`/`//` 等外部 URL）→ 保留在 CSS 顶部（浏览器自行拉取）；
/// - 其余 CSS 文本 → JS 字符串，`typeof document !== "undefined"` 守卫下注入 `<style>`
///   （node/SSR 环境安全跳过）。
///
/// 注：url() 资源改写留待 6.4；此处 CSS 文本原样注入。
pub(crate) fn css_to_js_module(src: &str) -> String {
    let css = wake_css::analyze(src);
    let mut js = String::new();
    emit_css_prelude(&mut js, &css.imports, &css.code);
    js.push_str("export default __wake_css__;\n");
    js
}

/// 把一个 `.module.css` 翻译成 JS 模块：作用域化 CSS 注入 + 默认导出 `{ 局部名: 作用域名 }`（6.3）。
pub(crate) fn css_module_to_js(src: &str, path: &Path) -> String {
    let seed = path.to_string_lossy();
    let m = wake_css::transform_modules(src, &seed);
    let mut js = String::new();
    emit_css_prelude(&mut js, &m.imports, &m.code);
    // 默认导出局部名→作用域名映射（键一律加引号，兼容 `my-class` 之类）。
    js.push_str("export default {");
    for (local, scoped) in &m.exports {
        js.push_str(&format!(" {local:?}: {scoped:?},"));
    }
    js.push_str(" };\n");
    js
}

/// 发射 CSS 模块的公共前缀：`@import`（相对→ESM import，外部→回填 CSS 顶部）+ `<style>` 注入。
/// `__wake_css__` 变量随后可被默认导出（普通 CSS）或忽略（CSS Modules 导出映射）。
fn emit_css_prelude(js: &mut String, imports: &[wake_css::CssImport], css_code: &str) {
    // 外部 URL 的 @import 无法作为模块导入，回填到 CSS 顶部（CSS 要求 @import 在最前）。
    let mut css_head = String::new();
    for imp in imports {
        if is_external_url(&imp.specifier) {
            css_head.push_str("@import \"");
            css_head.push_str(&imp.specifier);
            css_head.push('"');
            if let Some(media) = &imp.media {
                css_head.push(' ');
                css_head.push_str(media);
            }
            css_head.push_str(";\n");
        } else {
            js.push_str("import ");
            push_js_string(js, &normalize_css_import(&imp.specifier));
            js.push_str(";\n");
        }
    }

    let full_css = if css_head.is_empty() {
        css_code.to_string()
    } else {
        format!("{css_head}{css_code}")
    };

    js.push_str("var __wake_css__ = ");
    push_js_string(js, &full_css);
    js.push_str(";\n");
    js.push_str("if (typeof document !== \"undefined\") {\n");
    js.push_str("  var __wake_style__ = document.createElement(\"style\");\n");
    js.push_str("  __wake_style__.textContent = __wake_css__;\n");
    js.push_str("  document.head.appendChild(__wake_style__);\n");
    js.push_str("}\n");
}

/// CSS `@import` 说明符是否为无法本地解析的外部 URL。
fn is_external_url(spec: &str) -> bool {
    spec.starts_with("http://")
        || spec.starts_with("https://")
        || spec.starts_with("//")
        || spec.starts_with("data:")
}

/// 规范化 CSS `@import` 说明符为 JS import 说明符。
///
/// CSS `@import` 默认相对于当前样式表 URL（如 `base.css` 即 `./base.css`）——
/// 补 `./` 使其走 resolver 的相对路径分支。已带 `.`/`/` 的原样保留。
/// （bare = node_modules 的 CSS 约定后置到 resolver v2。）
fn normalize_css_import(spec: &str) -> String {
    if spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') {
        spec.to_string()
    } else {
        format!("./{spec}")
    }
}

/// 把 `s` 追加为一个双引号 JS 字符串字面量（转义控制字符与行分隔符）。
pub(crate) fn push_js_string(out: &mut String, s: &str) {
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
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_import_becomes_js_import() {
        let js = css_to_js_module("@import \"./reset.css\";\n.a { color: red; }");
        assert!(js.contains("import \"./reset.css\";"));
        assert!(js.contains(".a { color: red; }"));
        assert!(js.contains("document.createElement(\"style\")"));
    }

    #[test]
    fn bare_css_import_gets_relative_prefix() {
        let js = css_to_js_module("@import \"base.css\";\nbody{}");
        assert!(js.contains("import \"./base.css\";"));
    }

    #[test]
    fn external_import_stays_in_css() {
        let js = css_to_js_module("@import \"https://fonts.example/x.css\";\nbody{}");
        assert!(!js.contains("import \"https"));
        assert!(js.contains("@import \\\"https://fonts.example/x.css\\\";"));
    }

    #[test]
    fn escapes_newlines_and_quotes() {
        let mut s = String::new();
        push_js_string(&mut s, "a\n\"b\"\t\\c");
        assert_eq!(s, "\"a\\n\\\"b\\\"\\t\\\\c\"");
    }

    #[test]
    fn css_extensions_detected() {
        assert!(is_css_path(Path::new("a.css")));
        assert!(is_css_path(Path::new("a.module.css")));
        assert!(is_css_path(Path::new("a.scss")));
        assert!(!is_css_path(Path::new("a.js")));
    }

    #[test]
    fn json_becomes_default_export() {
        let js = json_to_js_module("{ \"a\": 1, \"b\": [2, 3] }");
        assert_eq!(js, "export default { \"a\": 1, \"b\": [2, 3] };\n");
        assert_eq!(json_to_js_module("  "), "export default null;\n");
    }

    #[test]
    fn raw_becomes_default_export_string() {
        let js = raw_to_js_module("line1\n\"quoted\"\tend");
        assert_eq!(js, "export default \"line1\\n\\\"quoted\\\"\\tend\";\n");
        assert!(is_raw_path(Path::new("shader.raw")));
        assert!(!is_raw_path(Path::new("x.css")));
    }

    #[test]
    fn base64_matches_reference() {
        // RFC 4648 测试向量。
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn asset_inlines_as_data_uri() {
        let js = asset_to_js_module(Path::new("logo.png"), &[1, 2, 3]);
        assert!(js.starts_with("export default \"data:image/png;base64,"));
        assert!(js.contains(&base64(&[1, 2, 3])));
    }

    #[test]
    fn mime_lookup() {
        assert_eq!(mime_for(Path::new("a.svg")), "image/svg+xml");
        assert_eq!(mime_for(Path::new("a.woff2")), "font/woff2");
        assert_eq!(mime_for(Path::new("a.xyz")), "application/octet-stream");
    }
}
