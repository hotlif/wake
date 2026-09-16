use std::collections::BTreeMap;
use wake_common::{FxHashMap, FxHashSet, Interner};
use wake_ecma_parser::SourceParseOutput;
use wake_ecma_semantic::{
    SourceSemanticModel, SourceTypeModel, SourceTypeQueryResolution, TypeDeclarationKind,
    TypeResolution,
};

use crate::{LintDiagnostic, RuleLevel};

pub(crate) fn check(
    interner: &Interner,
    parsed: &SourceParseOutput,
    values: &SourceSemanticModel,
    types: &SourceTypeModel,
    levels: &BTreeMap<String, RuleLevel>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    const ID: &str = "ts/consistent-type-imports";
    let level = levels[ID];
    if level == RuleLevel::Off && levels["ts/consistent-type-exports"] == RuleLevel::Off {
        return;
    }
    check_exports(
        interner,
        parsed,
        values,
        types,
        levels["ts/consistent-type-exports"],
        diagnostics,
    );
    if level == RuleLevel::Off {
        return;
    }
    let type_used: FxHashSet<_> = types
        .references
        .iter()
        .filter_map(|reference| match reference.resolution {
            TypeResolution::Resolved(symbol) => Some(symbol),
            _ => None,
        })
        .collect();
    let import_types: FxHashMap<_, _> = types
        .symbols
        .iter()
        .flat_map(|symbol| {
            symbol
                .declarations
                .iter()
                .filter(|decl| decl.kind == TypeDeclarationKind::Import)
                .map(|decl| (decl.span, symbol.id))
        })
        .collect();
    let value_bindings: FxHashMap<_, _> = values
        .model
        .binding_occurrences
        .iter()
        .filter(|binding| values.source_symbols.contains(&binding.symbol))
        .map(|binding| (binding.span, binding.symbol))
        .collect();
    let mut value_used: FxHashSet<_> = values
        .references
        .iter()
        .filter_map(|&index| values.model.references[index].resolved)
        .collect();
    value_used.extend(values.exports.iter().filter_map(|export| export.resolved));
    let mut query_used = FxHashSet::default();
    let mut unknown_queries = FxHashSet::default();
    for query in &values.type_queries {
        match query.resolution {
            SourceTypeQueryResolution::Resolved(symbol) => {
                query_used.insert(symbol);
            }
            _ => {
                unknown_queries.insert(query.name);
            }
        }
    }
    for import in &parsed.imports {
        if import.type_only || import.equals_target.is_some() || import.attributes.is_some() {
            continue;
        }
        for binding in &import.bindings {
            if binding.type_only {
                continue;
            }
            let Some(&value_symbol) = value_bindings.get(&binding.local.span) else {
                continue;
            };
            if value_used.contains(&value_symbol)
                || unknown_queries.contains(&interner.intern(&binding.local.name))
            {
                continue;
            }
            let has_type_use = import_types
                .get(&binding.local.span)
                .is_some_and(|symbol| type_used.contains(symbol));
            if has_type_use || query_used.contains(&value_symbol) {
                diagnostics.push(LintDiagnostic {
                    rule_id: ID.into(),
                    level,
                    message_id: "type".into(),
                    message: format!(
                        "Import '{}' is used only in types; mark it as a type import.",
                        binding.local.name
                    ),
                    start: binding.local.span.lo,
                    end: binding.local.span.hi,
                    fix: None,
                });
            }
        }
    }
}

fn check_exports(
    interner: &Interner,
    parsed: &SourceParseOutput,
    values: &SourceSemanticModel,
    types: &wake_ecma_semantic::SourceTypeModel,
    level: RuleLevel,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    if level == RuleLevel::Off {
        return;
    }
    let value_exports: FxHashSet<_> = values
        .exports
        .iter()
        .filter(|export| export.resolved.is_some())
        .map(|export| export.span)
        .collect();
    for export in &parsed.exports {
        if export.kind != wake_ecma_ast::SourceExportKind::Named
            || export.type_only
            || export.source.is_some()
        {
            continue;
        }
        for specifier in &export.specifiers {
            let local = &specifier.local;
            if specifier.type_only || !local.identifier || value_exports.contains(&local.span) {
                continue;
            }
            let name = interner.intern(&local.value);
            let Some(symbol) = types.resolve_in(types.scope_at(local.span), name) else {
                continue;
            };
            if types.symbols[symbol.0]
                .declarations
                .iter()
                .all(|declaration| {
                    matches!(
                        declaration.kind,
                        TypeDeclarationKind::Alias
                            | TypeDeclarationKind::Interface
                            | TypeDeclarationKind::TypeImport
                    )
                })
            {
                diagnostics.push(LintDiagnostic {
                    rule_id: "ts/consistent-type-exports".into(),
                    level,
                    message_id: "type".into(),
                    message: format!(
                        "Export '{}' has only a type binding; mark it as a type export.",
                        local.value
                    ),
                    start: local.span.lo,
                    end: local.span.hi,
                    fix: None,
                });
            }
        }
    }
}
