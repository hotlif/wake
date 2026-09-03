//! Conservative AST facts used exclusively by the bundler's scope-concat wrapper policy.

use wake_common::Interner;
use wake_ecma_ast::{
    Expression, Program, Statement, VarKind, Visit, walk_expression, walk_statement,
};

/// Whether one parsed module can participate in the minified bundle's bare-block concat path.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConcatBlockInfo {
    /// The AST contains ESM syntax. Only ESM modules may make the combined wrapper strict.
    pub(crate) is_esm: bool,
    /// ESM with no `var` and no `this` anywhere in the tree can use a bare block instead of an IIFE.
    pub(crate) block_safe: bool,
    /// A conservative parser-tree fact that the module observes or mutates CommonJS wrapper
    /// bindings. Hybrid ESM/CJS modules must keep an independent module/exports owner.
    pub(crate) observes_commonjs_bindings: bool,
}

/// Scan the exact parser tree used by the bundler.
///
/// This deliberately remains an over-approximation: `var` or `this` inside a nested function also
/// rejects the bare-block path. A false negative only costs bytes; a false positive could change
/// hoisting or receiver semantics across concatenated modules.
pub(crate) fn scan_concat_block_info(
    program: &Program<'_>,
    interner: &Interner,
) -> ConcatBlockInfo {
    struct Scan<'a> {
        has_var: bool,
        has_this: bool,
        observes_commonjs_bindings: bool,
        interner: &'a Interner,
    }

    impl<'ast> Visit<'ast> for Scan<'_> {
        fn visit_statement(&mut self, statement: &Statement<'ast>) {
            if let Statement::VariableDeclaration(declaration) = statement
                && declaration.kind == VarKind::Var
            {
                self.has_var = true;
            }
            walk_statement(self, statement);
        }

        fn visit_expression(&mut self, expression: &Expression<'ast>) {
            if matches!(expression, Expression::This(_)) {
                self.has_this = true;
            }
            if let Expression::Identifier(identifier) = expression
                && matches!(
                    self.interner.resolve(identifier.name).as_str(),
                    "module" | "exports"
                )
            {
                self.observes_commonjs_bindings = true;
            }
            if let Expression::Call(call) = expression
                && let Expression::Identifier(identifier) = call.callee
                && self.interner.resolve(identifier.name) == "eval"
            {
                self.observes_commonjs_bindings = true;
            }
            walk_expression(self, expression);
        }
    }

    let is_esm = program.body.iter().any(|statement| {
        matches!(
            statement,
            Statement::Import(_)
                | Statement::ExportNamed(_)
                | Statement::ExportDefault(_)
                | Statement::ExportAll(_)
        )
    });
    let mut scan = Scan {
        has_var: false,
        has_this: false,
        observes_commonjs_bindings: false,
        interner,
    };
    scan.visit_program(program);

    ConcatBlockInfo {
        is_esm,
        block_safe: is_esm && !scan.has_var && !scan.has_this,
        observes_commonjs_bindings: scan.observes_commonjs_bindings,
    }
}

#[cfg(test)]
mod tests {
    use wake_common::Interner;
    use wake_ecma_ast::SourceType;
    use wake_ecma_parser::parse;

    use super::scan_concat_block_info;

    #[test]
    fn concat_block_scan_preserves_conservative_wrapper_policy() {
        let cases = [
            ("safe esm", "export const value=1", true, true),
            ("top-level var", "export var value=1", true, false),
            (
                "nested var remains conservative",
                "export function value(){var nested=1;return nested}",
                true,
                false,
            ),
            (
                "nested this remains conservative",
                "export function value(){return this}",
                true,
                false,
            ),
            (
                "module source type without module syntax",
                "const value=1",
                false,
                false,
            ),
        ];

        for (name, source, expected_esm, expected_safe) in cases {
            let interner = Interner::new();
            let parsed = parse(source, &interner, SourceType::Module);
            assert!(!parsed.has_errors(), "{name}: {:?}", parsed.diagnostics);
            let info = parsed
                .module
                .with_ast(|program| scan_concat_block_info(program, &interner));
            assert_eq!(info.is_esm, expected_esm, "{name}");
            assert_eq!(info.block_safe, expected_safe, "{name}");
            assert!(!info.observes_commonjs_bindings, "{name}");
        }

        let interner = Interner::new();
        let parsed = parse("let value=1", &interner, SourceType::Script);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        let info = parsed
            .module
            .with_ast(|program| scan_concat_block_info(program, &interner));
        assert!(!info.is_esm);
        assert!(!info.block_safe);

        let parsed = parse(
            "export const value=eval('module.exports=1')",
            &interner,
            SourceType::Module,
        );
        let info = parsed
            .module
            .with_ast(|program| scan_concat_block_info(program, &interner));
        assert!(info.observes_commonjs_bindings);
    }
}
