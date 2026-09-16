# Wake 工程路线图

路线图从 [AUDIT.md](AUDIT.md) 的开放风险出发，不承诺日期。优先级表示先后关系；每项只有满足验收条件才算完成。

# R1 — BuildSession 构建所有权收敛（已完成，持续门禁）

结果：CLI、Node、dev、Docs 与 Federation 产品构建统一由 typed `BuildSession` 拥有；retained 与
one-shot 只区分显式生命周期，共享 Scan/Link/Optimize/Emit 与 `BuildOutput` 契约。底层 engine setter
降为 crate 内实现边界，长期决策见
[ADR 0027](decisions/0027-build-session-ownership-and-lifetime.md)。

持续验收条件：

- one-shot/retained 全字段产物、诊断和错误等价测试保持通过；
- 产品源码、外部 integration tests 与 benchmark 不得引用底层 engine 或迁移构造器；
- persistent cache、Tree Shaking、分包、CSS/资源、Federation 和顶层 await 不因生命周期不同而变化；
- 新增 bundler 语义必须先进入 immutable `BuildOptions` 及相应缓存身份，不得恢复 product setter；
- 兼容 façade 只能委托 `BuildSession`，删除底层 public re-export 时不得改变 npm API。

# R1.1 — BuildGeneration 完整候选一致性（已完成，持续门禁）

结果：一个 production candidate 由一个 `BuildGeneration` 拥有；application retained/one-shot view 与
Federation container/shared one-shot views 共享 generation-scoped observation cache。类型 identity 每代
只冻结一次，application/Federation/types/manifest/bootstrap/hidden maps 作为完整候选一次发布；dev
Federation 则由单一 retained session 编译 combined graph。长期决策见
[ADR 0028](decisions/0028-build-generation-ownership-and-observation-cache.md)。

## Node/npm 公共边界：已完成

Federation init/lock 已下沉为共享应用服务并同时接通 Rust CLI、Node API 与 npm CLI；输出 kind、
Federation 错误码和 dev 事件都有闭合契约。发布前会验证 Federation runtime、Wake 类型、完整
tarball 目标及 PnP 树外 NodeNext 消费。决策见
[ADR 0029](decisions/0029-node-contract-and-federation-control-ownership.md)。

持续验收条件：

- `wake_app` production federation 子构建不得硬编码 `OsFileSystem` 或直接构造 `BuildSession`；
- build context 必须把 retained application 与 generation owner 共同持有，并在 watcher batch 观察前 advance；
- generation cache 的六个 query family、failure replay、single-flight 和 advance 有行为测试；
- 文档和测试始终声明 lazy、query-scoped snapshot 的边界，不宣称未观察路径或跨方法事务一致性；
- 类型声明只读取一次，再重绑定到最终 `buildId`；失败候选保留 last-good output/runtime snapshot；
- dev application、synthetic container、exposes 与 shared fallback 继续由一个 retained combined session 拥有。

# R2 — 建立性能历史基线（P1）

目标：从“benchmark 可编译”发展为低噪声、可复跑的回归检测。

验收条件：

- 固定 runner、工具链和电源/负载策略；
- 保存 interner、lexer、parser、resolver、turbo、bundle 的原始历史样本；
- 按 benchmark 噪声设置阈值和人工复跑流程；
- 性能报告同时验证输出与诊断等价；
- CI 文档明确区分 compile smoke 和 regression gate。

Docs 大依赖的局部 `require` 候选过滤已完成，避免把内置加载器调用当作外部 CommonJS 依赖提前
解析/编译；局部绑定、真实未绑定调用、缓存摘要、增量编辑和缺失依赖诊断进入回归测试。
2026-09-09 已为独立 Docs Site 接入按访问编译依赖，并处理 PnP 下的 React 共享；范围与测量见
[PERFORMANCE](PERFORMANCE.md) 第 13 节。后续重点是图标 barrel 裁剪、跨页非共享依赖复用、元数据
重复扫描与最终产物组装；聚合站、Components、自定义 Preview/JSX runtime 尚未接入按需路径。

# R3 — 覆盖声明的 Node 支持下界（已完成，持续门禁）

结果：常规 CI 以仓库外、非 PnP 的干净 npm consumer 在 Windows/Linux 覆盖精确下界
Node 22.14.0，并与 Node 24/26 的完整 Node job 共同验证主 API、类型、CLI 与 `build()`；发布前
local tarball 和发布后 registry smoke 都覆盖 22.14.0/24/26。发布门禁会拒绝丢失精确下界的矩阵。

持续验收条件：

- CI 至少在 Node 22.14、24、26 运行主 API 与类型测试；
- 发布后干净安装在最低版本验证 CLI 和 `build()`；
- 如果上游 Action/原生工具不再支持 22，则先调整并发布 engines/迁移说明，不能静默失配。

# R4 — 浏览器运行时端到端矩阵（P1）

目标：覆盖 Node 执行无法发现的 Live Reload、iframe、hash、CSS 和静态路由问题。

验收条件：

- 在至少一个主流浏览器启动 React fixture 和 docs fixture；
- 验证应用源码变化只触发一次整页刷新且页面状态重置、文档 Demo resize/theme、Components
  Controls/URL round-trip；
- 验证 `/` 与非根 `base_path` 的直达、刷新、404 外壳和资源加载；
- 失败保留截图、控制台和网络诊断。

# R5 — 工程文档持续一致性（P2）

目标：避免再次出现迁移目录缺失或实现状态与 rustdoc 相反。

验收条件：

- `npm run docs:check` 和生产文档构建为必需 CI；
- 改 CLI、配置、Node 类型或组件状态协议的 PR 同步更新对应参考页；
- DESIGN/PLAN/COMPATIBILITY 锚点检查通过；
- 每个发布系列至少复核一次 AUDIT，旧性能数字没有环境时不得升级为 SLA。

# R6 — JavaScript 字符串码元兼容（P1）

2026-09-06 的 Lexical 补丁排查已修复相邻代理对转义及 async 表达式后缀。
当时仍未解决孤立 UTF-16 代理项（例如 `"\ud800"`）：lexer 拒绝它，而 AST 字符串
使用 UTF-8 Atom，仅移除诊断会造成解码丢失码元。

2026-09-15 已先锁定模板字符串的 raw/cooked 双值契约：模板 raw 保留源码转义，cooked 按
ECMAScript 转义解码；已补齐 CR/CRLF 的 cooked 归一化及 LF/CR/CRLF/LS/PS 行接续，普通、
压缩与模板降级输出通过 Node 原始源码对照。孤立代理项仍需在同一无损表示设计中完成，
不能用替换字符代替。

2026-09-16 已完成无损值基础设施：`JsString` 保留任意 UTF-16 码元，`JsAtom` 使用独立的
进程内驻留身份，lexer 新增无损解码入口。全部单码元、代理对、跨拼接边界、内容身份和
并发驻留回归通过。候选跨层契约见 [ADR 0068](decisions/0068-lossless-ecmascript-string-values.md)。
同日已继续迁移运行时字符串、属性键、enum 字符串成员、模板 cooked、原始 JSX/switch 值事实、
优化 IR、常量折叠、两类 emitter 和定义值缓存身份。孤立代理项现在保留并以 Unicode 转义
输出；普通/压缩/模板降级、装饰器及全新会话持久缓存命中和内容失效均有 Node 运行时对照。
lexer 同时修复 `\u{z}` 的漏报，标识符仍拒绝代理项。

import attributes 的字符串键和值已接入 JsAtom/JsString，包含静态导入/重导出、TS import-type、
动态 import options 与重复导入判断。普通和优化 ESM 产物通过 Node 模块链接回调验证实际码元，
静态重复键按解码值拒绝，动态对象保留后写覆盖语义。
ambient 模块名称、擦除的 TS import/export-type、原始模块请求及冻结声明请求已接入无损值；
同值转义共享 ambient 类型/值作用域，不同代理项保持独立。声明模板保留外部请求原文，
相对请求在 UTF-8 文件系统边界明确失败；动态字面量模块请求不再因孤立代理项降为未知。
剩余边界：运行时静态 import/export AST 与依赖记录仍使用 UTF-8 契约，含孤立代理项时明确
诊断，尚未完成整个模块输出链路迁移。原生类型服务已复现 JSON 通道对
孤立代理项的替换，普通字符串字面量现在通过同一类型的无截断表示及 Wake parser 恢复
为 JsString；字面量联合、类型别名、模板实例与跨文件长字符串有回归。含替换字符歧义的
枚举值和结构属性名称仍无法通用恢复，必须报告分析失败，不再用失真值证明类型等价；
这些类型服务边界继续实施。tagged 模板中非法转义对应的 cooked undefined 已补齐：每段 raw 保留，非法
转义仅使所在段 cooked 缺失；无标签模板与 TS 模板字面量类型继续诊断，嵌套模板和 TS
推测解析有回归覆盖，普通/压缩及启用模板降级的产物通过 Node 原始源码对照。R6/P1 保持实施中。

验收条件：先明确字符串值跨 lexer、AST、优化器和 emitter 的无损表示契约，再覆盖孤立高/低
代理项、代理对、字符串键、模板 cooked/raw、常量折叠，以及普通/压缩/缓存产物的运行时一致性。
在对应契约和测试落地前，不宣称完整 UTF-16 字符串兼容。

# R7 — 声明与 TS7/PnP 兼容（P1）

2026-09-12 已复现，后续验收：

- 已修复（2026-09-15）：`parse_declaration_facts` 在 ADR 0040 的 parser-owned 模型中处理普通函数、
  arrow、方法、构造器及嵌套解构参数的默认值，保留类型注解并从实现声明模板删除可执行初始化器；
  重载签名同样去除默认值，避免生成触发 TS2371 的 `.d.ts`。对应 parser 回归已覆盖。
- TS7 / PnP：已修复并验证 TS7.0.2 compatibility fixture（`scripts/check-typescript-7.mjs`）通过；
  Yarn lock/provenance 检查也通过。任意 PnP zip 包、共享 tsconfig、Wake/CSS 包类型入口的通用
  解析仍需独立 fixture 矩阵；本机 `check-pnp-conformance.mjs` 在临时项目执行 Yarn install 时因
  Corepack 需要联网下载 Yarn 4.16.0 且网络策略拒绝连接，暂不能登记为通过。

# R8 — Wake 原生 lint（实施中）

目标、有限规则基线、P0–P7 顺序和验收见 [LINT.md](LINT.md)。用户已选择原生完整能力并允许
ESLint 规则/配置迁移；不要求运行原有 JS 插件。候选架构见
[ADR 0049](decisions/0049-native-lint-product.md)，尚未成为机器边界。

已建立真实注释、原始 token、静态 import/export、JSX 值、数组与 TS 原始结构、77 条规则和共享 Rust/Node 项目入口。
已接入规则参数、版本化预设、配置来源解释、安全修复迭代、预览与按文件原子写回；原生 TypeScript 类型服务、LSP/VS Code
quick-fix、ESLint 迁移报告、lint Criterion 性能基线、独立扩展 SDK 和内建 Markdown 处理器已接入；类型联合字面量、枚举成员
switch 覆盖证明和版本化 browser/node environments 也已接入。真实 CLI smoke 已覆盖 JS/MJS/CJS/JSX/TS/MTS/CTS/TSX/Markdown 代表矩阵；第三方规则自动启用和目标平台
发布验收仍需逐项完成。
不能将源码采集或少量规则标记为完整 lint。规则目录及单文件诊断缓存已接入，覆盖内容/配置失效、
跨进程写入、损坏恢复与修复绕过。显式批量抑制基线已支持生成/检查/清理、上下文唯一匹配、
修复过滤、缓存失效和冲突保护发布。手动 LintContext 已提供多文档版本、代次取消和安全 Node
生命周期；CLI/Node 已共用自动监听、去抖、配置恢复和有界结果队列。独立文件共用进程线程池和
两个批次的准入上限，串行/并行结果等价验证通过；真实 CLI smoke 与扩展 SDK 已闭环，继续推进
完整生态项目矩阵与目标平台实际构建验证。当前真实 CLI smoke 已覆盖 JS/MJS/CJS/JSX/TS/MTS/CTS/TSX/Markdown，且增加 npm package、workspace
monorepo、PnP 风格 manifest、React/Docs/PnP 仓库以及两个独立 Docs workspace 根 fixture；Preact `8101ff8` 和 p-map `bc8380d` 的外部
快照也已完成 CLI smoke。Linux x64/ARM64 平台包已在本机交叉构建并完成 manifest 验证，macOS 与 Unix 可执行位仍需目标 runner 的发布门禁。

# 非路线图事项

以下内容没有当前承诺：稳定 Rust 插件 ABI、任意 JS 配置执行、完整 Sass/Less 内建链、冻结 experimental AST schema。提出这些能力前需单独设计、兼容性和安全评审。
