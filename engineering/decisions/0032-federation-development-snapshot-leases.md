# ADR 0032: Federation 开发快照使用浏览器租约与有界竞态窗口

- Status: accepted
- Date: 2026-09-02

## Context

ADR 0025 要求开发 broker 为页面曾接受的每个 build 永久缓存 Manifest，开发服务器也把每个历史
`FederationSnapshot`（Manifest、稳定路由、build 路由与 identity map）保留到进程结束。这个策略能让
旧页面继续请求 lazy chunk，但没有活跃页面证据；连续编辑会使服务端内存和浏览器 map 随 generation
无界增长。

只对 history 设置静默数量上限会破坏仍在运行的旧 requester。另一方面，HTTP build URL 是公开输入；
让任意 410 请求广播 full reload 会把构造的 unknown build URL 变成远程刷新 DoS。生命周期必须同时
证明旧 runtime 的可用性、无客户端时的有界内存、连接断开后的回收和请求局部的失败语义。

## Decision

Federation development 增加独立的 `wake.federation.dev-lease.v1` WebSocket 协议。浏览器发送 `lease`
帧，原子替换该连接完整的、排序去重的 buildId 集合；服务端返回 `lease-ack`。服务端发出的
`full-reload` 控制携带 remote、当前 buildId/generation、可选过期 buildId，以及封闭原因
`build-gone`、`invalid-lease`、`lease-limit` 或 `update-lagged`。

`lease-ack` 不是只确认 buildId 集合的收据：浏览器必须把 ack 的 `currentBuildId/generation` 与自己已
接受的 development cursor 精确比较。即使 lease 集合未变，types-only 或 isolated 状态推进也要按新
cursor 重新发送 replacement；ack 超前、落后或身份不同都说明连接期间存在 generation gap，只刷新该
页面并终止旧 socket。cursor 相等时记录已确认 cursor，不降级已接受状态，也不把 ack 冒充尚未派发的
isolated/full-reload action。

每连接最多拥有 8 个 build lease。隔离 remount 可能让旧 requester 与新 isolated runtime 同时存活，
因此集合不能简化为一个“最新 build”；第 9 个 build 必须显式整页刷新，不能静默淘汰。连接 open、
Manifest 接受和开发更新状态变化都会同步实际活跃集合；断开连接释放该连接的全部 refcount。非法、
重复、跨 remote、未知或超限的替换在修改 refcount 前整体失败，不得增长内存。

每个 enabled development container 独占一个 bounded broadcast sender；多 mount 配置不得复用
`container_name`。因此 sibling remote 的消息风暴只能 lag 自己的连接，不能让另一个 remote socket
收到 `update-lagged`。公开 `ServerHandle` 发布也必须按 remote 查找对应 sender，不能回退到全局总线。

`wake_dev_server` 只完整保存 current snapshot。retired history 仅保存 build-scoped routes，并且只在
至少一个连接租约该 build，或它位于当前 generation 之前的固定 2 代 grace window 时保留。两代窗口
覆盖 Manifest 接受后、WebSocket lease 到达前以及 Manifest 到 remote entry 的短竞态；它不是页面活跃
证据。服务端历史内存因此为
`O(distinct active browser build leases + 2 grace generations)`。

HTTP route lookup 是纯读操作，绝不广播、申领租约或修改 refcount。已保留 build 中未授权/不存在的
文件仍返回 404；语法正确但已裁剪或未知的 build URL 返回 410。GET body 是 typed `full-reload` JSON；
GET 与 HEAD 同时返回版本化 `Wake-Federation-*` 控制头，包含 currentBuildId 与 generation，并通过
`Access-Control-Expose-Headers` 允许跨 origin 浏览器读取。浏览器的强制 HEAD preflight 只有在 schema、
remote、expiredBuildId、currentBuildId、generation 和 reason 全部与本次 development asset identity
匹配时才刷新当前页面；native load 失败后的 GET 也验证同一 JSON。缺失、损坏或跨 remote 控制退化为
普通 `FED_NETWORK`，不得刷新。

`/@wake/federation/` 与 `wake-federation.json` 是服务端保留命名空间。lookup 必须区分
`NotFederation` 与 reserved `Missing`；reserved missing 立即 404，不能继续命中 public 文件或
extensionless SPA fallback。410 指向不同 `currentBuildId` 时，控制 generation 必须严格大于请求资产
generation；equal-generation HEAD/GET 只产生普通网络错误，不能刷新。

本决策只修改 development remote。Production Manifest/lock/SRI 和页面生命周期缓存保持 ADR 0025 的
既有语义，不发送 lease，也不因开发上限裁剪。remote registration 的 `mode` 是 runtime 是否启用开发
行为的唯一 owner；production Manifest 即使携带 `development` metadata，该字段也保持 inert，所有
entry/expose/style asset context 都固定 `development=false`、generation 0。

## Invariants

- 一个 socket 的 lease 是完整替换集合，长度为 1..=8，buildId 必须排序、唯一且属于该 remote 的
  current/history；验证完成前 refcount 不变。
- current snapshot 永远完整；retired snapshot 只含 build-scoped routes，不复制稳定 Manifest 路由和
  identity maps。
- 无连接时连续数百次成功发布最多保留 2 个 retired generation。
- 任一已租约旧 build 在连接存活期间都能取得其授权 lazy assets；最后一个连接释放后，超出 grace 的
  build 可立即回收。
- WS invalid/expired/over-limit/lagged 只向该连接发送 typed full reload；不得广播其他连接。
- lease ack 必须同时匹配 buildIds 和浏览器已接受的 currentBuildId/generation；相同 build set 的 cursor
  推进仍需重新确认，重连漏代必须定向刷新。
- 每个 enabled container 独占 broadcast sender，且多 mount container name 唯一；一个 remote 的 65+
  消息不得让 sibling remote socket lagged。
- HTTP 410 是无副作用的纯读结果。任意客户端构造 unknown build URL 都不能导致 socket 广播或状态变化。
- known build 的 unknown/unauthorized file 是 404；well-formed unknown/pruned build 是 410。
- reserved Federation missing route 不得落入 public/SPA；不同 currentBuildId 的 410 只有 generation 严格
  增长才允许刷新。
- 410 HEAD 没有 body，因此完整控制必须存在于 CORS-exposed headers；解析失败不得触发刷新。
- snapshot install、lease replacement 和 HTTP cursor 读取共用同一 mount publication lock，不能交叉
  currentBuildId 与 generation。
- 浏览器 development accepted-manifest/lease 集合不超过 8；生产缓存路径不读取该开发集合，production
  asset context 只读取 remote mode 而不读取 Manifest development metadata。

## Evidence

- `crates/wake_federation_contract/src/dev.rs` 拥有 lease/ack/full-reload DTO、封闭原因和 canonical set
  校验；`FEDERATION_DEV_MAX_BUILD_LEASES` 固定为 8。
- `crates/wake_dev_server/src/federation.rs` 拥有 route-only history、lease refcount、2-generation grace、
  原子替换/释放和 Found/Missing/Gone lookup。
- `crates/wake_dev_server/src/lib.rs` 拥有 per-remote sender、唯一 container 配置校验、per-connection WS
  生命周期、reserved namespace、typed 410 GET/HEAD、CORS exposed headers，以及 HTTP 不广播行为。
- `npm/wake/federation.mjs` 与嵌入式 runtime 镜像发送真实 lease frame，限制开发 accepted builds，
  严格处理 WS/HEAD/GET full reload；`npm/wake/federation.d.mts` 暴露同一公共类型。
- Rust 回归覆盖 300 个 idle generation、长期旧 lease、双连接 refcount、断开回收、非法/超限不增长、
  reserved public/SPA 隔离、404/410 分界、重复 container 拒绝、sibling remote 256-frame lag 隔离和真实
  WebSocket ack/reload。Node 回归执行真实 runtime WebSocket `send()`、重连 cursor gap、第 9 build
  reload、production mode 隔离、跨 origin HEAD/equal-generation 拒绝以及 native GET JSON。

## Consequences

开发页面可以跨多次 isolated remount 保留旧 requester，但最多 8 个活跃 build；超过边界会诚实地刷新
并释放页面生命周期内的资源。无浏览器或断开的浏览器不再让服务端永久持有全部历史。短暂断网超过
两代 grace 后重连旧页面会收到定向 full reload，这是显式恢复语义而不是悬空 dynamic import 或伪装
404。

本决策修订 ADR 0025 的 decision 15 与相关 invariant：开发态 previously accepted Manifest 不再无条件
缓存到页面结束，而是由有界 active lease 集合拥有；production 页面缓存事实没有改变。

## Validation

- `cargo test -p wake_federation_contract`
- `cargo test -p wake_dev_server --lib`
- `cargo test -p wake_app federation_runtime_asset_matches_the_published_runtime`
- `node --test npm/wake/test/federation.test.mjs npm/wake/test/federation-react.test.mjs`
- `corepack yarn npm:typecheck:wake`
- `cargo clippy -p wake_federation_contract -p wake_dev_server -p wake_app --all-targets -- -D warnings`
- `cargo fmt --all -- --check`
- `corepack yarn architecture:check && node --test scripts/check-architecture.test.mjs`

## Supersedes

None.

## Amends

- [ADR 0025](0025-wake-native-federation-contract.md): decision 15 及相关 invariant 的开发快照保留边界

## Removal plan

旧的无界 `history: BTreeMap<String, FederationSnapshot>` 与 development `retiredBuildIds` 永久集合在本
切片直接删除，不保留并行实现。未来若提高 8/2 常量或改变租约粒度，必须提供浏览器生命周期、竞态和
内存上界的新证据；不得恢复 silent cap、HTTP 广播副作用或生产/开发共用的裁剪路径。
