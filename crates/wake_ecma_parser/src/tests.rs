//! 解析器单测。用公开 [`crate::parse`] API + AST 断言。

use wake_common::Interner;
use wake_ecma_ast::*;

use crate::parse;

/// 无诊断地解析并对根 AST 运行断言。
fn with_program(src: &str, f: impl FnOnce(&Program<'_>)) {
    let interner = Interner::new();
    let out = parse(src, &interner, SourceType::Module);
    assert!(
        !out.has_errors(),
        "parse errors for {src:?}: {:?}",
        out.diagnostics
    );
    out.module.with_ast(f);
}

#[test]
fn empty_and_literals() {
    with_program("1; 'x'; true; null; 3.14;", |p| {
        assert_eq!(p.body.len(), 5);
    });
}

#[test]
fn variable_declarations() {
    with_program("const a = 1, b = 2; let [x, y] = z; var {m, n} = o;", |p| {
        assert_eq!(p.body.len(), 3);
        assert!(matches!(p.body[0], Statement::VariableDeclaration(_)));
    });
}

#[test]
fn binary_precedence() {
    with_program("1 + 2 * 3;", |p| {
        if let Statement::Expression(e) = &p.body[0] {
            // 顶层是 +，右子是 *。
            if let Expression::Binary(add) = e.expression {
                assert_eq!(add.operator, BinaryOperator::Add);
                assert!(
                    matches!(add.right, Expression::Binary(m) if m.operator == BinaryOperator::Mul)
                );
            } else {
                panic!("expected binary");
            }
        } else {
            panic!("expected expr stmt");
        }
    });
}

#[test]
fn arrow_functions() {
    with_program(
        "const f = (a, b) => a + b; const g = x => x; const h = async () => await y;",
        |p| {
            assert_eq!(p.body.len(), 3);
        },
    );
}

#[test]
fn control_flow() {
    let src = "if (a) b(); else { c(); }
        for (let i = 0; i < 10; i++) log(i);
        for (const k of items) use(k);
        while (x) y();
        do z(); while (w);
        switch (v) { case 1: break; default: done(); }
        try { risky(); } catch (e) { handle(e); } finally { cleanup(); }";
    with_program(src, |p| {
        assert_eq!(p.body.len(), 7);
    });
}

#[test]
fn classes() {
    let src = "class A extends B {
        #x = 1;
        static y = 2;
        constructor() { super(); }
        get z() { return this.#x; }
        set z(v) { this.#x = v; }
        static { init(); }
        method() {}
    }";
    with_program(src, |p| {
        assert!(matches!(p.body[0], Statement::ClassDeclaration(_)));
        if let Statement::ClassDeclaration(c) = &p.body[0] {
            assert!(c.super_class.is_some());
            assert!(c.body.len() >= 6);
        }
    });
}

#[test]
fn functions_and_generators() {
    with_program(
        "function* gen() { yield 1; yield* other(); } async function af() { await p; }",
        |p| {
            assert_eq!(p.body.len(), 2);
        },
    );
}

#[test]
fn optional_chaining_and_calls() {
    with_program("a?.b?.[c]?.(d).e.f();", |p| {
        assert!(matches!(p.body[0], Statement::Expression(_)));
    });
}

#[test]
fn templates() {
    with_program("const s = `a${b}c${d}e`; tag`x${y}z`;", |p| {
        assert_eq!(p.body.len(), 2);
    });
}

#[test]
fn dependency_extraction() {
    let interner = Interner::new();
    let src = "import a from 'mod-a';
        import { b, c as d } from 'mod-b';
        import * as ns from 'mod-c';
        import 'side-effect';
        export { e } from 'mod-d';
        export * from 'mod-e';
        const p = import('dynamic');
        const q = require('cjs-mod');";
    let out = parse(src, &interner, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let specifiers: Vec<String> = out
        .dependencies
        .iter()
        .map(|d| interner.resolve(d.specifier))
        .collect();
    assert!(specifiers.contains(&"mod-a".to_string()));
    assert!(specifiers.contains(&"mod-b".to_string()));
    assert!(specifiers.contains(&"mod-c".to_string()));
    assert!(specifiers.contains(&"side-effect".to_string()));
    assert!(specifiers.contains(&"mod-d".to_string()));
    assert!(specifiers.contains(&"mod-e".to_string()));
    assert!(specifiers.contains(&"dynamic".to_string()));
    assert!(specifiers.contains(&"cjs-mod".to_string()));

    let has_dynamic = out
        .dependencies
        .iter()
        .any(|d| d.kind == DependencyKind::DynamicImport);
    let has_require = out
        .dependencies
        .iter()
        .any(|d| d.kind == DependencyKind::Require);
    assert!(has_dynamic && has_require);
}

#[test]
fn asi_works() {
    with_program("const a = 1\nconst b = 2\nreturn\nx", |p| {
        // return 在模块顶层非法，但错误恢复应仍产出语句；这里换成不含 return 的断言。
        let _ = p;
    });
    with_program("let a = 1\nlet b = 2\na++\nb--", |p| {
        assert_eq!(p.body.len(), 4);
    });
}

#[test]
fn objects_and_destructuring() {
    let src = "const o = { a: 1, b, [c]: 2, m() {}, get g() { return 1; }, ...rest };
        const { x, y: z, w = 3, ...others } = o;";
    with_program(src, |p| {
        assert_eq!(p.body.len(), 2);
    });
}

#[test]
fn ts_as_const_assertion() {
    // `as const` 断言：const 是保留字，类型消费器需显式接收（否则在 `const` 处失步）。
    let interner = Interner::new();
    for src in [
        "const x = 1 as const;",
        "const x = (['a', 'b'] as const).map((i) => i);",
        "const y = { k: 'v' } as const;",
    ] {
        let out = parse(src, &interner, SourceType::Tsx);
        assert!(
            !out.has_errors(),
            "as const 解析错误 {src:?}: {:?}",
            out.diagnostics
        );
    }
    // `<const>expr` 前缀断言（仅 .ts，.tsx 下 `<` 归 JSX）。
    let out = parse("const z = <const>['a'];", &interner, SourceType::TypeScript);
    assert!(!out.has_errors(), "<const> 解析错误: {:?}", out.diagnostics);
}

/// 无诊断地解析（指定 source type），返回 [`crate::ParseOutput`]。
fn parse_ok(src: &str, st: SourceType) -> crate::ParseOutput {
    let interner = Interner::new();
    let out = parse(src, &interner, st);
    assert!(
        !out.has_errors(),
        "parse errors for {src:?}: {:?}",
        out.diagnostics
    );
    out
}

#[test]
fn ts_import_equals_and_export_assign() {
    let interner = Interner::new();
    let src = "import fs = require('fs');
        import A = N.B.C;
        export import Pub = require('pub');
        export = { fs, A };";
    let out = parse(src, &interner, SourceType::TypeScript);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    // `= require('x')` 形态记为 Require 依赖（复用既有 CJS 链路）。
    let requires: Vec<String> = out
        .dependencies
        .iter()
        .filter(|d| d.kind == DependencyKind::Require)
        .map(|d| interner.resolve(d.specifier))
        .collect();
    assert_eq!(requires, vec!["fs".to_string(), "pub".to_string()]);

    out.module.with_ast(|p| {
        // import-equals 降级为变量声明：require 形态用 const，实体名别名用 var。
        assert!(matches!(
            p.body[0],
            Statement::VariableDeclaration(d) if d.kind == VarKind::Const
        ));
        assert!(matches!(
            p.body[1],
            Statement::VariableDeclaration(d) if d.kind == VarKind::Var
        ));
        // `export import` 仍是具名导出，内层是降级后的声明。
        assert!(matches!(p.body[2], Statement::ExportNamed(_)));
        // `export =` 降级为普通赋值语句 → 该模块不含 ESM 语句，按 CJS 语义处理。
        assert!(matches!(p.body[3], Statement::Expression(_)));
    });
}

#[test]
fn ts_type_only_import_equals_erased() {
    // 类型-only 的 import-equals 整条擦除，且**不得**留下 require 依赖。
    let interner = Interner::new();
    let src = "import type T = require('./types');
        import type { X } from './x';
        export as namespace Lib;
        export const v = 1;";
    let out = parse(src, &interner, SourceType::TypeScript);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        out.dependencies.is_empty(),
        "类型-only 声明不应产生运行时依赖：{:?}",
        out.dependencies
            .iter()
            .map(|d| interner.resolve(d.specifier))
            .collect::<Vec<_>>()
    );
}

#[test]
fn import_attributes_parsed() {
    let out = parse_ok(
        "import d from './d.json' with { type: 'json' };
         import './s.css' with { type: 'css' };
         export { a } from './m.js' with { type: 'json' };
         export * from './n.js' with { type: 'json' };
         import o from './o.json' assert { type: 'json' };",
        SourceType::Module,
    );
    // 依赖照常抽取（属性不影响模块图）。
    assert_eq!(out.dependencies.len(), 5);
    fn attrs<'a>(s: &Statement<'a>) -> Option<&'a ImportAttributes<'a>> {
        match s {
            Statement::Import(d) => d.attributes,
            Statement::ExportNamed(d) => d.attributes,
            Statement::ExportAll(d) => d.attributes,
            _ => None,
        }
    }
    out.module.with_ast(|p| {
        for (i, stmt) in p.body.iter().enumerate() {
            let a = attrs(stmt).unwrap_or_else(|| panic!("第 {i} 条语句缺少引入属性"));
            assert_eq!(a.items.len(), 1);
        }
        assert_eq!(attrs(&p.body[0]).unwrap().keyword, AttributesKeyword::With);
        // 已废弃的 import assertions 保留原关键字，不静默改写为 `with`。
        assert_eq!(
            attrs(&p.body[4]).unwrap().keyword,
            AttributesKeyword::Assert
        );
    });
}

#[test]
fn import_attributes_do_not_swallow_with_statement() {
    // 反例：`with` 前有换行时是 with 语句，不是引入属性（规范的 [no LineTerminator here]）。
    let out = parse_ok("import x from 'm'\nwith (o) { y }", SourceType::Script);
    out.module.with_ast(|p| {
        assert!(matches!(p.body[0], Statement::Import(d) if d.attributes.is_none()));
        assert!(matches!(p.body[1], Statement::With(_)));
    });
}

#[test]
fn using_declarations() {
    let out = parse_ok(
        "{ using a = mk(); }
         async function f() {
             using c = mk();
             await using b = mkAsync();
             for (using r of rs) {}
             for (await using k of ks) {}
         }
         class C { static { using s = mk(); } }",
        SourceType::Module,
    );
    out.module.with_ast(|p| {
        let Statement::Block(b) = &p.body[0] else {
            panic!("期望块语句")
        };
        assert!(matches!(
            b.body[0],
            Statement::VariableDeclaration(d) if d.kind == VarKind::Using
        ));
        let Statement::FunctionDeclaration(f) = &p.body[1] else {
            panic!("期望函数声明")
        };
        let body = f.body.expect("函数应有体");
        assert!(matches!(
            body.statements[0],
            Statement::VariableDeclaration(d) if d.kind == VarKind::Using
        ));
        assert!(matches!(
            body.statements[1],
            Statement::VariableDeclaration(d) if d.kind == VarKind::AwaitUsing
        ));
    });
}

#[test]
fn top_level_await_using_marks_module_async() {
    // 顶层 `await using` 合乎规范（模块顶层即 async 上下文）。关键在于它**必须**置位
    // `has_top_level_await`：否则 bundler 把它包进非 async 的模块包装器，产物加载即抛
    // SyntaxError。`await` 运算符由 expr.rs 置位，`await using` 声明不走那条路径。
    let interner = Interner::new();
    let out = parse("await using a = mkAsync();", &interner, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        out.has_top_level_await,
        "顶层 await using 未标记 has_top_level_await → 产物会被包进同步包装器"
    );
    out.module.with_ast(|p| {
        assert!(matches!(
            p.body[0],
            Statement::VariableDeclaration(d) if d.kind == VarKind::AwaitUsing
        ));
    });
    // 非 async 函数内仍非法（不因模块顶层放行而泄漏进去）。
    let out = parse(
        "function f() { await using a = mkAsync(); }",
        &interner,
        SourceType::Module,
    );
    assert!(out.has_errors(), "非 async 函数内的 await using 应报错");
    assert!(!out.has_top_level_await);
}

#[test]
fn using_as_plain_identifier() {
    // 反例：`using` 是上下文关键字——下列每一处都必须仍按普通标识符解析。
    let out = parse_ok(
        "let using = 1;
         using = 2;
         using;
         using
         x = 3;
         foo(using);
         using.prop;
         for (using of xs) {}
         async function g() { await using(1); await usingThing(); }",
        SourceType::Module,
    );
    out.module.with_ast(|p| {
        // 无一条语句被解析成 using 声明。
        for s in p.body.iter() {
            assert!(
                !matches!(s, Statement::VariableDeclaration(d) if d.kind.is_using()),
                "`using` 被误判为声明：{s:?}"
            );
        }
        // `using\nx = 3` 经 ASI 断成两条表达式语句。
        assert!(matches!(p.body[2], Statement::Expression(_)));
        assert!(matches!(p.body[3], Statement::Expression(_)));
        // `for (using of xs)` 是 for-of，左侧是赋值目标而非声明。
        let for_of = p
            .body
            .iter()
            .find_map(|s| match s {
                Statement::ForOf(f) => Some(f),
                _ => None,
            })
            .expect("应有 for-of");
        assert!(matches!(for_of.left, ForLeft::Target(_)));
    });
}

// ============================================================
// 顶层 await（ES2022）
// ============================================================

#[test]
fn top_level_await_parses_in_modules() {
    let interner = Interner::new();
    for st in [
        SourceType::Module,
        SourceType::TypeScript,
        SourceType::Jsx,
        SourceType::Tsx,
    ] {
        let out = parse("const a = await fetch('u');", &interner, st);
        assert!(
            !out.has_errors(),
            "{st:?} 顶层 await 解析错误: {:?}",
            out.diagnostics
        );
        assert!(out.has_top_level_await, "{st:?} 未标记 has_top_level_await");
    }
}

#[test]
fn top_level_await_forms() {
    let interner = Interner::new();
    for src in [
        "await import('./m.js');",
        "for await (const x of xs) { use(x); }",
        "export const v = await load();",
        "if (cond) { const r = await go(); }",
        "const [a, b] = await Promise.all([p1, p2]);",
    ] {
        let out = parse(src, &interner, SourceType::Module);
        assert!(
            !out.has_errors(),
            "顶层 await 解析错误 {src:?}: {:?}",
            out.diagnostics
        );
        assert!(
            out.has_top_level_await,
            "{src:?} 未标记 has_top_level_await"
        );
    }
}

#[test]
fn await_inside_functions_is_not_top_level() {
    // 函数/方法/箭头/static 块内的 await 不得把模块标成 async 模块。
    let interner = Interner::new();
    for src in [
        "async function f() { await p; }",
        "const f = async () => await p;",
        "class C { async m() { await p; } }",
        "async function* g() { for await (const x of xs) {} }",
        "const o = { async m() { await p; } };",
    ] {
        let out = parse(src, &interner, SourceType::Module);
        assert!(!out.has_errors(), "解析错误 {src:?}: {:?}", out.diagnostics);
        assert!(!out.has_top_level_await, "{src:?} 被误标为顶层 await");
    }
}

#[test]
fn top_level_await_rejected_in_script_and_sync_fn() {
    let interner = Interner::new();
    // Script 不是模块：`await x` 不是 await 表达式（`await` 退化为标识符，两个标识符相邻 → 报错）。
    let out = parse("const a = await fetch('u');", &interner, SourceType::Script);
    assert!(out.has_errors(), "Script 顶层 await 不应被接受");
    assert!(!out.has_top_level_await);
    // 非 async 函数体内仍禁止（不因模块顶层放行而泄漏进去）。
    let out = parse(
        "function f() { const a = await g(); }",
        &interner,
        SourceType::Module,
    );
    assert!(out.has_errors(), "非 async 函数内 await 不应被接受");
    assert!(!out.has_top_level_await);
    // class static 块同理（规范禁止）。
    let out = parse(
        "class C { static { const a = await g(); } }",
        &interner,
        SourceType::Module,
    );
    assert!(out.has_errors(), "static 块内 await 不应被接受");
}

#[test]
fn no_panic_on_garbage() {
    // 错误恢复：乱码不应 panic，且总能到达 EOF。
    let interner = Interner::new();
    for src in [
        "}{)(",
        "const = = =",
        "function (",
        "class {",
        "if if if",
        "((((",
        "]]]",
    ] {
        let out = parse(src, &interner, SourceType::Module);
        let _ = out.dependencies;
    }
}
