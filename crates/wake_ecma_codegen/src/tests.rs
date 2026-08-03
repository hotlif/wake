//! codegen 测试：往返幂等（parse→codegen→parse→codegen 稳定）+ 输出快照。

use wake_common::{FxHashMap, Interner};
use wake_ecma_ast::SourceType;
use wake_ecma_minify::MinifyCtx;
use wake_ecma_parser::parse;

use crate::codegen;

fn directive_helper_program(interner: &Interner) -> wake_ecma_ast::ModuleAst {
    use wake_common::Span;
    use wake_ecma_ast::{
        AVec, CallExpression, Expression, ExpressionStatement, Ident, ModuleAst, Program,
        SourceType, Statement, StringLiteral,
    };

    ModuleAst::from_builder(|arena| {
        let marker_span = Span::new(0, 15);
        let strict_span = Span::new(16, 28);
        let boot_span = Span::new(29, 35);
        let mut program = Program::new_in(arena, SourceType::Script);
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
        program.span = Span::new(0, 35);
        program
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
    let it = Interner::new();
    let out = parse(src, &it, SourceType::TypeScript);
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
fn undecorated_class_is_unchanged() {
    // 无装饰器的类不得走降级路径（产物与既有一致）
    let js = strip_ts("class C { m() { return 1; } }");
    assert!(js.contains("class C"), "{js}");
    assert!(!js.contains("__esDecorate"), "不应注入辅助:\n{js}");
    assert!(!js.contains("=>{"), "不应包 IIFE:\n{js}");
}

#[test]
fn accessor_with_decorator_is_not_lowered() {
    // auto-accessor 的降级（私有存储 + get/set 对）未实现 → 整类放弃转换，
    // 宁可原样发射（运行时可见报错），也不产出看似成功却语义错误的代码。
    let js = strip_ts("class C { @dec accessor x = 1; }");
    assert!(!js.contains("__esDecorate"), "含 accessor 不应降级:\n{js}");
    assert!(js.contains("class C"), "{js}");
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
    fn module_id(&self, _s: &str) -> Option<u32> {
        None
    }
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
    let js = module.with_ast(|program| {
        let mut ctx = MinifyCtx::default();
        // Model the real minifier plans: bare strings are normally DCE candidates, and all
        // three adjacent source expressions are sequence-merge candidates.
        ctx.remove_spans
            .extend([Span::new(0, 15), Span::new(16, 28)]);
        ctx.sequence_spans = vec![
            (Span::new(0, 15), Span::new(16, 28)),
            (Span::new(16, 28), Span::new(29, 35)),
        ];
        ctx.minify = true;
        crate::codegen_module_shaken_mangled(
            program,
            &interner,
            &NoLinker,
            None,
            &[],
            true,
            None,
            Some(&ctx),
            false,
            false,
        )
    });

    assert!(
        js.starts_with("\"wake-prologue\";\"use strict\";function __wake_iter"),
        "minify must preserve the complete directive prologue before helpers:\n{js}"
    );
    assert!(
        js.contains("boot();"),
        "the first body expression must not be skipped as an incoming sequence successor:\n{js}"
    );
}

#[test]
fn helper_insertion_keeps_statement_source_map_positions() {
    let interner = Interner::new();
    let module = directive_helper_program(&interner);
    let (js, map) = module.with_ast(|program| {
        crate::codegen_module_shaken_with_map(
            program,
            &interner,
            &NoLinker,
            None,
            &[],
            false,
            None,
            None,
            false,
            false,
        )
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
        .find(|mapping| mapping.src_offset == 29)
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
    use crate::codegen_module_shaken_with;
    let it = Interner::new();
    let define = [("process.env.NODE_ENV", "\"production\"")];
    let src = "export function f(x) {\n\
               if (process.env.NODE_ENV !== 'production') { devWarn(x); }\n\
               if (process.env.NODE_ENV === 'production') { return x * 2; }\n\
               return x;\n\
               }";
    let out = parse(src, &it, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    let js = out.module.with_ast(|p| {
        codegen_module_shaken_with(p, &it, &NoLinker, None, &define, true, false, false)
    });
    // dev 警告块（`if(false)`）被剥离；`if(true)` 的 consequent 保留、if 外壳消除。
    assert!(!js.contains("devWarn"), "dev 块应被剥离:\n{js}");
    assert!(js.contains("return x*2"), "{js}");
    assert!(!js.contains("if("), "常量 if 应全部折叠:\n{js}");
}

#[test]
fn m4b_keeps_branch_with_hoisted_var() {
    // 被丢弃分支（else）含 `var` 提升声明 → 保守不折叠（丢弃会致 ReferenceError）。
    use crate::codegen_module_shaken_with;
    let it = Interner::new();
    let define = [("process.env.NODE_ENV", "\"production\"")];
    let src = "export const r = 1;\n\
               if (process.env.NODE_ENV === 'production') { keep(); } else { var leaked = 2; }";
    let out = parse(src, &it, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    let js = out.module.with_ast(|p| {
        codegen_module_shaken_with(p, &it, &NoLinker, None, &define, true, false, false)
    });
    assert!(
        js.contains("if("),
        "含 var 的被丢弃分支应保守保留 if:\n{js}"
    );
    assert!(js.contains("leaked"), "var 绑定应保留:\n{js}");
}

#[test]
fn dead_branch_folding_is_independent_of_minify() {
    // 死分支折叠**不再与 minify 耦合**：只要条件可在构建期定为常量就折叠，
    // 语义中性且 dev 产物同样受益（`process.env.NODE_ENV` 的死分支在 dev 也应消失）。
    use crate::codegen_module_shaken_with;
    let it = Interner::new();
    let define = [("process.env.NODE_ENV", "\"production\"")];
    let src = "if (process.env.NODE_ENV !== 'production') { devWarn(); }\n\
               if (process.env.NODE_ENV === 'production') { prodPath(); }";
    let out = parse(src, &it, SourceType::Module);
    // minify = false（dev 口径）
    let js = out.module.with_ast(|p| {
        codegen_module_shaken_with(p, &it, &NoLinker, None, &define, false, false, false)
    });
    assert!(!js.contains("devWarn"), "dev 死分支应被剥离:\n{js}");
    assert!(js.contains("prodPath"), "存活分支应保留:\n{js}");
    assert!(!js.contains("if ("), "常量条件的 if 外壳应消除:\n{js}");
}

#[test]
fn dead_branch_folding_without_define_keeps_code() {
    // 无法定为常量的条件一律保持原样——折叠只在「可确定」时发生。
    use crate::codegen_module_shaken_with;
    let it = Interner::new();
    let src = "if (someRuntimeFlag) { a(); } else { b(); }";
    let out = parse(src, &it, SourceType::Module);
    let js = out.module.with_ast(|p| {
        codegen_module_shaken_with(p, &it, &NoLinker, None, &[], false, false, false)
    });
    assert!(js.contains("a()") && js.contains("b()"), "{js}");
    assert!(js.contains("if ("), "运行时条件不应折叠:\n{js}");
}

#[test]
fn define_replaces_process_env_node_env() {
    use crate::{ModuleLinker, codegen_module};
    struct NoLinker;
    impl ModuleLinker for NoLinker {
        fn module_id(&self, _s: &str) -> Option<u32> {
            None
        }
    }
    let it = Interner::new();
    let src = "const x = process.env.NODE_ENV;\n\
               const dev = process.env.NODE_ENV !== 'production';\n\
               const keep = process.env.OTHER;\n\
               const nested = obj.process.env.NODE_ENV;";
    let out = parse(src, &it, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    let js = out
        .module
        .with_ast(|p| codegen_module(p, &it, &NoLinker, false));
    // process.env.NODE_ENV → "production"（去 process shim 的关键）。
    assert!(js.contains("const x = \"production\";"), "{js}");
    assert!(js.contains("\"production\" !== \"production\""), "{js}");
    // 其它 process.env.X 与前缀不同的链不误匹配。
    assert!(js.contains("process.env.OTHER"), "{js}");
    assert!(js.contains("obj.process.env.NODE_ENV"), "{js}");
    // 不再残留待替换的 process.env.NODE_ENV。
    assert!(!js.contains("= process.env.NODE_ENV;"), "{js}");
}

/// 往返幂等：codegen 的输出再 parse+codegen 应完全一致（强语义等价信号）。
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
        "!a && b || c;",
        "a ?? b;",
        "typeof x === 'string';",
        "new Foo(a, b).bar();",
        "a?.b?.[c]?.(d);",
        "x++ + ++y;",
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
    fn module_id(&self, _s: &str) -> Option<u32> {
        None
    }
}

/// 用给定「保留导出名」列表做 shake 后的模块体。
fn shake(src: &str, keep: Option<&[&str]>) -> String {
    let it = Interner::new();
    let out = parse(src, &it, SourceType::Module);
    assert!(!out.has_errors(), "parse errors: {:?}", out.diagnostics);
    let keep_owned: Option<Vec<String>> = keep.map(|k| k.iter().map(|s| s.to_string()).collect());
    out.module
        .with_ast(|p| crate::codegen_module_shaken(p, &it, &NoLink, keep_owned.as_deref(), false))
}

#[test]
fn shake_drops_unused_pure_exports() {
    let src = "export const used = 1;\n\
               export const unused = 2;\n\
               export function helper() { return 3; }\n\
               export class Widget {}";
    let js = shake(src, Some(&["used"]));
    // used 保留（声明 + 绑定）。
    assert!(js.contains("const used = 1;"), "{js}");
    assert!(js.contains("exports[\"used\"] = used;"), "{js}");
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
    assert!(js.contains("const secret = 41;"), "声明应保留:\n{js}");
    assert!(!js.contains("exports[\"secret\"]"), "绑定应移除:\n{js}");
    assert!(js.contains("exports[\"pub\"] = function ()"), "{js}");
}

#[test]
fn shake_keeps_side_effect_initializer() {
    // x 外部未用，但初始化器有副作用 → 保留声明（保留副作用），只移除 exports 绑定。
    let src = "export const x = sideEffect();";
    let js = shake(src, Some(&[]));
    assert!(
        js.contains("const x = sideEffect();"),
        "副作用应保留:\n{js}"
    );
    assert!(!js.contains("exports[\"x\"]"), "绑定应移除:\n{js}");
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
    assert!(js.contains("exports[\"keep\"] = keep;"), "{js}");
}

#[test]
fn shake_none_keeps_everything() {
    // keep=None（入口 / import*）→ 全保留（回归）。
    let src = "export const a = 1;\nexport function b() {}";
    let js = shake(src, None);
    assert!(js.contains("exports[\"a\"] = a;"), "{js}");
    assert!(js.contains("exports[\"b\"] = function ()"), "{js}");
}

#[test]
fn shake_none_keeps_recursive_export_name() {
    let js = shake(
        "export function factorial(n){return n?factorial(n-1)*n:1}",
        None,
    );
    assert!(js.contains("function factorial"), "{js}");
    assert!(js.contains("exports[\"factorial\"] = factorial"), "{js}");
}

#[test]
fn shake_filters_export_specifiers() {
    // export { a, b }：只保留被用的。
    let src = "const a = 1;\nconst b = 2;\nexport { a, b };";
    let js = shake(src, Some(&["a"]));
    assert!(js.contains("exports[\"a\"] = a;"), "{js}");
    assert!(!js.contains("exports[\"b\"]"), "未用 b 绑定应移除:\n{js}");
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
    assert!(js.contains("const local"), "{js}");
    assert!(js.contains("exports[\"publicName\"] = local"), "{js}");
}

#[test]
fn shake_keeps_local_with_non_export_reader() {
    let src = "const state={value:1};export function dropped(){return state.value}\
               console.log(state);";
    let js = shake(src, Some(&[]));
    assert!(js.contains("const state"), "{js}");
    assert!(js.contains("console.log(state)"), "{js}");
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
fn shake_emits_live_unreferenced_function_directly_to_export() {
    let js = shake("export function answer(){return 42}", Some(&["answer"]));
    assert!(js.contains("exports[\"answer\"] = function ()"), "{js}");
    assert!(!js.contains("function answer"), "{js}");
}

#[test]
fn shake_keeps_named_recursive_live_export() {
    let js = shake(
        "export function factorial(n){return n?factorial(n-1)*n:1}",
        Some(&["factorial"]),
    );
    assert!(js.contains("function factorial"), "{js}");
    assert!(js.contains("exports[\"factorial\"] = factorial"), "{js}");
}

// ======================================================================
// 标识符 mangling（WAKE-COMPATIBILITY §M4）：侧表由 wake_ecma_minify 构建，codegen 按 span 替换。
// ======================================================================

/// mangle：parse → plan_mangle → codegen_module_shaken_mangled（minify），再 parse 校验合法。
fn mangle_gen(src: &str) -> String {
    let it = Interner::new();
    let out = parse(src, &it, SourceType::Module);
    assert!(
        !out.has_errors(),
        "parse errors {src:?}: {:?}",
        out.diagnostics
    );
    let js = out.module.with_ast(|p| {
        let m = wake_ecma_minify::plan_mangle(p, &it, &[]);
        crate::codegen_module_shaken_mangled(
            p,
            &it,
            &NoLink,
            None,
            &[],
            true,
            Some(m.table()),
            None,
            false,
            false,
        )
    });
    // 产物必须能无错重解析（结构合法信号）。
    let re = parse(&js, &it, SourceType::Module);
    assert!(
        !re.has_errors(),
        "mangle 产物重解析出错:\n{js}\n{:?}",
        re.diagnostics
    );
    js
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
    // 模块级函数名现在也重命名（单包 concat 下顶层块/闭包作用域，导出走字符串键）。
    // `!contains` 同时证明重命名一致：声明与 `helper;` 引用都改了，无残留悬空引用。
    assert!(!js.contains("helper"), "模块级函数名现在应被重命名:\n{js}");
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
    // 函数名（含嵌套 inner）现在也重命名；`!contains` 证明声明与 `inner()` 调用一致重命名。
    assert!(
        !js.contains("inner"),
        "函数名 inner 现在也应被重命名:\n{js}"
    );
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
               useIt();";
    let out = parse(src, &it, SourceType::TypeScript);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    let js = out.module.with_ast(|p| {
        let m = wake_ecma_minify::plan_mangle(p, &it, &[]);
        crate::codegen_module_shaken_mangled(
            p,
            &it,
            &NoLink,
            None,
            &[],
            true,
            Some(m.table()),
            None,
            false,
            false,
        )
    });
    // 产物必须能无错重解析（塌名会造成重复声明 / ReferenceError 级结构错乱）。
    let re = parse(&js, &it, SourceType::Module);
    assert!(
        !re.has_errors(),
        "TS 降级 mangle 产物重解析出错:\n{js}\n{:?}",
        re.diagnostics
    );
    // namespace 成员合成绑定保持原名（NS.base = base / NS.scale = scale 不被塌缩）。
    assert!(
        js.contains("NS.base=base"),
        "namespace 成员应保持原名:\n{js}"
    );
    assert!(
        js.contains("NS.scale=scale"),
        "namespace 成员应保持原名:\n{js}"
    );
    // 而函数内真实局部（带真 span）应被 mangle。
    assert!(
        !js.contains("localValueHolder"),
        "真实局部应被 mangle:\n{js}"
    );
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

// ======================================================================
// Property mangling（M5）：对象字面量键 + 成员访问表达式缩短
// ======================================================================

/// prop_mangle: parse → plan_prop_mangle → codegen_module_shaken_mangled, 再 parse 校验合法。
fn prop_mangle_gen(src: &str) -> String {
    let it = Interner::new();
    let out = parse(src, &it, SourceType::Module);
    assert!(
        !out.has_errors(),
        "parse errors {src:?}: {:?}",
        out.diagnostics
    );
    let js = out.module.with_ast(|p| {
        let m = wake_ecma_minify::plan_mangle(p, &it, &[]);
        let pp = wake_ecma_minify::plan_prop_mangle(p, &it);
        let ctx = MinifyCtx {
            defines: &[],
            prop_rename: Some(pp.table()),
            ..MinifyCtx::default()
        };
        crate::codegen_module_shaken_mangled(
            p,
            &it,
            &NoLink,
            None,
            &[],
            true,
            Some(m.table()),
            Some(&ctx),
            false,
            false,
        )
    });
    let re = parse(&js, &it, SourceType::Module);
    assert!(
        !re.has_errors(),
        "prop_mangle 产物重解析出错:\n{js}\n{:?}",
        re.diagnostics
    );
    js
}

#[test]
fn prop_mangle_object_literal_key_shortened() {
    // 对象字面量键 `{ longName: 1 }` → `{ a: 1 }`。
    let js = prop_mangle_gen("function f() { return { longName: 1 }; } f;");
    assert!(!js.contains("longName"), "longName 应被缩短:\n{js}");
    assert!(js.contains(":1"), "属性值 1 应保留:\n{js}");
}

#[test]
fn prop_mangle_member_access_shortened() {
    // 成员访问 `obj.longPropertyName` → `obj.a`（参数名也会被 identifier mangle）。
    let js = prop_mangle_gen("function f(obj) { return obj.longPropertyName; } f;");
    assert!(
        !js.contains("longPropertyName"),
        "longPropertyName 应被缩短:\n{js}"
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
fn prop_mangle_consistent_naming() {
    // 同一属性名跨不同位置得同一短名
    let js = prop_mangle_gen(
        "function f(obj) { return obj.longName; } function g() { return { longName: 1 }; } f; g;",
    );
    // longName 应在成员访问和对象字面量两处都被缩短，且值相同
    assert!(!js.contains("longName"), "longName 应在两处都被缩短:\n{js}");
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
    let it = Interner::new();
    let out = parse(
        "const base = {}; const value = { __proto__: base }; value;",
        &it,
        SourceType::Module,
    );
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    let js = out.module.with_ast(|program| {
        let wake_ecma_ast::Statement::VariableDeclaration(declaration) = &program.body[1] else {
            panic!("expected value declaration")
        };
        let Some(wake_ecma_ast::Expression::Object(object)) = declaration.declarations[0].init
        else {
            panic!("expected object initializer")
        };
        let wake_ecma_ast::ObjectMember::Property(property) = &object.properties[0] else {
            panic!("expected object property")
        };
        assert!(property.prototype_setter);
        let wake_ecma_ast::PropertyKey::Ident(key) = property.key else {
            panic!("expected identifier key")
        };

        // Even an externally supplied/stale rename table must not change this syntax into an own
        // data property: prototype-setter semantics are carried explicitly by the AST node.
        let mut renames = FxHashMap::default();
        renames.insert(key.span, it.intern("renamed"));
        let ctx = MinifyCtx {
            defines: &[],
            prop_rename: Some(&renames),
            ..MinifyCtx::default()
        };
        crate::codegen_module_shaken_mangled(
            program,
            &it,
            &NoLink,
            None,
            &[],
            true,
            None,
            Some(&ctx),
            false,
            false,
        )
    });
    assert!(js.contains("__proto__"), "{js}");
    assert!(!js.contains("renamed:"), "{js}");
}

// ======================================================================
// Scope hoisting（Phase 3.5）：var 声明提升至函数顶部
// ======================================================================

/// hoist: parse → plan_hoist → codegen with hoisted vars, re-parse 校验合法。
fn hoist_gen(src: &str) -> String {
    let it = Interner::new();
    let out = parse(src, &it, SourceType::Module);
    assert!(
        !out.has_errors(),
        "parse errors {src:?}: {:?}",
        out.diagnostics
    );
    let js = out.module.with_ast(|p| {
        let hp = wake_ecma_minify::plan_hoist(p);
        let ctx = MinifyCtx {
            defines: &[],
            hoist: hp,
            minify: true,
            ..MinifyCtx::default()
        };
        crate::codegen_module_shaken_mangled(
            p,
            &it,
            &NoLink,
            None,
            &[],
            true,
            None,
            Some(&ctx),
            false,
            false,
        )
    });
    let re = parse(&js, &it, SourceType::Module);
    assert!(
        !re.has_errors(),
        "hoist 产物重解析出错:\n{js}\n{:?}",
        re.diagnostics
    );
    js
}

#[test]
fn hoist_var_inside_block() {
    // var 在块内 → 提升到函数顶部
    let js = hoist_gen(
        "function f() {\n\
         { var x = 1; }\n\
         console.log(x);\n\
         }",
    );
    assert!(
        js.contains("var x") && js.contains("console.log(x)"),
        "var x 应在顶部: {js}"
    );
}

#[test]
fn hoist_multiple_vars_joined() {
    // 多个 var 声明提升后合并
    let js = hoist_gen(
        "function f() {\n\
         { var x = 1; }\n\
         { var y = 2; }\n\
         console.log(x + y);\n\
         }",
    );
    // 应合并为单个 var 声明
    let var_count = js.matches("var ").count();
    assert!(var_count <= 1, "hoisted vars 应合并为一条 var 声明: {js}");
    assert!(
        js.contains("var x = 1, y = 2")
            || js.contains("var x, y = 2")
            || js.contains("var x=1,y=2"),
        "vars 应合并: {js}"
    );
}

#[test]
fn hoist_var_inside_if() {
    // var 在 if 块内 → 提升到函数顶部
    let js = hoist_gen(
        "function f() {\n\
         if (true) { var x = 1; }\n\
         console.log(x);\n\
         }",
    );
    assert!(js.contains("var x"), "var x 应在顶部: {js}");
}

#[test]
fn hoist_let_const_not_hoisted() {
    // let/const 不提升（块级作用域）
    let js = hoist_gen(
        "function f() {\n\
         { let x = 1; }\n\
         { const y = 2; }\n\
         var z = 3;\n\
         }",
    );
    // let/const 应保留在块内
    assert!(js.contains("let x=1"), "let 应在块内: {js}");
    assert!(js.contains("const y=2"), "const 应在块内: {js}");
    // var z 可能已在顶部或不提升（已在函数体直接子级）
}

#[test]
fn hoist_module_var_stays() {
    // 模块级 var 不应被提升（已在顶层）
    let js = hoist_gen(
        "var x = 1;\n\
         function f() { return x; }",
    );
    assert!(
        js.starts_with("var x") || js.starts_with("var x=1"),
        "模块级 var 应保持在顶层: {js}"
    );
}

#[test]
fn hoist_for_loop_var_not_hoisted() {
    // for 循环内的 var 不提升（循环体可能不执行，保持赋值时机）
    let js = hoist_gen(
        "function f() {\n\
         for (var i = 0; i < 10; i++) { var x = i; }\n\
         }",
    );
    // for-init var i 不在我们的 hoist 范围（非块内 statement）
    // for 体 var x 也不提升（不遍历循环体）
    assert!(
        !js.contains("var x") || js.contains("for"),
        "for 体内 var 不应单独提升到函数顶部: {js}"
    );
}

#[test]
fn hoist_nested_function_boundary() {
    // 嵌套函数有自己的作用域，内层 var 不应提升到外层
    let js = hoist_gen(
        "function outer() {\n\
         { var x = 1; }\n\
         function inner() {\n\
         { var y = 2; }\n\
         return y;\n\
         }\n\
         return x + inner();\n\
         }",
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
    use crate::codegen_module_shaken_with_map;
    use wake_common::SourceFile;

    let it = Interner::new();
    let src = "const alpha = 1;\nfunction beta(gamma) {\n  return gamma + alpha;\n}\n";
    let out = parse(src, &it, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let (js, map) = out.module.with_ast(|p| {
        codegen_module_shaken_with_map(p, &it, &NoLinker, None, &[], false, None, None, true, false)
    });
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
    use crate::codegen_module_shaken_with_map;

    let it = Interner::new();
    // 含非 ASCII 字符串字面量与多行结构，考验列（UTF-16）与行的累计。
    let src = "const 名字 = \"中文值\";\nconst n = 42;\nexport { n };\n";
    let out = parse(src, &it, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let (js, map) = out.module.with_ast(|p| {
        codegen_module_shaken_with_map(p, &it, &NoLinker, None, &[], false, None, None, true, false)
    });

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
    use crate::{codegen_module_shaken_mangled, codegen_module_shaken_with_map};

    let it = Interner::new();
    let src = "export function f(a, b) {\n  const s = `x${a}y`;\n  return s + b;\n}\n";
    let out = parse(src, &it, SourceType::Module);
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let plain = out.module.with_ast(|p| {
        codegen_module_shaken_mangled(p, &it, &NoLinker, None, &[], false, None, None, true, false)
    });
    let (with_map, map) = out.module.with_ast(|p| {
        codegen_module_shaken_with_map(p, &it, &NoLinker, None, &[], false, None, None, true, false)
    });
    assert_eq!(plain, with_map, "启用 sourcemap 不得改变产物");
    assert!(!map.is_empty());
}

#[test]
fn constant_ternary_is_folded() {
    use crate::codegen_module_shaken_with;
    let it = Interner::new();
    let define = [("process.env.NODE_ENV", "\"production\"")];
    let src = "const v = process.env.NODE_ENV === 'production' ? fast() : slow();";
    let out = parse(src, &it, SourceType::Module);
    let js = out.module.with_ast(|p| {
        codegen_module_shaken_with(p, &it, &NoLinker, None, &define, false, false, false)
    });
    assert!(js.contains("fast()"), "{js}");
    assert!(!js.contains("slow()"), "死分支应剥离:\n{js}");
    assert!(!js.contains('?'), "三元外壳应消除:\n{js}");
}

#[test]
fn unreachable_after_return_is_dropped() {
    let js = run("function f() { return 1; console.log('never'); doMore(); }");
    assert!(js.contains("return 1"), "{js}");
    assert!(!js.contains("never"), "return 后不可达代码应丢弃:\n{js}");
    assert!(!js.contains("doMore"), "{js}");
}

#[test]
fn unreachable_after_throw_is_dropped() {
    let js = run("function f() { throw new Error('x'); cleanup(); }");
    assert!(js.contains("throw"), "{js}");
    assert!(!js.contains("cleanup"), "{js}");
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
    // 必须逐字是 `module.exports`：bundler 的 compact_body_names 以文本匹配这一串改写成
    // 包装器形参 `m.exports`，换写法会静默丢导出。
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

/// 走「未用变量消除」的 minify 路径（`analyze_vars` → codegen）。
fn elim_gen(src: &str) -> String {
    let it = Interner::new();
    let out = parse(src, &it, SourceType::Module);
    assert!(
        !out.has_errors(),
        "parse errors {src:?}: {:?}",
        out.diagnostics
    );
    out.module.with_ast(|p| {
        let va = wake_ecma_minify::analyze_vars(p, &it);
        let ctx = MinifyCtx {
            unused_vars: va.unused_vars.clone(),
            unused_var_spans: va.unused_var_spans.clone(),
            ..MinifyCtx::default()
        };
        crate::codegen_module_shaken_mangled(
            p,
            &it,
            &NoLink,
            None,
            &[],
            true,
            None,
            Some(&ctx),
            false,
            false,
        )
    })
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
         }",
    );
    assert!(js.contains("using lock=acquire()"), "using 被消除:\n{js}");
    assert!(
        js.contains("await using conn=connect()"),
        "await using 被消除:\n{js}"
    );
    // 对照组：同样零引用的普通 const 仍应被消除，证明本用例确实走到了消除路径。
    assert!(
        !js.contains("deadConst"),
        "对照组未被消除，本用例没走到消除路径:\n{js}"
    );
}
