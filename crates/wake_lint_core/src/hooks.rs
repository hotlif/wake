//! React policy over original binding identities and semantic-owned execution paths.
use crate::{LintDiagnostic, RuleLevel};
use wake_common::{FxHashMap, FxHashSet, Interner, Span};
use wake_ecma_ast::*;
use wake_ecma_parser::SourceParseOutput;
use wake_ecma_semantic::{CallExecutionFacts, SourceSemanticModel, SymbolId};

pub(crate) mod dependencies;

pub(crate) const RULE: &str = "react-hooks/rules-of-hooks";

fn hook_name(name: &str) -> bool {
    name.strip_prefix("use")
        .and_then(|tail| tail.as_bytes().first())
        .is_some_and(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

fn component_name(name: &str) -> bool {
    name.as_bytes().first().is_some_and(u8::is_ascii_uppercase) || hook_name(name)
}

enum ImportOrigin {
    ReactNamespace,
    Named { name: String, react: bool },
}

struct Identities<'s> {
    interner: &'s Interner,
    semantic: &'s SourceSemanticModel,
    references: FxHashMap<Span, SymbolId>,
    bindings: FxHashMap<Span, SymbolId>,
    ambient: FxHashSet<SymbolId>,
    imports: FxHashMap<SymbolId, ImportOrigin>,
}

impl<'s> Identities<'s> {
    fn new(
        parsed: &SourceParseOutput,
        interner: &'s Interner,
        semantic: &'s SourceSemanticModel,
    ) -> Self {
        let original: FxHashSet<_> = parsed
            .identifiers
            .iter()
            .filter(|id| id.role == SourceIdentifierRole::ValueBinding)
            .map(|id| id.span)
            .collect();
        let bindings: FxHashMap<_, _> = semantic
            .model
            .binding_occurrences
            .iter()
            .filter(|binding| {
                original.contains(&binding.span)
                    && semantic.source_symbols.contains(&binding.symbol)
            })
            .map(|binding| (binding.span, binding.symbol))
            .collect();
        let references = semantic
            .references
            .iter()
            .filter_map(|&index| {
                let reference = &semantic.model.references[index];
                if reference.resolved.is_none()
                    && semantic.incomplete_value_names.contains(&reference.name)
                {
                    return None;
                }
                reference
                    .resolved
                    .filter(|symbol| semantic.source_symbols.contains(symbol))
                    .map(|symbol| (reference.span, symbol))
            })
            .collect();
        let mut imports = FxHashMap::default();
        for import in &parsed.imports {
            if import.type_only {
                continue;
            }
            let react = import
                .source
                .as_ref()
                .is_some_and(|source| source.value == "react");
            for binding in &import.bindings {
                if binding.type_only {
                    continue;
                }
                let Some(&symbol) = bindings.get(&binding.local.span) else {
                    continue;
                };
                let origin = match binding.kind {
                    SourceImportBindingKind::Default | SourceImportBindingKind::Namespace
                        if react =>
                    {
                        ImportOrigin::ReactNamespace
                    }
                    SourceImportBindingKind::Named => ImportOrigin::Named {
                        name: binding.imported.clone().unwrap_or_default(),
                        react,
                    },
                    _ => continue,
                };
                imports.insert(symbol, origin);
            }
        }
        Self {
            interner,
            semantic,
            references,
            bindings,
            ambient: semantic.ambient_value_symbols.clone(),
            imports,
        }
    }

    /// A named binding is evidence; a same-spelled object property is not.
    fn callee(&self, expression: Expression<'_>) -> Option<(String, bool)> {
        match expression {
            Expression::Identifier(id) => {
                let symbol = *self.references.get(&id.span)?;
                if self.ambient.contains(&symbol) {
                    return None;
                }
                match self.imports.get(&symbol) {
                    Some(ImportOrigin::Named { name, react }) => Some((name.clone(), *react)),
                    Some(ImportOrigin::ReactNamespace) => None,
                    None => Some((
                        self.interner
                            .resolve(self.semantic.model.symbols[symbol as usize].name)
                            .to_owned(),
                        false,
                    )),
                }
            }
            Expression::Member(member) => {
                let Expression::Identifier(id) = member.object else {
                    return None;
                };
                let symbol = self.references.get(&id.span)?;
                if !matches!(self.imports.get(symbol), Some(ImportOrigin::ReactNamespace)) {
                    return None;
                }
                let name = match member.property {
                    MemberProperty::Ident(id) => self.interner.resolve(id.name),
                    MemberProperty::Computed(Expression::StringLiteral(literal)) => {
                        self.interner.resolve_js(literal.value).as_str()?.to_owned()
                    }
                    _ => return None,
                };
                Some((name, true))
            }
            Expression::Sequence(sequence) if sequence.expressions.len() == 1 => {
                self.callee(sequence.expressions[0])
            }
            _ => None,
        }
    }

    fn owner(&self, id: Ident) -> bool {
        self.bindings.contains_key(&id.span) && self.interner.with_resolved(id.name, component_name)
    }
}

#[derive(Default)]
struct FunctionInfo {
    allowed: bool,
    asynchronous: bool,
    generator: bool,
}

struct Sites<'s> {
    identities: Identities<'s>,
    original_functions: FxHashSet<Span>,
    inferred: FxHashMap<Span, bool>,
    wrapped: FxHashSet<Span>,
    class_methods: FxHashSet<Span>,
    functions: FxHashMap<Span, FunctionInfo>,
    calls: FxHashMap<Span, (String, bool)>,
}

impl Sites<'_> {
    fn variables(&mut self, declaration: &VariableDeclaration<'_>) {
        for variable in &declaration.declarations {
            if let (Pattern::Ident(id), Some(init)) = (variable.id, variable.init) {
                self.infer(init, self.identities.owner(*id));
            }
        }
    }
    fn infer(&mut self, expression: Expression<'_>, allowed: bool) {
        if matches!(expression, Expression::Function(_) | Expression::Arrow(_)) {
            self.inferred.insert(expression.span(), allowed);
        }
    }
}

impl<'a> Visit<'a> for Sites<'_> {
    fn visit_statement(&mut self, statement: &Statement<'a>) {
        match statement {
            Statement::VariableDeclaration(declaration) => {
                self.variables(declaration);
            }
            Statement::For(loop_) => {
                if let Some(ForInit::Variable(declaration)) = loop_.init {
                    self.variables(declaration);
                }
            }
            Statement::ExportDefault(export) => match export.declaration {
                ExportDefaultKind::Function(function) => {
                    self.inferred.insert(function.span, true);
                }
                ExportDefaultKind::Expression(expression) => self.infer(expression, true),
                _ => {}
            },
            _ => {}
        }
        walk_statement(self, statement);
    }

    fn visit_function(&mut self, function: &Function<'a>) {
        let allowed = self.wrapped.contains(&function.span)
            || function
                .id
                .map(|id| self.identities.owner(id))
                .unwrap_or_else(|| self.inferred.get(&function.span).copied().unwrap_or(false));
        self.functions.insert(
            function.span,
            FunctionInfo {
                allowed: allowed
                    && self.original_functions.contains(&function.span)
                    && !self.class_methods.contains(&function.span),
                asynchronous: function.is_async,
                generator: function.is_generator,
            },
        );
        walk_function(self, function);
    }

    fn visit_class(&mut self, class: &Class<'a>) {
        for member in &class.body {
            if let ClassMember::Method(method) = member {
                self.class_methods.insert(method.value.span);
            }
        }
        walk_class(self, class);
    }

    fn visit_expression(&mut self, expression: &Expression<'a>) {
        match expression {
            Expression::Assignment(assignment) => {
                if let Expression::Identifier(id) = assignment.left {
                    let allowed = self
                        .identities
                        .references
                        .get(&id.span)
                        .is_some_and(|symbol| {
                            self.identities.interner.with_resolved(
                                self.identities.semantic.model.symbols[*symbol as usize].name,
                                component_name,
                            )
                        });
                    self.infer(assignment.right, allowed);
                }
            }
            Expression::Arrow(arrow) => {
                self.functions.insert(
                    arrow.span,
                    FunctionInfo {
                        allowed: self.original_functions.contains(&arrow.span)
                            && self.inferred.get(&arrow.span).copied().unwrap_or(false),
                        asynchronous: arrow.is_async,
                        generator: false,
                    },
                );
            }
            Expression::Call(call) => {
                if let Some((name, react)) = self.identities.callee(call.callee) {
                    if react && matches!(name.as_str(), "memo" | "forwardRef") {
                        if let Some(argument) = call.arguments.first() {
                            self.infer(*argument, true);
                            self.wrapped.insert(argument.span());
                        }
                    } else if hook_name(&name) || react && name == "use" {
                        self.calls
                            .insert(call.span, (name.clone(), react && name == "use"));
                    }
                }
            }
            _ => {}
        }
        walk_expression(self, expression);
    }
}

pub(crate) fn check(
    program: &Program<'_>,
    parsed: &SourceParseOutput,
    interner: &Interner,
    semantic: &SourceSemanticModel,
    facts: &CallExecutionFacts,
    level: RuleLevel,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let mut sites = Sites {
        identities: Identities::new(parsed, interner, semantic),
        original_functions: parsed
            .functions
            .iter()
            .map(|function| function.span)
            .collect(),
        inferred: FxHashMap::default(),
        wrapped: FxHashSet::default(),
        class_methods: FxHashSet::default(),
        functions: FxHashMap::default(),
        calls: FxHashMap::default(),
    };
    sites.visit_program(program);
    for call in &facts.calls {
        let Some((name, special_use)) = sites.calls.get(&call.span) else {
            continue;
        };
        if !call.reachable {
            continue;
        }
        let function = sites.functions.get(&call.region);
        let (id, reason) = if call.in_class || !function.is_some_and(|function| function.allowed) {
            (
                "context",
                "must be called in a React component or custom Hook",
            )
        } else if function.is_some_and(|function| function.asynchronous) {
            ("async", "cannot be called in an async function")
        } else if function.is_some_and(|function| function.generator) {
            ("generator", "cannot be called in a generator function")
        } else if call.in_parameters {
            (
                "parameters",
                "must be called in the function body, outside parameter defaults",
            )
        } else if call.inside_exception {
            (
                "exception",
                "cannot be called inside try, catch, or finally",
            )
        } else if !special_use && (call.inside_loop || call.may_repeat) {
            ("loop", "cannot be called in a loop")
        } else if !special_use && call.on_normal_path && !call.unconditional {
            (
                "conditional",
                "must be called on every normal path through the function",
            )
        } else {
            continue;
        };
        diagnostics.push(LintDiagnostic {
            rule_id: RULE.into(),
            level,
            message_id: id.into(),
            message: format!("Hook {name} {reason}."),
            start: call.span.lo,
            end: call.span.hi,
            fix: None,
        });
    }
}
