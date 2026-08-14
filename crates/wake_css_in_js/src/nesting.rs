//! CSS 嵌套展开：把 `` css`...` `` 的**声明块体**编译为一组平铺规则。
//!
//! 输入是「裸声明 + 嵌套规则」的混合体（无最外层选择器），输出是完整的 CSS 规则序列。
//!
//! ```text
//! 输入 parent=".box"：            输出：
//!   color: red;                    .box{color: red;}
//!   &:hover { color: blue; }       .box:hover{color: blue;}
//!   .icon { width: 1em; }          .box .icon{width: 1em;}
//!   @media (min-width:600px) {     @media (min-width:600px){.box{padding: 8px;}}
//!     padding: 8px;
//!   }
//! ```
//!
//! 展开规则（对齐 CSS Nesting / Sass）：
//! - 嵌套选择器含 `&` → 替换为父选择器（`&:hover` → `.box:hover`）
//! - 不含 `&` → 作后代选择器（`.icon` → `.box .icon`）
//! - 逗号分组逐项展开（`&:hover, &:focus`）
//! - `@media`/`@supports` 等条件 at-rule → 保留外壳，内部按同规则递归
//! - `@font-face` 等 → 原样提升到顶层（其内部不是选择器上下文）
//!
//! 另实现两项 Crab CSS 选择器语义：
//! - **`:global()` 逃逸**：`:global() { … }` 块内的选择器**不加**父类前缀，整体全局生效；
//!   `:global(sel)` 形式则以 `sel` 原样参与（见 [`expand_selector`]）。
//! - **`@keyframes` 名作用域化**：关键帧名加 `-<父选择器去非字母数字>` 后缀，并同步改写
//!   `animation` / `animation-name` 中**本块内已定义**的名字；`:global(name)` 不加后缀。

/// 把 `body`（声明块体）按 `parent` 选择器展开为平铺 CSS。
pub fn flatten(parent: &str, body: &str) -> String {
    // 关键帧作用域化需要先知道本块**定义了哪些**关键帧：只有被定义的名字才在
    // `animation` 值里改写（未定义的名字可能来自全局，不能乱改）。
    let defined = collect_keyframe_names(body);
    let suffix = keyframe_suffix(parent);
    let ctx = KeyframeCtx {
        defined: &defined,
        suffix: &suffix,
    };
    let mut out = String::new();
    flatten_into(parent, body, &ctx, &mut out);
    out
}

/// 关键帧作用域化上下文。
struct KeyframeCtx<'a> {
    /// 本 css 块内定义的关键帧原名（不含 `:global(...)` 的）。
    defined: &'a [String],
    /// 追加的后缀（父选择器去掉非 `[a-zA-Z0-9_-]` 字符）。
    suffix: &'a str,
}

/// 父选择器 → 关键帧后缀。
fn keyframe_suffix(parent: &str) -> String {
    parent
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect()
}

/// 扫描块体（含嵌套）收集 `@keyframes <name>` 定义的名字；`:global(name)` 不收（保持全局）。
fn collect_keyframe_names(body: &str) -> Vec<String> {
    let mut names = Vec::new();
    let bytes = body.as_bytes();
    let marker = b"@keyframes";
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b'@' if bytes[i..].starts_with(marker) => {
                let after = &body[i + marker.len()..];
                let name_part = after.trim_start();
                // `:global(spin)` → 全局关键帧，不作用域化
                if !name_part.starts_with(":global(") {
                    let name: String = name_part
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
                        .collect();
                    if !name.is_empty() && !names.contains(&name) {
                        names.push(name);
                    }
                }
                i += marker.len();
            }
            _ => {}
        }
        i += 1;
    }
    names
}

/// 改写 `@keyframes` 的名字：`name` → `name-<suffix>`；`:global(name)` → `name`。
fn scope_keyframes_prelude(prelude: &str, ctx: &KeyframeCtx) -> String {
    let after = prelude["@keyframes".len()..].trim_start();
    if let Some(inner) = after
        .strip_prefix(":global(")
        .and_then(|s| s.strip_suffix(')'))
    {
        return format!("@keyframes {}", inner.trim());
    }
    let name: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if name.is_empty() || ctx.suffix.is_empty() {
        return prelude.to_string();
    }
    format!("@keyframes {}-{}", name, ctx.suffix)
}

/// 改写一条声明里的动画名。
///
/// 仅对 `animation` / `animation-name`（含厂商前缀）生效；值里出现的
/// **本块已定义**的关键帧名加后缀，`:global(name)` 还原为 `name`，其余原样。
fn scope_animation_decl(decl: &str, ctx: &KeyframeCtx) -> String {
    let Some((prop, value)) = decl.split_once(':') else {
        return decl.to_string();
    };
    let p = prop.trim().trim_start_matches('-').to_ascii_lowercase();
    let p = p
        .strip_prefix("webkit-")
        .or_else(|| p.strip_prefix("moz-"))
        .or_else(|| p.strip_prefix("ms-"))
        .or_else(|| p.strip_prefix("o-"))
        .unwrap_or(&p);
    if p != "animation" && p != "animation-name" {
        return decl.to_string();
    }

    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while !rest.is_empty() {
        // `:global(name)` → name（不加后缀）
        if let Some(idx) = rest.find(":global(") {
            let (before, tail) = rest.split_at(idx);
            out.push_str(&scope_idents(before, ctx));
            let tail = &tail[":global(".len()..];
            match tail.find(')') {
                Some(end) => {
                    out.push_str(tail[..end].trim());
                    rest = &tail[end + 1..];
                }
                None => {
                    out.push_str(tail);
                    rest = "";
                }
            }
        } else {
            out.push_str(&scope_idents(rest, ctx));
            rest = "";
        }
    }
    format!("{prop}:{out}")
}

/// 把文本中出现的「本块已定义关键帧名」整词替换为带后缀的名字。
fn scope_idents(text: &str, ctx: &KeyframeCtx) -> String {
    if ctx.defined.is_empty() || ctx.suffix.is_empty() {
        return text.to_string();
    }
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    let mut out = String::with_capacity(text.len());
    let mut buf = String::new();
    for c in text.chars() {
        if is_word(c) {
            buf.push(c);
        } else {
            flush_ident(&mut out, &mut buf, ctx);
            out.push(c);
        }
    }
    flush_ident(&mut out, &mut buf, ctx);
    out
}

fn flush_ident(out: &mut String, buf: &mut String, ctx: &KeyframeCtx) {
    if buf.is_empty() {
        return;
    }
    if ctx.defined.iter().any(|d| d == buf.as_str()) {
        out.push_str(buf);
        out.push('-');
        out.push_str(ctx.suffix);
    } else {
        out.push_str(buf);
    }
    buf.clear();
}

fn flatten_into(parent: &str, body: &str, ctx: &KeyframeCtx, out: &mut String) {
    let mut decls = String::new();

    let bytes = body.as_bytes();
    let mut i = 0;
    let mut seg_start = 0; // 当前「选择器/声明」片段起点

    while i < bytes.len() {
        match bytes[i] {
            // 字符串：整体跳过，避免其中的 `;{}` 被误判
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            // 注释
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            b';' => {
                let decl = body[seg_start..i].trim();
                if !decl.is_empty() {
                    if parent.is_empty() && decl.starts_with('@') {
                        flush_declarations(parent, &mut decls, out);
                        out.push_str(decl);
                        out.push(';');
                    } else {
                        decls.push_str(&scope_animation_decl(decl, ctx));
                        decls.push(';');
                    }
                }
                i += 1;
                seg_start = i;
            }
            b'{' => {
                let prelude = body[seg_start..i].trim().to_string();
                let (block, next) = read_block(body, i);
                i = next;
                seg_start = i;

                // A nested rule splits the surrounding declaration sequence. Flush before
                // emitting the block so declarations written after it remain after it in the
                // generated CSS; grouping all declarations/nested/at-rules by kind changes the
                // cascade for equal-specificity rules.
                flush_declarations(parent, &mut decls, out);

                if prelude.starts_with('@') {
                    let name = at_rule_name(&prelude);
                    if is_conditional_at_rule(name) {
                        let mut inner = String::new();
                        flatten_into(parent, &block, ctx, &mut inner);
                        if !inner.is_empty() {
                            out.push_str(&prelude);
                            out.push('{');
                            out.push_str(&inner);
                            out.push('}');
                        }
                    } else if name == "keyframes" {
                        // 关键帧：名字作用域化后原样提升（内部是 `from/to/%`，非选择器上下文）。
                        let scoped = scope_keyframes_prelude(&prelude, ctx);
                        out.push_str(&scoped);
                        out.push('{');
                        out.push_str(&block);
                        out.push('}');
                    } else {
                        // @font-face 等：原样提升。
                        out.push_str(&prelude);
                        out.push('{');
                        out.push_str(&block);
                        out.push('}');
                    }
                } else if !prelude.is_empty() {
                    // `:global()`（空括号）块：其内容整体逃逸出类作用域，以**空父选择器**递归。
                    if is_bare_global(&prelude) {
                        flatten_into("", &block, ctx, out);
                    } else {
                        let expanded = expand_selector(parent, &prelude);
                        flatten_into(&expanded, &block, ctx, out);
                    }
                }
            }
            _ => i += 1,
        }
    }
    // 末尾未以 `;` 收尾的声明
    let tail = body[seg_start.min(body.len())..].trim();
    if !tail.is_empty() && !tail.starts_with('@') {
        decls.push_str(&scope_animation_decl(tail, ctx));
        decls.push(';');
    }
    flush_declarations(parent, &mut decls, out);
}

fn flush_declarations(parent: &str, declarations: &mut String, out: &mut String) {
    // 全局作用域（`:global()` 块内）没有选择器可挂，裸声明只能丢弃——写出来是非法 CSS。
    if !declarations.is_empty() && !parent.is_empty() {
        out.push_str(parent);
        out.push('{');
        out.push_str(declarations);
        out.push('}');
    }
    declarations.clear();
}

/// 从 `body[open]` 处的 `{` 读到配对的 `}`，返回（块内文本, `}` 之后的下标）。
fn read_block(body: &str, open: usize) -> (String, usize) {
    let bytes = body.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    let start = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return (body[start..i].to_string(), i + 1);
                }
            }
            _ => {}
        }
        i += 1;
    }
    // 未闭合：吃到结尾（容错，不 panic）
    (body[start.min(body.len())..].to_string(), bytes.len())
}

/// 该选择器是否是「裸 `:global()`」——空括号形式，用于把整块内容逃逸到全局。
fn is_bare_global(sel: &str) -> bool {
    let s = sel.trim();
    s == ":global()" || s.replace(char::is_whitespace, "") == ":global()"
}

/// 展开嵌套选择器：`&` 替换为父选择器；无 `&` 则作后代。逗号分组逐项处理。
///
/// `:global(sel)` 的部分**原样取出 sel 且不加父前缀**（全局逃逸）；父选择器为空
/// （处于 `:global()` 块内）时同样不加前缀。
fn expand_selector(parent: &str, sel: &str) -> String {
    let parents = split_selector_list(parent);
    let mut expanded = Vec::new();

    for part in split_selector_list(sel) {
        let child = part.trim();
        // `:global(x)` → `x`，且该项不受父作用域约束。它不与父选择器做笛卡尔积，
        // 否则同一条全局选择器会被重复输出一次/父分支。
        if let Some(inner) = extract_global(child) {
            expanded.push(inner);
            continue;
        }

        if parent.is_empty() {
            expanded.push(child.replace('&', ""));
            continue;
        }

        // `parent` 本身可能是上一层嵌套展开得到的 selector list。每个 child 必须分别与
        // 每个 parent 组合；直接把整个 `parent` 字符串替换进 `&` 会让 `:hover` / `.child`
        // 等后缀只落到逗号列表的最后一支。
        for parent in &parents {
            let parent = parent.trim();
            if child.contains('&') {
                expanded.push(child.replace('&', parent));
            } else {
                expanded.push(format!("{parent} {child}"));
            }
        }
    }

    expanded.join(",")
}

/// Split only selector-list commas. Commas inside functional pseudos (`:is()`, `:not()`),
/// attribute selectors, strings, or comments belong to one selector and must remain untouched.
fn split_selector_list(selector: &str) -> Vec<&str> {
    let bytes = selector.as_bytes();
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut paren_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut i = 0usize;

    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => {
                let quote = bytes[i];
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\\' {
                        i += 2;
                        continue;
                    }
                    if bytes[i] == quote {
                        break;
                    }
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += usize::from(i < bytes.len());
            }
            b'(' => paren_depth += 1,
            b')' => paren_depth = paren_depth.saturating_sub(1),
            b'[' => bracket_depth += 1,
            b']' => bracket_depth = bracket_depth.saturating_sub(1),
            b',' if paren_depth == 0 && bracket_depth == 0 => {
                parts.push(&selector[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    parts.push(&selector[start..]);
    parts
}

/// 取出 `:global(sel)` 里的 `sel`（并拼回其前后的其余部分）；非 `:global` 形式返回 `None`。
fn extract_global(part: &str) -> Option<String> {
    let idx = part.find(":global(")?;
    let before = part[..idx].trim();
    let tail = &part[idx + ":global(".len()..];
    let end = tail.find(')')?;
    let inner = tail[..end].trim();
    let after = tail[end + 1..].trim();
    let mut out = String::new();
    if !before.is_empty() {
        out.push_str(before);
    }
    out.push_str(inner);
    if !after.is_empty() {
        if !out.is_empty() && !after.starts_with([':', '.', '[']) {
            out.push(' ');
        }
        out.push_str(after);
    }
    Some(out.trim().to_string())
}

/// 取 at-rule 名（`@media (…)` → `media`）。
fn at_rule_name(prelude: &str) -> &str {
    let s = prelude.trim_start_matches('@');
    let end = s
        .find(|c: char| c.is_whitespace() || c == '(')
        .unwrap_or(s.len());
    &s[..end]
}

/// 条件组 at-rule：其块内仍是**选择器上下文**，需按父选择器递归展开。
fn is_conditional_at_rule(name: &str) -> bool {
    matches!(
        name,
        "media" | "supports" | "container" | "layer" | "scope" | "document"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_declarations() {
        let css = flatten(".box", "color: red;\n  padding: 8px;");
        assert_eq!(css, ".box{color: red;padding: 8px;}");
    }

    #[test]
    fn global_statement_at_rules_are_preserved_in_source_order() {
        assert_eq!(
            flatten(
                "",
                "@layer reset, theme; @layer reset { body { margin: 0; } }"
            ),
            "@layer reset, theme;@layer reset{body{margin: 0;}}"
        );
    }

    #[test]
    fn declaration_without_trailing_semicolon() {
        let css = flatten(".box", "color: red");
        assert_eq!(css, ".box{color: red;}");
    }

    #[test]
    fn ampersand_pseudo_class() {
        let css = flatten(".box", "color: red; &:hover { color: blue; }");
        assert_eq!(css, ".box{color: red;}.box:hover{color: blue;}");
    }

    #[test]
    fn declarations_and_nested_rules_keep_source_order() {
        let css = flatten(
            ".box",
            "color: red; & { color: blue; } color: rebeccapurple;",
        );
        assert_eq!(
            css,
            ".box{color: red;}.box{color: blue;}.box{color: rebeccapurple;}"
        );
    }

    #[test]
    fn bare_nested_selector_becomes_descendant() {
        let css = flatten(".box", ".icon { width: 1em; }");
        assert_eq!(css, ".box .icon{width: 1em;}");
    }

    #[test]
    fn child_combinator() {
        let css = flatten(".box", "& > .item { gap: 4px; }");
        assert_eq!(css, ".box > .item{gap: 4px;}");
    }

    #[test]
    fn comma_group_expands_each() {
        let css = flatten(".box", "&:hover, &:focus { outline: none; }");
        assert_eq!(css, ".box:hover,.box:focus{outline: none;}");
    }

    #[test]
    fn commas_inside_functional_pseudos_do_not_split_the_selector_list() {
        assert_eq!(
            flatten(".box", "&:is(:hover, :focus) { color: red; }"),
            ".box:is(:hover, :focus){color: red;}"
        );
        assert_eq!(
            flatten(".box", "&[data-label='a,b'], &:not(.x, .y) { color: red; }"),
            ".box[data-label='a,b'],.box:not(.x, .y){color: red;}"
        );
    }

    #[test]
    fn nested_selector_lists_expand_as_a_cartesian_product() {
        assert_eq!(
            flatten(
                ".box",
                "& .a, & .b { &:hover { color: red; } .child { color: blue; } }"
            ),
            ".box .a:hover,.box .b:hover{color: red;}\
.box .a .child,.box .b .child{color: blue;}"
        );

        // Functional-pseudo commas remain inside one selector while top-level child selectors
        // still combine with every parent branch.
        assert_eq!(
            flatten(
                ".box",
                "& .a, & .b { &:is(:hover, :focus), &[aria-current] { color: red; } }"
            ),
            ".box .a:is(:hover, :focus),.box .b:is(:hover, :focus),\
.box .a[aria-current],.box .b[aria-current]{color: red;}"
        );
    }

    #[test]
    fn media_wraps_and_recurses() {
        let css = flatten(".box", "@media (min-width: 600px) { padding: 8px; }");
        assert_eq!(css, "@media (min-width: 600px){.box{padding: 8px;}}");
    }

    #[test]
    fn media_with_nested_selector() {
        let css = flatten(".box", "@media screen { &:hover { color: red; } }");
        assert_eq!(css, "@media screen{.box:hover{color: red;}}");
    }

    #[test]
    fn keyframes_are_scoped_and_animation_reference_rewritten() {
        let css = flatten(
            ".box_a1",
            "animation: spin 1s;@keyframes spin { from { transform: rotate(0) } to { transform: rotate(360deg) } }",
        );
        // 关键帧名加后缀（后缀 = 父选择器去掉非字母数字 → `box_a1`）
        assert!(css.contains("@keyframes spin-box_a1{"), "{css}");
        // 引用处同步改写
        assert!(css.contains("animation: spin-box_a1 1s;"), "{css}");
        // keyframes 内部不得被当作选择器展开（不能出现 `.box_a1 from`）
        assert!(!css.contains("from{"), "内部不应被展开为选择器: {css}");
    }

    #[test]
    fn global_keyframes_are_not_scoped() {
        let css = flatten(
            ".box_a1",
            "animation: spin 1s;@keyframes :global(spin) { from { opacity: 0 } }",
        );
        assert!(
            css.contains("@keyframes spin{"),
            "全局关键帧不应加后缀: {css}"
        );
        // 未在本块「作用域化定义」的名字，引用处也不改写
        assert!(css.contains("animation: spin 1s;"), "{css}");
    }

    #[test]
    fn animation_name_property_is_rewritten() {
        let css = flatten(
            ".b",
            "animation-name: pulse;@keyframes pulse{from{opacity:0}}",
        );
        assert!(css.contains("animation-name: pulse-b;"), "{css}");
        assert!(css.contains("@keyframes pulse-b{"), "{css}");
    }

    #[test]
    fn undefined_animation_names_are_left_alone() {
        // 本块没定义 `spin` → 可能来自全局，不得改写
        let css = flatten(".b", "animation: spin 1s linear infinite;");
        assert!(css.contains("animation: spin 1s linear infinite;"), "{css}");
    }

    #[test]
    fn keyframe_markers_inside_strings_and_comments_are_ignored() {
        let css = flatten(
            ".b",
            r#"content: "@keyframes fake"; /* @keyframes also-fake */ animation: fake 1s;"#,
        );
        assert!(css.contains("animation: fake 1s;"), "{css}");
        assert!(!css.contains("fake-b"), "{css}");
    }

    #[test]
    fn global_in_animation_value_is_unwrapped() {
        let css = flatten(
            ".b",
            "animation: :global(spin) 1s;@keyframes spin{from{opacity:0}}",
        );
        // `:global(spin)` 还原为 spin 且不加后缀，即使本块定义了同名关键帧
        assert!(css.contains("animation: spin 1s;"), "{css}");
        assert!(css.contains("@keyframes spin-b{"), "{css}");
    }

    #[test]
    fn bare_global_block_escapes_class_scope() {
        // 真实用法：`:global() { html, body { … } }`
        let css = flatten(
            ".box_a1",
            ":global() { html, body, #root { margin: 0; height: 100%; } }",
        );
        assert_eq!(css, "html,body,#root{margin: 0;height: 100%;}");
        assert!(!css.contains(".box_a1"), "不应带类前缀: {css}");
    }

    #[test]
    fn global_selector_form_drops_parent_prefix() {
        let css = flatten(".box", ":global(.ant-btn) { color: red; }");
        assert_eq!(css, ".ant-btn{color: red;}");
    }

    #[test]
    fn global_block_coexists_with_scoped_rules() {
        let css = flatten(
            ".box",
            "color: red; :global() { body { margin: 0; } } &:hover { color: blue; }",
        );
        assert!(css.contains(".box{color: red;}"), "{css}");
        assert!(css.contains("body{margin: 0;}"), "{css}");
        assert!(css.contains(".box:hover{color: blue;}"), "{css}");
    }

    #[test]
    fn deep_nesting() {
        let css = flatten(".box", "& .a { & .b { color: red; } }");
        assert_eq!(css, ".box .a .b{color: red;}");
    }

    #[test]
    fn semicolons_inside_strings_are_not_split() {
        let css = flatten(".box", r#"content: "a;b";"#);
        assert_eq!(css, r#".box{content: "a;b";}"#);
    }

    #[test]
    fn comments_are_skipped() {
        let css = flatten(".box", "/* c; { } */ color: red;");
        assert!(css.contains("color: red"), "{css}");
    }

    #[test]
    fn empty_body_emits_nothing() {
        assert_eq!(flatten(".box", "   \n  "), "");
    }

    #[test]
    fn empty_media_emits_nothing() {
        assert_eq!(flatten(".box", "@media print { }"), "");
    }
}
