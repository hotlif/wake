//! CSS nesting expansion driven by the shared `wake_css` concrete syntax tree.

use wake_common::Span;
use wake_css::syntax::{
    CssSyntaxContext, CssSyntaxItem, CssSyntaxItemKind, CssSyntaxKind, CssSyntaxNode, CssSyntaxTree,
};

pub fn flatten(parent: &str, body: &str) -> String {
    let context = if parent.is_empty() {
        CssSyntaxContext::Stylesheet
    } else {
        CssSyntaxContext::StyleBlock
    };
    let tree = CssSyntaxTree::parse_with_context(body, Span::new(0, body.len() as u32), context);
    flatten_tree(parent, body, &tree)
}

pub fn flatten_tree(parent: &str, body: &str, tree: &CssSyntaxTree) -> String {
    let defined = collect_keyframe_names(body, &tree.nodes, &tree.items);
    let suffix = keyframe_suffix(parent);
    let ctx = KeyframeCtx {
        defined: &defined,
        suffix: &suffix,
    };
    let parents = selector_parts(parent);
    let mut out = String::new();
    flatten_items(&parents, body, &tree.nodes, &tree.items, &ctx, &mut out);
    out
}

struct KeyframeCtx<'a> {
    defined: &'a [String],
    suffix: &'a str,
}

fn keyframe_suffix(parent: &str) -> String {
    parent
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        .collect()
}

fn collect_keyframe_names(
    source: &str,
    nodes: &[CssSyntaxNode],
    items: &[CssSyntaxItem],
) -> Vec<String> {
    let mut names = Vec::new();
    collect_keyframes(source, nodes, items, &mut names);
    names
}

fn collect_keyframes(
    source: &str,
    nodes: &[CssSyntaxNode],
    items: &[CssSyntaxItem],
    names: &mut Vec<String>,
) {
    for item in items {
        if matches!(&item.kind, CssSyntaxItemKind::AtRule { name } if name.eq_ignore_ascii_case("keyframes"))
            && let Some((name, global, _)) = keyframe_name(source, item.nodes(nodes))
            && !global
            && !names.contains(&name)
        {
            names.push(name);
        }
        if let Some(block) = item.block(nodes) {
            collect_keyframes(source, &block.children, &item.children, names);
        }
    }
}

fn flatten_items(
    parents: &[String],
    source: &str,
    nodes: &[CssSyntaxNode],
    items: &[CssSyntaxItem],
    ctx: &KeyframeCtx<'_>,
    out: &mut String,
) {
    let mut declarations = String::new();
    for item in items {
        match &item.kind {
            CssSyntaxItemKind::Declaration(declaration) => {
                if !parents.is_empty() {
                    let declaration_nodes = item.nodes(nodes);
                    let span = nodes_span(declaration_nodes, declaration.span);
                    declarations.push_str(&scope_animation_declaration(
                        source,
                        declaration_nodes,
                        span,
                        ctx,
                    ));
                    declarations.push(';');
                }
            }
            CssSyntaxItemKind::AtRule { name } if item.block_index.is_none() => {
                flush_declarations(parents, &mut declarations, out);
                out.push_str(slice(source, item.span).trim());
            }
            CssSyntaxItemKind::AtRule { name } => {
                flush_declarations(parents, &mut declarations, out);
                let Some(block) = item.block(nodes) else {
                    continue;
                };
                let prelude_nodes = item.nodes(nodes);
                let prelude_span = nodes_span(prelude_nodes, item.span);
                if is_conditional_at_rule(name) {
                    let mut inner = String::new();
                    flatten_items(
                        parents,
                        source,
                        &block.children,
                        &item.children,
                        ctx,
                        &mut inner,
                    );
                    if !inner.is_empty() {
                        out.push_str(slice(source, prelude_span).trim());
                        out.push('{');
                        out.push_str(&inner);
                        out.push('}');
                    }
                } else if name.eq_ignore_ascii_case("keyframes") {
                    out.push_str(&scope_keyframes_prelude(
                        source,
                        prelude_nodes,
                        prelude_span,
                        ctx,
                    ));
                    out.push('{');
                    out.push_str(slice(source, block.content_span()));
                    out.push('}');
                } else {
                    out.push_str(slice(source, prelude_span).trim());
                    out.push('{');
                    out.push_str(slice(source, block.content_span()));
                    out.push('}');
                }
            }
            CssSyntaxItemKind::QualifiedRule => {
                flush_declarations(parents, &mut declarations, out);
                let Some(block) = item.block(nodes) else {
                    continue;
                };
                let prelude_nodes = item.nodes(nodes);
                let prelude_span = nodes_span(prelude_nodes, item.span);
                if is_bare_global(prelude_nodes) {
                    flatten_items(&[], source, &block.children, &item.children, ctx, out);
                } else if !slice(source, prelude_span).trim().is_empty() {
                    let expanded = expand_selectors(parents, source, prelude_nodes, prelude_span);
                    flatten_items(&expanded, source, &block.children, &item.children, ctx, out);
                }
            }
            CssSyntaxItemKind::KeyframeRule => {}
            CssSyntaxItemKind::Raw => {
                let raw = slice(source, item.span).trim();
                if raw.is_empty() {
                    continue;
                }
                if parents.is_empty() {
                    flush_declarations(parents, &mut declarations, out);
                    out.push_str(raw);
                } else {
                    declarations.push_str(raw.trim_end_matches(';'));
                    declarations.push(';');
                }
            }
        }
    }
    flush_declarations(parents, &mut declarations, out);
}

fn flush_declarations(parents: &[String], declarations: &mut String, out: &mut String) {
    if !declarations.is_empty() && !parents.is_empty() {
        out.push_str(&parents.join(","));
        out.push('{');
        out.push_str(declarations);
        out.push('}');
    }
    declarations.clear();
}

fn nodes_span(nodes: &[CssSyntaxNode], fallback: Span) -> Span {
    nodes
        .first()
        .zip(nodes.last())
        .map_or(fallback, |(first, last)| {
            Span::new(first.span.lo, last.span.hi)
        })
}

fn is_conditional_at_rule(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "media" | "supports" | "container" | "layer" | "scope" | "document"
    )
}

fn keyframe_name(source: &str, nodes: &[CssSyntaxNode]) -> Option<(String, bool, Span)> {
    let at = nodes.iter().position(|node| {
        matches!(&node.kind, CssSyntaxKind::AtKeyword(name) if name.eq_ignore_ascii_case("keyframes"))
    })?;
    let significant = significant_indices(nodes, at + 1);
    let first = *significant.first()?;
    if matches!(nodes[first].kind, CssSyntaxKind::Colon)
        && let Some(second) = significant.get(1).copied()
        && let CssSyntaxKind::Function(name) = &nodes[second].kind
        && name.eq_ignore_ascii_case("global")
    {
        let function = &nodes[second];
        return Some((
            slice(source, function.content_span()).trim().to_string(),
            true,
            Span::new(nodes[first].span.lo, function.span.hi),
        ));
    }
    let node = &nodes[first];
    if let CssSyntaxKind::Ident(name) = &node.kind {
        Some((name.clone(), false, node.span))
    } else {
        None
    }
}

fn scope_keyframes_prelude(
    source: &str,
    nodes: &[CssSyntaxNode],
    span: Span,
    ctx: &KeyframeCtx<'_>,
) -> String {
    let Some((name, global, name_span)) = keyframe_name(source, nodes) else {
        return slice(source, span).trim().to_string();
    };
    let replacement = if global || ctx.suffix.is_empty() {
        name
    } else {
        format!("{name}-{}", ctx.suffix)
    };
    apply_edits(source, span, &[(name_span, replacement)])
        .trim()
        .to_string()
}

fn scope_animation_declaration(
    source: &str,
    nodes: &[CssSyntaxNode],
    span: Span,
    ctx: &KeyframeCtx<'_>,
) -> String {
    let significant = significant_indices(nodes, 0);
    let Some(first) = significant.first().copied() else {
        return slice(source, span).trim().to_string();
    };
    let CssSyntaxKind::Ident(property) = &nodes[first].kind else {
        return slice(source, span).trim().to_string();
    };
    let property = property.trim_start_matches('-').to_ascii_lowercase();
    let property = ["webkit-", "moz-", "ms-", "o-"]
        .iter()
        .find_map(|prefix| property.strip_prefix(prefix))
        .unwrap_or(&property);
    if !matches!(property, "animation" | "animation-name") {
        return slice(source, span).trim().to_string();
    }
    let Some(colon) = significant
        .iter()
        .copied()
        .find(|index| matches!(nodes[*index].kind, CssSyntaxKind::Colon))
    else {
        return slice(source, span).trim().to_string();
    };

    let mut edits = Vec::new();
    collect_animation_edits(source, &nodes[colon + 1..], ctx, &mut edits);
    apply_edits(source, span, &edits).trim().to_string()
}

fn collect_animation_edits(
    source: &str,
    nodes: &[CssSyntaxNode],
    ctx: &KeyframeCtx<'_>,
    edits: &mut Vec<(Span, String)>,
) {
    let significant = significant_indices(nodes, 0);
    let mut skip = None;
    for (position, index) in significant.iter().copied().enumerate() {
        if skip == Some(index) {
            continue;
        }
        let node = &nodes[index];
        if matches!(node.kind, CssSyntaxKind::Colon)
            && let Some(next) = significant.get(position + 1).copied()
            && let CssSyntaxKind::Function(name) = &nodes[next].kind
            && name.eq_ignore_ascii_case("global")
        {
            let function = &nodes[next];
            edits.push((
                Span::new(node.span.lo, function.span.hi),
                slice(source, function.content_span()).trim().to_string(),
            ));
            skip = Some(next);
            continue;
        }
        if let CssSyntaxKind::Ident(name) = &node.kind
            && ctx.defined.iter().any(|defined| defined == name)
            && !ctx.suffix.is_empty()
        {
            edits.push((node.span, format!("{name}-{}", ctx.suffix)));
        }
        if !matches!(&node.kind, CssSyntaxKind::Function(name) if name.eq_ignore_ascii_case("global"))
        {
            collect_animation_edits(source, &node.children, ctx, edits);
        }
    }
}

fn selector_parts(selector: &str) -> Vec<String> {
    let tree = CssSyntaxTree::parse_with_context(
        selector,
        Span::new(0, selector.len() as u32),
        CssSyntaxContext::ComponentValues,
    );
    selector_parts_from_nodes(&tree.nodes, Span::new(0, selector.len() as u32))
        .into_iter()
        .map(|(span, _)| slice(selector, span).trim().to_string())
        .filter(|part| !part.is_empty())
        .collect()
}

fn expand_selectors(
    parents: &[String],
    source: &str,
    nodes: &[CssSyntaxNode],
    span: Span,
) -> Vec<String> {
    let mut expanded = Vec::new();
    for (part_span, part_nodes) in selector_parts_from_nodes(nodes, span) {
        let child = slice(source, part_span).trim();
        if child.is_empty() {
            continue;
        }
        if let Some(global) = extract_global(source, part_nodes, part_span) {
            expanded.push(global);
            continue;
        }

        let ampersands = delimiter_spans(part_nodes, '&');
        if parents.is_empty() {
            let edits = ampersands
                .into_iter()
                .map(|span| (span, String::new()))
                .collect::<Vec<_>>();
            expanded.push(apply_edits(source, part_span, &edits).trim().to_string());
        } else {
            for parent in parents {
                if ampersands.is_empty() {
                    expanded.push(format!("{} {}", parent.trim(), child));
                } else {
                    let edits = ampersands
                        .iter()
                        .copied()
                        .map(|span| (span, parent.trim().to_string()))
                        .collect::<Vec<_>>();
                    expanded.push(apply_edits(source, part_span, &edits).trim().to_string());
                }
            }
        }
    }
    expanded
}

fn selector_parts_from_nodes(nodes: &[CssSyntaxNode], span: Span) -> Vec<(Span, &[CssSyntaxNode])> {
    let mut parts = Vec::new();
    let mut start_span = span.lo;
    let mut start_index = 0usize;
    for (index, node) in nodes.iter().enumerate() {
        if matches!(node.kind, CssSyntaxKind::Comma) {
            parts.push((
                Span::new(start_span, node.span.lo),
                &nodes[start_index..index],
            ));
            start_span = node.span.hi;
            start_index = index + 1;
        }
    }
    parts.push((Span::new(start_span, span.hi), &nodes[start_index..]));
    parts
}

fn is_bare_global(nodes: &[CssSyntaxNode]) -> bool {
    let significant = significant_indices(nodes, 0);
    matches!(significant.as_slice(), [colon, function]
        if matches!(nodes[*colon].kind, CssSyntaxKind::Colon)
            && matches!(&nodes[*function].kind, CssSyntaxKind::Function(name)
                if name.eq_ignore_ascii_case("global")
                    && nodes[*function].children.iter().all(CssSyntaxNode::is_trivia)))
}

fn extract_global(source: &str, nodes: &[CssSyntaxNode], span: Span) -> Option<String> {
    let significant = significant_indices(nodes, 0);
    for pair in significant.windows(2) {
        let colon = &nodes[pair[0]];
        let function = &nodes[pair[1]];
        if matches!(colon.kind, CssSyntaxKind::Colon)
            && matches!(&function.kind, CssSyntaxKind::Function(name) if name.eq_ignore_ascii_case("global"))
        {
            return Some(
                apply_edits(
                    source,
                    span,
                    &[(
                        Span::new(colon.span.lo, function.span.hi),
                        slice(source, function.content_span()).trim().to_string(),
                    )],
                )
                .trim()
                .to_string(),
            );
        }
    }
    None
}

fn delimiter_spans(nodes: &[CssSyntaxNode], delimiter: char) -> Vec<Span> {
    let mut spans = Vec::new();
    visit_nodes(nodes, &mut |node| {
        if matches!(node.kind, CssSyntaxKind::Delim(value) if value == delimiter) {
            spans.push(node.span);
        }
    });
    spans
}

fn visit_nodes(nodes: &[CssSyntaxNode], visitor: &mut impl FnMut(&CssSyntaxNode)) {
    for node in nodes {
        visitor(node);
        visit_nodes(&node.children, visitor);
    }
}

fn significant_indices(nodes: &[CssSyntaxNode], start: usize) -> Vec<usize> {
    nodes
        .iter()
        .enumerate()
        .skip(start)
        .filter_map(|(index, node)| (!node.is_trivia()).then_some(index))
        .collect()
}

fn apply_edits(source: &str, span: Span, edits: &[(Span, String)]) -> String {
    let mut output = slice(source, span).to_string();
    let mut edits = edits
        .iter()
        .filter(|(edit, _)| span.contains(*edit))
        .collect::<Vec<_>>();
    edits.sort_by_key(|(edit, _)| std::cmp::Reverse(edit.lo));
    for (edit, replacement) in edits {
        let lo = (edit.lo - span.lo) as usize;
        let hi = (edit.hi - span.lo) as usize;
        output.replace_range(lo..hi, replacement);
    }
    output
}

fn slice(source: &str, span: Span) -> &str {
    source.get(span.lo as usize..span.hi as usize).unwrap_or("")
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
    fn balanced_blocks_in_custom_property_values_remain_declarations() {
        let css = flatten(".box", "--theme: { color: red }; color: var(--theme);");
        assert_eq!(css, ".box{--theme: { color: red };color: var(--theme);}");
        assert!(!css.contains(".box --theme"), "{css}");
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
