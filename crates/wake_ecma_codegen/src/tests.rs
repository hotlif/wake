//! codegen 测试：往返幂等（parse→codegen→parse→codegen 稳定）+ 输出快照。

use wake_common::Interner;
use wake_ecma_ast::{SourceType, Statement};
use wake_ecma_parser::parse;

use crate::{ModuleSpecifierKind, ModuleSpecifierRewriter, PreserveModuleFormat, codegen};

fn optimize_fixture(
    program: &wake_ecma_ast::Program<'_>,
    interner: &Interner,
    input: &wake_ecma_minify::OptimizeInput<'_>,
) -> Result<wake_ecma_minify::OptimizedProgram, wake_ecma_minify::MinifyDiagnostic> {
    wake_ecma_minify::optimize(
        std::sync::Arc::new(wake_ecma_ast::clone_program_owned_with_source(
            program,
            std::sync::Arc::from(input.source),
            interner,
        )),
        interner,
        input,
    )
}

fn directive_helper_program(interner: &Interner) -> wake_ecma_ast::ModuleAst {
    use wake_common::Span;
    use wake_ecma_ast::{
        AVec, CallExpression, Expression, ExpressionStatement, Ident, ModuleAst, Program,
        SourceType, Statement, StringLiteral,
    };

    ModuleAst::from_builder(|arena| {
        let span_of = |needle: &str| {
            let start = DIRECTIVE_HELPER_SOURCE.find(needle).expect("fixture token") as u32;
            Span::new(start, start + needle.len() as u32)
        };
        let marker_span = span_of("'wake-prologue'");
        let strict_span = span_of("'use strict'");
        let keep_span = span_of("keep(__wake_iter,__wake_object,__wake_for_of)");
        let boot_span = span_of("boot()");
        let mut program = Program::new_in(arena, SourceType::Script);
        let mut keep_arguments = AVec::new_in(arena);
        for name in ["__wake_iter", "__wake_object", "__wake_for_of"] {
            keep_arguments.push(Expression::Identifier(
                arena.alloc(Ident::new(span_of(name), interner.intern(name))),
            ));
        }
        program
            .body
            .push(Statement::Expression(arena.alloc(ExpressionStatement {
                span: marker_span,
                expression: Expression::StringLiteral(arena.alloc(StringLiteral {
                    span: marker_span,
                    value: interner.intern("wake-prologue"),
                })),
            })));
        program
            .body
            .push(Statement::Expression(arena.alloc(ExpressionStatement {
                span: strict_span,
                expression: Expression::StringLiteral(arena.alloc(StringLiteral {
                    span: strict_span,
                    value: interner.intern("use strict"),
                })),
            })));
        program
            .body
            .push(Statement::Expression(arena.alloc(ExpressionStatement {
                span: keep_span,
                expression: Expression::Call(arena.alloc(CallExpression {
                    span: keep_span,
                    callee: Expression::Identifier(
                        arena.alloc(Ident::new(span_of("keep"), interner.intern("keep"))),
                    ),
                    arguments: keep_arguments,
                    optional: false,
                })),
            })));
        program
            .body
            .push(Statement::Expression(arena.alloc(ExpressionStatement {
                span: boot_span,
                expression: Expression::Call(arena.alloc(CallExpression {
                    span: boot_span,
                    callee: Expression::Identifier(
                        arena.alloc(Ident::new(boot_span, interner.intern("boot"))),
                    ),
                    arguments: AVec::new_in(arena),
                    optional: false,
                })),
            })));
        program.spread_helper = Some(interner.intern("__wake_iter"));
        program.object_spread_helper = Some(interner.intern("__wake_object"));
        program.for_of_helper = Some(interner.intern("__wake_for_of"));
        program.span = Span::new(0, DIRECTIVE_HELPER_SOURCE.len() as u32);
        program
    })
}

const DIRECTIVE_HELPER_SOURCE: &str =
    "'wake-prologue';'use strict';keep(__wake_iter,__wake_object,__wake_for_of);boot()";

fn emitted_top_level_function_names(source: &str) -> Vec<String> {
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Script);
    assert!(
        !parsed.has_errors(),
        "materialized helper output did not parse:\n{source}\n{:?}",
        parsed.diagnostics
    );
    parsed.module.with_ast(|program| {
        program
            .body
            .iter()
            .filter_map(|statement| {
                let Statement::FunctionDeclaration(function) = statement else {
                    return None;
                };
                function
                    .id
                    .map(|identifier| interner.resolve(identifier.name).to_owned())
            })
            .collect()
    })
}

fn for_of_helper_program(interner: &Interner) -> wake_ecma_ast::ModuleAst {
    use wake_ecma_ast::{ModuleAst, Program, SourceType};

    ModuleAst::from_builder(|arena| {
        let mut program = Program::new_in(arena, SourceType::Script);
        program.for_of_helper = Some(interner.intern("__wake_for_of"));
        program
    })
}

fn run(src: &str) -> String {
    let it = Interner::new();
    let out = parse(src, &it, SourceType::Module);
    assert!(
        !out.has_errors(),
        "parse errors for {src:?}: {:?}",
        out.diagnostics
    );
    out.module.with_ast(|p| codegen(p, &it))
}

/// TS 擦除：TypeScript 源码解析（跳过类型语法）→ codegen 出无类型 JS → 可作纯 JS 无错重解析。
fn strip_ts(src: &str) -> String {
    strip_typescript(src, SourceType::TypeScript)
}

fn strip_typescript(src: &str, source_type: SourceType) -> String {
    let it = Interner::new();
    let out = parse(src, &it, source_type);
    assert!(
        !out.has_errors(),
        "TS parse errors {src:?}: {:?}",
        out.diagnostics
    );
    let js = out.module.with_ast(|p| codegen(p, &it));
    let re = parse(&js, &it, SourceType::Module);
    assert!(
        !re.has_errors(),
        "擦除后 JS 重解析出错:\n{js}\n{:?}",
        re.diagnostics
    );
    js
}

#[test]
fn ts_erasure_basic() {
    let js = strip_ts(
        "const x: number = 1;\n\
         function add(a: number, b: number): number { return a + b; }\n\
         let y!: string;\n\
         function id<T>(v: T): T { return v; }\n\
         interface Foo { a: number; b(): void; }\n\
         type Bar = { x: number } | string;",
    );
    // 类型语法擦除。
    assert!(!js.contains(": number"), "残留类型注解:\n{js}");
    assert!(!js.contains("interface"), "残留 interface:\n{js}");
    assert!(!js.contains("<T>"), "残留泛型:\n{js}");
    // 值语义保留。
    assert!(js.contains("const x = 1"), "{js}");
    assert!(js.contains("function add(a, b)"), "{js}");
    assert!(js.contains("function id(v)"), "{js}");
    assert!(js.contains("return a + b"), "{js}");
}

#[test]
fn ts_erasure_expressions() {
    // as / satisfies / 非空断言 ! / import type。
    let js = strip_ts(
        "import type { T } from './t';\n\
         const a = 1;\n\
         const b = (a as number) + 1;\n\
         const c = a!;\n\
         const d = a satisfies number;\n\
         const e = a!.toString();",
    );
    assert!(!js.contains("as number"), "残留 as:\n{js}");
    assert!(!js.contains("satisfies"), "残留 satisfies:\n{js}");
    assert!(!js.contains("import type"), "残留 import type:\n{js}");
    assert!(js.contains("const b = a + 1"), "{js}");
    assert!(js.contains("const c = a;"), "{js}");
    assert!(js.contains("const d = a;"), "{js}");
    assert!(js.contains("const e = a.toString()"), "{js}");
}

#[test]
fn ts_erasure_full_type_grammar() {
    // 完整类型文法：联合/交叉、函数类型、条件类型、keyof/typeof、映射/对象/元组、
    // 索引访问、模板字面量类型、`import()` 类型、限定名 + 类型实参、字面量类型。
    let js = strip_ts(
        "type A = string | number & boolean;\n\
         type Fn = (a: number, b?: string) => void;\n\
         type Ctor = new (x: number) => Foo;\n\
         type Cond<T> = T extends string ? number : boolean;\n\
         type Keys = keyof typeof obj;\n\
         type Obj = { a: number; b?: string; readonly c: T[]; [k: string]: unknown };\n\
         type Mapped = { [K in Keys]: T[K] };\n\
         type Tup = [first: string, second?: number, ...rest: boolean[]];\n\
         type Idx = Foo['bar'][number];\n\
         type Tpl = `prefix-${string}-suffix`;\n\
         type Imp = import('react').FC<Props>;\n\
         type Ref = Map<string, Array<number>>;\n\
         type Lit = 'a' | 'b' | 42 | -1 | true;\n\
         type Nested = Array<Array<Map<K, V>>>;\n\
         const x: Ref = new Map();",
    );
    // 全部类型声明擦除为空；仅剩最后的 const。
    assert!(js.contains("const x = new Map()"), "{js}");
    assert!(!js.contains("type "), "残留 type 声明:\n{js}");
    assert!(!js.contains("extends"), "残留条件类型:\n{js}");
    assert!(!js.contains("keyof"), "残留 keyof:\n{js}");
    assert!(!js.contains(": number"), "残留类型注解:\n{js}");
    assert!(!js.contains("=>") || js.contains("new Map"), "{js}");
}

#[test]
fn ts_erasure_class_full() {
    // 类：泛型、extends<T>、implements、修饰符、可选/明确赋值、索引签名、重载、abstract/declare 成员、参数属性、this 参数。
    let js = strip_ts(
        "class C<T> extends Base<T> implements I, J<T> {\n\
           private readonly x: number = 1;\n\
           static y?: string;\n\
           declare z: boolean;\n\
           [key: string]: unknown;\n\
           foo(a: number): void;\n\
           foo(a: string): void;\n\
           foo(a: any): void { return; }\n\
           bar<U>(this: C<T>, u: U): U { return u; }\n\
           constructor(private a: number, public b: string) { super(); }\n\
           get val(): number { return this.x; }\n\
         }",
    );
    assert!(js.contains("class C extends Base"), "{js}");
    assert!(!js.contains("implements"), "残留 implements:\n{js}");
    assert!(!js.contains("private"), "残留修饰符:\n{js}");
    assert!(!js.contains("declare"), "残留 declare 成员:\n{js}");
    assert!(!js.contains(": number"), "残留类型:\n{js}");
    // 重载签名擦除，仅保留有体的实现。
    let foo_count = js.matches("foo(").count();
    assert!(
        foo_count <= 1,
        "重载签名未擦除（foo 出现 {foo_count} 次）:\n{js}"
    );
    assert!(
        js.contains("bar(u)"),
        "泛型方法 + this 参数未正确擦除:\n{js}"
    );
    assert!(js.contains("get val()"), "{js}");
}

#[test]
fn ts_erasure_declare_and_modules() {
    // declare 环境声明擦除 + import type / 内联 type / export type。
    let js = strip_ts(
        "declare const g: number;\n\
         declare function h(x: string): void;\n\
         declare namespace NS { const k: number; }\n\
         import type { T1 } from './t1';\n\
         import { type T2, real } from './t2';\n\
         export type { T3 } from './t3';\n\
         export { type T4, actual } from './t4';\n\
         const use = real + actual;",
    );
    assert!(!js.contains("declare"), "残留 declare:\n{js}");
    assert!(!js.contains("import type"), "残留 import type:\n{js}");
    assert!(
        !js.contains("T1") && !js.contains("T3"),
        "残留类型-only 名:\n{js}"
    );
    // 运行时导入保留。
    assert!(js.contains("real") && js.contains("actual"), "{js}");
}

#[test]
fn ts_7_erasure_matrix_reparses_as_javascript() {
    type ErasureCase<'case> = (
        &'case str,
        &'case str,
        SourceType,
        &'case [&'case str],
        &'case [&'case str],
    );

    let cases: [ErasureCase<'_>; 6] = [
        (
            "advanced types",
            "interface Row { readonly id: string; label: string }
             type Producer<out Value> = () => Value;
             type Consumer<in Value> = (value: Value) => void;
             type AwaitedValue<Value> = Value extends Promise<infer Inner> ? Inner : Value;
             type Getters<Value extends Row> = { [Key in keyof Value as `get${Capitalize<string & Key>}`]-?: () => Value[Key] };
             type Tuple = [head: string, count?: number, ...flags: boolean[]];
             type Imported = import('./types.js').Row;
             const rows = [{ id: 'a', label: 'A' }] as const satisfies readonly Row[];",
            SourceType::TypeScript,
            &["const rows = ["],
            &["interface Row", "type Producer", "satisfies", "as const"],
        ),
        (
            "functions and overloads",
            "interface Row { id: string }
             export function format(value: string): string;
             export function format(value: number): string;
             export function format(value: string | number): string { return String(value); }
             function assertRow(value: unknown): asserts value is Row { if (!value) throw Error(); }
             function collect<const Values extends readonly unknown[]>(values: Values): Values { return values; }
             const identity = <Value>(value: Value): Value => value;",
            SourceType::TypeScript,
            &["export function format(value)", "function collect(values)"],
            &["asserts", "value is Row", "<const Values", ": string"],
        ),
        (
            "classes",
            "interface Named { readonly name: string }
             abstract class Entity implements Named {
               abstract readonly name: string;
               constructor(public readonly id: string, private rank = 1) {}
             }
             class User extends Entity {
               accessor nickname: string = 'wake';
               constructor(id: string, public override readonly name: string) { super(id); }
             }
             declare class AmbientService { readonly ready: boolean; }",
            SourceType::TypeScript,
            &["class Entity", "class User extends Entity", "this.name = name"],
            &["implements", "abstract", "override", "declare class"],
        ),
        (
            "type-only modules",
            "import type { Row } from './row.js';
             import { type Config } from './config.js';
             export type { Result } from './result.js';
             export { type Options } from './options.js';
             export const ready: boolean = true;",
            SourceType::TypeScript,
            &["export const ready = true"],
            &["./row.js", "./config.js", "./result.js", "./options.js", ": boolean"],
        ),
        (
            "value-bearing TypeScript",
            "enum Color { Red, Green, Blue }
             namespace Metrics { export const base = 7; export function score(value: number): number { return base * value; } }
             class Point { constructor(public x: number, public y: number) {} total(): number { return this.x + this.y; } }
             export const result: number = Color.Blue + Metrics.score(2) + new Point(1, 2).total();",
            SourceType::TypeScript,
            &["Color", "Metrics", "this.x = x", "this.y = y"],
            &["enum Color", "namespace Metrics", ": number"],
        ),
        (
            "TSX generics",
            "interface Row { id: string; label: string }
             interface FormProps<Value> { value: Value }
             declare function Form<Value>(props: FormProps<Value>): unknown;
             const before = <div />;
             const constrained = <Value extends Row>(value: Value): string => value.label;
             const constant = <const Value extends Row>(value: Value): Value => value;
             const after = <Form<Row> value={{ id: 'row', label: 'TSX' }} />;",
            SourceType::Tsx,
            &["react/jsx-runtime", "value.label"],
            &["interface Row", "extends Row", ": Value", "<Form<Row>"],
        ),
    ];

    for (name, source, source_type, retained, erased) in cases {
        let js = strip_typescript(source, source_type);
        for fragment in retained {
            assert!(
                js.contains(fragment),
                "TypeScript 7 case {name:?} lost {fragment:?}:\n{js}"
            );
        }
        for fragment in erased {
            assert!(
                !js.contains(fragment),
                "TypeScript 7 case {name:?} retained {fragment:?}:\n{js}"
            );
        }
        if name == "functions and overloads" {
            assert_eq!(
                js.matches("function format(").count(),
                1,
                "overload signatures must erase while the implementation remains:\n{js}"
            );
        }
    }
}

#[test]
fn ts_decorators_lower_to_stage3() {
    // 装饰器降级为 TC39 Stage-3 形态（`__esDecorate` + `__runInitializers`，对齐 tsc）。
    // 参数装饰器 `@inject a` 在 Stage-3 中不存在，仍作擦除。
    let js = strip_ts(
        "@sealed\n\
         @Component({ selector: 'app' })\n\
         class C {\n\
           @readonly name: string = 'x';\n\
           @log method(@inject a: number): void { return; }\n\
         }",
    );
    assert!(!js.contains('@'), "残留装饰器语法:\n{js}");
    // 运行时辅助注入 + 类被包进 IIFE 并绑定
    assert!(js.contains("__esDecorate"), "应注入运行时辅助:\n{js}");
    assert!(js.contains("__runInitializers"), "{js}");
    assert!(js.contains("let C ="), "类声明应绑定到 IIFE 结果:\n{js}");
    // 各元素以正确 kind 登记
    assert!(js.contains("kind:\"class\""), "{js}");
    assert!(js.contains("kind:\"method\""), "{js}");
    assert!(js.contains("kind:\"field\""), "{js}");
    // 装饰器表达式保留（含带参调用）
    assert!(js.contains("sealed"), "{js}");
    assert!(js.contains("Component("), "{js}");
    // 方法体与字段初值仍在
    assert!(js.contains("method(a)"), "{js}");
    assert!(js.contains("\"x\""), "{js}");
}

#[test]
fn decorated_named_default_export_keeps_its_outer_binding() {
    let interner = Interner::new();
    let source = "function replace(value){return class extends value{static replaced=3}}\n\
                  @replace export default class Named{static original=2}\n\
                  globalThis.result=[Named.replaced,Named.original];globalThis.same=Named;";
    let parsed = parse(source, &interner, SourceType::TypeScript);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);

    let (esm, linked) = parsed.module.with_ast(|program| {
        let esm_optimized = optimize_fixture(
            program,
            &interner,
            &wake_ecma_minify::OptimizeInput::new(source),
        )
        .expect("decorated export should optimize");
        let mut linked_input = wake_ecma_minify::OptimizeInput::new(source);
        linked_input.set_bundled_commonjs(true);
        let linked_optimized = optimize_fixture(program, &interner, &linked_input)
            .expect("decorated export should optimize for the bundled module contract");
        (
            crate::codegen_preserved_optimized(
                &esm_optimized,
                &interner,
                PreserveModuleFormat::EsModule,
                &ExtensionRewriter(".mjs"),
            ),
            crate::codegen_optimized(&linked_optimized, &interner, &NoLinker, false),
        )
    });

    let export_marker = "export default ";
    let exported_name = esm
        .split_once(export_marker)
        .and_then(|(_, tail)| tail.split(';').next())
        .expect("preserved ESM should emit an explicit default binding");
    assert!(
        esm.contains(&format!("let {exported_name}=")),
        "decorated default class must create the outer local binding:\n{esm}"
    );
    let reparsed = parse(&esm, &interner, SourceType::Module);
    assert!(
        !reparsed.has_errors(),
        "decorated preserved ESM must reparse:\n{esm}\n{:?}",
        reparsed.diagnostics
    );

    let script = format!(
        "var exports={{}},__wake_require__={{objectDefineProperty:Object.defineProperty}};{linked};process.stdout.write(JSON.stringify([globalThis.result[0],globalThis.result[1],exports.default===globalThis.same]));"
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .expect("run decorated linked output");
    assert!(
        output.status.success() && output.stdout == br#"[3,2,true]"#,
        "decorated linked default export lost its binding or replacement:\n{linked}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn undecorated_class_is_unchanged() {
    // 无装饰器的类不得走降级路径（产物与既有一致）
    let js = strip_ts("class C { m() { return 1; } }");
    assert!(js.contains("class C"), "{js}");
    assert!(!js.contains("__esDecorate"), "不应注入辅助:\n{js}");
    assert!(!js.contains("=>{"), "不应包 IIFE:\n{js}");
}

#[test]
fn decorated_auto_accessor_is_fully_lowered_by_the_optimizer() {
    let source = "function dec(value){return value}class C{@dec accessor x=1}";
    let (js, mapped, _) = optimized_bundled_fixture(source, SourceType::TypeScript, true);
    assert_eq!(js, mapped);
    assert!(
        !js.contains('@') && !js.contains("accessor x"),
        "decorator/auto-accessor syntax survived optimizer lowering:\n{js}"
    );
    let reparsed = parse(&js, &Interner::new(), SourceType::Module);
    assert!(
        !reparsed.has_errors(),
        "lowered auto-accessor output did not reparse:\n{js}\n{:?}",
        reparsed.diagnostics
    );
}

#[test]
fn ts_namespace_desugars() {
    // namespace → IIFE；export 成员改写为 N.name = name；非导出保持局部；类型擦除。
    let js = strip_ts(
        "namespace Geo {\n\
           export const PI = 3.14;\n\
           interface Point { x: number; }\n\
           function helper(): void {}\n\
           export function area(r: number): number { return PI * r * r; }\n\
         }",
    );
    assert!(js.contains("var Geo = function"), "{js}");
    // export const → 局部 + N.PI = PI。
    assert!(js.contains("Geo.PI = PI"), "{js}");
    // export function → 局部 + N.area = area。
    assert!(js.contains("Geo.area = area"), "{js}");
    // 非导出 helper 保持局部（无 Geo.helper）。
    assert!(
        !js.contains("Geo.helper"),
        "非导出成员不应挂到命名空间:\n{js}"
    );
    // interface 擦除。
    assert!(!js.contains("interface") && !js.contains("Point"), "{js}");
    assert!(js.contains("return Geo"), "{js}");
}

#[test]
fn ts_parameter_properties() {
    // 参数属性 → 构造函数体注入 `this.x = x`（super 之后）。
    let js = strip_ts(
        "class A {\n\
           constructor(private x: number, public readonly y: string) {\n\
             super();\n\
             this.z = 1;\n\
           }\n\
         }\n\
         class B {\n\
           constructor(readonly a: number) {}\n\
         }",
    );
    // 修饰符擦除，形参保留。
    assert!(js.contains("constructor(x, y)"), "{js}");
    assert!(
        !js.contains("private") && !js.contains("readonly"),
        "残留修饰符:\n{js}"
    );
    // super 之后注入赋值。
    let super_pos = js.find("super()").unwrap();
    let assign_x = js.find("this.x = x").expect("缺少 this.x=x 注入");
    let assign_y = js.find("this.y = y").expect("缺少 this.y=y 注入");
    assert!(
        super_pos < assign_x && assign_x < assign_y,
        "注入顺序/位置错误:\n{js}"
    );
    // 无 super 的构造函数：体首注入。
    assert!(js.contains("this.a = a"), "{js}");
}

#[test]
fn ts_enum_desugars() {
    // enum → IIFE（正/反向映射 + 自增 + 字符串成员）。可作纯 JS 重解析。
    let js = strip_ts(
        "enum Color { Red, Green, Blue }\n\
         const enum Dir { Up = 1, Down }\n\
         enum S { A = \"a\", B = \"b\" }",
    );
    assert!(js.contains("var Color = function"), "{js}");
    // 数字成员正反向映射。
    assert!(js.contains("Color[Color[\"Red\"] = 0] = \"Red\""), "{js}");
    assert!(
        js.contains("Color[Color[\"Green\"] = 1] = \"Green\""),
        "{js}"
    );
    // 显式起始 + 自增。
    assert!(js.contains("Dir[Dir[\"Up\"] = 1] = \"Up\""), "{js}");
    assert!(js.contains("Dir[Dir[\"Down\"] = 2] = \"Down\""), "{js}");
    // 字符串成员仅正向。
    assert!(js.contains("S[\"A\"] = \"a\""), "{js}");
    assert!(
        !js.contains("S[S[\"A\"]"),
        "字符串枚举不应有反向映射:\n{js}"
    );
}

#[test]
fn ts_erasure_call_type_args() {
    // 调用表达式类型实参 `f<T>()`（useState/useRef 等 React 高频写法）擦除。
    let js = strip_ts(
        "const s = useState<number>(0);\n\
         const p = foo<A, B>(x);\n\
         const n = make<Map<string, number[]>>(x);\n\
         const m = obj.method<T>(a, b);\n\
         const t = tag<T>`hello`;",
    );
    assert!(!js.contains("<number>"), "残留调用类型实参:\n{js}");
    assert!(!js.contains("<A, B>"), "残留调用类型实参:\n{js}");
    assert!(!js.contains("<T>"), "残留调用类型实参:\n{js}");
    // 值语义保留：调用与实参不变。
    assert!(js.contains("useState(0)"), "{js}");
    assert!(js.contains("foo(x)"), "{js}");
    assert!(js.contains("make(x)"), "{js}");
    assert!(js.contains("obj.method(a, b)"), "{js}");
    assert!(js.contains("tag`hello`"), "{js}");
}

#[test]
fn ts_erasure_arrow_params_and_return() {
    // 箭头函数参数类型注解 + 返回类型注解擦除（真实 TSX 事件处理器/回调高频）。
    let js = strip_ts(
        "const f = (a: number, b: string): boolean => a > 0;\n\
         const g = (x: number) => x * 2;\n\
         const h = (...args: number[]): number => args.length;\n\
         const cb = arr.map((item: T, i: number) => item);",
    );
    assert!(!js.contains(": number"), "残留参数类型:\n{js}");
    assert!(!js.contains(": boolean"), "残留返回类型:\n{js}");
    assert!(js.contains("const f = (a, b) => a > 0"), "{js}");
    assert!(js.contains("const g = (x) => x * 2"), "{js}");
    assert!(js.contains("(...args) => args.length"), "{js}");
    assert!(js.contains("(item, i) => item"), "{js}");
}

#[test]
fn ts_erasure_arrow_return_type_not_conditional() {
    // 反例：条件表达式 `c ? (y) : z` 的 `:` 不能被箭头返回类型擦除误吃。
    let js = strip_ts("const r = c ? (y) : z;\nconst s = cond ? (a + b) : (d);");
    assert!(js.contains("c ? y : z"), "条件表达式被破坏:\n{js}");
    assert!(js.contains("? a + b :"), "条件表达式被破坏:\n{js}");
}

#[test]
fn ts_erasure_keeps_comparisons() {
    // 反例：真正的比较运算（`<` 后并非「平衡角括号 + 紧跟 `(`」）不得被误擦除。
    let js = strip_ts(
        "const a = x < y;\n\
         const b = x < y ? 1 : 2;\n\
         const c = p < q > r;\n\
         const d = fn(x < y, z > w);\n\
         const e = (x < y) && (z > w);",
    );
    assert!(js.contains("x < y"), "比较运算被误擦除:\n{js}");
    assert!(js.contains("p < q > r"), "比较运算被误擦除:\n{js}");
    assert!(js.contains("z > w"), "比较运算被误擦除:\n{js}");
}

/// JSX（.tsx）解析 → 降级为 automatic runtime 调用 → codegen 出可作纯 JS 重解析的产物。
fn jsx_codegen(src: &str) -> String {
    let it = Interner::new();
    let out = parse(src, &it, SourceType::Tsx);
    assert!(
        !out.has_errors(),
        "JSX parse errors {src:?}: {:?}",
        out.diagnostics
    );
    let js = out.module.with_ast(|p| codegen(p, &it));
    let re = parse(&js, &it, SourceType::Module);
    assert!(
        !re.has_errors(),
        "JSX 降级后重解析出错:\n{js}\n{:?}",
        re.diagnostics
    );
    js
}

#[test]
fn jsx_desugars_to_automatic_runtime() {
    let js = jsx_codegen(
        "const a = <div className=\"x\" data-id={1}>Hello</div>;\n\
         const b = <Foo a={1} b=\"c\">{child}</Foo>;\n\
         const c = <><span>1</span><span>2</span></>;\n\
         const d = <img src=\"u.png\" alt=\"a\" />;\n\
         const e = <List key={k} {...rest} />;",
    );
    // 顶部注入 react/jsx-runtime import。
    assert!(js.contains("react/jsx-runtime"), "{js}");
    // intrinsic 小写 → 字符串；组件 → 标识符。
    assert!(js.contains("_jsx(\"div\""), "{js}");
    assert!(js.contains("_jsx(Foo"), "{js}");
    assert!(js.contains("_jsx(\"img\""), "{js}");
    assert!(js.contains("_jsx(List"), "{js}");
    // 片段（多子节点）→ _jsxs(_Fragment。
    assert!(js.contains("_jsxs(_Fragment"), "{js}");
    // 连字符属性 → 字符串键。
    assert!(js.contains("\"data-id\""), "{js}");
    // children 归入 props。
    assert!(js.contains("children"), "{js}");
    // 不残留任何原始 JSX 尖括号语法。
    assert!(!js.contains("</"), "残留 JSX 闭合标签:\n{js}");
}

#[test]
fn jsx_nested_and_text() {
    let js = jsx_codegen(
        "const t = (\n  <ul className=\"list\">\n    <li>a &amp; b</li>\n    <li>{value}</li>\n  </ul>\n);",
    );
    // 嵌套：ul 有两个 li 子 → _jsxs("ul"。
    assert!(js.contains("_jsxs(\"ul\""), "{js}");
    assert!(js.contains("_jsx(\"li\""), "{js}");
    // 实体解码。
    assert!(js.contains("a & b"), "实体未解码:\n{js}");
    // 空白折叠：不应把缩进换行当作文本子节点。
    assert!(!js.contains("\\n"), "{js}");
}

/// M4b 用的最简 linker。
#[cfg(test)]
struct NoLinker;
#[cfg(test)]
impl crate::ModuleLinker for NoLinker {
    fn module_id(&self, _s: &str, _kind: crate::ModuleRequestKind) -> Option<u32> {
        None
    }
}

struct RuntimeImportLinker;

impl crate::ModuleLinker for RuntimeImportLinker {
    fn module_id(&self, _specifier: &str, _kind: crate::ModuleRequestKind) -> Option<u32> {
        None
    }

    fn runtime_dynamic_import(&self, specifier: &str) -> Option<String> {
        specifier
            .starts_with("catalog/")
            .then(|| specifier.to_owned())
    }

    fn runtime_dynamic_import_expose(&self, specifier: &str) -> Option<String> {
        specifier
            .starts_with("catalog/")
            .then(|| "./Widget".to_owned())
    }

    fn runtime_shared_module(
        &self,
        specifier: &str,
        _kind: crate::ModuleRequestKind,
    ) -> Option<(String, String)> {
        (specifier == "react").then(|| (specifier.to_owned(), "react18".to_owned()))
    }
}

#[test]
fn bundled_dynamic_import_can_be_owned_by_the_runtime() {
    let source = "export async function load(){return import('catalog/Button')}";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let mut input = wake_ecma_minify::OptimizeInput::new(source);
    input.set_bundled_commonjs(true);
    let optimized = parsed.module.with_ast(|program| {
        optimize_fixture(program, &interner, &input)
            .expect("runtime import fixture should optimize")
    });

    let js = crate::codegen_optimized(&optimized, &interner, &RuntimeImportLinker, false);

    assert!(
        js.contains("__wake_require__.runtimeImport(\"catalog/Button\", \"./Widget\")")
            || js.contains("__wake_require__.runtimeImport(\"catalog/Button\",\"./Widget\")"),
        "{js}"
    );
    assert!(!js.contains("import(\"catalog/Button\")"), "{js}");
}

#[test]
fn runtime_import_hook_does_not_capture_static_imports() {
    let source = "import Button from 'catalog/Button';export default Button";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let mut input = wake_ecma_minify::OptimizeInput::new(source);
    input.set_bundled_commonjs(true);
    let optimized = parsed.module.with_ast(|program| {
        optimize_fixture(program, &interner, &input).expect("static import fixture should optimize")
    });

    let js = crate::codegen_optimized(&optimized, &interner, &RuntimeImportLinker, false);

    assert!(
        js.contains("__wake_require__.external(\"catalog/Button\")"),
        "{js}"
    );
    assert!(!js.contains("runtimeImport"), "{js}");
}

#[test]
fn bundled_static_import_can_be_owned_by_a_runtime_share_context() {
    let source = "import React from 'react';export const version=React.version";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let mut input = wake_ecma_minify::OptimizeInput::new(source);
    input.set_bundled_commonjs(true);
    let optimized = parsed.module.with_ast(|program| {
        optimize_fixture(program, &interner, &input).expect("shared import fixture should optimize")
    });

    let js = crate::codegen_optimized(&optimized, &interner, &RuntimeImportLinker, false);

    assert!(
        js.contains("__wake_require__.shared(\"react\",\"react18\")")
            || js.contains("__wake_require__.shared(\"react\", \"react18\")"),
        "{js}"
    );
    assert!(!js.contains("require(\"react\")"), "{js}");
}

fn emit_optimized_linked(
    program: &wake_ecma_ast::Program<'_>,
    interner: &Interner,
    source: &str,
    minify: bool,
    no_esmodule: bool,
) -> String {
    let mut input = wake_ecma_minify::OptimizeInput::new(source);
    input.minify = minify;
    input.set_bundled_commonjs(true);
    let optimized = optimize_fixture(program, interner, &input)
        .expect("optimizer should accept codegen fixture");
    crate::codegen_optimized(&optimized, interner, &NoLinker, no_esmodule)
}

fn emit_optimized_linked_with_map(
    program: &wake_ecma_ast::Program<'_>,
    interner: &Interner,
    source: &str,
    minify: bool,
    no_esmodule: bool,
) -> (String, crate::ModuleMappings) {
    let mut input = wake_ecma_minify::OptimizeInput::new(source);
    input.minify = minify;
    input.set_bundled_commonjs(true);
    let optimized = optimize_fixture(program, interner, &input)
        .expect("optimizer should accept mapped codegen fixture");
    crate::codegen_optimized_with_map(&optimized, interner, &NoLinker, no_esmodule)
}

fn codegen_with_optimizer_defines(
    source: &str,
    source_type: SourceType,
    defines: &[(&str, &str)],
    minify: bool,
) -> String {
    codegen_with_optimizer_defines_and_map(source, source_type, defines, minify).0
}

fn codegen_with_optimizer_defines_and_map(
    source: &str,
    source_type: SourceType,
    defines: &[(&str, &str)],
    minify: bool,
) -> (String, String) {
    let interner = Interner::new();
    let parsed = parse(source, &interner, source_type);
    assert!(
        !parsed.has_errors(),
        "parse errors: {:?}",
        parsed.diagnostics
    );
    parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.minify = minify;
        input.set_bundled_commonjs(true);
        input.defines = defines
            .iter()
            .map(|(key, value)| match *value {
                "true" => wake_ecma_minify::ValidatedDefine::primitive(
                    *key,
                    wake_ecma_minify::ConstVal::Bool(true),
                ),
                "false" => wake_ecma_minify::ValidatedDefine::primitive(
                    *key,
                    wake_ecma_minify::ConstVal::Bool(false),
                ),
                quoted if quoted.starts_with('"') && quoted.ends_with('"') => {
                    wake_ecma_minify::ValidatedDefine::primitive(
                        *key,
                        wake_ecma_minify::ConstVal::Str(quoted[1..quoted.len() - 1].to_string()),
                    )
                }
                numeric if numeric.parse::<f64>().is_ok() => {
                    wake_ecma_minify::ValidatedDefine::primitive(
                        *key,
                        wake_ecma_minify::ConstVal::Num(
                            numeric.parse::<f64>().expect("numeric define"),
                        ),
                    )
                }
                source => {
                    let replacement = parse(source, &interner, SourceType::Module);
                    assert!(
                        !replacement.has_errors(),
                        "define expression did not parse: {source:?}: {:?}",
                        replacement.diagnostics
                    );
                    wake_ecma_minify::ValidatedDefine::expression(
                        *key,
                        wake_ecma_minify::TrustedExpression::from_parsed_program(
                            &replacement.module,
                            &interner,
                        ),
                    )
                }
            })
            .collect();
        let optimized = optimize_fixture(program, &interner, &input).expect("optimizer defines");
        let plain = crate::codegen_optimized(&optimized, &interner, &NoLinker, false);
        let (mapped, _) =
            crate::codegen_optimized_with_map(&optimized, &interner, &NoLinker, false);
        (plain, mapped)
    })
}

#[test]
fn program_helpers_follow_directive_prologue() {
    let interner = Interner::new();
    let module = directive_helper_program(&interner);
    let js = module.with_ast(|program| codegen(program, &interner));

    let marker = js.find("\"wake-prologue\";").expect("marker directive");
    let strict = js.find("\"use strict\";").expect("strict directive");
    let iterator = js
        .find("function __wake_iter(value, limit)")
        .expect("iterator helper");
    let object = js
        .find("function __wake_object(target)")
        .expect("object helper");
    let for_of = js
        .find("function __wake_for_of(value)")
        .expect("for-of helper");
    let boot = js.find("boot();").expect("source body");
    assert_eq!(marker, 0, "directive prologue must remain first:\n{js}");
    assert!(
        marker < strict
            && strict < iterator
            && iterator < object
            && object < for_of
            && for_of < boot,
        "helpers must be emitted between directives and the source body:\n{js}"
    );
}

#[test]
fn for_of_helper_is_lazy_and_implements_iterator_close() {
    let interner = Interner::new();
    let module = for_of_helper_program(&interner);
    let helper = module.with_ast(|program| codegen(program, &interner));

    assert!(
        helper.starts_with("function __wake_for_of(value)"),
        "{helper}"
    );
    assert!(
        helper.contains("state = {")
            && helper.contains("s: function()")
            && helper.contains("n: function()")
            && helper.contains("e: function(caught)")
            && helper.contains("f: function()")
            && helper.contains("v: void 0"),
        "the helper must expose the stable {{s,n,e,f,v}} state interface:\n{helper}"
    );
    assert!(
        helper.contains("next = iterator.next")
            && helper.contains("next.call(iterator)")
            && helper.contains("var done = step.done")
            && helper.contains("state.v = step.value"),
        "iterator operations must be captured/read exactly at their state-machine boundary:\n{helper}"
    );
    assert!(
        helper.contains("var returnMethod = iterator.return")
            && helper.contains("returnMethod.call(iterator)")
            && helper.contains("finally {")
            && helper.contains("if (hasError) throw error"),
        "IteratorClose must preserve a saved body exception:\n{helper}"
    );
    assert!(
        !helper.contains("Array.isArray") && !helper.contains("slice("),
        "for-of must require the iterator protocol instead of an array fallback:\n{helper}"
    );

    let node_available = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if !node_available {
        return;
    }

    let checks = r#"
function assert(value, message) { if (!value) throw new Error(message); }
var methodGets = 0, nextGets = 0, nextCalls = 0, doneGets = 0, valueGets = 0, returns = 0;
var iterable = {};
Object.defineProperty(iterable, Symbol.iterator, {
  get: function() {
    methodGets++;
    return function() {
      var iterator = {
        return: function() { returns++; return {}; }
      };
      Object.defineProperty(iterator, "next", {
        get: function() {
          nextGets++;
          return function() {
            nextCalls++;
            assert(this === iterator, "next receiver");
            var step = {};
            Object.defineProperty(step, "done", {
              get: function() { doneGets++; return nextCalls > 1; }
            });
            Object.defineProperty(step, "value", {
              get: function() { valueGets++; return 42; }
            });
            return step;
          };
        }
      });
      return iterator;
    };
  }
});
var state = __wake_for_of(iterable);
assert(Object.keys(state).join(",") === "s,n,e,f,v", "state keys");
assert(methodGets === 0 && nextGets === 0, "construction must be lazy");
state.s();
assert(methodGets === 1 && nextGets === 1, "GetIterator/next capture count");
assert(state.n() === false && state.v === 42, "first step");
assert(state.n() === true, "done step");
assert(doneGets === 2 && valueGets === 1, "done/value read count");
state.f();
assert(returns === 0, "completed iterator must not close");

var bodyError = new Error("body");
var closeCalls = 0;
var closing = {};
closing[Symbol.iterator] = function() {
  return {
    next: function() { return { done: false, value: 1 }; },
    return: function() { closeCalls++; throw new Error("close"); }
  };
};
var closingState = __wake_for_of(closing), thrown;
try {
  closingState.s();
  closingState.n();
  try { throw bodyError; } catch (caught) { closingState.e(caught); }
  finally { closingState.f(); }
} catch (caught) { thrown = caught; }
assert(thrown === bodyError && closeCalls === 1, "body error must override close error");

var primitiveClose = {};
primitiveClose[Symbol.iterator] = function() {
  return { next: function() { return { done: false, value: 1 }; }, return: function() { return 0; } };
};
var primitiveState = __wake_for_of(primitiveClose), primitiveError;
primitiveState.s(); primitiveState.n();
try { primitiveState.f(); } catch (caught) { primitiveError = caught; }
assert(primitiveError instanceof TypeError, "return result must be an object");

function startError(value) {
  var candidate = __wake_for_of(value);
  try { candidate.s(); } catch (caught) { return caught; }
}
assert(startError(null) instanceof TypeError, "null");
assert(startError(void 0) instanceof TypeError, "undefined");
assert(startError({}) instanceof TypeError, "missing iterator");
var nonObjectIterator = {};
nonObjectIterator[Symbol.iterator] = function() { return 1; };
assert(startError(nonObjectIterator) instanceof TypeError, "iterator object");
var invalidStep = {};
invalidStep[Symbol.iterator] = function() { return { next: function() { return 1; } }; };
var invalidState = __wake_for_of(invalidStep), invalidError;
invalidState.s();
try { invalidState.n(); } catch (caught) { invalidError = caught; }
assert(invalidError instanceof TypeError, "IteratorResult object");

var valueReturnCalls = 0, valueFailure = {};
valueFailure[Symbol.iterator] = function() {
  return {
    next: function() {
      return { done: false, get value() { throw new Error("value"); } };
    },
    return: function() { valueReturnCalls++; return {}; }
  };
};
var valueState = __wake_for_of(valueFailure), valueError;
valueState.s();
try { valueState.n(); } catch (caught) { valueError = caught; }
valueState.f();
assert(valueError && valueError.message === "value", "IteratorValue error");
assert(valueReturnCalls === 0, "IteratorValue failure must not close a done iterator record");

var savedSymbol = Symbol;
globalThis.Symbol = void 0;
assert(startError([]) instanceof TypeError, "missing Symbol must not use an array fallback");
globalThis.Symbol = savedSymbol;
process.stdout.write("OK");
"#;
    let script = format!("{helper}\n{checks}");
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .expect("run node");
    assert!(
        output.status.success() && output.stdout == b"OK",
        "for-of helper runtime failed: {}\n{helper}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn minify_keeps_directives_and_breaks_incoming_sequence_at_helpers() {
    use wake_common::Span;

    let interner = Interner::new();
    let module = directive_helper_program(&interner);
    let source = DIRECTIVE_HELPER_SOURCE;
    let js = module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.minify = true;
        input.set_bundled_commonjs(true);
        input.extend_statement_removals([Span::new(0, 15), Span::new(16, 28)]);
        let optimized = optimize_fixture(program, &interner, &input)
            .expect("trusted directive removals should optimize");
        crate::codegen_optimized(&optimized, &interner, &NoLinker, false)
    });

    assert!(
        js.starts_with("\"wake-prologue\";\"use strict\";"),
        "minify must preserve the complete directive prologue:\n{js}"
    );
    let helper = js.find("function __wake_").expect("materialized helper");
    let boot = js.find("boot();").expect("source body");
    assert!(helper < boot, "helpers must precede the source body:\n{js}");
    assert!(
        js.contains("boot();"),
        "the first body expression must not be skipped as an incoming sequence successor:\n{js}"
    );
}

#[test]
fn helper_insertion_keeps_statement_source_map_positions() {
    let interner = Interner::new();
    let module = directive_helper_program(&interner);
    let source = DIRECTIVE_HELPER_SOURCE;
    let (js, map) = module.with_ast(|program| {
        emit_optimized_linked_with_map(program, &interner, source, false, false)
    });

    let marker = map
        .mappings
        .iter()
        .find(|mapping| mapping.src_offset == 0)
        .expect("marker mapping");
    let strict = map
        .mappings
        .iter()
        .find(|mapping| mapping.src_offset == 16)
        .expect("strict mapping");
    let boot = map
        .mappings
        .iter()
        .find(|mapping| {
            mapping.src_offset == DIRECTIVE_HELPER_SOURCE.find("boot").expect("boot token") as u32
        })
        .expect("body mapping");
    assert_eq!((marker.gen_line, marker.gen_col), (0, 0));
    assert_eq!((strict.gen_line, strict.gen_col), (1, 0));
    assert!(
        boot.gen_line > strict.gen_line + 2,
        "body mapping must account for injected helper lines: {map:?}\n{js}"
    );
    let generated_line = js
        .lines()
        .nth(boot.gen_line as usize)
        .expect("mapped generated line");
    assert!(generated_line.starts_with("boot();"), "{map:?}\n{js}");
}

#[test]
fn decorator_helpers_follow_directive_prologue() {
    let js = strip_ts("\"wake-prologue\";\"use strict\";@dec class C {}");
    let marker = js.find("\"wake-prologue\";").expect("marker directive");
    let strict = js.find("\"use strict\";").expect("strict directive");
    let run = js
        .find("var __runInitializers")
        .expect("decorator initializer helper");
    let decorate = js.find("var __esDecorate").expect("decorator helper");
    let class = js.find("let C =").expect("lowered class");
    assert_eq!(marker, 0, "{js}");
    assert!(
        marker < strict && strict < run && run < decorate && decorate < class,
        "decorator helpers must follow all source directives:\n{js}"
    );
}

#[test]
fn m4b_dead_branch_strips_dev_block() {
    let define = [("process.env.NODE_ENV", "\"production\"")];
    let src = "export function f(x) {\n\
               if (process.env.NODE_ENV !== 'production') { devWarn(x); }\n\
               if (process.env.NODE_ENV === 'production') { return x * 2; }\n\
               return x;\n\
               }";
    let js = codegen_with_optimizer_defines(src, SourceType::Module, &define, true);
    // dev 警告块（`if(false)`）被剥离；`if(true)` 的 consequent 保留、if 外壳消除。
    assert!(!js.contains("devWarn"), "dev 块应被剥离:\n{js}");
    assert!(
        js.contains("*2"),
        "the live production return must remain: {js}"
    );
    assert!(!js.contains("if("), "常量 if 应全部折叠:\n{js}");
}

#[test]
fn m4b_keeps_branch_with_hoisted_var() {
    // The dead `var` binding has no remaining reads, so only the selected branch's effects remain.
    let define = [("process.env.NODE_ENV", "\"production\"")];
    let src = "export const r = 1;\n\
               if (process.env.NODE_ENV === 'production') { keep(); } else { var leaked = 2; }";
    let js = codegen_with_optimizer_defines(src, SourceType::Module, &define, true);
    assert!(
        !js.contains("if("),
        "the known branch should be flattened:\n{js}"
    );
    assert!(
        js.contains("keep()"),
        "the selected branch effect must remain:\n{js}"
    );
    assert!(
        !js.contains("leaked"),
        "the proven-unused module-local binding may be removed:\n{js}"
    );
}

#[test]
fn dead_branch_folding_is_independent_of_minify() {
    // 死分支折叠**不再与 minify 耦合**：只要条件可在构建期定为常量就折叠，
    // 语义中性且 dev 产物同样受益（`process.env.NODE_ENV` 的死分支在 dev 也应消失）。
    let define = [("process.env.NODE_ENV", "\"production\"")];
    let src = "if (process.env.NODE_ENV !== 'production') { devWarn(); }\n\
               if (process.env.NODE_ENV === 'production') { prodPath(); }";
    // minify = false（dev 口径）
    let js = codegen_with_optimizer_defines(src, SourceType::Module, &define, false);
    assert!(!js.contains("devWarn"), "dev 死分支应被剥离:\n{js}");
    assert!(js.contains("prodPath"), "存活分支应保留:\n{js}");
    assert!(!js.contains("if ("), "常量条件的 if 外壳应消除:\n{js}");
}

#[test]
fn dead_branch_folding_without_define_keeps_code() {
    // 无法定为常量的条件一律保持原样——折叠只在「可确定」时发生。
    let it = Interner::new();
    let src = "if (someRuntimeFlag) { a(); } else { b(); }";
    let out = parse(src, &it, SourceType::Module);
    let js = out
        .module
        .with_ast(|p| emit_optimized_linked(p, &it, src, false, false));
    assert!(js.contains("a()") && js.contains("b()"), "{js}");
    assert!(js.contains("if ("), "运行时条件不应折叠:\n{js}");
}

#[test]
fn define_replaces_process_env_node_env() {
    let src = "const x = process.env.NODE_ENV;\n\
               const dev = process.env.NODE_ENV !== 'production';\n\
               const keep = process.env.OTHER;\n\
               const nested = obj.process.env.NODE_ENV;";
    let js = codegen_with_optimizer_defines(
        src,
        SourceType::Module,
        &[("process.env.NODE_ENV", "\"production\"")],
        false,
    );
    // process.env.NODE_ENV → "production"（去 process shim 的关键）。
    assert!(js.contains("const x = \"production\";"), "{js}");
    assert!(js.contains("const dev = false;"), "{js}");
    // 其它 process.env.X 与前缀不同的链不误匹配。
    assert!(js.contains("process.env.OTHER"), "{js}");
    assert!(js.contains("obj.process.env.NODE_ENV"), "{js}");
    // 不再残留待替换的 process.env.NODE_ENV。
    assert!(!js.contains("= process.env.NODE_ENV;"), "{js}");
}

#[test]
fn define_expression_replacements_preserve_parent_grammar_and_mapped_bytes() {
    for (source, replacement, expected) in [
        ("globalThis.exp = X ** power();", "-1", "(-1)**power()"),
        ("globalThis.member = X.toString();", "1", "(1).toString()"),
        ("globalThis.call = X();", "()=>1", "(()=>1)()"),
        ("globalThis.unary = !X;", "0,1", "!(0,1)"),
        ("globalThis.binary = X * 3;", "1+2", "=9"),
    ] {
        let (plain, mapped) = codegen_with_optimizer_defines_and_map(
            source,
            SourceType::Module,
            &[("X", replacement)],
            true,
        );

        assert_eq!(
            plain, mapped,
            "source-map collection changed replacement bytes for {source:?}"
        );
        assert!(
            plain.contains(expected),
            "replacement {replacement:?} lost its expression boundary:\n{plain}"
        );

        let interner = Interner::new();
        let reparsed = parse(&plain, &interner, SourceType::Module);
        assert!(
            !reparsed.has_errors(),
            "replacement output must remain valid JavaScript:\n{plain}\n{:?}",
            reparsed.diagnostics
        );
    }
}

#[test]
fn defines_never_replace_write_targets_and_expand_object_shorthand() {
    for (source, define, required, forbidden) in [
        (
            "DEBUG = 1; globalThis.value = DEBUG;",
            "DEBUG",
            "DEBUG=1",
            "false=1",
        ),
        (
            "DEBUG++; globalThis.value = DEBUG;",
            "DEBUG",
            "DEBUG++",
            "false++",
        ),
        (
            "process.env.X = 1; globalThis.value = process.env.X;",
            "process.env.X",
            "process.env.X=1",
            "false=1",
        ),
        (
            "globalThis.value = { DEBUG };",
            "DEBUG",
            "{DEBUG:!1}",
            "{false}",
        ),
    ] {
        let (plain, mapped) = codegen_with_optimizer_defines_and_map(
            source,
            SourceType::Module,
            &[(define, "false")],
            true,
        );
        assert_eq!(plain, mapped, "mapping collection changed define bytes");
        assert!(plain.contains(required), "missing {required:?}:\n{plain}");
        assert!(
            !plain.contains(forbidden),
            "define rewrote a write target or property key:\n{plain}"
        );

        let interner = Interner::new();
        let reparsed = parse(&plain, &interner, SourceType::Module);
        assert!(
            !reparsed.has_errors(),
            "define output must remain valid JavaScript:\n{plain}\n{:?}",
            reparsed.diagnostics
        );
    }
}

/// 往返幂等：codegen 的输出再 parse+codegen 应完全一致（强语义等价信号）。
#[test]
fn define_replaces_import_meta_hot_but_preserves_other_meta_properties() {
    let interner = Interner::new();
    let src = "export const hot = import.meta.hot;\n\
               export const url = import.meta.url;";
    let parsed = parse(src, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);

    let esm = parsed
        .module
        .with_ast(|program| codegen(program, &interner));
    assert!(esm.contains("import.meta.hot"), "{esm}");
    assert!(esm.contains("import.meta.url"), "{esm}");

    let define = [("import.meta.hot", "false")];
    let bundled = codegen_with_optimizer_defines(src, SourceType::Module, &define, false);
    assert!(bundled.contains("const hot = false;"), "{bundled}");
    assert!(!bundled.contains("import.meta.hot"), "{bundled}");
    assert!(bundled.contains("import.meta.url"), "{bundled}");
}

fn assert_stable(src: &str) {
    let it = Interner::new();
    let out1 = parse(src, &it, SourceType::Module);
    assert!(
        !out1.has_errors(),
        "parse1 errors {src:?}: {:?}",
        out1.diagnostics
    );
    let gen1 = out1.module.with_ast(|p| codegen(p, &it));

    let out2 = parse(&gen1, &it, SourceType::Module);
    assert!(
        !out2.has_errors(),
        "re-parse errors:\n{gen1}\n{:?}",
        out2.diagnostics
    );
    let gen2 = out2.module.with_ast(|p| codegen(p, &it));

    assert_eq!(
        gen1, gen2,
        "codegen 非幂等\n--- gen1 ---\n{gen1}\n--- gen2 ---\n{gen2}"
    );
}

#[test]
fn roundtrip_expressions() {
    for src in [
        "1 + 2 * 3;",
        "(1 + 2) * 3;",
        "a ? b : c ? d : e;",
        "a = b = c;",
        "-(-x);",
        "a ** b ** c;",
        "(a ** b) ** c;",
        "(-1) ** 2;",
        "!a && b || c;",
        "a ?? b;",
        "typeof x === 'string';",
        "new Foo(a, b).bar();",
        "a?.b?.[c]?.(d);",
        "x++ + ++y;",
        "left ?? (middle || right);",
        "(left || middle) ?? right;",
        "left ?? (middle && right);",
        "(left && middle) ?? right;",
        "[...a, b, , c];",
        "({ a: 1, b, [c]: 2, ...d });",
        "`a${b + c}d${e}`;",
        "tag`x${y}z`;",
        "(a, b, c);",
        "f(...args);",
        "delete obj.prop;",
    ] {
        assert_stable(src);
    }
}

#[test]
fn readable_number_codegen_does_not_saturate_large_integral_f64_values() {
    let source = "globalThis.pos = 100000000000000000000;\n\
                  globalThis.neg = -100000000000000000000;";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);

    let (plain, mapped) = parsed.module.with_ast(|program| {
        let plain = codegen(program, &interner);
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.minify = false;
        input.set_bundled_commonjs(true);
        let optimized = optimize_fixture(program, &interner, &input).expect("optimizer");
        let (mapped, mappings) =
            crate::codegen_optimized_with_map(&optimized, &interner, &NoLinker, false);
        assert!(
            !mappings.mappings.is_empty(),
            "mapped emission must collect a map"
        );
        (plain, mapped)
    });

    assert_eq!(plain, mapped, "mapping collection changed readable bytes");
    assert!(
        !plain.contains("9223372036854775807"),
        "f64-to-i64 saturation corrupted a positive literal:\n{plain}"
    );
    assert!(
        !plain.contains("-9223372036854775808"),
        "f64-to-i64 saturation corrupted a negative literal:\n{plain}"
    );

    let reparsed = parse(&plain, &interner, SourceType::Module);
    assert!(
        !reparsed.has_errors(),
        "large number output must reparse:\n{plain}\n{:?}",
        reparsed.diagnostics
    );
}

#[test]
fn object_pattern_preserves_explicit_keys_and_nested_aliases() {
    let js = run(
        "const { alias: local, context: { placement, elements: { floating } }, style: { transform, ...restStyle } = {} } = props;",
    );

    assert!(js.contains("alias: local"), "{js}");
    assert!(
        js.contains("context: { placement, elements: { floating } }"),
        "{js}"
    );
    assert!(
        js.contains("style: { transform, ...restStyle } = {}"),
        "{js}"
    );
}

#[test]
fn roundtrip_statements() {
    for src in [
        "const a = 1, b = 2;",
        "let [x, , y = 3, ...z] = arr;",
        "const { p, q: r, ...s } = obj;",
        "if (a) b(); else { c(); d(); }",
        "for (let i = 0; i < n; i++) log(i);",
        "for (const k in obj) use(k);",
        "for (const v of items) use(v);",
        "while (x) y();",
        "do z(); while (w);",
        "switch (v) { case 1: a(); break; default: b(); }",
        "try { risky(); } catch (e) { handle(e); } finally { done(); }",
        "label: for (;;) break label;",
        "throw new Error('boom');",
    ] {
        assert_stable(src);
    }
}

#[test]
fn roundtrip_functions_classes() {
    for src in [
        "function f(a, b = 1, ...rest) { return a + b; }",
        "async function* g() { yield await x; }",
        "const h = (a) => a * 2;",
        "const k = async (x) => { return await x; };",
        "const o = { m() {}, async n() {}, *gen() {}, get p() { return 1; } };",
        "class A extends B { #x = 1; static y = 2; constructor() { super(); } get z() { return this.#x; } static { init(); } method() {} }",
    ] {
        assert_stable(src);
    }
}

#[test]
fn roundtrip_modules() {
    for src in [
        "import a from 'm';",
        "import { b, c as d } from 'm';",
        "import def, { e } from 'm';",
        "import * as ns from 'm';",
        "import 'side-effect';",
        "export const x = 1;",
        "export function f() {}",
        "export default class {}",
        "export default 42;",
        "export { a, b as c };",
        "export { d } from 'm';",
        "export * from 'm';",
        "export * as ns from 'm';",
    ] {
        assert_stable(src);
    }
}

#[test]
fn snapshot_output() {
    insta::assert_snapshot!(run(
        "export function add(a,b){return a+b}\nconst x=(1+2)*3;\nclass C extends D{#p=0;m(){return this.#p??0}}"
    ));
}

#[test]
fn precedence_parens_preserved() {
    // (1+2)*3 必须保留括号；1+2*3 不加括号。
    assert!(run("(1 + 2) * 3;").contains("(1 + 2) * 3"));
    assert!(!run("1 + 2 * 3;").contains("("));
    // 箭头体对象字面量加括号。
    assert!(run("const f = () => ({ a: 1 });").contains("=> ({"));
}

// ======================================================================
// Tree Shaking（PLAN §6.6）
// ======================================================================

struct NoLink;
impl crate::ModuleLinker for NoLink {
    fn module_id(&self, _s: &str, _kind: crate::ModuleRequestKind) -> Option<u32> {
        None
    }
}

/// 用给定「保留导出名」列表做 shake 后的模块体。
fn shake(src: &str, keep: Option<&[&str]>) -> String {
    let it = Interner::new();
    let out = parse(src, &it, SourceType::Module);
    assert!(!out.has_errors(), "parse errors: {:?}", out.diagnostics);
    out.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(src);
        input.minify = keep.is_some();
        input.set_bundled_commonjs(true);
        if let Some(keep) = keep {
            input.linker_liveness = Some(wake_ecma_minify::LinkerExportLiveness::new(
                0,
                keep.iter().copied(),
            ));
        }
        let optimized =
            optimize_fixture(program, &it, &input).expect("tree-shaking fixture should optimize");
        crate::codegen_optimized(&optimized, &it, &NoLink, false)
    })
}

fn has_export_binding(js: &str, name: &str) -> bool {
    js.contains(&format!("exports.{name}"))
        || js.contains(&format!("exports[\"{name}\"]"))
        || js.contains(&format!("Object.defineProperty(exports,\"{name}\""))
        || js.contains(&format!("Object.defineProperty(exports, \"{name}\""))
        || js.contains(&format!(
            "__wake_require__.objectDefineProperty(exports,\"{name}\""
        ))
        || js.contains(&format!(
            "__wake_require__.objectDefineProperty(exports, \"{name}\""
        ))
}

#[test]
fn shake_drops_unused_pure_exports() {
    let src = "export const used = 1;\n\
               export const unused = 2;\n\
               export function helper() { return 3; }\n\
               export class Widget {}";
    let js = shake(src, Some(&["used"]));
    // used 保留（声明 + 绑定）。
    assert!(js.contains("=1"), "{js}");
    assert!(has_export_binding(&js, "used"), "{js}");
    // unused / helper / Widget 整体移除（纯 + 外部未用 + 内部未引用）。
    assert!(!js.contains("unused"), "unused 应被移除:\n{js}");
    assert!(!js.contains("helper"), "helper 应被移除:\n{js}");
    assert!(!js.contains("Widget"), "Widget 应被移除:\n{js}");
}

#[test]
fn shake_keeps_internally_referenced_decl() {
    // secret 外部未用，但被 pub 读取 → 保留声明，仅移除其 exports 绑定。
    let src = "export const secret = 41;\n\
               export function pub() { return secret + 1; }";
    let js = shake(src, Some(&["pub"]));
    let binding = js
        .split("const ")
        .nth(1)
        .and_then(|tail| tail.split('=').next())
        .expect("retained const binding");
    assert!(js.contains("=41"), "声明应保留:\n{js}");
    assert!(
        js.contains(&format!("return {binding}+1")),
        "内部读取必须跟随最终绑定名:\n{js}"
    );
    assert!(!has_export_binding(&js, "secret"), "绑定应移除:\n{js}");
    assert!(has_export_binding(&js, "pub"), "{js}");
}

#[test]
fn shake_keeps_side_effect_initializer() {
    // x 外部未用，但初始化器有副作用 → 保留声明（保留副作用），只移除 exports 绑定。
    let src = "export const x = sideEffect();";
    let js = shake(src, Some(&[]));
    assert!(js.contains("sideEffect();"), "副作用应保留:\n{js}");
    assert!(!has_export_binding(&js, "x"), "绑定应移除:\n{js}");
}

#[test]
fn shake_parenthesizes_preserved_object_and_class_initializers() {
    let object = shake(
        "const unused = { value: sideEffect() }; export const keep = 1;",
        Some(&["keep"]),
    );
    assert!(
        object.contains("({value:sideEffect()});") || object.contains("({ value: sideEffect() });"),
        "对象表达式必须在语句位置加括号:\n{object}"
    );

    let class = shake(
        "const unused = class { [sideEffect()]() {} }; export const keep = 1;",
        Some(&["keep"]),
    );
    assert!(
        class.contains("(class{") || class.contains("(class {"),
        "匿名类表达式必须在语句位置加括号:\n{class}"
    );
    let reparsed = parse(&class, &Interner::new(), SourceType::Module);
    assert!(!reparsed.has_errors(), "产物必须可重新解析:\n{class}");
}

#[test]
fn minified_bigint_suffix_remains_part_of_the_literal_token() {
    let interner = Interner::new();
    let source = "export const zero = 0n; export const one = 1n;";
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let js = parsed
        .module
        .with_ast(|program| emit_optimized_linked(program, &interner, source, true, false));
    assert!(js.contains("0n"), "{js}");
    assert!(js.contains("1n"), "{js}");
    assert!(!js.contains("0 n") && !js.contains("1 n"), "{js}");

    let reparsed = parse(&js, &Interner::new(), SourceType::Script);
    assert!(!reparsed.has_errors(), "{js}: {:?}", reparsed.diagnostics);
}

#[test]
fn shake_drops_unused_default() {
    let src = "export default function App() { return 1; }\nexport const keep = 2;";
    let js = shake(src, Some(&["keep"]));
    assert!(
        !js.contains("exports.default"),
        "未用 default 应移除:\n{js}"
    );
    assert!(!js.contains("App"), "App 应整体移除:\n{js}");
    assert!(has_export_binding(&js, "keep"), "{js}");
}

#[test]
fn shake_none_keeps_everything() {
    // keep=None（入口 / import*）→ 全保留（回归）。
    let src = "export const a = 1;\nexport function b() {}";
    let js = shake(src, None);
    assert!(has_export_binding(&js, "a"), "{js}");
    assert!(has_export_binding(&js, "b"), "{js}");
    assert!(js.contains("function b"), "{js}");
}

#[test]
fn shake_none_keeps_recursive_export_name() {
    let js = shake(
        "export function factorial(n){return n?factorial(n-1)*n:1}",
        None,
    );
    assert!(js.contains("function factorial"), "{js}");
    assert!(has_export_binding(&js, "factorial"), "{js}");
}

#[test]
fn shake_filters_export_specifiers() {
    // export { a, b }：只保留被用的。
    let src = "const a = 1;\nconst b = 2;\nexport { a, b };";
    let js = shake(src, Some(&["a"]));
    assert!(has_export_binding(&js, "a"), "{js}");
    assert!(!has_export_binding(&js, "b"), "未用 b 绑定应移除:\n{js}");
    // 无其它读取的纯局部声明随未用导出一起移除。
    assert!(!js.contains("const b = 2;"), "{js}");
}

#[test]
fn shake_prunes_locals_only_read_by_dropped_exports() {
    let src = "const state={value:1};const listeners=new Set();\
               export function read(){return state.value}\
               export function subscribe(fn){listeners.add(fn)}";
    let js = shake(src, Some(&[]));
    assert!(!js.contains("state"), "{js}");
    assert!(js.contains("new Set()"), "构造求值必须保留:\n{js}");
    assert!(!js.contains("listeners"), "{js}");
}

#[test]
fn shake_keeps_local_used_by_live_aliased_export() {
    let src = "const local={value:1};export {local as publicName};";
    let js = shake(src, Some(&["publicName"]));
    assert!(js.contains("{value:1}"), "{js}");
    assert!(has_export_binding(&js, "publicName"), "{js}");
}

#[test]
fn shake_keeps_local_with_non_export_reader() {
    let src = "const state={value:1};export function dropped(){return state.value}\
               console.log(state);";
    let js = shake(src, Some(&[]));
    assert!(js.contains("const ") && js.contains("={value:1}"), "{js}");
    assert!(js.contains("console.log("), "{js}");
    assert!(!has_export_binding(&js, "dropped"), "{js}");
}

#[test]
fn shake_prunes_self_recursive_and_helper_functions_in_dead_chain() {
    let src = "export function flatten(xs){return xs.map(flatten)}\
               function process(){return 1}\
               export function enqueue(){return process()}";
    let js = shake(src, Some(&[]));
    assert!(!js.contains("function flatten"), "{js}");
    assert!(!js.contains("function process"), "{js}");
    assert!(!js.contains("function enqueue"), "{js}");
}

#[test]
fn shake_live_function_uses_optimizer_owned_declaration_and_binding() {
    let js = shake("export function answer(){return 42}", Some(&["answer"]));
    assert!(js.contains("function"), "{js}");
    assert!(has_export_binding(&js, "answer"), "{js}");
}

#[test]
fn shake_keeps_named_recursive_live_export() {
    let js = shake(
        "export function factorial(n){return n?factorial(n-1)*n:1}",
        Some(&["factorial"]),
    );
    assert!(js.contains("function factorial"), "{js}");
    assert!(has_export_binding(&js, "factorial"), "{js}");
}

// ======================================================================
// 标识符 mangling（WAKE-COMPATIBILITY §M4）：只通过统一 optimizer 入口发射。
// ======================================================================

/// mangle：parse → optimize → codegen_optimized，再 parse 校验合法。
fn mangle_gen(src: &str) -> String {
    let src = observable_expression_fixture(src);
    let it = Interner::new();
    let out = parse(&src, &it, SourceType::Module);
    assert!(
        !out.has_errors(),
        "parse errors {src:?}: {:?}",
        out.diagnostics
    );
    let js = out
        .module
        .with_ast(|p| emit_optimized_linked(p, &it, &src, true, false));
    // 产物必须能无错重解析（结构合法信号）。
    let re = parse(&js, &it, SourceType::Module);
    assert!(
        !re.has_errors(),
        "mangle 产物重解析出错:\n{js}\n{:?}",
        re.diagnostics
    );
    js
}

/// Output-inspection fixtures historically ended in a bare expression such as `f;`. Once the
/// fixed-point effect analysis correctly proves that read pure, both the expression and its whole
/// declaration closure disappear. Route the final value to an unresolved host sink so these tests
/// continue exercising mangling/codegen instead of accidentally testing DCE.
fn observable_expression_fixture(src: &str) -> String {
    let interner = Interner::new();
    let parsed = parse(src, &interner, SourceType::Module);
    if parsed.has_errors() {
        return src.to_owned();
    }
    let expression_span = parsed.module.with_ast(|program| {
        program.body.last().and_then(|statement| match statement {
            Statement::Expression(statement) => Some(statement.expression.span()),
            _ => None,
        })
    });
    let Some(span) = expression_span.filter(|span| !span.is_dummy()) else {
        return src.to_owned();
    };
    let Some(expression) = src.get(span.lo as usize..span.hi as usize) else {
        return src.to_owned();
    };
    let mut observable = src.to_owned();
    observable.replace_range(
        span.lo as usize..span.hi as usize,
        &format!("globalThis.__wake_fixture__=({expression})"),
    );
    observable
}

#[test]
fn mangle_renames_nested_locals_keeps_module_and_props() {
    // 参数 x、局部 y 被重命名；模块级 helper 名保留；成员/属性名不动。
    let js = mangle_gen(
        "function helper(longParam) {\n\
         const longLocal = longParam.value;\n\
         return longLocal + longParam.count;\n\
         }\n\
         helper;",
    );
    // 参数与局部长名消失（被短名替换）。
    assert!(!js.contains("longParam"), "参数应被重命名:\n{js}");
    assert!(!js.contains("longLocal"), "局部应被重命名:\n{js}");
    // Function#name / new.target.name are observable, so function declarations keep their name.
    assert!(js.contains("helper"), "函数声明名必须保留:\n{js}");
    // 成员属性名 .value / .count 不动。
    assert!(js.contains(".value"), "属性名 value 不应改:\n{js}");
    assert!(js.contains(".count"), "属性名 count 不应改:\n{js}");
}

#[test]
fn mangle_expands_object_shorthand() {
    // 局部 longName 被重命名 → 对象 shorthand `{ longName }` 须展开为 `longName: 新名`。
    let js = mangle_gen(
        "function f() {\n\
         const longName = 1;\n\
         return { longName };\n\
         }\n\
         f;",
    );
    // 属性名保留 longName，值是短名 → 出现 "longName:"（展开），且不是裸 `{ longName }`。
    assert!(
        js.contains("longName:"),
        "shorthand 应展开为 key: value:\n{js}"
    );
    // 值处的旧名不应再作为独立标识符出现（只剩属性名那一处 longName）。
    assert_eq!(
        js.matches("longName").count(),
        1,
        "仅剩属性名一处 longName:\n{js}"
    );
}

#[test]
fn mangle_cost_includes_object_shorthand_expansion() {
    let js = mangle_gen("function f(ab){return {ab}} f;");

    assert!(
        js.contains("{ab}"),
        "renaming a short shorthand binding would grow `{{ab}}` into `{{ab:a}}`:\n{js}"
    );
    assert!(
        !js.contains("ab:a"),
        "a growing rename was committed:\n{js}"
    );
}

#[test]
fn mangle_cost_includes_destructuring_shorthand_expansion() {
    let js = mangle_gen("function f(object){const {ab}=object;return ab} f;");

    assert!(
        js.contains("{ab}") || js.contains("{ab:"),
        "the public destructuring key must remain `ab`:\n{js}"
    );
    assert!(!parse(&js, &Interner::new(), SourceType::Module).has_errors());
}

#[test]
fn mangle_expands_destructuring_shorthand_with_default() {
    // 解构 shorthand + 默认值：`{ longKey = 5 }` 绑定重命名 → `longKey: 新名 = 5`。
    let js = mangle_gen(
        "function f(obj) {\n\
         const { longKey = 5 } = obj;\n\
         return longKey;\n\
         }\n\
         f;",
    );
    assert!(js.contains("longKey:"), "解构 shorthand 应展开:\n{js}");
    // 属性名 longKey 保留一处（key），绑定与引用用短名。
    assert_eq!(
        js.matches("longKey").count(),
        1,
        "仅剩属性名一处 longKey:\n{js}"
    );
    assert!(js.contains("=5"), "默认值应保留:\n{js}");
}

#[test]
fn mangle_nested_shadow_stays_correct() {
    // 外层局部 outerVar 与内层局部 innerVar 同链，须不同名；内层对 outerVar 的引用一致重命名。
    let js = mangle_gen(
        "function o() {\n\
         const outerVar = 1;\n\
         function inner() {\n\
         const innerVar = 2;\n\
         return outerVar + innerVar;\n\
         }\n\
         return inner();\n\
         }\n\
         o;",
    );
    assert!(!js.contains("outerVar"), "outerVar 应被重命名:\n{js}");
    assert!(!js.contains("innerVar"), "innerVar 应被重命名:\n{js}");
    // `inner` is only a direct callee and never escapes or exposes `.name`, so its declaration
    // remains safely compressible while the captured outer binding keeps a distinct slot.
    assert!(!js.contains("inner"), "不可观察的内层函数名应被压缩:\n{js}");
}

#[test]
fn mangle_does_not_touch_globals_or_this() {
    // 全局 console / this / 属性名都不动；参数 msg 重命名。
    let js = mangle_gen(
        "function log(msg) {\n\
         console.log(msg, this.tag);\n\
         }\n\
         log;",
    );
    assert!(js.contains("console.log"), "全局 console 不动:\n{js}");
    assert!(js.contains("this.tag"), "this / 属性不动:\n{js}");
    assert!(!js.contains("msg"), "参数 msg 应被重命名:\n{js}");
}

#[test]
fn mangle_ts_namespace_enum_roundtrip() {
    // TS enum/namespace 降级用 DUMMY span 合成节点（NS.member = member 等）。mangle 必须排除它们，
    // 否则多个不同标识符按 DUMMY span 塌成同名 → 重复声明 / 引用错乱。回归守护此坑。
    let it = Interner::new();
    let src = "enum Color { Red, Green }\n\
               namespace NS { export const base = 10; export function scale(x: number){ return x * base; } }\n\
               function useIt(){ const localValueHolder = NS.scale(2); return localValueHolder + Color.Green; }\n\
               globalThis.result=useIt();";
    let out = parse(src, &it, SourceType::TypeScript);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    let js = out
        .module
        .with_ast(|p| emit_optimized_linked(p, &it, src, true, false));
    // 产物必须能无错重解析（塌名会造成重复声明 / ReferenceError 级结构错乱）。
    let re = parse(&js, &it, SourceType::Module);
    assert!(
        !re.has_errors(),
        "TS 降级 mangle 产物重解析出错:\n{js}\n{:?}",
        re.diagnostics
    );
    // 容器局部允许缩名，但公开成员键及枚举双向映射必须保持结构。
    for member in [".Red=0", ".Green=1", ".base=", ".scale="] {
        assert!(
            js.contains(member),
            "namespace/enum 成员 {member:?} 丢失:\n{js}"
        );
    }
    // 而函数内真实局部（带真 span）应被 mangle。
    assert!(
        !js.contains("localValueHolder"),
        "真实局部应被 mangle:\n{js}"
    );
    if let Ok(output) = std::process::Command::new("node")
        .arg("-e")
        .arg(format!(
            "{js};process.stdout.write(String(globalThis.result));"
        ))
        .output()
    {
        assert!(
            output.status.success() && output.stdout == b"21",
            "namespace/enum runtime changed:\n{js}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn optimized_dotted_namespace_merges_keep_nested_initializers() {
    // 两个点分 namespace 会降级成相邻的 `var A = ...`，从而命中 join-vars。
    // 每个外层 IIFE 内的 B/C 初始化虽由合成节点构成，仍必须按原求值顺序保留。
    let it = Interner::new();
    let src = "namespace A.B.C { export const x = 1; export function f() { return x + 1; } }\n\
               namespace A.B.C { export const y = 2; }\n\
               enum E { P = 1 }\n\
               enum E { Q = 2 }\n\
               namespace Outer { export namespace Inner { export const z = 3; } \
               const priv = 4; export const pub = priv; }\n\
               export default JSON.stringify({ x: A.B.C.x, y: A.B.C.y, f: A.B.C.f(), \
               eP: E.P, eQ: E.Q, rev: E[1], inner: Outer.Inner.z, pub: Outer.pub });";
    let out = parse(src, &it, SourceType::TypeScript);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    let js = out.module.with_ast(|p| {
        let mut input = wake_ecma_minify::OptimizeInput::new(src);
        input.set_bundled_commonjs(true);
        let optimized = optimize_fixture(p, &it, &input).expect("optimizer");
        crate::codegen_optimized(&optimized, &it, &NoLinker, false)
    });
    let reparsed = parse(&js, &it, SourceType::Module);
    assert!(
        !reparsed.has_errors(),
        "点分 namespace 压缩产物不可重解析:\n{js}\n{:?}",
        reparsed.diagnostics
    );
    assert!(js.contains(".B="), "B 初始化被错误删除:\n{js}");
    assert!(js.contains(".C="), "C 初始化被错误删除:\n{js}");
    assert!(js.contains(".x="), "首个合并块成员丢失:\n{js}");
    assert!(js.contains(".y=2"), "第二个合并块成员丢失:\n{js}");
    if let Ok(output) = std::process::Command::new("node")
        .arg("-e")
        .arg(format!(
            "const exports={{}},__wake_require__={{objectDefineProperty:Object.defineProperty}};{js};process.stdout.write(exports.default);"
        ))
        .output()
    {
        assert!(
            output.status.success()
                && output.stdout
                    == br#"{"x":1,"y":2,"f":2,"eP":1,"eQ":2,"rev":"P","inner":3,"pub":4}"#,
            "merged namespace runtime changed:\n{js}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn mangle_roundtrip_valid_for_tricky_sources() {
    // 一批含闭包/解构/默认值/catch/for 作用域的源码——只要 mangle 产物能无错重解析即算结构合法。
    for src in [
        "function f(a, b = a) { return a + b; } f;",
        "function f() { try { g(); } catch (err) { return err; } } f;",
        "function f(items) { for (const it of items) { use(it); } } f;",
        "function f() { const { a, b: c, ...rest } = obj; return a + c + rest.x; } f;",
        "function outer() { const cb = (x) => x * factor; let factor = 2; return cb; } outer;",
        "function f() { let x = 1; { let x = 2; return x; } } f;",
        "const g = function named(n) { return n <= 1 ? 1 : n * named(n - 1); }; g;",
    ] {
        let _ = mangle_gen(src);
    }
}

#[test]
fn minify_preserves_observable_named_function_expression_name() {
    let js = mangle_gen("const f = function descriptiveName() {}; export default f.name;");

    assert!(
        js.contains("function descriptiveName"),
        "a named function expression exposes its inner name through Function#name and stacks:\n{js}"
    );
}

#[test]
fn minify_preserves_observable_recursive_function_expression_name() {
    let js = mangle_gen(
        "const f = function descriptiveName(n) { return n ? descriptiveName(n - 1) : 0; }; export default [f(2), f.name];",
    );

    assert!(
        js.contains("function descriptiveName"),
        "a live recursive inner binding must not change the observable Function#name:\n{js}"
    );
}

#[test]
fn minify_preserves_inferred_function_and_class_binding_names() {
    for source in [
        "const descriptiveFunction = function() {}; export default descriptiveFunction.name;",
        "const descriptiveArrow = () => 1; export default descriptiveArrow.name;",
        "const DescriptiveClass = class {}; export default DescriptiveClass.name;",
    ] {
        let js = mangle_gen(source);
        let expected = if source.contains("descriptiveFunction") {
            "descriptiveFunction"
        } else if source.contains("descriptiveArrow") {
            "descriptiveArrow"
        } else {
            "DescriptiveClass"
        };
        assert!(
            js.contains(expected),
            "binding-name inference makes the original spelling observable through `.name`:\n{js}"
        );
    }
}

#[test]
fn minify_preserves_declaration_names_when_runtime_name_is_observable() {
    for source in [
        "function DescriptiveConstructor(){this.seen=new.target.name}export default new DescriptiveConstructor().seen;",
        "function descriptiveCallback(){}export default {descriptiveCallback};",
    ] {
        let js = mangle_gen(source);
        let expected = if source.contains("DescriptiveConstructor") {
            "function DescriptiveConstructor"
        } else {
            "function descriptiveCallback"
        };
        assert!(
            js.contains(expected),
            "an observable declaration runtime name must survive mangling:\n{js}"
        );
    }
}

// ======================================================================
// Property mangling（M5）：仅发射 optimizer 证明安全的封闭形状。
// ======================================================================

/// prop_mangle: parse → optimize → codegen_optimized，再 parse 校验合法。
fn prop_mangle_gen(src: &str) -> String {
    let src = observable_expression_fixture(src);
    let it = Interner::new();
    let out = parse(&src, &it, SourceType::Module);
    assert!(
        !out.has_errors(),
        "parse errors {src:?}: {:?}",
        out.diagnostics
    );
    let js = out
        .module
        .with_ast(|p| emit_optimized_linked(p, &it, &src, true, false));
    let re = parse(&js, &it, SourceType::Module);
    assert!(
        !re.has_errors(),
        "prop_mangle 产物重解析出错:\n{js}\n{:?}",
        re.diagnostics
    );
    js
}

fn optimized_codegen(src: &str, mapped: bool) -> (String, Option<crate::ModuleMappings>) {
    let interner = Interner::new();
    let parsed = parse(src, &interner, SourceType::Module);
    assert!(
        !parsed.has_errors(),
        "parse errors: {:?}",
        parsed.diagnostics
    );
    parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(src);
        input.set_bundled_commonjs(true);
        let optimized =
            optimize_fixture(program, &interner, &input).expect("optimizer should converge");
        if mapped {
            let (code, mappings) =
                crate::codegen_optimized_with_map(&optimized, &interner, &NoLink, false);
            (code, Some(mappings))
        } else {
            (
                crate::codegen_optimized(&optimized, &interner, &NoLink, false),
                None,
            )
        }
    })
}

#[test]
fn optimized_program_emits_after_the_source_ast_owner_is_dropped() {
    let source = "export function answer(descriptiveParameter){return descriptiveParameter+1}";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let mut input = wake_ecma_minify::OptimizeInput::new(source);
    input.set_bundled_commonjs(true);
    let optimized = wake_ecma_minify::optimize(parsed.module.clone(), &interner, &input)
        .expect("owned optimization");

    drop(parsed);
    let generated = crate::codegen_optimized(&optimized, &interner, &NoLink, false);
    assert!(!generated.contains("descriptiveParameter"), "{generated}");
    assert!(!parse(&generated, &interner, SourceType::Script).has_errors());
}

#[test]
fn optimized_late_peephole_omits_only_effect_free_undefined_returns() {
    let source =
        "export function empty(){return void 0}export function effect(){return void sideEffect()}";
    let (generated, _) = optimized_codegen(source, false);

    assert!(generated.contains("return;"), "{generated}");
    assert!(
        generated.contains("return void sideEffect()"),
        "an effectful void expression must remain at its evaluation site: {generated}"
    );
    assert!(!parse(&generated, &Interner::new(), SourceType::Module).has_errors());
}

#[test]
fn optimizer_owned_primitives_use_codegen_shortest_tokens_and_precedence() {
    let (inlined, _) = optimized_codegen(
        "function read(){const descriptiveFlag=true;return consume(descriptiveFlag)}read();",
        false,
    );
    assert!(inlined.contains("consume(!0)"), "{inlined}");
    assert!(!inlined.contains("descriptiveFlag"), "{inlined}");

    let source = "globalThis.value=BUILD_VALUE.property;";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    let generated = parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.set_bundled_commonjs(true);
        input
            .defines
            .push(wake_ecma_minify::ValidatedDefine::primitive(
                "BUILD_VALUE",
                wake_ecma_minify::ConstVal::Undefined,
            ));
        let optimized =
            optimize_fixture(program, &interner, &input).expect("undefined define should optimize");
        crate::codegen_optimized(&optimized, &interner, &NoLink, false)
    });
    assert!(generated.contains("(void 0).property"), "{generated}");
    assert!(!parse(&generated, &interner, SourceType::Script).has_errors());
}

#[test]
fn optimized_entry_emits_only_proof_backed_property_renames() {
    let (closed, _) = optimized_codegen(
        "const closedShape={descriptiveProperty:1};globalThis.answer=closedShape.descriptiveProperty;",
        false,
    );
    assert!(!closed.contains("descriptiveProperty"), "{closed}");

    let (escaped, _) = optimized_codegen(
        "const escapedShape={descriptiveProperty:1};globalThis.answer=escapedShape;",
        false,
    );
    assert!(escaped.contains("descriptiveProperty"), "{escaped}");

    let (private, _) = optimized_codegen(
        "class Example{#descriptiveSecret=1;read(){return this.#descriptiveSecret}}globalThis.answer=new Example().read();",
        false,
    );
    assert!(!private.contains("descriptiveSecret"), "{private}");
    assert!(!parse(&private, &Interner::new(), SourceType::Module).has_errors());
}

#[test]
fn optimized_defines_respect_shadowed_roots() {
    let source = "globalThis.mode=process.env.NODE_ENV;function read(process){return process.env.NODE_ENV}globalThis.local=read({env:{NODE_ENV:'local'}});";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Script);
    assert!(
        !parsed.has_errors(),
        "parse errors: {:?}",
        parsed.diagnostics
    );
    let generated = parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.minify = false;
        input.set_bundled_commonjs(true);
        input
            .defines
            .push(wake_ecma_minify::ValidatedDefine::primitive(
                "process.env.NODE_ENV",
                wake_ecma_minify::ConstVal::Str("production".into()),
            ));
        let optimized = optimize_fixture(program, &interner, &input).expect("binding-aware define");
        crate::codegen_optimized(&optimized, &interner, &NoLink, false)
    });
    assert!(generated.contains("\"production\""), "{generated}");
    assert!(
        generated.contains("return process.env.NODE_ENV"),
        "a parameter named process must suppress the global define: {generated}"
    );
}

#[test]
fn optimized_unused_initializers_preserve_possible_exceptions() {
    for source in [
        "const unused=missingGlobal;globalThis.done=1;",
        "const unused=1n+1;globalThis.done=1;",
        "class Unused extends 1{}globalThis.done=1;",
    ] {
        let (generated, _) = optimized_codegen(source, false);
        assert!(
            generated.contains("missingGlobal")
                || generated.contains("1n+1")
                || generated.contains("extends 1"),
            "a possibly throwing initializer/class evaluation was removed: {source}\n{generated}"
        );
    }
}

#[test]
fn optimized_boolean_and_short_circuit_rewrites_preserve_values() {
    let source = "globalThis.bool=!!{};globalThis.orValue='ready'||fallback();globalThis.andValue=0&&fallback();";
    let (generated, _) = optimized_codegen(source, false);
    assert!(
        generated.contains("!!{}"),
        "boolean coercion must remain boolean: {generated}"
    );
    assert!(
        generated.contains("\"ready\""),
        "logical OR must return its left value: {generated}"
    );
    assert!(
        generated.contains("=0"),
        "logical AND must return numeric zero: {generated}"
    );
    assert!(
        !generated.contains("fallback"),
        "short-circuited calls must not be emitted: {generated}"
    );
}

#[test]
fn optimized_mapped_and_unmapped_entry_are_byte_identical() {
    let source = "const closedShape={descriptiveProperty:1};globalThis.answer=closedShape.descriptiveProperty;";
    let (plain, _) = optimized_codegen(source, false);
    let (mapped, mappings) = optimized_codegen(source, true);
    assert_eq!(plain, mapped);
    let mappings = mappings.expect("mapped output");
    assert!(!mappings.mappings.is_empty());
    let property_offsets: Vec<u32> = source
        .match_indices("descriptiveProperty")
        .map(|(offset, _)| offset as u32)
        .collect();
    assert!(
        property_offsets.iter().all(|offset| mappings
            .mappings
            .iter()
            .any(|mapping| mapping.src_offset == *offset)),
        "renamed key and access must map to their original identifiers: {:?}",
        mappings.mappings
    );
    assert!(
        mappings
            .names
            .iter()
            .any(|name| name == "descriptiveProperty"),
        "proof-backed property rename must retain the original name: {mappings:?}"
    );
}

#[test]
fn consuming_module_finalizer_matches_atomic_emission() {
    let source = "import value from 'dep';export const answer=value;";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);

    parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.minify = true;
        input.set_bundled_commonjs(true);
        let optimized = optimize_fixture(program, &interner, &input).unwrap();
        let facts = crate::bundled_module_facts(&optimized, &NoLinker, false);

        let mut atomic_program = optimized.typed_program().clone();
        let mut atomic_plan = optimized.typed_module_plan().clone();
        wake_ecma_minify::codegen_bridge::finalize_typed_modules(
            &mut atomic_program,
            &mut atomic_plan,
            &facts,
        )
        .unwrap();
        let atomic = crate::codegen_typed(&atomic_program, optimized.minify());

        let (finalized, _) = wake_ecma_minify::codegen_bridge::finalize_owned_typed_modules(
            optimized.typed_program().clone(),
            optimized.typed_module_plan().clone(),
            &facts,
        )
        .unwrap();
        let consuming = crate::typed::codegen_finalized_typed(&finalized, optimized.minify());

        assert_eq!(consuming, atomic);
    });
}

#[test]
fn optimized_private_renames_carry_the_original_name() {
    let source = "class Example{#descriptiveSecret=1;read(){return this.#descriptiveSecret}}globalThis.answer=new Example().read();";
    let (generated, mappings) = optimized_codegen(source, true);
    assert!(!generated.contains("descriptiveSecret"), "{generated}");
    let mappings = mappings.expect("mapped output");
    assert!(
        mappings
            .names
            .iter()
            .any(|name| name == "descriptiveSecret"),
        "private rename must retain the original name: {mappings:?}"
    );
    assert!(
        mappings
            .mappings
            .iter()
            .filter_map(|mapping| mapping.name_index)
            .any(|index| mappings.names[index as usize] == "descriptiveSecret"),
        "private declaration/access must use a named mapping: {mappings:?}"
    );
}

#[test]
fn closed_function_inline_is_byte_identical_and_maps_to_definition_literal() {
    let source =
        "function outer(){function answer(){return 1}return answer()}globalThis.result=outer();";
    let (plain, _) = optimized_codegen(source, false);
    let (mapped, mappings) = optimized_codegen(source, true);
    assert_eq!(plain, mapped);
    assert!(
        plain.contains("=1"),
        "definition literal should inline: {plain}"
    );
    assert!(
        !plain.contains("function answer"),
        "the inner declaration should be removed after every call is inlined: {plain}"
    );

    let definition_literal = source.find("return 1").expect("definition return") as u32 + 7;
    let mappings = mappings.expect("mapped output");
    let literal = plain.find("=1").expect("inlined literal") as u32 + 1;
    assert!(
        mappings.mappings.iter().any(|mapping| {
            mapping.gen_line == 0
                && mapping.gen_col == literal
                && !mapping.is_unmapped
                && mapping.src_offset == definition_literal
        }),
        "inlined source token must map to the definition literal: {mappings:?}"
    );
    assert!(
        mappings
            .mappings
            .iter()
            .any(|mapping| !mapping.is_unmapped && mapping.src_offset == definition_literal),
        "the inlined token must map to the literal in the function definition: {:?}",
        mappings.mappings
    );
}

#[test]
fn closed_identity_function_inline_maps_selected_argument_to_call_site() {
    let source = "function outer(){function pick(left,right){return right}return pick(1,2)}globalThis.result=outer();";
    let (plain, _) = optimized_codegen(source, false);
    let (mapped, mappings) = optimized_codegen(source, true);
    assert_eq!(plain, mapped);
    assert!(
        plain.contains("=2"),
        "selected literal argument should inline: {plain}"
    );
    assert!(
        !plain.contains("function pick"),
        "closed helper should be removed: {plain}"
    );

    let call_argument = source.find("1,2").expect("call arguments") as u32 + 2;
    let literal = plain.find("=2").expect("inlined argument") as u32 + 1;
    let mappings = mappings.expect("mapped output");
    assert!(
        mappings.mappings.iter().any(|mapping| {
            mapping.gen_line == 0
                && mapping.gen_col == literal
                && !mapping.is_unmapped
                && mapping.src_offset == call_argument
        }),
        "the substituted argument token must map to the call-site argument: {mappings:?}"
    );
}

#[test]
fn prop_mangle_returned_object_literal_key_is_preserved() {
    // 返回值会逃逸当前函数，不能证明所有属性访问都封闭在本地。
    let js = prop_mangle_gen("function f() { return { longName: 1 }; } f;");
    assert!(js.contains("longName"), "逃逸对象属性名必须保留:\n{js}");
    assert!(js.contains(":1"), "属性值 1 应保留:\n{js}");
}

#[test]
fn prop_mangle_unknown_parameter_member_is_preserved() {
    // 参数可能来自外部，无法证明其形状封闭。
    let js = prop_mangle_gen("function f(obj) { return obj.longPropertyName; } f;");
    assert!(
        js.contains("longPropertyName"),
        "未知参数属性名必须保留:\n{js}"
    );
    assert!(js.contains("."), "成员访问语法保留:\n{js}");
}

#[test]
fn prop_mangle_computed_not_mangled() {
    // 计算属性访问 `obj[expr]` 不动（括号语法保留）。
    let js = prop_mangle_gen("function f(o, k) { return o[k]; } f;");
    assert!(js.contains("["), "计算属性括号应保留:\n{js}");
    assert!(js.contains("]"), "计算属性括号应保留:\n{js}");
}

#[test]
fn prop_mangle_optional_chain_not_mangled() {
    // 可选链 `obj?.prop` 不动。
    let js = prop_mangle_gen("function f(obj) { return obj?.prop; } f;");
    assert!(js.contains("?."), "可选链应保留:\n{js}");
    assert!(js.contains("prop"), "可选链属性名应保留:\n{js}");
}

#[test]
fn prop_mangle_well_known_not_mangled() {
    // well-known 属性名不缩短：`length`, `toString`, `constructor`
    let js = prop_mangle_gen("function f(arr) { return arr.length + arr.toString(); } f;");
    assert!(js.contains("length"), "length 应保留:\n{js}");
    assert!(js.contains("toString"), "toString 应保留:\n{js}");
}

#[test]
fn prop_mangle_well_known_in_object_literal() {
    // 对象字面量中的 well-known 名保留
    let js = prop_mangle_gen("function f() { return { length: 1, constructor: 2 }; } f;");
    assert!(js.contains("length"), "length 键应保留:\n{js}");
    assert!(js.contains("constructor"), "constructor 键应保留:\n{js}");
}

#[test]
fn prop_mangle_method_not_mangled() {
    // 对象方法的键不缩短
    let js = prop_mangle_gen("function f() { return { myMethod() { return 1; } }; } f;");
    assert!(js.contains("myMethod"), "方法名应保留:\n{js}");
}

#[test]
fn prop_mangle_getter_setter_not_mangled() {
    // getter/setter 键不缩短
    let js = prop_mangle_gen(
        "function f() { return { get myProp() { return 1; }, set myProp(v) {} }; } f;",
    );
    assert!(js.contains("myProp"), "getter/setter 名应保留:\n{js}");
}

#[test]
fn prop_mangle_shorthand_not_mangled() {
    // shorthand `{ longName }` 不动（key 与 value 同一名，不能单独改 key）
    let js = prop_mangle_gen("function f() { const longName = 1; return { longName }; } f;");
    assert!(js.contains("longName"), "shorthand 的 key 应保留:\n{js}");
}

#[test]
fn prop_mangle_class_member_not_mangled() {
    // 类成员名不缩短
    let js = prop_mangle_gen("class C { myMethod() {} myField = 1; } C;");
    assert!(js.contains("myMethod"), "类方法名应保留:\n{js}");
    assert!(js.contains("myField"), "类字段名应保留:\n{js}");
}

#[test]
fn prop_mangle_same_spelling_without_shape_proof_is_preserved() {
    // 相同拼写不代表相同封闭形状；未知参数和逃逸返回值都必须保守。
    let js = prop_mangle_gen(
        "function f(obj) { return obj.longName; } function g() { return { longName: 1 }; } globalThis.f=f; g;",
    );
    assert_eq!(
        js.matches("longName").count(),
        2,
        "无局部形状证明时两处属性名都必须保留:\n{js}"
    );
    assert_eq!(
        js.matches(":1").count(),
        1,
        "对象字面量键被缩短后值保留:\n{js}"
    );
}

#[test]
fn prop_mangle_export_names_preserved() {
    // export 名不动（模块级语法安全）
    let js = prop_mangle_gen("export { foo }; const foo = 1;");
    assert!(js.contains("foo"), "export 名应保留:\n{js}");
}

#[test]
fn prop_mangle_runtime_names_not_mangled() {
    // runtime 注入名不动
    let js = prop_mangle_gen("function f() { return exports.foo + module.bar; } f;");
    assert!(js.contains("exports"), "exports 应保留:\n{js}");
    assert!(js.contains("module"), "module 应保留:\n{js}");
}

#[test]
fn prop_mangle_roundtrip_valid() {
    // 一组含属性访问的源码——产物必须可重解析
    for src in [
        "function f(o) { return o.a + o.b; } f;",
        "function f() { return { a: 1, b: 2 }; } f;",
        "function f(o) { return o?.a; } f;",
        "function f(o) { return o[a]; } f;",
        "function f() { return { [key]: 1 }; } f;",
        "function f() { const x = 1; return { x }; } f;",
        "var result = { longName: 1, anotherName: 2 }; result.longName;",
    ] {
        let js = prop_mangle_gen(src);
        // 必须能无错重解析
        let it = Interner::new();
        let re = parse(&js, &it, SourceType::Module);
        assert!(
            !re.has_errors(),
            "prop_mangle 重解析出错: {src}\n{js}\n{:?}",
            re.diagnostics
        );
    }
}

#[test]
fn prop_mangle_computed_object_key_not_mangled() {
    // 计算属性键 `{ [expr]: val }` 不动（括号语法保留，参数名可能被 identifier mangle）
    let js = prop_mangle_gen("function f(k) { return { [k]: 1 }; } f;");
    assert!(js.contains("["), "计算键括号应保留:\n{js}");
    assert!(js.contains("]:1"), "计算键值对保留:\n{js}");
}

#[test]
fn prop_mangle_cannot_rewrite_object_prototype_setter() {
    let source = "const base = {}; globalThis.value = { __proto__: base };";
    let it = Interner::new();
    let out = parse(source, &it, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    let js = out
        .module
        .with_ast(|program| emit_optimized_linked(program, &it, source, true, false));
    assert!(js.contains("__proto__"), "{js}");
    assert!(!js.contains("renamed:"), "{js}");
}

// ======================================================================
// var 语义：optimizer 必须保留初始化器的原求值位置。
// ======================================================================

/// 统一 optimizer 路径下的 var 发射，并重新解析校验合法。
fn var_position_gen(src: &str) -> String {
    let it = Interner::new();
    let out = parse(src, &it, SourceType::Module);
    assert!(
        !out.has_errors(),
        "parse errors {src:?}: {:?}",
        out.diagnostics
    );
    let js = out.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(src);
        input.set_bundled_commonjs(true);
        let optimized =
            optimize_fixture(program, &it, &input).expect("var-position fixture should optimize");
        crate::codegen_optimized(&optimized, &it, &NoLink, false)
    });
    let re = parse(&js, &it, SourceType::Module);
    assert!(
        !re.has_errors(),
        "var-position 产物重解析出错:\n{js}\n{:?}",
        re.diagnostics
    );
    js
}

#[test]
fn var_initializer_stays_inside_block() {
    // `var` 的绑定具有函数作用域，但初始化器必须留在原块的求值位置。
    let js = var_position_gen(
        "function f() {\n\
         { var x = 1; }\n\
         console.log(x);\n\
         consume(x);\n\
         } f();",
    );
    assert!(
        js.contains("{var x=1;}") && js.contains("console.log(x)") && js.contains("consume(x)"),
        "var 初始化器必须留在原块: {js}"
    );
}

#[test]
fn var_initializers_in_distinct_blocks_are_not_joined() {
    // 不同块中的初始化器不能为了合并声明而越过块边界。
    let js = var_position_gen(
        "function f() {\n\
         { var x = 1; }\n\
         { var y = 2; }\n\
         console.log(x + y);\n\
         consume(x, y);\n\
         } f();",
    );
    assert!(
        js.contains("{var x=1;}{var y=2;}")
            && js.contains("console.log(x+y)")
            && js.contains("consume(x,y)"),
        "不同块的 var 初始化器不得被提升/合并: {js}"
    );
}

#[test]
fn var_initializer_stays_inside_condition() {
    // 条件可能为假，初始化器不能被提升到条件之前。
    let js = var_position_gen(
        "function f() {\n\
         if (condition()) { var x = make(); }\n\
         consume(x);\n\
         observe(x);\n\
         } f();",
    );
    assert!(
        js.contains("if(condition()){var x=make();}")
            && js.contains("consume(x)")
            && js.contains("observe(x)"),
        "条件内 var 初始化器不得提前求值: {js}"
    );
}

#[test]
fn lexical_initializers_stay_inside_their_blocks() {
    // let/const 不提升（块级作用域）
    let js = var_position_gen(
        "function f() {\n\
         { let x = 1; sink(x, x); }\n\
         { const y = 2; sink(y, y); }\n\
         var z = 3;\n\
         } f();",
    );
    // let/const 应保留在块内
    assert!(js.contains("let x=1"), "let 应在块内: {js}");
    assert!(js.contains("const y=2"), "const 应在块内: {js}");
    // var z 可能已在顶部或不提升（已在函数体直接子级）
}

#[test]
fn module_var_remains_at_module_scope() {
    // 模块级 var 不应被提升（已在顶层）
    let js = var_position_gen(
        "var x = 1;\n\
         function f() { return x; }\n\
         globalThis.result = f();",
    );
    assert!(
        js.starts_with("var x") || js.starts_with("var x=1"),
        "模块级 var 应保持在顶层: {js}"
    );
}

#[test]
fn for_loop_var_initializer_stays_in_loop() {
    // for 循环内的 var 不提升（循环体可能不执行，保持赋值时机）
    let js = var_position_gen(
        "function f() {\n\
         for (var i = 0; i < 10; i++) { var x = i; }\n\
         } f();",
    );
    // for-init var i 与循环体 var x 都必须保持原有求值位置。
    assert!(
        !js.contains("var x") || js.contains("for"),
        "for 体内 var 不应单独提升到函数顶部: {js}"
    );
}

#[test]
fn var_bindings_do_not_cross_function_boundaries() {
    // 嵌套函数有自己的作用域，内层 var 不应提升到外层
    let js = var_position_gen(
        "function outer() {\n\
         { var x = 1; }\n\
         function inner() {\n\
         { var y = 2; }\n\
         observe(y);\n\
         return y;\n\
         }\n\
         observe(x);\n\
         return x + inner();\n\
         } outer();",
    );
    assert!(js.contains("var x"), "outer 应有 var x: {js}");
    // inner 内部应有 var y（或者在 inner 顶部）
    assert!(
        js.contains("y") || js.contains("var y"),
        "inner 应有 y: {js}"
    );
}

// ======================================================================
// M4d — SourceMap 映射正确性
// ======================================================================

/// 用映射把产物位置反查源码，验证「产物 token ↔ 源码 token」真正对应。
#[test]
fn sourcemap_maps_identifiers_back_to_source() {
    use wake_common::SourceFile;

    let it = Interner::new();
    let src = "const alpha = 1;\nfunction beta(gamma) {\n  return gamma + alpha;\n}\n";
    let out = parse(src, &it, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let (js, map) = out
        .module
        .with_ast(|p| emit_optimized_linked_with_map(p, &it, src, false, true));
    assert!(!map.is_empty(), "应产出映射");

    let sf = SourceFile::new("a.js", src);
    let gen_lines: Vec<&str> = js.lines().collect();

    // 逐条映射：产物位置处的标识符，应与源码该字节偏移处的标识符同名。
    let mut checked = 0;
    for m in &map.mappings {
        let gl = gen_lines
            .get(m.gen_line as usize)
            .unwrap_or_else(|| panic!("产物行 {} 越界:\n{js}", m.gen_line));
        // 产物列是 UTF-16 列；本用例全 ASCII，可直接当字节列用。
        let gen_tail = &gl[(m.gen_col as usize).min(gl.len())..];
        let src_tail = &src[m.src_offset as usize..];

        let ident_of = |s: &str| -> String {
            s.chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
                .collect()
        };
        let (g, s) = (ident_of(gen_tail), ident_of(src_tail));
        // 只在两侧都取到标识符时比对（语句映射可能落在 `const`/`return` 等关键字上，同样应相等）。
        if !g.is_empty() && !s.is_empty() {
            assert_eq!(
                g, s,
                "映射错位：产物 ({},{}) 处是 `{}`，源码偏移 {} 处是 `{}`\n产物:\n{}",
                m.gen_line, m.gen_col, g, m.src_offset, s, js
            );
            checked += 1;
        }
        // 源侧行列换算不得越界
        let (line, _col) = sf.location0_utf16(m.src_offset);
        assert!(line < sf.line_count(), "源行越界");
    }
    assert!(
        checked >= 4,
        "有效比对太少（{checked}），映射可能未覆盖标识符"
    );
}

/// 产物游标必须与真实产物文本一致：mark 记录的行列处确实是对应内容。
#[test]
fn sourcemap_cursor_tracks_generated_position() {
    let it = Interner::new();
    // 含非 ASCII 字符串字面量与多行结构，考验列（UTF-16）与行的累计。
    let src = "const 名字 = \"中文值\";\nconst n = 42;\nexport { n };\n";
    let out = parse(src, &it, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let (js, map) = out
        .module
        .with_ast(|p| emit_optimized_linked_with_map(p, &it, src, false, true));

    let gen_lines: Vec<&str> = js.lines().collect();
    for m in &map.mappings {
        let gl = gen_lines
            .get(m.gen_line as usize)
            .unwrap_or_else(|| panic!("产物行 {} 越界:\n{js}", m.gen_line));
        // 把 UTF-16 列换算回字节位置，确认不越界（越界即游标漂移）。
        let mut units = 0u32;
        let mut byte = gl.len();
        for (bi, ch) in gl.char_indices() {
            if units == m.gen_col {
                byte = bi;
                break;
            }
            units += ch.len_utf16() as u32;
        }
        assert!(
            byte <= gl.len(),
            "产物列 {} 超出行长（游标漂移）: {gl:?}",
            m.gen_col
        );
    }
    assert!(!map.is_empty());
}

/// 不请求 map 时零开销，且产物与带 map 版本**逐字节相同**（map 采集不得影响输出）。
#[test]
fn sourcemap_does_not_alter_output() {
    let it = Interner::new();
    let src = "export function f(a, b) {\n  const s = `x${a}y`;\n  return s + b;\n}\n";
    let out = parse(src, &it, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let plain = out
        .module
        .with_ast(|p| emit_optimized_linked(p, &it, src, false, true));
    let (with_map, map) = out
        .module
        .with_ast(|p| emit_optimized_linked_with_map(p, &it, src, false, true));
    assert_eq!(plain, with_map, "启用 sourcemap 不得改变产物");
    assert!(!map.is_empty());
}

/// Minified identifier emission must keep the original identifier span even when the token text
/// is replaced by the mangle plan. Collecting mappings is observational only: it may not select a
/// different short name or otherwise perturb the generated bytes.
#[test]
fn sourcemap_minified_mangled_identifier_keeps_original_span_and_bytes() {
    let it = Interner::new();
    let src = "function compute(descriptiveParameter){return descriptiveParameter+1}export default compute;";
    let out = parse(src, &it, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let (plain, mapped, map) = out.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(src);
        input.set_bundled_commonjs(true);
        let optimized = optimize_fixture(program, &it, &input)
            .expect("identifier source-map fixture should optimize");
        let plain = crate::codegen_optimized(&optimized, &it, &NoLink, false);
        let (mapped, map) = crate::codegen_optimized_with_map(&optimized, &it, &NoLink, false);
        (plain, mapped, map)
    });

    assert_eq!(plain, mapped, "mapping collection changed minified bytes");
    assert!(!mapped.contains("descriptiveParameter"), "{mapped}");
    let original_offset = src.find("descriptiveParameter").unwrap() as u32;
    assert!(
        map.mappings
            .iter()
            .any(|mapping| mapping.src_offset == original_offset),
        "renamed binding must map to its original identifier span: {map:?}\n{mapped}"
    );
    assert!(
        map.mappings.iter().any(|mapping| {
            mapping.src_offset == original_offset
                && mapping
                    .name_index
                    .and_then(|index| map.names.get(index as usize))
                    .is_some_and(|name| name == "descriptiveParameter")
        }),
        "renamed binding must carry its original name: {map:?}\n{mapped}"
    );
}

#[test]
fn sourcemap_named_binding_starts_at_the_emitted_identifier_column() {
    let interner = Interner::new();
    let source = "function expose(){let descriptiveBinding=side();return descriptiveBinding+descriptiveBinding}globalThis.expose=expose;";
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);

    let (generated, mappings) = parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.set_bundled_commonjs(true);
        let optimized = optimize_fixture(program, &interner, &input)
            .expect("identifier-column fixture should optimize");
        crate::codegen_optimized_with_map(&optimized, &interner, &NoLink, false)
    });

    let binding_offset = source.find("descriptiveBinding").unwrap() as u32;
    let mapping = mappings
        .mappings
        .iter()
        .find(|mapping| {
            mapping.src_offset == binding_offset
                && mapping
                    .name_index
                    .and_then(|index| mappings.names.get(index as usize))
                    .is_some_and(|name| name == "descriptiveBinding")
        })
        .expect("renamed binding should have a named mapping");
    assert_eq!(
        mapping.gen_line, 0,
        "fixture is expected to stay on one line"
    );
    let emitted = generated
        .as_bytes()
        .get(mapping.gen_col as usize)
        .copied()
        .map(char::from);
    assert!(
        emitted.is_some_and(|ch| ch == '_' || ch == '$' || ch.is_ascii_alphabetic()),
        "named mapping points at {emitted:?}, not the emitted identifier:\n{generated}\n{mappings:?}"
    );
}

#[test]
fn sourcemap_folded_expression_maps_to_replaced_expression() {
    let it = Interner::new();
    let src = "const value = 1 + 2; value;";
    let out = parse(src, &it, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let (js, map, folded_offset) = out.module.with_ast(|program| {
        let wake_ecma_ast::Statement::VariableDeclaration(declaration) = &program.body[0] else {
            panic!("expected variable declaration")
        };
        let Some(wake_ecma_ast::Expression::Binary(binary)) = declaration.declarations[0].init
        else {
            panic!("expected binary initializer")
        };
        let mut input = wake_ecma_minify::OptimizeInput::new(src);
        input.minify = false;
        input.set_bundled_commonjs(true);
        let replacement_interner = Interner::new();
        let replacement = parse("3", &replacement_interner, SourceType::Module);
        input.add_expression_edit(
            wake_ecma_minify::TrustedExpressionEdit::from_parsed_program(
                binary.span,
                &replacement.module,
                &replacement_interner,
            ),
        );
        let optimized = optimize_fixture(program, &it, &input)
            .expect("trusted folded-expression replacement should optimize");
        let (js, map) = crate::codegen_optimized_with_map(&optimized, &it, &NoLink, false);
        (js, map, binary.span.lo)
    });

    assert!(js.contains("value=3") || js.contains("value = 3"), "{js}");
    assert!(
        map.mappings
            .iter()
            .any(|mapping| mapping.src_offset == folded_offset),
        "folded token must retain the replaced expression origin: {map:?}\n{js}"
    );
}

#[test]
fn sourcemap_fences_parentheses_around_a_folded_negative_constant() {
    let source = "const value=-2;globalThis.result=value.toString();";
    let (plain, _) = optimized_codegen(source, false);
    let (mapped, mappings) = optimized_codegen(source, true);
    assert_eq!(plain, mapped);
    let mappings = mappings.expect("mapped output");
    let group = mapped
        .find("(-2)")
        .unwrap_or_else(|| panic!("negative fold was not grouped:\n{mapped}"))
        as u32;
    let origin = source.find("-2").expect("inlined initializer") as u32;
    assert!(
        mappings.mappings.iter().any(|mapping| {
            mapping.is_unmapped && (mapping.gen_line, mapping.gen_col) == (0, group)
        }),
        "opening group is not unmapped: {mappings:?}\n{mapped}"
    );
    assert!(
        mappings.mappings.iter().any(|mapping| {
            !mapping.is_unmapped
                && (mapping.gen_line, mapping.gen_col) == (0, group + 1)
                && mapping.src_offset == origin
        }),
        "folded token is not mapped: {mappings:?}\n{mapped}"
    );
    assert!(
        mappings.mappings.iter().any(|mapping| {
            mapping.is_unmapped && (mapping.gen_line, mapping.gen_col) == (0, group + 3)
        }),
        "closing group is not unmapped: {mappings:?}\n{mapped}"
    );
}

#[test]
fn constant_ternary_is_folded() {
    let define = [("process.env.NODE_ENV", "\"production\"")];
    let src = "const v = process.env.NODE_ENV === 'production' ? fast() : slow();";
    let js = codegen_with_optimizer_defines(src, SourceType::Module, &define, false);
    assert!(js.contains("fast()"), "{js}");
    assert!(!js.contains("slow()"), "死分支应剥离:\n{js}");
    assert!(!js.contains('?'), "三元外壳应消除:\n{js}");
}

#[test]
fn plain_codegen_preserves_unreachable_after_return() {
    let js = run("function f() { return 1; console.log('never'); doMore(); }");
    assert!(js.contains("return 1"), "{js}");
    assert!(js.contains("never"), "纯 codegen 不得自行做 DCE:\n{js}");
    assert!(js.contains("doMore"), "纯 codegen 不得自行做 DCE:\n{js}");
}

#[test]
fn plain_codegen_preserves_unreachable_after_throw() {
    let js = run("function f() { throw new Error('x'); cleanup(); }");
    assert!(js.contains("throw"), "{js}");
    assert!(js.contains("cleanup"), "纯 codegen 不得自行做 DCE:\n{js}");
}

#[test]
fn optimized_codegen_drops_unreachable_tails() {
    for (source, dead) in [
        (
            "function f(){return 1;console.log('never');doMore()}globalThis.result=f();",
            "doMore",
        ),
        (
            "function f(){throw new Error('x');cleanup()}globalThis.f=f;",
            "cleanup",
        ),
    ] {
        let interner = Interner::new();
        let parsed = parse(source, &interner, SourceType::Module);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        let js = parsed
            .module
            .with_ast(|program| emit_optimized_linked(program, &interner, source, true, false));
        assert!(!js.contains(dead), "optimizer 应删除不可达尾部:\n{js}");
    }
}

#[test]
fn optimized_empty_required_statement_bodies_emit_an_empty_statement() {
    for (source, required) in [
        ("while(flag)1;after();", "while(flag);after();"),
        ("for(;flag;)1;after();", "for(;flag;);after();"),
        ("for(const key in object)1;after();", "in object);after();"),
        ("for(const value of items)1;after();", "of items);after();"),
        ("do 1;while(flag);after();", "do;while(flag);after();"),
        ("label:1;after();", "label:;after();"),
        ("with(box)1;after();", "with(box);after();"),
        (
            "if(flag)1;else alternate();after();",
            "if(flag);else alternate();after();",
        ),
        ("if(flag)var unused=1;after();", "if(flag);after();"),
    ] {
        let interner = Interner::new();
        let parsed = parse(source, &interner, SourceType::Script);
        assert!(!parsed.has_errors(), "{source}: {:?}", parsed.diagnostics);
        let generated = parsed.module.with_ast(|program| {
            let removed_body = match &program.body[0] {
                wake_ecma_ast::Statement::If(statement) => statement.consequent.span(),
                wake_ecma_ast::Statement::For(statement) => statement.body.span(),
                wake_ecma_ast::Statement::ForIn(statement) => statement.body.span(),
                wake_ecma_ast::Statement::ForOf(statement) => statement.body.span(),
                wake_ecma_ast::Statement::While(statement) => statement.body.span(),
                wake_ecma_ast::Statement::DoWhile(statement) => statement.body.span(),
                wake_ecma_ast::Statement::Labeled(statement) => statement.body.span(),
                wake_ecma_ast::Statement::With(statement) => statement.body.span(),
                other => panic!("expected required-body statement, got {other:?}"),
            };
            let mut input = wake_ecma_minify::OptimizeInput::new(source);
            input.set_bundled_commonjs(true);
            input.add_statement_removal(removed_body);
            let optimized = optimize_fixture(program, &interner, &input)
                .expect("trusted body removal should optimize");
            crate::codegen_optimized(&optimized, &interner, &NoLinker, false)
        });
        assert!(
            generated.contains(required),
            "required empty statement was not emitted for {source:?}:\n{generated}"
        );
        let reparsed = parse(&generated, &interner, SourceType::Script);
        assert!(
            !reparsed.has_errors(),
            "optimized required-body output must reparse for {source:?}:\n{generated}\n{:?}",
            reparsed.diagnostics
        );
    }
}

#[test]
fn unreachable_tail_with_hoisted_decl_is_kept() {
    // `var`/函数声明会被提升，丢弃会致 ReferenceError → 保守全保留
    let js = run("function f() { return 1; var leaked = 2; }");
    assert!(js.contains("leaked"), "含 var 的不可达尾部应保留:\n{js}");

    let js2 = run("function f() { return 1; function g() {} }");
    assert!(js2.contains("function g"), "函数声明应保留:\n{js2}");
}

#[test]
fn reachable_code_before_terminator_is_untouched() {
    let js = run("function f() { setup(); if (x) return 1; after(); }");
    assert!(js.contains("setup()"), "{js}");
    assert!(
        js.contains("after()"),
        "终止语句在嵌套 if 内，其后仍可达:\n{js}"
    );
}

#[test]
fn jsx_namespaced_name_becomes_string_type() {
    // `<a:b/>` → `_jsx("a:b", …)`（与 tsc 一致：冒号名不可能是组件，整体作字符串类型）
    let it = Interner::new();
    let out = parse("const c = <xlink:href x=\"1\"/>;", &it, SourceType::Tsx);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    let js = out.module.with_ast(|p| codegen(p, &it));
    assert!(js.contains("\"xlink:href\""), "命名空间名应为字符串: {js}");
    assert!(!js.contains("xlink.href"), "不应被当成员访问: {js}");
}

#[test]
fn jsx_dev_runtime_shape_matches_tsc() {
    // 对齐 tsc `--jsx react-jsxdev`：
    //   import { Fragment as _Fragment, jsxDEV as _jsxDEV } from "react/jsx-dev-runtime";
    //   _jsxDEV("div", { className:"x", children:"hi" }, void 0, false,
    //           { fileName, lineNumber: 1, columnNumber: 11 }, this)
    use wake_ecma_parser::{ParseOptions, parse_with};
    let it = Interner::new();
    let opts = ParseOptions {
        jsx_import_source: "react",
        jsx_dev: true,
        file_name: "src/a.tsx",
        ..ParseOptions::default()
    };
    let src = "const a = <div className=\"x\">hi</div>;";
    let out = parse_with(src, &it, SourceType::Tsx, opts);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    let js = out.module.with_ast(|p| codegen(p, &it));

    assert!(
        js.contains("react/jsx-dev-runtime"),
        "应导入 dev runtime: {js}"
    );
    assert!(js.contains("jsxDEV"), "{js}");
    assert!(js.contains("\"src/a.tsx\""), "应带 fileName: {js}");
    // `<` 在第 1 行第 11 列（`const a = ` 占 10 字符）
    assert!(js.contains("lineNumber: 1"), "{js}");
    assert!(js.contains("columnNumber: 11"), "列应指向 `<`: {js}");
    assert!(js.contains("this"), "第 6 参 self: {js}");
    // 单子节点 → isStaticChildren=false
    assert!(js.contains("false"), "{js}");
}

#[test]
#[should_panic(expected = "preserve-CommonJS codegen requires an optimizer-owned import plan")]
fn preserve_commonjs_codegen_has_no_independent_planner_fallback() {
    let source = "import { value } from './dep.js'; globalThis.result = value;";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.minify = false;
        let optimized = optimize_fixture(program, &interner, &input).expect("optimizer");
        crate::codegen_preserved_optimized(
            &optimized,
            &interner,
            PreserveModuleFormat::CommonJs,
            &ExtensionRewriter(".cjs"),
        )
    });
}

#[test]
fn fallible_preserve_codegen_reports_mode_mismatch_without_panicking() {
    let source = "export const value = 1;";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);

    let error = parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.minify = false;
        let optimized = optimize_fixture(program, &interner, &input).expect("optimizer");
        crate::try_codegen_preserved_optimized(
            &optimized,
            &interner,
            PreserveModuleFormat::CommonJs,
            &ExtensionRewriter(".cjs"),
        )
        .expect_err("an ESM plan must not be emitted as CommonJS")
    });

    assert!(
        matches!(error, crate::CodegenError::ModuleModeMismatch { .. }),
        "unexpected error: {error:?}"
    );
}

#[test]
fn preserved_commonjs_keeps_synthetic_jsx_and_byte_zero_import_namespaces_distinct() {
    use wake_ecma_parser::{ParseOptions, parse_with};

    let interner = Interner::new();
    let source = "import {value} from './dep.js'; const view = <section>{value}</section>;";
    let parsed = parse_with(
        source,
        &interner,
        SourceType::Tsx,
        ParseOptions {
            jsx_dev: true,
            file_name: "view.test.tsx",
            ..ParseOptions::default()
        },
    );
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let generated = parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.minify = false;
        input.set_preserve_commonjs(true);
        let optimized = optimize_fixture(program, &interner, &input)
            .expect("preserved JSX fixture should optimize");
        crate::codegen_preserved_optimized(
            &optimized,
            &interner,
            PreserveModuleFormat::CommonJs,
            &ExtensionRewriter(".js"),
        )
    });
    let namespaces = generated
        .lines()
        .filter_map(|line| {
            let declaration = line.strip_prefix("const ")?;
            let (binding, initializer) = declaration.split_once(" = ")?;
            initializer
                .starts_with("require(")
                .then_some((binding, initializer))
        })
        .collect::<Vec<_>>();
    assert_eq!(namespaces.len(), 2, "{generated}");
    assert_ne!(namespaces[0].0, namespaces[1].0, "{generated}");
    assert!(
        namespaces
            .iter()
            .any(|(_, initializer)| initializer.contains("react/jsx-dev-runtime")),
        "{generated}"
    );
    assert!(
        namespaces
            .iter()
            .any(|(_, initializer)| initializer.contains("./dep.js")),
        "{generated}"
    );
}

#[test]
fn preserved_commonjs_dynamic_scope_keeps_binding_and_localizes_with_fallback() {
    let interner = Interner::new();
    let source = "import {value} from './dep.js';with(box){globalThis.inside=value}globalThis.outside=value;";
    let parsed = parse(source, &interner, SourceType::Script);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let generated = parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.minify = false;
        input.set_preserve_commonjs(true);
        let optimized = optimize_fixture(program, &interner, &input)
            .expect("with must be a local conservative boundary");
        crate::codegen_preserved_optimized(
            &optimized,
            &interner,
            PreserveModuleFormat::CommonJs,
            &ExtensionRewriter(".cjs"),
        )
    });

    assert!(generated.contains("const value = "), "{generated}");
    assert!(
        generated.contains("with (box) {\n  globalThis.inside = value;"),
        "a with-body read must retain dynamic name lookup:\n{generated}"
    );
    assert!(
        generated.contains("globalThis.outside = __wake_namespace_0[\"value\"]"),
        "the read outside with must keep the live namespace substitution:\n{generated}"
    );
}

#[test]
fn jsx_dev_runtime_static_children_flag() {
    use wake_ecma_parser::{ParseOptions, parse_with};
    let it = Interner::new();
    let opts = ParseOptions {
        jsx_dev: true,
        file_name: "f.tsx",
        ..ParseOptions::default()
    };
    // 多子节点 → isStaticChildren = true（对应 prod 的 jsxs）
    let out = parse_with("const b = <p><i/><b/></p>;", &it, SourceType::Tsx, opts);
    let js = out.module.with_ast(|p| codegen(p, &it));
    assert!(
        js.contains("true"),
        "多子节点应 isStaticChildren=true: {js}"
    );
}

#[test]
fn jsx_import_source_is_configurable() {
    use wake_ecma_parser::{ParseOptions, parse_with};
    let it = Interner::new();
    let opts = ParseOptions {
        jsx_import_source: "preact",
        ..ParseOptions::default()
    };
    let out = parse_with("const a = <div/>;", &it, SourceType::Tsx, opts);
    let js = out.module.with_ast(|p| codegen(p, &it));
    assert!(js.contains("preact/jsx-runtime"), "应用可配来源: {js}");
    // 注意 "preact/jsx-runtime" 本身含子串 "react/jsx-runtime"，须按带引号的完整说明符比对
    assert!(!js.contains("\"react/jsx-runtime\""), "{js}");
}

#[test]
fn jsx_default_stays_production_runtime() {
    // 默认（不配置）保持 production automatic runtime，产物与既有一致
    let it = Interner::new();
    let out = parse("const a = <div/>;", &it, SourceType::Tsx);
    let js = out.module.with_ast(|p| codegen(p, &it));
    assert!(js.contains("react/jsx-runtime"), "{js}");
    assert!(!js.contains("jsxDEV"), "默认不应用 dev runtime: {js}");
}

// ======================================================================
// import= / export= / import attributes / using（DESIGN §4.1 的四处语法缺口）
// ======================================================================

#[test]
fn ts_import_equals_lowers_to_require() {
    let js = strip_ts(
        "import fs = require('fs');\n\
         import path = require('path');\n\
         import Alias = NS.Inner.Leaf;\n\
         export function f() { return fs.readFileSync(path.join(Alias.p)); }",
    );
    // require 形态 → `const x = require("m")`（codegen 的 emit_require_call 会在链接期
    // 把它改写为 `__wake_require__(id)`，故必须逐字保持这个形态）。
    assert!(js.contains("const fs = require(\"fs\");"), "{js}");
    assert!(js.contains("const path = require(\"path\");"), "{js}");
    // 实体名别名 → `var`（命名空间声明合并允许后补段，var 的提升避免 TDZ）。
    assert!(js.contains("var Alias = NS.Inner.Leaf;"), "{js}");
    assert!(!js.contains("import "), "不应残留 import 语句:\n{js}");
}

#[test]
fn ts_export_assign_lowers_to_module_exports() {
    let js = strip_ts("const api = { a: 1 };\nexport = api;");
    // `export =` has CommonJS assignment semantics. Typed codegen and the bundler's canonical
    // factory wrapper agree on `module`; no later generated-body text rewrite owns this binding.
    assert!(js.contains("module.exports = api;"), "{js}");
    assert!(!js.contains("export ="), "不应残留 `export =`:\n{js}");
    // 降级后模块内无任何 ESM 语句 → `program_is_esm` 为假 → 不打 `__esModule` 标记，
    // 默认导入 interop 因而拿到整个 exports 对象，与 TS 的 `export =` 语义一致。
    assert!(!js.contains("export {"), "不应残留具名导出:\n{js}");
}

#[test]
fn ts_export_as_namespace_erased() {
    let js = strip_ts("export as namespace MyLib;\nexport const v = 1;");
    assert!(!js.contains("namespace"), "UMD 全局声明应擦除:\n{js}");
    assert!(js.contains("export const v = 1;"), "{js}");
}

#[test]
fn import_attributes_emitted() {
    let js = run("import d from './d.json' with { type: 'json' };\n\
         import './s.css' with { type: 'css' };\n\
         export { a } from './m.js' with { type: 'json' };\n\
         export * from './n.js' with { type: 'json' };\n\
         import o from './o.json' assert { type: 'json' };");
    assert!(
        js.contains("import d from \"./d.json\" with { type: \"json\" };"),
        "{js}"
    );
    assert!(
        js.contains("import \"./s.css\" with { type: \"css\" };"),
        "{js}"
    );
    assert!(
        js.contains("export { a } from \"./m.js\" with { type: \"json\" };"),
        "{js}"
    );
    assert!(
        js.contains("export * from \"./n.js\" with { type: \"json\" };"),
        "{js}"
    );
    // 已废弃的 import assertions 保留原关键字（改写成 with 会在旧运行时上改变语义）。
    assert!(
        js.contains("import o from \"./o.json\" assert { type: \"json\" };"),
        "{js}"
    );
    assert_stable(
        "import d from './d.json' with { type: 'json' };\nexport * from './n.js' with { type: 'json' };",
    );
}

#[test]
fn import_attributes_do_not_swallow_with_statement() {
    // 反例：换行后的 `with (o) {}` 是 with 语句，不能被吞成引入属性子句。
    let js = run("import x from 'm'\nwith (o) { y }");
    assert!(js.contains("import x from \"m\";"), "{js}");
    assert!(js.contains("with (o)"), "{js}");
}

#[test]
fn using_declarations_emit() {
    // `await using` 只在 async 上下文内识别（顶层 await 未支持，见 parser 的
    // `top_level_await_using_is_rejected`）。
    let js = run("{ using a = mk(); use(a); }\n\
         async function f() {\n\
           await using b = mkAsync();\n\
           for (using r of rs) { r.q(); }\n\
           for await (using k of ks) { k.q(); }\n\
         }\n\
         class C { static { using s = mk(); s.q(); } }");
    assert!(js.contains("using a = mk();"), "{js}");
    assert!(js.contains("await using b = mkAsync();"), "{js}");
    assert!(js.contains("for (using r of rs)"), "{js}");
    assert!(js.contains("for await (using k of ks)"), "{js}");
    assert!(js.contains("using s = mk();"), "{js}");
    assert_stable("{ using a = mk(); }\nasync function f() { await using b = mkAsync(); }");
}

#[test]
fn using_identifier_is_not_declaration() {
    // 反例：`using` 是上下文关键字，下列每处都应原样保持为标识符。
    let js = run("let using = 1;\n\
         using = 2;\n\
         using\n\
         x = 3;\n\
         foo(using);\n\
         using.prop;\n\
         for (using of xs) { g(using); }");
    assert!(js.contains("let using = 1;"), "{js}");
    assert!(js.contains("using = 2;"), "{js}");
    assert!(js.contains("foo(using);"), "{js}");
    assert!(js.contains("for (using of xs)"), "{js}");
    // ASI：`using` 与下一行的 `x = 3` 是两条语句，绝不能粘成 `using x = 3`。
    assert!(!js.contains("using x"), "ASI 被破坏:\n{js}");
}

/// 走统一 optimizer 的未用变量消除路径。
fn elim_gen(src: &str) -> String {
    let it = Interner::new();
    let out = parse(src, &it, SourceType::Module);
    assert!(
        !out.has_errors(),
        "parse errors {src:?}: {:?}",
        out.diagnostics
    );
    out.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(src);
        input.set_bundled_commonjs(true);
        let optimized =
            optimize_fixture(program, &it, &input).expect("unused-binding fixture should optimize");
        crate::codegen_optimized(&optimized, &it, &NoLink, false)
    })
}

#[test]
fn used_trailing_parameter_after_destructured_parameter_is_preserved() {
    let js = elim_gen(
        "function useInitialMotionValues({transformTemplate},visualState){return useMemo(()=>{const state=createHtmlRenderState();buildHTMLStyles(state,visualState,transformTemplate);return Object.assign({},state.vars,state.style)},[visualState])}\n\
         function useStyle(props,visualState){const styleProp=props.style||{};const style={};copyRawValuesOnly(style,styleProp,props);Object.assign(style,useInitialMotionValues(props,visualState));return style}\n\
         globalThis.result=useStyle(inputProps,inputState);",
    );
    let first_parameters = js
        .split_once("function ")
        .and_then(|(_, function)| function.split_once('('))
        .and_then(|(_, parameters)| parameters.split_once(')'))
        .map(|(parameters, _)| parameters)
        .expect("first generated function parameters");
    assert!(
        first_parameters.contains("},"),
        "the trailing parameter after the destructuring parameter was removed:\n{js}"
    );
    assert!(
        js.contains("buildHTMLStyles("),
        "the trailing value use must remain:\n{js}"
    );
}

#[test]
fn unused_trailing_parameter_is_preserved_during_minification() {
    let js =
        elim_gen("function keepArity(first,second){return first}globalThis.keepArity=keepArity;");
    let parameters = js
        .split_once('(')
        .and_then(|(_, rest)| rest.split_once(')'))
        .map(|(parameters, _)| parameters)
        .expect("generated function parameter list");
    assert!(
        parameters.contains(','),
        "parameter list was trimmed:\n{js}"
    );
}
#[test]
fn unused_using_declaration_is_never_eliminated() {
    // `using _ = acquire()` 的**零引用**形态恰是 using 最典型的用法：绑定虽无人读，
    // 但离开作用域时的 dispose 调用是可观测副作用，声明必须原样保留。
    let js = elim_gen(
        "async function f() {\n\
           const deadConst = pure();\n\
           using lock = acquire();\n\
           await using conn = connect();\n\
           return 1;\n\
         } globalThis.f=f;",
    );
    assert!(
        js.contains("using ") && js.contains("=acquire()"),
        "using 被消除:\n{js}"
    );
    assert!(
        js.contains("await using ") && js.contains("=connect()"),
        "await using 被消除:\n{js}"
    );
    // 对照组：同样零引用的普通 const 仍应被消除，证明本用例确实走到了消除路径。
    assert!(
        !js.contains("deadConst"),
        "对照组未被消除，本用例没走到消除路径:\n{js}"
    );
}

struct ExtensionRewriter(&'static str);

impl ModuleSpecifierRewriter for ExtensionRewriter {
    fn rewrite(&self, specifier: &str) -> Option<String> {
        specifier
            .starts_with('.')
            .then(|| format!("{}{}", specifier.trim_end_matches(".js"), self.0))
    }
}

struct ConditionalKindRewriter;

impl ModuleSpecifierRewriter for ConditionalKindRewriter {
    fn rewrite(&self, _specifier: &str) -> Option<String> {
        None
    }

    fn rewrite_with_kind(&self, specifier: &str, kind: ModuleSpecifierKind) -> Option<String> {
        let profile = match kind {
            ModuleSpecifierKind::Import => "import",
            ModuleSpecifierKind::Require => "require",
        };
        Some(format!("{profile}:{specifier}"))
    }

    fn lower_dynamic_import_to_require(&self) -> bool {
        true
    }
}

fn preserved_module(source: &str, format: PreserveModuleFormat, extension: &'static str) -> String {
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.minify = false;
        input.set_preserve_commonjs(format == PreserveModuleFormat::CommonJs);
        let optimized = optimize_fixture(program, &interner, &input)
            .expect("preserved-module fixture should optimize");
        crate::codegen_preserved_optimized(
            &optimized,
            &interner,
            format,
            &ExtensionRewriter(extension),
        )
    })
}

#[test]
fn preserved_commonjs_retains_import_and_require_edge_kinds_for_one_specifier() {
    let interner = Interner::new();
    let source = "import value from 'dual'; const required = require('dual'); export async function load() { return import('dual') } globalThis.result = [value, required];";
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    for minify in [false, true] {
        let generated = parsed.module.with_ast(|program| {
            let mut input = wake_ecma_minify::OptimizeInput::new(source);
            input.minify = minify;
            input.set_preserve_commonjs(true);
            let optimized = optimize_fixture(program, &interner, &input)
                .expect("conditional preserve fixture should optimize");
            crate::codegen_preserved_optimized(
                &optimized,
                &interner,
                PreserveModuleFormat::CommonJs,
                &ConditionalKindRewriter,
            )
        });

        assert!(
            generated.contains("require(\"import:dual\")"),
            "{generated}"
        );
        assert!(
            generated.contains("require(\"require:dual\")"),
            "{generated}"
        );
        assert!(
            generated.contains("Promise.resolve(require(\"import:dual\"))"),
            "{generated}"
        );
        assert!(
            !generated.contains("__wake_require__.external")
                && !generated.contains("__wake_require__.promiseResolve"),
            "preserved CommonJS has no bundle runtime service owner:\n{generated}"
        );

        if std::process::Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
        {
            let script = format!(
                r#"
const module={{exports:{{}}}},exports=module.exports;
function require(specifier){{
  if(specifier==="import:dual")return {{__esModule:true,default:"import-value",named:"lazy-value"}};
  if(specifier==="require:dual")return "require-value";
  throw new Error("unexpected "+specifier);
}}
{generated}
Promise.resolve(module.exports.load()).then(lazy=>{{
  const actual=[globalThis.result,lazy.default,lazy.named];
  const expected=[["import-value","require-value"],"import-value","lazy-value"];
  if(JSON.stringify(actual)!==JSON.stringify(expected)){{console.error(actual);process.exitCode=2;}}
}},error=>{{console.error(error&&error.stack||error);process.exitCode=1;}});
"#
            );
            let output = std::process::Command::new("node")
                .arg("-e")
                .arg(script)
                .output()
                .expect("execute preserved CommonJS request-kind fixture");
            assert!(
                output.status.success(),
                "minify={minify}\n{generated}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}

#[test]
fn preserved_modules_rewrite_every_module_edge_without_a_wake_runtime() {
    let source = r#"
import value, { named as local } from "./dep.js";
import * as namespace from "./namespace.js";
export { item } from "./other.js";
export * from "external";
export const legacy = require("./legacy.js");
export async function load() { return import("./lazy.js"); }
export default value + local + namespace.value;
"#;

    let esm = preserved_module(source, PreserveModuleFormat::EsModule, ".mjs");
    assert!(esm.contains("from \"./dep.mjs\""), "{esm}");
    assert!(esm.contains("from \"./other.mjs\""), "{esm}");
    assert!(esm.contains("from \"external\""), "{esm}");
    assert!(esm.contains("require(\"./legacy.mjs\")"), "{esm}");
    assert!(esm.contains("import(\"./lazy.mjs\")"), "{esm}");
    assert!(esm.contains("export default"), "{esm}");
    assert!(!esm.contains("__wake_"), "{esm}");
    assert!(!esm.contains("Object.defineProperty(exports"), "{esm}");

    let cjs = preserved_module(source, PreserveModuleFormat::CommonJs, ".cjs");
    assert!(cjs.contains("require(\"./dep.cjs\")"), "{cjs}");
    assert!(cjs.contains("require(\"./other.cjs\")"), "{cjs}");
    assert!(cjs.contains("require(\"external\")"), "{cjs}");
    assert!(cjs.contains("require(\"./legacy.cjs\")"), "{cjs}");
    assert!(cjs.contains("import(\"./lazy.cjs\")"), "{cjs}");
    assert!(has_export_binding(&cjs, "default"), "{cjs}");
    assert!(!cjs.contains("__wake_modules__"), "{cjs}");
    assert_eq!(
        cjs.matches("function __wake_interop_default(").count(),
        0,
        "a single default import uses the optimizer-owned inline interop read:\n{cjs}"
    );
    assert_eq!(
        cjs.matches(".__esModule ?").count(),
        2,
        "default and namespace interop each need one marker-safe read:\n{cjs}"
    );
    assert_eq!(
        cjs.matches("Object.assign({},").count(),
        1,
        "the namespace wrapper must be constructed exactly once:\n{cjs}"
    );
    assert!(
        !cjs.contains("function __wake_interop_star("),
        "typed finalization must not depend on a free legacy helper:\n{cjs}"
    );
}

#[test]
fn preserve_commonjs_mapping_uses_the_same_emitter_output() {
    let source = "import value from './dep.js';\nexport const label: string = `🦀${value}`;";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::TypeScript);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);

    let (plain, mapped, mappings) = parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.minify = false;
        input.set_preserve_commonjs(true);
        let optimized = optimize_fixture(program, &interner, &input)
            .expect("mapped preserve fixture should optimize");
        let plain = crate::codegen_preserved_optimized(
            &optimized,
            &interner,
            PreserveModuleFormat::CommonJs,
            &ExtensionRewriter(".cjs"),
        );
        let (mapped, mappings) = crate::codegen_preserved_optimized_with_map(
            &optimized,
            &interner,
            PreserveModuleFormat::CommonJs,
            &ExtensionRewriter(".cjs"),
        );
        (plain, mapped, mappings)
    });

    assert_eq!(
        mapped, plain,
        "source-map collection must not change codegen"
    );
    assert!(!mappings.is_empty());
    assert!(
        mappings
            .mappings
            .windows(2)
            .all(|pair| (pair[0].gen_line, pair[0].gen_col) <= (pair[1].gen_line, pair[1].gen_col)),
        "preserve-module mappings must be generated in deterministic order"
    );
    assert!(
        mappings
            .mappings
            .iter()
            .any(|mapping| mapping.src_offset as usize == source.find("label").unwrap())
    );
}

#[test]
fn preserve_commonjs_receiver_strip_punctuation_is_unmapped_but_the_read_is_named() {
    fn generated_position(source: &str, byte_offset: usize) -> (u32, u32) {
        let prefix = &source[..byte_offset];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
        let column = prefix
            .rsplit_once('\n')
            .map_or(prefix, |(_, tail)| tail)
            .encode_utf16()
            .count() as u32;
        (line, column)
    }

    let source = "import {named as local} from './dep.js';globalThis.answer=local();";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);

    let (plain, mapped, mappings) = parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.set_preserve_commonjs(true);
        let optimized = optimize_fixture(program, &interner, &input)
            .expect("receiver-strip fixture should optimize");
        let plain = crate::codegen_preserved_optimized(
            &optimized,
            &interner,
            PreserveModuleFormat::CommonJs,
            &ExtensionRewriter(".cjs"),
        );
        let (mapped, mappings) = crate::codegen_preserved_optimized_with_map(
            &optimized,
            &interner,
            PreserveModuleFormat::CommonJs,
            &ExtensionRewriter(".cjs"),
        );
        (plain, mapped, mappings)
    });

    assert_eq!(plain, mapped, "mapping collection changed generated bytes");
    let receiver = mapped
        .find("(0,")
        .unwrap_or_else(|| panic!("fixture emitted no receiver strip:\n{mapped}"));
    let (prefix_line, prefix_col) = generated_position(&mapped, receiver);
    let (zero_line, zero_col) = generated_position(&mapped, receiver + 1);
    let (read_line, read_col) = generated_position(&mapped, receiver + 3);
    assert!(mappings.mappings.iter().any(|mapping| {
        mapping.is_unmapped && (mapping.gen_line, mapping.gen_col) == (prefix_line, prefix_col)
    }));
    assert!(mappings.mappings.iter().all(|mapping| {
        (mapping.gen_line, mapping.gen_col) != (zero_line, zero_col) || mapping.is_unmapped
    }));
    assert!(mappings.mappings.iter().all(|mapping| {
        mapping.gen_line != prefix_line
            || mapping.gen_col < prefix_col
            || mapping.gen_col >= read_col
            || mapping.is_unmapped
    }));

    let reference_offset = source.rfind("local").expect("local reference") as u32;
    assert!(
        mappings.mappings.iter().any(|mapping| {
            !mapping.is_unmapped
                && (mapping.gen_line, mapping.gen_col) == (read_line, read_col)
                && mapping.src_offset == reference_offset
                && mapping
                    .name_index
                    .and_then(|index| mappings.names.get(index as usize))
                    .is_some_and(|name| name == "local")
        }),
        "source read must begin a named mapping after `(0,`: {mappings:?}\n{mapped}"
    );
}

#[test]
fn preserve_commonjs_maps_source_function_boundaries_but_not_synthetic_export_getters() {
    fn generated_position(source: &str, byte_offset: usize) -> (u32, u32) {
        let prefix = &source[..byte_offset];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
        let column = prefix
            .rsplit_once('\n')
            .map_or(prefix, |(_, tail)| tail)
            .chars()
            .map(char::len_utf16)
            .sum::<usize>() as u32;
        (line, column)
    }

    let source = concat!(
        "export function declared() {}\n",
        "export const arrow = () => 0;\n",
        "export const object = { method() {} };\n",
        "export class Box { classMethod() {} }",
    );
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);

    let (generated, mappings) = parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.minify = false;
        input.set_preserve_commonjs(true);
        let optimized = optimize_fixture(program, &interner, &input)
            .expect("function-boundary fixture should optimize");
        crate::codegen_preserved_optimized_with_map(
            &optimized,
            &interner,
            PreserveModuleFormat::CommonJs,
            &ExtensionRewriter(".cjs"),
        )
    });

    for (generated_needle, source_needle) in [
        ("function declared", "function declared"),
        ("() => 0", "() => 0"),
        ("method()", "method()"),
        ("classMethod()", "classMethod()"),
    ] {
        let generated_offset = generated
            .find(generated_needle)
            .unwrap_or_else(|| panic!("missing {generated_needle:?} in:\n{generated}"));
        let source_offset = source.find(source_needle).unwrap() as u32;
        let (gen_line, gen_col) = generated_position(&generated, generated_offset);
        assert!(
            mappings.mappings.iter().any(|mapping| {
                mapping.gen_line == gen_line
                    && mapping.gen_col == gen_col
                    && mapping.src_offset == source_offset
            }),
            "source function boundary {source_needle:?} has no exact generated anchor:\n{generated}\n{mappings:?}"
        );
    }

    let synthetic_getters = generated
        .match_indices("get: function()")
        .map(|(offset, _)| offset + "get: ".len())
        .collect::<Vec<_>>();
    assert!(
        !synthetic_getters.is_empty(),
        "fixture emitted no export getter:\n{generated}"
    );
    for getter_offset in synthetic_getters {
        let (gen_line, gen_col) = generated_position(&generated, getter_offset);
        assert!(
            mappings
                .mappings
                .iter()
                .all(|mapping| mapping.gen_line != gen_line || mapping.gen_col != gen_col),
            "synthetic export getter unexpectedly owns a source function anchor:\n{generated}\n{mappings:?}"
        );
    }
}

fn typed_codegen_fixture(
    source: &str,
    source_type: SourceType,
    minify: bool,
) -> (String, String, crate::ModuleMappings) {
    let interner = Interner::new();
    let parsed = parse(source, &interner, source_type);
    assert!(
        !parsed.has_errors(),
        "typed codegen fixture did not parse:\n{source}\n{:?}",
        parsed.diagnostics
    );
    parsed.module.with_ast(|program| {
        let typed = wake_ecma_minify::codegen_bridge::TypedProgram::lower(program, &interner, None)
            .expect("fixture should lower to typed IR");
        let plain = crate::codegen_typed(&typed, minify);
        let (mapped, mappings) = crate::codegen_typed_with_map(&typed, minify);
        (plain, mapped, mappings)
    })
}

fn optimized_bundled_fixture(
    source: &str,
    source_type: SourceType,
    minify: bool,
) -> (String, String, crate::ModuleMappings) {
    let interner = Interner::new();
    let parsed = parse(source, &interner, source_type);
    assert!(
        !parsed.has_errors(),
        "optimized codegen fixture did not parse:\n{source}\n{:?}",
        parsed.diagnostics
    );
    parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.minify = minify;
        input.set_bundled_commonjs(true);
        let optimized = optimize_fixture(program, &interner, &input)
            .expect("fixture should optimize through the production pipeline");
        let plain = crate::codegen_optimized(&optimized, &interner, &NoLinker, true);
        let (mapped, mappings) =
            crate::codegen_optimized_with_map(&optimized, &interner, &NoLinker, true);
        (plain, mapped, mappings)
    })
}

#[test]
fn typed_ir_codegen_groups_problematic_expression_statement_prefixes() {
    for source in [
        "(function(value){return value})(1);",
        "({value:1}).value;",
        "(class {}).name;",
        "(function(){}).name;",
    ] {
        for minify in [false, true] {
            let (plain, mapped, _) = typed_codegen_fixture(source, SourceType::Script, minify);
            assert_eq!(plain, mapped, "mapping changed bytes for {source:?}");
            assert!(
                plain.starts_with('('),
                "statement-leading expression became declaration syntax: {plain}"
            );
            let reparsed = parse(&plain, &Interner::new(), SourceType::Script);
            assert!(
                !reparsed.has_errors(),
                "typed expression statement did not reparse: {plain}\n{:?}",
                reparsed.diagnostics
            );
        }
    }
}

#[test]
fn typed_ir_codegen_reparses_owned_js_ts_jsx_and_tsx_syntax() {
    let fixtures = [
        (
            SourceType::Module,
            concat!(
                "import main, { item as renamed } from './dep.js' with { type: \"json\" };\n",
                "export { renamed as item }; export * as ns from './other.js';\n",
                "const obj = { value: 1, method(x) { return x ?? this.value; }, ...main };\n",
                "class Box extends Base { #secret = 1; static value = 2; get size() { return this.#secret; } static { this.ready = true; } }\n",
                "async function run(values) { outer: for await (const value of values) { if (value?.ok) continue outer; try { await value.work?.(); } catch ({message}) { throw message; } finally { debugger; } } return import('./lazy.js', { with: { type: 'json' } }); }\n",
                "export default (flag => flag ? obj.method(1) : new Box().size)(true);"
            ),
        ),
        (
            SourceType::TypeScript,
            concat!(
                "interface Shape { value: number } type Choice = string | number;\n",
                "const read = <T extends Shape>(value: T): number => value.value;\n",
                "const current = ({ value: 1 } as Shape) satisfies Shape;\n",
                "export const answer: Choice = read(current!);"
            ),
        ),
        (
            SourceType::Jsx,
            "const view = <section data-kind=\"card\"><span>{value}</span>{...children}</section>; export { view };",
        ),
        (
            SourceType::Tsx,
            "type Props = { title: string }; const View = ({ title }: Props) => <main><h1>{title}</h1></main>; export default View;",
        ),
    ];

    for minify in [false, true] {
        for (source_type, source) in fixtures {
            let (plain, mapped, mappings) = typed_codegen_fixture(source, source_type, minify);
            assert_eq!(
                plain, mapped,
                "mapping changed typed output for {source_type:?}"
            );
            assert!(
                !parse(&plain, &Interner::new(), SourceType::Module).has_errors(),
                "typed output did not reparse for {source_type:?}:\n{plain}"
            );
            assert!(!mappings.is_empty(), "typed mapped output had no mappings");
        }
    }
}

#[test]
fn typed_ir_codegen_maps_original_names_and_generated_punctuation() {
    let source = "const descriptiveName = 1; export default descriptiveName + 2;";
    let (plain, mapped, mappings) = typed_codegen_fixture(source, SourceType::Module, true);
    assert_eq!(plain, mapped);
    assert!(mappings.names.iter().any(|name| name == "descriptiveName"));
    let original_offset = source.find("descriptiveName").unwrap() as u32;
    assert!(mappings.mappings.iter().any(|mapping| {
        !mapping.is_unmapped
            && mapping.src_offset == original_offset
            && mapping
                .name_index
                .and_then(|index| mappings.names.get(index as usize))
                .is_some_and(|name| name == "descriptiveName")
    }));
    assert!(
        mappings.mappings.iter().any(|mapping| mapping.is_unmapped),
        "generated punctuation must create source-map fences: {mappings:?}\n{mapped}"
    );
}

#[test]
fn typed_ir_codegen_reparses_operator_pattern_and_statement_matrix() {
    let fixtures = [
        concat!(
            "let a=1,b=2,c=3,obj={a:1},arr=[1,2],fn=x=>x;\n",
            "a+=b;a-=b;a*=b;a/=b;a%=b;a**=b;a<<=b;a>>=b;a>>>=b;a&=b;a|=b;a^=b;a&&=b;a||=b;a??=b;\n",
            "const binary=[a+b,a-b,a*b,a/b,a%b,a**b,a&b,a|b,a^b,a<<b,a>>b,a>>>b,a==b,a!=b,a===b,a!==b,a<b,a>b,a<=b,a>=b,'a' in obj,obj instanceof Object];\n",
            "const logical=[a&&b,a||b,a??b,(a&&b)??c]; const unary=[-a,+a,!a,~a,typeof a,void a,delete obj.a];\n",
            "a++;--b; const calls=[fn?.(a),new Date(),obj?.a,obj?.[a],(a,b,c),fn`x${a}y`,[...arr],{...obj},/a+/gi,1n,`v${a}`];\n",
            "const holes=[,,]; const [,,third]=holes; const {a:renamed=1,...rest}=obj; ({a}=obj);\n",
            "outer:{if(a){break outer}else{debugger}} for(let i=0;i<2;i++){} for(const key in obj){} for(const value of arr){} while(a){break} do{a--}while(a);\n",
            "switch(a){case 1:continueLabel:while(false){continue continueLabel}break;default:throw new Error('x')}\n",
            "try{fn()}catch({message}){console.log(message)}finally{fn()}\n",
            "function* gen(){yield 1;yield* arr} async function asyncFn(){return await import('./lazy.js')}"
        ),
        concat!(
            "class Full extends Base { #private=1; static field=2; accessor value=3; constructor(){} get size(){return this.#private} set size(v){this.#private=v} async *items(){yield this.#private} ['computed'](){} static{this.ready=true} }\n",
            "const expression = class Named { method(){} }; const callable = function named(value){return value}; const arrow = value => ({value});"
        ),
        "using resource = open(); await using asyncResource = openAsync(); export { resource, asyncResource };",
    ];

    for source in fixtures {
        for minify in [false, true] {
            let (plain, mapped, _) = typed_codegen_fixture(source, SourceType::Module, minify);
            assert_eq!(plain, mapped);
            let reparsed = parse(&plain, &Interner::new(), SourceType::Module);
            assert!(
                !reparsed.has_errors(),
                "typed syntax matrix did not reparse:\n{plain}\n{:?}",
                reparsed.diagnostics
            );
        }
    }
}

#[test]
fn typed_ir_codegen_preserves_elisions_precedence_and_helper_directives() {
    let source = concat!(
        "'use strict';",
        "const holes=[,,];const [,,third]=holes;",
        "const power=(-a)**b;const mixed=(a&&b)??c;",
        "for((a in object,c);;){break}",
        "const made=new (factory())();",
        "const boolText=true.toString();",
        "({value}=source);"
    );
    for minify in [false, true] {
        let (plain, mapped, _) = typed_codegen_fixture(source, SourceType::Module, minify);
        assert_eq!(plain, mapped);
        assert!(
            plain.replace(' ', "").contains("[,,]"),
            "trailing elision was lost: {plain}"
        );
        assert!(
            !parse(&plain, &Interner::new(), SourceType::Module).has_errors(),
            "precedence fixture did not reparse:\n{plain}"
        );
    }

    let interner = Interner::new();
    let module = directive_helper_program(&interner);
    let (plain, mapped) = module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(DIRECTIVE_HELPER_SOURCE);
        input.minify = true;
        input.set_bundled_commonjs(true);
        let optimized = optimize_fixture(program, &interner, &input)
            .expect("helper fixture should optimize through the production pipeline");
        let plain = crate::codegen_optimized(&optimized, &interner, &NoLinker, true);
        let (mapped, _) = crate::codegen_optimized_with_map(&optimized, &interner, &NoLinker, true);
        (plain, mapped)
    });
    assert_eq!(plain, mapped);
    let strict = plain.find("\"use strict\"").expect("strict directive");
    let helper_names = emitted_top_level_function_names(&plain);
    assert_eq!(
        helper_names.len(),
        3,
        "expected all runtime helpers: {plain}"
    );
    let helper = plain
        .find(&format!("function {}", helper_names[0]))
        .expect("spread helper definition");
    assert!(
        strict < helper,
        "helper broke the directive prologue: {plain}"
    );
    assert!(
        !parse(&plain, &Interner::new(), SourceType::Script).has_errors(),
        "typed helper output did not reparse"
    );
}

#[test]
fn typed_runtime_helpers_match_legacy_runtime_and_mapping_is_inert() {
    let interner = Interner::new();
    let module = directive_helper_program(&interner);
    let legacy = module.with_ast(|program| codegen(program, &interner));
    let (typed, mapped, mappings) = module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(DIRECTIVE_HELPER_SOURCE);
        input.minify = true;
        input.set_bundled_commonjs(true);
        let optimized = optimize_fixture(program, &interner, &input)
            .expect("helper fixture should optimize through the production pipeline");
        let plain = crate::codegen_optimized(&optimized, &interner, &NoLinker, true);
        let (mapped, mappings) =
            crate::codegen_optimized_with_map(&optimized, &interner, &NoLinker, true);
        (plain, mapped, mappings)
    });
    assert_eq!(
        typed, mapped,
        "source maps must not change the JS token walk"
    );
    assert!(
        !parse(&typed, &Interner::new(), SourceType::Script).has_errors(),
        "materialized helper output must reparse:\n{typed}"
    );
    let expected_source_offsets = [
        "'wake-prologue'",
        "'use strict'",
        "keep",
        "__wake_iter",
        "__wake_object",
        "__wake_for_of",
        "boot",
    ]
    .into_iter()
    .map(|token| DIRECTIVE_HELPER_SOURCE.find(token).expect("fixture token") as u32)
    .chain(std::iter::once(0))
    .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        mappings
            .mappings
            .iter()
            .map(|mapping| mapping.src_offset)
            .collect::<std::collections::BTreeSet<_>>(),
        expected_source_offsets,
        "synthetic helper syntax must stay unmapped: {mappings:?}"
    );

    let helper_names = emitted_top_level_function_names(&typed);
    assert_eq!(
        helper_names.len(),
        3,
        "expected spread, object-spread, and for-of helper declarations: {typed}"
    );
    let iter_name = &helper_names[0];
    let object_name = &helper_names[1];
    let for_of_name = &helper_names[2];
    let checks = r#"
function keep() {}
function boot() {}
function probe() {
  var spreadClosed = 0, spreadIterable = {};
  spreadIterable[Symbol.iterator] = function() {
    var index = 0;
    return {
      next: function() { return index < 3 ? { done: false, value: ++index } : { done: true }; },
      return: function() { spreadClosed++; return {}; }
    };
  };
  var limited = __ITER__(spreadIterable, 2);
  var unicode = __ITER__("😀x", 2);
  var spreadNull = false;
  try { __ITER__(null); } catch (error) { spreadNull = error instanceof TypeError; }

  var symbol = Symbol("visible"), hidden = Symbol("hidden"), source = { a: 1 };
  source[symbol] = 2;
  Object.defineProperty(source, hidden, { value: 3, enumerable: false });
  var copied = __OBJECT__({}, null, source);
  var previous = {};
  Object.defineProperty(previous, "pair", { get: function() { return 7; }, configurable: true });
  var accessor = {};
  Object.defineProperty(accessor, "pair", { set: function(value) {}, enumerable: true, configurable: true });
  __OBJECT__.define(previous, accessor);
  var merged = Object.getOwnPropertyDescriptor(previous, "pair");
  var prototype = { marker: 9 }, child = __OBJECT__.proto({}, prototype);
  var rest = __OBJECT__.rest(source, ["a", symbol]);

  var forClosed = 0, forIterable = {};
  forIterable[Symbol.iterator] = function() {
    return {
      next: function() { return { done: false, value: 11 }; },
      return: function() { forClosed++; return {}; }
    };
  };
  var state = __FOR_OF__(forIterable);
  var lazy = forClosed === 0;
  state.s();
  var advanced = state.n() === false && state.v === 11;
  state.f();

  return JSON.stringify({
    limited: limited,
    unicode: unicode,
    spreadClosed: spreadClosed,
    spreadNull: spreadNull,
    copied: [copied.a, copied[symbol], Object.prototype.hasOwnProperty.call(copied, hidden)],
    descriptor: [typeof merged.get, typeof merged.set, merged.enumerable],
    prototype: Object.getPrototypeOf(child) === prototype,
    rest: [Object.keys(rest).length, Object.getOwnPropertySymbols(rest).length],
    forOf: [lazy, advanced, forClosed]
  });
}

process.stdout.write(probe());
"#;
    let typed_checks = checks
        .replace("__ITER__", iter_name)
        .replace("__OBJECT__", object_name)
        .replace("__FOR_OF__", for_of_name);
    let legacy_checks = checks
        .replace("__ITER__", "__wake_iter")
        .replace("__OBJECT__", "__wake_object")
        .replace("__FOR_OF__", "__wake_for_of");

    let node_available = std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success());
    if !node_available {
        return;
    }
    let execute = |helper: &str, checks: &str| {
        // The fixture calls `boot()` after the helper declarations. Hoisted function syntax in
        // the probe tail provides that binding before execution reaches the call.
        let output = std::process::Command::new("node")
            .arg("-e")
            .arg(format!("{helper}\n{checks}"))
            .output()
            .expect("run helper differential");
        assert!(
            output.status.success(),
            "helper runtime failed:\n{}\n{helper}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    let legacy_result = execute(&legacy, &legacy_checks);
    let typed_result = execute(&typed, &typed_checks);
    assert_eq!(
        typed_result, legacy_result,
        "typed helper behavior diverged from the previous implementation"
    );
}

#[test]
fn typed_decorator_lowering_reparses_maps_and_matches_legacy_runtime() {
    let source = concat!(
        "'use strict';\n",
        "let events=[];\n",
        "function dec(value,context){\n",
        " events.push('decorate:'+context.kind+':'+context.name);\n",
        " context.addInitializer(function(){events.push('extra:'+context.name)});\n",
        " if(context.kind==='field')return function(initial){return initial+10};\n",
        " if(context.kind==='method')return function(arg){events.push('call:'+context.name);return value.call(this,arg)+1};\n",
        " if(context.kind==='getter')return function(){return value.call(this)+2};\n",
        " if(context.kind==='setter')return function(arg){return value.call(this,arg*2)};\n",
        "}\n",
        "function classDec(value,context){events.push('decorate:'+context.kind+':'+context.name);context.addInitializer(function(){events.push('extra:class')});return class extends value{static replacement=7}}\n",
        "@classDec class Example{\n",
        " @dec field=1;\n",
        " @dec static staticField=2;\n",
        " @dec method(arg){return this.field+arg}\n",
        " @dec get current(){return this.field}\n",
        " @dec set next(arg){this.field=arg}\n",
        "}\n",
        "let instance=new Example;instance.next=3;globalThis.decoratorResult=[events,instance.method(4),instance.current,Example.staticField,Example.replacement];"
    );
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::TypeScript);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let legacy = parsed
        .module
        .with_ast(|program| codegen(program, &interner));
    let (typed, mapped, mappings) = optimized_bundled_fixture(source, SourceType::TypeScript, true);

    assert_eq!(
        typed, mapped,
        "source maps changed the decorator token walk"
    );
    assert!(
        !typed.contains('@'),
        "supported decorator syntax survived production lowering: {typed}"
    );
    assert!(
        typed.find("use strict").expect("directive")
            < typed
                .find("Function expected")
                .expect("materialized decorator helper"),
        "decorator helpers broke the directive prologue: {typed}"
    );
    let reparsed = parse(&typed, &Interner::new(), SourceType::Module);
    assert!(
        !reparsed.has_errors(),
        "materialized decorator output did not reparse:\n{typed}\n{:?}",
        reparsed.diagnostics
    );
    for source_offset in [
        source.find("class Example").expect("class definition") as u32,
        source.find("@classDec").expect("class decorator") as u32 + 1,
        source.find("@dec field").expect("field decorator") as u32 + 1,
    ] {
        assert!(
            mappings
                .mappings
                .iter()
                .any(|mapping| !mapping.is_unmapped && mapping.src_offset == source_offset),
            "source decorator/definition offset {source_offset} was not mapped: {mappings:?}\n{typed}"
        );
    }
    assert!(
        mappings.mappings.iter().any(|mapping| mapping.is_unmapped),
        "synthetic helper/wrapper punctuation must be fenced from source mappings"
    );

    let run = |javascript: &str| {
        let script = format!(
            "{javascript};process.stdout.write(JSON.stringify(globalThis.decoratorResult));"
        );
        let output = std::process::Command::new("node")
            .arg("-e")
            .arg(script)
            .output()
            .expect("run decorator output with Node");
        assert!(
            output.status.success(),
            "decorator output failed in Node:\n{javascript}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    assert_eq!(
        run(&typed),
        run(&legacy),
        "typed and legacy decorator runtimes differ"
    );
}

#[test]
fn typed_decorator_lowering_is_per_class_and_never_preserves_decorator_syntax() {
    let source = concat!(
        "function dec(value){return value}\n",
        "class Lowered{@dec field=1}\n",
        "class Preserved{@dec accessor value=2}\n"
    );
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::TypeScript);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let (plain, mapped, _) = optimized_bundled_fixture(source, SourceType::TypeScript, true);
    assert_eq!(plain, mapped);
    assert!(
        !plain.contains("@dec"),
        "decorator syntax survived: {plain}"
    );
    let reparsed = parse(&plain, &Interner::new(), SourceType::Module);
    assert!(
        !reparsed.has_errors(),
        "fully lowered output did not reparse:\n{plain}\n{:?}",
        reparsed.diagnostics
    );
}

#[test]
fn typed_decorator_lowering_handles_private_fields_and_all_class_contexts() {
    let runtime_source = concat!(
        "let contexts=[];function dec(value,context){contexts.push([context.name,context.private,context.static]);return initial=>initial+1}\n",
        "class PrivateFields{@dec #value=1;@dec static #staticValue=2;read(){return this.#value}static read(){return this.#staticValue}}\n",
        "globalThis.privateDecoratorResult=[new PrivateFields().read(),PrivateFields.read(),contexts];"
    );
    let interner = Interner::new();
    let parsed = parse(runtime_source, &interner, SourceType::TypeScript);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let (runtime, mapped, _) =
        optimized_bundled_fixture(runtime_source, SourceType::TypeScript, true);
    assert_eq!(runtime, mapped);
    assert!(
        !runtime.contains('@'),
        "private decorators survived: {runtime}"
    );
    assert!(
        !parse(&runtime, &Interner::new(), SourceType::Module).has_errors(),
        "private decorator output did not reparse: {runtime}"
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(format!(
            "{runtime};process.stdout.write(JSON.stringify(globalThis.privateDecoratorResult));"
        ))
        .output()
        .expect("run private-field decorators");
    assert!(
        output.status.success(),
        "private-field output failed:\n{runtime}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        br##"[2,3,[["#staticValue",true,true],["#value",true,false]]]"##
    );

    let contexts_source = concat!(
        "let classNames=[];function dec(value,context){if(context.kind==='class')classNames.push(context.name);return value}\n",
        "@dec class Named{}\n",
        "const Expression=class Inner{@dec item=1};\n",
        "globalThis.classDecoratorNames=classNames;\n",
        "@dec export default class {@dec item=2};"
    );
    let interner = Interner::new();
    let parsed = parse(contexts_source, &interner, SourceType::TypeScript);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let (output, mapped, _) =
        optimized_bundled_fixture(contexts_source, SourceType::TypeScript, true);
    assert_eq!(output, mapped);
    assert!(
        !output.contains('@'),
        "class decorator syntax survived: {output}"
    );
    let reparsed = parse(&output, &Interner::new(), SourceType::Module);
    assert!(
        !reparsed.has_errors(),
        "class expression/default export output did not reparse:\n{output}\n{:?}",
        reparsed.diagnostics
    );
    let runtime = std::process::Command::new("node")
        .arg("-e")
        .arg(format!(
            "var exports={{}},__wake_require__={{objectDefineProperty:Object.defineProperty}};{output};process.stdout.write(JSON.stringify(globalThis.classDecoratorNames));"
        ))
        .output()
        .expect("run named and anonymous class decorator contexts");
    assert!(
        runtime.status.success(),
        "class decorator context output failed:\n{output}\n{}",
        String::from_utf8_lossy(&runtime.stderr)
    );
    assert_eq!(runtime.stdout, br#"["Named",null]"#);
}

#[test]
fn typed_decorator_lowering_executes_private_methods_accessors_and_auto_accessors() {
    let source = concat!(
        "let methodAccess,getterAccess,accessorAccess,getterReads=0,inputFacts=[];",
        "function canConstruct(value){try{Reflect.construct(function(){},[],value);return true}catch{return false}}",
        "function dec(value,context){",
        "if(context.kind==='method'){inputFacts.push(value.name,canConstruct(value));methodAccess=context.access;return function(v){return value.call(this,v)+10}}",
        "if(context.kind==='getter'){inputFacts.push(value.name,canConstruct(value));getterAccess=context.access;return function(){getterReads++;return value.call(this)+1}};",
        "if(context.kind==='setter'){inputFacts.push(value.name,canConstruct(value));return function(v){return value.call(this,v*2)}};",
        "if(context.kind==='accessor'){inputFacts.push(value.get.name,canConstruct(value.get),value.set.name,canConstruct(value.set));accessorAccess=context.access;return {get(){getterReads++;return value.get.call(this)+1},set(v){value.set.call(this,v*2)},init(v){return v+3}}};",
        "return value}\n",
        "class PrivateElements{#back=1;\n@dec #method(v){return this.#back+v}\n",
        "@dec get #read(){return this.#back}\n@dec set #write(v){this.#back=v}\n",
        "@dec accessor #auto=2;\nmethodValue(){return this.#method}\n",
        "run(){this.#write=3;this.#auto=4;return [this.#method(5),this.#read,this.#auto]}}",
        "let instance=new PrivateElements,result=instance.run(),reads=getterReads;",
        "globalThis.privateElementDecoratorResult=[result,methodAccess.get(instance)===instance.methodValue(),",
        "getterAccess.has(instance),accessorAccess.has(instance),!getterAccess.has({}),!accessorAccess.has({}),getterReads===reads,inputFacts];"
    );
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::TypeScript);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let (output, mapped, _) = optimized_bundled_fixture(source, SourceType::TypeScript, true);
    assert_eq!(output, mapped);
    assert!(
        !output.contains('@'),
        "private decorators survived: {output}"
    );
    let reparsed = parse(&output, &Interner::new(), SourceType::Module);
    assert!(
        !reparsed.has_errors(),
        "private decorator output did not reparse:\n{output}\n{:?}",
        reparsed.diagnostics
    );
    let runtime = std::process::Command::new("node")
        .arg("-e")
        .arg(format!(
            "{output};process.stdout.write(JSON.stringify(globalThis.privateElementDecoratorResult));"
        ))
        .output()
        .expect("run private method/accessor decorators");
    assert!(
        runtime.status.success(),
        "private decorator output failed:\n{output}\n{}",
        String::from_utf8_lossy(&runtime.stderr)
    );
    assert_eq!(
        runtime.stdout,
        br##"[[21,7,9],true,true,true,true,true,true,["#method",false,"get #read",false,"set #write",false,"get #auto",false,"set #auto",false]]"##
    );
}

#[test]
fn typed_decorator_lowering_preserves_decorator_and_computed_key_evaluation_order() {
    let source = concat!(
        "let String=()=>{throw Error('shadowed String called')},events=[];function make(label){events.push('decorator-expression:'+label);",
        "return function(value,context){events.push('apply:'+context.name+':'+typeof context.name);return initial=>initial+1}}",
        "function key(label){events.push('key:'+label);return {toString(){events.push('coerce:'+label);return label}}}",
        "class C{@make('computed') [key('decorated')]=1;[key('plain')]=2}",
        "let instance=new C;globalThis.computedDecoratorResult=[events,[instance.decorated,instance.plain]];"
    );
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::TypeScript);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let (output, mapped, _) = optimized_bundled_fixture(source, SourceType::TypeScript, true);
    assert_eq!(output, mapped);
    let reparsed = parse(&output, &Interner::new(), SourceType::Module);
    assert!(
        !reparsed.has_errors(),
        "computed decorator output did not reparse:\n{output}\n{:?}",
        reparsed.diagnostics
    );
    let runtime = std::process::Command::new("node")
        .arg("-e")
        .arg(format!(
            "{output};process.stdout.write(JSON.stringify(globalThis.computedDecoratorResult));"
        ))
        .output()
        .expect("run computed decorators");
    assert!(
        runtime.status.success(),
        "computed decorator output failed:\n{output}\n{}",
        String::from_utf8_lossy(&runtime.stderr)
    );
    assert_eq!(
        runtime.stdout,
        br#"[["decorator-expression:computed","key:decorated","coerce:decorated","key:plain","coerce:plain","apply:decorated:string"],[2,2]]"#
    );
}

#[test]
fn typed_decorator_lowering_orders_initializers_across_plain_runtime_elements() {
    let source = concat!(
        "let events=[];function dec(value,context){events.push('decorate:'+context.name);",
        "context.addInitializer(function(){events.push('extra:'+context.name)});",
        "if(context.kind==='field')return function(initial){events.push('init:'+context.name);return initial};return value}",
        "class Base{}class C extends Base{",
        "@dec method(){}plain=(events.push('plain'),1);",
        "@dec field=(events.push('field-value'),2);later=(events.push('later'),3);",
        "static plainStatic=(events.push('static-plain'),1);",
        "@dec static decoratedStatic=(events.push('static-value'),2);",
        "static{events.push('static-block')}accessor automatic=(events.push('plain-accessor'),4);",
        "constructor(){super();events.push('body')}}new C;",
        "let privateFieldAccess;function capture(value,context){privateFieldAccess=context.access;return initial=>initial}",
        "function check(value,context){context.addInitializer(function(){events.push('private-before:'+privateFieldAccess.has(this))});return value}",
        "class P{@check method(){}@capture #field=1}new P;globalThis.decoratorInitializerOrder=events;"
    );
    let (output, mapped, _) = optimized_bundled_fixture(source, SourceType::TypeScript, true);
    assert_eq!(output, mapped);
    assert!(
        !output.contains('@') && !output.contains("accessor automatic"),
        "decorator/auto-accessor syntax survived:\n{output}"
    );
    let reparsed = parse(&output, &Interner::new(), SourceType::Module);
    assert!(
        !reparsed.has_errors(),
        "initializer-order output did not reparse:\n{output}\n{:?}",
        reparsed.diagnostics
    );
    let runtime = std::process::Command::new("node")
        .arg("-e")
        .arg(format!(
            "{output};process.stdout.write(JSON.stringify(globalThis.decoratorInitializerOrder));"
        ))
        .output()
        .expect("run decorator initializer ordering fixture");
    assert!(
        runtime.status.success(),
        "decorator initializer output failed:\n{output}\n{}",
        String::from_utf8_lossy(&runtime.stderr)
    );
    assert_eq!(
        runtime.stdout,
        br#"["decorate:method","decorate:decoratedStatic","decorate:field","static-plain","static-value","init:decoratedStatic","extra:decoratedStatic","static-block","extra:method","plain","field-value","init:field","extra:field","later","plain-accessor","body","private-before:false"]"#
    );
}

#[test]
fn typed_ir_codegen_expands_renamed_shorthand_and_maps_the_original_name() {
    use wake_ecma_minify::codegen_bridge::{IrNodeData, NameRole, TypedProgram};

    let source = concat!(
        "function make(descriptiveName){return {descriptiveName};}",
        "function read({descriptiveName}){return descriptiveName;}"
    );
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let (plain, mapped, mappings) = parsed.module.with_ast(|program| {
        let mut typed = TypedProgram::lower(program, &interner, None).expect("typed lowering");
        let rename = typed
            .nodes()
            .iter()
            .filter_map(|node| {
                let IrNodeData::Name { name } = node.data() else {
                    return None;
                };
                let record = typed.name(*name)?;
                (record.original() == "descriptiveName"
                    && matches!(
                        record.role(),
                        NameRole::Binding | NameRole::Reference | NameRole::AssignmentTarget
                    ))
                .then_some(*name)
            })
            .collect::<Vec<_>>();
        for name in rename {
            typed
                .set_emitted_name(name, "a")
                .expect("rename occurrence");
        }
        let plain = crate::codegen_typed(&typed, true);
        let (mapped, mappings) = crate::codegen_typed_with_map(&typed, true);
        (plain, mapped, mappings)
    });
    assert_eq!(plain, mapped);
    assert!(
        plain.matches("descriptiveName:a").count() >= 2,
        "renamed shorthand was not expanded: {plain}"
    );
    assert!(mappings.names.iter().any(|name| name == "descriptiveName"));
    let colon =
        plain.find("descriptiveName:a").expect("expanded shorthand") + "descriptiveName".len();
    let prefix = &plain[..colon];
    let colon_line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let colon_col = prefix
        .rsplit_once('\n')
        .map_or(prefix, |(_, tail)| tail)
        .encode_utf16()
        .count() as u32;
    assert!(mappings.mappings.iter().any(|mapping| {
        mapping.is_unmapped && (mapping.gen_line, mapping.gen_col) == (colon_line, colon_col)
    }));
    assert!(
        !parse(&plain, &Interner::new(), SourceType::Module).has_errors(),
        "renamed shorthand output did not reparse: {plain}"
    );
}

#[test]
fn typed_ir_codegen_maps_derived_module_names_to_their_anchor() {
    let source = "import value from './dep.js'; export { value };";
    let (_, generated, mappings) = typed_codegen_fixture(source, SourceType::Module, true);
    let specifier = generated.find("'./dep.js'").unwrap_or_else(|| {
        generated
            .find("\"./dep.js\"")
            .expect("generated module specifier")
    });
    let prefix = &generated[..specifier];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix, |(_, tail)| tail)
        .encode_utf16()
        .count() as u32;
    let active = mappings
        .mappings
        .iter()
        .rfind(|mapping| {
            (mapping.gen_line < line) || (mapping.gen_line == line && mapping.gen_col <= column)
        })
        .expect("mapping active at module specifier");
    assert!(!active.is_unmapped && active.src_offset == 0, "{active:?}");
}

#[test]
fn typed_ir_codegen_does_not_turn_parenthesized_strings_into_directives() {
    let source = "(\"use strict\"); function receiver(){ return this; }";
    for minify in [false, true] {
        let (plain, mapped, _) = typed_codegen_fixture(source, SourceType::Script, minify);
        assert_eq!(plain, mapped);
        assert!(
            plain.contains("(\"use strict\")") || plain.contains("('use strict')"),
            "parenthesized string lost its non-directive provenance: {plain}"
        );
        let reparsed = parse(&plain, &Interner::new(), SourceType::Script);
        assert!(
            !reparsed.has_errors(),
            "{:?}\n{plain}",
            reparsed.diagnostics
        );
        reparsed.module.with_ast(|program| {
            assert!(
                !program.strict,
                "parenthesized string became a strict-mode directive: {plain}"
            );
        });
    }
}

#[test]
fn typed_ir_codegen_maps_trusted_synthetic_tokens_but_fences_optimizer_syntax() {
    use wake_common::Span;
    use wake_ecma_minify::codegen_bridge::{
        IrNodeData, IrOrigin, SyntheticOriginKind, TypedProgram,
    };

    let source = "const trusted = 1, optimized = 2;";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let (generated, mappings) = parsed.module.with_ast(|program| {
        let mut typed = TypedProgram::lower(program, &interner, None).expect("typed lowering");
        let numbers = typed
            .nodes()
            .iter()
            .filter_map(|node| {
                matches!(node.data(), IrNodeData::NumberLiteral { .. }).then_some(node.id())
            })
            .collect::<Vec<_>>();
        assert_eq!(numbers.len(), 2);
        typed
            .set_origin(
                numbers[0],
                IrOrigin::Synthetic {
                    anchor: Some(Span::new(
                        source.find('1').expect("trusted literal") as u32,
                        source.find('1').expect("trusted literal") as u32 + 1,
                    )),
                    kind: SyntheticOriginKind::TrustedEdit,
                },
            )
            .expect("trusted origin");
        typed
            .set_origin(
                numbers[1],
                IrOrigin::Synthetic {
                    anchor: Some(Span::new(
                        source.find('2').expect("optimizer literal") as u32,
                        source.find('2').expect("optimizer literal") as u32 + 1,
                    )),
                    kind: SyntheticOriginKind::Optimization,
                },
            )
            .expect("optimizer origin");
        crate::codegen_typed_with_map(&typed, true)
    });

    let active_mapping = |byte_offset: usize| {
        let prefix = &generated[..byte_offset];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
        let column = prefix
            .rsplit_once('\n')
            .map_or(prefix, |(_, tail)| tail)
            .encode_utf16()
            .count() as u32;
        mappings
            .mappings
            .iter()
            .rfind(|mapping| {
                mapping.gen_line < line || (mapping.gen_line == line && mapping.gen_col <= column)
            })
            .copied()
            .expect("active mapping")
    };
    let trusted = generated.find("=1").expect("trusted output") + 1;
    let optimized = generated.find("=2").expect("optimizer output") + 1;
    let trusted_mapping = active_mapping(trusted);
    assert!(
        !trusted_mapping.is_unmapped
            && trusted_mapping.src_offset == source.find('1').unwrap() as u32,
        "{trusted_mapping:?}\n{generated}"
    );
    assert!(
        active_mapping(optimized).is_unmapped,
        "optimizer-created token must stay unmapped: {mappings:?}\n{generated}"
    );
}

#[test]
fn preserved_optimized_mapping_uses_optimizer_state_and_keeps_identical_bytes() {
    let source = "globalThis.compute=function(descriptiveParameter){return process.env.NODE_ENV==='production'?descriptiveParameter:0}";
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);

    let (plain, mapped, mappings) = parsed.module.with_ast(|program| {
        let mut input = wake_ecma_minify::OptimizeInput::new(source);
        input.minify = true;
        input.set_preserve_commonjs(true);
        input
            .defines
            .push(wake_ecma_minify::ValidatedDefine::primitive(
                "process.env.NODE_ENV",
                wake_ecma_minify::ConstVal::Str("production".into()),
            ));
        let optimized = optimize_fixture(program, &interner, &input).expect("optimizer");
        let plain = crate::codegen_preserved_optimized(
            &optimized,
            &interner,
            PreserveModuleFormat::CommonJs,
            &ExtensionRewriter(".cjs"),
        );
        let (mapped, mappings) = crate::codegen_preserved_optimized_with_map(
            &optimized,
            &interner,
            PreserveModuleFormat::CommonJs,
            &ExtensionRewriter(".cjs"),
        );
        (plain, mapped, mappings)
    });

    assert_eq!(plain, mapped, "mapping collection changed optimized bytes");
    assert!(!mapped.contains("process.env.NODE_ENV"), "{mapped}");
    assert!(
        !mappings.is_empty(),
        "optimized preserve output must be mapped"
    );
}
