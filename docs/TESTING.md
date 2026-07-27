# 测试与质量门禁

## 1. 提交前命令

```powershell
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
```

当前状态：全量测试通过；Clippy 门禁尚未通过，具体见 [AUDIT.md](AUDIT.md)。

## 2. 测试层次

### 单元测试

适合 lexer、parser 规则、路径规范化、hash/key 生成、活跃性转移、chunk 划分和缓存编码。

### 属性与模糊测试

重点不变量：

- lexer/parser 对任意字节不发生越界或永久循环；
- codegen 后再次 parse 不崩溃；
- 模块/chunk 输出不依赖 hash map 遍历顺序；
- cache decode 对截断、随机和旧版本输入安全失败。

### 并发测试

`wake_turbo` 需要覆盖：

- 同 key 并发请求只执行一次；
- 依赖变化期间没有丢失唤醒；
- 任务 panic、错误或取消后等待者全部结束；
- cycle 检测不会留下永久 running cell；
- 并发度 1 与 N 的结果完全一致。

状态机级测试使用 Loom；吞吐和真实阻塞行为使用普通线程测试。

### 端到端测试

每个重要能力至少验证两件事：

1. 产物结构符合预期；
2. 产物在 Node 或浏览器样环境中执行正确。

现有测试已经覆盖 ESM/CJS、TypeScript、JSX、CSS、动态 import、TLA、tree shaking、PnP 和缓存，应继续保留。

## 3. 必补回归矩阵

| 场景 | 冷内存 | 热内存 | 冷持久化 | 热持久化 |
|---|---:|---:|---:|---:|
| 基础 ESM | ✓ | ✓ | ✓ | ✓ |
| Tree shaking | ✓ | ✓ | ✓ | ✓ |
| Code splitting | ✓ | ✓ | ✓ | ✓ |
| TLA | ✓ | ✓ | ✓ | ✓ |
| CSS/assets | ✓ | ✓ | ✓ | ✓ |
| Source map | ✓ | ✓ | ✓ | ✓ |

四种模式的 JS、CSS、asset、manifest 和 source map 应逐字节一致。允许不同的只有统计信息和耗时。

## 4. 缓存失效矩阵

逐项改变以下输入，并断言相关任务 miss、无关任务仍可 hit：

- 源文本和文件类型；
- define、target、dev/prod、minify、tree shaking；
- alias、解析扩展名、package exports 条件；
- PnP manifest；
- CSS inline 阈值与 public path；
- cache schema 和算法 revision。

## 5. 确定性测试

同一 fixture：

- 重复构建至少 20 次；
- 使用不同线程数；
- 打乱文件系统枚举顺序；
- 使用不同进程；
- 分别从空缓存和热缓存启动。

比较所有输出文件名和字节。若失败，应打印首个不同阶段的稳定摘要，而不是只比较最终 bundle。

## 6. 性能基准

基准必须固定机器信息、Rust 版本、profile、线程数和 fixture revision。至少报告中位数、p95 和峰值内存：

- 冷构建；
- 无修改 rebuild；
- 单叶模块修改；
- 公共依赖修改；
- 1k/10k 模块宽图；
- source map 和 minify 独立开关。

性能优化不得通过减少正确性检查或让热缓存产物退化来换取。
