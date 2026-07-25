//! # Minification Context
//!
//! Aggregates all analysis side-tables into a single struct consumed by
//! `wake_ecma_codegen` during emission. This is how the minifier communicates
//! its decisions to the code generator without modifying the AST.

use wake_common::{Atom, FxHashMap, FxHashSet, Span};
use crate::const_eval::ConstVal;
use crate::HoistPlan;
use wake_ecma_ast::Expression;

/// Aggregated minification context: all analysis results available to codegen.
#[derive(Debug, Default)]
pub struct MinifyCtx<'a> {
    // ── From existing passes ──
    /// Compile-time constant substitutions (define table).
    pub defines: &'a [(&'a str, &'a str)],
    /// Mangling side-table (span → new identifier name).
    pub rename: Option<&'a FxHashMap<Span, Atom>>,

    // ── Constant folding (Phase 2.1) ──
    /// Expressions that resolve to known constant values.
    pub constants: FxHashMap<Span, ConstVal>,

    // ── Expression simplification (Phase 2.1) ──
    /// Spans where `!!x` should be simplified to `x`.
    pub double_not_spans: FxHashSet<Span>,
    /// Bracket notation to convert to dot: `a['b']` → `a.b`
    pub bracket_to_dot: FxHashMap<Span, String>,
    /// Expression-level replacements (ternary/logical branch inlining, etc.): span → replacement source.
    pub expression_replacements: FxHashMap<Span, String>,

    // ── Dead code elimination (Phase 2.2) ──
    /// Statements to skip during emission (from DcePlan).
    pub remove_spans: FxHashSet<Span>,

    // ── Variable elimination (Phase 2.3 / 2.4) ──
    /// Variables that are unused (decl can be removed or reduced to side effects only).
    pub unused_vars: FxHashSet<Atom>,
    /// Single-use pure variables: variable name → initializer expression to inline.
    pub inline_vars: FxHashMap<Atom, &'a Expression<'a>>,

    // ── Statement merging (Phase 3) ──
    /// Consecutive var declarations that can be merged.
    pub join_var_spans: Vec<(Span, Span)>,
    /// Consecutive expression statements that can be comma-merged.
    pub sequence_spans: Vec<(Span, Span)>,
    /// If-return optimization candidates.
    pub if_return_spans: Vec<IfReturnCandidate>,

    // ── Property mangling (Phase M5) ──
    /// Property name mangling side-table (span → new name).
    /// Built by `plan_prop_mangle`, consumed by codegen to shorten property
    /// names in member access expressions and object literal keys.
    pub prop_rename: Option<&'a FxHashMap<Span, Atom>>,

    // ── Scope hoisting (Phase 3.5) ──
    /// Plan for hoisting `var` declarations to the function top.
    pub hoist: HoistPlan,

    // ── Literal minification ──
    /// `true` if `undefined` is never shadowed in any scope → safe to emit `void 0`.
    pub no_undefined_shadow: bool,

    // ── Tree shaking integration ──
    /// Variable declarator spans whose export was tree-shaken away.
    /// Span-based matching avoids scoping issues (different from Atom-based `removed_export_vars`).
    pub removed_export_spans: FxHashSet<Span>,

    // ── Flags ──
    /// Compact output (skip newlines/indentation).
    pub minify: bool,
}

impl<'a> MinifyCtx<'a> {
    /// Populate context from DCE and variable analysis results.
    /// Populate context from Phase 3 statement-level analysis results.
    pub fn populate_stmts(
        &mut self,
        if_return: Vec<IfReturnCandidate>,
        join: Vec<(Span, Span)>,
        seq: Vec<(Span, Span)>,
    ) {
        self.if_return_spans = if_return;
        self.join_var_spans = join;
        self.sequence_spans = seq;
    }

    pub fn populate_from<'b>(
        &mut self,
        plan: &crate::DcePlan,
        var: &crate::VarAnalysis,
        init_map: &FxHashMap<Span, &'b Expression<'b>>,
    ) where
        'b: 'a,
    {
        self.remove_spans = plan.remove_spans.clone();
        self.unused_vars = var.unused_vars.clone();
        for (&name, &decl_span) in &var.inline_candidates {
            if let Some(&init) = init_map.get(&decl_span) {
                self.inline_vars.insert(name, init);
            }
        }
    }
}

/// Candidate for if-return optimization: `if (cond) return a; return b;`
#[derive(Debug, Clone)]
pub struct IfReturnCandidate {
    pub cond_span: Span,
    pub if_span: Span,
    pub return_span: Span,
    pub subsequent_return_span: Span,
}
