//! # 非 JS 资源 loader —— 把 CSS / JSON / 静态资源转成 JS 模块源码
//!
//! DESIGN §8：CSS 与静态资源不是 JS，但打包器只认 JS 模块。策略与 css-loader / Vite 一致：
//! **把它们「翻译」成一段 JS 模块源码**，再走完全相同的 parse → link → codegen 管线——
//! 引擎、codegen、linker 零改动。CSS 的 `@import` 翻译成 ESM `import`（进模块图去重排序）。
//!
//! 两种形态由 [`LoadOptions`] 切换：
//! - **dev**：样式经运行时 `<style>` 注入（可 HMR），资源一律内联 base64；
//! - **prod**（`extract_css`）：CSS 文本经 [`Loaded::css`] 带出聚合为独立 `.css`，
//!   超过 `asset_inline_limit` 的资源写为带内容 hash 的独立产物。
//!
//! **CSS 里的 `url()` 与 JS `import` 走同一套资源策略**（见 [`prepare_css`]）——字体与图片
//! 的主要引用方式是 `@font-face` / `background-image` 而非 JS import，不改写就是产物死链。

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
    ///
    /// 一个 CSS 模块可以引出**多个**资源（`@font-face` 的 fallback 链、多张背景图），
    /// 故是 `Vec` 而非单个。
    pub assets: Vec<(String, Vec<u8>)>,
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
                assets: vec![(name, bytes)],
                css: None,
            })
        } else {
            Ok(Loaded {
                source: asset_to_js_module(path, &bytes),
                source_type: SourceType::Module,
                assets: Vec::new(),
                css: None,
            })
        }
    } else if is_css_preprocessor_path(path) {
        // 需要预处理器的样式源：明确拒绝，而不是当普通 CSS 透传。
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "wake 不内置 Sass/Less 预处理器，无法编译 `.{ext}`；请改用原生 CSS（`.css`），或先用 sass/less CLI 预编译为 `.css` 再导入"
            ),
        ))
    } else {
        let text = fs.read_to_string(path)?;
        if is_css_path(path) {
            // CSS 里的 `url()`（`@font-face` 的字体、`background-image` 的图片）先落地为真实
            // 产物并改写 URL——dev/prod、普通 CSS/CSS Modules 四条路径统一处理。
            let Prepared {
                imports,
                code,
                assets,
                exports,
            } = prepare_css(fs, path, &text, opts);
            if opts.extract_css {
                // prod 抽取：JS 模块不注入 `<style>`，CSS 文本带出。
                let (mut source, css) = split_css_imports(&imports, &code);
                push_css_module_exports(&mut source, &exports);
                Ok(Loaded {
                    source,
                    source_type: SourceType::Module,
                    assets,
                    css: Some(css),
                })
            } else {
                let mut source = String::new();
                emit_css_prelude(&mut source, &imports, &code);
                if exports.is_some() {
                    push_css_module_exports(&mut source, &exports);
                } else {
                    source.push_str("export default __wake_css__;\n");
                }
                Ok(Loaded {
                    source,
                    source_type: SourceType::Module,
                    assets,
                    css: None,
                })
            }
        } else if is_json_path(path) {
            Ok(Loaded {
                source: json_to_js_module(&text),
                source_type: SourceType::Module,
                assets: Vec::new(),
                css: None,
            })
        } else if is_raw_path(path) {
            Ok(Loaded {
                source: raw_to_js_module(&text),
                source_type: SourceType::Module,
                assets: Vec::new(),
                css: None,
            })
        } else {
            Ok(Loaded {
                source: text,
                source_type: source_type_for(path),
                assets: Vec::new(),
                css: None,
            })
        }
    }
}

/// [`prepare_css`] 的产出：`@import` 依赖 + `url()` 已改写的 CSS + 引出的资源产物
/// + CSS Modules 的类名映射（普通 CSS 为 `None`）。
struct Prepared {
    imports: Vec<wake_css::CssImport>,
    code: String,
    assets: Vec<(String, Vec<u8>)>,
    exports: Option<Vec<(String, String)>>,
}

/// CSS 的公共前处理：CSS Modules 作用域化（若是 `*.module.css`）→ 提 `@import` → **改写 `url()`**。
///
/// `url()` 改写必须在这里、按**模块粒度**做：[`wake_css::CssUrl`] 的偏移是相对单个模块的
/// `code` 的，一旦进了 `split_css_imports`（前置外部 `@import`）或跨模块聚合/压缩，偏移即失效。
fn prepare_css(fs: &dyn FileSystem, path: &Path, text: &str, opts: &LoadOptions) -> Prepared {
    // CSS Modules：先做类名作用域化，再在其**输出**上分析 url。
    // （`transform_modules` 不记录 url 位置，故对其结果重跑一次 `analyze`——此时 `@import`
    // 已被移除，`analyze` 对余下文本是恒等的，偏移因而对齐 `code`。）
    let (imports, base_code, exports) = if is_css_module_path(path) {
        let seed = path.to_string_lossy();
        let m = wake_css::transform_modules(text, &seed);
        (m.imports, m.code, Some(m.exports))
    } else {
        let m = wake_css::analyze(text);
        (m.imports, m.code, None)
    };
    let analyzed = wake_css::analyze(&base_code);
    let mut assets = Vec::new();
    let code = analyzed.rewrite_urls(|u| rewrite_one_css_url(fs, path, u, opts, &mut assets));
    Prepared {
        imports,
        code,
        assets,
        exports,
    }
}

/// 改写单处 `url()`：本地文件按阈值内联 base64 或写为带 hash 的独立产物；其余保留原样。
fn rewrite_one_css_url(
    fs: &dyn FileSystem,
    css_path: &Path,
    u: &wake_css::CssUrl,
    opts: &LoadOptions,
    assets: &mut Vec<(String, Vec<u8>)>,
) -> Option<String> {
    let spec = u.specifier.trim();
    if !is_local_css_url(spec) {
        return None;
    }
    // 字体常见的 `?#iefix` / SVG 字体的 `#fontname`：解析时要去掉，发 URL 时要带回。
    let (rel, suffix) = split_url_suffix(spec);
    if rel.is_empty() {
        return None;
    }
    let base = css_path.parent().unwrap_or_else(|| Path::new(""));
    let target = wake_common::fs::normalize(&base.join(rel));
    // 读不到就原样保留：一处写错的 url 不该让整个构建失败，也不该被改成更坏的东西。
    let bytes = fs.read(&target).ok()?;
    if bytes.len() > opts.asset_inline_limit {
        let name = asset_file_name(&target, &bytes);
        let url = join_public_path(&opts.public_path, &name);
        assets.push((name, bytes));
        Some(format!("{url}{suffix}"))
    } else {
        // data URI 上挂 `?#iefix` 这类 IE hack 没有意义（且会破坏解析），丢弃后缀。
        Some(format!(
            "data:{};base64,{}",
            mime_for(&target),
            base64(&bytes)
        ))
    }
}

/// CSS `url()` 引用是否指向**可打包的本地文件**。
///
/// 排除：空引用；`data:`/`blob:` 等已内联或运行时 URL；协议绝对 URL（`http://`、`//cdn`）；
/// 纯片段 `#gradient`（SVG 内部引用，改写会破坏渲染）；站点绝对路径 `/img/x.png`
/// （由部署方提供，不归打包器接管）。
///
/// 注意**不能**复用 [`is_external_url`]——那个只服务 `@import`，漏了 `#fragment` 与绝对路径。
fn is_local_css_url(spec: &str) -> bool {
    if spec.is_empty() || spec.starts_with('#') || spec.starts_with('/') {
        return false;
    }
    // `scheme:` 前缀（`data:` / `http:` / `blob:` …）：冒号出现在任何 `/` 之前即视为协议。
    match (spec.find(':'), spec.find('/')) {
        (Some(c), Some(s)) if c < s => return false,
        (Some(_), None) => return false,
        _ => {}
    }
    true
}

/// 把 `./f.eot?#iefix` 拆成 `("./f.eot", "?#iefix")`——查询串与片段不参与文件解析。
fn split_url_suffix(spec: &str) -> (&str, &str) {
    match spec.find(['?', '#']) {
        Some(i) => (&spec[..i], &spec[i..]),
        None => (spec, ""),
    }
}

/// 追加 CSS Modules 的默认导出 `{ 局部名: 作用域名 }`（`exports` 为 `None` 时不追加）。
fn push_css_module_exports(js: &mut String, exports: &Option<Vec<(String, String)>>) {
    let Some(exports) = exports else { return };
    js.push_str("export default {");
    for (local, scoped) in exports {
        js.push_str(&format!(" {local:?}: {scoped:?},"));
    }
    js.push_str(" };\n");
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
    matches!(path.extension().and_then(|e| e.to_str()), Some("css"))
}

/// `.scss` / `.sass` / `.less`——需要预处理器编译的样式源。
///
/// wake **不内置** Sass/Less 编译器。这些扩展名曾被并入 [`is_css_path`] 当作普通 CSS
/// 原样透传：嵌套、变量、mixin、`@use` 等语法会直接落进产物，形成非法 CSS 或静默错误样式。
/// 现改为在 [`load_source`] 明确报错——**宁可构建失败，也不产出看似成功的错误产物**。
pub(crate) fn is_css_preprocessor_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("scss" | "sass" | "less")
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
    use wake_common::MemoryFileSystem;

    /// 用内存 FS 走**真实** [`load_source`] 路径加载一个 CSS 文件（dev 形态：不抽取、全内联）。
    fn load_css(
        files: impl IntoIterator<Item = (&'static str, &'static str)>,
        path: &str,
    ) -> Loaded {
        let fs = MemoryFileSystem::from_files(files);
        load_source(&fs, Path::new(path), &LoadOptions::default()).expect("load")
    }

    #[test]
    fn css_import_becomes_js_import() {
        let js = load_css(
            [("a.css", "@import \"./reset.css\";\n.a { color: red; }")],
            "a.css",
        )
        .source;
        assert!(js.contains("import \"./reset.css\";"));
        assert!(js.contains(".a { color: red; }"));
        assert!(js.contains("document.createElement(\"style\")"));
    }

    #[test]
    fn bare_css_import_gets_relative_prefix() {
        let js = load_css([("a.css", "@import \"base.css\";\nbody{}")], "a.css").source;
        assert!(js.contains("import \"./base.css\";"));
    }

    #[test]
    fn external_import_stays_in_css() {
        let js = load_css(
            [("a.css", "@import \"https://fonts.example/x.css\";\nbody{}")],
            "a.css",
        )
        .source;
        assert!(!js.contains("import \"https"));
        assert!(js.contains("@import \\\"https://fonts.example/x.css\\\";"));
    }

    #[test]
    fn css_url_inlines_local_asset() {
        // 字体与图片的**主要**引用方式是 CSS，不是 JS import。
        let loaded = load_css(
            [
                (
                    "s/a.css",
                    "@font-face{src:url(\"./f.woff2\") format(\"woff2\"),url(../img/g.woff) format(\"woff\")}\
                     .h{background:url(./bg.png)}",
                ),
                ("s/f.woff2", "FONTBYTES"),
                ("img/g.woff", "OLDFONT"),
                ("s/bg.png", "PNGBYTES"),
            ],
            "s/a.css",
        );
        let css = loaded.source;
        // 三处都被换成 data URI，且 MIME 按各自扩展名。
        assert!(css.contains("data:font/woff2;base64,"), "{css}");
        assert!(css.contains("data:font/woff;base64,"), "{css}");
        assert!(css.contains("data:image/png;base64,"), "{css}");
        // 原相对路径不复存在（否则产物里是死链）。
        assert!(!css.contains("./f.woff2"), "{css}");
        assert!(!css.contains("../img/g.woff"), "{css}");
        // `format(...)` 不受影响。
        assert!(css.contains("format(\\\"woff2\\\")"), "{css}");
        // 全内联（默认阈值 usize::MAX）→ 无独立产物。
        assert!(loaded.assets.is_empty());
    }

    #[test]
    fn css_url_emits_separate_file_over_limit() {
        let fs = MemoryFileSystem::from_files([
            ("s/a.css", ".h{background:url(./big.png)}"),
            ("s/big.png", "0123456789"),
        ]);
        let opts = LoadOptions {
            asset_inline_limit: 4,
            public_path: "/static/".to_string(),
            ..LoadOptions::default()
        };
        let loaded = load_source(&fs, Path::new("s/a.css"), &opts).expect("load");
        assert_eq!(loaded.assets.len(), 1, "超阈值应产出独立文件");
        let (name, bytes) = &loaded.assets[0];
        assert!(name.starts_with("big.") && name.ends_with(".png"), "{name}");
        assert_eq!(bytes, b"0123456789");
        // CSS 里的 url 换成带 publicPath 前缀的最终 URL。
        assert!(
            loaded.source.contains(&format!("/static/{name}")),
            "{}",
            loaded.source
        );
    }

    #[test]
    fn css_url_leaves_non_local_references_alone() {
        // 反例：这些都不该被改写——改写 `#gradient` 会直接破坏 SVG 渲染。
        let src = ".a{background:url(data:image/gif;base64,R0lGOD)}\
                   .b{background:url(https://cdn.example/x.png)}\
                   .c{background:url(//cdn.example/y.png)}\
                   .d{filter:url(#blur)}\
                   .e{background:url(/site/abs.png)}";
        let css = load_css([("a.css", src)], "a.css").source;
        for kept in [
            "data:image/gif;base64,R0lGOD",
            "https://cdn.example/x.png",
            "//cdn.example/y.png",
            "#blur",
            "/site/abs.png",
        ] {
            assert!(css.contains(kept), "{kept} 不应被改写:\n{css}");
        }
    }

    #[test]
    fn css_url_missing_file_is_left_untouched() {
        // 读不到的引用原样保留——一处写错的 url 不该让整个构建失败。
        let css = load_css([("a.css", ".a{background:url(./nope.png)}")], "a.css").source;
        assert!(css.contains("./nope.png"), "{css}");
    }

    #[test]
    fn css_url_strips_query_and_fragment_for_lookup() {
        // 字体的 `?#iefix` / SVG 字体的 `#name`：解析时去掉，发文件 URL 时带回。
        let fs = MemoryFileSystem::from_files([
            (
                "a.css",
                "@font-face{src:url(./f.eot?#iefix) format(\"eot\")}",
            ),
            ("f.eot", "0123456789"),
        ]);
        let opts = LoadOptions {
            asset_inline_limit: 4,
            ..LoadOptions::default()
        };
        let loaded = load_source(&fs, Path::new("a.css"), &opts).expect("load");
        assert_eq!(loaded.assets.len(), 1);
        assert!(loaded.source.contains("?#iefix"), "{}", loaded.source);
    }

    #[test]
    fn css_module_urls_are_rewritten_too() {
        // `.module.css` 的 url 此前连位置都不记录，是比普通 CSS 更差的一档。
        let loaded = load_css(
            [
                ("s/a.module.css", ".hero{background:url(./bg.png)}"),
                ("s/bg.png", "PNGBYTES"),
            ],
            "s/a.module.css",
        );
        assert!(
            loaded.source.contains("data:image/png;base64,"),
            "{}",
            loaded.source
        );
        // 作用域化仍然生效，且导出映射还在。
        assert!(loaded.source.contains("hero_"), "{}", loaded.source);
        assert!(
            loaded.source.contains("export default {"),
            "{}",
            loaded.source
        );
    }

    #[test]
    fn local_css_url_classification() {
        for local in ["./a.png", "../a.png", "a.png", "img/a.png", "a.png?v=1"] {
            assert!(is_local_css_url(local), "{local} 应视为本地资源");
        }
        for external in [
            "",
            "#frag",
            "/abs.png",
            "data:image/png;base64,AA",
            "http://x/a.png",
            "https://x/a.png",
            "//x/a.png",
            "blob:abc",
        ] {
            assert!(!is_local_css_url(external), "{external} 不应视为本地资源");
        }
    }

    #[test]
    fn url_suffix_split() {
        assert_eq!(split_url_suffix("./f.eot?#iefix"), ("./f.eot", "?#iefix"));
        assert_eq!(split_url_suffix("./f.svg#name"), ("./f.svg", "#name"));
        assert_eq!(split_url_suffix("./f.woff2"), ("./f.woff2", ""));
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
        assert!(!is_css_path(Path::new("a.scss")), "预处理器源不再当作 CSS");
        assert!(is_css_preprocessor_path(Path::new("a.scss")));
        assert!(is_css_preprocessor_path(Path::new("a.less")));
        assert!(!is_css_preprocessor_path(Path::new("a.css")));
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
