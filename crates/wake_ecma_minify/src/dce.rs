//! # Dead Code Elimination (DCE) Analysis
//!
//! Walks the AST and identifies statements that can be safely removed:
//!
//! 1. **Unreachable statements** after `return` / `throw` / `break` / `continue`.
//! 2. **Empty statements** (`;`).
//! 3. **Debugger statements** (when `drop_debugger` is set).
//! 4. **Pure expression statements** (calls to pure functions whose result is unused).
//! 5. **Console.\* calls** (when `drop_console` is set).
//!
//! The analysis is block-scoped: a `return` inside an `if` only makes subsequent
//! statements in that `if` branch unreachable, not statements after the `if`.

use wake_common::{FxHashSet, Interner, Span};
use wake_ecma_ast::*;
use wake_ecma_parser::analyze;

use crate::const_eval::has_hoisted_decl;
use crate::purity::call_is_pure;

/// Set of statement spans that should be removed during codegen.
#[derive(Debug, Default)]
pub struct DcePlan {
    /// Spans of statements that should be removed entirely.
    pub remove_spans: FxHashSet<Span>,
}

/// Run DCE analysis on a parsed program.
///
/// Returns a [`DcePlan`] containing all statement spans that can be safely removed.
pub fn analyze_dce(
    program: &Program,
    interner: &Interner,
    drop_debugger: bool,
    drop_console: bool,
) -> DcePlan {
    let semantic = analyze(program);
    let resolved_reference_spans: FxHashSet<Span> = semantic
        .references
        .iter()
        .filter(|reference| reference.resolved.is_some())
        .map(|reference| reference.span)
        .collect();
    let mut analyzer = DceAnalyzer {
        remove_spans: FxHashSet::default(),
        drop_debugger,
        drop_console,
        interner: Some(interner),
        resolved_reference_spans,
    };
    analyzer.analyze_seq(&program.body);
    DcePlan {
        remove_spans: analyzer.remove_spans,
    }
}

// ── Internal analyzer ──────────────────────────────────────────────────────

struct DceAnalyzer<'a> {
    remove_spans: FxHashSet<Span>,
    drop_debugger: bool,
    drop_console: bool,
    interner: Option<&'a Interner>,
    resolved_reference_spans: FxHashSet<Span>,
}

impl DceAnalyzer<'_> {
    /// Analyze a sequence of statements (block body) sequentially.
    fn analyze_seq(&mut self, stmts: &[Statement]) {
        let mut unreachable = false;
        for stmt in stmts {
            if unreachable {
                if has_hoisted_decl(stmt) {
                    // Hoisted declarations are preserved but we still recurse.
                } else {
                    self.remove_spans.insert(stmt.span());
                }
                // Always recurse in case there are nested function bodies.
                self.analyze_single(stmt);
                continue;
            }

            // Check for individually removable statements.
            match stmt {
                Statement::Empty(span) => {
                    self.remove_spans.insert(*span);
                }
                Statement::Debugger(span) if self.drop_debugger => {
                    self.remove_spans.insert(*span);
                }
                Statement::Expression(es) if self.is_removable_expr_stmt(&es.expression) => {
                    self.remove_spans.insert(es.span);
                }
                Statement::Return(_)
                | Statement::Throw(_)
                | Statement::Break(_)
                | Statement::Continue(_) => {
                    unreachable = true;
                }
                _ => {}
            }

            // Recurse into nested blocks / function bodies.
            self.analyze_single(stmt);
        }
    }

    /// Recurse into a single statement's nested blocks and function bodies.
    fn analyze_single(&mut self, stmt: &Statement) {
        match stmt {
            Statement::Block(b) => self.analyze_seq(&b.body),
            Statement::If(s) => {
                self.analyze_single(&s.consequent);
                if let Some(alt) = &s.alternate {
                    self.analyze_single(alt);
                }
            }
            Statement::For(s) => self.analyze_single(&s.body),
            Statement::ForIn(s) => self.analyze_single(&s.body),
            Statement::ForOf(s) => self.analyze_single(&s.body),
            Statement::While(s) => self.analyze_single(&s.body),
            Statement::DoWhile(s) => self.analyze_single(&s.body),
            Statement::Switch(s) => {
                for case in s.cases.iter() {
                    self.analyze_seq(&case.consequent);
                }
            }
            Statement::Try(s) => {
                self.analyze_seq(&s.block.body);
                if let Some(h) = &s.handler {
                    self.analyze_seq(&h.body.body);
                }
                if let Some(f) = &s.finalizer {
                    self.analyze_seq(&f.body);
                }
            }
            Statement::Labeled(s) => self.analyze_single(&s.body),
            Statement::With(s) => self.analyze_single(&s.body),
            Statement::FunctionDeclaration(f) => {
                if let Some(body) = &f.body {
                    self.analyze_seq(&body.statements);
                }
            }
            Statement::ClassDeclaration(c) => self.analyze_class(c),
            Statement::ExportNamed(s) => {
                if let Some(decl) = &s.declaration {
                    self.analyze_single(decl);
                }
            }
            Statement::ExportDefault(s) => match s.declaration {
                ExportDefaultKind::Function(f) => {
                    if let Some(body) = &f.body {
                        self.analyze_seq(&body.statements);
                    }
                }
                ExportDefaultKind::Class(c) => self.analyze_class(c),
                ExportDefaultKind::Expression(e) => self.analyze_expr_for_functions(&e),
            },
            // Walk variable initializers for function/arrow/class bodies.
            Statement::VariableDeclaration(d) => {
                for decl in d.declarations.iter() {
                    if let Some(init) = &decl.init {
                        self.analyze_expr_for_functions(init);
                    }
                }
            }
            // Walk return/throw arguments for function/arrow/class bodies.
            Statement::Return(s) => {
                if let Some(arg) = &s.argument {
                    self.analyze_expr_for_functions(arg);
                }
            }
            Statement::Throw(s) => {
                self.analyze_expr_for_functions(&s.argument);
            }
            Statement::Expression(e) => {
                self.analyze_expr_for_functions(&e.expression);
            }
            _ => {}
        }
    }

    /// Check whether an expression statement's expression can be removed.
    fn is_removable_expr_stmt(&self, expr: &Expression) -> bool {
        match expr {
            // Reading a lexical binding has no observable effect. Keep member reads because
            // getters/proxies may run user code, and keep calls/new expressions unless the
            // dedicated purity analysis proves them safe.
            Expression::Identifier(id) => self.resolved_reference_spans.contains(&id.span),
            Expression::NumberLiteral(_)
            | Expression::StringLiteral(_)
            | Expression::BooleanLiteral(_)
            | Expression::NullLiteral(_)
            | Expression::BigIntLiteral(_)
            | Expression::RegExpLiteral(_)
            | Expression::Function(_)
            | Expression::Arrow(_) => true,
            Expression::Call(c) => {
                // console.* call removal.
                if self.drop_console && self.is_console_call(&c.callee) {
                    return true;
                }
                // Pure function call removal.
                if let Some(interner) = self.interner {
                    let args: Vec<_> = c.arguments.iter().copied().collect();
                    call_is_pure(&c.callee, &args, Some(interner))
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Check if an expression is `console.<method>` (non-optional member access).
    fn is_console_call(&self, callee: &Expression) -> bool {
        match callee {
            Expression::Member(m) if !m.optional => match &m.object {
                Expression::Identifier(id) => {
                    if let Some(interner) = self.interner {
                        let name = interner.resolve(id.name);
                        name == "console" && matches!(m.property, MemberProperty::Ident(_))
                    } else {
                        false
                    }
                }
                _ => false,
            },
            _ => false,
        }
    }

    /// Walk an expression tree to find function/arrow/class bodies to analyze.
    fn analyze_expr_for_functions(&mut self, expr: &Expression) {
        match expr {
            Expression::Function(f) => {
                if let Some(body) = &f.body {
                    self.analyze_seq(&body.statements);
                }
            }
            Expression::Arrow(a) => {
                if let ArrowBody::Block(b) = &a.body {
                    self.analyze_seq(&b.statements);
                }
            }
            Expression::Class(c) => self.analyze_class(c),
            Expression::Call(c) => {
                for arg in c.arguments.iter() {
                    self.analyze_expr_for_functions(arg);
                }
                self.analyze_expr_for_functions(&c.callee);
            }
            Expression::New(n) => {
                for arg in n.arguments.iter() {
                    self.analyze_expr_for_functions(arg);
                }
                self.analyze_expr_for_functions(&n.callee);
            }
            Expression::Member(m) => {
                self.analyze_expr_for_functions(&m.object);
            }
            Expression::TaggedTemplate(t) => {
                self.analyze_expr_for_functions(&t.tag);
                // Template expressions won't contain function bodies, skip.
            }
            Expression::Array(a) => {
                for el in a.elements.iter().flatten() {
                    self.analyze_expr_for_functions(el);
                }
            }
            Expression::Object(o) => {
                for member in o.properties.iter() {
                    match member {
                        ObjectMember::Property(p) => {
                            self.analyze_expr_for_functions(&p.value);
                        }
                        ObjectMember::Spread(s) => {
                            self.analyze_expr_for_functions(&s.argument);
                        }
                    }
                }
            }
            Expression::Unary(u) => self.analyze_expr_for_functions(&u.argument),
            Expression::Binary(b) => {
                self.analyze_expr_for_functions(&b.left);
                self.analyze_expr_for_functions(&b.right);
            }
            Expression::Logical(l) => {
                self.analyze_expr_for_functions(&l.left);
                self.analyze_expr_for_functions(&l.right);
            }
            Expression::Assignment(a) => {
                self.analyze_expr_for_functions(&a.left);
                self.analyze_expr_for_functions(&a.right);
            }
            Expression::Conditional(c) => {
                self.analyze_expr_for_functions(&c.test);
                self.analyze_expr_for_functions(&c.consequent);
                self.analyze_expr_for_functions(&c.alternate);
            }
            Expression::Sequence(s) => {
                for e in s.expressions.iter() {
                    self.analyze_expr_for_functions(e);
                }
            }
            Expression::Spread(s) => self.analyze_expr_for_functions(&s.argument),
            Expression::Await(a) => self.analyze_expr_for_functions(&a.argument),
            Expression::Yield(y) => {
                if let Some(arg) = &y.argument {
                    self.analyze_expr_for_functions(arg);
                }
            }
            Expression::Update(u) => self.analyze_expr_for_functions(&u.argument),
            // Literals, identifiers, etc. never contain function bodies.
            _ => {}
        }
    }

    /// Analyze a class for method bodies and static blocks.
    fn analyze_class(&mut self, c: &Class) {
        for member in c.body.iter() {
            match member {
                ClassMember::Method(m) => {
                    if let Some(body) = &m.value.body {
                        self.analyze_seq(&body.statements);
                    }
                }
                ClassMember::Property(p) => {
                    if let Some(val) = &p.value {
                        self.analyze_expr_for_functions(val);
                    }
                }
                ClassMember::StaticBlock(b) => {
                    self.analyze_seq(&b.body);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wake_common::Interner;
    use wake_ecma_ast::SourceType;
    use wake_ecma_parser::parse;

    fn run_dce(src: &str, drop_debugger: bool, drop_console: bool) -> (FxHashSet<Span>, Interner) {
        let it = Interner::new();
        let out = parse(src, &it, SourceType::Module);
        assert!(!out.has_errors(), "parse error: {:?}", out.diagnostics);
        out.module.with_ast(|p| {
            let plan = analyze_dce(p, &it, drop_debugger, drop_console);
            (plan.remove_spans, it)
        })
    }

    fn remove_spans(src: &str, drop_debugger: bool, drop_console: bool) -> Vec<(u32, u32)> {
        let (spans, _) = run_dce(src, drop_debugger, drop_console);
        let mut v: Vec<(u32, u32)> = spans.iter().map(|s| (s.lo, s.hi)).collect();
        v.sort();
        v
    }

    fn remove_count(src: &str, drop_debugger: bool, drop_console: bool) -> usize {
        let (spans, _) = run_dce(src, drop_debugger, drop_console);
        spans.len()
    }

    fn spans_text(src: &str, drop_debugger: bool, drop_console: bool) -> Vec<String> {
        let (spans, _) = run_dce(src, drop_debugger, drop_console);
        let mut v: Vec<String> = spans
            .iter()
            .map(|s| src[s.lo as usize..s.hi as usize].to_string())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn resolved_identifier_read_is_removable() {
        assert_eq!(spans_text("const value=1;value;", false, false), ["value;"]);
    }

    #[test]
    fn unresolved_identifier_read_is_preserved_for_reference_error() {
        assert!(spans_text("missingGlobal;", false, false).is_empty());
    }

    // ── 1. Unreachable after return ──

    #[test]
    fn code_after_return_marked_unreachable() {
        let src = "function f() { return 1; let x = 2; }";
        assert_eq!(remove_count(src, false, false), 1);
        assert!(
            spans_text(src, false, false)
                .iter()
                .any(|t| t.contains("let x = 2"))
        );
    }

    #[test]
    fn multiple_stmts_after_return() {
        let src = "function f() { return; let a; let b; let c; }";
        assert_eq!(remove_count(src, false, false), 3);
    }

    #[test]
    fn return_in_block_only_affects_own_block() {
        let src = "function f() { if (true) { return 1; let x = 2; } let y = 3; }";
        let spans = remove_spans(src, false, false);
        // Only `let x = 2` should be removed, `let y = 3` should stay.
        assert!(!spans.iter().any(|s| {
            let text = &src[s.0 as usize..s.1 as usize];
            text.contains("let y = 3")
        }));
        assert!(spans.iter().any(|s| {
            let text = &src[s.0 as usize..s.1 as usize];
            text.contains("let x = 2")
        }));
    }

    #[test]
    fn function_decl_after_return_preserved() {
        let src = "function f() { return 1; function g() {} }";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.is_empty(),
            "should preserve function decl after return"
        );
    }

    #[test]
    fn var_decl_after_return_preserved() {
        let src = "function f() { return 1; var x = 2; }";
        let spans = remove_spans(src, false, false);
        assert!(spans.is_empty(), "should preserve var decl after return");
    }

    #[test]
    fn let_after_return_is_removed() {
        let src = "function f() { return 1; let x = 2; }";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text.contains("let x = 2")
            }),
            "let after return should be removed"
        );
    }

    #[test]
    fn non_hoisted_after_return_removed() {
        let src = "function f() { return; const x = 1; }";
        assert_eq!(remove_count(src, false, false), 1);
        assert!(
            spans_text(src, false, false)
                .iter()
                .any(|t| t.contains("const x = 1"))
        );
    }

    // ── 2. Unreachable after throw ──

    #[test]
    fn code_after_throw_marked_unreachable() {
        let src = "function f() { throw new Error(); let x = 2; }";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text.contains("let x = 2")
            }),
            "let after throw should be removed"
        );
    }

    #[test]
    fn function_decl_after_throw_preserved() {
        let src = "function f() { throw new Error(); function g() {} }";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.is_empty(),
            "function decl after throw should be preserved"
        );
    }

    // ── 3. Unreachable after break ──

    #[test]
    fn code_after_break_in_switch_marked_unreachable() {
        let src = "function f(x) { switch(x) { case 1: break; let y = 2; } }";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text.contains("let y = 2")
            }),
            "let after break in switch should be removed"
        );
    }

    #[test]
    fn code_after_break_in_loop_marked_unreachable() {
        let src = "function f() { while(true) { break; let x = 2; } }";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text.contains("let x = 2")
            }),
            "let after break in loop should be removed"
        );
    }

    // ── 4. Unreachable after continue ──

    #[test]
    fn code_after_continue_marked_unreachable() {
        let src = "function f() { while(true) { continue; let x = 2; } }";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text.contains("let x = 2")
            }),
            "let after continue should be removed"
        );
    }

    // ── 5. Empty statement ──

    #[test]
    fn empty_statement_removed() {
        let src = "function f() { ; let x = 1; }";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text == ";"
            }),
            "empty statement should be removed"
        );
    }

    #[test]
    fn multiple_empty_statements_removed() {
        let src = "function f() { ; ; ; let x = 1; }";
        assert_eq!(remove_count(src, false, false), 3);
    }

    // ── 6. Debugger statement ──

    #[test]
    fn debugger_removed_when_drop_debugger() {
        let src = "function f() { debugger; }";
        assert_eq!(remove_count(src, true, false), 1);
        assert!(
            spans_text(src, true, false)
                .iter()
                .any(|t| t.contains("debugger"))
        );
    }

    #[test]
    fn debugger_not_removed_when_not_drop_debugger() {
        let src = "function f() { debugger; }";
        assert_eq!(remove_count(src, false, false), 0);
    }

    // ── 7. Pure call expression statement ──

    #[test]
    fn pure_call_expr_stmt_removed() {
        let src = "Math.abs(-5);";
        assert_eq!(remove_count(src, false, false), 1);
        assert!(
            spans_text(src, false, false)
                .iter()
                .any(|t| t.contains("Math.abs"))
        );
    }

    #[test]
    fn pure_call_expr_stmt_in_block_removed() {
        let src = "function f() { Math.abs(-5); let x = 1; }";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text.contains("Math.abs(-5)")
            }),
            "Math.abs(-5) in block should be removed"
        );
    }

    #[test]
    fn impure_call_not_removed() {
        let src = "console.log('hello');";
        assert_eq!(remove_count(src, false, false), 0);
    }

    // ── 8. Console.* call ──

    #[test]
    fn console_log_removed_when_drop_console() {
        let src = "console.log('hello');";
        assert_eq!(remove_count(src, false, true), 1);
        assert!(
            spans_text(src, false, true)
                .iter()
                .any(|t| t.contains("console.log"))
        );
    }

    #[test]
    fn console_error_removed_when_drop_console() {
        let src = "console.error('oops');";
        assert_eq!(remove_count(src, false, true), 1);
        assert!(
            spans_text(src, false, true)
                .iter()
                .any(|t| t.contains("console.error"))
        );
    }

    #[test]
    fn console_call_not_removed_when_not_drop_console() {
        let src = "console.log('hello');";
        assert_eq!(remove_count(src, false, false), 0);
    }

    // ── 9. Function declarations after return are preserved ──

    #[test]
    fn hoisted_func_decl_preserved_after_return() {
        let src = "function f() { return 1; function g() { return 2; } }";
        let spans = remove_spans(src, false, false);
        // Function g() should NOT be removed even though it's after return.
        assert!(!spans.iter().any(|s| {
            let text = &src[s.0 as usize..s.1 as usize];
            text.contains("function g")
        }));
    }

    #[test]
    fn hoisted_func_after_throw() {
        let src = "function f() { throw 1; function g() {} }";
        assert_eq!(remove_count(src, false, false), 0);
    }

    #[test]
    fn hoisted_func_after_break() {
        let src = "function f() { while(true) { break; function g() {} } }";
        assert_eq!(remove_count(src, false, false), 0);
    }

    #[test]
    fn hoisted_func_after_continue() {
        let src = "function f() { while(true) { continue; function g() {} } }";
        assert_eq!(remove_count(src, false, false), 0);
    }

    // ── 10. Nested blocks: return in inner block does not affect outer ──

    #[test]
    fn return_in_inner_block_does_not_affect_outer() {
        let src = "function f() { { return 1; let x = 2; } let y = 3; }";
        let spans = remove_spans(src, false, false);
        // `let x = 2` should be removed (inner block after return).
        // `let y = 3` should NOT be removed (outside inner block).
        assert!(spans.iter().any(|s| {
            let text = &src[s.0 as usize..s.1 as usize];
            text.contains("let x = 2")
        }));
        assert!(!spans.iter().any(|s| {
            let text = &src[s.0 as usize..s.1 as usize];
            text.contains("let y = 3")
        }));
    }

    // ── 11. Multiple returns: only the first return makes code unreachable ──

    #[test]
    fn only_first_return_triggers_unreachable() {
        let src = "function f() { return 1; return 2; let x = 3; }";
        let spans = remove_spans(src, false, false);
        // Both `return 2;` and `let x = 3;` should be removed.
        assert!(spans.iter().any(|s| {
            let text = &src[s.0 as usize..s.1 as usize];
            text.contains("return 2")
        }));
        assert!(spans.iter().any(|s| {
            let text = &src[s.0 as usize..s.1 as usize];
            text.contains("let x = 3")
        }));
    }

    // ── Edge cases ──

    #[test]
    fn empty_program() {
        let src = "";
        assert_eq!(remove_count(src, true, true), 0);
    }

    #[test]
    fn no_dce_opportunities() {
        let src = "let x = 1; x = x + 1; export default x;";
        assert_eq!(remove_count(src, false, false), 0);
    }

    #[test]
    fn try_finally_return_not_unreachable() {
        // Code in finally block is still reachable even after return in try.
        let src = "function f() { try { return 1; } finally { let x = 2; } let y = 3; }";
        let spans = remove_spans(src, false, false);
        // `let x = 2` in finally should NOT be removed.
        assert!(!spans.iter().any(|s| {
            let text = &src[s.0 as usize..s.1 as usize];
            text.contains("let x = 2")
        }));
    }

    #[test]
    fn nested_function_bodies_analyzed() {
        let src = "function outer() { function inner() { return 1; let x = 2; } }";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text.contains("let x = 2")
            }),
            "nested function body should also be analyzed"
        );
    }

    #[test]
    fn function_expression_in_var_init_analyzed() {
        let src = "const f = function() { return 1; let x = 2; };";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text.contains("let x = 2")
            }),
            "function expression in const should be analyzed"
        );
    }

    #[test]
    fn arrow_function_body_analyzed() {
        let src = "const f = () => { return 1; let x = 2; };";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text.contains("let x = 2")
            }),
            "arrow function body should be analyzed"
        );
    }

    #[test]
    fn class_method_body_analyzed() {
        let src = "class A { method() { return 1; let x = 2; } }";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text.contains("let x = 2")
            }),
            "class method body should be analyzed"
        );
    }

    #[test]
    fn combined_dce_all_flags() {
        let src = "
            function f() {
                ;
                debugger;
                Math.abs(-5);
                console.log('hi');
                return 1;
                let x = 2;
            }
        ";
        let count = remove_count(src, true, true);
        // Should remove: ;, debugger, Math.abs(-5), console.log('hi'), let x = 2
        assert!(count >= 5, "expected >=5 removals, got {count}");
    }

    #[test]
    fn export_default_function_analyzed() {
        let src = "export default function() { return 1; let x = 2; }";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text.contains("let x = 2")
            }),
            "export default function body should be analyzed"
        );
    }

    #[test]
    fn if_after_return_not_affected() {
        // Return inside if branch should only affect that branch.
        let src = "function f() { if (a) { return 1; let x = 2; } let y = 3; }";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text.contains("let x = 2")
            }),
            "let x = 2 in if branch should be removed"
        );
        assert!(
            !spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text.contains("let y = 3")
            }),
            "let y = 3 should not be removed"
        );
    }

    #[test]
    fn labeled_break_unreachable() {
        let src = "function f() { label: { break label; let x = 2; } let y = 3; }";
        let spans = remove_spans(src, false, false);
        assert!(
            spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text.contains("let x = 2")
            }),
            "let x = 2 after break label should be removed"
        );
        assert!(
            !spans.iter().any(|s| {
                let text = &src[s.0 as usize..s.1 as usize];
                text.contains("let y = 3")
            }),
            "let y = 3 after labeled block should stay"
        );
    }
}
