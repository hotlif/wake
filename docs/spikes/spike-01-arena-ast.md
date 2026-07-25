# Spike ① — arena AST + 自引用持有者

> 对应：PLAN §0.5 / DESIGN §10.4
> 结论：**可行（GREEN）**，采用「Box\<Bump\> + 生命周期擦写 + `for<'a>` 安全借用」方案。
> 代码：[`crates/wake_ecma_ast/src/holder.rs`](../../crates/wake_ecma_ast/src/holder.rs)、
> demo：`cargo run -p wake_ecma_ast --example spike_arena_ast`

## 1. 要验证什么

wake_turbo 引擎的任务输出要求 `'static`（存进分片 slot 表、以 `Arc` 持有），而 arena AST 带
生命周期参数 `Program<'a>`（引用指回 `Bump`）。这是两大自研件的正面冲突（DESIGN §10.4）。

需要证明：能把 `Bump` arena 与借它的 `Program<'self>` 封成一个 `'static` 类型，对外提供
**安全** 的借用接口，且 **无 UB**（miri 验证）。若不可行，退回备选：AST 索引化（用 `u32` id
代替 `&'a` 引用，牺牲一点解引用成本换掉生命周期传染）。

## 2. 方案

自引用持有者 `ModuleAst`：

```rust
pub struct ModuleAst {
    program: Program<'static>,   // arena 内 AST 的 'static 视图（不变量维护）
    _arena: Box<Bump>,           // 地址稳定的 arena，与 program 同生共死
    structure_hash: u64,         // 构建时算好的结构指纹（非指针）
}
```

> 注：`program` 字段实为 `Program<'static>`（生命周期擦写），arena 用 **裸指针** `NonNull<Bump>`
> 经 `ArenaOwner` 持有——**不** 用 `Box<Bump>`，原因见 §3.2「miri 首轮捕获的 bug」。

- **构造**：`from_builder<F: for<'a> FnOnce(&'a Bump) -> Program<'a>>`。`Box::into_raw` 得到
  `NonNull<Bump>`，经 **共享** 引用 `ptr.as_ref()` 构建 `Program<'b>`，`unsafe transmute` 到
  `Program<'static>`，与 arena 句柄一并封入结构体。
- **借用**：`with_ast<R>(&self, f: impl for<'a> FnOnce(&'a Program<'a>) -> R) -> R`。把 `'static`
  视图协变收窄到本次借用的生命周期。

## 3. 健全性论证（4 条不变量）

1. **program 只引用 arena 内分配**：由构造闭包签名 `for<'a> FnOnce(&'a Bump) -> Program<'a>`
   强制——闭包对 *任意* `'a` 都要成立，无法把外部（更短生命周期）引用混进返回的 `Program`。
2. **arena 不 reset、与 program 同 drop**：二者是同一结构体的字段。字段 drop 顺序为
   `program`（只含引用与 `Copy` 元素 `Stmt`，无 Drop 副作用）先、`_arena` 后释放内存 →
   不存在「先释放 arena 再访问 program」的窗口。
3. **不泄漏 'static 视图**：`with_ast` 的 `for<'a>` 高阶约束使闭包无法把 arena 引用带出——
   带出的引用类型需对所有 `'a` 成立，除非它根本不借 arena（如 `Copy` 的 `Span`）。
4. **指纹用结构 hash 而非指针**：`structure_hash` 由 FNV 折叠 AST 形状/字面量/运算符得到，
   跨实例、跨重启稳定（demo 中两次 `build_sample(6)` 指纹相等）。这满足 DESIGN §10.4「指纹用
   结构 hash 而非指针」的纪律，也是持久化正确性（§10.3）的前提。

`transmute::<Program<'_>, Program<'static>>` 只擦写类型层面的借用记账，运行时表示不变
（引用即指针，生命周期是零大小信息），两个类型布局恒等，`transmute` 大小检查通过。

### 3.2 miri 首轮捕获的 bug（spike 的核心收获）

初版用 `_arena: Box<Bump>` 持有 arena，并在构造末尾 `ModuleAst { program, _arena: arena }`。
miri（Stacked Borrows）报错：

```
Undefined Behavior: trying to retag ... for SharedReadWrite ... but that tag does not exist
  <tag> was created by a SharedReadWrite retag at holder.rs (program 字段)
  <tag> was later invalidated by a Unique retag at holder.rs (_arena: arena)
```

**根因**：`Box<T>` 携带 `noalias`（唯一性）语义，把 `Box<Bump>` move 进结构体字段时会对其
指向的 `Bump` 做一次 `Unique` retag，**弹掉** `program` 内 bumpalo `Vec` 已持有的共享借用 tag →
后续任何经 `program` 的访问都踩到「已失效的 tag」= UB。这类 bug 在 release 下大概率「碰巧能跑」，
正是 DESIGN §13 里最难查的一类；**spike 的价值就在于用 miri 在写正式代码前把它挡下**。

**修复**：改用裸指针 `NonNull<Bump>`（不带唯一性断言），经 `ptr.as_ref()` 的共享引用构建
program；arena 的释放交给独立的 `ArenaOwner`（Drop 里 `Box::from_raw` 回收），并靠字段声明顺序
保证 `program` 先析构、`arena` 后释放（避免 program 的 `Vec` 析构读到已释放的 arena 内存）。
修复后 miri 全绿。

## 4. 验证结果

| 项 | 结果 |
|----|------|
| `cargo test -p wake_ecma_ast` | 7 passed（holder 4 + visit 1 + lib 2） |
| demo 运行 | 构建 / 遍历 / 批量持有者 drop 正常 |
| 结构指纹稳定且可区分 | 相同结构相等、不同深度不等 ✓ |
| `many_holders_drop_cleanly` | 50 个持有者建立并 drop 无 panic |
| **miri（Stacked Borrows）** | **7 tests passed，0 UB**（修复初版 Box bug 后，见 §3.2） |

### miri 命令

```bash
rustup toolchain install nightly --component miri
MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test -p wake_ecma_ast
```

CI（`.github/workflows/ci.yml`）加入常驻 miri job，防止后续 Phase 补全 AST 时重新引入自引用 UB。

## 5. 结论

自引用持有者方案 **可行**，成为 wake_ecma_ast 的正式设计，**不** 退回 AST 索引化备选。
后续 Phase 2 的全量 AST 沿用同一 `ModuleAst` 持有者与 `with_ast` 借用纪律：arena 引用绝不
逃出 `with_ast` 闭包边界。

**遗留给 Phase 2.5**：引擎要把 `ModuleAst` 放进 `Arc` 跨线程共享（只读），届时需为其手动 `impl Send`
（可能 `Sync`）并给出「构造后 arena 只读、绝不再 alloc」的安全性论证。P0 不需要，暂不实现。

## miri 运行记录

- 2026-07-23，nightly-2026-07-22 + miri 0.1.0：初版 `Box<Bump>` 持有触发 Stacked Borrows UB
  （Unique retag 弹掉共享借用），改 `NonNull<Bump>` + `ArenaOwner` 后 **7 tests passed, 0 UB**。
