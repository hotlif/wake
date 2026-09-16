use crate::source_helpers::{jsx_tags, pattern_names};
use crate::{EffectiveRule, LintDiagnostic, RuleLevel};
use std::collections::BTreeMap;
use wake_common::{Atom, FxHashMap, FxHashSet, Interner, Span};
use wake_ecma_ast::*;
use wake_ecma_parser::SourceParseOutput;
use wake_ecma_semantic::{
    DeclKind, ScopeKind, SourceSemanticModel, SourceTypeModel, SourceTypeQueryResolution,
    TypeDeclarationKind, TypeResolution,
};

#[derive(Default)]
struct UsageSyntax {
    parameters: Vec<Vec<Vec<Ident>>>,
    self_regions: Vec<(Ident, Span)>,
    discarded: FxHashSet<Span>,
    rest_siblings: FxHashSet<Span>,
    calls: FxHashSet<Span>,
    has_with: bool,
}

impl UsageSyntax {
    fn params(&mut self, params: &[Pattern<'_>]) {
        self.parameters.push(
            params
                .iter()
                .map(|&pattern| {
                    let mut names = Vec::new();
                    pattern_names(pattern, &mut names);
                    names
                })
                .collect(),
        );
    }
    fn discarded(&mut self, expression: Expression<'_>) {
        match expression {
            Expression::Update(value) => {
                if let Expression::Identifier(id) = value.argument {
                    self.discarded.insert(id.span);
                }
            }
            Expression::Assignment(value) if value.operator != AssignmentOperator::Assign => {
                if let Expression::Identifier(id) = value.left {
                    self.discarded.insert(id.span);
                }
            }
            Expression::Sequence(value) => {
                for &item in &value.expressions {
                    self.discarded(item);
                }
            }
            Expression::Unary(value) if value.operator == UnaryOperator::Void => {
                self.discarded(value.argument)
            }
            _ => {}
        }
    }
}

impl<'a> Visit<'a> for UsageSyntax {
    fn visit_statement(&mut self, statement: &Statement<'a>) {
        match statement {
            Statement::FunctionDeclaration(function) => {
                if let Some(id) = function.id {
                    self.self_regions.push((id, function.span));
                }
            }
            Statement::VariableDeclaration(declaration) => {
                for item in &declaration.declarations {
                    if let Pattern::Ident(id) = item.id
                        && let Some(init) = item.init
                        && matches!(init, Expression::Function(_) | Expression::Arrow(_))
                    {
                        self.self_regions.push((*id, init.span()));
                    }
                }
            }
            Statement::Expression(value) => self.discarded(value.expression),
            Statement::With(_) => self.has_with = true,
            Statement::For(value) => {
                if let Some(update) = value.update {
                    self.discarded(update);
                }
            }
            _ => {}
        }
        walk_statement(self, statement);
    }
    fn visit_function(&mut self, function: &Function<'a>) {
        self.params(&function.params);
        walk_function(self, function);
    }
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        if let Expression::Call(call) = expression
            && !call.optional
            && let Expression::Identifier(id) = call.callee
        {
            self.calls.insert(id.span);
        }
        if let Expression::Arrow(arrow) = expression {
            self.params(&arrow.params);
        }
        walk_expression(self, expression);
    }
    fn visit_pattern(&mut self, pattern: &Pattern<'a>) {
        if let Pattern::Object(object) = pattern
            && object.rest.is_some()
        {
            for property in &object.properties {
                let mut names = Vec::new();
                pattern_names(property.value, &mut names);
                self.rest_siblings
                    .extend(names.into_iter().map(|id| id.span));
            }
        }
        walk_pattern(self, pattern);
    }
}

fn contains(outer: Span, inner: Span) -> bool {
    outer.lo <= inner.lo && inner.hi <= outer.hi
}

fn merge_regions(regions: &mut Vec<Span>) {
    regions.sort_by_key(|span| (span.lo, span.hi));
    let mut merged: Vec<Span> = Vec::new();
    for span in std::mem::take(regions) {
        if let Some(previous) = merged.last_mut()
            && span.lo <= previous.hi
        {
            previous.hi = previous.hi.max(span.hi);
        } else {
            merged.push(span);
        }
    }
    *regions = merged;
}

fn inside(regions: &[Span], span: Span) -> bool {
    regions
        .partition_point(|region| region.lo <= span.lo)
        .checked_sub(1)
        .is_some_and(|index| contains(regions[index], span))
}

struct Filter {
    pattern: Option<regex::Regex>,
    report_used: bool,
}
impl Filter {
    fn new(rule: &crate::RuleConfiguration, name: &str) -> Self {
        let text = rule.options[name].as_str().expect("validated pattern");
        Self {
            pattern: (!text.is_empty())
                .then(|| regex::Regex::new(text).expect("validated pattern")),
            report_used: rule.options["report_used_ignore_pattern"] == true,
        }
    }
    fn report(&self, name: &str, used: bool) -> Option<&'static str> {
        if self
            .pattern
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(name))
        {
            (used && self.report_used).then_some("usedIgnored")
        } else {
            (!used).then_some("unused")
        }
    }
}

pub(crate) fn check(
    program: &Program<'_>,
    interner: &Interner,
    parsed: &SourceParseOutput,
    values: &SourceSemanticModel,
    types: Option<&SourceTypeModel>,
    configuration: &BTreeMap<String, EffectiveRule>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let rule = &configuration["js/no-unused-vars"].configuration;
    if rule.level == RuleLevel::Off {
        return;
    }
    let mut syntax = UsageSyntax::default();
    syntax.visit_program(program);
    if crate::source_helpers::dynamic_access(values, interner, &syntax.calls, syntax.has_with) {
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
    let mut declarations: Vec<_> = values
        .model
        .binding_occurrences
        .iter()
        .filter(|binding| {
            original.contains(&(binding.span, binding.name))
                // An erased string ambient module may have the same spelling as a represented
                // local binding. The incomplete-name guard protects only declarations without a
                // source symbol; source-owned symbols remain eligible for declaration diagnostics.
                && (!values.incomplete_value_names.contains(&binding.name)
                    || values.source_symbols.contains(&binding.symbol))
        })
        .collect();
    declarations.sort_by_key(|binding| (binding.span.lo, binding.span.hi));
    let mut seen = FxHashSet::default();
    declarations.retain(|binding| seen.insert(binding.symbol));
    let by_span: FxHashMap<_, _> = values
        .model
        .binding_occurrences
        .iter()
        .filter(|binding| original.contains(&(binding.span, binding.name)))
        .map(|binding| ((binding.span, binding.name), binding.symbol))
        .collect();
    let mut self_regions: FxHashMap<_, Vec<_>> = FxHashMap::default();
    for (id, span) in syntax.self_regions {
        if let Some(&symbol) = by_span.get(&(id.span, id.name)) {
            self_regions.entry(symbol).or_default().push(span);
        }
    }
    for regions in self_regions.values_mut() {
        merge_regions(regions);
    }
    let mut ambient: Vec<_> = parsed
        .syntax
        .iter()
        .filter(|node| {
            matches!(
                node.kind,
                SourceNodeKind::TsDeclare
                    | SourceNodeKind::TsAmbientModule
                    | SourceNodeKind::TsGlobalAugmentation
            )
        })
        .map(|node| node.span)
        .collect();
    merge_regions(&mut ambient);
    let tags = jsx_tags(&parsed.syntax);
    let jsx = configuration["react/jsx-uses-vars"].configuration.level != RuleLevel::Off;
    let mut used = FxHashSet::default();
    for &index in &values.references {
        let reference = &values.model.references[index];
        let Some(symbol) = reference.resolved else {
            continue;
        };
        if !reference.access.is_read()
            || syntax.discarded.contains(&reference.span)
            || (!jsx
                && tags
                    .get(&reference.span.lo)
                    .is_some_and(|&end| reference.span.hi <= end))
            || self_regions
                .get(&symbol)
                .is_some_and(|regions| inside(regions, reference.span))
        {
            continue;
        }
        used.insert(symbol);
    }
    used.extend(values.exports.iter().filter_map(|export| export.resolved));
    let mut unknown_queries = FxHashSet::default();
    for query in &values.type_queries {
        if let SourceTypeQueryResolution::Resolved(symbol) = query.resolution {
            used.insert(symbol);
        } else if query.resolution == SourceTypeQueryResolution::Unavailable {
            unknown_queries.insert(query.name);
        }
    }
    let mut type_used = FxHashSet::default();
    let mut type_values = FxHashMap::default();
    if let Some(types) = types {
        for symbol in &types.symbols {
            if let Some(value) = symbol
                .declarations
                .iter()
                .find_map(|declaration| by_span.get(&(declaration.span, symbol.name)))
            {
                type_values.insert(symbol.id, *value);
            }
        }
        let owners: FxHashMap<_, _> = parsed
            .type_declarations
            .iter()
            .filter(|declaration| {
                matches!(
                    parsed.syntax[declaration.node].kind,
                    SourceNodeKind::TsTypeAlias | SourceNodeKind::TsInterface
                )
            })
            .map(|declaration| (declaration.name.span, parsed.syntax[declaration.node].span))
            .collect();
        let self_types: FxHashMap<_, _> = types
            .symbols
            .iter()
            .map(|symbol| {
                let mut regions: Vec<_> = symbol
                    .declarations
                    .iter()
                    .filter_map(|declaration| owners.get(&declaration.span).copied())
                    .collect();
                merge_regions(&mut regions);
                (symbol.id, regions)
            })
            .collect();
        for reference in &types.references {
            if let TypeResolution::Resolved(symbol) = reference.resolution {
                if inside(&self_types[&symbol], reference.span) {
                    continue;
                }
                type_used.insert(symbol);
            }
        }
        let mut export_targets: FxHashMap<_, Vec<_>> = FxHashMap::default();
        for declaration in &parsed.type_declarations {
            export_targets
                .entry(parsed.syntax[declaration.node].span.lo)
                .or_default()
                .push(declaration);
        }
        for export in &parsed.exports {
            if export.source.is_some() {
                continue;
            }
            if export.kind == SourceExportKind::Named {
                for specifier in &export.specifiers {
                    if specifier.local.identifier
                        && let Some(symbol) = types.resolve_in(
                            types.scope_at(specifier.local.span),
                            interner.intern(&specifier.local.value),
                        )
                    {
                        type_used.insert(symbol);
                    }
                }
            } else if matches!(
                export.kind,
                SourceExportKind::Declaration | SourceExportKind::Default
            ) && let Some(target) = export.target
            {
                for declaration in export_targets.get(&target.lo).into_iter().flatten() {
                    if let Some(symbol) = types.resolve_in(
                        types.scope_at(declaration.name.span),
                        interner.intern(&declaration.name.name),
                    ) {
                        type_used.insert(symbol);
                    }
                }
            }
        }
        used.extend(
            type_used
                .iter()
                .filter_map(|symbol| type_values.get(symbol))
                .copied(),
        );
    }
    // Runtime alias/copy environments share the same original use. These are semantic edges,
    // not fabricated source reads, so propagate only from actually used bindings.
    let mut edges: FxHashMap<_, Vec<_>> = FxHashMap::default();
    for (a, b) in values
        .model
        .parameter_copies
        .iter()
        .map(|copy| (copy.parameter, copy.body))
        .chain(
            values
                .model
                .annex_b_copies
                .iter()
                .filter_map(|copy| copy.outer.map(|outer| (copy.lexical, outer))),
        )
    {
        edges.entry(a).or_default().push(b);
        edges.entry(b).or_default().push(a);
    }
    let mut pending: Vec<_> = used.iter().copied().collect();
    while let Some(symbol) = pending.pop() {
        if let Some(neighbors) = edges.get(&symbol) {
            for &neighbor in neighbors {
                if used.insert(neighbor) {
                    pending.push(neighbor);
                }
            }
        }
    }
    let mut positions = FxHashMap::default();
    let mut last_used: FxHashMap<usize, usize> = FxHashMap::default();
    for (function, parameters) in syntax.parameters.iter().enumerate() {
        for (position, names) in parameters.iter().enumerate() {
            for id in names {
                if let Some(&symbol) = by_span.get(&(id.span, id.name)) {
                    positions.insert(symbol, (function, position));
                    if used.contains(&symbol) {
                        last_used
                            .entry(function)
                            .and_modify(|last| *last = (*last).max(position))
                            .or_insert(position);
                    }
                }
            }
        }
    }
    let vars_filter = Filter::new(rule, "vars_ignore_pattern");
    let args_filter = Filter::new(rule, "args_ignore_pattern");
    let catch_filter = Filter::new(rule, "caught_errors_ignore_pattern");
    for declaration in declarations {
        let is_used = used.contains(&declaration.symbol);
        if unknown_queries.contains(&declaration.name)
            || declaration.decl_kind == DeclKind::Arguments
            || values.model.scopes[declaration.scope as usize].kind == ScopeKind::FunctionName
            || inside(&ambient, declaration.span)
            || (rule.options["ignore_rest_siblings"] == true
                && syntax.rest_siblings.contains(&declaration.span))
        {
            continue;
        }
        let filter = match declaration.decl_kind {
            DeclKind::Param => {
                if rule.options["args"] == "none" {
                    continue;
                }
                if !is_used
                    && rule.options["args"] == "after-used"
                    && positions
                        .get(&declaration.symbol)
                        .is_some_and(|(function, position)| {
                            last_used.get(function).is_some_and(|last| position < last)
                        })
                {
                    continue;
                }
                &args_filter
            }
            DeclKind::CatchParam => {
                if rule.options["caught_errors"] == "none" {
                    continue;
                }
                &catch_filter
            }
            _ => {
                if rule.options["vars"] == "local"
                    && values.model.scopes[declaration.scope as usize].kind == ScopeKind::Module
                {
                    continue;
                }
                &vars_filter
            }
        };
        if let Some(message) = filter.report(&interner.resolve(declaration.name), is_used) {
            report(
                interner,
                rule.level,
                declaration.name,
                declaration.span,
                message,
                diagnostics,
            );
        }
    }
    if let Some(types) = types {
        for symbol in &types.symbols {
            if type_values.contains_key(&symbol.id) {
                continue;
            }
            let Some(declaration) = symbol.declarations.first() else {
                continue;
            };
            let scope = &types.scopes[declaration.scope.0];
            if matches!(
                declaration.kind,
                TypeDeclarationKind::Mapped
                    | TypeDeclarationKind::Infer
                    | TypeDeclarationKind::Namespace
            ) || scope.ambient
                || inside(&ambient, declaration.span)
                || (rule.options["vars"] == "local"
                    && matches!(
                        scope.kind,
                        wake_ecma_semantic::TypeScopeKind::Module
                            | wake_ecma_semantic::TypeScopeKind::Global
                    ))
            {
                continue;
            }
            if let Some(message) = vars_filter.report(
                &interner.resolve(symbol.name),
                type_used.contains(&symbol.id),
            ) {
                report(
                    interner,
                    rule.level,
                    symbol.name,
                    declaration.span,
                    message,
                    diagnostics,
                );
            }
        }
    }
}

fn report(
    interner: &Interner,
    level: RuleLevel,
    name: Atom,
    span: Span,
    message: &str,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    diagnostics.push(LintDiagnostic {
        rule_id: "js/no-unused-vars".into(),
        level,
        message_id: message.into(),
        message: if message == "unused" {
            format!("'{}' is declared but never used.", interner.resolve(name))
        } else {
            format!(
                "'{}' is used but matches an ignore pattern.",
                interner.resolve(name)
            )
        },
        start: span.lo,
        end: span.hi,
        fix: None,
    });
}
