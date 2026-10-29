# ADR 0075: Wake Test 工程控制与可信统计

- Status: accepted
- Product maturity: experimental
- Date: 2026-10-29

## Context

Wake Test 已有原生测试内核，但宿主简化哈希、覆盖率仅包含已加载图、有限的 project 覆盖以及缺少
整轮生命周期、重试和指定 Git 基线筛选，限制了测试结果可信度和多项目使用。

## Decision

保持 ADR 0020 的所有者和外部快照格式。Wake 原生增加项目配置覆盖、配置驱动的整轮 setup/teardown、
显式 Git 基线、逐用例重试和预期失败测试；不引入 Jest/Node runner 或源码内快照改写。
测试选项继续由 wake_test_contract 拥有，并经 wake_app 同时服务 CLI、Node 和持久 host。

## Invariants

- 整轮生命周期由 wake_test 在独立 fast DOM realm 中执行，setup 和 teardown 共用该 realm；
  每次 run/watch 重跑均重新建立。teardown 在 setup、测试或取消失败后仍尽力执行，受独立 deadline 约束。
  phase 之间暂停 Wake 拥有的计时器，结束时取消残留任务，不引入独立后台事件循环。
- retries 默认零，只重试普通测试回调失败，重新执行 beforeEach/afterEach 和清理；不吞掉超时、
  hook、清理、泄漏、快照或基础设施错误。beforeAll/afterAll 仍只执行一次，结果 attempts 记录实际次数。
- test.failing 只反转测试回调的普通异常；hook、超时和框架错误始终失败。
- coverage.collect 从显式 glob 中纳入未加载源码，不执行它们；以同一源码映射和统计模型生成零命中数据。
  按项目独立收集、报告和检查阈值；跨 suite 的命中继续按源码身份合并。
- changed_since 对已验证的 commit/branch 与 HEAD 取 merge-base，再纳入工作树和未忽略的新文件；
  不解析 shell，不接受无效或歧义 revision，不猜测远程基线。
- 未支持的宿主算法和编码报结构化错误，不以近似实现伪装为成功。

## Evidence

`wake_test::engineering_controls_tests` 覆盖重试边界、预期失败、外部异常快照、空测试集覆盖率、
项目报告隔离、生命周期超时/取消/清理和 watch 冷热等价；同一调度器在系统 Chromium 中执行回归。
`host_hashes_use_standard_algorithms_and_reject_unsupported_semantics` 使用 SHA 标准向量、UTF-8
异常字符、字节视图和非法算法验证宿主行为。`wake_compiler_core::coverage_syntax_tests` 验证原始 TSX 元数据。
`wake_config` 验证严格字段；`wake_node`、CLI 与架构字段集合测试验证选项传递；
`wake_test_contract` 保护旧请求和新增可选 changedSince 字段。既有 DOM/Chromium TSX 覆盖率、
截图和超时回归，以及 Test262 固定清单共同验证共享运行时。

## Consequences

测试产品保持 experimental。新增能力是 Wake 自有契约，不承诺 Jest ABI。项目未设置的字段继承根配置，
显式集合替换继承值；嵌套配置按已公开的覆盖规则处理。生命周期只保证 Wake 支持的宿主能力。

## Validation

先运行新增回归的失败证据，再运行受影响 crate 测试、clippy、公开类型和包消费测试；
执行架构检查及测试，运行测试内核、DOM、browser、host 和 CLI/Node 的受影响门禁。

## Supersedes

None.

## Removal plan

删除简化哈希替身，不保留兼容开关。无临时 runner 或第二套结果模型。
