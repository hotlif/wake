use crate::{EffectiveRule, LintDiagnostic, RuleLevel};
use std::collections::BTreeMap;
use wake_ecma_parser::{Comment, CommentKind, SourceNode, SourceNodeKind};

pub(crate) fn check_array_type(
    parsed: &wake_ecma_parser::SourceParseOutput,
    configuration: &BTreeMap<String, EffectiveRule>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    const ID: &str = "ts/array-type";
    let setting = &configuration[ID].configuration;
    if setting.level == RuleLevel::Off {
        return;
    }
    let generic = setting.options["syntax"] == "generic";
    let nodes = &parsed.syntax;
    let mut children = vec![Vec::new(); nodes.len()];
    if !generic {
        for (index, node) in nodes.iter().enumerate() {
            if let Some(parent) = node.parent {
                children[parent].push(index);
            }
        }
    }
    let references = parsed
        .identifiers
        .iter()
        .filter(|identifier| {
            identifier.role == wake_ecma_ast::SourceIdentifierRole::TypeReference
                && matches!(identifier.name.as_str(), "Array" | "ReadonlyArray")
        })
        .map(|identifier| (identifier.span.lo, identifier))
        .collect::<BTreeMap<_, _>>();
    for (index, node) in nodes.iter().enumerate() {
        let report = if generic {
            node.kind == SourceNodeKind::TsArrayType
        } else if node.kind == SourceNodeKind::TsTypeReference {
            if node.parent.is_some_and(|parent| {
                nodes[parent].kind == SourceNodeKind::TsType
                    && nodes[parent]
                        .parent
                        .is_some_and(|parent| nodes[parent].kind == SourceNodeKind::TsHeritageType)
            }) {
                continue;
            }
            let Some(identifier) = references.get(&node.span.lo) else {
                continue;
            };
            let Some(arguments) = children[index]
                .iter()
                .copied()
                .find(|&child| nodes[child].kind == SourceNodeKind::TsTypeArguments)
            else {
                continue;
            };
            let token = parsed
                .tokens
                .partition_point(|token| token.span.hi <= node.span.lo);
            // The next committed token must be the argument opener: A.B<T> is qualified even
            // when the root A happens to be spelled Array. Comments are not committed tokens.
            parsed
                .tokens
                .get(token)
                .is_some_and(|token| token.span == identifier.span)
                && parsed
                    .tokens
                    .get(token + 1)
                    .is_some_and(|token| token.span.lo == nodes[arguments].span.lo)
                && children[arguments]
                    .iter()
                    .filter(|&&child| nodes[child].kind == SourceNodeKind::TsType)
                    .count()
                    == 1
        } else {
            false
        };
        if report {
            diagnostics.push(LintDiagnostic {
                rule_id: ID.into(),
                level: setting.level,
                message_id: if generic { "generic" } else { "array" }.into(),
                message: if generic {
                    "Use Array<T> or ReadonlyArray<T> for array types."
                } else {
                    "Use T[] or readonly T[] for array types."
                }
                .into(),
                start: node.span.lo,
                end: node.span.hi,
                fix: None,
            });
        }
    }
}

pub(crate) fn check(
    nodes: &[SourceNode],
    levels: &BTreeMap<String, RuleLevel>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let mut interface_has_members = vec![false; nodes.len()];
    for node in nodes {
        if node.kind == SourceNodeKind::TsTypeMember
            && let Some(body) = node.parent
            && nodes[body].kind == SourceNodeKind::TsObjectType
            && let Some(interface) = nodes[body].parent
            && nodes[interface].kind == SourceNodeKind::TsInterface
        {
            interface_has_members[interface] = true;
        }
    }
    for (index, node) in nodes.iter().enumerate() {
        let (id, message_id, message) = match node.kind {
            SourceNodeKind::TsNamespace => (
                "ts/no-namespace",
                "namespace",
                "Use module exports instead of a named namespace.",
            ),
            SourceNodeKind::TsInterface if !interface_has_members[index] => (
                "ts/no-empty-interface",
                "empty",
                "Interface has no members.",
            ),
            SourceNodeKind::TsAny => ("ts/no-explicit-any", "any", "Unexpected explicit any type."),
            SourceNodeKind::TsNonNullAssertion => (
                "ts/no-non-null-assertion",
                "assertion",
                "Unexpected non-null assertion.",
            ),
            _ => continue,
        };
        let level = levels[id];
        if level == RuleLevel::Off {
            continue;
        }
        diagnostics.push(LintDiagnostic {
            rule_id: id.into(),
            level,
            message_id: message_id.into(),
            message: message.into(),
            start: node.span.lo,
            end: node.span.hi,
            fix: None,
        });
    }
}

pub(crate) fn check_comments(
    source: &str,
    comments: &[Comment],
    levels: &BTreeMap<String, RuleLevel>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let id = "ts/ban-ts-comment";
    let level = levels[id];
    if level == RuleLevel::Off {
        return;
    }
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
        for line in body.split(['\r', '\n', '\u{2028}', '\u{2029}']) {
            let line = line
                .trim_start()
                .trim_start_matches(['*', '/'])
                .trim_start();
            let end = line
                .find(|ch: char| ch.is_whitespace() || ch == ':')
                .unwrap_or(line.len());
            let directive = &line[..end];
            let forbidden = match directive {
                "@ts-ignore" | "@ts-nocheck" => true,
                "@ts-expect-error" => {
                    line[end..]
                        .trim()
                        .trim_start_matches(':')
                        .trim()
                        .chars()
                        .count()
                        < 3
                }
                _ => false,
            };
            if forbidden {
                let start =
                    comment.span.lo as usize + (line.as_ptr() as usize - text.as_ptr() as usize);
                diagnostics.push(LintDiagnostic {
                    rule_id: id.into(),
                    level,
                    message_id: "directive".into(),
                    message:
                        "Use @ts-expect-error with an explanation of at least three characters."
                            .into(),
                    start: start as u32,
                    end: (start + directive.len()) as u32,
                    fix: None,
                });
            }
        }
    }
}
