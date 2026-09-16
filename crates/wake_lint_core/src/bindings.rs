use std::collections::BTreeMap;
use wake_common::{FxHashMap, FxHashSet, Interner};
use wake_ecma_ast::{SourceIdentifierRole, SourceNodeKind};
use wake_ecma_parser::SourceParseOutput;
use wake_ecma_semantic::{DeclKind, SourceSemanticModel};

use crate::{EffectiveRule, LintDiagnostic, RuleLevel};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Namespace,
    Enum,
    Class,
    Function,
    Other,
}

impl Kind {
    fn bit(self) -> u8 {
        1 << self as u8
    }

    fn can_merge(self, prior: u8) -> bool {
        let allowed = match self {
            Self::Namespace => {
                Self::Namespace.bit() | Self::Enum.bit() | Self::Class.bit() | Self::Function.bit()
            }
            Self::Enum => Self::Namespace.bit() | Self::Enum.bit(),
            Self::Class | Self::Function => Self::Namespace.bit(),
            Self::Other => 0,
        };
        prior & !allowed == 0
    }
}

pub(crate) fn check(
    interner: &Interner,
    parsed: &SourceParseOutput,
    semantic: &SourceSemanticModel,
    configuration: &BTreeMap<String, EffectiveRule>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    const ID: &str = "js/no-redeclare";
    let level = configuration[ID].configuration.level;
    let before = &configuration["js/no-use-before-define"].configuration;
    let shadow = &configuration["js/no-shadow"].configuration;
    if level == RuleLevel::Off && before.level == RuleLevel::Off && shadow.level == RuleLevel::Off {
        return;
    }
    let original: FxHashSet<_> = parsed
        .identifiers
        .iter()
        .filter(|id| {
            matches!(
                id.role,
                SourceIdentifierRole::ValueBinding | SourceIdentifierRole::ValueReference
            )
        })
        .map(|id| (id.span, interner.intern(&id.name)))
        .collect();
    let before_original: FxHashSet<_> = parsed
        .identifiers
        .iter()
        .filter(|id| {
            matches!(
                id.role,
                SourceIdentifierRole::ValueBinding
                    | SourceIdentifierRole::EnumMemberBinding
                    | SourceIdentifierRole::ValueReference
            )
        })
        .map(|id| (id.span, interner.intern(&id.name)))
        .collect();
    let enum_members: FxHashSet<_> = parsed
        .identifiers
        .iter()
        .filter(|id| id.role == SourceIdentifierRole::EnumMemberBinding)
        .map(|id| (id.span, interner.intern(&id.name)))
        .collect();
    let mut special: FxHashMap<_, _> = parsed
        .namespaces
        .iter()
        .flat_map(|namespace| &namespace.names)
        .map(|name| (name.span, Kind::Namespace))
        .collect();
    special.extend(parsed.type_declarations.iter().filter_map(|declaration| {
        (parsed.syntax[declaration.node].kind == SourceNodeKind::TsEnum)
            .then_some((declaration.name.span, Kind::Enum))
    }));
    let mut declarations: Vec<_> = semantic
        .model
        .binding_occurrences
        .iter()
        .filter(|declaration| original.contains(&(declaration.span, declaration.name)))
        .collect();
    declarations.sort_by_key(|declaration| (declaration.span.lo, declaration.span.hi));
    let mut first = FxHashMap::default();
    for declaration in &declarations {
        first.entry(declaration.symbol).or_insert(*declaration);
    }
    let mut before_declarations: Vec<_> = semantic
        .model
        .binding_occurrences
        .iter()
        .filter(|declaration| before_original.contains(&(declaration.span, declaration.name)))
        .collect();
    before_declarations.sort_by_key(|declaration| (declaration.span.lo, declaration.span.hi));
    let mut before_first = FxHashMap::default();
    for declaration in &before_declarations {
        before_first
            .entry(declaration.symbol)
            .or_insert(*declaration);
    }
    if shadow.level != RuleLevel::Off {
        let copies: FxHashSet<_> = semantic
            .model
            .parameter_copies
            .iter()
            .map(|copy| copy.body)
            .chain(
                semantic
                    .model
                    .annex_b_copies
                    .iter()
                    .map(|copy| copy.lexical),
            )
            .collect();
        let class_names: FxHashSet<_> = parsed
            .type_declarations
            .iter()
            .filter(|declaration| declaration.in_own_scope)
            .map(|declaration| declaration.name.span)
            .collect();
        let mut seen = FxHashSet::default();
        for declaration in &declarations {
            let scope = &semantic.model.scopes[declaration.scope as usize];
            if !seen.insert((declaration.scope, declaration.name))
                || copies.contains(&declaration.symbol)
                || (semantic.incomplete_value_names.contains(&declaration.name)
                    && !semantic.source_symbols.contains(&declaration.symbol))
                || semantic.ambient_value_symbols.contains(&declaration.symbol)
                || (shadow.options["ignore_named_expressions"] == true
                    && (scope.kind == wake_ecma_semantic::ScopeKind::FunctionName
                        || class_names.contains(&declaration.span)))
            {
                continue;
            }
            let Some(outer) = scope
                .parent
                .and_then(|parent| semantic.model.resolve_in(parent, declaration.name))
            else {
                continue;
            };
            let Some(outer_declaration) = first.get(&outer) else {
                continue;
            };
            if shadow.options["hoist"] == false && outer_declaration.span.lo > declaration.span.lo {
                continue;
            }
            diagnostics.push(LintDiagnostic {
                rule_id: "js/no-shadow".into(),
                level: shadow.level,
                message_id: "shadow".into(),
                message: format!(
                    "'{}' shadows a binding in an outer scope.",
                    interner.resolve(declaration.name)
                ),
                start: declaration.span.lo,
                end: declaration.span.hi,
                fix: None,
            });
        }
    }
    if before.level != RuleLevel::Off {
        for &index in &semantic.references {
            let reference = &semantic.model.references[index];
            let Some(declaration) = reference
                .resolved
                .and_then(|symbol| before_first.get(&symbol))
            else {
                continue;
            };
            if semantic.ambient_value_symbols.contains(&declaration.symbol)
                && !enum_members.contains(&(declaration.span, declaration.name))
            {
                continue;
            }
            let category = match declaration.decl_kind {
                DeclKind::Function => "functions",
                DeclKind::Class => "classes",
                _ => "variables",
            };
            if reference.span.lo < declaration.span.lo && before.options[category] == true {
                diagnostics.push(LintDiagnostic {
                    rule_id: "js/no-use-before-define".into(),
                    level: before.level,
                    message_id: "before".into(),
                    message: format!(
                        "Use of '{}' precedes its declaration.",
                        interner.resolve(reference.name)
                    ),
                    start: reference.span.lo,
                    end: reference.span.hi,
                    fix: None,
                });
            }
        }
    }
    if level == RuleLevel::Off {
        return;
    }
    let mut seen: FxHashMap<_, u8> = FxHashMap::default();
    for declaration in declarations {
        let kind = special
            .get(&declaration.span)
            .copied()
            .unwrap_or(match declaration.decl_kind {
                DeclKind::Class => Kind::Class,
                DeclKind::Function => Kind::Function,
                _ => Kind::Other,
            });
        let prior = seen
            .entry((declaration.scope, declaration.name))
            .or_default();
        if *prior != 0 && !kind.can_merge(*prior) {
            diagnostics.push(LintDiagnostic {
                rule_id: ID.into(),
                level,
                message_id: "duplicate".into(),
                message: format!(
                    "'{}' is already declared in this scope.",
                    interner.resolve(declaration.name)
                ),
                start: declaration.span.lo,
                end: declaration.span.hi,
                fix: None,
            });
        }
        *prior |= kind.bit();
    }
}
