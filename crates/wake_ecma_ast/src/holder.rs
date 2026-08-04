//! 自引用 AST 持有者 [`ModuleAst`]（Spike ①，PLAN §0.5 / DESIGN §10.4）。
//!
//! ## 要解决的冲突
//!
//! wake_turbo 引擎的任务输出要求 `'static`，而 arena AST 带生命周期参数——这是两大自研件的
//! 正面冲突。解法：把 `Bump` arena 与借它的 `Program<'self>` 封在同一个结构里，二者同生共死，
//! 对外只暴露安全的 [`ModuleAst::with_ast`] 借用接口。
//!
//! ## 不变量（保证 `unsafe` 健全）
//!
//! 1. `program` 中所有引用只指向 `arena` 内的分配——由构造闭包 `for<'a> FnOnce(&'a Bump) -> Program<'a>`
//!    的签名强制：闭包对任意生命周期 `'a` 都必须成立，因此无法混入外部引用。
//! 2. arena 用裸指针 [`NonNull<Bump>`] 持有，**不** 用 `Box<Bump>`——因为 `Box` 断言唯一性
//!    (noalias)，把它 move 进结构体会对 arena 做 `Unique` retag，使 `program` 内已有的共享借用
//!    失效（miri Stacked Borrows 违规，Spike ① 首轮实测捕获）。裸指针不作此断言。arena 一经分配
//!    不再 reset/移动其内容，只经共享引用只读访问；释放由 [`ArenaOwner`] 在 program **之后** 完成。
//! 3. 对外 **绝不** 泄漏 `'static` 视图：`with_ast` 用高阶生命周期 `for<'a>` 把借用收窄到调用期，
//!    闭包无法把 arena 引用带出（带出的引用需对所有 `'a` 成立，不可能）。
//! 4. 指纹用 AST 的 **结构** hash（构建时顺手算），绝不用指针地址——重启/复用池后指针会变。

use std::ptr::NonNull;

use bumpalo::Bump;
use wake_common::{Hash64, Interner};

use crate::Program;

/// arena 的所有权句柄：Drop 时回收堆上 `Bump`。
///
/// 单独成类型是为了 **控制 drop 顺序**：[`ModuleAst`] 无 `Drop` impl，字段按声明顺序析构——
/// `program` 先（其 bumpalo `Vec` 析构仍需读 arena 内存，此时 arena 尚在），`arena` 后
/// （`ArenaOwner::drop` 释放）。若在 `ModuleAst` 上写 `Drop::drop` 提前释放 arena，会造成
/// program 析构时的 use-after-free。
struct ArenaOwner(NonNull<Bump>);

impl Drop for ArenaOwner {
    fn drop(&mut self) {
        // SAFETY: `self.0` 来自 `Box::into_raw`，此前从未被释放；此刻 program 字段已析构完毕
        //（ModuleAst 无 Drop impl，字段按声明顺序 drop，program 在 arena 之前），无悬垂访问。
        unsafe { drop(Box::from_raw(self.0.as_ptr())) };
    }
}

/// 拥有一个 arena 及其上 AST 的 `'static` 持有者。可安全地放进引擎的 `Arc<_>` cell。
pub struct ModuleAst {
    /// arena 内 AST 的 `'static` 视图。真实生命周期 = `arena` 存活期，由本类型不变量维护。
    /// **字段顺序要紧**：`program` 先 drop（bumpalo `Vec` 析构需读 arena，彼时 arena 仍在），
    /// `arena` 后 drop 释放内存。
    program: Program<'static>,
    /// 裸指针持有的堆上 arena（见不变量 2）。仅为其 Drop 副作用存在（释放 arena），故加 `_` 前缀。
    _arena: ArenaOwner,
    /// 构建时算好的结构指纹（早期截断 / 缓存键用）。
    structure_hash: Hash64,
}

impl ModuleAst {
    /// 用一个「对任意生命周期都成立」的构造闭包建立持有者。
    ///
    /// 闭包在借来的 arena 上构建并返回 `Program`；本函数负责把二者封成 `'static` 持有者。
    pub fn from_builder<F>(build: F) -> ModuleAst
    where
        F: for<'a> FnOnce(&'a Bump) -> Program<'a>,
    {
        Self::from_builder_with_hash(|arena| {
            let program = build(arena);
            let structure_hash = crate::structure_hash(&program);
            (program, structure_hash)
        })
    }

    /// 使用调用方在构建期得到的稳定指纹创建 AST。
    ///
    /// parser 已持有完整源码，可用源码内容指纹覆盖 AST、span 及所有解析侧输出，避免构建后
    /// 再遍历整棵 AST。其它手工 AST 构造继续使用 [`Self::from_builder`] 的结构遍历。
    #[doc(hidden)]
    pub fn from_builder_with_hash<F>(build: F) -> ModuleAst
    where
        F: for<'a> FnOnce(&'a Bump) -> (Program<'a>, Hash64),
    {
        // 堆分配 arena，转为裸指针持有——绝不再以 `Box`/`&mut` 断言唯一性（不变量 2）。
        let ptr = NonNull::new(Box::into_raw(Box::new(Bump::new()))).expect("Box 指针非空");
        // SAFETY: `ptr` 指向刚分配、当前唯一持有的 Bump；以 **共享** 引用构建 program
        //（bumpalo 的 alloc 走 `&self` 内部可变，共享引用足够）。此共享 tag 在结构体存活期内
        // 不会被任何 Unique retag 弹出（我们此后只经共享引用只读访问 arena）。
        let arena_ref: &Bump = unsafe { ptr.as_ref() };
        let (program, structure_hash): (Program<'_>, Hash64) = build(arena_ref);

        // SAFETY: `program` 只引用 `*ptr` 内的分配（不变量 1，由 F 的 `for<'a>` 签名强制）。
        // 把生命周期擦写为 'static 仅改变类型层面的借用记账，不改变运行时表示（引用即指针）。
        // arena 与 program 一并封入返回值，由 ArenaOwner 保证在 program 之后释放（不变量 2）；
        // 对外只经 with_ast 以更短生命周期借出（不变量 3）。故该 transmute 不产生悬垂引用。
        let program: Program<'static> =
            unsafe { std::mem::transmute::<Program<'_>, Program<'static>>(program) };

        ModuleAst {
            program,
            _arena: ArenaOwner(ptr),
            structure_hash,
        }
    }

    /// 使用调用方预先计算的稳定指纹创建 AST，避免构建后再次遍历。
    /// 调用方必须保证该指纹覆盖所有会影响下游输出的输入。
    #[doc(hidden)]
    pub fn from_builder_prehashed<F>(structure_hash: Hash64, build: F) -> ModuleAst
    where
        F: for<'a> FnOnce(&'a Bump) -> Program<'a>,
    {
        // 堆分配 arena，转为裸指针持有——绝不再以 `Box`/`&mut` 断言唯一性（不变量 2）。
        let ptr = NonNull::new(Box::into_raw(Box::new(Bump::new()))).expect("Box 指针非空");
        // SAFETY: `ptr` 指向刚分配、当前唯一持有的 Bump；以共享引用构建 program。
        // bumpalo 的 alloc 走内部可变；构建完成后只经共享引用只读访问 arena。
        let arena_ref: &Bump = unsafe { ptr.as_ref() };
        let program: Program<'_> = build(arena_ref);

        // SAFETY: `program` 只引用 `*ptr` 内的分配；arena 与 program 一并封入返回值，
        // 由 ArenaOwner 保证在 program 之后释放，对外只经 with_ast 缩短生命周期。
        let program: Program<'static> =
            unsafe { std::mem::transmute::<Program<'_>, Program<'static>>(program) };

        ModuleAst {
            program,
            _arena: ArenaOwner(ptr),
            structure_hash,
        }
    }
    /// 便捷构造：样例 AST `let sum = 0 + 1 + ... + depth;`（spike 演示用）。
    pub fn build_sample(interner: &Interner, depth: u32) -> ModuleAst {
        ModuleAst::from_builder(|arena| crate::build_sample(arena, interner, depth))
    }

    /// 安全借用内部 AST。闭包无法把 arena 引用带出本次调用（不变量 3）。
    pub fn with_ast<R>(&self, f: impl for<'a> FnOnce(&'a Program<'a>) -> R) -> R {
        // 把 'static 视图协变收窄到本次借用的生命周期。
        let program: &Program<'static> = &self.program;
        f(program)
    }

    /// 结构指纹（构建时算好，O(1) 读取）。
    pub fn structure_hash(&self) -> Hash64 {
        self.structure_hash
    }

    /// 语句条数（经 with_ast 的便捷封装，验证借用接口可用）。
    pub fn statement_count(&self) -> usize {
        self.with_ast(|p| p.body.len())
    }
}

// SAFETY: `ModuleAst` 默认因 `NonNull<Bump>` 而 `!Send + !Sync`（裸指针的保守默认）。以下手动
// 实现健全，依据其不变量：
//  - **构建后冻结**：`from_builder` 返回后，arena 再不 alloc / reset / 移动内容（不变量 2），
//    AST 节点是纯数据、无内部可变性（无 Cell/RefCell）。故对已分配内存的访问纯为只读。
//  - **只读跨线程**：对外仅经 `&self` 的 `with_ast` / `structure_hash` 只读访问，绝不并发写。
//    多线程共享 `&ModuleAst` 做只读遍历不触碰 `Bump` 的分配游标（那只在 alloc 时改，我们不 alloc），
//    因此无数据竞争 → `Sync` 健全；把整个持有者移动到另一线程（arena 可在任意线程 drop）→ `Send` 健全。
//  - **单次析构**：`Arc<ModuleAst>` 引用计数归零时在单线程 drop 一次（不变量 2 的 drop 顺序仍成立）。
// 这正是 DESIGN §10.4 所述「放进引擎 `Arc<_>` cell，下游任务借用读取」所需——parse 任务在某 worker
// 线程产出 `ModuleAst`，codegen 等下游任务在（可能不同的）worker 线程只读借用。跨线程只读共享经
// `tests::shared_read_across_threads` 在 miri 下验证无 UB / 无数据竞争。
unsafe impl Send for ModuleAst {}
unsafe impl Sync for ModuleAst {}

// 让 `ModuleAst` 可直接作 wake_turbo 任务输出（`TaskOutput` 对 `T: Hash` 有 blanket 实现）。
// 指纹用构建时算好的结构 hash（不含指针地址，跨实例/重启稳定，不变量 4）。
impl std::hash::Hash for ModuleAst {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        state.write_u64(self.structure_hash);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Expression, ExpressionStatement, Ident, ObjectExpression, ObjectMember, ObjectProperty,
        Pattern, PropertyKey, PropertyKind, SourceType, Statement,
    };

    #[test]
    fn build_and_borrow() {
        let interner = Interner::new();
        let ast = ModuleAst::build_sample(&interner, 4);
        assert_eq!(ast.statement_count(), 1);
        // 经 with_ast 读内部结构：let sum = 0 + 1 + 2 + 3 + 4;
        let (nums, binaries) = ast.with_ast(|p| {
            let mut n = 0;
            let mut b = 0;
            if let Statement::VariableDeclaration(decl) = &p.body[0] {
                let d = &decl.declarations[0];
                assert!(matches!(d.id, Pattern::Ident(_)));
                if let Some(init) = &d.init {
                    count(init, &mut n, &mut b);
                }
            }
            (n, b)
        });
        assert_eq!(nums, 5); // 0..=4
        assert_eq!(binaries, 4);
    }

    fn count(e: &Expression<'_>, nums: &mut u32, bins: &mut u32) {
        match e {
            Expression::NumberLiteral(_) => *nums += 1,
            Expression::Binary(b) => {
                *bins += 1;
                count(&b.left, nums, bins);
                count(&b.right, nums, bins);
            }
            _ => {}
        }
    }

    #[test]
    fn structure_hash_is_stable_and_discriminating() {
        let interner = Interner::new();
        let a = ModuleAst::build_sample(&interner, 6);
        let b = ModuleAst::build_sample(&interner, 6);
        let c = ModuleAst::build_sample(&interner, 7);
        // 相同结构 → 相同指纹（且不含指针地址，故可跨实例相等）。
        assert_eq!(a.structure_hash(), b.structure_hash());
        assert_ne!(a.structure_hash(), c.structure_hash());
    }

    #[test]
    fn structure_hash_includes_for_of_helper_metadata() {
        fn build(interner: &Interner, with_helper: bool) -> ModuleAst {
            let helper = interner.intern("__wake_for_of");
            ModuleAst::from_builder(move |arena| {
                let mut program = Program::new_in(arena, SourceType::Module);
                if with_helper {
                    program.for_of_helper = Some(helper);
                }
                program
            })
        }

        let interner = Interner::new();
        let plain = build(&interner, false);
        let with_helper = build(&interner, true);
        assert_ne!(plain.structure_hash(), with_helper.structure_hash());
        plain.with_ast(|program| assert!(program.for_of_helper.is_none()));
        with_helper.with_ast(|program| {
            assert_eq!(
                program.for_of_helper,
                Some(interner.intern("__wake_for_of"))
            )
        });
    }

    #[test]
    fn structure_hash_includes_object_prototype_setter_semantics() {
        fn build(interner: &Interner, prototype_setter: bool) -> ModuleAst {
            let proto = interner.intern("__proto__");
            let base = interner.intern("base");
            ModuleAst::from_builder(move |arena| {
                let span = wake_common::Span::new(0, 17);
                let property = arena.alloc(ObjectProperty {
                    span,
                    key: PropertyKey::Ident(Ident::new(span, proto)),
                    value: Expression::Identifier(arena.alloc(Ident::new(span, base))),
                    kind: PropertyKind::Init,
                    method: false,
                    shorthand: false,
                    computed: false,
                    prototype_setter,
                });
                let mut properties = crate::AVec::new_in(arena);
                properties.push(ObjectMember::Property(property));
                let object = Expression::Object(arena.alloc(ObjectExpression { span, properties }));
                let mut program = Program::new_in(arena, SourceType::Module);
                program
                    .body
                    .push(Statement::Expression(arena.alloc(ExpressionStatement {
                        span,
                        expression: object,
                    })));
                program
            })
        }

        let interner = Interner::new();
        let ordinary = build(&interner, false);
        let prototype = build(&interner, true);
        assert_ne!(ordinary.structure_hash(), prototype.structure_hash());
    }

    #[test]
    fn many_holders_drop_cleanly() {
        // 制造大量持有者并 drop——miri 下验证无 UB / 无泄漏访问。
        let interner = Interner::new();
        let holders: Vec<ModuleAst> = (0..50)
            .map(|d| ModuleAst::build_sample(&interner, d))
            .collect();
        let total: usize = holders.iter().map(|h| h.statement_count()).sum();
        assert_eq!(total, 50);
        // holders 在此处 drop：program 先（无副作用），arena 后（释放）。
    }

    #[test]
    fn shared_read_across_threads() {
        // 验证 `unsafe impl Send + Sync`：多线程共享 &ModuleAst 做只读遍历无数据竞争。
        // miri 下（`cargo miri test`）会检查跨线程内存访问的 UB / 数据竞争。
        use std::sync::Arc;
        use std::thread;

        let interner = Interner::new();
        let ast = Arc::new(ModuleAst::build_sample(&interner, 5));
        let handles: Vec<_> = (0..3)
            .map(|_| {
                let ast = Arc::clone(&ast);
                thread::spawn(move || {
                    // 只读访问：结构指纹 + 遍历 AST。
                    let h = ast.structure_hash();
                    let n = ast.with_ast(|p| p.body.len());
                    (h, n)
                })
            })
            .collect();
        for handle in handles {
            let (h, n) = handle.join().unwrap();
            assert_eq!(h, ast.structure_hash());
            assert_eq!(n, 1);
        }
    }

    #[test]
    fn with_ast_cannot_leak_reference() {
        // 编译期性质：下述闭包若尝试把 &Program 带出会无法通过 for<'a> 约束。
        // 这里只做正常用法的运行时验证。
        let interner = Interner::new();
        let ast = ModuleAst::build_sample(&interner, 2);
        let span = ast.with_ast(|p| p.span); // Span 是 Copy、不借 arena → 可带出
        assert!(!span.is_dummy());
    }
}
