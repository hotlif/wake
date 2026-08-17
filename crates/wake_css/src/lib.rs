//! # wake_css — Wake 的共享 CSS 语法层（DESIGN §8.1）
//!
//! [`syntax::CssSyntaxTree`] 是编译器、CSS-in-JS 与编辑器语言服务共同使用的 CSS CST。
//! 打包侧基于这棵树完成两件事：
//!
//! - `@import`：依赖提取——进模块图统一去重排序（顶部 @import 语句从产物中**移除**，
//!   由驱动层转成 JS `import` 让模块图处理顺序与去重）；
//! - `url()`：资源引用——记录其在**输出** `code` 中的字节区间，供资源改写（6.4）原地替换。
//!
//! 其余内容（选择器、声明、注释、字符串）**原样透传**。所有结构判断都来自共享 CST；
//! 输出阶段只按节点提供的源码区间应用编辑，不再维护第二套字符扫描器。

pub mod syntax;

use cssparser::TokenSerializationType;
use syntax::{
    CssBlockKind, CssSyntaxItem, CssSyntaxItemKind, CssSyntaxKind, CssSyntaxNode, CssSyntaxTree,
};
use wake_common::Span;

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

/// 压缩一段 CSS（prod，WAKE-COMPATIBILITY §M4c）。**安全子集**：折叠空白为单空格、去注释、
/// 删 `{` `}` `;` `,` 相邻空白、删 `}` 前多余 `;`；字符串内原样。
///
/// 刻意**不**删的（避免破坏语义）：后代组合器空白（`.a .b`）、`calc(1px + 2px)` 等值内空白、
/// `>`/`+`/`~` 组合器周围空白、`prop: value` 冒号后空白——这些删了会改变含义或非法。
pub fn minify(src: &str) -> String {
    let tree = CssSyntaxTree::parse(src, Span::new(0, src.len() as u32));
    let mut writer = CssMinifier::new(src);
    writer.write_nodes(&tree.nodes);
    writer.finish()
}

struct CssMinifier<'a> {
    source: &'a str,
    output: String,
    pending_space: bool,
    suppress_space: bool,
    previous_token: TokenSerializationType,
}

impl<'a> CssMinifier<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            output: String::with_capacity(source.len()),
            pending_space: false,
            suppress_space: true,
            previous_token: TokenSerializationType::Nothing,
        }
    }

    fn write_nodes(&mut self, nodes: &[CssSyntaxNode]) {
        for node in nodes {
            match node.kind {
                CssSyntaxKind::Comment => {}
                CssSyntaxKind::Whitespace => self.pending_space = true,
                CssSyntaxKind::Comma | CssSyntaxKind::Semicolon => {
                    self.pending_space = false;
                    self.push_token(node.span, node.serialization_type);
                    self.suppress_space = true;
                }
                CssSyntaxKind::Block(CssBlockKind::Curly) => self.write_curly_block(node),
                _ if node.block_kind().is_some() => self.write_inline_block(node),
                _ => {
                    self.push_token(node.span, node.serialization_type);
                    self.suppress_space = false;
                }
            }
        }
    }

    fn write_curly_block(&mut self, node: &CssSyntaxNode) {
        self.pending_space = false;
        self.push_token(node.head_span, node.serialization_type);
        self.suppress_space = true;
        self.write_nodes(&node.children);
        self.pending_space = false;
        if self.output.ends_with(';') {
            self.output.pop();
        }
        self.push_closing_delimiter(node);
        self.previous_token = node.serialization_type;
        self.suppress_space = true;
    }

    fn write_inline_block(&mut self, node: &CssSyntaxNode) {
        self.push_token(node.head_span, node.serialization_type);
        self.suppress_space = false;
        self.write_nodes(&node.children);
        self.flush_space();
        self.push_closing_delimiter(node);
        self.previous_token = node.serialization_type;
        self.suppress_space = false;
    }

    fn flush_space(&mut self) {
        if self.pending_space && !self.suppress_space {
            self.output.push(' ');
            self.previous_token = TokenSerializationType::WhiteSpace;
        }
        self.pending_space = false;
    }

    fn push_token(&mut self, span: Span, token: TokenSerializationType) {
        self.flush_space();
        if self.previous_token.needs_separator_when_before(token) {
            self.output.push_str("/**/");
        }
        self.push_span(span);
        self.previous_token = token;
    }

    fn push_span(&mut self, span: Span) {
        if let Some(text) = source_slice(self.source, span) {
            self.output.push_str(text);
        }
    }

    fn push_closing_delimiter(&mut self, node: &CssSyntaxNode) {
        if node.closed && node.span.hi > node.head_span.hi {
            self.push_span(Span::new(node.span.hi - 1, node.span.hi));
        }
    }

    fn finish(self) -> String {
        self.output
    }
}

#[derive(Clone)]
struct ImportRecord {
    import: CssImport,
    span: Span,
}

#[derive(Clone)]
struct SourceEdit {
    span: Span,
    replacement: String,
}

/// 分析一段 CSS：从共享 CST 提取 `@import` 依赖和 `url()` 引用，其余透传到 `code`。
pub fn analyze(src: &str) -> CssModule {
    let tree = CssSyntaxTree::parse(src, Span::new(0, src.len() as u32));
    let import_records = collect_imports(src, &tree.nodes);
    let removals = import_records
        .iter()
        .map(|record| record.span)
        .collect::<Vec<_>>();
    let code = apply_edits(
        src,
        import_records
            .iter()
            .map(|record| SourceEdit {
                span: record.span,
                replacement: String::new(),
            })
            .collect(),
    );
    let mut source_urls = Vec::new();
    collect_urls(&tree.nodes, &removals, &mut source_urls);
    let urls = source_urls
        .into_iter()
        .map(|url| CssUrl {
            specifier: url.specifier,
            start: output_offset(url.span.lo as usize, &removals),
            end: output_offset(url.span.hi as usize, &removals),
            quoted: url.quoted,
        })
        .collect();

    CssModule {
        imports: import_records
            .into_iter()
            .map(|record| record.import)
            .collect(),
        urls,
        code,
    }
}

fn collect_imports(source: &str, nodes: &[CssSyntaxNode]) -> Vec<ImportRecord> {
    let mut records = Vec::new();
    let mut statement_start = true;
    let mut index = 0usize;
    while index < nodes.len() {
        let node = &nodes[index];
        if node.is_trivia() {
            index += 1;
            continue;
        }
        if statement_start
            && matches!(&node.kind, CssSyntaxKind::AtKeyword(name) if name.eq_ignore_ascii_case("import"))
        {
            let end_index = nodes[index + 1..]
                .iter()
                .position(|candidate| matches!(candidate.kind, CssSyntaxKind::Semicolon))
                .map_or(nodes.len() - 1, |offset| index + 1 + offset);
            let rule_nodes = &nodes[index + 1..=end_index];
            let value = rule_nodes.iter().find(|candidate| !candidate.is_trivia());
            let (specifier, value_end) = value
                .map(|value| import_value(source, value))
                .unwrap_or_else(|| (String::new(), node.span.hi));
            let statement_end = nodes[end_index].span.hi;
            let media_end = if matches!(nodes[end_index].kind, CssSyntaxKind::Semicolon) {
                nodes[end_index].span.lo
            } else {
                statement_end
            };
            let media = source_slice(source, Span::new(value_end, media_end))
                .map(str::trim)
                .filter(|media| !media.is_empty())
                .map(str::to_string);
            let removal_end = nodes
                .get(end_index + 1)
                .filter(|next| next.is_trivia() && next.span.lo == statement_end)
                .map_or(statement_end, |next| {
                    consume_import_line_break(source, statement_end, next.span.hi)
                });
            records.push(ImportRecord {
                import: CssImport { specifier, media },
                span: Span::new(node.span.lo, removal_end),
            });
            index = end_index + 1;
            statement_start = true;
            continue;
        }
        statement_start = matches!(
            node.kind,
            CssSyntaxKind::Semicolon | CssSyntaxKind::Block(CssBlockKind::Curly)
        );
        index += 1;
    }
    records
}

fn import_value(source: &str, node: &CssSyntaxNode) -> (String, u32) {
    match &node.kind {
        CssSyntaxKind::QuotedString(value) | CssSyntaxKind::Url(value) => {
            (value.clone(), node.span.hi)
        }
        CssSyntaxKind::Function(name) if name.eq_ignore_ascii_case("url") => {
            let value = node.children.iter().find(|child| !child.is_trivia());
            let specifier = value
                .and_then(css_string_value)
                .unwrap_or_default()
                .to_string();
            (specifier, node.span.hi)
        }
        _ => (
            source_slice(source, node.span)
                .unwrap_or_default()
                .to_string(),
            node.span.hi,
        ),
    }
}

fn consume_import_line_break(source: &str, start: u32, whitespace_end: u32) -> u32 {
    let Some(whitespace) = source_slice(source, Span::new(start, whitespace_end)) else {
        return start;
    };
    let horizontal = whitespace
        .chars()
        .take_while(|character| matches!(character, ' ' | '\t'))
        .map(char::len_utf8)
        .sum::<usize>();
    let rest = &whitespace[horizontal..];
    let line_break = if rest.starts_with("\r\n") {
        2
    } else if rest.starts_with(['\r', '\n']) {
        1
    } else {
        0
    };
    start + (horizontal + line_break) as u32
}

struct SourceUrl {
    specifier: String,
    span: Span,
    quoted: bool,
}

fn collect_urls(nodes: &[CssSyntaxNode], removals: &[Span], output: &mut Vec<SourceUrl>) {
    for node in nodes {
        if removals.iter().any(|span| span.contains(node.span)) {
            continue;
        }
        match &node.kind {
            CssSyntaxKind::Url(value) => {
                if let Some(span) = node.value_span {
                    output.push(SourceUrl {
                        specifier: value.clone(),
                        span,
                        quoted: false,
                    });
                }
            }
            CssSyntaxKind::Function(name) if name.eq_ignore_ascii_case("url") => {
                if let Some(value) = node.children.iter().find(|child| !child.is_trivia())
                    && let Some(specifier) = css_string_value(value)
                    && let Some(span) = value.value_span
                {
                    output.push(SourceUrl {
                        specifier: specifier.to_string(),
                        span,
                        quoted: matches!(value.kind, CssSyntaxKind::QuotedString(_)),
                    });
                }
            }
            _ => collect_urls(&node.children, removals, output),
        }
    }
}

fn css_string_value(node: &CssSyntaxNode) -> Option<&str> {
    match &node.kind {
        CssSyntaxKind::QuotedString(value) | CssSyntaxKind::Url(value) => Some(value),
        _ => None,
    }
}

fn output_offset(source_offset: usize, removals: &[Span]) -> usize {
    source_offset
        - removals
            .iter()
            .filter(|span| span.hi as usize <= source_offset)
            .map(|span| (span.hi - span.lo) as usize)
            .sum::<usize>()
}

fn source_slice(source: &str, span: Span) -> Option<&str> {
    source.get(span.lo as usize..span.hi as usize)
}

fn apply_edits(source: &str, mut edits: Vec<SourceEdit>) -> String {
    edits.sort_by_key(|edit| (edit.span.lo, std::cmp::Reverse(edit.span.hi)));
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0usize;
    for edit in edits {
        let start = edit.span.lo as usize;
        let end = edit.span.hi as usize;
        if start < cursor || end < start || end > source.len() {
            continue;
        }
        output.push_str(&source[cursor..start]);
        output.push_str(&edit.replacement);
        cursor = end;
    }
    output.push_str(&source[cursor..]);
    output
}

// ======================================================================
// CSS Modules（`.module.css` 类名局部作用域，PLAN §6.3）
// ======================================================================

/// CSS Modules 转换结果。
#[derive(Debug, Clone, Default)]
pub struct CssModulesResult {
    /// `@import` 依赖（与 [`analyze`] 同）。
    pub imports: Vec<CssImport>,
    /// `url()` 引用，位置已映射到转换后的 [`Self::code`]。
    pub urls: Vec<CssUrl>,
    /// 局部类名 → 作用域化类名（按首次出现顺序、去重）。
    pub exports: Vec<(String, String)>,
    /// 改写后的 CSS：类选择器 `.foo` → `.foo_<hash>`，`@import` 已移除。
    pub code: String,
}

/// 把一个 `.module.css` 源转换为「类名作用域化」的 CSS + 导出映射。
///
/// - 类选择器 `.foo` → `.foo_<hash>`（`hash` 由 `seed`（通常是文件路径）+ 局部名决定，
///   同文件同名稳定、跨文件不撞）；构建 `局部名 → 作用域名` 映射供 JS `import styles` 使用。
/// - 正确区分**选择器上下文**（顶层 / `@media`·`@supports`·`@container`·`@layer` 体）与
///   **声明块**（`.foo { }` 体内、`@keyframes`/`@font-face` 体），只在前者改写 `.`。
/// - `@import` 与 `url()` 依赖提取同 [`analyze`]，URL span 映射到所有作用域编辑之后。
///
/// 未覆盖（后续）：`#id` 作用域、`composes`、keyframes 名作用域。
pub fn transform_modules(src: &str, seed: &str) -> CssModulesResult {
    let tree = CssSyntaxTree::parse(src, Span::new(0, src.len() as u32));
    let import_records = collect_imports(src, &tree.nodes);
    let mut exports: Vec<(String, String)> = Vec::new();
    let mut edits = import_records
        .iter()
        .map(|record| SourceEdit {
            span: record.span,
            replacement: String::new(),
        })
        .collect::<Vec<_>>();
    collect_module_rules(&tree.nodes, &tree.items, seed, &mut exports, &mut edits);
    let removals = import_records
        .iter()
        .map(|record| record.span)
        .collect::<Vec<_>>();
    let mut source_urls = Vec::new();
    collect_urls(&tree.nodes, &removals, &mut source_urls);
    let urls = source_urls
        .into_iter()
        .map(|url| CssUrl {
            specifier: url.specifier,
            start: output_offset_after_edits(url.span.lo as usize, &edits),
            end: output_offset_after_edits(url.span.hi as usize, &edits),
            quoted: url.quoted,
        })
        .collect();

    CssModulesResult {
        imports: import_records
            .into_iter()
            .map(|record| record.import)
            .collect(),
        urls,
        exports,
        code: apply_edits(src, edits),
    }
}

fn output_offset_after_edits(source_offset: usize, edits: &[SourceEdit]) -> usize {
    let delta = edits
        .iter()
        .filter(|edit| edit.span.hi as usize <= source_offset)
        .map(|edit| edit.replacement.len() as i64 - i64::from(edit.span.hi - edit.span.lo))
        .sum::<i64>();
    (source_offset as i64 + delta).max(0) as usize
}

fn collect_module_rules(
    nodes: &[CssSyntaxNode],
    items: &[CssSyntaxItem],
    seed: &str,
    exports: &mut Vec<(String, String)>,
    edits: &mut Vec<SourceEdit>,
) {
    for item in items {
        if matches!(item.kind, CssSyntaxItemKind::QualifiedRule) {
            collect_selector_classes(item.nodes(nodes), true, seed, exports, edits);
        }
        if let Some(block) = item.block(nodes) {
            collect_module_rules(&block.children, &item.children, seed, exports, edits);
        }
    }
}

fn collect_selector_classes(
    nodes: &[CssSyntaxNode],
    local: bool,
    seed: &str,
    exports: &mut Vec<(String, String)>,
    edits: &mut Vec<SourceEdit>,
) {
    let mut index = 0usize;
    while index < nodes.len() {
        let node = &nodes[index];
        if matches!(node.kind, CssSyntaxKind::Colon)
            && let Some((function_index, function)) = nodes
                .iter()
                .enumerate()
                .skip(index + 1)
                .find(|(_, candidate)| !candidate.is_trivia())
            && let CssSyntaxKind::Function(name) = &function.kind
            && (name.eq_ignore_ascii_case("global") || name.eq_ignore_ascii_case("local"))
        {
            edits.push(SourceEdit {
                span: Span::new(node.span.lo, function.head_span.hi),
                replacement: String::new(),
            });
            if function.closed {
                edits.push(SourceEdit {
                    span: Span::new(function.span.hi - 1, function.span.hi),
                    replacement: String::new(),
                });
            }
            collect_selector_classes(
                &function.children,
                name.eq_ignore_ascii_case("local"),
                seed,
                exports,
                edits,
            );
            index = function_index + 1;
            continue;
        }
        if local
            && matches!(node.kind, CssSyntaxKind::Delim('.'))
            && let Some(identifier) = nodes[index + 1..].iter().find(|node| !node.is_trivia())
            && let CssSyntaxKind::Ident(local) = &identifier.kind
        {
            let scoped = scoped_name(seed, local);
            edits.push(SourceEdit {
                span: identifier.span,
                replacement: scoped.clone(),
            });
            if !exports.iter().any(|(name, _)| name == local) {
                exports.push((local.clone(), scoped));
            }
        }
        collect_selector_classes(&node.children, local, seed, exports, edits);
        index += 1;
    }
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
    fn minify_preserves_token_boundaries_when_dropping_comments() {
        assert_eq!(
            minify(".a { font-family: red/**/blue; width: 10/**/px; }"),
            ".a{font-family: red/**/blue;width: 10/**/px}"
        );
        assert_eq!(minify("p/**/.class { color: red; }"), "p.class{color: red}");
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
    fn parser_decodes_escaped_import_keyword() {
        let m = analyze(
            r#"@\69mport "reset.css";
.a {}"#,
        );
        assert_eq!(m.imports[0].specifier, "reset.css");
        assert_eq!(m.code, ".a {}");
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
    fn url_detection_uses_function_nodes_and_ignores_text_lookalikes() {
        let source = r#".a { content: "url(fake.png)"; /* url(comment.png) */ background: u\72l("logo.png"); }"#;
        let module = analyze(source);
        assert_eq!(module.urls.len(), 1);
        assert_eq!(module.urls[0].specifier, "logo.png");
        let url = &module.urls[0];
        assert_eq!(&module.code[url.start..url.end], "logo.png");
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

    #[test]
    fn modules_scope_decoded_escaped_class_identifiers() {
        let module = transform_modules(r#".t\69 tle { color: red; }"#, "s");
        assert_eq!(module.exports[0].0, "title");
        assert!(
            module.code.contains(&format!(".{}", module.exports[0].1)),
            "{}",
            module.code
        );
        assert!(!module.code.contains(r#".t\69 tle"#));
    }

    #[test]
    fn modules_use_ast_scope_for_global_and_local_selectors() {
        let module = transform_modules(
            ":global(.reset) .card, :local(.explicit) { color: red; }",
            "s",
        );
        let map: std::collections::HashMap<_, _> = module.exports.iter().cloned().collect();
        assert!(!map.contains_key("reset"));
        assert!(map.contains_key("card"));
        assert!(map.contains_key("explicit"));
        assert!(!module.code.contains(":global"), "{}", module.code);
        assert!(!module.code.contains(":local"), "{}", module.code);
        assert!(module.code.contains(".reset"), "{}", module.code);
        assert!(
            module.code.contains(&format!(".{}", map["card"])),
            "{}",
            module.code
        );
    }

    #[test]
    fn modules_do_not_treat_custom_property_blocks_as_nested_rules() {
        let module =
            transform_modules(".root { --theme: { .value }; .child { color: red; } }", "s");
        let names = module
            .exports
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"root"), "{:?}", module.exports);
        assert!(names.contains(&"child"), "{:?}", module.exports);
        assert!(!names.contains(&"value"), "{:?}", module.exports);
    }

    #[test]
    fn modules_map_url_spans_through_ast_edits_without_reparsing() {
        let module = transform_modules(
            "@import 'base.css';\n:local(.hero) { background: url(\"./hero.png\"); }",
            "s",
        );
        assert_eq!(module.urls.len(), 1);
        let url = &module.urls[0];
        assert_eq!(url.specifier, "./hero.png");
        assert_eq!(&module.code[url.start..url.end], "./hero.png");
    }
}
