use wake_common::Span;
use wake_ecma_parser::{Comment, CommentKind};

use crate::{LintDiagnostic, RULES, RuleLevel};

#[derive(Clone, Copy)]
enum Kind {
    Disable,
    Enable,
    Line,
    NextLine,
}

struct Directive<'s> {
    kind: Kind,
    offset: u32,
    line: usize,
    rules: Vec<&'s str>,
    span: Span,
    used: Vec<bool>,
}

pub(crate) fn apply(
    source: &str,
    comments: &[Comment],
    diagnostics: &mut Vec<LintDiagnostic>,
    unused_level: RuleLevel,
) {
    let lines = line_starts(source);
    let line_of = |offset| {
        lines
            .partition_point(|start| *start <= offset)
            .saturating_sub(1)
    };
    let mut directives = Vec::new();
    let mut invalid = Vec::new();
    for comment in comments {
        if comment.kind == CommentKind::Hashbang {
            continue;
        }
        let text = &source[comment.span.lo as usize..comment.span.hi as usize];
        let body = if comment.kind == CommentKind::Block {
            text.strip_prefix("/*")
                .unwrap()
                .strip_suffix("*/")
                .unwrap_or("")
        } else {
            &text[2..]
        };
        let body = body.trim();
        let Some(body) = body.strip_prefix("wake-lint-") else {
            continue;
        };
        let end = body.find(char::is_whitespace).unwrap_or(body.len());
        let command = &body[..end];
        let kind = match command {
            "disable" => Some(Kind::Disable),
            "enable" => Some(Kind::Enable),
            "disable-line" => Some(Kind::Line),
            "disable-next-line" => Some(Kind::NextLine),
            _ => None,
        };
        let arguments = body[end..]
            .trim()
            .split_once("--")
            .map_or(body[end..].trim(), |(rules, _)| rules.trim());
        let mut rule_ids: Vec<_> = if arguments.is_empty() {
            Vec::new()
        } else {
            arguments.split(',').map(str::trim).collect()
        };
        if kind.is_none()
            || (matches!(kind, Some(Kind::Line))
                && line_of(comment.span.lo) != line_of(comment.span.hi))
            || rule_ids
                .iter()
                .any(|id| !RULES.iter().any(|rule| rule.id == *id))
        {
            invalid.push(LintDiagnostic {
                rule_id: "wake/invalid-directive".into(),
                level: RuleLevel::Error,
                message_id: "invalid".into(),
                message: "Unknown lint directive or rule ID.".into(),
                start: comment.span.lo,
                end: comment.span.hi,
                fix: None,
            });
            continue;
        }
        // A repeated name in one directive is a single suppression, preserving declaration order.
        let mut seen = std::collections::HashSet::new();
        rule_ids.retain(|id| seen.insert(*id));
        directives.push(Directive {
            kind: kind.unwrap(),
            offset: comment.span.hi,
            line: line_of(comment.span.hi),
            span: comment.span,
            used: vec![false; rule_ids.len().max(1)],
            rules: rule_ids,
        });
    }
    diagnostics.retain(|diagnostic| {
        let mut active = None;
        let mut local = None;
        let line = line_of(diagnostic.start);
        for (index, directive) in directives.iter().enumerate() {
            let slot = if directive.rules.is_empty() {
                Some(0)
            } else {
                directive
                    .rules
                    .iter()
                    .position(|id| *id == diagnostic.rule_id)
            };
            let Some(slot) = slot else { continue };
            match directive.kind {
                Kind::Disable if directive.offset <= diagnostic.start => {
                    active.get_or_insert((index, slot));
                }
                Kind::Enable if directive.offset <= diagnostic.start => active = None,
                Kind::Line if directive.line == line => {
                    local.get_or_insert((index, slot));
                }
                Kind::NextLine if directive.line + 1 == line => {
                    local.get_or_insert((index, slot));
                }
                _ => {}
            }
        }
        if let Some((index, slot)) = active.or(local) {
            directives[index].used[slot] = true;
            false
        } else {
            true
        }
    });
    if unused_level != RuleLevel::Off {
        for directive in &directives {
            if matches!(directive.kind, Kind::Enable) {
                continue;
            }
            for (slot, used) in directive.used.iter().enumerate() {
                if *used {
                    continue;
                }
                let target = directive.rules.get(slot).copied().unwrap_or("all rules");
                diagnostics.push(LintDiagnostic {
                    rule_id: "wake/unused-disable".into(),
                    level: unused_level,
                    message_id: "unused".into(),
                    message: format!("Unused lint suppression for {target}."),
                    start: directive.span.lo,
                    end: directive.span.hi,
                    fix: None,
                });
            }
        }
    }
    diagnostics.extend(invalid);
}

fn line_starts(source: &str) -> Vec<u32> {
    let mut starts = vec![0];
    let mut chars = source.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '\r' => {
                let end = if chars.peek().is_some_and(|(_, next)| *next == '\n') {
                    chars.next().unwrap().0 + 1
                } else {
                    index + 1
                };
                starts.push(end as u32);
            }
            '\n' | '\u{2028}' | '\u{2029}' => starts.push((index + ch.len_utf8()) as u32),
            _ => {}
        }
    }
    starts
}
