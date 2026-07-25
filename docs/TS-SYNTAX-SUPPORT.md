# TypeScript 语法支持清单

> 最后更新：2025-07-24 · 基于 TypeScript 7（`erasableSyntaxOnly` 模式可用）
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
| `enum E { ... }`（值语义 → IIFE） | ✅ | `ts_value.rs:parse_enum()` |
| `const enum E { ... }`（值语义 → IIFE） | ✅ | `stmt.rs` → `parse_enum()` |
| `namespace N { ... }`（值语义 → IIFE） | ✅ | `ts_value.rs:parse_namespace()` |
| `module N { ... }`（值语义 → IIFE） | ✅ | 同 `namespace` |
| Ambient `module "..." { }` | ✅ | `ts_skip_ambient()` 通用消费 |
| 点分 namespace `A.B.C` | ⚠️ | 取首段，不支持跨声明合并 |

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
| `accessor` 字段 (auto-accessor) | ✅ | 同上 |
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
| 装饰器 `@dec` | ⚠️ | 语法消费擦除，**不构建 AST 节点**，无下游 emit |

---

## 4. 表达式

| 特性 | 支持 | 位置 |
|------|------|------|
| `as Type` 断言 | ✅ | `expr.rs:154` `parse_binary_expression` |
| `as const` 断言 | ✅ | 同上 → `ts_type()` → `ts_primary` 消费 `const` |
| `satisfies Type` | ✅ | `expr.rs:154` 同 `as` 处理 |
| `<Type>expr` 尖括号断言 | ✅ | `expr.rs:238`（仅 `.ts`，`.tsx` 中 `<` 是 JSX） |
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
| **`import x = require('...')`** | ❌ 🟡 | 无任何处理分支 |
| **`import x = await import('...')`** | ❌ 🟡 | 同上 |
| **`export = expr`** | ❌ 🟡 | `parse_export` 无 `=` 分支 |
| **`export as namespace`** | ❌ 🟢 | 无处理 |
| **Import attributes `with { type: "json" }`** | ❌ 🔴 | 消费完 `from "src"` 后直接 `semicolon()` |

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

| 特性 | 支持 | 根因 |
|------|------|------|
| **`using x = expr`** | ❌ 🔴 | 词法器 `Keyword` 无 `Using` 变体；`VarKind` 无 `Using`；`var_kind_here()` 只认 `var/let/const` |
| **`await using x = expr`** | ❌ 🔴 | 同上 |
| **`for (using x of xs)`** | ❌ 🔴 | `parse_for()` 用 `var_kind_here()` 判定循环变量种类 |
| **`for (await using x of xs)`** | ❌ 🔴 | 同上 |
| **`yield using x = expr`** | ❌ 🔴 | `parse_yield()` 无 `using` 分支 |

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
| `enum` → IIFE | ✅ | ✅ | ✅ |
| `namespace` → IIFE | ✅ | ❌¹ | ✅ |
| 参数属性 `this.x = x` | ✅ | ✅ | ✅ |
| `import type` / `export type` | ✅ | ✅ | ✅ |
| `using` / `await using` | ❌ | ✅ | ✅ |
| Import attributes `with` | ❌ | ✅ | ✅ |
| 装饰器下游 emit | ❌² | ✅ | ✅ |
| `import = require` | ❌ | ❌³ | ❌³ |
| `export =` | ❌ | ❌³ | ❌³ |

> ¹ esbuild 不支持 `namespace` 降级。[^1]  
> ² wake 消费装饰器语法但不构建 AST，无法做 `__decorate` 或 stage 3 运行时 emit。  
> ³ esbuild/SWC 推荐使用 ESM 等价写法。

[^1]: https://esbuild.github.io/content-types/#typescript-namespaces

---

## 10. 代码位置速查

| 组件 | 文件 | 职责 |
|------|------|------|
| 类型消费器（擦除引擎） | `crates/wake_ecma_parser/src/ts.rs` | 完整类型文法递归下降消费 |
| 值语义降级 | `crates/wake_ecma_parser/src/ts_value.rs` | `enum`/`namespace` → IIFE |
| 语句级擦除派发 | `crates/wake_ecma_parser/src/stmt.rs` | `interface`/`type`/`declare`/`import type`/`export type`/类成员 |
| 表达式级擦除 | `crates/wake_ecma_parser/src/expr.rs` | `as`/`satisfies`/非空断言/调用类型实参 |
| JSX 解析 | `crates/wake_ecma_parser/src/jsx.rs` | `.tsx`/`.jsx` 文件 |
| 词法器 | `crates/wake_ecma_lexer/src/token.rs` | `Keyword`/`TokenKind` 定义 |
| 类型标志 | `crates/wake_ecma_ast/src/lib.rs` | `SourceType::TypeScript` `SourceType::Tsx` |
| 擦除验证测试 | `crates/wake_ecma_codegen/src/tests.rs` | 11 个 `strip_ts` 测试函数 |
| Bundler 集成测试 | `crates/wake_bundler/src/tests.rs` | TS 项目擦除 + Node 执行验证 |
