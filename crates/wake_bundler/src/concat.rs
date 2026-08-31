//! Conservative AST facts used exclusively by the bundler's scope-concat wrapper policy.

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
}

/// Scan the exact parser tree used by the bundler.
///
/// This deliberately remains an over-approximation: `var` or `this` inside a nested function also
/// rejects the bare-block path. A false negative only costs bytes; a false positive could change
/// hoisting or receiver semantics across concatenated modules.
pub(crate) fn scan_concat_block_info(program: &Program<'_>) -> ConcatBlockInfo {
    struct Scan {
        has_var: bool,
        has_this: bool,
    }

    impl<'ast> Visit<'ast> for Scan {
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
    };
    scan.visit_program(program);

    ConcatBlockInfo {
        is_esm,
        block_safe: is_esm && !scan.has_var && !scan.has_this,
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
            let info = parsed.module.with_ast(scan_concat_block_info);
            assert_eq!(info.is_esm, expected_esm, "{name}");
            assert_eq!(info.block_safe, expected_safe, "{name}");
        }

        let interner = Interner::new();
        let parsed = parse("let value=1", &interner, SourceType::Script);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        let info = parsed.module.with_ast(scan_concat_block_info);
        assert!(!info.is_esm);
        assert!(!info.block_safe);
    }
}
