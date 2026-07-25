//! # wake_css — 极简 CSS「tokenizer」（DESIGN §8.1）
//!
//! **不做完整 CSS 引擎**（lightningcss 级别留给远期）。第一版只识别打包必需的两件事：
//!
//! - `@import`：依赖提取——进模块图统一去重排序（顶部 @import 语句从产物中**移除**，
//!   由驱动层转成 JS `import` 让模块图处理顺序与去重）；
//! - `url()`：资源引用——记录其在**输出** `code` 中的字节区间，供资源改写（6.4）原地替换。
//!
//! 其余内容（选择器、声明、注释、字符串）**原样透传**。扫描器正确跳过 `/* */` 注释与
//! `"..."`/`'...'` 字符串，避免把它们内部的 `@import`/`url(` 误判为规则。
//!
//! 设计取「懒 flush」：默认不复制，遇到需要删除（@import）或需要定位（url）的片段时才把
//! 已扫描区间一次性写入输出——既 UTF-8 安全（永不切多字节字符），又零多余分配。

/// 一条 `@import` 依赖。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CssImport {
    /// 被导入的说明符（已去掉引号 / `url()` 包装）。
    pub specifier: String,
    /// 可选媒体查询文本（`@import "x" screen and (...)` 的尾部），去首尾空白后保留。
    /// 第一版按普通样式注入，媒体条件暂不下推（记录以备后续包 `@media`）。
    pub media: Option<String>,
}

/// 一处 `url()` 引用（供资源改写，6.4）。位置相对**输出** [`CssModule::code`]。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CssUrl {
    /// `url()` 内的原始引用（已去引号，未 trim 内部空白）。
    pub specifier: String,
    /// 引用在 `code` 中的起始字节偏移（不含 `url(` 与引号）。
    pub start: usize,
    /// 引用在 `code` 中的结束字节偏移（半开区间 `[start, end)`）。
    pub end: usize,
    /// 原引用是否带引号（改写时决定是否补引号）。
    pub quoted: bool,
}

/// CSS 分析结果。
#[derive(Debug, Clone, Default)]
pub struct CssModule {
    /// 顶部 `@import` 依赖（按源码出现顺序；去重交给模块图）。
    pub imports: Vec<CssImport>,
    /// `url()` 引用（位置相对 `code`，按出现顺序）。
    pub urls: Vec<CssUrl>,
    /// 处理后的 CSS：`@import` 语句已移除，其余（含注释/空白/字符串）透传。
    pub code: String,
}

impl CssModule {
    /// 按 `url()` 记录，用 `rewrite` 回调把每个引用替换为新文本（原地、从后往前不移位）。
    /// `rewrite` 返回 `None` 表示保留原引用（如 `data:`/绝对 URL/`#fragment`）。
    ///
    /// 返回改写后的 CSS。用于 6.4：把 `url(./logo.png)` 换成产物 URL 或内联 data URI。
    pub fn rewrite_urls(&self, mut rewrite: impl FnMut(&CssUrl) -> Option<String>) -> String {
        let mut code = self.code.clone();
        // 从后往前替换，避免前面的替换改变后面记录的偏移。
        for u in self.urls.iter().rev() {
            if let Some(new) = rewrite(u) {
                code.replace_range(u.start..u.end, &new);
            }
        }
        code
    }
}

/// 压缩一段 CSS（prod，CRUSTIFY-PARITY §M4c）。**安全子集**：折叠空白为单空格、去注释、
/// 删 `{` `}` `;` `,` 相邻空白、删 `}` 前多余 `;`；字符串内原样。
///
/// 刻意**不**删的（避免破坏语义）：后代组合器空白（`.a .b`）、`calc(1px + 2px)` 等值内空白、
/// `>`/`+`/`~` 组合器周围空白、`prop: value` 冒号后空白——这些删了会改变含义或非法。
pub fn minify(src: &str) -> String {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    let mut pending_space = false; // 见到空白，待定是否发单空格
    let mut suppress = true; // 抑制紧邻结构符/开头 之后的空格（开头 true → 去前导空白）
    while i < n {
        let c = b[i];
        if c == b'/' && i + 1 < n && b[i + 1] == b'*' {
            // 注释：整段丢弃（不发空格；源码真实空白仍触发 pending_space）。
            i += 2;
            while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(n);
        } else if c == b'"' || c == b'\'' {
            if pending_space && !suppress {
                out.push(' ');
            }
            pending_space = false;
            suppress = false;
            let start = i;
            i += 1;
            while i < n && b[i] != c {
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i = (i + 1).min(n);
            out.push_str(&src[start..i.min(n)]);
        } else if c == b' ' || c == b'\t' || c == b'\n' || c == b'\r' {
            pending_space = true;
            i += 1;
        } else if c == b'{' || c == b',' || c == b';' {
            pending_space = false;
            out.push(c as char);
            suppress = true;
            i += 1;
        } else if c == b'}' {
            pending_space = false;
            if out.ends_with(';') {
                out.pop(); // 删 `}` 前多余 `;`
            }
            out.push('}');
            suppress = true;
            i += 1;
        } else {
            // 普通片段：扫到下一个分隔符再整段推入（UTF-8 安全：边界均在 ASCII 分隔符）。
            if pending_space && !suppress {
                out.push(' ');
            }
            pending_space = false;
            suppress = false;
            let start = i;
            while i < n {
                let d = b[i];
                if d == b'/' && i + 1 < n && b[i + 1] == b'*' {
                    break;
                }
                if matches!(
                    d,
                    b'"' | b'\'' | b' ' | b'\t' | b'\n' | b'\r' | b'{' | b'}' | b',' | b';'
                ) {
                    break;
                }
                i += 1;
            }
            out.push_str(&src[start..i]);
        }
    }
    out
}

/// 分析一段 CSS：提取 `@import` 依赖、探测 `url()` 引用，其余透传到 `code`。
pub fn analyze(src: &str) -> CssModule {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n);
    let mut imports: Vec<CssImport> = Vec::new();
    let mut urls: Vec<CssUrl> = Vec::new();

    let mut i = 0; // 当前扫描位置
    let mut mark = 0; // 尚未 flush 到 out 的输入起点
    let mut depth: i32 = 0; // `{}` 嵌套深度（@import 只在 depth==0 有效）

    while i < n {
        match b[i] {
            // —— 注释 /* ... */：整体透传，内部不解析 ——
            b'/' if i + 1 < n && b[i + 1] == b'*' => {
                i += 2;
                while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(n);
            }
            // —— 字符串 "..." / '...'：整体透传，内部不解析 ——
            b'"' | b'\'' => {
                let q = b[i];
                i += 1;
                while i < n && b[i] != q {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(n);
            }
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                depth -= 1;
                i += 1;
            }
            // —— @import（仅顶层）：提取依赖并从输出移除整条语句 ——
            b'@' if depth == 0 && keyword_at(b, i + 1, b"import") => {
                out.push_str(&src[mark..i]); // flush @import 之前的内容
                let (import, end) = parse_import_rule(src, b, i);
                imports.push(import);
                i = end;
                // 顺带吞掉紧随的一个换行，避免留下空行。
                while i < n && (b[i] == b' ' || b[i] == b'\t') {
                    i += 1;
                }
                if i < n && b[i] == b'\n' {
                    i += 1;
                } else if i + 1 < n && b[i] == b'\r' && b[i + 1] == b'\n' {
                    i += 2;
                }
                mark = i;
            }
            // —— url( ... )：记录输出中的引用区间（不含 url( 与引号）——
            b'u' | b'U' if keyword_at(b, i, b"url") && next_nonspace_is(b, i + 3, b'(') => {
                if let Some((u_specifier, ref_start_in, ref_end_in, quoted, stmt_end)) =
                    parse_url(src, b, i)
                {
                    // flush 到引用起点，使 out.len() 恰为引用在输出中的起偏移。
                    out.push_str(&src[mark..ref_start_in]);
                    let start = out.len();
                    out.push_str(&src[ref_start_in..ref_end_in]);
                    let end = out.len();
                    urls.push(CssUrl {
                        specifier: u_specifier,
                        start,
                        end,
                        quoted,
                    });
                    mark = ref_end_in;
                    i = stmt_end;
                } else {
                    i += 3;
                }
            }
            _ => i += 1,
        }
    }
    out.push_str(&src[mark..n]);

    CssModule {
        imports,
        urls,
        code: out,
    }
}

/// `b[at..]` 是否以 `kw`（ASCII，大小写不敏感）起头且其后为非标识符字符（词边界）。
fn keyword_at(b: &[u8], at: usize, kw: &[u8]) -> bool {
    if at + kw.len() > b.len() {
        return false;
    }
    for (k, &kb) in kw.iter().enumerate() {
        if !b[at + k].eq_ignore_ascii_case(&kb) {
            return false;
        }
    }
    // 词边界：kw 后不能紧跟标识符字符（`@imports`、`urlx` 不算）。
    match b.get(at + kw.len()) {
        Some(&c) => !is_ident_byte(c),
        None => true,
    }
}

/// CSS 标识符字节（简化：字母/数字/`-`/`_`/非 ASCII）。
fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c >= 0x80
}

/// 从 `at` 起跳过空白后，第一个非空白字节是否为 `target`。
fn next_nonspace_is(b: &[u8], at: usize, target: u8) -> bool {
    let mut j = at;
    while j < b.len() && b[j].is_ascii_whitespace() {
        j += 1;
    }
    b.get(j) == Some(&target)
}

/// 解析 `@import` 规则（`i` 指向 `@`）。返回 (依赖, 语句结束偏移即 `;` 之后)。
fn parse_import_rule(src: &str, b: &[u8], i: usize) -> (CssImport, usize) {
    let n = b.len();
    let mut j = i + "@import".len();
    while j < n && b[j].is_ascii_whitespace() {
        j += 1;
    }

    // 说明符：url(...) 形式或 "..."/'...' 字符串形式。
    let (specifier, mut j) = if keyword_at(b, j, b"url") && next_nonspace_is(b, j + 3, b'(') {
        match parse_url(src, b, j) {
            Some((spec, _, _, _, end)) => (spec, end),
            None => (String::new(), j + 3),
        }
    } else if j < n && (b[j] == b'"' || b[j] == b'\'') {
        let (spec, end) = read_string(src, b, j);
        (spec, end)
    } else {
        // 兜底：读到分号/空白为止。
        let start = j;
        while j < n && b[j] != b';' && !b[j].is_ascii_whitespace() {
            j += 1;
        }
        (src[start..j].to_string(), j)
    };

    // 媒体查询：剩余到 `;` 的部分。
    let media_start = j;
    while j < n && b[j] != b';' {
        j += 1;
    }
    let media_raw = src[media_start..j].trim();
    let media = if media_raw.is_empty() {
        None
    } else {
        Some(media_raw.to_string())
    };
    if j < n && b[j] == b';' {
        j += 1; // 吞掉分号
    }
    (CssImport { specifier, media }, j)
}

/// 读一个字符串字面量（`i` 指向引号）。返回 (内容去引号, 结束偏移即闭引号之后)。
fn read_string(src: &str, b: &[u8], i: usize) -> (String, usize) {
    let n = b.len();
    let q = b[i];
    let start = i + 1;
    let mut j = start;
    while j < n && b[j] != q {
        j += if b[j] == b'\\' { 2 } else { 1 };
    }
    let content = src[start..j.min(n)].to_string();
    ((content), (j + 1).min(n))
}

/// 解析 `url(...)`（`i` 指向 `u`）。
/// 返回 (引用内容去引号, 引用起始输入偏移, 引用结束输入偏移, 是否带引号, `)` 之后偏移)。
fn parse_url(src: &str, b: &[u8], i: usize) -> Option<(String, usize, usize, bool, usize)> {
    let n = b.len();
    let mut j = i + 3; // 跳过 `url`
    while j < n && b[j].is_ascii_whitespace() {
        j += 1;
    }
    if j >= n || b[j] != b'(' {
        return None;
    }
    j += 1; // 跳过 `(`
    while j < n && b[j].is_ascii_whitespace() {
        j += 1;
    }
    if j < n && (b[j] == b'"' || b[j] == b'\'') {
        // 带引号：引用为引号内内容。
        let q = b[j];
        let ref_start = j + 1;
        let mut k = ref_start;
        while k < n && b[k] != q {
            k += if b[k] == b'\\' { 2 } else { 1 };
        }
        let ref_end = k.min(n);
        let spec = src[ref_start..ref_end].to_string();
        k = (k + 1).min(n); // 跳过闭引号
        while k < n && b[k] != b')' {
            k += 1;
        }
        let stmt_end = (k + 1).min(n); // 跳过 `)`
        Some((spec, ref_start, ref_end, true, stmt_end))
    } else {
        // 无引号：引用到 `)` 或空白为止。
        let ref_start = j;
        let mut k = j;
        while k < n && b[k] != b')' && !b[k].is_ascii_whitespace() {
            k += 1;
        }
        let ref_end = k;
        let spec = src[ref_start..ref_end].to_string();
        while k < n && b[k] != b')' {
            k += 1;
        }
        let stmt_end = (k + 1).min(n);
        Some((spec, ref_start, ref_end, false, stmt_end))
    }
}

// ======================================================================
// CSS Modules（`.module.css` 类名局部作用域，PLAN §6.3）
// ======================================================================

/// CSS Modules 转换结果。
#[derive(Debug, Clone, Default)]
pub struct CssModulesResult {
    /// `@import` 依赖（与 [`analyze`] 同）。
    pub imports: Vec<CssImport>,
    /// 局部类名 → 作用域化类名（按首次出现顺序、去重）。
    pub exports: Vec<(String, String)>,
    /// 改写后的 CSS：类选择器 `.foo` → `.foo_<hash>`，`@import` 已移除。
    pub code: String,
}

/// 当前块类型：`Rule`（含嵌套规则，如 `@media` 体，内部仍是选择器上下文）/
/// `Decl`（声明块，内部是 `prop: value`，`.` 属值不改）。
#[derive(PartialEq)]
enum BlockKind {
    Rule,
    Decl,
}

/// 把一个 `.module.css` 源转换为「类名作用域化」的 CSS + 导出映射。
///
/// - 类选择器 `.foo` → `.foo_<hash>`（`hash` 由 `seed`（通常是文件路径）+ 局部名决定，
///   同文件同名稳定、跨文件不撞）；构建 `局部名 → 作用域名` 映射供 JS `import styles` 使用。
/// - 正确区分**选择器上下文**（顶层 / `@media`·`@supports`·`@container`·`@layer` 体）与
///   **声明块**（`.foo { }` 体内、`@keyframes`/`@font-face` 体），只在前者改写 `.`。
/// - `@import` 依赖提取同 [`analyze`]。（url() 改写在 module 场景暂略，见 6.4。）
///
/// 未覆盖（后续）：`#id` 作用域、`composes`、`:global(...)`/`:local(...)`、keyframes 名作用域。
pub fn transform_modules(src: &str, seed: &str) -> CssModulesResult {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = String::with_capacity(n + 32);
    let mut imports: Vec<CssImport> = Vec::new();
    let mut exports: Vec<(String, String)> = Vec::new();
    let mut stack: Vec<BlockKind> = Vec::new();

    let mut i = 0;
    let mut mark = 0;
    let mut prelude_start = 0; // 当前 `{` 之前的 prelude 起点

    while i < n {
        match b[i] {
            b'/' if i + 1 < n && b[i + 1] == b'*' => {
                i += 2;
                while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(n);
            }
            b'"' | b'\'' => {
                let q = b[i];
                i += 1;
                while i < n && b[i] != q {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i = (i + 1).min(n);
            }
            b'@' if stack.is_empty() && keyword_at(b, i + 1, b"import") => {
                out.push_str(&src[mark..i]);
                let (imp, end) = parse_import_rule(src, b, i);
                imports.push(imp);
                i = end;
                while i < n && (b[i] == b' ' || b[i] == b'\t') {
                    i += 1;
                }
                if i < n && b[i] == b'\n' {
                    i += 1;
                } else if i + 1 < n && b[i] == b'\r' && b[i + 1] == b'\n' {
                    i += 2;
                }
                mark = i;
                prelude_start = i;
            }
            b'{' => {
                let prelude = src[prelude_start..i].trim_start();
                let kind = if is_rule_at_rule(prelude) {
                    BlockKind::Rule
                } else {
                    BlockKind::Decl
                };
                stack.push(kind);
                i += 1;
                prelude_start = i;
            }
            b'}' => {
                stack.pop();
                i += 1;
                prelude_start = i;
            }
            b';' => {
                i += 1;
                prelude_start = i;
            }
            // 类选择器：仅在选择器上下文（顶层 / 规则型 at-rule 体内）改写。
            b'.' if in_selector_ctx(&stack) && i + 1 < n && is_class_start(b[i + 1]) => {
                let name_start = i + 1;
                let mut j = name_start;
                while j < n && is_ident_byte(b[j]) {
                    j += 1;
                }
                let local = &src[name_start..j];
                let scoped = scoped_name(seed, local);
                out.push_str(&src[mark..name_start]); // flush 到（含）`.`
                out.push_str(&scoped);
                mark = j;
                if !exports.iter().any(|(l, _)| l == local) {
                    exports.push((local.to_string(), scoped));
                }
                i = j;
            }
            _ => i += 1,
        }
    }
    out.push_str(&src[mark..n]);

    CssModulesResult {
        imports,
        exports,
        code: out,
    }
}

/// 内层块是否处于选择器上下文（空栈=顶层，或最内层是规则型 at-rule 体）。
fn in_selector_ctx(stack: &[BlockKind]) -> bool {
    stack.last().is_none_or(|k| *k == BlockKind::Rule)
}

/// prelude 是否为「含嵌套规则」的 at-rule（其块内仍是选择器上下文）。
fn is_rule_at_rule(prelude: &str) -> bool {
    let p = prelude.as_bytes();
    let kw = |k: &[u8]| keyword_at(p, 0, k);
    !prelude.is_empty()
        && p[0] == b'@'
        && (kw(b"@media")
            || kw(b"@supports")
            || kw(b"@container")
            || kw(b"@layer")
            || kw(b"@document")
            || kw(b"@scope"))
}

/// 类名首字符（CSS 标识符起始：字母 / `_` / `-` / 非 ASCII；不含数字）。
fn is_class_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c == b'-' || c >= 0x80
}

/// 作用域化类名：`{local}_{hash6}`，hash = FNV-1a(seed ‖ local) 低 24 位。
fn scoped_name(seed: &str, local: &str) -> String {
    let h = fnv1a(seed, local) & 0x00ff_ffff;
    format!("{local}_{h:06x}")
}

/// FNV-1a（seed 与 local 间隔一个 0 分隔），稳定、无外部依赖。
fn fnv1a(seed: &str, local: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &byte in seed
        .as_bytes()
        .iter()
        .chain(std::iter::once(&0u8))
        .chain(local.as_bytes())
    {
        h ^= byte as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minify_basic_whitespace_and_semicolons() {
        let css = ".a {\n  color: red;\n  margin: 0;\n}\n";
        assert_eq!(minify(css), ".a{color: red;margin: 0}");
    }

    #[test]
    fn minify_preserves_descendant_combinator() {
        // 后代组合器空白必须保留（`.a .b` ≠ `.a.b`）；多空格折叠为单空格。
        assert_eq!(minify(".a    .b { x: 1 }"), ".a .b{x: 1}");
        assert_eq!(minify(".a.b { x: 1 }"), ".a.b{x: 1}");
    }

    #[test]
    fn minify_preserves_calc_spaces() {
        // calc 内 +/- 周围空白是必需的，不能删。
        assert_eq!(
            minify(".x { width: calc(100% - 20px); }"),
            ".x{width: calc(100% - 20px)}"
        );
    }

    #[test]
    fn minify_drops_comments_keeps_strings() {
        assert_eq!(minify(".a{/* c */color:red}"), ".a{color:red}");
        // 字符串内容原样（含空白）。
        assert_eq!(
            minify(".a { content: \"  hi  \" ; }"),
            ".a{content: \"  hi  \"}"
        );
    }

    #[test]
    fn minify_utf8_safe() {
        assert_eq!(minify(".a { content: \"中文\"; }"), ".a{content: \"中文\"}");
    }

    #[test]
    fn extracts_import_string_form() {
        let m = analyze("@import \"reset.css\";\n.a { color: red; }");
        assert_eq!(m.imports.len(), 1);
        assert_eq!(m.imports[0].specifier, "reset.css");
        assert_eq!(m.imports[0].media, None);
        // @import 语句连同其换行被移除。
        assert_eq!(m.code, ".a { color: red; }");
    }

    #[test]
    fn extracts_import_url_form() {
        let m = analyze("@import url('base.css');\nbody{}");
        assert_eq!(m.imports.len(), 1);
        assert_eq!(m.imports[0].specifier, "base.css");
        assert_eq!(m.code, "body{}");
    }

    #[test]
    fn import_with_media_query() {
        let m = analyze("@import \"print.css\" print and (min-width: 40em);\n");
        assert_eq!(m.imports[0].specifier, "print.css");
        assert_eq!(
            m.imports[0].media.as_deref(),
            Some("print and (min-width: 40em)")
        );
    }

    #[test]
    fn multiple_imports_ordered() {
        let m = analyze("@import \"a.css\";\n@import \"b.css\";\n.x{}");
        assert_eq!(
            m.imports
                .iter()
                .map(|i| i.specifier.as_str())
                .collect::<Vec<_>>(),
            vec!["a.css", "b.css"]
        );
        assert_eq!(m.code, ".x{}");
    }

    #[test]
    fn ignores_import_word_inside_rule() {
        // @import 只在顶层有效；块内的 @import 字样（此处放选择器注释里）不应被当依赖。
        let src = ".a { /* @import \"fake.css\"; */ color: red; }";
        let m = analyze(src);
        assert!(m.imports.is_empty());
        assert_eq!(m.code, src);
    }

    #[test]
    fn ignores_import_like_string_content() {
        let src = ".a::before { content: \"@import x\"; }";
        let m = analyze(src);
        assert!(m.imports.is_empty());
        assert_eq!(m.code, src);
    }

    #[test]
    fn does_not_match_importx() {
        let src = "@importx { color: red; }";
        let m = analyze(src);
        assert!(m.imports.is_empty());
        assert_eq!(m.code, src);
    }

    #[test]
    fn detects_url_quoted() {
        let m = analyze(".a { background: url(\"logo.png\"); }");
        assert_eq!(m.urls.len(), 1);
        assert_eq!(m.urls[0].specifier, "logo.png");
        assert!(m.urls[0].quoted);
        // 记录的区间恰好圈住 code 中的引用文本。
        let u = &m.urls[0];
        assert_eq!(&m.code[u.start..u.end], "logo.png");
    }

    #[test]
    fn detects_url_unquoted() {
        let m = analyze(".a { background: url(logo.png); }");
        assert_eq!(m.urls.len(), 1);
        assert_eq!(m.urls[0].specifier, "logo.png");
        assert!(!m.urls[0].quoted);
        let u = &m.urls[0];
        assert_eq!(&m.code[u.start..u.end], "logo.png");
    }

    #[test]
    fn rewrite_urls_replaces_by_span() {
        let m = analyze(".a { background: url(logo.png); } .b { background: url('bg.jpg'); }");
        let out = m.rewrite_urls(|u| Some(format!("/assets/{}", u.specifier)));
        assert!(out.contains("url(/assets/logo.png)"));
        assert!(out.contains("url('/assets/bg.jpg')"));
    }

    #[test]
    fn rewrite_urls_can_skip() {
        let m = analyze(".a { background: url(data:image/png;base64,AAAA); }");
        // data: 引用保留（返回 None）。
        let out = m.rewrite_urls(|u| {
            if u.specifier.starts_with("data:") {
                None
            } else {
                Some("X".into())
            }
        });
        assert!(out.contains("data:image/png;base64,AAAA"));
    }

    #[test]
    fn import_and_url_together() {
        let src = "@import url(\"reset.css\");\n.a { background: url(bg.png); }";
        let m = analyze(src);
        assert_eq!(m.imports.len(), 1);
        assert_eq!(m.imports[0].specifier, "reset.css");
        // reset.css 是 @import 的 url，不应重复记进 urls。
        assert_eq!(m.urls.len(), 1);
        assert_eq!(m.urls[0].specifier, "bg.png");
    }

    #[test]
    fn utf8_passthrough_safe() {
        let src = ".a::after { content: \"héllo 世界 🌍\"; }";
        let m = analyze(src);
        assert_eq!(m.code, src);
    }

    #[test]
    fn empty_and_whitespace() {
        assert_eq!(analyze("").code, "");
        assert_eq!(analyze("   \n\t").code, "   \n\t");
    }

    // —— CSS Modules ——

    #[test]
    fn modules_scopes_class_selectors() {
        let m = transform_modules(
            ".title { color: red; }\n.box .item { margin: 0; }",
            "a.module.css",
        );
        // 三个类：title / box / item，各被作用域化。
        let map: std::collections::HashMap<_, _> = m.exports.iter().cloned().collect();
        assert!(map.contains_key("title"));
        assert!(map.contains_key("box"));
        assert!(map.contains_key("item"));
        // 改写后 CSS 含作用域名、不含裸 `.title`。
        assert!(m.code.contains(&format!(".{}", map["title"])), "{}", m.code);
        assert!(!m.code.contains(".title "), "{}", m.code);
    }

    #[test]
    fn modules_deterministic_and_distinct_per_seed() {
        let a = transform_modules(".x {}", "foo.module.css");
        let b = transform_modules(".x {}", "foo.module.css");
        let c = transform_modules(".x {}", "bar.module.css");
        assert_eq!(a.exports, b.exports, "同 seed 同名应稳定");
        assert_ne!(a.exports[0].1, c.exports[0].1, "不同文件应不同作用域名");
    }

    #[test]
    fn modules_ignore_dots_in_declarations() {
        // 声明块内的 `.5em`（数值）不应被当类选择器。
        let m = transform_modules(".a { margin: .5em; width: 1.5px; }", "s");
        assert_eq!(m.exports.len(), 1, "只有 .a 一个类");
        assert_eq!(m.exports[0].0, "a");
        assert!(m.code.contains(".5em"), "数值应原样保留:\n{}", m.code);
        assert!(m.code.contains("1.5px"), "{}", m.code);
    }

    #[test]
    fn modules_scope_class_inside_media() {
        // @media 体内的类仍应作用域化（选择器上下文）。
        let m = transform_modules(
            "@media (min-width: 40em) {\n  .responsive { display: flex; }\n}",
            "s",
        );
        assert_eq!(m.exports.len(), 1);
        assert_eq!(m.exports[0].0, "responsive");
        assert!(
            m.code.contains(&format!(".{}", m.exports[0].1)),
            "{}",
            m.code
        );
    }

    #[test]
    fn modules_extracts_imports_too() {
        let m = transform_modules("@import \"./base.css\";\n.card { padding: 4px; }", "s");
        assert_eq!(m.imports.len(), 1);
        assert_eq!(m.imports[0].specifier, "./base.css");
        assert_eq!(m.exports.len(), 1);
        assert!(!m.code.contains("@import"), "{}", m.code);
    }

    #[test]
    fn modules_multiple_classes_one_selector() {
        let m = transform_modules(".a.b { color: red; }", "s");
        // .a 与 .b 都被改写。
        assert_eq!(m.exports.len(), 2);
        let sa = &m.exports.iter().find(|(l, _)| l == "a").unwrap().1;
        let sb = &m.exports.iter().find(|(l, _)| l == "b").unwrap().1;
        assert!(m.code.contains(&format!(".{sa}.{sb}")), "{}", m.code);
    }
}
