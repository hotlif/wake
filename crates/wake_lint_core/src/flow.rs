use crate::{LintDiagnostic, RuleLevel};
use std::collections::BTreeMap;
use wake_common::{FxHashSet, Span};
use wake_ecma_parser::{Comment, CommentKind, SourceNode, SourceNodeKind};
use wake_ecma_semantic::ControlFlowFacts;

pub(crate) fn check(
    source: &str,
    nodes: &[SourceNode],
    comments: &[Comment],
    facts: &ControlFlowFacts,
    levels: &BTreeMap<String, RuleLevel>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let bodies: FxHashSet<_> = nodes
        .iter()
        .filter(|node| node.kind == SourceNodeKind::JsFunctionBody)
        .map(|node| node.span)
        .collect();
    let mut report = |id: &str, span: Span, message: &str| {
        if levels[id] != RuleLevel::Off && !span.is_dummy() {
            diagnostics.push(LintDiagnostic {
                rule_id: id.into(),
                level: levels[id],
                message_id: "flow".into(),
                message: message.into(),
                start: span.lo,
                end: span.hi,
                fix: None,
            });
        }
    };
    for span in &facts.unreachable {
        report("js/no-unreachable", *span, "Statement cannot be reached.");
    }
    for span in &facts.finally_exits {
        report(
            "js/no-unsafe-finally",
            *span,
            "This jump escapes finally and overrides the pending completion.",
        );
    }
    for function in &facts.functions {
        if function.returns_value
            && (function.returns_void || function.end_reachable)
            && bodies.contains(&function.body)
        {
            report(
                "js/consistent-return",
                function.body,
                "Some function paths return a value while others return without one.",
            );
        }
    }
    for fallthrough in &facts.fallthrough {
        let intentional = comments
            .iter()
            .filter(|comment| {
                comment.span.lo >= fallthrough.comment_gap.lo
                    && comment.span.hi <= fallthrough.comment_gap.hi
            })
            .any(|comment| {
                let text = &source[comment.span.lo as usize..comment.span.hi as usize];
                let body = match comment.kind {
                    CommentKind::Block => text
                        .strip_prefix("/*")
                        .unwrap()
                        .strip_suffix("*/")
                        .unwrap_or(""),
                    CommentKind::Line => &text[2..],
                    CommentKind::Hashbang => return false,
                };
                matches!(
                    body.trim().to_ascii_lowercase().as_str(),
                    "fallthrough" | "fall through" | "falls through"
                )
            });
        if !intentional {
            report(
                "js/no-fallthrough",
                fallthrough.case,
                "Case may continue into the next case without an explanation.",
            );
        }
    }
}
