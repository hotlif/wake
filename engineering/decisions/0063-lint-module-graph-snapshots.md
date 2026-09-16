# ADR 0063: lint 模块图快照与项目分析

- Status: accepted
- Date: 2026-09-14

## Context

六条 import 规则需要原始请求、解析身份、依赖拓扑和包声明。ADR 0053 的源码事实以及
ADR 0022 的 ResolutionEnvironment 已分别拥有语法/符号与实际模块解析；不能从生成的
JSX helper、TS 擦除结果或文件名猜测这些信息。单文件缓存键不能证明跨文件结论有效。

## Decision

原始 module request 由纯 lint 核心投影 parser 的 import/export 事实和原生 AST/源码引用，
保留原始字符串范围、静态/动态/require、纯类型与 import attributes。只接受真实未遮蔽的
CommonJS require；非字面量请求明确保留为动态未知，不伪造已解析目标。

wake_app 拥有有界项目快照、文件发现、未保存文档覆盖、取消和解析调度。每轮通过新的
ResolutionEnvironment 读取经典 npm 或 PnP/zip、条件 exports、alias 和最近包声明；不执行
JavaScript 配置、加载目标模块或恢复私有 resolver。核心消费 owned 请求及解析/拓扑事实，
产生规则诊断后统一经过真实注释抑制、基线、排序和安全修复流程。
原始请求是否属于包、包根名称以及 alias 与 PnP 的优先级由 resolver 提供只读查询；应用
不能自行拆分 scoped 名称或把 PnP 已选择的包请求误作本地 alias。查询不要求包已安装，
便于缺失目标仍检查 issuer 声明；损坏 PnP 清单保持权威失败。

应用文件系统快照在同一检查内保留首次读取的字节、失败、文件种类、目录列表与 canonical
结果；未保存源码仅覆盖显式文件，其他路径继续读取基础文件系统。新虚拟目录必须出现在
目录列表中，负面观察不能因同轮磁盘新增而改变。每轮新建快照恢复后续磁盘变化。
快照最多保留 512 MiB 输入字节及 250,000 个观察路径/目录项，输入计数包含路径文本与 PnP
实体 archive；虚拟目录不能覆盖已存在的文件。源码图另有
128 MiB 上限。取消和预算失败须锁存，FileSystem 的 bool 接口返回 false 不能吞掉该失败；
解析协调者在消费解析结果及返回项目前检查锁存错误。观察集合供 watch 建立依赖覆盖。

项目图以所选源码为根，按窗口解析其传递依赖，未选择或被发现规则忽略的依赖仍提供拓扑事实。
每个解析上下文的四种运行时/纯类型 × import/require 解析环境共享同一文件系统快照，各自拥有正确的入口、条件
和扩展名集合；根 Wake alias 同样适用。类型环境优先 types/typings 和声明扩展名，运行时环境
保留 Wake 的 TS 孪生与扩展名策略；TS import-type 的 resolution-mode 明确选择 import/require。
这描述 Wake 的模块解析事实，不以名称替代 P5 的 TypeScript 类型服务。
Node 内建身份按 Node 24.0.0 默认能力冻结，完整名称匹配，含精确的标准子路径与必须带 node:
前缀的模块；不按首段或任意 node: 字符串放行，也不读取当前宿主的动态模块列表。
内建身份表示模块可识别，不声明浏览器或其他执行宿主能够提供该 API。
参考 [Node 24 模块契约](https://nodejs.org/download/release/v24.0.0/docs/api/modules.html#built-in-modules)。
解析成功的 JSON/CSS 为非 JS 叶子，其他无法解析的代码载荷保留 opaque；从不加载目标代码。
图构造失败仍返回已知观察路径，以便监控缺失依赖、坏 PnP 清单和恢复事件。

resolver 提供不透明的 ResolutionContext，应用只保存、比较并传回解析环境，不能创建任意
PnP 根或重写其 package map。真实最近的 PnP 根优先；没有祖先清单时，继承上下文只接管
其 manifest 明确拥有的 issuer。未拥有的外部文件保持普通解析，坏清单不会回退。
图节点身份同时包含完整 ModuleIdentity 和 ResolutionContext；各上下文拥有独立解析缓存，
共享源码快照。每轮最多 256 个解析上下文。该入口扩展不改变现有无上下文调用的解析结果。

模块分析和普通批次共享进程的两个准入名额；模块检查从第一次源码读取到最后一个分析窗口
持有名额，在同一个共享执行池内按窗口运行，不嵌套准入或创建独立线程池。
窗口句柄只有一个协调者，不能共享到并发窗口；取消窗口不提前释放仍由协调者持有的名额。
模块图保留的源码、节点、边及工作量有显式上限，超限返回 WAKE_LINT_ANALYSIS。只启用单文件规则时
继续沿用现有批次与缓存路径。

模块规则启用的文件绕过现有单文件持久缓存，cache 统计明确记录 bypass。同一轮冻结已读取
源码与未保存版本；修复预览使用最终虚拟源码建立项目结论，所有 graph/规则/抑制分析通过后
才进入既有源码发布事务。后续持久化模块分析必须单独登记依赖身份，不复用内容键伪造命中。
每轮重建解析环境使 package/PnP/alias/源码变化不受旧进程缓存影响；watch 同时观察根外依赖、
解析失败 witnesses 和 PnP 物理 archive，不能只观察已经成功的源文件。
上下文只接受当前代次的观察集合，成功或失败均替换旧集合，取消及过时代次不能覆盖它。
监听注册集合随观察变化更新：项目内的实际依赖穿透默认忽略目录，根外依赖及负面路径由
最近现存目录覆盖；结构事件同时覆盖祖先替换。新增监听覆盖后必须重新检查，关闭首次
读取与注册之间的事件缺口。关闭上下文释放观察状态，停止监听释放注册，不累积历史依赖。
目录本身的非结构修改通知不代表源码变化（例如忽略输出目录创建引起父目录时间更新）；
使用文件/子路径事件、结构事件及目录身份探测恢复实际变化，避免该通知绕过忽略规则。

## Invariants

- core 不读文件、不发现配置、不解析安装布局；resolver 不包含规则 ID 或 lint 诊断策略。
- 同名、同版本、不同 PnP peer/virtual 上下文不能成为同一个依赖节点。
- PnP 包位于项目外缓存时，其传递请求仍由导入该包的 PnP 权威解析；嵌套项目可选择不同权威，
  不能仅按全局缓存文件的祖先寻找清单，也不能把一个项目的权威传播给不属于该清单的源码。
- 纯类型边、动态未知边与运行时边显式区分；缺失解析和不完整图不能充当无环证明。
- CLI、Node、上下文、stdin、修复和基线经过同一项目事实与抑制顺序。
- 图失败不会发布源码或把部分项目结论写入单文件缓存；取消释放所有准入与临时状态。

## Evidence

- `wake_lint_core/tests/module_requests.rs`、`module_graph.rs`：原始请求、规则、参数、身份、
  类型/动态/不完整边、迭代拓扑及预算；原始 import-type parser fixture 与编译回归。
- `wake_app/src/lint/module_snapshot.rs`、`modules.rs`：稀疏覆盖、保留负面观察、取消/预算、
  解析 profiles、全局 PnP 缓存、嵌套根和独立上下文；resolver 69 项与 bundler 回归。
- `wake_app/tests/lint_modules.rs`：项目 alias、混合缓存、最终源码修复、失败不发布、stdin、
  未保存依赖、指令/基线以及共享物理 ZIP 的独立虚拟 peer 身份和 archive 监听。
- `wake_app/tests/lint_watch.rs` 与 context/watch 单元测试：根外/忽略目录/缺失依赖、坏 PnP
  恢复、旧观察退出、忽略输出、代次隔离和注册后重新检查。
- `wake_cli/tests/lint.rs` 12 项及真实 Node addon 32 项、公开类型 schema；真实 npm CLI 冒烟。

## Consequences

模块规则使用真实 Wake 安装与源码身份，代价是启用这些规则时需要有界项目分析及缓存绕过。
完整交付仍须实现规则参数与迁移，并验证真实项目、PnP、编辑器和发布消费路径。
解析权威仍属于 ResolutionEnvironment；保留上下文扩展了项目外 PnP issuer 的数据流，
没有上下文的现有解析入口保持原行为。监听仅为实际查询的依赖及负面路径增加覆盖。

## Validation

先运行原始模块请求与六条规则的最小失败测试，再验证解析、未保存源码、循环、抑制、修复、
取消、冷热与配置失效。运行 core/app/resolver、真实 CLI/Node/上下文、类型与架构门禁。
准入、项目快照和发布顺序必须有独立失败/恢复与确定性证据。

## Supersedes

None.

## Amends

- [ADR 0022](0022-yarn-pnp-ownership.md): 不透明 ResolutionContext 保留根外 PnP issuer 的原解析权威，实际最近根优先。
- [ADR 0053](0053-source-semantic-facts-for-lint.md): 原始模块请求投影及源绑定 owned 模块图进入纯核心。
- [ADR 0055](0055-content-addressed-lint-cache.md): 模块规则文件显式绕过单文件缓存，混合项目保留普通文件命中。
- [ADR 0057](0057-versioned-lint-project-context.md): 当前成功或失败代次发布依赖观察，取消及旧代次不得替换。
- [ADR 0058](0058-lint-watch-lifecycle.md): 实际模块依赖及负面路径扩展根外监听覆盖，注册变化后重检，目录时间事件不绕过输出忽略。
- [ADR 0059](0059-bounded-lint-file-execution.md): 模块项目在同一准入内执行多个有界窗口，保留源码图预算及唯一协调方。

## Removal plan

无兼容解析器或持久化旧格式；模块规则未启用时保留现有单文件执行与缓存路径。
