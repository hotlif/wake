# ADR 0067: 顶层 ambient 值绑定投影

- Status: accepted
- Date: 2026-09-14

## Context

TypeScript 的顶层 `declare`、`declare global` 与 ambient namespace 声明在编译 AST 中被擦除，
但它们仍然创建源码值环境。把这些名字全部标成不可用会让 `no-undef`、`typeof` 查询和跨文件
规则错误地忽略真实的顶层绑定；把块内声明、字符串 ambient module 或 namespace 成员直接提升
到模块又会跨越原始作用域并捕获错误的同名外层值。

## Decision

`wake_ecma_semantic::analyze_source` 在源码事实明确表示绑定直接位于顶层 `declare` 或
`declare global` 时，为其创建 semantic-owned 的源码值 SymbolId，并把原始运行时引用重新
解析到该符号。对非字符串的 ambient namespace，semantic 另外创建与原始 namespace 层级对应
的源码 scope；namespace 根绑定进入其父 scope，直接值成员进入 namespace scope。声明签名的
第一个值绑定是函数/类样声明本身；同一无函数体签名范围内的后续值绑定是参数，只进入该签名的
独立源码值 scope，不能被投影到模块作用域或其他签名。投影符号只用于源码语义，绝不写入编译 AST 或普通
`analyze` 结果。

绑定位于无值参数绑定的纯类型签名或字符串 ambient module 内时，继续保留 `Unavailable`/不完整状态；已有语义
scope 的普通块、函数体和 switch 环境可以承载其直接 `declare` 值成员；namespace
成员只能在其原始 namespace scope 解析，不能回退到同名外层绑定。投影出的 ambient symbol 带有独立标记：未使用、use-before-define
和 React Hooks 不把没有可检查函数体的声明当作普通源码实现；`require` 等动态 loader 仍将
ambient 声明视为无法证明的来源并发布不完整状态。字符串 ambient module 的不完整名称不会
污染另一个 scope 中已经解析到的本地值；块或未表示 namespace 的名称仍按文件级保守保护，
避免在源码环境尚未完整时把外层绑定当成证明。

## Invariants

- parser 仍拥有原始声明和容器范围，semantic 是唯一的 SymbolId 投影所有者。
- 只有直接顶层 `declare`/`declare global` 值绑定、非字符串 ambient namespace 的根/直接成员、字符串
  ambient module body 内的直接值成员、函数体或 switch scope 内的直接 `declare` 值绑定，以及无函数体
  声明签名/值参数绑定的独立 scope 内的参数可被恢复；这些参数和块边界不可穿透到外部；普通块、函数体
  与 switch 投影复用已有 resolver scope，不创建模块级绑定。
- 每个 namespace 声明的直接成员进入独立源码 scope；重复声明按父 scope 与名称合并，不能把成员
  提升到模块或跨 namespace 捕获同名值。
- 同一源文件中相同字符串模块 specifier 的 ambient body 共享值与类型成员 scope；跨文件 global
ambient 声明合并由项目模块图负责。项目图只导出声明文件中的 `declare global`、其 ambient
namespace 根与 script 顶层 `declare` 值名称，模块本地值优先，不把 namespace/module 成员提升到全局。
- 值参数绑定的纯 `TsSignature` 进入独立签名值作用域，并继承最近已表示的 ambient namespace/module
  或函数父 scope；无值参数绑定的纯类型签名或未表示
  类型环境仍保持 `Unavailable`；函数体直接 `declare` 值成员复用已有 function-body scope；字符串
  ambient module 的不完整名称只在没有独立本地解析时保持 `Unavailable`；switch 直接声明复用 resolver
  已有 switch scope，不提升到模块。
  namespace 的不完整名称继续按文件级保守保护，同名的外层值不能被误当作证明。
- 规则消费端也必须按源码符号身份应用这条边界：字符串 ambient module 的同名擦除声明不能
  抑制另一作用域中已表示本地绑定的 `no-unused-vars` 等声明级诊断，也不能抑制已解析类型
  符号的 `consistent-type-exports` 诊断；`declare global` 的投影也不能把同名本地 SymbolId
  标成 ambient。不完整名称集合只能保护没有可表示符号身份的擦除声明。
- ambient symbol 的源码范围和名称必须来自真实 identifier occurrence，不能由名称猜测。
- 普通编译语义、缓存之外的 AST 身份和生成代码不因投影改变。
- 任何需要函数体、模块 loader 身份或完整作用域的规则必须识别 ambient 标记并 fail-closed。

## Evidence

- `wake_ecma_semantic/tests/source_type_queries.rs`：顶层 ambient 查询与运行时引用解析、
  `declare global` 的函数/类/枚举声明、声明重载与纯函数/方法类型签名参数在独立 scope 解析且不泄漏到模块作用域、普通块、函数体和 switch
  `declare` 值成员复用已有语义 scope，以及 namespace 根/直接成员的独立 scope；字符串 ambient module
  body 内本地值在独立 scope 解析，未知外部成员和嵌套擦除
  环境仍保持不可用保护，同名本地值不被外部 ambient 名称遮蔽。
- `wake_ecma_semantic/tests/source_types.rs`：重复字符串 ambient module 的类型成员合并与声明 identity。
- `wake_lint_core/tests/undefined.rs`：顶层 ambient 值不产生 `no-undef`。
- `wake_lint_core/tests/unused.rs`、`hooks.rs`、`use_before_define.rs`：不误报未使用、Hook
  调用或声明顺序，并保留嵌套 ambient 的不可用诊断。
- `wake_lint_core/tests/module_requests.rs`：ambient `require` 不产生未经证明的模块请求，
  同时保留不完整状态。
- `wake_ecma_semantic/tests/source_type_queries.rs` 与 `wake_lint_core/tests/use_before_define.rs`：
  `declare global` 与同名本地绑定共存时保持本地 SymbolId，声明顺序规则继续检查本地引用。
- `wake_lint_core/tests/undefined.rs`：项目图中的声明文件 global ambient 值可被另一文件解析，
  同名模块本地值优先，ambient namespace/module 成员不会被提升。

## Consequences

顶层声明文件、全局声明、无函数体签名、值参数绑定的纯 `TsSignature`、函数体或 switch 直接 `declare` 值成员、非字符串 ambient namespace 和字符串 ambient module 的本地 body 可以
参与源码 lint 的值身份解析，减少保守跳过范围；代价是规则必须区分“可解析的 ambient 值”与
“有可执行实现的源码绑定”。字符串 ambient module 的跨文件成员类型合并仍由原生 TypeScript
项目服务负责；lint semantic 不根据模块名称猜测运行时值身份。

## Validation

先运行 semantic、lint 核心和应用聚焦回归，再运行受影响 Clippy、架构检查和文档门禁。

## Supersedes

None.

## Amends

- [ADR 0053](0053-source-semantic-facts-for-lint.md): 顶层 ambient 值绑定的源码语义投影和消费者边界。

## Removal plan

不移除源码 semantic 投影；若未来完整 TypeScript ambient scope 取代该切片，保留现有 SymbolId
身份并以新 ADR 规定兼容和缓存迁移。
