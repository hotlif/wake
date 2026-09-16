//! Validated, indivisible text edits and bounded reparsing. No file access or publication.
use crate::{LintError, LintOptions, LintResult, SourceType, lint_text};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextEdit {
    pub start: u32,
    pub end: u32,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LintFix {
    pub edits: Vec<TextEdit>,
}

pub struct FixApplication {
    pub output: String,
    pub applied: usize,
    pub skipped: usize,
}
#[derive(Debug)]
pub struct FixResult {
    pub output: String,
    pub result: LintResult,
    pub passes: u8,
}

fn conflict(a: &TextEdit, b: &TextEdit) -> bool {
    if a.start == a.end {
        b.start <= a.start && a.start <= b.end
    } else if b.start == b.end {
        a.start <= b.start && b.start <= a.end
    } else {
        a.start < b.end && b.start < a.end
    }
}

/// Candidates are in priority order. Every edit is validated before any candidate is selected.
/// Adjacent nonempty replacements can coexist; insertion points reserve both interval endpoints.
pub fn apply_fixes(source: &str, candidates: &[LintFix]) -> Result<FixApplication, LintError> {
    validate_fixes(source, candidates)?;
    select_fixes(source, candidates)
}

pub(crate) fn validate_fixes(source: &str, candidates: &[LintFix]) -> Result<(), LintError> {
    for fix in candidates {
        if fix.edits.is_empty() {
            return Err(LintError::Fix("Empty fix edit set".into()));
        }
        for (index, edit) in fix.edits.iter().enumerate() {
            if edit.start > edit.end
                || edit.end as usize > source.len()
                || !source.is_char_boundary(edit.start as usize)
                || !source.is_char_boundary(edit.end as usize)
            {
                return Err(LintError::Fix("Invalid fix UTF-8 range".into()));
            }
            if fix.edits[..index].iter().any(|other| conflict(edit, other)) {
                return Err(LintError::Fix("Overlapping edits within one fix".into()));
            }
        }
    }
    Ok(())
}

fn select_fixes(source: &str, candidates: &[LintFix]) -> Result<FixApplication, LintError> {
    let mut selected: Vec<&TextEdit> = Vec::new();
    let mut applied = 0;
    let mut skipped = 0;
    for fix in candidates {
        if fix
            .edits
            .iter()
            .all(|e| source[e.start as usize..e.end as usize] == e.text)
        {
            continue;
        }
        if fix
            .edits
            .iter()
            .any(|e| selected.iter().any(|other| conflict(e, other)))
        {
            skipped += 1;
            continue;
        }
        selected.extend(&fix.edits);
        applied += 1;
    }
    selected.sort_by_key(|edit| (edit.start, edit.end));
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    for edit in selected {
        output.push_str(&source[cursor..edit.start as usize]);
        output.push_str(&edit.text);
        cursor = edit.end as usize;
    }
    output.push_str(&source[cursor..]);
    Ok(FixApplication {
        output,
        applied,
        skipped,
    })
}

pub fn fix_text(
    source: &str,
    source_type: SourceType,
    options: &LintOptions,
) -> Result<FixResult, LintError> {
    fix_with(source, |text| lint_text(text, source_type, options))
}

/// Match the baseline before selecting fixes on every newly parsed source snapshot.
pub fn fix_text_with_baseline(
    source: &str,
    source_type: SourceType,
    options: &LintOptions,
    baseline: &[crate::BaselineEntry],
) -> Result<(FixResult, crate::BaselineMatch), LintError> {
    crate::validate_baseline(baseline)?;
    let mut matched = crate::BaselineMatch::default();
    let result = fix_with(source, |text| {
        let mut result = lint_text(text, source_type, options)?;
        matched = crate::apply_baseline(text, &mut result.diagnostics, baseline)?;
        Ok(result)
    })?;
    Ok((result, matched))
}

fn fix_with(
    source: &str,
    mut analyze: impl FnMut(&str) -> Result<LintResult, LintError>,
) -> Result<FixResult, LintError> {
    let mut output = source.to_owned();
    let mut result = analyze(&output)?;
    let mut seen = HashSet::from([output.clone()]);
    let mut passes = 0;
    if result
        .parse_diagnostics
        .iter()
        .any(wake_common::Diagnostic::is_error)
    {
        return Ok(FixResult {
            output,
            result,
            passes,
        });
    }
    loop {
        let candidates: Vec<_> = result
            .diagnostics
            .iter()
            .filter_map(|d| d.fix.clone())
            .collect();
        let next = apply_fixes(&output, &candidates)?;
        if next.output == output {
            return Ok(FixResult {
                output,
                result,
                passes,
            });
        }
        if passes == 10 {
            return Err(LintError::Fix(
                "Fixes did not converge within ten passes".into(),
            ));
        }
        if !seen.insert(next.output.clone()) {
            return Err(LintError::Fix("Circular lint fixes detected".into()));
        }
        let checked = analyze(&next.output)?;
        if checked
            .parse_diagnostics
            .iter()
            .any(wake_common::Diagnostic::is_error)
        {
            return Err(LintError::Fix("Fix introduced a syntax error".into()));
        }
        output = next.output;
        result = checked;
        passes += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LintDiagnostic, RuleLevel};
    fn report(source: &str, text: String) -> LintResult {
        LintResult {
            parse_diagnostics: Vec::new(),
            diagnostics: vec![LintDiagnostic {
                rule_id: "test/rule".into(),
                level: RuleLevel::Error,
                message_id: "test".into(),
                message: "test".into(),
                start: 0,
                end: source.len() as u32,
                fix: Some(LintFix {
                    edits: vec![TextEdit {
                        start: 0,
                        end: source.len() as u32,
                        text,
                    }],
                }),
            }],
        }
    }
    #[test]
    fn rejects_cycles_nonconvergence_and_parse_regressions_without_partial_output() {
        assert!(
            fix_with("a", |s| Ok(report(
                s,
                if s == "a" { "b" } else { "a" }.into()
            )))
            .unwrap_err()
            .to_string()
            .contains("Circular")
        );
        assert!(
            fix_with("a", |s| Ok(report(s, format!("{s}a"))))
                .unwrap_err()
                .to_string()
                .contains("ten passes")
        );
        assert!(
            fix_with("a", |s| if s == "a" {
                Ok(report(s, "!".into()))
            } else {
                Ok(LintResult {
                    diagnostics: Vec::new(),
                    parse_diagnostics: vec![wake_common::Diagnostic::error("bad edit")],
                })
            })
            .unwrap_err()
            .to_string()
            .contains("syntax error")
        );
    }
}
