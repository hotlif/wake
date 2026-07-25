//! AST 结构指纹：遍历折叠节点判别式 + 叶子值（DESIGN §10.4——指纹用结构而非指针地址）。

use std::hash::{Hash, Hasher};
use std::mem::Discriminant;

use wake_common::Hash64;

use crate::expr::Expression;
use crate::stmt::Statement;
use crate::visit::{Visit, walk_expression, walk_statement};
use crate::{Ident, Program};

/// 计算一个 Program 的结构指纹。同结构 → 同值；跨实例/重启稳定（不含指针）。
pub fn structure_hash(program: &Program) -> Hash64 {
    let mut fold = HashFold { state: FNV_OFFSET };
    fold.visit_program(program);
    fold.state
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

struct HashFold {
    state: u64,
}

impl HashFold {
    /// 单字 FNV-1a 混合 + 一次 xorshift 收尾：整数叶值一次乘加，取代原逐字节 8 次乘。
    #[inline]
    fn mix_u64(&mut self, v: u64) {
        self.state = (self.state ^ v).wrapping_mul(FNV_PRIME);
        self.state ^= self.state >> 29;
    }

    /// 折叠节点判别式：直接把 `Discriminant` 折进本 `Hasher`，省去每节点新建
    /// `DefaultHasher`（SipHash-1-3 的建栈 + compress + finalize）。判别式值编译期稳定、
    /// 折叠常量固定 → 同二进制内确定。此指纹只喂引擎的内存内早期截断（同进程内比较），
    /// 从不落盘（`ModuleAst` 不序列化，持久缓存另用源文本 hash），故改算法无需 bump 缓存版本。
    #[inline]
    fn mix_disc<T>(&mut self, d: Discriminant<T>) {
        d.hash(self);
    }
}

impl Hasher for HashFold {
    #[inline]
    fn finish(&self) -> u64 {
        self.state
    }

    /// 判别式（及任何整数写入）经此按 8 字节块单字混合，尾部零填充。
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.mix_u64(u64::from_le_bytes(c.try_into().unwrap()));
        }
        let rem = chunks.remainder();
        if !rem.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rem.len()].copy_from_slice(rem);
            self.mix_u64(u64::from_le_bytes(buf));
        }
    }
}

impl<'a> Visit<'a> for HashFold {
    fn visit_statement(&mut self, node: &Statement<'a>) {
        self.mix_disc(std::mem::discriminant(node));
        walk_statement(self, node);
    }

    fn visit_expression(&mut self, node: &Expression<'a>) {
        self.mix_disc(std::mem::discriminant(node));
        // 叶子字面量的值必须全部参与指纹：codegen 直接发射这些值，指纹遗漏会导致
        // 早期截断误判「输出未变」→ 只改字面量时下游 codegen 被跳过、产出陈旧（真实 bug）。
        match node {
            Expression::NumberLiteral(n) => self.mix_u64(n.value.to_bits()),
            Expression::StringLiteral(s) => self.mix_u64(s.value.as_u32() as u64),
            Expression::BooleanLiteral(b) => self.mix_u64(b.value as u64),
            Expression::BigIntLiteral(b) => self.mix_u64(b.raw.as_u32() as u64),
            Expression::RegExpLiteral(r) => {
                self.mix_u64(r.pattern.as_u32() as u64);
                self.mix_u64(r.flags.as_u32() as u64);
            }
            Expression::TemplateLiteral(t) => {
                for q in t.quasis.iter() {
                    self.mix_u64(q.raw.as_u32() as u64);
                }
            }
            _ => {}
        }
        walk_expression(self, node);
    }

    fn visit_ident(&mut self, node: &Ident) {
        self.mix_u64(node.name.as_u32() as u64);
    }
}
