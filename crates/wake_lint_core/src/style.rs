use std::collections::BTreeMap;
use wake_ecma_parser::{SourceToken, SourceTokenContext, SourceTokenKind};

use crate::{EffectiveRule, LintDiagnostic, LintFix, RuleLevel, TextEdit};

pub(crate) fn check_commas(
    source: &str,
    lists: &[wake_ecma_ast::SourceList],
    rules: &BTreeMap<String, EffectiveRule>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let rule = &rules["style/comma-dangle"].configuration;
    if rule.level == RuleLevel::Off {
        return;
    }
    let mode = rule.options["mode"].as_str().expect("validated comma mode");
    for list in lists {
        if list.kind == wake_ecma_ast::SourceListKind::Parenthesized || list.must_trail {
            continue;
        }
        let Some(last) = list.last else {
            continue;
        };
        let multiline = source[last.hi as usize..list.close.lo as usize]
            .contains(['\n', '\r', '\u{2028}', '\u{2029}']);
        let require = mode == "always" || (mode == "always-multiline" && multiline);
        let allow = mode != "never" && (mode == "always" || multiline);
        let (span, missing) = match list.comma {
            Some(comma) if !allow => (comma, false),
            None if require && list.can_trail => (wake_common::Span::new(last.hi, last.hi), true),
            _ => continue,
        };
        diagnostics.push(LintDiagnostic {
            rule_id: "style/comma-dangle".into(),
            level: rule.level,
            message_id: if missing { "missing" } else { "extra" }.into(),
            message: if missing {
                "Missing trailing comma."
            } else {
                "Unnecessary trailing comma."
            }
            .into(),
            start: span.lo,
            end: span.hi,
            fix: Some(LintFix {
                edits: vec![TextEdit {
                    start: span.lo,
                    end: span.hi,
                    text: if missing { "," } else { "" }.into(),
                }],
            }),
        });
    }
}

pub(crate) fn check_terminators(
    terminators: &[wake_ecma_ast::SourceTerminator],
    rules: &BTreeMap<String, EffectiveRule>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let rule = &rules["style/semi"].configuration;
    if rule.level == RuleLevel::Off {
        return;
    }
    let always = rule.options["mode"] == "always";
    for terminator in terminators {
        if (always && terminator.explicit)
            || (!always && (!terminator.explicit || !terminator.can_omit))
        {
            continue;
        }
        diagnostics.push(LintDiagnostic {
            rule_id: "style/semi".into(),
            level: rule.level,
            message_id: if always { "missing" } else { "extra" }.into(),
            message: if always {
                "Missing semicolon."
            } else {
                "Unnecessary semicolon."
            }
            .into(),
            start: terminator.span.lo,
            end: terminator.span.hi,
            fix: Some(LintFix {
                edits: vec![TextEdit {
                    start: terminator.span.lo,
                    end: terminator.span.hi,
                    text: if always { ";" } else { "" }.into(),
                }],
            }),
        });
    }
}

pub(crate) fn check(
    source: &str,
    tokens: &[SourceToken],
    rules: &BTreeMap<String, EffectiveRule>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let quotes = &rules["style/quotes"].configuration;
    if quotes.level != RuleLevel::Off {
        let preferred = if quotes.options["quote"] == "double" {
            '"'
        } else {
            '\''
        };
        for token in tokens {
            if token.kind != SourceTokenKind::Str || token.context != SourceTokenContext::JavaScript
            {
                continue;
            }
            let raw = &source[token.span.lo as usize..token.span.hi as usize];
            let Some(current) = raw.chars().next() else {
                continue;
            };
            if current == preferred {
                continue;
            }
            let mut text = String::new();
            text.push(preferred);
            let mut chars = raw[1..raw.len() - 1].chars();
            while let Some(ch) = chars.next() {
                if ch == '\\' {
                    if let Some(escaped) = chars.next() {
                        if escaped != current {
                            text.push('\\');
                        }
                        text.push(escaped);
                    }
                } else {
                    if ch == preferred {
                        text.push('\\');
                    }
                    text.push(ch);
                }
            }
            text.push(preferred);
            diagnostics.push(LintDiagnostic {
                rule_id: "style/quotes".into(),
                level: quotes.level,
                message_id: "quote".into(),
                message: format!(
                    "Use {} quotes.",
                    quotes.options["quote"]
                        .as_str()
                        .expect("validated quote option")
                ),
                start: token.span.lo,
                end: token.span.hi,
                fix: Some(LintFix {
                    edits: vec![TextEdit {
                        start: token.span.lo,
                        end: token.span.hi,
                        text,
                    }],
                }),
            });
        }
    }
    let level = rules["style/no-trailing-spaces"].configuration.level;
    if level == RuleLevel::Off {
        return;
    }
    let ends = source
        .char_indices()
        .filter(|(_, ch)| matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}'))
        .map(|(index, _)| index)
        .chain(std::iter::once(source.len()));
    for end in ends {
        let mut start = end;
        while start > 0 && matches!(source.as_bytes()[start - 1], b' ' | b'\t') {
            start -= 1;
        }
        if start == end {
            continue;
        }
        // Editing inside an original literal may change runtime content. Report it without a
        // fix; the source token boundary, rather than text heuristics, owns this distinction.
        let next_token = tokens.partition_point(|token| token.span.hi <= start as u32);
        let inside_token = tokens
            .get(next_token)
            .is_some_and(|token| token.span.lo < end as u32);
        diagnostics.push(LintDiagnostic {
            rule_id: "style/no-trailing-spaces".into(),
            level,
            message_id: "trailing".into(),
            message: "Trailing spaces or tabs.".into(),
            start: start as u32,
            end: end as u32,
            fix: (!inside_token).then(|| LintFix {
                edits: vec![TextEdit {
                    start: start as u32,
                    end: end as u32,
                    text: String::new(),
                }],
            }),
        });
    }
}
