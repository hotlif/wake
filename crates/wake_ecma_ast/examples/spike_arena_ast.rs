//! Spike ① 可运行 demo（PLAN §0.5 / DESIGN §10.4）。
//!
//! 运行：`cargo run -p wake_ecma_ast --example spike_arena_ast`
//! miri：`cargo +nightly miri run -p wake_ecma_ast --example spike_arena_ast`
//!
//! 演示：arena 分配 AST + 自引用 `ModuleAst` 持有者 + `with_ast` 安全借用 + visitor 遍历。

use wake_common::Interner;
use wake_ecma_ast::visit::{Visit, walk_expression};
use wake_ecma_ast::{Expression, ModuleAst, Statement};

#[derive(Default)]
struct Printer {
    depth: usize,
}

impl<'a> Visit<'a> for Printer {
    fn visit_expression(&mut self, expr: &Expression<'a>) {
        let indent = "  ".repeat(self.depth);
        match expr {
            Expression::NumberLiteral(n) => println!("{indent}Num({})", n.value),
            Expression::Binary(b) => println!("{indent}Binary({})", b.operator.as_str()),
            Expression::Identifier(i) => println!("{indent}Ident(#{})", i.name.as_u32()),
            other => println!("{indent}{:?}", std::mem::discriminant(other)),
        }
        self.depth += 1;
        walk_expression(self, expr);
        self.depth -= 1;
    }
}

fn main() {
    println!("== Spike ① arena AST + 自引用持有者 demo ==\n");

    let interner = Interner::new();

    // 构建 `let sum = 0 + 1 + 2 + 3 + 4;`，arena 与 AST 封装为 'static 持有者。
    let ast = ModuleAst::build_sample(&interner, 4);

    println!("语句数：{}", ast.statement_count());
    println!("结构指纹：{:#018x}\n", ast.structure_hash());

    // 经安全借用接口遍历内部 AST（arena 引用不逃出闭包）。
    ast.with_ast(|program| {
        println!("AST 遍历：");
        let mut printer = Printer { depth: 0 };
        if let Statement::VariableDeclaration(decl) = &program.body[0] {
            let d = &decl.declarations[0];
            if let (wake_ecma_ast::Pattern::Ident(id), Some(init)) = (d.id, &d.init) {
                interner.with_resolved(id.name, |s| println!("{} {s} =", decl.kind.as_str()));
                printer.depth = 1;
                printer.visit_expression(init);
            }
        }
    });

    // 持有者可移动、可放进 Vec/Arc，arena 随之同生共死。
    let holders: Vec<ModuleAst> = (0..3)
        .map(|d| ModuleAst::build_sample(&interner, d))
        .collect();
    let total: usize = holders.iter().map(|h| h.statement_count()).sum();
    println!("\n批量持有者语句总数：{total}（drop 时 arena 整块释放）");

    println!("\n结论：自引用持有者方案可行。详见 docs/spikes/spike-01-arena-ast.md");
}
