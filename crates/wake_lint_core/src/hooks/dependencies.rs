//! Captures and dependency paths retain original symbols; React stability is a closed policy.
use super::Identities;
use crate::{EffectiveRule, LintDiagnostic, LintError, RuleLevel};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use wake_common::{Atom, FxHashMap, FxHashSet, Interner, Span};
use wake_ecma_ast::*;
use wake_ecma_parser::SourceParseOutput;
use wake_ecma_semantic::{ReferenceAccess, SourceSemanticModel, SymbolId};

pub(crate) const RULE: &str = "react-hooks/exhaustive-deps";
const MAX_WORK: usize = 10_000_000;

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dependency_work_limits_return_analysis_errors_without_results() {
        let source = "import {useEffect} from 'react'; function App(p) { useEffect(() => p, []); }";
        let interner = Interner::new();
        let parsed = wake_ecma_parser::parse_source(
            source,
            &interner,
            crate::SourceType::Module,
            Default::default(),
        );
        let options = crate::LintOptions {
            recommended: false,
            rules: [(RULE.into(), RuleLevel::Error.into())].into(),
            ..Default::default()
        };
        let configuration = crate::effective_configuration(&options).unwrap();
        parsed.parsed.module.with_ast(|program| {
            let semantic = wake_ecma_semantic::analyze_source(
                program,
                &interner,
                wake_ecma_semantic::SourceSemanticInput {
                    identifiers: &parsed.identifiers,
                    exports: &parsed.exports,
                    syntax: &parsed.syntax,
                    functions: &parsed.functions,
                    namespaces: &parsed.namespaces,
                },
            );
            let mut diagnostics = Vec::new();
            let mut work = Work {
                remaining: 1,
                exhausted: false,
            };
            let error = check_with_work(
                program,
                &parsed,
                &interner,
                &semantic,
                &configuration[RULE],
                &mut diagnostics,
                &mut work,
            )
            .unwrap_err();
            assert!(matches!(error, LintError::Analysis(_)));
            assert!(diagnostics.is_empty());
        });
    }
}

fn contains(outer: Span, inner: Span) -> bool {
    outer.lo <= inner.lo && inner.hi <= outer.hi
}

struct Work {
    remaining: usize,
    exhausted: bool,
}
impl Work {
    fn step(&mut self) -> bool {
        if self.remaining == 0 {
            self.exhausted = true;
            false
        } else {
            self.remaining -= 1;
            true
        }
    }
    fn result(&self) -> Result<(), LintError> {
        if self.exhausted {
            Err(LintError::Analysis(
                "Hook dependency analysis exceeded the work limit".into(),
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy)]
struct SourceReference {
    name: Atom,
    symbol: Option<SymbolId>,
    access: ReferenceAccess,
}
struct Facts<'s> {
    identities: Identities<'s>,
    references: FxHashMap<Span, SourceReference>,
    writes: FxHashSet<SymbolId>,
}
impl<'s> Facts<'s> {
    fn new(
        parsed: &SourceParseOutput,
        interner: &'s Interner,
        semantic: &'s SourceSemanticModel,
    ) -> Self {
        let references = semantic
            .references
            .iter()
            .map(|&index| {
                let reference = &semantic.model.references[index];
                (
                    reference.span,
                    SourceReference {
                        name: reference.name,
                        symbol: reference.resolved,
                        access: reference.access,
                    },
                )
            })
            .collect();
        let writes = semantic
            .references
            .iter()
            .filter_map(|&index| {
                let reference = &semantic.model.references[index];
                reference
                    .access
                    .is_write()
                    .then_some(reference.resolved)
                    .flatten()
            })
            .collect();
        Self {
            identities: Identities::new(parsed, interner, semantic),
            references,
            writes,
        }
    }
    fn binding(&self, id: Ident) -> Option<SymbolId> {
        self.identities.bindings.get(&id.span).copied()
    }
    fn reactive(&self, symbol: SymbolId, owner: Span, callback: Span) -> bool {
        let declaration = self.identities.semantic.model.symbols[symbol as usize].span;
        contains(owner, declaration) && !contains(callback, declaration)
    }
    fn path(&self, expression: Expression<'_>) -> Option<Dependency> {
        match expression {
            Expression::Identifier(id) => {
                let reference = self.references.get(&id.span)?;
                if reference.symbol.is_none()
                    && self
                        .identities
                        .semantic
                        .incomplete_value_names
                        .contains(&reference.name)
                {
                    return None;
                }
                let root = match self.identities.references.get(&id.span) {
                    Some(&symbol) => Root::Symbol(symbol),
                    None if reference.symbol.is_none() => {
                        Root::External(self.identities.interner.resolve(id.name))
                    }
                    _ => return None,
                };
                Some(Dependency {
                    root,
                    path: Vec::new(),
                })
            }
            Expression::Member(member) => {
                let name = match member.property {
                    MemberProperty::Ident(id) => self.identities.interner.resolve(id.name),
                    MemberProperty::Computed(Expression::StringLiteral(literal)) => self
                        .identities
                        .interner
                        .resolve_js(literal.value)
                        .as_str()?
                        .to_owned(),
                    _ => return None,
                };
                let mut dependency = self.path(member.object)?;
                dependency.path.push(name);
                Some(dependency)
            }
            Expression::Sequence(sequence) if sequence.expressions.len() == 1 => {
                self.path(sequence.expressions[0])
            }
            _ => None,
        }
    }
    fn display(&self, dependency: &Dependency) -> String {
        let mut name = match &dependency.root {
            Root::Symbol(symbol) => self
                .identities
                .interner
                .resolve(self.identities.semantic.model.symbols[*symbol as usize].name),
            Root::External(name) => name.clone(),
        };
        for part in &dependency.path {
            name.push('.');
            name.push_str(part);
        }
        name
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Root {
    Symbol(SymbolId),
    External(String),
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Dependency {
    root: Root,
    path: Vec<String>,
}
impl Dependency {
    fn covers(&self, other: &Self) -> bool {
        self.root == other.root && other.path.starts_with(&self.path)
    }
}

#[derive(Clone, Copy)]
enum Callable<'a> {
    Function(&'a Function<'a>),
    Arrow(&'a ArrowFunction<'a>),
}
impl<'a> Callable<'a> {
    fn from(expression: Expression<'a>) -> Option<Self> {
        match expression {
            Expression::Function(value) => Some(Self::Function(value)),
            Expression::Arrow(value) => Some(Self::Arrow(value)),
            _ => None,
        }
    }
    fn span(self) -> Span {
        match self {
            Self::Function(value) => value.span,
            Self::Arrow(value) => value.span,
        }
    }
    fn asynchronous(self) -> bool {
        match self {
            Self::Function(value) => value.is_async,
            Self::Arrow(value) => value.is_async,
        }
    }
    fn visit(self, visitor: &mut impl Visit<'a>) {
        match self {
            Self::Function(value) => visitor.visit_function(value),
            Self::Arrow(value) => visitor.visit_expression(&Expression::Arrow(value)),
        }
    }
}
#[derive(Clone, Copy)]
struct FunctionValue<'a> {
    callback: Callable<'a>,
    owner: Span,
}
struct EffectCall<'a> {
    call: &'a CallExpression<'a>,
    callback_index: usize,
    effect: bool,
    owner: Span,
}
#[derive(Default)]
struct Collection<'a> {
    functions: FxHashMap<SymbolId, FunctionValue<'a>>,
    ambiguous: FxHashSet<SymbolId>,
    stable: FxHashSet<SymbolId>,
    refs: FxHashSet<SymbolId>,
    calls: Vec<EffectCall<'a>>,
}
struct Collector<'a, 's, 'f> {
    facts: &'f Facts<'s>,
    custom: Option<regex::Regex>,
    owners: Vec<Span>,
    original: FxHashSet<Span>,
    data: Collection<'a>,
    work: &'f mut Work,
}
impl<'a> Collector<'a, '_, '_> {
    fn function_value(&mut self, id: Ident, callback: Callable<'a>) {
        let Some(symbol) = self.facts.binding(id) else {
            return;
        };
        if self.facts.writes.contains(&symbol) {
            return;
        }
        let value = FunctionValue {
            callback,
            owner: *self.owners.last().unwrap(),
        };
        if self.data.functions.insert(symbol, value).is_some() {
            self.data.ambiguous.insert(symbol);
        }
    }
    fn variables(&mut self, declaration: &VariableDeclaration<'a>) {
        for variable in &declaration.declarations {
            let Some(init) = variable.init else {
                continue;
            };
            if let Pattern::Ident(id) = variable.id {
                if let Some(callback) = Callable::from(init) {
                    self.function_value(*id, callback);
                }
                if declaration.kind == VarKind::Const
                    && literal(init)
                    && let Some(symbol) = self.facts.binding(*id)
                    && !self.facts.writes.contains(&symbol)
                {
                    self.data.stable.insert(symbol);
                }
            }
            let Expression::Call(call) = init else {
                continue;
            };
            let Some((name, true)) = self.facts.identities.callee(call.callee) else {
                continue;
            };
            let id = match (name.as_str(), variable.id) {
                ("useRef", Pattern::Ident(id)) => Some(*id),
                ("useState" | "useReducer" | "useTransition", Pattern::Array(array)) => {
                    match array.elements.get(1) {
                        Some(Some(Pattern::Ident(id))) => Some(**id),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some(symbol) = id.and_then(|id| self.facts.binding(id))
                && !self.facts.writes.contains(&symbol)
            {
                self.data.stable.insert(symbol);
                if name == "useRef" {
                    self.data.refs.insert(symbol);
                }
            }
        }
    }
}
fn literal(expression: Expression<'_>) -> bool {
    match expression {
        Expression::NumberLiteral(_)
        | Expression::StringLiteral(_)
        | Expression::BooleanLiteral(_)
        | Expression::NullLiteral(_)
        | Expression::BigIntLiteral(_) => true,
        Expression::Unary(unary) => literal(unary.argument),
        _ => false,
    }
}
impl<'a> Visit<'a> for Collector<'a, '_, '_> {
    fn visit_statement(&mut self, statement: &Statement<'a>) {
        if !self.work.step() {
            return;
        }
        match statement {
            Statement::FunctionDeclaration(function) => {
                if let Some(id) = function.id {
                    self.function_value(id, Callable::Function(function));
                }
            }
            Statement::ExportDefault(export) => {
                if let ExportDefaultKind::Function(function) = export.declaration
                    && let Some(id) = function.id
                {
                    self.function_value(id, Callable::Function(function));
                }
            }
            Statement::VariableDeclaration(declaration) => self.variables(declaration),
            Statement::For(loop_) => {
                if let Some(ForInit::Variable(declaration)) = loop_.init {
                    self.variables(declaration);
                }
            }
            Statement::ForIn(loop_) => {
                if let ForLeft::Variable(declaration) = loop_.left {
                    self.variables(declaration);
                }
            }
            Statement::ForOf(loop_) => {
                if let ForLeft::Variable(declaration) = loop_.left {
                    self.variables(declaration);
                }
            }
            _ => {}
        }
        walk_statement(self, statement);
    }
    fn visit_function(&mut self, function: &Function<'a>) {
        if !self.work.step() {
            return;
        }
        if self.original.contains(&function.span) {
            self.owners.push(function.span);
            walk_function(self, function);
            self.owners.pop();
        } else {
            walk_function(self, function);
        }
    }
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        if !self.work.step() {
            return;
        }
        if let Expression::Function(function) = expression
            && let Some(id) = function.id
        {
            self.function_value(id, Callable::Function(function));
        }
        if let Expression::Arrow(arrow) = expression {
            self.owners.push(arrow.span);
            walk_expression(self, expression);
            self.owners.pop();
            return;
        }
        if let Expression::Call(call) = expression
            && let Some((name, react)) = self.facts.identities.callee(call.callee)
        {
            let kind = match (react, name.as_str()) {
                (true, "useEffect" | "useLayoutEffect" | "useInsertionEffect") => Some((0, true)),
                (true, "useMemo" | "useCallback") => Some((0, false)),
                (true, "useImperativeHandle") => Some((1, true)),
                _ if self
                    .custom
                    .as_ref()
                    .is_some_and(|regex| regex.is_match(&name)) =>
                {
                    Some((0, true))
                }
                _ => None,
            };
            if let Some((callback_index, effect)) = kind {
                self.data.calls.push(EffectCall {
                    call,
                    callback_index,
                    effect,
                    owner: *self.owners.last().unwrap(),
                });
            }
        }
        walk_expression(self, expression);
    }
}

#[derive(Default)]
struct Captures {
    dependencies: BTreeMap<Dependency, Span>,
    writes: BTreeMap<SymbolId, Span>,
    unavailable: bool,
}
struct CaptureVisitor<'s, 'f> {
    facts: &'f Facts<'s>,
    owner: Span,
    callback: Span,
    captures: Captures,
    work: &'f mut Work,
}
impl CaptureVisitor<'_, '_> {
    fn path(&mut self, expression: Expression<'_>) -> bool {
        let Some(dependency) = self.facts.path(expression) else {
            return false;
        };
        if let Root::Symbol(symbol) = dependency.root
            && self.facts.reactive(symbol, self.owner, self.callback)
        {
            self.captures
                .dependencies
                .entry(dependency)
                .or_insert(expression.span());
        }
        true
    }
    fn receiver<'a>(&mut self, member: &MemberExpression<'a>) {
        self.visit_expression(&member.object);
        if let MemberProperty::Computed(expression) = member.property {
            self.visit_expression(&expression);
        }
    }
}
impl<'a> Visit<'a> for CaptureVisitor<'_, '_> {
    fn visit_statement(&mut self, statement: &Statement<'a>) {
        if !self.work.step() {
            return;
        }
        if matches!(statement, Statement::With(_)) {
            self.captures.unavailable = true;
        }
        walk_statement(self, statement);
    }
    fn visit_expression(&mut self, expression: &Expression<'a>) {
        if !self.work.step() {
            return;
        }
        match expression {
            Expression::Member(_) if self.path(*expression) => return,
            Expression::Call(call) => {
                if let Expression::Identifier(id) = call.callee
                    && self
                        .facts
                        .references
                        .get(&id.span)
                        .is_some_and(|r| r.symbol.is_none())
                    && self
                        .facts
                        .identities
                        .interner
                        .with_resolved(id.name, |name| name == "eval")
                {
                    self.captures.unavailable = true;
                }
                if let Expression::Member(member) = call.callee {
                    self.receiver(member);
                    for argument in &call.arguments {
                        self.visit_expression(argument);
                    }
                    return;
                }
            }
            Expression::Assignment(assignment) => {
                if let Expression::Member(member) = assignment.left {
                    self.receiver(member);
                    self.visit_expression(&assignment.right);
                    return;
                }
            }
            Expression::Update(update) => {
                if let Expression::Member(member) = update.argument {
                    self.receiver(member);
                    return;
                }
            }
            _ => {}
        }
        walk_expression(self, expression);
    }
    fn visit_ident(&mut self, id: &Ident) {
        if !self.work.step() {
            return;
        }
        let Some(reference) = self.facts.references.get(&id.span) else {
            return;
        };
        if reference.symbol.is_none()
            && self
                .facts
                .identities
                .semantic
                .incomplete_value_names
                .contains(&reference.name)
        {
            self.captures.unavailable = true;
            return;
        }
        if let Some(symbol) = reference.symbol
            && self.facts.reactive(symbol, self.owner, self.callback)
        {
            if reference.access.is_write() {
                self.captures.writes.entry(symbol).or_insert(id.span);
            }
            if reference.access.is_read() {
                self.path(Expression::Identifier(id));
            }
        }
    }
}
fn capture(facts: &Facts<'_>, callback: Callable<'_>, owner: Span, work: &mut Work) -> Captures {
    let mut visitor = CaptureVisitor {
        facts,
        owner,
        callback: callback.span(),
        captures: Captures::default(),
        work,
    };
    callback.visit(&mut visitor);
    visitor.captures
}

fn stable_functions(
    facts: &Facts<'_>,
    data: &Collection<'_>,
    work: &mut Work,
) -> FxHashSet<SymbolId> {
    let mut stable = data.stable.clone();
    let mut invalid = FxHashSet::default();
    let mut dependents: FxHashMap<SymbolId, Vec<SymbolId>> = FxHashMap::default();
    for (&symbol, value) in &data.functions {
        if !work.step() {
            break;
        }
        let captures = capture(facts, value.callback, value.owner, work);
        if captures.unavailable || !captures.writes.is_empty() || data.ambiguous.contains(&symbol) {
            invalid.insert(symbol);
        }
        for dependency in captures.dependencies.keys() {
            if !work.step() {
                break;
            }
            let Root::Symbol(dependency) = dependency.root else {
                continue;
            };
            if stable.contains(&dependency) {
                continue;
            }
            if data.functions.contains_key(&dependency) {
                dependents.entry(dependency).or_default().push(symbol);
            } else {
                invalid.insert(symbol);
            }
        }
    }
    let mut queue: VecDeque<_> = invalid.iter().copied().collect();
    while let Some(symbol) = queue.pop_front() {
        if !work.step() {
            break;
        }
        for &dependent in dependents.get(&symbol).into_iter().flatten() {
            if !work.step() {
                break;
            }
            if invalid.insert(dependent) {
                queue.push_back(dependent);
            }
        }
    }
    stable.extend(
        data.functions
            .keys()
            .filter(|symbol| !invalid.contains(symbol))
            .copied(),
    );
    stable
}

struct Reporter<'d> {
    level: RuleLevel,
    diagnostics: &'d mut Vec<LintDiagnostic>,
}
impl Reporter<'_> {
    fn report(&mut self, span: Span, id: &str, message: impl Into<String>) {
        self.diagnostics.push(LintDiagnostic {
            rule_id: RULE.into(),
            level: self.level,
            message_id: id.into(),
            message: message.into(),
            start: span.lo,
            end: span.hi,
            fix: None,
        });
    }
}

pub(crate) fn check(
    program: &Program<'_>,
    parsed: &SourceParseOutput,
    interner: &Interner,
    semantic: &SourceSemanticModel,
    configuration: &EffectiveRule,
    diagnostics: &mut Vec<LintDiagnostic>,
) -> Result<(), LintError> {
    let mut work = Work {
        remaining: MAX_WORK,
        exhausted: false,
    };
    check_with_work(
        program,
        parsed,
        interner,
        semantic,
        configuration,
        diagnostics,
        &mut work,
    )
}

fn check_with_work(
    program: &Program<'_>,
    parsed: &SourceParseOutput,
    interner: &Interner,
    semantic: &SourceSemanticModel,
    configuration: &EffectiveRule,
    diagnostics: &mut Vec<LintDiagnostic>,
    work: &mut Work,
) -> Result<(), LintError> {
    let facts = Facts::new(parsed, interner, semantic);
    let pattern = configuration.configuration.options["additional_effect_hooks"]
        .as_str()
        .unwrap();
    let custom =
        (!pattern.is_empty()).then(|| regex::Regex::new(pattern).expect("validated regex"));
    let mut collector = Collector {
        facts: &facts,
        custom,
        owners: vec![program.span],
        original: parsed
            .functions
            .iter()
            .map(|function| function.span)
            .collect(),
        data: Collection::default(),
        work,
    };
    collector.visit_program(program);
    let data = collector.data;
    work.result()?;
    if data.calls.is_empty() {
        return Ok(());
    }
    let stable = stable_functions(&facts, &data, work);
    work.result()?;
    let mut reporter = Reporter {
        level: configuration.configuration.level,
        diagnostics,
    };
    for site in &data.calls {
        if !work.step() {
            break;
        }
        let callback_expression = site.call.arguments.get(site.callback_index).copied();
        let dependency_expression = site.call.arguments.get(site.callback_index + 1).copied();
        let callback = callback_expression.and_then(|expression| {
            Callable::from(expression).or_else(|| {
                let Expression::Identifier(id) = expression else {
                    return None;
                };
                let symbol = facts.identities.references.get(&id.span)?;
                if facts.writes.contains(symbol) || data.ambiguous.contains(symbol) {
                    return None;
                }
                data.functions.get(symbol).map(|value| value.callback)
            })
        });
        let Some(callback) = callback else {
            reporter.report(callback_expression.map_or(site.call.span, |value| value.span()), "unknown-callback", "Hook callback captures cannot be determined; use an inline or unchanged local function.");
            continue;
        };
        if site.effect && callback.asynchronous() {
            reporter.report(callback.span(), "async", "Effect callbacks must be synchronous; start asynchronous work inside the callback.");
            continue;
        }
        let captures = capture(&facts, callback, site.owner, work);
        if captures.unavailable {
            reporter.report(callback.span(), "unavailable", "Hook captures are unavailable in an incomplete or dynamically resolved value scope.");
            continue;
        }
        for (&symbol, &span) in &captures.writes {
            if !work.step() {
                break;
            }
            let name = facts
                .identities
                .interner
                .resolve(facts.identities.semantic.model.symbols[symbol as usize].name);
            reporter.report(span, "stale-write", format!("Assignment to reactive binding '{name}' inside the callback is lost on a later render."));
        }
        let Some(dependency_expression) = dependency_expression else {
            if !site.effect {
                reporter.report(
                    site.call.span,
                    "missing-array",
                    "Memo and callback Hooks require an explicit dependency array.",
                );
            }
            continue;
        };
        let Expression::Array(array) = dependency_expression else {
            reporter.report(
                dependency_expression.span(),
                "dynamic",
                "Hook dependencies must be an array literal.",
            );
            continue;
        };
        let callback_dependency = callback_expression.and_then(|expression| facts.path(expression));
        let mut declared = BTreeSet::new();
        let mut dynamic = false;
        for element in &array.elements {
            if !work.step() {
                break;
            }
            let Some(dependency) = element.and_then(|expression| facts.path(expression)) else {
                reporter.report(element.map_or(array.span, |value| value.span()), "dynamic", "Each dependency must be a static identifier or property path; holes and spreads are not analyzable.");
                dynamic = true;
                continue;
            };
            let span = element.unwrap().span();
            let name = facts.display(&dependency);
            if !declared.insert(dependency.clone()) {
                reporter.report(span, "duplicate", format!("Duplicate dependency '{name}'."));
                continue;
            }
            if matches!(dependency.root, Root::Symbol(symbol) if data.refs.contains(&symbol))
                && dependency
                    .path
                    .first()
                    .is_some_and(|part| part == "current")
            {
                reporter.report(
                    span,
                    "mutable",
                    format!("Mutable dependency '{name}' does not schedule React updates."),
                );
                continue;
            }
            let Root::Symbol(symbol) = dependency.root else {
                reporter.report(
                    span,
                    "external",
                    format!("External dependency '{name}' does not schedule React updates."),
                );
                continue;
            };
            if !facts.reactive(symbol, site.owner, callback.span()) {
                reporter.report(
                    span,
                    "external",
                    format!("External dependency '{name}' does not schedule React updates."),
                );
                continue;
            }
            if !site.effect
                && callback_dependency.as_ref() != Some(&dependency)
                && !stable.contains(&symbol)
                && !captures
                    .dependencies
                    .keys()
                    .any(|used| work.step() && dependency.covers(used))
            {
                reporter.report(
                    span,
                    "unnecessary",
                    format!(
                        "Unused dependency '{name}' is unnecessary for this memoized callback."
                    ),
                );
            }
        }
        if dynamic {
            continue;
        }
        if callback_dependency
            .as_ref()
            .is_some_and(|dependency| declared.contains(dependency))
        {
            continue;
        }
        for dependency in captures.dependencies.keys() {
            if !work.step() {
                break;
            }
            let Root::Symbol(symbol) = dependency.root else {
                continue;
            };
            if stable.contains(&symbol) || captures.writes.contains_key(&symbol) {
                continue;
            }
            let captured_parent = (0..dependency.path.len()).any(|length| {
                work.step()
                    && captures.dependencies.contains_key(&Dependency {
                        root: dependency.root.clone(),
                        path: dependency.path[..length].to_vec(),
                    })
            });
            if captured_parent {
                continue;
            }
            let covered = (0..=dependency.path.len()).any(|length| {
                work.step()
                    && declared.contains(&Dependency {
                        root: dependency.root.clone(),
                        path: dependency.path[..length].to_vec(),
                    })
            });
            if !covered {
                reporter.report(
                    array.span,
                    "missing",
                    format!("Missing dependency '{}'.", facts.display(dependency)),
                );
            }
        }
    }
    work.result()
}
