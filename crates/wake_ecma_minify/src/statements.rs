//! # Phase 3 — Statement-level optimization analyses
//!
//! These analyzers detect patterns that the codegen can exploit to merge or
//! simplify adjacent statements. They produce side-tables only – no AST mutation.

use wake_common::Span;
use wake_ecma_ast::*;

use crate::IfReturnCandidate;

// ── If-return analyzer ──────────────────────────────────────────────────────

/// Find `if (cond) return a; return b;` (and `if … else return …`) patterns.
///
/// Returns a candidate for every occurrence where:
///
/// 1. An `if` with no `else` whose consequent is a single return, followed
///    immediately by a return statement: `if (cond) return a; return b;`
/// 2. An `if` with an `else` where both branches are single returns:
///    `if (cond) return a; else return b;`
pub fn analyze_if_return(program: &Program) -> Vec<IfReturnCandidate> {
    let mut candidates = Vec::new();
    analyze_seq_if_return(&program.body, &mut candidates);
    candidates
}

fn analyze_seq_if_return(stmts: &[Statement], candidates: &mut Vec<IfReturnCandidate>) {
    // ── Pattern 1: if (cond) return a; (no else) + return b; ──
    for i in 0..stmts.len().saturating_sub(1) {
        let (first, second) = (&stmts[i], &stmts[i + 1]);
        if let Statement::If(if_stmt) = first {
            if if_stmt.alternate.is_none() {
                if let Some(cons_ret) = extract_single_return(&if_stmt.consequent) {
                    if let Statement::Return(next_ret) = second {
                        candidates.push(IfReturnCandidate {
                            cond_span: if_stmt.test.span(),
                            if_span: if_stmt.span,
                            return_span: cons_ret.span,
                            subsequent_return_span: next_ret.span,
                        });
                    }
                }
            }
        }
    }

    // ── Pattern 2: if (cond) return a; else return b; ──
    for stmt in stmts {
        if let Statement::If(if_stmt) = stmt {
            if let Some(cons_ret) = extract_single_return(&if_stmt.consequent) {
                if let Some(alt) = &if_stmt.alternate {
                    if let Some(alt_ret) = extract_single_return(alt) {
                        candidates.push(IfReturnCandidate {
                            cond_span: if_stmt.test.span(),
                            if_span: if_stmt.span,
                            return_span: cons_ret.span,
                            subsequent_return_span: alt_ret.span,
                        });
                    }
                }
            }
        }
    }

    // ── Recurse ──
    for stmt in stmts {
        recurse_if_return(stmt, candidates);
    }
}

/// If `stmt` is a return statement (or a block containing exactly one return),
/// return a reference to the inner `ReturnStatement`.
fn extract_single_return<'a>(stmt: &'a Statement<'a>) -> Option<&'a ReturnStatement<'a>> {
    match stmt {
        Statement::Return(r) => Some(r),
        Statement::Block(b) if b.body.len() == 1 => extract_single_return(&b.body[0]),

        _ => None,
    }
}

fn recurse_if_return(stmt: &Statement, candidates: &mut Vec<IfReturnCandidate>) {
    match stmt {
        Statement::Block(b) => analyze_seq_if_return(&b.body, candidates),
        Statement::If(s) => {
            analyze_seq_if_return(stmt_list_from_single(&s.consequent), candidates);
            if let Some(alt) = &s.alternate {
                analyze_seq_if_return(stmt_list_from_single(alt), candidates);
            }
        }
        Statement::For(s) => recurse_if_return(&s.body, candidates),
        Statement::ForIn(s) => recurse_if_return(&s.body, candidates),
        Statement::ForOf(s) => recurse_if_return(&s.body, candidates),
        Statement::While(s) => recurse_if_return(&s.body, candidates),
        Statement::DoWhile(s) => recurse_if_return(&s.body, candidates),
        Statement::Switch(s) => {
            for case in s.cases.iter() {
                analyze_seq_if_return(&case.consequent, candidates);
            }
        }
        Statement::Try(s) => {
            analyze_seq_if_return(&s.block.body, candidates);
            if let Some(h) = &s.handler {
                analyze_seq_if_return(&h.body.body, candidates);
            }
            if let Some(f) = &s.finalizer {
                analyze_seq_if_return(&f.body, candidates);
            }
        }
        Statement::Labeled(s) => recurse_if_return(&s.body, candidates),
        Statement::With(s) => recurse_if_return(&s.body, candidates),

        // Function / class bodies
        Statement::FunctionDeclaration(f) => {
            if let Some(body) = &f.body {
                analyze_seq_if_return(&body.statements, candidates);
            }
        }
        Statement::ClassDeclaration(c) => walk_class_for_if_return(c, candidates),
        Statement::VariableDeclaration(d) => {
            for decl in d.declarations.iter() {
                if let Some(init) = &decl.init {
                    walk_expr_for_if_return(init, candidates);
                }
            }
        }
        Statement::Return(s) => {
            if let Some(arg) = &s.argument {
                walk_expr_for_if_return(arg, candidates);
            }
        }
        Statement::Throw(s) => walk_expr_for_if_return(&s.argument, candidates),
        Statement::Expression(e) => walk_expr_for_if_return(&e.expression, candidates),

        Statement::ExportNamed(s) => {
            if let Some(d) = &s.declaration {
                recurse_if_return(d, candidates);
            }
        }
        Statement::ExportDefault(s) => match s.declaration {
            ExportDefaultKind::Function(f) => {
                if let Some(body) = &f.body {
                    analyze_seq_if_return(&body.statements, candidates);
                }
            }
            ExportDefaultKind::Class(c) => walk_class_for_if_return(c, candidates),
            ExportDefaultKind::Expression(e) => walk_expr_for_if_return(&e, candidates),
        },

        Statement::Empty(_) | Statement::Debugger(_)
        | Statement::Break(_) | Statement::Continue(_)
        | Statement::Import(_) | Statement::ExportAll(_) => {}
    }
}

fn walk_expr_for_if_return(expr: &Expression, candidates: &mut Vec<IfReturnCandidate>) {
    match expr {
        Expression::Function(f) => {
            if let Some(body) = &f.body {
                analyze_seq_if_return(&body.statements, candidates);
            }
        }
        Expression::Arrow(a) => {
            if let ArrowBody::Block(b) = &a.body {
                analyze_seq_if_return(&b.statements, candidates);
            }
        }
        Expression::Class(c) => walk_class_for_if_return(c, candidates),
        Expression::Call(c) => {
            walk_expr_for_if_return(&c.callee, candidates);
            for arg in c.arguments.iter() {
                walk_expr_for_if_return(arg, candidates);
            }
        }
        Expression::New(n) => {
            walk_expr_for_if_return(&n.callee, candidates);
            for arg in n.arguments.iter() {
                walk_expr_for_if_return(arg, candidates);
            }
        }
        Expression::Member(m) => walk_expr_for_if_return(&m.object, candidates),
        Expression::TaggedTemplate(t) => walk_expr_for_if_return(&t.tag, candidates),
        Expression::Array(a) => {
            for el in a.elements.iter().flatten() {
                walk_expr_for_if_return(el, candidates);
            }
        }
        Expression::Object(o) => {
            for member in o.properties.iter() {
                match member {
                    ObjectMember::Property(p) => walk_expr_for_if_return(&p.value, candidates),
                    ObjectMember::Spread(s) => walk_expr_for_if_return(&s.argument, candidates),
                }
            }
        }
        Expression::Unary(u) => walk_expr_for_if_return(&u.argument, candidates),
        Expression::Binary(b) => {
            walk_expr_for_if_return(&b.left, candidates);
            walk_expr_for_if_return(&b.right, candidates);
        }
        Expression::Logical(l) => {
            walk_expr_for_if_return(&l.left, candidates);
            walk_expr_for_if_return(&l.right, candidates);
        }
        Expression::Assignment(a) => {
            walk_expr_for_if_return(&a.left, candidates);
            walk_expr_for_if_return(&a.right, candidates);
        }
        Expression::Conditional(c) => {
            walk_expr_for_if_return(&c.test, candidates);
            walk_expr_for_if_return(&c.consequent, candidates);
            walk_expr_for_if_return(&c.alternate, candidates);
        }
        Expression::Sequence(s) => {
            for e in s.expressions.iter() {
                walk_expr_for_if_return(e, candidates);
            }
        }
        Expression::Spread(s) => walk_expr_for_if_return(&s.argument, candidates),
        Expression::Await(a) => walk_expr_for_if_return(&a.argument, candidates),
        Expression::Yield(y) => {
            if let Some(arg) = &y.argument {
                walk_expr_for_if_return(arg, candidates);
            }
        }
        Expression::Update(u) => walk_expr_for_if_return(&u.argument, candidates),
        _ => {}
    }
}

fn walk_class_for_if_return(c: &Class, candidates: &mut Vec<IfReturnCandidate>) {
    for member in c.body.iter() {
        match member {
            ClassMember::Method(m) => {
                if let Some(body) = &m.value.body {
                    analyze_seq_if_return(&body.statements, candidates);
                }
            }
            ClassMember::Property(p) => {
                if let Some(val) = &p.value {
                    walk_expr_for_if_return(val, candidates);
                }
            }
            ClassMember::StaticBlock(b) => {
                analyze_seq_if_return(&b.body, candidates);
            }
        }
    }
}

/// Treat a single statement as a slice of 0 or 1 elements for recursion.
fn stmt_list_from_single<'a>(stmt: &'a Statement<'a>) -> &'a [Statement<'a>] {
    std::slice::from_ref(stmt)
}

// ── Join-vars analyzer ──────────────────────────────────────────────────────

/// Find consecutive `VariableDeclaration` statements of the same kind that can
/// be merged: `var a = 1; var b = 2;` → `var a = 1, b = 2;`
pub fn analyze_join_vars(program: &Program) -> Vec<(Span, Span)> {
    let mut pairs = Vec::new();
    analyze_seq_join_vars(&program.body, &mut pairs);
    pairs
}

fn analyze_seq_join_vars(stmts: &[Statement], pairs: &mut Vec<(Span, Span)>) {
    for i in 0..stmts.len().saturating_sub(1) {
        match (&stmts[i], &stmts[i + 1]) {
            (Statement::VariableDeclaration(a), Statement::VariableDeclaration(b))
                if a.kind == b.kind =>
            {
                pairs.push((a.span, b.span));
            }
            _ => {}
        }
    }

    for stmt in stmts {
        recurse_join_vars(stmt, pairs);
    }
}

fn recurse_join_vars(stmt: &Statement, pairs: &mut Vec<(Span, Span)>) {
    match stmt {
        Statement::Block(b) => analyze_seq_join_vars(&b.body, pairs),
        Statement::If(s) => {
            analyze_seq_join_vars(std::slice::from_ref(&s.consequent), pairs);
            if let Some(alt) = &s.alternate {
                analyze_seq_join_vars(std::slice::from_ref(alt), pairs);
            }
        }
        Statement::For(s) => recurse_join_vars(&s.body, pairs),
        Statement::ForIn(s) => recurse_join_vars(&s.body, pairs),
        Statement::ForOf(s) => recurse_join_vars(&s.body, pairs),
        Statement::While(s) => recurse_join_vars(&s.body, pairs),
        Statement::DoWhile(s) => recurse_join_vars(&s.body, pairs),
        Statement::Switch(s) => {
            for case in s.cases.iter() {
                analyze_seq_join_vars(&case.consequent, pairs);
            }
        }
        Statement::Try(s) => {
            analyze_seq_join_vars(&s.block.body, pairs);
            if let Some(h) = &s.handler {
                analyze_seq_join_vars(&h.body.body, pairs);
            }
            if let Some(f) = &s.finalizer {
                analyze_seq_join_vars(&f.body, pairs);
            }
        }
        Statement::Labeled(s) => recurse_join_vars(&s.body, pairs),
        Statement::With(s) => recurse_join_vars(&s.body, pairs),
        Statement::FunctionDeclaration(f) => {
            if let Some(body) = &f.body {
                analyze_seq_join_vars(&body.statements, pairs);
            }
        }
        Statement::ClassDeclaration(c) => walk_class_for_join_vars(c, pairs),
        Statement::VariableDeclaration(d) => {
            for decl in d.declarations.iter() {
                if let Some(init) = &decl.init {
                    walk_expr_for_join_vars(init, pairs);
                }
            }
        }
        Statement::Return(s) => {
            if let Some(arg) = &s.argument {
                walk_expr_for_join_vars(arg, pairs);
            }
        }
        Statement::Throw(s) => walk_expr_for_join_vars(&s.argument, pairs),
        Statement::Expression(e) => walk_expr_for_join_vars(&e.expression, pairs),
        Statement::ExportNamed(s) => {
            if let Some(d) = &s.declaration {
                recurse_join_vars(d, pairs);
            }
        }
        Statement::ExportDefault(s) => match s.declaration {
            ExportDefaultKind::Function(f) => {
                if let Some(body) = &f.body {
                    analyze_seq_join_vars(&body.statements, pairs);
                }
            }
            ExportDefaultKind::Class(c) => walk_class_for_join_vars(c, pairs),
            ExportDefaultKind::Expression(e) => walk_expr_for_join_vars(&e, pairs),
        },
        _ => {}
    }
}

fn walk_expr_for_join_vars(expr: &Expression, pairs: &mut Vec<(Span, Span)>) {
    match expr {
        Expression::Function(f) => {
            if let Some(body) = &f.body {
                analyze_seq_join_vars(&body.statements, pairs);
            }
        }
        Expression::Arrow(a) => {
            if let ArrowBody::Block(b) = &a.body {
                analyze_seq_join_vars(&b.statements, pairs);
            }
        }
        Expression::Class(c) => walk_class_for_join_vars(c, pairs),
        Expression::Call(c) => {
            walk_expr_for_join_vars(&c.callee, pairs);
            for arg in c.arguments.iter() {
                walk_expr_for_join_vars(arg, pairs);
            }
        }
        Expression::New(n) => {
            walk_expr_for_join_vars(&n.callee, pairs);
            for arg in n.arguments.iter() {
                walk_expr_for_join_vars(arg, pairs);
            }
        }
        Expression::Member(m) => walk_expr_for_join_vars(&m.object, pairs),
        Expression::TaggedTemplate(t) => walk_expr_for_join_vars(&t.tag, pairs),
        Expression::Array(a) => {
            for el in a.elements.iter().flatten() {
                walk_expr_for_join_vars(el, pairs);
            }
        }
        Expression::Object(o) => {
            for member in o.properties.iter() {
                match member {
                    ObjectMember::Property(p) => walk_expr_for_join_vars(&p.value, pairs),
                    ObjectMember::Spread(s) => walk_expr_for_join_vars(&s.argument, pairs),
                }
            }
        }
        Expression::Unary(u) => walk_expr_for_join_vars(&u.argument, pairs),
        Expression::Binary(b) => {
            walk_expr_for_join_vars(&b.left, pairs);
            walk_expr_for_join_vars(&b.right, pairs);
        }
        Expression::Logical(l) => {
            walk_expr_for_join_vars(&l.left, pairs);
            walk_expr_for_join_vars(&l.right, pairs);
        }
        Expression::Assignment(a) => {
            walk_expr_for_join_vars(&a.left, pairs);
            walk_expr_for_join_vars(&a.right, pairs);
        }
        Expression::Conditional(c) => {
            walk_expr_for_join_vars(&c.test, pairs);
            walk_expr_for_join_vars(&c.consequent, pairs);
            walk_expr_for_join_vars(&c.alternate, pairs);
        }
        Expression::Sequence(s) => {
            for e in s.expressions.iter() {
                walk_expr_for_join_vars(e, pairs);
            }
        }
        Expression::Spread(s) => walk_expr_for_join_vars(&s.argument, pairs),
        Expression::Await(a) => walk_expr_for_join_vars(&a.argument, pairs),
        Expression::Yield(y) => {
            if let Some(arg) = &y.argument {
                walk_expr_for_join_vars(arg, pairs);
            }
        }
        Expression::Update(u) => walk_expr_for_join_vars(&u.argument, pairs),
        _ => {}
    }
}

fn walk_class_for_join_vars(c: &Class, pairs: &mut Vec<(Span, Span)>) {
    for member in c.body.iter() {
        match member {
            ClassMember::Method(m) => {
                if let Some(body) = &m.value.body {
                    analyze_seq_join_vars(&body.statements, pairs);
                }
            }
            ClassMember::Property(p) => {
                if let Some(val) = &p.value {
                    walk_expr_for_join_vars(val, pairs);
                }
            }
            ClassMember::StaticBlock(b) => {
                analyze_seq_join_vars(&b.body, pairs);
            }
        }
    }
}

// ── Sequences analyzer ──────────────────────────────────────────────────────

/// Find consecutive `ExpressionStatement` nodes that can be merged with the
/// comma operator: `a(); b();` → `a(), b();`
pub fn analyze_sequences(program: &Program) -> Vec<(Span, Span)> {
    let mut pairs = Vec::new();
    analyze_seq_sequences(&program.body, &mut pairs);
    pairs
}

fn analyze_seq_sequences(stmts: &[Statement], pairs: &mut Vec<(Span, Span)>) {
    for i in 0..stmts.len().saturating_sub(1) {
        match (&stmts[i], &stmts[i + 1]) {
            (Statement::Expression(a), Statement::Expression(b)) => {
                pairs.push((a.span, b.span));
            }
            _ => {}
        }
    }

    for stmt in stmts {
        recurse_sequences(stmt, pairs);
    }
}

fn recurse_sequences(stmt: &Statement, pairs: &mut Vec<(Span, Span)>) {
    match stmt {
        Statement::Block(b) => analyze_seq_sequences(&b.body, pairs),
        Statement::If(s) => {
            analyze_seq_sequences(std::slice::from_ref(&s.consequent), pairs);
            if let Some(alt) = &s.alternate {
                analyze_seq_sequences(std::slice::from_ref(alt), pairs);
            }
        }
        Statement::For(s) => recurse_sequences(&s.body, pairs),
        Statement::ForIn(s) => recurse_sequences(&s.body, pairs),
        Statement::ForOf(s) => recurse_sequences(&s.body, pairs),
        Statement::While(s) => recurse_sequences(&s.body, pairs),
        Statement::DoWhile(s) => recurse_sequences(&s.body, pairs),
        Statement::Switch(s) => {
            for case in s.cases.iter() {
                analyze_seq_sequences(&case.consequent, pairs);
            }
        }
        Statement::Try(s) => {
            analyze_seq_sequences(&s.block.body, pairs);
            if let Some(h) = &s.handler {
                analyze_seq_sequences(&h.body.body, pairs);
            }
            if let Some(f) = &s.finalizer {
                analyze_seq_sequences(&f.body, pairs);
            }
        }
        Statement::Labeled(s) => recurse_sequences(&s.body, pairs),
        Statement::With(s) => recurse_sequences(&s.body, pairs),
        Statement::FunctionDeclaration(f) => {
            if let Some(body) = &f.body {
                analyze_seq_sequences(&body.statements, pairs);
            }
        }
        Statement::ClassDeclaration(c) => walk_class_for_sequences(c, pairs),
        Statement::VariableDeclaration(d) => {
            for decl in d.declarations.iter() {
                if let Some(init) = &decl.init {
                    walk_expr_for_sequences(init, pairs);
                }
            }
        }
        Statement::Return(s) => {
            if let Some(arg) = &s.argument {
                walk_expr_for_sequences(arg, pairs);
            }
        }
        Statement::Throw(s) => walk_expr_for_sequences(&s.argument, pairs),
        Statement::Expression(e) => walk_expr_for_sequences(&e.expression, pairs),
        Statement::ExportNamed(s) => {
            if let Some(d) = &s.declaration {
                recurse_sequences(d, pairs);
            }
        }
        Statement::ExportDefault(s) => match s.declaration {
            ExportDefaultKind::Function(f) => {
                if let Some(body) = &f.body {
                    analyze_seq_sequences(&body.statements, pairs);
                }
            }
            ExportDefaultKind::Class(c) => walk_class_for_sequences(c, pairs),
            ExportDefaultKind::Expression(e) => walk_expr_for_sequences(&e, pairs),
        },
        _ => {}
    }
}

fn walk_expr_for_sequences(expr: &Expression, pairs: &mut Vec<(Span, Span)>) {
    match expr {
        Expression::Function(f) => {
            if let Some(body) = &f.body {
                analyze_seq_sequences(&body.statements, pairs);
            }
        }
        Expression::Arrow(a) => {
            if let ArrowBody::Block(b) = &a.body {
                analyze_seq_sequences(&b.statements, pairs);
            }
        }
        Expression::Class(c) => walk_class_for_sequences(c, pairs),
        Expression::Call(c) => {
            walk_expr_for_sequences(&c.callee, pairs);
            for arg in c.arguments.iter() {
                walk_expr_for_sequences(arg, pairs);
            }
        }
        Expression::New(n) => {
            walk_expr_for_sequences(&n.callee, pairs);
            for arg in n.arguments.iter() {
                walk_expr_for_sequences(arg, pairs);
            }
        }
        Expression::Member(m) => walk_expr_for_sequences(&m.object, pairs),
        Expression::TaggedTemplate(t) => walk_expr_for_sequences(&t.tag, pairs),
        Expression::Array(a) => {
            for el in a.elements.iter().flatten() {
                walk_expr_for_sequences(el, pairs);
            }
        }
        Expression::Object(o) => {
            for member in o.properties.iter() {
                match member {
                    ObjectMember::Property(p) => walk_expr_for_sequences(&p.value, pairs),
                    ObjectMember::Spread(s) => walk_expr_for_sequences(&s.argument, pairs),
                }
            }
        }
        Expression::Unary(u) => walk_expr_for_sequences(&u.argument, pairs),
        Expression::Binary(b) => {
            walk_expr_for_sequences(&b.left, pairs);
            walk_expr_for_sequences(&b.right, pairs);
        }
        Expression::Logical(l) => {
            walk_expr_for_sequences(&l.left, pairs);
            walk_expr_for_sequences(&l.right, pairs);
        }
        Expression::Assignment(a) => {
            walk_expr_for_sequences(&a.left, pairs);
            walk_expr_for_sequences(&a.right, pairs);
        }
        Expression::Conditional(c) => {
            walk_expr_for_sequences(&c.test, pairs);
            walk_expr_for_sequences(&c.consequent, pairs);
            walk_expr_for_sequences(&c.alternate, pairs);
        }
        Expression::Sequence(s) => {
            for e in s.expressions.iter() {
                walk_expr_for_sequences(e, pairs);
            }
        }
        Expression::Spread(s) => walk_expr_for_sequences(&s.argument, pairs),
        Expression::Await(a) => walk_expr_for_sequences(&a.argument, pairs),
        Expression::Yield(y) => {
            if let Some(arg) = &y.argument {
                walk_expr_for_sequences(arg, pairs);
            }
        }
        Expression::Update(u) => walk_expr_for_sequences(&u.argument, pairs),
        _ => {}
    }
}

fn walk_class_for_sequences(c: &Class, pairs: &mut Vec<(Span, Span)>) {
    for member in c.body.iter() {
        match member {
            ClassMember::Method(m) => {
                if let Some(body) = &m.value.body {
                    analyze_seq_sequences(&body.statements, pairs);
                }
            }
            ClassMember::Property(p) => {
                if let Some(val) = &p.value {
                    walk_expr_for_sequences(val, pairs);
                }
            }
            ClassMember::StaticBlock(b) => {
                analyze_seq_sequences(&b.body, pairs);
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use wake_common::Interner;
    use wake_ecma_ast::SourceType;
    use wake_ecma_parser::parse;

    // ── Helpers ──

    fn run_if_return(src: &str) -> Vec<IfReturnCandidate> {
        let it = Interner::new();
        let out = parse(src, &it, SourceType::Script);
        assert!(!out.has_errors(), "parse error: {:?}", out.diagnostics);
        out.module.with_ast(|p| analyze_if_return(p))
    }

    fn run_join_vars(src: &str) -> Vec<(Span, Span)> {
        let it = Interner::new();
        let out = parse(src, &it, SourceType::Script);
        assert!(!out.has_errors(), "parse error: {:?}", out.diagnostics);
        out.module.with_ast(|p| analyze_join_vars(p))
    }

    fn run_sequences(src: &str) -> Vec<(Span, Span)> {
        let it = Interner::new();
        let out = parse(src, &it, SourceType::Script);
        assert!(!out.has_errors(), "parse error: {:?}", out.diagnostics);
        out.module.with_ast(|p| analyze_sequences(p))
    }

    fn candidate_texts(src: &str, candidates: &[IfReturnCandidate]) -> Vec<String> {
        candidates
            .iter()
            .map(|c| {
                let cond = &src[c.cond_span.lo as usize..c.cond_span.hi as usize];
                let ret = &src[c.return_span.lo as usize..c.return_span.hi as usize];
                let sub = &src[c.subsequent_return_span.lo as usize..c.subsequent_return_span.hi as usize];
                format!("cond:{cond} ret:{ret} sub:{sub}")
            })
            .collect()
    }

    fn pair_texts(src: &str, pairs: &[(Span, Span)]) -> Vec<String> {
        pairs
            .iter()
            .map(|(a, b)| {
                let sa = &src[a.lo as usize..a.hi as usize];
                let sb = &src[b.lo as usize..b.hi as usize];
                format!("({sa}) + ({sb})")
            })
            .collect()
    }

    // ── If-return tests ──

    #[test]
    fn if_return_simple() {
        let src = "function f() { if (x) return a; return b; }";
        let candidates = run_if_return(src);
        assert_eq!(candidates.len(), 1, "should detect if-return pattern");
        let texts = candidate_texts(src, &candidates);
        assert!(texts.iter().any(|t| t.contains("cond:x") && t.contains("ret:return a") && t.contains("sub:return b")));
    }

    #[test]
    fn if_return_block_body() {
        let src = "function f() { if (x) { return a; } return b; }";
        let candidates = run_if_return(src);
        assert_eq!(candidates.len(), 1, "should detect if-return with block body");
    }

    #[test]
    fn if_return_with_else() {
        let src = "function f() { if (x) return a; else return b; }";
        let candidates = run_if_return(src);
        assert_eq!(candidates.len(), 1, "should detect if-else-return pattern");
        let texts = candidate_texts(src, &candidates);
        assert!(texts.iter().any(|t| t.contains("cond:x") && t.contains("ret:return a") && t.contains("sub:return b")));
    }

    #[test]
    fn if_return_block_else_block() {
        let src = "function f() { if (x) { return a; } else { return b; } }";
        let candidates = run_if_return(src);
        assert_eq!(candidates.len(), 1, "blocks wrapping returns should still be detected");
    }

    #[test]
    fn if_return_non_return_in_consequent() {
        let src = "function f() { if (x) { console.log(x); return a; } return b; }";
        let candidates = run_if_return(src);
        assert_eq!(candidates.len(), 0, "consequent with non-return stmt should NOT be detected");
    }

    #[test]
    fn if_return_normal_if_no_false_positive() {
        let src = "function f() { if (x) { a(); } }";
        let candidates = run_if_return(src);
        assert_eq!(candidates.len(), 0, "normal if should not be detected");
    }

    #[test]
    fn if_return_no_return_after() {
        let src = "function f() { if (x) return a; console.log(b); }";
        let candidates = run_if_return(src);
        assert_eq!(candidates.len(), 0, "no return after if should not be detected");
    }

    #[test]
    fn if_return_nested_function() {
        let src = "function f() { function g() { if (x) return a; return b; } }";
        let candidates = run_if_return(src);
        assert_eq!(candidates.len(), 1, "pattern inside nested function should be detected");
    }

    #[test]
    fn if_return_empty_program() {
        let src = "";
        let candidates = run_if_return(src);
        assert_eq!(candidates.len(), 0);
    }

    // ── Join-vars tests ──

    #[test]
    fn join_vars_same_kind_var() {
        let src = "var a = 1; var b = 2;";
        let pairs = run_join_vars(src);
        assert_eq!(pairs.len(), 1, "var a = 1; var b = 2; should be joinable");
    }

    #[test]
    fn join_vars_same_kind_let() {
        let src = "let a = 1; let b = 2;";
        let pairs = run_join_vars(src);
        assert_eq!(pairs.len(), 1, "let a = 1; let b = 2; should be joinable");
    }

    #[test]
    fn join_vars_same_kind_const() {
        let src = "const a = 1; const b = 2;";
        let pairs = run_join_vars(src);
        assert_eq!(pairs.len(), 1, "const a = 1; const b = 2; should be joinable");
    }

    #[test]
    fn join_vars_different_kinds() {
        let src = "var a = 1; let b = 2;";
        let pairs = run_join_vars(src);
        assert_eq!(pairs.len(), 0, "var and let should NOT be joinable");
    }

    #[test]
    fn join_vars_not_consecutive() {
        let src = "var a = 1; console.log(a); var b = 2;";
        let pairs = run_join_vars(src);
        assert_eq!(pairs.len(), 0, "non-consecutive var decls should NOT be joinable");
    }

    #[test]
    fn join_vars_empty_declarations() {
        let src = "var a; var b;";
        let pairs = run_join_vars(src);
        assert_eq!(pairs.len(), 1, "empty var declarations should be joinable");
    }

    #[test]
    fn join_vars_three_consecutive() {
        let src = "var a = 1; var b = 2; var c = 3;";
        let pairs = run_join_vars(src);
        assert_eq!(pairs.len(), 2, "three consecutive vars should produce 2 pairs");
    }

    #[test]
    fn join_vars_in_block() {
        let src = "{ var a = 1; var b = 2; }";
        let pairs = run_join_vars(src);
        assert_eq!(pairs.len(), 1, "vars inside a block should be detected");
    }

    #[test]
    fn join_vars_in_function() {
        let src = "function f() { var a = 1; var b = 2; }";
        let pairs = run_join_vars(src);
        assert_eq!(pairs.len(), 1, "vars inside a function should be detected");
    }

    #[test]
    fn join_vars_empty_program() {
        let src = "";
        let pairs = run_join_vars(src);
        assert_eq!(pairs.len(), 0);
    }

    // ── Sequences tests ──

    #[test]
    fn sequences_simple() {
        let src = "a(); b();";
        let pairs = run_sequences(src);
        assert_eq!(pairs.len(), 1, "a(); b(); should produce a sequence pair");
        let texts = pair_texts(src, &pairs);
        assert!(texts.iter().any(|t| t.contains("a()") && t.contains("b()")));
    }

    #[test]
    fn sequences_not_consecutive() {
        let src = "a(); if(x) {} b();";
        let pairs = run_sequences(src);
        assert_eq!(pairs.len(), 0, "non-consecutive expression stmts should NOT be detected");
    }

    #[test]
    fn sequences_binary_expr() {
        let src = "a() + b(); c();";
        let pairs = run_sequences(src);
        assert_eq!(pairs.len(), 1, "a() + b() is still an expression stmt");
    }

    #[test]
    fn sequences_single_stmt() {
        let src = "a();";
        let pairs = run_sequences(src);
        assert_eq!(pairs.len(), 0, "single expression stmt should not produce a pair");
    }

    #[test]
    fn sequences_no_false_positive_var_decl() {
        let src = "var a = 1; b();";
        let pairs = run_sequences(src);
        assert_eq!(pairs.len(), 0, "var decl followed by expr stmt should not be detected");
    }

    #[test]
    fn sequences_in_block() {
        let src = "{ a(); b(); }";
        let pairs = run_sequences(src);
        assert_eq!(pairs.len(), 1, "expression stmts inside a block should be detected");
    }

    #[test]
    fn sequences_in_function() {
        let src = "function f() { a(); b(); }";
        let pairs = run_sequences(src);
        assert_eq!(pairs.len(), 1, "expression stmts inside a function should be detected");
    }

    #[test]
    fn sequences_three_consecutive() {
        let src = "a(); b(); c();";
        let pairs = run_sequences(src);
        assert_eq!(pairs.len(), 2, "three consecutive expr stmts should produce 2 pairs");
    }

    #[test]
    fn sequences_empty_program() {
        let src = "";
        let pairs = run_sequences(src);
        assert_eq!(pairs.len(), 0);
    }
}
