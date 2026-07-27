# TypeScript 语法支持清单

> ⚠️ **本表经 2026-07-27 的实证复核修订**（真实构建 + node 跑产物 + 与 tsc 7.0.2 对拍）。
> 复核抽查的 19 条里改判了 5 条：`enum`（成员互引 ReferenceError / 反向映射丢失）、
> 嵌套 `namespace` 合并、`accessor` 字段与其装饰器（整包 SyntaxError）、`.tsx` 泛型箭头函数。
> **凡标 ✅ 的行，若未经「node 跑产物断言行为」验证过，都应视为待复核**——本次改判的 5 条
> 全部属于「构建成功、测试全绿、产出静默错误」。完整复核见
> [CRUSTIFY-PARITY-PLAN.md §1.2](CRUSTIFY-PARITY-PLAN.md)。

> 最后更新：2026-07-27 · 基于 TypeScript 7（`erasableSyntaxOnly` 模式可用）
>
> wake 采用 **解析器级类型擦除**（DESIGN §4.1）：解析时直接跳过类型语法，不构造类型 AST。
> `enum`/`namespace` 等值语义构造降级为 IIFE；`import type`/`export type` 等类型-only 语法擦除为空。
> 不做类型检查，与 esbuild/SWC 策略一致。

## 图例

| 标记 | 含义 |
|------|------|
| ✅ | 完整支持 |
| ⚠️ | 部分支持（见说明） |
| ❌ | 不支持 |
| 🔴 高 | 影响运行时行为 |
| 🟡 中 | 影响部分项目 |
| 🟢 低 | 边缘场景 |

---

## 1. 类型注解 / 类型文法

全部由 `crates/wake_ecma_parser/src/ts.rs`（类型消费器）处理。

| 特性 | 支持 | 说明 |
|------|------|------|
| 基本类型 `number` `string` `boolean` `null` `undefined` `void` `any` `never` `unknown` | ✅ | `ts_primary` 按 `Keyword` 匹配 |
| `object` `symbol` `bigint` | ✅ | 同上 |
| 联合类型 `A \| B` | ✅ | `ts_union()` |
| 交叉类型 `A & B` | ✅ | `ts_intersection()` |
| 函数类型 `(a: T) => R` | ✅ | `ts_function_type()` + 前瞻 lookahead |
| 构造类型 `new (x: T) => R` | ✅ | 同上 |
| 抽象构造类型 `abstract new () => R` | ✅ | 同上 |
| 条件类型 `T extends U ? X : Y` | ✅ | `ts_type()` 中 `extends` → `?` → `:` |
| 映射类型 `{ [K in T]: V }` | ✅ | `ts_skip_balanced()` 消费 `{}` |
| 对象类型 `{ a: T; b?: U; readonly c: V; [k: string]: W }` | ✅ | 同上 |
| 数组类型 `T[]` | ✅ | `ts_postfix()` |
| 元组类型 `[T, U]` / `[name: T, ...rest: V[]]` | ✅ | `ts_skip_balanced()` |
| 索引访问类型 `T[K]` | ✅ | `ts_postfix()` |
| 模板字面量类型 `` \`prefix-${T}-suffix\` `` | ✅ | `ts_template_literal_type()` |
| `import('mod').X` 类型 | ✅ | `ts_import_type()` |
| 类型引用 `T` / `A.B.C<T>` | ✅ | `ts_entity_name()` + `ts_type_arguments()` |
| `typeof` 类型查询 `typeof x` | ✅ | `ts_type_operand()` |
| `keyof T` | ✅ | 同上 |
| `readonly` 映射修饰符 | ✅ | 同上（contextual keyword） |
| `unique symbol` | ✅ | 同上 |
| `infer T` / `infer T extends U` | ✅ | 同上 |
| 字面量类型 `42` / `'hello'` / `true` / `false` | ✅ | `ts_primary()` |
| `this` 类型 | ✅ | `Keyword::This` |
| `const` 类型位置（`as const`、`<const T>`） | ✅ | `Keyword::Const` in `ts_primary()` |
| 泛型约束 `<T extends U>` | ✅ | `ts_type_parameters()` |
| 泛型默认值 `<T = U>` | ✅ | 同上 |
| `in`/`out` 方差标记 `<in T, out U>` | ✅ | 同上修饰符循环 |
| `const` 类型参数 `<const T>` | ✅ | 同上 |
| 类型实参 `<A, B>` | ✅ | `ts_type_arguments()` |
| 调用类型实参 `f<T>()`（歧义回溯） | ✅ | `try_ts_type_arguments()` + checkpoint/rewind |
| `>>`/`>>>` 拆分泛型关闭 | ✅ | `consume_type_gt()` |
| `as const` 断言类型位置 | ✅ | `ts_primary` → `Keyword::Const` |

---

## 2. 类型专用声明

| 特性 | 支持 | 位置 |
|------|------|------|
| `interface X { ... }` | ✅ | `stmt.rs:skip_interface()` 擦除为 `Empty` |
| `type X = T;` | ✅ | `stmt.rs:skip_type_alias()` 擦除为 `Empty` |
| `enum E { ... }`（值语义 → IIFE） | ⚠️ | `ts_value.rs:parse_enum()`。**两个已实证的缺陷**见下方 ⚠️ 框 |
| `const enum E { ... }`（值语义 → IIFE） | ⚠️ | `stmt.rs` → `parse_enum()`，同上 |
| `namespace N { ... }`（值语义 → IIFE） | ✅ | `ts_value.rs:parse_namespace()` |
| `module N { ... }`（值语义 → IIFE） | ✅ | 同 `namespace` |
| Ambient `module "..." { }` | ✅ | `ts_skip_ambient()` 通用消费 |
| 点分 namespace `A.B.C` | ✅ | 展开为嵌套 IIFE（成员挂最内层段） |
| namespace / enum **跨声明合并** | ✅ | 实参用 `X \|\| (X = {})`，同名声明复用已有对象 |
| 嵌套 `namespace`（`export namespace Inner`） | ⚠️ | 递归降级 + `N.Inner = Inner`。但**内层的跨声明合并失效**：IIFE 实参种子取内层新建的局部变量而非父对象属性（tsc 取 `Outer.Inner || (Outer.Inner = {})`），故第二个同名 `export namespace Inner` 不会复用第一个 |

> ⚠️ **enum 的两个已实证缺陷（2026-07-27，与 tsc 7.0.2 对拍）**
>
> 1. **成员引用同 enum 的前序成员会 ReferenceError**：`enum E { A = 1, B = A * 3 }` 发射出
>    `E["B"] = A * 3;` —— 裸 `A` 未改写为限定名 `E.A`（tsc 发的是 `E[E["B"] = E.A * 3] = "B"`）。
>    构建成功，运行即崩。
> 2. **非纯数字字面量成员丢失反向映射**：`enum L { Low = 1 << 0 }` 只发正向 `L["Low"] = 1`，
>    不发 `L[1] = "Low"`。**构建成功、node 不报错，只是 `L[1]` 静默返回 `undefined`** —— 比崩溃更隐蔽。
>    §9 对比表把本行与 esbuild/SWC 并列为 ✅ 属于夸大：那两者均正确产出反向映射并限定成员引用。

---

## 3. 类特性

| 特性 | 支持 | 位置 |
|------|------|------|
| `abstract class` | ✅ | `stmt.rs:92-97` 消费 `abstract` 后按 class 解析 |
| `public`/`private`/`protected` 成员 | ✅ | `stmt.rs:835-843` 擦除 |
| `readonly` 成员 | ✅ | `stmt.rs:846` 擦除 |
| `abstract` 成员 | ✅ | `stmt.rs:855` 擦除 |
| `declare` 成员 | ✅ | 同上 |
| `override` 成员 | ✅ | `stmt.rs:848` 擦除 |
| `accessor` 字段 (auto-accessor) | ❌ 🔴 | 修饰符解析并**原样发射**——但**没有任何 JS 引擎实现 auto-accessor**（实测 Node v24.14.1 / V8 13.6 直接 SyntaxError，`--harmony` 亦然）→ **一个 `accessor` 字段就让整包无法加载**（require 阶段即 throw），而 CLI 报「✓ 构建成功」、无任何诊断。按 §7 自述的发射方针「引擎跑不了的才降级」，本项恰属应降级却没降 |
| 参数属性 `constructor(public x)` | ✅ | `ts.rs:ts_skip_param_modifiers()` + `this.x = x` 注入 |
| 参数属性 `constructor(readonly x)` | ✅ | 同上 |
| 参数属性 `constructor(override x)` | ✅ | 同上 |
| `implements I, J<K>` | ✅ | `stmt.rs:781-789` 擦除 |
| `extends Base<T>` 类型实参 | ✅ | `stmt.rs:773-775` 擦除 |
| 可选属性 `x?: T` | ✅ | `stmt.rs:919` |
| 明确赋值 `x!: T` | ✅ | `stmt.rs:920` |
| 索引签名 `[k: T]: U` | ✅ | `stmt.rs:884-889` 擦除 |
| 重载签名 `foo(x: T): void;` | ✅ | 无体 → 擦除 |
| `this` 参数 `foo(this: T)` | ✅ | 函数参数解析，擦除 |
| `static {}` 块 | ✅ | `stmt.rs:865-881` |
| 装饰器 `@dec`（类/方法/取值器/设值器/字段） | ✅ | **TC39 Stage-3** 降级：`__esDecorate` + `__runInitializers`，与 `tsc --target es2022` 行为对拍一致 |
| `accessor x = 1` 上的装饰器 | ❌ 🔴 | 原记「整类放弃转换、原样发射（不产出错误语义）」，**两个安全承诺实测均不成立**：① 装饰器被**静默丢弃**（产物里既无 `@dec` 也无 `__esDecorate`），不是原样发射；② 结果是**整包 SyntaxError**（因 accessor 原样发射，见上行）。且「整类放弃」会连累同类中本可正确降级的方法/字段装饰器一并失效。另：类里存在**未加装饰器**的 `accessor` 时连这层兜底都不触发——装饰器编排照常生成、`accessor` 原样留下，产物同样 SyntaxError |
| 参数装饰器 `f(@dec x)` | ❌ | Stage-3 无参数装饰器（属 legacy），仍作擦除 |

---

## 4. 表达式

| 特性 | 支持 | 位置 |
|------|------|------|
| `as Type` 断言 | ✅ | `expr.rs:154` `parse_binary_expression` |
| `as const` 断言 | ✅ | 同上 → `ts_type()` → `ts_primary` 消费 `const` |
| `satisfies Type` | ✅ | `expr.rs:154` 同 `as` 处理 |
| `<Type>expr` 尖括号断言 | ✅ | `expr.rs:238`（仅 `.ts`，`.tsx` 中 `<` 是 JSX） |
| **`.tsx` 中的泛型箭头函数** | ❌ 🟡 | TS 官方用于消歧的两种写法 `<T,>(v) => v` 与 `<T extends U>(v) => v` 在 `.tsx` 下**均为硬解析错误**（WAKE0200）——`<` 被无条件当作 JSX 起始标签，未做 TSX 消歧回溯。`.ts` 文件不受影响 |
| 非空断言 `expr!` | ✅ | `expr.rs:444` |
| 类型谓词 `x is T` / `asserts x is T` | ✅ | `ts.rs:28-51` |
| 可选链 `?.` | ✅ | 词法器 + 解析器 |
| 空值合并 `??` | ✅ | 同上 |
| `??=` `\|\|=` `&&=` 逻辑赋值 | ✅ | 同上 |
| `new.target` | ✅ | 表达式解析 |

---

## 5. 模块（导入/导出）

| 特性 | 支持 | 位置 |
|------|------|------|
| `import type { ... }` | ✅ | `stmt.rs:1165-1183` 完整擦除，不记录运行时依赖 |
| `export type { ... }` | ✅ | `stmt.rs:1287-1302` 同上 |
| `import { type A, real }` 内联 type | ✅ | `stmt.rs:1232` 跳过 type 说明符 |
| `export { type A, actual }` 内联 type | ✅ | `stmt.rs:1356` |
| `export * as ns from '...'` | ✅ | `stmt.rs:1326-1346` |
| `export { } from '...'` | ✅ | `stmt.rs:1350-1401` |
| `import()` 动态导入 | ✅ | `import_is_expression()` |
| `import.meta` | ✅ | 表达式解析 |
| `import x = require('...')` | ✅ | `stmt.rs:parse_import_equals()` → `const x = require('...')`，右侧按表达式解析，依赖经 `maybe_record_require` 记为 `Require`、codegen `emit_require_call` 改写为 `__wake_require__(id)` |
| `import A = N.B.C`（实体名别名） | ✅ | 同上 → `var A = N.B.C;`（`var` 提升以配合命名空间声明合并，对齐 tsc） |
| `export import A = ...` | ✅ | 经 `parse_export` → `parse_statement` 复用同一分支；namespace 体内由 `lower_namespace_member` 挂成 `N.A = A` |
| `import type A = require('...')` | ✅ | 整条擦除，且**不记录**运行时依赖（解析后截断依赖列表） |
| `export = expr` | ✅ | `stmt.rs:parse_export()` → `ts_value.rs:module_exports_assign()` → `module.exports = expr;` |
| `export as namespace X` | ✅ | 纯类型，擦除为 `Empty` |
| Import attributes `with { type: "json" }` | ✅ | `stmt.rs:parse_import_attributes()`；`import` / `import ''` / `export {} from` / `export * from` 四处均支持 |
| Import assertions `assert { type: "json" }`（已废弃） | ✅ | 一并解析；**原样发射**为 `assert`，不静默改写为 `with`（改写会在旧运行时上改变语义） |
| **`import x = await import('...')`** | ❌ 🟢 | 受阻于**顶层 await 未支持**（`const a = await f()` 在模块顶层同样报错），与 import-equals 无关 |

> 引入属性仅在**非链接**路径发射；链接（打包）路径下目标模块已内联进产物，属性对运行时不再有意义。
> `.json` 的加载由 loader 按扩展名完成（`wake_bundler::loader::json_to_js_module` → `export default <json>`），
> 故 JSON 应走 ESM 默认导入；`require('./x.json')` 拿到的是 `{ default: … }` 而非裸对象。

---

## 6. 环境 / 声明

| 特性 | 支持 | 位置 |
|------|------|------|
| `declare const/let/var` | ✅ | `parse_declare()` → `ts_skip_ambient()` 通用消费 |
| `declare function` / `declare class` | ✅ | 同上 |
| `declare module "..." { }` | ✅ | 同上（`ts_skip_ambient` 直到 `{...}` 平衡） |
| `declare global { }` | ✅ | 同上（`ts_declare_is_declaration` 含 `global`） |
| `declare namespace N { }` | ✅ | 同上 |
| `export declare ...` | ✅ | `parse_export` → `parse_statement` → `parse_declare` |
| **`/// <reference path="..." />`** | ❌ 🟢 | 词法器无解析 |
| **`/// <reference types="..." />`** | ❌ 🟢 | 同上 |
| **`/// <amd-module` 等** | ❌ 🟢 | 同上 |

---

## 7. 运行时资源管理（`using`）

> TC39 stage 4 / TypeScript 5.2+，TS 7 默认启用

**实现口径：语法级完整支持 + 原样发射（pass-through），不做 downlevel 降级。**
wake 无 ES target 配置，一贯「引擎能跑的原样发射，引擎跑不了的才降级」（`enum`/`namespace`/装饰器属后者）。
`using` 已是 stage 3 且 V8 已实现（Node 24+ 原生可跑），故与 `??`/`?.`/`static {}` 同列，原样发射。
若日后需支持 Node 22 及以下，再补 tsc 口径的 `__addDisposableResource` / `__disposeResources` 降级。

| 特性 | 支持 | 位置 |
|------|------|------|
| `using x = expr` | ✅ | `VarKind::Using`；`stmt.rs:using_decl_here()` 按**上下文关键字**识别 |
| `await using x = expr` | ✅ | `VarKind::AwaitUsing`；2-token 前瞻用 checkpoint 试探 |
| `for (using x of xs)` | ✅ | `stmt.rs:for_head_var_kind()` |
| `for (await using x of xs)` / `for await (using x of xs)` | ✅ | 同上 |
| `using` 作普通标识符（`using = 1` / `using.p` / `using`+换行） | ✅ | 靠「同行紧跟绑定名」前瞻区分 |
| `for (using of xs)` 中 `using` 是循环变量 | ✅ | 规范为消歧显式排除该形态，`for_head_var_kind()` 特判 |
| **模块顶层 `await using`** | ⚠️ | **解析期报错**（非静默产坏码）。根因是顶层 `await` 整体未支持，且 bundler 的模块包装器 `function(module, exports, __wake_require__)` 非 async |

**管线协同**（`using` 带 dispose 副作用，所有「未用即删」优化都必须放过它）：

| 位置 | 处理 |
|------|------|
| `semantic.rs` | 新增 `DeclKind::Using`（作用域规则同 `Const`） |
| `minify/analyze.rs` | 与 `DeclKind::Import` 一并跳过「无引用即删」与「单次引用内联」 |
| `codegen/lib.rs` | `emit_var_decl_elim` / `emit_joined_vars` 对 using 直接原样发射（第二道防线） |
| `wake_graph/lib.rs` | using 声明恒作**副作用根**，不进可 DCE 的 `ml.decls`（即使 init 是纯表达式） |
| `minify/statements.rs` | 不参与相邻声明合并 |
| `minify/mangle.rs` | `is_reserved` 加 `"using"`，避免生成名为 `using` 的变量导致语句起始处被误解析 |
| `ast/hash.rs` | `structure_hash` 混入 `VarKind` 判别式（否则 `var`↔`using` 的编辑指纹相同 → 增量缓存产出陈旧包） |

---

## 8. 标准 JavaScript 特性

| 特性 | 支持 |
|------|------|
| `#private` 字段 (PrivateIdent) | ✅ |
| `static {}` 初始化块 | ✅ |
| `for await...of` | ✅ |
| BigInt `0n` | ✅ |
| 模板字面量（含标签模板 `` tag\`...\` ``） | ✅ |
| 正则字面量 | ✅ |
| 解构（含 `...rest`、嵌套、默认值） | ✅ |
| 箭头函数 | ✅ |
| `async`/`await` | ✅ |
| `yield*` 委托 | ✅ |
| `import()` 动态导入 | ✅ |
| `export default` | ✅ |
| 逗号序列表达式 | ✅ |
| `Object`/`Array`/`class` 字面量 | ✅ |

---

## 9. 与 esbuild/SWC 对比

| 特性 | wake | esbuild | SWC |
|------|------|---------|-----|
| 类型擦除（注解/接口/类型别名） | ✅ | ✅ | ✅ |
| `enum` → IIFE | ⚠️⁵ | ✅ | ✅ |
| `namespace` → IIFE（含点分名/合并） | ⚠️⁶ | ❌¹ | ✅ |
| 参数属性 `this.x = x` | ✅ | ✅ | ✅ |
| `import type` / `export type` | ✅ | ✅ | ✅ |
| `using` / `await using` | ✅（原样发射） | ✅ | ✅ |
| Import attributes `with` | ✅ | ✅ | ✅ |
| 装饰器下游 emit | ✅（Stage-3） | ✅ | ✅ |
| `import = require` | ✅ | ❌³ | ❌³ |
| `export =` | ✅ | ❌³ | ❌³ |

> ¹ esbuild 不支持 `namespace` 降级。[^1]  
> ² wake 消费装饰器语法但不构建 AST，无法做 `__decorate` 或 stage 3 运行时 emit。  
> ³ esbuild/SWC 推荐使用 ESM 等价写法。wake 支持是因为其模块包装器
> （`function(module, exports, __wake_require__)`）天然提供 CJS 三件套，
> `import = require` 与 `export =` 可直接复用既有 CJS interop，无需新增运行时。  
> ⁴ `using` 一行的口径差异：esbuild/SWC 在低 target 下会**降级**为 try/finally + helper，
> wake 无 target 配置，一律原样发射（需 Node 24+ / 支持该提案的引擎）。  
> ⁵ 原标 ✅ 与 esbuild/SWC 并列属**夸大**：那两者正确产出反向映射并把成员引用改写为限定名，
> wake 两者都不做（见 §2 的 ⚠️ 框）。  
> ⁶ 顶层与点分名的合并可用，但 namespace **体内**的 `export namespace`/`export enum` 合并失效（见 §2）。

[^1]: https://esbuild.github.io/content-types/#typescript-namespaces

---

## 10. 代码位置速查

| 组件 | 文件 | 职责 |
|------|------|------|
| 类型消费器（擦除引擎） | `crates/wake_ecma_parser/src/ts.rs` | 完整类型文法递归下降消费 |
| 值语义降级 | `crates/wake_ecma_parser/src/ts_value.rs` | `enum`/`namespace` → IIFE；`export =` → `module.exports =` |
| import-equals / 引入属性 / `using` | `crates/wake_ecma_parser/src/stmt.rs` | `parse_import_equals()` / `parse_import_attributes()` / `using_decl_here()` / `for_head_var_kind()` |
| 语句级擦除派发 | `crates/wake_ecma_parser/src/stmt.rs` | `interface`/`type`/`declare`/`import type`/`export type`/类成员 |
| 表达式级擦除 | `crates/wake_ecma_parser/src/expr.rs` | `as`/`satisfies`/非空断言/调用类型实参 |
| JSX 解析 | `crates/wake_ecma_parser/src/jsx.rs` | `.tsx`/`.jsx` 文件 |
| 词法器 | `crates/wake_ecma_lexer/src/token.rs` | `Keyword`/`TokenKind` 定义 |
| 类型标志 | `crates/wake_ecma_ast/src/lib.rs` | `SourceType::TypeScript` `SourceType::Tsx` |
| 擦除验证测试 | `crates/wake_ecma_codegen/src/tests.rs` | 11 个 `strip_ts` 测试函数 |
| Bundler 集成测试 | `crates/wake_bundler/src/tests.rs` | TS 项目擦除 + Node 执行验证 |
