# Crate 依赖方向约束

> 对应：DESIGN §3.1、PLAN §0.1。此文件是「谁能依赖谁」的规范来源；新增跨 crate 依赖前先查这里。

## 分层（自底向上，箭头表示「依赖」）

```
                         wake_cli  (bin: wake)
                            │
             ┌──────────────┴───────────────┐
             ▼                              ▼
      wake_dev_server ───────────►  wake_bundler
             │                        │  │  │  │
             │        ┌───────────────┘  │  │  └────────────┐
             ▼        ▼                  ▼  ▼               ▼
          wake_turbo  wake_cache   wake_graph  wake_resolver  wake_css
             │  ▲        │              │
             │  └────────┘              ▼
             │              wake_ecma_{codegen,transform}
             │                          │
   wake_turbo_macros                    ▼
                              wake_ecma_parser
                                        │
                              ┌─────────┴─────────┐
                              ▼                   ▼
                       wake_ecma_lexer      wake_ecma_ast
                              │                   │
                              └─────────┬─────────┘
                                        ▼
                                   wake_common   ← 地基，不依赖任何 wake crate
```

## 逐 crate 允许的内部依赖

| crate | 允许依赖 | 角色 |
|-------|----------|------|
| `wake_common` | （无） | Span / Atom / 诊断 / FileSystem 地基 |
| `wake_turbo_macros` | （无，仅 syn/quote/proc-macro2） | `#[wake::task]` 过程宏 |
| `wake_turbo` | `wake_common`, `wake_turbo_macros` | 增量引擎 |
| `wake_ecma_ast` | `wake_common` | arena AST |
| `wake_ecma_lexer` | `wake_common` | 词法分析 |
| `wake_ecma_parser` | `wake_common`, `wake_ecma_ast`, `wake_ecma_lexer` | 语法分析 |
| `wake_ecma_transform` | `wake_common`, `wake_ecma_ast` | 转换 pass |
| `wake_ecma_codegen` | `wake_common`, `wake_ecma_ast` | 代码生成 |
| `wake_resolver` | `wake_common` | 模块解析 |
| `wake_graph` | `wake_common`, `wake_ecma_ast`（只读视图） | 模块图 / tree shaking |
| `wake_css` | `wake_common` | CSS |
| `wake_cache` | `wake_common`, `wake_turbo` | 持久化 |
| `wake_bundler` | `wake_common`, `wake_turbo`, `wake_ecma_*`, `wake_resolver`, `wake_graph`, `wake_css`, `wake_cache` | 编排器 |
| `wake_dev_server` | `wake_common`, `wake_turbo`, `wake_bundler` | HTTP / HMR |
| `wake_cli` | `wake_common`（P0）；`wake_bundler`, `wake_dev_server`（P3/P5 接入） | CLI 入口 |

## 硬规则

1. **无环**：上表构成 DAG。`ast ← lexer ← parser ← transform/codegen`；`graph` 只依赖 `ast` 的只读视图。
2. **编译核心依赖白名单**（DESIGN §14.1）：`wake_ecma_*` 只允许 `bumpalo / rustc-hash / memchr /
   simdutf8 / xxhash-rust`。带框架性质或拉传递依赖树的库禁止进入编译核心。
3. **引擎/网络库隔离**：`tokio / axum / notify` 只能出现在 `wake_dev_server`；`dashmap /
   crossbeam-deque` 只能在 `wake_turbo`（及必要的 `wake_cache`）。编译核心绝不碰。
4. **cli → bundler/dev_server 的边** 在 P3/P5 实际接入时才加，避免 P0 引入未使用依赖。

## 如何验证

- `cargo build --workspace` 通过即证明无环（Cargo 拒绝循环依赖）。
- 依赖是否越界：人工 review + 未来加 `cargo-deny` / 自定义脚本校验各 `wake_ecma_*` 的
  `Cargo.toml` 只含白名单。
