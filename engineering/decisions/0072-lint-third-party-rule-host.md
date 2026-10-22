# ADR 0072: 显式授权的 lint 第三方规则宿主

- Status: proposed
- Date: 2026-10-22

## Context

`@crab-dev/wake-lint-sdk` 已按 [ADR 0065](0065-lint-extension-sdk.md) 提供独立规则包、冻结输入、
诊断和编辑协议；原先没有将规则加入 `wake lint` 的宿主。此 ADR 的首期实现正在工作树中验证。
[ADR 0052](0052-lint-effective-configuration.md) 要求未知规则即使为 off 也失败；扩展宿主须保留
这项闭集校验。
[ADR 0055](0055-content-addressed-lint-cache.md) 明确要求插件补齐依赖身份后才能缓存。
现有 [ADR 0049](0049-native-lint-product.md) 仍是候选产品边界，不能作为启用第三方代码的授权。

加载 JavaScript 规则包会执行包及依赖的代码。SDK 的 `runRule` 能隔离抛错和非法编辑，但不能终止
无限循环，也不能限制包主动读取文件、访问网络或启动子进程。自动发现已安装包、单凭仓库配置执行包、
或把配置解释转为隐式 JS 执行，均会引入尚未接受的信任和跨层契约。

## Decision

以下为待验证的完整宿主方案。首期一次性 CLI/Node 切片已有实现和聚焦测试，但本 ADR 仍为
`proposed`，不作为已接受的机器策略或稳定能力依据。只有静态包声明、显式执行授权和有效规则设置
三个条件同时满足时，宿主才运行第三方规则；不扫描依赖树，不运行 ESLint 配置或插件 ABI。

1. `wake.config.toml` 仅声明精确包名；规则仍在既有 `[lint.rules]` 和 override 中按命名空间 ID
   设置等级与参数。包须为项目根 `package.json` 的直接依赖。未知包、重复包、未声明的规则及非法参数
   在所有配置层（包括 off 和未匹配 override）失败。规则默认关闭；包安装和配置声明本身不启用规则。
   原生规则 ID、预设和默认集合不能被扩展覆盖。`wake_config` 只解析数据；应用层统一物化有效规则、
   来源和诊断顺序，CLI、Node、LSP 不各建一份注册表。
2. CLI 以显式 `--allow-lint-extensions`、Node 以请求级 `allowExtensions: true` 授权执行；编辑器
   还须有可信工作区与对应项目的显式授权。有效扩展规则缺授权时返回明确错误，不静默跳过，且不导入
   任何扩展 JS。多根工作区按规范项目根独立授权和隔离。授权只属于当前调用或工作区会话，不写进
   仓库配置，也不从 `package.json` 安装状态推导。用户已选择此双重授权；接受 ADR 前仍须完成
   跨层验证。对声明扩展包的未授权 `--print-config` 同样失败关闭，不运行 PnP loader。
3. 每个包通过 `package.json` 中固定的 `wakeLint` 字段指向包内只读 JSON manifest 和 JS 模块。
   JSON manifest 使用独立的 `wake.lint.package.v1` schema，声明包名/版本、SDK 主版本、规则 ID、
   元数据和封闭参数 schema；宿主先解析、校验并用于 `--print-config`，不导入扩展模块。PnP
   解析若必须执行项目 loader，也要求先取得同一授权。模块执行后以
   `wake.lint.extension.v1` 校验其导出的包身份、规则顺序、ID 与元数据同静态 manifest 一致；
   诊断的 message ID 和参数仍按静态 manifest 验证。这样配置解释仍不读取目标源码、
   不执行扩展；包缺 manifest、版本不符、未知字段或运行时声明漂移均失败。manifest 格式和升级
   窗口在接受本 ADR 前须以独立包 fixture 验证，不改变既有 SDK v1 的默认结果。
4. 解析以规范项目根为 issuer，拒绝路径/URL 形式及项目依赖之外的包说明符；Node 解析器在同一
   项目上下文内处理 `node_modules` 和 Yarn PnP，不能退回 Wake 安装目录或全局包。PnP loader、
   `.pnp.cjs`、lockfile 与包归档的执行仅发生在已授权的隔离进程。宿主不联网安装、下载或更新包；
   无可用 Node、解析失败或包身份漂移均返回扩展错误。
5. `wake_app` 拥有项目检查、代次、取消、结果合并和写回。第三方 JS 在项目专用的可回收 Node
   子进程中运行，使用有版本与大小上限的消息帧、每轮时间/内存预算以及冻结的 owned 源码事实；
   不传内部 arena、编译 AST 或类型服务句柄。超时、崩溃、协议错误或规则抛错使本轮检查失败，
   不发布部分扩展诊断或成功快照；取消/关闭须终止并回收子进程，旧代次结果不得进入新结果。
   子进程只提供宿主 worker 级故障与有限资源约束，**不是权限沙箱**：获授权的包仍拥有该用户
   进程可用的文件、环境和网络权限。包代码若自行产生副作用，宿主无法用诊断事务撤销。
6. 完整方案中的扩展诊断与候选编辑经应用层协议、UTF-8 边界、消息 ID、级别、排序和当前源码快照校验后，
   才进入既有抑制、基线与修复选择。扩展不能直接获得宿主写入 API；`--fix` 复用
   [ADR 0051](0051-lint-source-fix-transactions.md) 的核心冲突选择、全项目预计算、逐文件取消栅栏
   与 `replace_lint_source`，映射不可逆的处理器文件只报告诊断。任何扩展失败须发生在首个写回前。
7. 首期对启用扩展规则的文件绕过持久化诊断缓存；不得用现有单文件 key 保存扩展结果。将来启用
   缓存前，key 至少纳入包解析 locator、manifest、锁文件及可执行依赖图的内容身份；无法证明依赖
   完整时继续绕过。watch/LSP 在能监听配置、直接依赖、模块图、lockfile、PnP loader/归档并使旧
   worker 与检查代次失效前，不启用扩展规则的自动复查；无法可靠监听时返回受限能力错误而不沿用
   旧成功结果。此处沿用 [ADR 0057](0057-versioned-lint-project-context.md) 和
   [ADR 0058](0058-lint-watch-lifecycle.md) 的项目代次与结果生命周期。

## Invariants

- 未授权的检查或配置解释不执行项目 loader 与扩展 JS；默认 lint 不执行第三方 JS，已安装包
  不改变原生规则的默认结果。
- CLI、Node、LSP 对同一项目根、源码快照、配置与授权给出一致的有效规则和有序诊断。
- 第三方失败不会产生部分成功结果、旧代次诊断、持久化缓存命中或源码写入。
- 宿主不将 Node 进程隔离称为安全沙箱，也不把包作者的主动副作用纳入 Wake 修复事务保证。
- 首期接入的执行权只来自当前 CLI/Node 调用的显式授权；watch/LSP 不因本 proposed ADR 自动接入。

## Evidence

- `npm/wake-lint-sdk/index.mjs`、`index.d.ts` 与 `test/index.test.mjs`：独立 SDK 的版本、
  冻结输入、UTF-8 编辑和错误协议；首期产品调用路径仍受下述范围约束。
- `crates/wake_config/tests/lint.rs`、`crates/wake_app/src/lint.rs`、
  `crates/wake_app/src/lint/extensions.rs`、`extension_host.cjs` 与
  `crates/wake_app/tests/lint_extensions.rs`：首期静态 manifest、显式授权、隔离 worker 和应用层验证。
- ADR 0051、0052、0055、0057、0058、0065：已接受的修复、有效配置、缓存、项目代次、
  监听及 SDK 约束。首期已有普通包、Yarn PnP portal 与 ZIP 独立消费的本机证据；目标平台 CI、
  完整依赖图身份和 watch/LSP 仍待验证。

## First phase limits

当前一次性 CLI/Node 实现仅接受空参数 schema 与不提供修复的规则。配置列出扩展包时，fix、
baseline、LintContext 与 watch 均拒绝；活跃扩展规则不运行于 Markdown 处理器或 LSP。
扩展诊断尚不进入注释抑制链，指令中的扩展 ID 会作为无效规则报告。首期每轮核对入口模块
路径与内容，但未冻结传递依赖图，包代码主动变化时仍可能影响同轮观察；这限制了完整方案的
“单一执行身份”承诺，不能据此启用持久化缓存或 watch/LSP。宿主子进程也不是权限沙箱。
超时/取消只承诺终止宿主 worker；规则包或项目 loader 自建的后代进程不在回收保证内，继承
句柄时可能延迟临时响应文件释放并继续消耗资源。响应解析至多读取 2 MiB，运行中周期检查
临时文件大小；这不是严格的磁盘写入上限。单帧校验只覆盖读取时已写入的内容，后代仍可在
校验后继续追加。若项目 PnP loader 在读取请求前挂起，输入写入线程也可能延迟回收。
完整进程树与资源隔离仍需后续设计。

## Consequences

显式授权使仓库配置不能单独触发任意第三方代码，但使用扩展需要可用 Node，CLI、Node 与编辑器
都需暴露清晰的授权和错误状态。静态 manifest 给配置解释提供不执行 JS 的规则目录；代价是发布包
必须维护静态/运行时一致性。首期绕过缓存、限制 watch/LSP 可用性，换取可证明的结果身份。
接受前需厘清本决策对 ADR 0052、0055、0058 和 0065 的局部修订关系；当前 `proposed` 状态不
更改这些已接受决策。

## Validation

1. 先以独立安装包 fixture 验证 manifest 与运行时声明一致、协议/SDK 升级拒绝、未知字段和
   off 规则校验；未授权/默认检查不得执行带副作用的探针包。Node 缺失、包不存在、路径穿越、
   依赖漂移及普通 `node_modules`/Yarn PnP ZIP 均有失败或成功用例。
2. CLI 与 Node 对同一单文件和多文件项目验证配置解释、来源、规则选项、Unicode 字节范围、
   排序、抑制与基线一致；抛错、循环超时、崩溃、超大帧、取消和旧代次返回均不得发布部分结果。
3. 修复验证冲突编辑、源码漂移、处理器不可逆映射和扩展失败在首个写回前；缓存验证绕过或完整
   依赖身份。随后才启用 watch/LSP，验证改包、改锁文件、PnP 归档替换、多根授权变化、旧 worker
   退休与未保存文档。
4. 完成目标平台 Node 版本、npm tarball 与 PnP 消费矩阵后，才考虑接受 ADR、复核用户文档及
   产品状态；运行相关 crate/package 测试、架构测试与检查。尚未执行的矩阵不能登记为通过。

## Supersedes

None.

## Removal plan

当前无第三方宿主旧路径可移除；SDK 独立使用保持可用。若候选方案未被接受，不引入兼容桥。
