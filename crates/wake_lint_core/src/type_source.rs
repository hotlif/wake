//! Owned input for type queries. The core parses its source; callers cannot supply detached ranges.
//! Syntax failures retain diagnostics and expose no query sites. Limits cover retained source and
//! query-site metadata, not a process-wide resident-memory ceiling.

use crate::{LintError, SourceType};
use std::sync::Arc;
use wake_common::{Diagnostic, Interner};
use wake_ecma_ast::{
    SourceAssignment, SourceAwait, SourceCall, SourceCallback, SourceCondition,
    SourceExpressionStatement, SourceMember, SourceReturn, SourceSwitch, SourceTemplate,
    SourceTypeAssertion,
};
use wake_ecma_parser::{ParseOptions, parse_source};

#[derive(Clone, Copy)]
struct Limits {
    bytes: usize,
    sites: usize,
}
impl Limits {
    const DEFAULT: Self = Self {
        bytes: 16 * 1024 * 1024,
        sites: 100_000,
    };
}

/// Immutable source and grammar-owned query sites. No filesystem or backend handles are retained.
pub struct TypeSource {
    path: String,
    source: Arc<str>,
    source_type: SourceType,
    calls: Vec<SourceCall>,
    awaits: Vec<SourceAwait>,
    expression_statements: Vec<SourceExpressionStatement>,
    assignments: Vec<SourceAssignment>,
    callbacks: Vec<SourceCallback>,
    returns: Vec<SourceReturn>,
    conditions: Vec<SourceCondition>,
    switches: Vec<SourceSwitch>,
    assertions: Vec<SourceTypeAssertion>,
    templates: Vec<SourceTemplate>,
    members: Vec<SourceMember>,
    diagnostics: Vec<Diagnostic>,
    complete: bool,
}

impl TypeSource {
    pub(crate) const MAX_SOURCE_BYTES: usize = Limits::DEFAULT.bytes;

    pub fn new(
        path: impl Into<String>,
        source: Arc<str>,
        source_type: SourceType,
    ) -> Result<Self, LintError> {
        Self::with_limits(path, source, source_type, Limits::DEFAULT)
    }

    fn with_limits(
        path: impl Into<String>,
        source: Arc<str>,
        source_type: SourceType,
        limits: Limits,
    ) -> Result<Self, LintError> {
        let path = path.into();
        if path.is_empty() || path.contains('\0') {
            return Err(LintError::InvalidData(
                "Type input requires a nonempty source identity".into(),
            ));
        }
        if source.len() > limits.bytes {
            return Err(LintError::Analysis(
                "Type source byte budget exceeded".into(),
            ));
        }
        let interner = Interner::new();
        let parsed = parse_source(&source, &interner, source_type, ParseOptions::default());
        let complete = !parsed.parsed.has_errors();
        let (
            calls,
            awaits,
            expression_statements,
            assignments,
            callbacks,
            returns,
            conditions,
            switches,
            assertions,
            templates,
            members,
        ) = if complete {
            let sites = parsed
                .calls
                .iter()
                .map(|call| 1 + call.arguments.len())
                .chain(parsed.awaits.iter().map(|_| 1))
                .chain(parsed.expression_statements.iter().map(|_| 1))
                .chain(parsed.assignments.iter().map(|_| 1))
                .chain(parsed.callbacks.iter().map(|_| 1))
                .chain(parsed.returns.iter().map(|_| 1))
                .chain(parsed.conditions.iter().map(|_| 1))
                .chain(parsed.switches.iter().map(|_| 1))
                .chain(parsed.assertions.iter().map(|_| 2))
                .chain(parsed.members.iter().map(|_| 2))
                .chain(
                    parsed
                        .templates
                        .iter()
                        .map(|template| 1 + template.expressions.len()),
                )
                .try_fold(0usize, |total, count| {
                    total
                        .checked_add(count)
                        .filter(|total| *total <= limits.sites)
                });
            if sites.is_none() {
                return Err(LintError::Analysis(
                    "Type source query-site budget exceeded".into(),
                ));
            }
            (
                parsed.calls,
                parsed.awaits,
                parsed.expression_statements,
                parsed.assignments,
                parsed.callbacks,
                parsed.returns,
                parsed.conditions,
                parsed.switches,
                parsed.assertions,
                parsed.templates,
                parsed.members,
            )
        } else {
            (
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
        };
        Ok(Self {
            path,
            source,
            source_type,
            calls,
            awaits,
            expression_statements,
            assignments,
            callbacks,
            returns,
            conditions,
            switches,
            assertions,
            templates,
            members,
            diagnostics: parsed.parsed.diagnostics,
            complete,
        })
    }

    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn source_type(&self) -> SourceType {
        self.source_type
    }
    pub fn calls(&self) -> &[SourceCall] {
        &self.calls
    }
    pub fn awaits(&self) -> &[SourceAwait] {
        &self.awaits
    }
    pub fn expression_statements(&self) -> &[SourceExpressionStatement] {
        &self.expression_statements
    }
    pub fn assignments(&self) -> &[SourceAssignment] {
        &self.assignments
    }
    pub fn callbacks(&self) -> &[SourceCallback] {
        &self.callbacks
    }
    pub fn returns(&self) -> &[SourceReturn] {
        &self.returns
    }
    pub fn conditions(&self) -> &[SourceCondition] {
        &self.conditions
    }
    pub fn switches(&self) -> &[SourceSwitch] {
        &self.switches
    }
    pub fn assertions(&self) -> &[SourceTypeAssertion] {
        &self.assertions
    }
    pub fn templates(&self) -> &[SourceTemplate] {
        &self.templates
    }
    pub fn members(&self) -> &[SourceMember] {
        &self.members
    }
    pub fn parse_diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
    pub fn is_complete(&self) -> bool {
        self.complete
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_inputs_bind_original_grammar_and_discard_partial_recovery_facts() {
        let source: Arc<str> = Arc::from(
            "const view = <div>{read()}</div>; const value = read() as number; const text = `${value!}`;",
        );
        let input = TypeSource::new("a.tsx", source.clone(), SourceType::Tsx).unwrap();
        assert_eq!(input.path(), "a.tsx");
        assert_eq!(input.source(), source.as_ref());
        assert_eq!(input.source_type(), SourceType::Tsx);
        assert!(input.is_complete());
        assert!(input.parse_diagnostics().is_empty());
        assert_eq!(input.calls().len(), 2);
        assert_eq!(input.assertions().len(), 2);
        assert_eq!(input.templates().len(), 1);
        for call in input.calls() {
            assert_eq!(
                &input.source()[call.span.lo as usize..call.span.hi as usize],
                "read()"
            );
        }
        let broken = TypeSource::new(
            "broken.ts",
            Arc::from("read(); function ("),
            SourceType::TypeScript,
        )
        .unwrap();
        assert!(!broken.is_complete());
        assert!(!broken.parse_diagnostics().is_empty());
        assert!(
            broken.calls().is_empty()
                && broken.assertions().is_empty()
                && broken.templates().is_empty()
        );
    }

    #[test]
    fn type_input_budgets_include_nested_arguments_and_template_positions() {
        let limits = Limits {
            bytes: 64,
            sites: 2,
        };
        for source in ["call(one, two)", "tag`${a}${b}`", "a as A as B"] {
            assert!(
                TypeSource::with_limits("a.ts", Arc::from(source), SourceType::TypeScript, limits)
                    .is_err(),
                "{source}"
            );
        }
        assert!(
            TypeSource::with_limits(
                "a.ts",
                Arc::from(" ".repeat(65)),
                SourceType::TypeScript,
                limits
            )
            .is_err()
        );
        assert!(TypeSource::new("", Arc::from(""), SourceType::TypeScript).is_err());
    }
}
