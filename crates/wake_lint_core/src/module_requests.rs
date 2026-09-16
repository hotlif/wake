//! Original request projection for product-owned resolution; no I/O or installation policy.
use wake_common::{Diagnostic, FxHashMap, FxHashSet, Interner, JsString, Span};
use wake_ecma_ast::*;
use wake_ecma_parser::{ParseOptions, SourceParseOutput, SourceType, parse_source};
use wake_ecma_semantic::{SourceSemanticInput, SourceSemanticModel, analyze_source};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleRequestKind {
    Import,
    Export,
    ImportEquals,
    TypeImport,
    DynamicImport,
    Require,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleRequest {
    pub kind: ModuleRequestKind,
    pub span: Span,
    pub specifier_span: Span,
    /// None is an evaluated request whose value cannot be proven from literal syntax.
    pub specifier: Option<JsString>,
    pub type_only: bool,
    /// Source declaration container, distinct from the physical file's module identity.
    pub parent: Option<Span>,
    pub attributes: Vec<(JsString, JsString)>,
    pub attributes_known: bool,
}

#[derive(Debug)]
pub struct ModuleRequests {
    pub requests: Vec<ModuleRequest>,
    pub parse_diagnostics: Vec<Diagnostic>,
    /// Dynamic scope prevents proof that apparent require calls denote the module loader.
    pub incomplete: bool,
}

pub fn inspect_module(source: &str, source_type: SourceType) -> ModuleRequests {
    let interner = Interner::new();
    let parsed = parse_source(source, &interner, source_type, ParseOptions::default());
    if parsed.parsed.has_errors() {
        return ModuleRequests {
            requests: Vec::new(),
            parse_diagnostics: parsed.parsed.diagnostics,
            incomplete: true,
        };
    }
    let (requests, incomplete) = parsed.parsed.module.with_ast(|program| {
        let semantic = analyze_source(
            program,
            &interner,
            SourceSemanticInput {
                identifiers: &parsed.identifiers,
                exports: &parsed.exports,
                syntax: &parsed.syntax,
                functions: &parsed.functions,
                namespaces: &parsed.namespaces,
            },
        );
        project(program, &parsed, &interner, &semantic)
    });
    ModuleRequests {
        requests,
        parse_diagnostics: parsed.parsed.diagnostics,
        incomplete,
    }
}

fn attributes(attributes: Option<&SourceImportAttributes>) -> Vec<(JsString, JsString)> {
    let mut entries: Vec<_> = attributes
        .into_iter()
        .flat_map(|attributes| &attributes.entries)
        .map(|entry| (entry.key.clone(), entry.value.clone()))
        .collect();
    entries.sort();
    entries
}

fn project(
    program: &Program<'_>,
    parsed: &SourceParseOutput,
    interner: &Interner,
    semantic: &SourceSemanticModel,
) -> (Vec<ModuleRequest>, bool) {
    let mut requests = Vec::new();
    let mut equals = FxHashSet::default();
    for import in &parsed.type_imports {
        let mut attributes: Vec<_> = import
            .attributes
            .iter()
            .map(|entry| (entry.key.clone(), entry.value.clone()))
            .collect();
        attributes.sort();
        requests.push(ModuleRequest {
            kind: ModuleRequestKind::TypeImport,
            span: import.span,
            specifier_span: import.source.span,
            specifier: Some(import.source.value.clone()),
            type_only: true,
            parent: None,
            attributes,
            attributes_known: import.attributes_known,
        });
    }
    for import in &parsed.imports {
        let Some(source) = &import.source else {
            continue;
        };
        if let Some(target) = import.equals_target {
            equals.insert(target);
        }
        requests.push(ModuleRequest {
            kind: if import.equals_target.is_some() {
                ModuleRequestKind::ImportEquals
            } else {
                ModuleRequestKind::Import
            },
            span: import.span,
            specifier_span: source.span,
            specifier: Some(source.value.clone()),
            type_only: import.type_only
                || !import.bindings.is_empty()
                    && import.bindings.iter().all(|binding| binding.type_only),
            parent: import.parent.map(|parent| parsed.syntax[parent].span),
            attributes: attributes(import.attributes.as_ref()),
            attributes_known: true,
        });
    }
    for export in &parsed.exports {
        let Some(source) = &export.source else {
            continue;
        };
        requests.push(ModuleRequest {
            kind: ModuleRequestKind::Export,
            span: export.span,
            specifier_span: source.span,
            specifier: Some(source.value.clone()),
            type_only: export.type_only
                || !export.specifiers.is_empty()
                    && export.specifiers.iter().all(|binding| binding.type_only),
            parent: export.parent.map(|parent| parsed.syntax[parent].span),
            attributes: attributes(export.attributes.as_ref()),
            attributes_known: true,
        });
    }
    let references = semantic
        .references
        .iter()
        .map(|&index| {
            let reference = &semantic.model.references[index];
            (
                reference.span,
                (
                    reference.resolved.is_none(),
                    reference
                        .resolved
                        .is_some_and(|symbol| semantic.ambient_value_symbols.contains(&symbol)),
                ),
            )
        })
        .collect();
    let mut visitor = Requests {
        interner,
        semantic,
        references,
        equals,
        requests,
        with_depth: 0,
        direct_eval: false,
        incomplete: false,
    };
    visitor.visit_program(program);
    if visitor.direct_eval {
        visitor
            .requests
            .retain(|request| request.kind != ModuleRequestKind::Require);
        visitor.incomplete = true;
    }
    visitor
        .requests
        .sort_by_key(|request| (request.span.lo, request.span.hi));
    (visitor.requests, visitor.incomplete)
}

struct Requests<'s> {
    interner: &'s Interner,
    semantic: &'s SourceSemanticModel,
    references: FxHashMap<Span, (bool, bool)>,
    equals: FxHashSet<Span>,
    requests: Vec<ModuleRequest>,
    with_depth: usize,
    direct_eval: bool,
    incomplete: bool,
}
impl Requests<'_> {
    fn expression_request(
        &mut self,
        kind: ModuleRequestKind,
        span: Span,
        expression: Expression<'_>,
        options: Option<Expression<'_>>,
    ) {
        let specifier = match expression {
            Expression::StringLiteral(literal) => Some(self.interner.resolve_js(literal.value)),
            _ => None,
        };
        let attributes = match options {
            None => Some(Vec::new()),
            Some(options) => dynamic_attributes(options, self.interner),
        };
        self.requests.push(ModuleRequest {
            kind,
            span,
            specifier_span: expression.span(),
            specifier,
            type_only: false,
            parent: None,
            attributes_known: attributes.is_some(),
            attributes: attributes.unwrap_or_default(),
        });
    }
}
impl<'a> Visit<'a> for Requests<'_> {
    fn visit_statement(&mut self, statement: &Statement<'a>) {
        if let Statement::With(with) = statement {
            self.visit_expression(&with.object);
            self.with_depth += 1;
            self.visit_statement(&with.body);
            self.with_depth -= 1;
        } else {
            walk_statement(self, statement);
        }
    }
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        match expression {
            Expression::Import(import) => self.expression_request(
                ModuleRequestKind::DynamicImport,
                import.span,
                import.source,
                import.options,
            ),
            Expression::Call(call) if !self.equals.contains(&call.span) => {
                if let Expression::Identifier(id) = call.callee {
                    let name = self.interner.resolve(id.name);
                    let original = self.references.get(&id.span).copied();
                    if name == "eval"
                        && original.is_some_and(|(unresolved, ambient)| unresolved && !ambient)
                        && !call.optional
                    {
                        self.direct_eval = true;
                    }
                    if name == "require"
                        && let Some((unresolved, ambient)) = original
                    {
                        if self.with_depth > 0
                            || ambient
                            || (unresolved
                                && self.semantic.incomplete_value_names.contains(&id.name))
                        {
                            self.incomplete = true;
                        } else if unresolved && let [argument] = call.arguments.as_slice() {
                            self.expression_request(
                                ModuleRequestKind::Require,
                                call.span,
                                *argument,
                                None,
                            );
                        }
                    }
                }
            }
            _ => {}
        }
        walk_expression(self, expression);
    }
}

fn object_entries<'a>(
    expression: Expression<'a>,
    interner: &Interner,
) -> Option<Vec<(JsString, Expression<'a>)>> {
    let Expression::Object(object) = expression else {
        return None;
    };
    let mut entries = Vec::new();
    for member in &object.properties {
        let ObjectMember::Property(property) = member else {
            return None;
        };
        if property.kind != PropertyKind::Init || property.method || property.prototype_setter {
            return None;
        }
        let name = match property.key {
            PropertyKey::Ident(id) if !property.computed => interner.resolve(id.name).into(),
            PropertyKey::String(literal) => interner.resolve_js(literal.value),
            _ => return None,
        };
        entries.push((name, property.value));
    }
    Some(entries)
}

fn dynamic_attributes(
    expression: Expression<'_>,
    interner: &Interner,
) -> Option<Vec<(JsString, JsString)>> {
    let options = object_entries(expression, interner)?;
    let mut attributes = None;
    for (name, value) in options {
        if name == "with" {
            attributes = Some(value);
        }
    }
    let Some(attributes) = attributes else {
        return Some(Vec::new());
    };
    let mut entries = std::collections::BTreeMap::new();
    for (name, value) in object_entries(attributes, interner)? {
        let Expression::StringLiteral(literal) = value else {
            return None;
        };
        entries.insert(name, interner.resolve_js(literal.value));
    }
    Some(entries.into_iter().collect())
}
