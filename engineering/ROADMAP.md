# Wake 工程路线图

路线图从 [AUDIT.md](AUDIT.md) 的开放风险和后续实施中确认的缺口出发，不承诺日期。
优先级表示先后关系；每项只有满足验收条件才算完成。下表用于定位，状态与验收细节见对应条目。

| 事项 | 状态 | 入口 |
| --- | --- | --- |
| R2 性能历史基线 | 部分完成，P1 | [目标与验收](#r2-建立性能历史基线-p1) |
| R4 浏览器端到端矩阵 | 实施中，P1 | [目标与验收](#r4-浏览器运行时端到端矩阵-p1) |
| R5 工程文档一致性 | 持续建设，P2 | [目标与验收](#r5-工程文档持续一致性-p2) |
| R6 JavaScript 字符串码元 | 实施中，P1 | [进展与剩余边界](#r6-javascript-字符串码元兼容-p1) |
| R7 声明与 TS7/PnP | 部分完成，P1 | [兼容验证缺口](#r7-声明与-ts7-pnp-兼容-p1) |
| R8 原生 Lint | 实施中 | [剩余验收](#r8-wake-原生-lint-实施中) |
| R1 / R1.1 构建所有权与候选一致性 | 已完成，持续门禁 | [完成结果](#已完成与持续门禁) |
| R3 Node 支持下界 | 已完成，持续门禁 | [持续验收](#r3-覆盖声明的-node-支持下界-已完成-持续门禁) |

## 当前工作

### R2 — 建立性能历史基线（P1）

目标：从“benchmark 可编译”发展为低噪声、可复跑的回归检测。

验收条件：

- 固定 runner、工具链和电源/负载策略；
- 保存 interner、lexer、parser、resolver、turbo、bundle 的原始历史样本；
- 按 benchmark 噪声设置阈值和人工复跑流程；
- 性能报告同时验证输出与诊断等价；
- CI 文档明确区分 compile smoke 和 regression gate。

独立 Performance 工作流已在 PR 内对关键 Criterion benchmark 执行 base/head 同机对比：
5% 提醒、15% 且复测确认后阻断，关键测量缺失或无效也阻断；原始数据保存 90 天。
canary 完整运行现按 benchmark 定义、锁文件、工具链与机器身份汇总近 90 天成功 artifact，
输出中位数/MAD 的报告型历史快照；远端 artifact 读取和跨运行趋势尚待目标 runner 验证。
该历史报告不阻断构建。性能比较另以同一探针源码编译 base/head Bundler，比较 2k 模块图的产物与
缺失依赖的诊断；不等价或探针不可用时关键 PR 比较失败关闭。本机当前工作树与 `HEAD^` 的两场景
输出和诊断已实测相等，远端 Performance 工作流仍待验证。固定或校准的机器、足够长期样本、
按噪声分级阈值及更广语料的等价证据仍待完成，见 [PERFORMANCE](PERFORMANCE.md) 第 7 节。

Docs 大依赖的局部 `require` 候选过滤已完成，避免把内置加载器调用当作外部 CommonJS 依赖提前
解析/编译；局部绑定、真实未绑定调用、缓存摘要、增量编辑和缺失依赖诊断进入回归测试。
2026-09-09 已为独立 Docs Site 接入按访问编译依赖，并处理 PnP 下的 React 共享；范围与测量见
[PERFORMANCE](PERFORMANCE.md) 第 13 节。后续重点是图标 barrel 裁剪、跨页非共享依赖复用、元数据
重复扫描与最终产物组装；聚合站、Components、自定义 Preview/JSX runtime 尚未接入按需路径。

### R4 — 浏览器运行时端到端矩阵（P1）

目标：覆盖 Node 执行无法发现的 Live Reload、iframe、hash、CSS 和静态路由问题。

验收条件：

- 在至少一个主流浏览器启动 React fixture 和 docs fixture；
- 验证应用源码变化只触发一次整页刷新且页面状态重置、文档 Demo resize/theme、Components
  Controls/URL round-trip；
- 验证 `/` 与非根 `base_path` 的直达、刷新、404 外壳和资源加载；
- 失败保留截图、控制台和网络诊断。

2026-10-22 已在本机 Windows Chrome 验证 React 应用源码变化后的单次整页刷新和状态重置，
以及根路径与 `/portal/` 的直达、刷新、404 和资源；Docs Demo 的主题/视口截图矩阵也已通过。
Components Controls/URL 编辑、刷新、深链、unset/reset、移动视口往返，以及 Demo 抛错后通过 Controls
编辑恢复，已在本机 Chrome 通过。
基于 `fixtures/react-docs` 的生产构建还验证 `/handbook/` 首页、深层页面直达/刷新、
Demo iframe、public 资源和页面/资源 404。同一生产站点的交互 Demo 已在本机 Chrome 验证点击后随内容增高、
反复交互不持续增高、移动视口内容可见、源码展开保持 Demo 状态，以及明暗主题同步到 iframe 和预览根。
这些用例已配置在 CI 的 Linux x64 受审浏览器 leg，目标 runner 成功证据仍待取得。
其他受审浏览器平台的产品矩阵仍待验收。

### R5 — 工程文档持续一致性（P2）

目标：避免再次出现迁移目录缺失或实现状态与 rustdoc 相反。

验收条件：

- `corepack yarn docs:check` 和生产文档构建为必需 CI；
- 改 CLI、配置、Node 类型或组件状态协议的 PR 同步更新对应参考页；
- DESIGN/PLAN/COMPATIBILITY 锚点检查通过；
- 每个发布系列至少复核一次 AUDIT，旧性能数字没有环境时不得升级为 SLA。

### R6 — JavaScript 字符串码元兼容（P1）

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
运行时静态 import/export AST 与依赖记录现已分别使用 `JsAtom` 与 owned `JsString`；
四类静态来源的普通/压缩 ESM 输出保留孤立代理项。相对模块请求进入 resolver 前执行显式 UTF-8
检查，无法表示时带源码位置诊断，不进入路径解析与缓存；CommonJS 降级同样明确拒绝。
bundler 冷热缓存和普通 Unicode 请求有回归，PnP 的非 ASCII 文件名后缀检查也已修复字节切片 panic。
原生类型服务已复现 JSON 通道对
孤立代理项的替换，普通字符串字面量现在通过同一类型的无截断表示及 Wake parser 恢复
为 JsString；字面量联合、类型别名、模板实例与跨文件长字符串有回归。具备验证过的源码声明地址时，
结构属性名称现已恢复为精确 UTF-16 码元，并通过原生高/低代理项及真实替换字符对照。
原生 enum 的直接字符串 initializer 现通过同一源码快照、成员 symbol 与声明地址恢复原始码元；
高/低孤立代理项、真实替换字符、带注释及带引号成员名和错挂 wire 负例已覆盖。无法源绑定的
结构属性名称及不满足上述 enum 证明的值仍报告分析失败，不用失真值证明类型等价。
其余类型服务边界继续实施。tagged 模板中非法转义对应的 cooked undefined 已补齐：每段 raw 保留，非法
转义仅使所在段 cooked 缺失；无标签模板与 TS 模板字面量类型继续诊断，嵌套模板和 TS
推测解析有回归覆盖，普通/压缩及启用模板降级的产物通过 Node 原始源码对照。R6/P1 保持实施中。

验收条件：先明确字符串值跨 lexer、AST、优化器和 emitter 的无损表示契约，再覆盖孤立高/低
代理项、代理对、字符串键、模板 cooked/raw、常量折叠，以及普通/压缩/缓存产物的运行时一致性。
在对应契约和测试落地前，不宣称完整 UTF-16 字符串兼容。

### R7 — 声明与 TS7/PnP 兼容（P1）

2026-09-12 已复现，后续验收：

- 已修复（2026-09-15）：`parse_declaration_facts` 在 ADR 0040 的 parser-owned 模型中处理普通函数、
  arrow、方法、构造器及嵌套解构参数的默认值，保留类型注解并从实现声明模板删除可执行初始化器；
  重载签名同样去除默认值，避免生成触发 TS2371 的 `.d.ts`。对应 parser 回归已覆盖。
- TS7 / PnP：已修复并验证 TS7.0.2 compatibility fixture（`scripts/check-typescript-7.mjs`）通过；
  Yarn lock/provenance 检查也通过。2026-10-22 在本机用缓存的 Yarn 4.16.0 运行
  `check-pnp-conformance.mjs`，原有任意 zip 包、压缩级别、peer/virtual、fallback 等差分矩阵通过。
  新增独立类型 fixture 的 `--local-entries-only` 离线模式通过：共享 tsconfig、Wake 声明投影包及
  真实 CSS tarball 的 ZIP 根和类型入口可解析。Wake 投影保留当前发布清单的 `exports` 和声明字节，
  省略运行时依赖与平台包；它不等于真实 Wake 发布 tarball 消费，后者由 Components PnP 与 npm
  consumer 门禁覆盖。以仓库 Yarn lock 固定工具链依赖图后，独立项目的 TS6+pnpify 类型消费
  在本机系统 TEMP 下通过，覆盖共享 ZIP tsconfig 的严格选项继承和 Wake/CSS ZIP 声明编译；
  完整 `corepack yarn pnp:conformance:check` 的 CI 目标 runner 结果仍待取得。TS7 fixture 的
  显式 `paths` 也不证明任意 PnP ZIP 的通用类型解析。

### R8 — Wake 原生 lint（实施中）

目标是 Wake 原生完整 JS/TS/JSX/TSX/React lint 能力，允许 ESLint 规则与配置迁移，不要求运行原有
JavaScript 插件。P0–P7 阶段状态、规则基线和详细验收统一维护在 [LINT.md](LINT.md#范围与完成定义)；
候选架构见 [ADR 0049](decisions/0049-native-lint-product.md)，尚未成为机器边界。

剩余验收重点：

- P1/P4：完成静态模块字符串与类型服务字符串边界，补齐完整规则语义依赖的源码事实。
- P5：继续扩展完整类型语义，保持未保存源码、跨文件、项目/PnP 失效和失败关闭的验证。
- P6：一次性 CLI/Node 宿主已在本机验证显式授权后的第三方只读诊断、静态 manifest、
  直接依赖校验与普通包消费；未授权不导入扩展代码。Yarn PnP portal 在最终宿主上已验证，
  ZIP 消费测试在输出 IPC 修改前通过；最终源码的 ZIP 复跑被 Corepack 离线下载阻断，仍待补证。
  首期拒绝 fix、baseline、
  LintContext/watch，活跃扩展规则不支持 Markdown、LSP、非空规则参数或注释抑制；绕过持久化
  诊断缓存，尚未冻结传递依赖图，获授权代码自建后代不保证回收。自动启用的完整边界与编辑器
  可信工作区验收继续按 [ADR 0072](decisions/0072-lint-third-party-rule-host.md)（仍为 proposed）推进，目标平台 CI
  结果待取得；独立 SDK 与本机首期验证均不代表完整宿主能力完成。
- P3/P7：完成目标平台运行、真实项目与发布消费验收。发布前的 5 平台 × 3 Node 版本
  npm consumer 门禁现检查安装后的 Unix test-host 可执行位，并以 tarball 消费执行 lint CLI 与
  CommonJS/ESM Node API smoke；这些门禁的目标 runner 成功结果仍待取得，已有 Linux x64/ARM64
  交叉构建及 manifest 证据不能替代 macOS/ARM 目标执行。

实施过程与当时的测试证据见 [Lint 实施记录](history/LINT_IMPLEMENTATION.md)。只有全部阶段满足验收，
才能把完整 Lint 标记为完成；本页不重复维护规则数量和逐次测试结果。

## 已完成与持续门禁

### R1 — BuildSession 构建所有权收敛（已完成，持续门禁）

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

### R1.1 — BuildGeneration 完整候选一致性（已完成，持续门禁）

结果：一个 production candidate 由一个 `BuildGeneration` 拥有；application retained/one-shot view 与
Federation container/shared one-shot views 共享 generation-scoped observation cache。类型 identity 每代
只冻结一次，application/Federation/types/manifest/bootstrap/hidden maps 作为完整候选一次发布；dev
Federation 则由单一 retained session 编译 combined graph。长期决策见
[ADR 0028](decisions/0028-build-generation-ownership-and-observation-cache.md)。

#### Node/npm 公共边界：已完成

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

### R3 — 覆盖声明的 Node 支持下界（已完成，持续门禁）

结果：常规 CI 以仓库外、非 PnP 的干净 npm consumer 在 Windows/Linux 覆盖精确下界
Node 22.14.0，并与 Node 24/26 的完整 Node job 共同验证主 API、类型、CLI 与 `build()`；发布前
local tarball 和发布后 registry smoke 都覆盖 22.14.0/24/26。发布门禁会拒绝丢失精确下界的矩阵。

持续验收条件：

- CI 至少在 Node 22.14、24、26 运行主 API 与类型测试；
- 发布后干净安装在最低版本验证 CLI 和 `build()`；
- 如果上游 Action/原生工具不再支持 22，则先调整并发布 engines/迁移说明，不能静默失配。

## 非路线图事项

以下内容没有当前承诺：稳定 Rust 插件 ABI、任意 JS 配置执行、完整 Sass/Less 内建链、冻结 experimental AST schema。提出这些能力前需单独设计、兼容性和安全评审。
