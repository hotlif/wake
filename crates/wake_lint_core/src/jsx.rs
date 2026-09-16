use crate::{LintDiagnostic, LintFix, RuleLevel, TextEdit};
use std::collections::{BTreeMap, HashSet};
use wake_common::Span;
use wake_ecma_parser::{Comment, SourceNode, SourceNodeKind as Kind};

pub(crate) fn check(
    source: &str,
    nodes: &[SourceNode],
    comments: &[Comment],
    levels: &BTreeMap<String, RuleLevel>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    if !levels
        .iter()
        .any(|(id, level)| id.starts_with("react/") && *level != RuleLevel::Off)
    {
        return;
    }
    let mut children = vec![Vec::new(); nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        if let Some(parent) = node.parent {
            children[parent].push(index);
        }
    }
    let mut report =
        |id: &str, span: Span, message_id: &str, message: String, fix: Option<LintFix>| {
            let level = levels[id];
            if level != RuleLevel::Off {
                diagnostics.push(LintDiagnostic {
                    rule_id: id.into(),
                    level,
                    message_id: message_id.into(),
                    message,
                    start: span.lo,
                    end: span.hi,
                    fix,
                });
            }
        };
    for (index, node) in nodes.iter().enumerate() {
        match node.kind {
            Kind::JsxOpeningElement => {
                let mut names = HashSet::new();
                for attribute in &children[index] {
                    if nodes[*attribute].kind != Kind::JsxAttribute {
                        continue;
                    }
                    let Some(name) = children[*attribute]
                        .iter()
                        .copied()
                        .find(|child| nodes[*child].kind == Kind::JsxName)
                    else {
                        continue;
                    };
                    let span = nodes[name].span;
                    let text = &source[span.lo as usize..span.hi as usize];
                    if !names.insert(text) {
                        report(
                            "react/jsx-no-duplicate-props",
                            span,
                            "duplicate",
                            format!("Duplicate JSX attribute '{text}'."),
                            None,
                        );
                    }
                    if text == "dangerouslySetInnerHTML" {
                        report(
                            "react/no-danger",
                            span,
                            "dangerous",
                            "Avoid dangerouslySetInnerHTML.".into(),
                            None,
                        );
                    }
                    if text == "children" {
                        report(
                            "react/no-children-prop",
                            span,
                            "children",
                            "Pass children between JSX tags.".into(),
                            None,
                        );
                    }
                }
            }
            Kind::JsxElement => {
                let opening = children[index]
                    .iter()
                    .copied()
                    .find(|child| nodes[*child].kind == Kind::JsxOpeningElement);
                let closing = children[index]
                    .iter()
                    .copied()
                    .find(|child| nodes[*child].kind == Kind::JsxClosingElement);
                if let (Some(opening), Some(closing)) = (opening, closing)
                    && nodes[opening].span.hi == nodes[closing].span.lo
                {
                    report(
                        "react/self-closing-comp",
                        nodes[opening].span,
                        "empty",
                        "Use a self-closing JSX element.".into(),
                        (!comments.iter().any(|comment| {
                            comment.span.lo < nodes[closing].span.hi
                                && comment.span.hi > nodes[opening].span.hi - 1
                        }))
                        .then(|| LintFix {
                            edits: vec![TextEdit {
                                start: nodes[opening].span.hi - 1,
                                end: nodes[closing].span.hi,
                                text: "/>".into(),
                            }],
                        }),
                    );
                }
            }
            _ => {}
        }
    }
}
