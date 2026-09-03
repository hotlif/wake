//! AST 结构指纹：遍历折叠节点判别式 + 叶子值（DESIGN §10.4——指纹用结构而非指针地址）。

use std::hash::{Hash, Hasher};
use std::mem::Discriminant;

use wake_common::Hash64;

use crate::expr::Expression;
use crate::module::{ImportAttributes, ModuleExportName};
use crate::stmt::Statement;
use crate::visit::{Visit, walk_expression, walk_statement};
use crate::{Ident, ObjectMember, Program, PropertyKey};

/// 计算一个 Program 的结构指纹。同结构 → 同值；跨实例/重启稳定（不含指针）。
pub fn structure_hash(program: &Program) -> Hash64 {
    let mut fold = HashFold { state: FNV_OFFSET };
    fold.mix_u64(
        program
            .spread_helper
            .map_or(u64::MAX, |atom| u64::from(atom.as_u32())),
    );
    fold.mix_u64(
        program
            .object_spread_helper
            .map_or(u64::MAX - 1, |atom| u64::from(atom.as_u32())),
    );
    fold.mix_u64(
        program
            .for_of_helper
            .map_or(u64::MAX - 2, |atom| u64::from(atom.as_u32())),
    );
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
        let (chunks, rem) = bytes.as_chunks::<8>();
        for chunk in chunks {
            self.mix_u64(u64::from_le_bytes(*chunk));
        }
        if !rem.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rem.len()].copy_from_slice(rem);
            self.mix_u64(u64::from_le_bytes(buf));
        }
    }
}

impl HashFold {
    /// 折叠 `with { .. }` / `assert { .. }` 子句。
    ///
    /// [`walk_statement`] 刻意不下钻引入属性（其中的 `Ident` 是属性名、不是引用，下钻会让
    /// mangler 把它误当自由变量）。故指纹须在此显式混入——否则只改 `with { type: "json" }`
    /// 的编辑会得到逐位相同的指纹，引擎早期截断误判「输出未变」→ 产出陈旧包。
    fn mix_attributes(&mut self, attrs: Option<&ImportAttributes>) {
        let Some(a) = attrs else { return };
        self.mix_disc(std::mem::discriminant(&a.keyword));
        for item in a.items {
            match item.key {
                ModuleExportName::Ident(id) => self.mix_u64(id.name.as_u32() as u64),
                ModuleExportName::String(s) => self.mix_u64(s.as_u32() as u64),
            }
            self.mix_u64(item.value.as_u32() as u64);
        }
    }
}

impl<'a> Visit<'a> for HashFold {
    fn visit_statement(&mut self, node: &Statement<'a>) {
        self.mix_disc(std::mem::discriminant(node));
        // 语句判别式之外、codegen 会直接发射的**非子节点**信息，必须一并进指纹（同
        // `visit_expression` 中字面量值的理由）：`var`↔`using` 只差 kind，引入属性只挂在语句上。
        match node {
            Statement::VariableDeclaration(d) => self.mix_disc(std::mem::discriminant(&d.kind)),
            Statement::Import(d) => self.mix_attributes(d.attributes),
            Statement::ExportNamed(d) => self.mix_attributes(d.attributes),
            Statement::ExportAll(d) => self.mix_attributes(d.attributes),
            _ => {}
        }
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
            Expression::Object(object) => {
                for member in object.properties.iter() {
                    self.mix_disc(std::mem::discriminant(member));
                    if let ObjectMember::Property(property) = member {
                        self.mix_disc(std::mem::discriminant(&property.kind));
                        self.mix_u64(
                            u64::from(property.method)
                                | (u64::from(property.shorthand) << 1)
                                | (u64::from(property.computed) << 2)
                                | (u64::from(property.prototype_setter) << 3),
                        );
                        self.mix_disc(std::mem::discriminant(&property.key));
                        match property.key {
                            PropertyKey::Ident(ident) | PropertyKey::Private(ident) => {
                                self.mix_u64(ident.name.as_u32() as u64);
                            }
                            PropertyKey::String(string) => {
                                self.mix_u64(string.value.as_u32() as u64);
                            }
                            PropertyKey::Number(number) => {
                                self.mix_u64(number.value.to_bits());
                            }
                            PropertyKey::Computed(_) => {}
                        }
                    }
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
