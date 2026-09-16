//! Annex B candidates in one var environment. Lexical blockers come from grammar nodes;
//! functions and classes are independent environments and are never traversed here.
use wake_common::{Atom, FxHashMap};
use wake_ecma_ast::*;

pub(crate) fn candidates<'a>(
    statements: &[Statement<'a>],
    parameters: &[Pattern<'a>],
) -> Vec<Ident> {
    let mut collector = Collector {
        blockers: FxHashMap::default(),
        candidates: Vec::new(),
    };
    for parameter in parameters {
        collector.pattern(parameter, true);
    }
    collector.names(statements, false, true);
    for statement in statements {
        collector.nested(statement);
    }
    collector.candidates
}

struct Collector {
    blockers: FxHashMap<Atom, usize>,
    candidates: Vec<Ident>,
}
impl Collector {
    fn name(&mut self, name: Atom, add: bool) {
        if add {
            *self.blockers.entry(name).or_default() += 1;
        } else if let Some(count) = self.blockers.get_mut(&name) {
            *count -= 1;
        }
    }
    fn pattern(&mut self, pattern: &Pattern, add: bool) {
        match pattern {
            Pattern::Ident(id) => self.name(id.name, add),
            Pattern::Assignment(p) => self.pattern(&p.left, add),
            Pattern::Rest(p) => self.pattern(&p.argument, add),
            Pattern::Array(p) => {
                for p in p.elements.iter().flatten() {
                    self.pattern(p, add);
                }
            }
            Pattern::Object(p) => {
                for p in &p.properties {
                    self.pattern(&p.value, add);
                }
                if let Some(p) = p.rest {
                    self.pattern(&p.argument, add);
                }
            }
        }
    }
    fn names(&mut self, statements: &[Statement], functions: bool, add: bool) {
        for statement in statements {
            match statement {
                Statement::FunctionDeclaration(f) if functions => {
                    if let Some(id) = f.id {
                        self.name(id.name, add);
                    }
                }
                Statement::ClassDeclaration(c) if !functions => {
                    if let Some(id) = c.id {
                        self.name(id.name, add);
                    }
                }
                Statement::VariableDeclaration(v) if !functions && v.kind != VarKind::Var => {
                    for declaration in &v.declarations {
                        self.pattern(&declaration.id, add);
                    }
                }
                _ => {}
            }
        }
    }
    fn candidate(&mut self, function: &Function) {
        if !function.is_async
            && !function.is_generator
            && let Some(id) = function.id
            && self.blockers.get(&id.name).copied().unwrap_or_default() == 0
        {
            self.candidates.push(id);
        }
    }
    fn block(&mut self, statements: &[Statement]) {
        self.names(statements, false, true);
        for statement in statements {
            if let Statement::FunctionDeclaration(function) = statement {
                self.candidate(function);
            }
        }
        // Sibling function declarations share this block binding, but block functions in a
        // descendant may not introduce a var which crosses it. Duplicate direct functions are allowed.
        self.names(statements, true, true);
        for statement in statements {
            self.nested(statement);
        }
        self.names(statements, true, false);
        self.names(statements, false, false);
    }
    fn arm(&mut self, statement: &Statement) {
        if let Statement::FunctionDeclaration(function) = statement {
            self.candidate(function);
        } else {
            self.nested(statement);
        }
    }
    fn loop_body(&mut self, left: Option<&VariableDeclaration>, body: &Statement) {
        if let Some(left) = left {
            self.names(&[Statement::VariableDeclaration(left)], false, true);
        }
        self.nested(body);
        if let Some(left) = left {
            self.names(&[Statement::VariableDeclaration(left)], false, false);
        }
    }
    fn nested(&mut self, statement: &Statement) {
        match statement {
            Statement::Block(block) => self.block(&block.body),
            Statement::If(branch) => {
                self.arm(&branch.consequent);
                if let Some(alternate) = &branch.alternate {
                    self.arm(alternate);
                }
            }
            Statement::Switch(switch) => {
                let statements = switch
                    .cases
                    .iter()
                    .flat_map(|case| case.consequent.iter().copied())
                    .collect::<Vec<_>>();
                self.block(&statements);
            }
            Statement::For(s) => self.loop_body(
                match s.init {
                    Some(ForInit::Variable(v)) => Some(v),
                    _ => None,
                },
                &s.body,
            ),
            Statement::ForIn(s) => self.loop_body(
                match s.left {
                    ForLeft::Variable(v) => Some(v),
                    _ => None,
                },
                &s.body,
            ),
            Statement::ForOf(s) => self.loop_body(
                match s.left {
                    ForLeft::Variable(v) => Some(v),
                    _ => None,
                },
                &s.body,
            ),
            Statement::While(s) => self.nested(&s.body),
            Statement::DoWhile(s) => self.nested(&s.body),
            Statement::Labeled(s) => self.nested(&s.body),
            Statement::With(s) => self.nested(&s.body),
            Statement::Try(s) => {
                self.block(&s.block.body);
                if let Some(handler) = s.handler {
                    // Annex B's simple catch parameter permits var hoisting through that catch.
                    let blocker = handler
                        .param
                        .as_ref()
                        .filter(|p| !matches!(p, Pattern::Ident(_)));
                    if let Some(p) = blocker {
                        self.pattern(p, true);
                    }
                    self.block(&handler.body.body);
                    if let Some(p) = blocker {
                        self.pattern(p, false);
                    }
                }
                if let Some(finalizer) = s.finalizer {
                    self.block(&finalizer.body);
                }
            }
            _ => {}
        }
    }
}
