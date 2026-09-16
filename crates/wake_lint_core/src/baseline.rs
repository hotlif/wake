//! Explicit, unique source-context matching. No paths, files, or fuzzy/ordinal fallback.
use crate::{LintDiagnostic, LintError, RULES};
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BaselineEntry {
    pub rule_id: String,
    pub message_id: String,
    pub fingerprint: String,
}

#[derive(Debug, Default)]
pub struct BaselineCandidates {
    pub entries: Vec<BaselineEntry>,
    /// Number of findings with a context shared by another finding in this file.
    pub ambiguous: usize,
}

#[derive(Debug, Default)]
pub struct BaselineMatch {
    pub matched: Vec<BaselineEntry>,
    pub suppressed: usize,
    pub ambiguous: usize,
}

pub fn validate_baseline(entries: &[BaselineEntry]) -> Result<(), LintError> {
    let mut seen = BTreeSet::new();
    for entry in entries {
        if !RULES.iter().any(|rule| {
            rule.id == entry.rule_id && rule.message_ids.contains(&entry.message_id.as_str())
        }) || entry.fingerprint.len() != 64
            || !entry
                .fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || !seen.insert(entry)
        {
            return Err(LintError::InvalidData(
                "Invalid or duplicate lint baseline identity".into(),
            ));
        }
    }
    Ok(())
}

struct Contexts<'a> {
    source: &'a str,
    lines: Vec<usize>,
}
impl<'a> Contexts<'a> {
    fn new(source: &'a str) -> Self {
        let mut lines = vec![0];
        let mut chars = source.char_indices().peekable();
        while let Some((index, ch)) = chars.next() {
            if matches!(ch, '\r' | '\n' | '\u{2028}' | '\u{2029}') {
                let mut end = index + ch.len_utf8();
                if ch == '\r' && chars.peek().is_some_and(|(_, next)| *next == '\n') {
                    end = chars.next().expect("peeked newline").0 + 1;
                }
                lines.push(end);
            }
        }
        Self { source, lines }
    }

    fn entry(&self, diagnostic: &LintDiagnostic) -> Result<Option<BaselineEntry>, LintError> {
        if !RULES.iter().any(|rule| {
            rule.id == diagnostic.rule_id
                && rule.message_ids.contains(&diagnostic.message_id.as_str())
        }) {
            return Ok(None);
        }
        let start = diagnostic.start as usize;
        let end = diagnostic.end as usize;
        if start > end
            || end > self.source.len()
            || !self.source.is_char_boundary(start)
            || !self.source.is_char_boundary(end)
        {
            return Err(LintError::InvalidData(
                "Invalid baseline diagnostic range".into(),
            ));
        }
        let line_start = self.lines[self.lines.partition_point(|offset| *offset <= start) - 1];
        let last = if end > start { end - 1 } else { end };
        let line_end = self
            .lines
            .get(self.lines.partition_point(|offset| *offset <= last))
            .copied()
            .unwrap_or(self.source.len());
        let raw = &self.source[line_start..line_end];
        let context_start = (line_start + raw.len() - raw.trim_start().len()).min(start);
        let context_end = (line_start + raw.trim_end().len()).max(end);
        let mut hash = Context::new(&SHA256);
        for part in [
            diagnostic.rule_id.as_bytes(),
            diagnostic.message_id.as_bytes(),
            &self.source.as_bytes()[context_start..context_end],
        ] {
            hash.update(&(part.len() as u64).to_le_bytes());
            hash.update(part);
        }
        hash.update(&((start - context_start) as u64).to_le_bytes());
        hash.update(&((end - context_start) as u64).to_le_bytes());
        let fingerprint = hash
            .finish()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Some(BaselineEntry {
            rule_id: diagnostic.rule_id.clone(),
            message_id: diagnostic.message_id.clone(),
            fingerprint,
        }))
    }
}

fn identities(
    source: &str,
    diagnostics: &[LintDiagnostic],
) -> Result<Vec<Option<BaselineEntry>>, LintError> {
    let contexts = Contexts::new(source);
    diagnostics
        .iter()
        .map(|diagnostic| contexts.entry(diagnostic))
        .collect()
}

pub fn baseline_candidates(
    source: &str,
    diagnostics: &[LintDiagnostic],
) -> Result<BaselineCandidates, LintError> {
    let mut counts = BTreeMap::new();
    for entry in identities(source, diagnostics)?.into_iter().flatten() {
        *counts.entry(entry).or_insert(0usize) += 1;
    }
    let mut result = BaselineCandidates::default();
    for (entry, count) in counts {
        if count == 1 {
            result.entries.push(entry);
        } else {
            result.ambiguous += count;
        }
    }
    Ok(result)
}

pub fn apply_baseline(
    source: &str,
    diagnostics: &mut Vec<LintDiagnostic>,
    entries: &[BaselineEntry],
) -> Result<BaselineMatch, LintError> {
    validate_baseline(entries)?;
    let keys = identities(source, diagnostics)?;
    let mut counts = BTreeMap::new();
    for key in keys.iter().flatten() {
        *counts.entry(key).or_insert(0usize) += 1;
    }
    let requested: BTreeSet<_> = entries.iter().collect();
    let mut result = BaselineMatch::default();
    let mut index = 0;
    diagnostics.retain(|_| {
        let key = &keys[index];
        index += 1;
        let Some(key) = key else {
            return true;
        };
        if counts[key] != 1 {
            result.ambiguous += 1;
            return true;
        }
        if requested.contains(key) {
            result.matched.push(key.clone());
            result.suppressed += 1;
            false
        } else {
            true
        }
    });
    result.matched.sort();
    Ok(result)
}
