# Northstar 2k modules benchmark

本 fixture 是一个由生成器维护的、确定性的商务控制台项目，不是把独立文件平铺导入入口的计数样例。
业务代码按 API、模型、规则、指标、组件、本地化与页面聚合分层，2000 个应用模块都必须从应用根自然可达；
构建器还会解析一个只调用 `runApplication()` 的 entry wrapper，因此实际 bundler 输入为 2001 个模块。
`expected/project.json` 是模块类别、依赖图和源码规模的机器可读事实来源。

## 准备与运行

从仓库根构建本轮 Wake release 二进制，再安装 fixture 锁定的比较工具：

```bash
cargo build --release -p wake_cli
cd fixtures/2k-modules
npm ci
npm run bench
```

`bench` 与 `compare` 是同一 runner 的两个名字。两者都会强制运行：

```bash
node generate.mjs
node verify-project.mjs
```

因此修改生成器后不会静默复用本机遗留的 `input/src`。生成源码属于派生文件并被 Git 忽略；
`expected/project.json`、`expected/checksum.txt` 和入口由生成器同步维护。

普通生成和 benchmark **不会**改写 committed oracle；业务结果、依赖图或源码摘要变化时会直接失败。
只有审阅并确认合同变化后，才显式执行 `npm run generate:update`，再检查 `expected/` diff。

## 正确性合同

1. `verify-project.mjs` 校验 manifest、12 个 bounded contexts、模块类别、静态依赖边和从入口出发的完整可达性。
2. runner 直接执行 `input/entry.js`，其完整 stdout 必须与 tracked oracle 逐字节相同。
3. 每次 Wake、Vite、webpack 构建后都递归检查输出目录：必须恰好有一个 `.js`，且不得有 Source Map。
4. 每个最终 bundle 都由 Node 执行，完整 stdout 必须与同轮源码 oracle 逐字节相同。

oracle 覆盖确定性的 Northstar 业务流程、全部授权 API 请求，以及所有 API/page 定义、模型、规则、
指标、组件树、授权页面树和本地化输出的分类 digest；它不包含构建器私有的模块 ID、chunk 名或文本形态。

## 公平性与测量口径

- 三方都面向 Chrome/Edge 120、Firefox 121、Safari/iOS 17.2，产出 browser IIFE 单 bundle，启用生产
  minify，关闭 Source Map 和持久缓存。
- 单 bundle 是硬合同：Vite 禁用 code splitting，webpack 使用 `LimitChunkCountPlugin`，Wake 使用严格
  `bundle --outfile`；意外新增 chunk 会由 runner 拒绝。
- 每次预热、计时和内存构建前，runner 都在计时区间外删除该工具自己的 `dist-*`；工具自身的重复清理关闭。
- 每个工具先预热 1 次，再直接启动新进程计时 5 次；报告 average 与 min–max。
- 内存另建 2 次：Windows 采样 WorkingSet，Linux 读取 GNU time 最大 RSS，macOS 读取 BSD time 最大 RSS。
  不支持系统级采样的其他平台仍完成构建与正确性校验，并把内存标为 unavailable。
- 体积只统计通过单 bundle 合同的最终 JavaScript，分别报告 raw、gzip level 9、Brotli quality 11 的精确字节。

比较结果还必须记录提交、操作系统、CPU、内存、Rust/Node 版本、电源模式和后台负载。共享 CI 不设置毫秒
SLA；CI 校验两次生成完全确定、项目图与业务 oracle，并维护 work-count 与 benchmark 编译门禁。

## 历史结果

Northstar 的真实依赖图和业务 oracle 取代了旧的 2013 个 side-effect 注册模块。两套语料的图结构、源码
规模、运行工作量和压缩特征不同，因此 ADR 0023/0024 中的旧速度与体积数字不能作为 Northstar 的回归基线。
