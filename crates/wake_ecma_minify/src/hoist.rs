//! # Scope Hoisting — Plan phase
//!
//! Collects `var` declarations inside function bodies for hoisting to the
//! function top. This enables `join_vars` to merge more declarations,
//! improving minification.
//!
//! ## Safety
//!
//! Lifting `var` declarations (with initializers) from any nesting depth to
//! the function top is valid JavaScript: `var` is hoisted by the engine.
//! Assignment timing changes only for vars inside conditional branches
//! (if/for/while), which is accepted for minification.
//!
//! ## What is hoisted
//!
//! All `var` declarations inside a function body, including those nested in
//! blocks, if, for, while, switch, try, etc. **Not** hoisted:
//! - `let` / `const` (block-scoped)
//! - Module-level `var` (already at top of scope)
//! - `var` inside nested function / class boundaries
//! - `var` in for-inits (`for (var i = 0; ...)`)

use wake_common::{FxHashMap, FxHashSet, Span};
use wake_ecma_ast::*;

/// Results of scope-hoisting analysis consumed by codegen.
#[derive(Debug, Default)]
pub struct HoistPlan {
    /// Per-function-body: spans of `var` declarations to hoist to the top.
    pub var_hoist_spans: FxHashMap<Span, Vec<Span>>,
    /// Flat set of all var declaration spans to hoist (O(1) lookup).
    pub var_hoist_flat: FxHashSet<Span>,
}

/// Analyse the program and produce a hoist plan.
pub fn plan_hoist(program: &Program) -> HoistPlan {
    let mut var_hoist_spans = FxHashMap::default();
    collect_from_program(&program.body, &mut var_hoist_spans);
    let var_hoist_flat = var_hoist_spans
        .values()
        .flat_map(|v| v.iter().copied())
        .collect();
    HoistPlan {
        var_hoist_spans,
        var_hoist_flat,
    }
}

/// Walk the program looking for function declarations (and function
/// expressions in default exports), collecting their hoisted vars.
fn collect_from_program(stmts: &[Statement], out: &mut FxHashMap<Span, Vec<Span>>) {
    for stmt in stmts {
        match stmt {
            Statement::FunctionDeclaration(f) => {
                if let Some(body) = &f.body {
                    collect_hoisted_impl(&body.statements, body.span, out);
                }
            }
            Statement::Block(b) => collect_from_program(&b.body, out),
            Statement::If(s) => {
                collect_from_program(std::slice::from_ref(&s.consequent), out);
                if let Some(alt) = &s.alternate {
                    collect_from_program(std::slice::from_ref(alt), out);
                }
            }
            Statement::For(s) => collect_from_program(std::slice::from_ref(&s.body), out),
            Statement::ForIn(s) => collect_from_program(std::slice::from_ref(&s.body), out),
            Statement::ForOf(s) => collect_from_program(std::slice::from_ref(&s.body), out),
            Statement::While(s) => collect_from_program(std::slice::from_ref(&s.body), out),
            Statement::DoWhile(s) => collect_from_program(std::slice::from_ref(&s.body), out),
            Statement::Switch(s) => {
                for case in s.cases.iter() {
                    collect_from_program(&case.consequent, out);
                }
            }
            Statement::Try(s) => {
                collect_from_program(&s.block.body, out);
                if let Some(h) = &s.handler {
                    collect_from_program(&h.body.body, out);
                }
                if let Some(f) = &s.finalizer {
                    collect_from_program(&f.body, out);
                }
            }
            Statement::Labeled(s) => collect_from_program(std::slice::from_ref(&s.body), out),
            Statement::With(s) => collect_from_program(std::slice::from_ref(&s.body), out),
            Statement::ExportNamed(e) => {
                if let Some(d) = &e.declaration {
                    collect_from_program(std::slice::from_ref(d), out);
                }
            }
            Statement::ExportDefault(e) => {
                if let ExportDefaultKind::Function(f) = &e.declaration
                    && let Some(body) = &f.body
                {
                    collect_hoisted_impl(&body.statements, body.span, out);
                }
            }
            _ => {}
        }
    }
}

/// Collect all `var` declarations inside `stmts` that belong to the function
/// body identified by `body_span`.  Descends into all container statements
/// (block, if, for, …) but stops at nested function / class boundaries.
fn collect_hoisted_impl(
    stmts: &[Statement],
    body_span: Span,
    out: &mut FxHashMap<Span, Vec<Span>>,
) {
    // Hoisted declarations are emitted before the original body. Doing that in a function with
    // a directive prologue would move code ahead of `"use strict"` (or framework directives) and
    // silently change the function's semantics. Keep such bodies in source order.
    if matches!(
        stmts.first(),
        Some(Statement::Expression(expression))
            if matches!(expression.expression, Expression::StringLiteral(_))
    ) {
        return;
    }
    let mut hoisted = Vec::new();
    collect_recursive(stmts, &mut hoisted);
    if !hoisted.is_empty() {
        out.insert(body_span, hoisted);
    }
}

fn collect_recursive(stmts: &[Statement], out: &mut Vec<Span>) {
    for stmt in stmts {
        match stmt {
            Statement::VariableDeclaration(d) if d.kind == VarKind::Var && !d.span.is_dummy() => {
                out.push(d.span);
            }
            // Descend into unconditional containers
            Statement::Block(b) => collect_recursive(&b.body, out),
            Statement::Labeled(s) => collect_recursive(std::slice::from_ref(&s.body), out),
            Statement::With(s) => collect_recursive(std::slice::from_ref(&s.body), out),
            // If branches (conditional but still hoisted by our optimization)
            Statement::If(s) => {
                collect_recursive(std::slice::from_ref(&s.consequent), out);
                if let Some(alt) = &s.alternate {
                    collect_recursive(std::slice::from_ref(alt), out);
                }
            }
            Statement::Switch(s) => {
                for case in s.cases.iter() {
                    collect_recursive(&case.consequent, out);
                }
            }
            Statement::Try(s) => {
                collect_recursive(&s.block.body, out);
                if let Some(h) = &s.handler {
                    collect_recursive(&h.body.body, out);
                }
                if let Some(f) = &s.finalizer {
                    collect_recursive(&f.body, out);
                }
            }
            // Loop bodies — do NOT hoist from loops (preserves assignment timing)
            Statement::For(_)
            | Statement::ForIn(_)
            | Statement::ForOf(_)
            | Statement::While(_)
            | Statement::DoWhile(_) => {}
            // Stop at scope boundaries
            Statement::FunctionDeclaration(_) | Statement::ClassDeclaration(_) => {}
            _ => {}
        }
    }
}
