//! # Expression Simplification Planner
//!
//! Identifies expressions that can be simplified (shortened) without changing semantics.
//! Produces side-table entries consumed by codegen at emit time.
//!
//! ## Simplifications
//!
//! | Pattern | Result | Condition |
//! |---------|--------|-----------|
//! | `!!x`   | `x`    | x has no side effects |
//! | `!true` | `false`| constant fold |
//! | `a['b']`| `a.b`  | b is a valid identifier |
//! | `true ? x : y` | `x` | constant fold |
//! | `false ? x : y` | `y` | constant fold |
//! | `void 0` | `void 0` | always pure, but keep (shorter than undefined) |
//! | `"a"+"b"` | `"ab"` | constant fold via const_eval |
//! | `2+3` | `5` | constant fold via const_eval |
//! | `` `hello ${1+2}` `` | `"hello 3"` | all-const template literal |

use crate::const_eval::{self, ConstCtx, ConstVal, expr_is_pure};
use wake_common::{FxHashMap, FxHashSet, Interner, Span};
use wake_ecma_ast::*;

/// A planned simplification at a specific span location.
#[derive(Debug, Clone, PartialEq)]
pub enum SimplifyAction {
    /// Replace the expression at this span with literal source text.
    ReplaceWith(String),
    /// Convert bracket notation to dot notation: `a['b']` → `a.b`
    BracketToDot,
    /// Remove double negation: `!!x` → `x`
    RemoveDoubleNot,
}

/// Result of the simplification planner: actions + const-folded constants + bracket names.
pub struct SimplifyPlan {
    /// Span → action for all simplifiable expressions.
    pub actions: FxHashMap<Span, SimplifyAction>,
    /// Span → constant value for all const-foldable expressions.
    pub constants: FxHashMap<Span, ConstVal>,
    /// Span → property name for bracket-to-dot candidates.
    pub bracket_names: FxHashMap<Span, String>,
}

/// Walk the AST and build a [`SimplifyPlan`] for all simplifiable expressions.
///
/// The `source` must be the original source text (used to read span contents
/// for branch inlining). `interner` is used for identifier resolution.
pub fn plan_simplifications(
    program: &Program<'_>,
    source: &str,
    interner: &Interner,
) -> SimplifyPlan {
    // Parser-time transforms deliberately reuse the original source span for their synthetic
    // parent/child nodes. A span-keyed rewrite is only safe when that span identifies exactly one
    // expression; otherwise a folded child such as the `0` in synthetic `void 0` can overwrite
    // the entry for the whole optional-chain sequence and replace it with `0` at codegen time.
    let mut span_audit = ExpressionSpanAudit::default();
    span_audit.visit_program(program);
    let mut planner = SimplifyPlanner {
        actions: FxHashMap::default(),
        constants: FxHashMap::default(),
        bracket_names: FxHashMap::default(),
        ambiguous_spans: span_audit.ambiguous,
        source,
        interner,
    };
    planner.visit_program(program);
    SimplifyPlan {
        actions: planner.actions,
        constants: planner.constants,
        bracket_names: planner.bracket_names,
    }
}

struct SimplifyPlanner<'a> {
    actions: FxHashMap<Span, SimplifyAction>,
    constants: FxHashMap<Span, ConstVal>,
    bracket_names: FxHashMap<Span, String>,
    ambiguous_spans: FxHashSet<Span>,
    source: &'a str,
    interner: &'a Interner,
}

#[derive(Default)]
struct ExpressionSpanAudit {
    seen: FxHashSet<Span>,
    ambiguous: FxHashSet<Span>,
}

impl<'ast> Visit<'ast> for ExpressionSpanAudit {
    fn visit_expression(&mut self, node: &Expression<'ast>) {
        let span = node.span();
        if span.is_dummy() || !self.seen.insert(span) {
            self.ambiguous.insert(span);
        }
        walk_expression(self, node);
    }
}

impl<'a> SimplifyPlanner<'a> {
    fn span_text(&self, span: Span) -> String {
        self.source[span.lo as usize..span.hi as usize].to_string()
    }

    fn const_ctx(&self) -> ConstCtx<'a> {
        ConstCtx {
            defines: &[],
            known_vars: &[],
            interner: Some(self.interner),
        }
    }
}

impl<'s, 'ast> Visit<'ast> for SimplifyPlanner<'s> {
    fn visit_expression(&mut self, node: &Expression<'ast>) {
        if node.span().is_dummy() || self.ambiguous_spans.contains(&node.span()) {
            walk_expression(self, node);
            return;
        }
        // Try constant-folding for every expression (populates MinifyCtx.constants)
        let ctx = self.const_ctx();
        if let Some(val) = const_eval::const_eval(node, &ctx) {
            self.constants.insert(node.span(), val);
        }

        match node {
            // ── Pattern 1 & 2: Double negation / Not-constant
            Expression::Unary(u) if u.operator == UnaryOperator::LogicalNot => {
                // !!x → x  when inner argument is pure
                if let Expression::Unary(inner) = &u.argument
                    && inner.operator == UnaryOperator::LogicalNot
                    && expr_is_pure(&inner.argument)
                {
                    self.actions.insert(u.span, SimplifyAction::RemoveDoubleNot);
                    // Recurse into the inner argument for nested simplifications
                    self.visit_expression(&inner.argument);
                    return;
                }
                // !constant → !ConstVal result
                if let Some(val) = const_eval::const_eval(node, &ctx) {
                    self.actions
                        .insert(node.span(), SimplifyAction::ReplaceWith(val.to_source()));
                    return;
                }
            }

            // ── Pattern 3: void — do nothing (keep void 0 as shortest form for undefined)

            // ── Pattern 4: Ternary on constant
            Expression::Conditional(c) => {
                if let Some(val) = const_eval::const_eval(&c.test, &ctx) {
                    let branch = if val.truthy() {
                        &c.consequent
                    } else {
                        &c.alternate
                    };
                    let text = self.span_text(branch.span());
                    self.actions
                        .insert(c.span, SimplifyAction::ReplaceWith(text));
                    // Recurse into the selected branch for nested simplifications
                    self.visit_expression(branch);
                    return;
                }
            }

            // ── Pattern 5: Logical on constant
            Expression::Logical(l) => {
                if let Some(val) = const_eval::const_eval(&l.left, &ctx) {
                    match l.operator {
                        LogicalOperator::And => {
                            if val.truthy() {
                                // true && x → x
                                let text = self.span_text(l.right.span());
                                self.actions
                                    .insert(l.span, SimplifyAction::ReplaceWith(text));
                                self.visit_expression(&l.right);
                            } else {
                                // false && x → false
                                self.actions
                                    .insert(l.span, SimplifyAction::ReplaceWith("false".into()));
                            }
                        }
                        LogicalOperator::Or => {
                            if val.truthy() {
                                // true || x → true
                                self.actions
                                    .insert(l.span, SimplifyAction::ReplaceWith("true".into()));
                            } else {
                                // false || x → x
                                let text = self.span_text(l.right.span());
                                self.actions
                                    .insert(l.span, SimplifyAction::ReplaceWith(text));
                                self.visit_expression(&l.right);
                            }
                        }
                        LogicalOperator::Coalesce => {
                            // a ?? b stays — no simplification from const left alone
                            walk_expression(self, node);
                        }
                    }
                    return;
                }
            }

            // ── Pattern 6: Bracket to dot  a['b'] → a.b
            Expression::Member(m) if !m.optional => {
                if let MemberProperty::Computed(e) = &m.property
                    && let Expression::StringLiteral(s) = e
                {
                    let name = self.interner.resolve(s.value);
                    if is_valid_ident(&name) {
                        self.actions.insert(m.span, SimplifyAction::BracketToDot);
                        self.bracket_names.insert(m.span, name);
                        self.visit_expression(&m.object);
                        return;
                    }
                }
            }

            // ── Pattern 7 & 8: Binary folding (concat + arithmetic)
            Expression::Binary(_) => {
                if let Some(val) = const_eval::const_eval(node, &ctx) {
                    self.actions
                        .insert(node.span(), SimplifyAction::ReplaceWith(val.to_source()));
                    return;
                }
            }

            // ── Pattern 9: Template literal with all-constant parts
            Expression::TemplateLiteral(_) => {
                if let Some(val) = const_eval::const_eval(node, &ctx) {
                    self.actions
                        .insert(node.span(), SimplifyAction::ReplaceWith(val.to_source()));
                    return;
                }
            }

            _ => {}
        }

        walk_expression(self, node);
    }
}

/// Check if a string is a valid ECMAScript identifier name (ASCII subset).
fn is_valid_ident(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' && first != '$' {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

#[cfg(test)]
mod tests {
    use super::*;
    use wake_common::Interner;
    use wake_ecma_ast::SourceType;
    use wake_ecma_parser::parse;

    /// Parse a script and run the planner, returning the SimplifyPlan.
    fn plan(src: &str) -> SimplifyPlan {
        let it = Interner::new();
        let out = parse(src, &it, SourceType::Script);
        assert!(!out.has_errors(), "parse error: {:?}", out.diagnostics);
        out.module.with_ast(|p| plan_simplifications(p, src, &it))
    }

    /// Helper: from action map, find the action whose span covers exactly `expr_src`.
    /// The source must contain exactly one occurrence of `expr_src`.
    fn action_on(src: &str, expr_src: &str) -> Option<SimplifyAction> {
        let sp = plan(src);
        let idx = src.find(expr_src).expect("expr_src not found in source");
        let span = Span::new(idx as u32, (idx + expr_src.len()) as u32);
        sp.actions.get(&span).cloned()
    }

    fn assert_action(src: &str, expr_src: &str, expected: SimplifyAction) {
        let got = action_on(src, expr_src);
        assert_eq!(got, Some(expected), "on `{}` at `{}`", src, expr_src);
    }

    fn assert_no_action(src: &str, expr_src: &str) {
        let got = action_on(src, expr_src);
        assert!(
            got.is_none(),
            "unexpected action {:?} on `{}` at `{}`",
            got,
            src,
            expr_src
        );
    }

    fn assert_action_count(src: &str, count: usize) {
        let sp = plan(src);
        assert_eq!(sp.actions.len(), count, "on `{}`", src);
    }

    // ── Double negation ──

    #[test]
    fn double_not_ident() {
        assert_action("!!x;", "!!x", SimplifyAction::RemoveDoubleNot);
    }

    #[test]
    fn double_not_literal() {
        assert_action("!!42;", "!!42", SimplifyAction::RemoveDoubleNot);
    }

    #[test]
    fn double_not_call() {
        assert_action("!!foo();", "!!foo()", SimplifyAction::RemoveDoubleNot);
    }

    #[test]
    fn double_not_not_const() {
        // x is a variable, still pure (identifier) → RemoveDoubleNot
        assert_action("!!x;", "!!x", SimplifyAction::RemoveDoubleNot);
    }

    #[test]
    fn double_not_non_pure_skipped() {
        // !!a++  — a++ is not pure (Update), so no simplification
        assert_action_count("!!a++;", 0);
    }

    // ── Not-constant ──

    #[test]
    fn not_true() {
        assert_action(
            "!true;",
            "!true",
            SimplifyAction::ReplaceWith("false".into()),
        );
    }

    #[test]
    fn not_false() {
        assert_action(
            "!false;",
            "!false",
            SimplifyAction::ReplaceWith("true".into()),
        );
    }

    #[test]
    fn not_zero() {
        assert_action("!0;", "!0", SimplifyAction::ReplaceWith("true".into()));
    }

    #[test]
    fn not_one() {
        assert_action("!1;", "!1", SimplifyAction::ReplaceWith("false".into()));
    }

    #[test]
    fn not_undefined() {
        assert_action(
            "!undefined;",
            "!undefined",
            SimplifyAction::ReplaceWith("true".into()),
        );
    }

    #[test]
    fn not_null() {
        assert_action(
            "!null;",
            "!null",
            SimplifyAction::ReplaceWith("true".into()),
        );
    }

    #[test]
    fn not_empty_string() {
        assert_action(
            "!\"\";",
            "!\"\"",
            SimplifyAction::ReplaceWith("true".into()),
        );
    }

    // ── Ternary on constant ──

    #[test]
    fn ternary_true() {
        assert_action(
            "true ? x : y;",
            "true ? x : y",
            SimplifyAction::ReplaceWith("x".into()),
        );
    }

    #[test]
    fn ternary_false() {
        assert_action(
            "false ? x : y;",
            "false ? x : y",
            SimplifyAction::ReplaceWith("y".into()),
        );
    }

    #[test]
    fn ternary_true_side_effect_free() {
        assert_action(
            "true ? a + b : c * d;",
            "true ? a + b : c * d",
            SimplifyAction::ReplaceWith("a + b".into()),
        );
    }

    #[test]
    fn ternary_non_const_test() {
        assert_no_action("x ? a : b;", "x ? a : b");
    }

    // ── Logical on constant ──

    #[test]
    fn logical_and_true() {
        assert_action(
            "true && x;",
            "true && x",
            SimplifyAction::ReplaceWith("x".into()),
        );
    }

    #[test]
    fn logical_and_false() {
        assert_action(
            "false && x;",
            "false && x",
            SimplifyAction::ReplaceWith("false".into()),
        );
    }

    #[test]
    fn logical_or_true() {
        assert_action(
            "true || x;",
            "true || x",
            SimplifyAction::ReplaceWith("true".into()),
        );
    }

    #[test]
    fn logical_or_false() {
        assert_action(
            "false || x;",
            "false || x",
            SimplifyAction::ReplaceWith("x".into()),
        );
    }

    #[test]
    fn logical_and_true_complex_right() {
        // The right operand span excludes parentheses — they are syntactic grouping
        assert_action(
            "true && (a + b);",
            "true && (a + b)",
            SimplifyAction::ReplaceWith("a + b".into()),
        );
    }

    // ── Bracket to dot ──

    #[test]
    fn bracket_to_dot_simple() {
        assert_action("a['b'];", "a['b']", SimplifyAction::BracketToDot);
    }

    #[test]
    fn bracket_to_dot_multi_char() {
        assert_action(
            "obj['propName'];",
            "obj['propName']",
            SimplifyAction::BracketToDot,
        );
    }

    #[test]
    fn bracket_to_dot_underscore() {
        assert_action("a['_foo'];", "a['_foo']", SimplifyAction::BracketToDot);
    }

    #[test]
    fn bracket_to_dot_dollar() {
        assert_action("a['$'];", "a['$']", SimplifyAction::BracketToDot);
    }

    #[test]
    fn bracket_to_dot_not_valid_ident() {
        // 'a-b' is not a valid identifier
        assert_no_action("a['a-b'];", "a['a-b']");
    }

    #[test]
    fn bracket_to_dot_digits_only() {
        // '123' is not a valid identifier (starts with digit)
        assert_no_action("a['123'];", "a['123']");
    }

    #[test]
    fn bracket_to_dot_computed_not_string() {
        // a[b] — computed with variable, not a string literal
        assert_no_action("a[b];", "a[b]");
    }

    // ── String concat folding ──

    #[test]
    fn str_concat() {
        assert_action(
            "\"a\" + \"b\";",
            "\"a\" + \"b\"",
            SimplifyAction::ReplaceWith("\"ab\"".into()),
        );
    }

    #[test]
    fn str_concat_multi() {
        assert_action(
            "\"a\" + \"b\" + \"c\";",
            "\"a\" + \"b\" + \"c\"",
            SimplifyAction::ReplaceWith("\"abc\"".into()),
        );
    }

    #[test]
    fn str_concat_with_number() {
        assert_action(
            "\"a\" + 1;",
            "\"a\" + 1",
            SimplifyAction::ReplaceWith("\"a1\"".into()),
        );
    }

    // ── Arithmetic folding ──

    #[test]
    fn arithmetic_add() {
        assert_action("2 + 3;", "2 + 3", SimplifyAction::ReplaceWith("5".into()));
    }

    #[test]
    fn arithmetic_sub() {
        assert_action("10 - 3;", "10 - 3", SimplifyAction::ReplaceWith("7".into()));
    }

    #[test]
    fn arithmetic_mul() {
        assert_action("7 * 8;", "7 * 8", SimplifyAction::ReplaceWith("56".into()));
    }

    #[test]
    fn arithmetic_div() {
        assert_action("10 / 2;", "10 / 2", SimplifyAction::ReplaceWith("5".into()));
    }

    #[test]
    fn arithmetic_complex() {
        assert_action(
            "2 + 3 * 4;",
            "2 + 3 * 4",
            SimplifyAction::ReplaceWith("14".into()),
        );
    }

    // ── Template literal ──

    #[test]
    fn template_no_expr() {
        assert_action(
            "`hello`;",
            "`hello`",
            SimplifyAction::ReplaceWith("\"hello\"".into()),
        );
    }

    #[test]
    fn template_all_const_exprs() {
        assert_action(
            "`hello ${1 + 2}`;",
            "`hello ${1 + 2}`",
            SimplifyAction::ReplaceWith("\"hello 3\"".into()),
        );
    }

    #[test]
    fn template_not_simplifiable() {
        assert_no_action("`hello ${name}`;", "`hello ${name}`");
    }

    #[test]
    fn template_all_const_multiple() {
        assert_action(
            "`${1} + ${2} = ${3}`;",
            "`${1} + ${2} = ${3}`",
            SimplifyAction::ReplaceWith("\"1 + 2 = 3\"".into()),
        );
    }

    // ── void 0 stays as is ──

    #[test]
    fn void_zero_unchanged() {
        assert_action_count("void 0;", 0);
    }

    #[test]
    fn void_any_unchanged() {
        assert_action_count("void x;", 0);
    }

    // ── No false positives ──

    #[test]
    fn no_false_positive_ident() {
        assert_action_count("x;", 0);
    }

    #[test]
    fn no_false_positive_call() {
        assert_action_count("foo();", 0);
    }

    #[test]
    fn no_false_positive_assignment() {
        assert_action_count("x = 42;", 0);
    }

    #[test]
    fn no_false_positive_non_const_ternary() {
        assert_no_action("a ? b : c;", "a ? b : c");
    }

    // ── Edge cases ──

    #[test]
    fn nested_double_not() {
        // !!(!true) -> !!0..9, inner !true at 3..8
        let sp = plan("!!(!true);");
        assert_eq!(
            sp.actions.get(&Span::new(0, 9)),
            Some(&SimplifyAction::RemoveDoubleNot)
        );
        assert_eq!(
            sp.actions.get(&Span::new(3, 8)),
            Some(&SimplifyAction::ReplaceWith("false".into()))
        );
    }

    #[test]
    fn multiple_simplifications() {
        let sp = plan("!!x;!true;true?y:z;");
        assert_eq!(sp.actions.len(), 3);
    }

    #[test]
    fn is_valid_ident_checks() {
        assert!(super::is_valid_ident("x"));
        assert!(super::is_valid_ident("_foo"));
        assert!(super::is_valid_ident("$"));
        assert!(super::is_valid_ident("prop2"));
        assert!(!super::is_valid_ident(""));
        assert!(!super::is_valid_ident("2abc"));
        assert!(!super::is_valid_ident("a-b"));
        assert!(!super::is_valid_ident("a.b"));
    }
}
