//! # wake_graph — 模块图分析：符号级 Tree Shaking（DESIGN §5.3）
//!
//! Tree Shaking v1（PLAN §6.6）：**跨模块导出可达性**——从入口出发，计算每个模块「哪些导出
//! 名被别的模块真正 import」，未被任何可达模块使用的导出交给 codegen 移除。
//!
//! 保守而安全的取舍（第一版）：
//! - 入口模块 → **全部导出保留**（它是 bundle 的公共面）；
//! - `import * as ns` / 动态 `import()` / `require()` / `export *` → 目标模块**全部导出视为已用**；
//! - `import { a, b }` → 用 `{a, b}`；`import D` → 用 `default`；`import "x"`（仅副作用）→ 用空集
//!   （模块仍进图运行，只是它的未用导出可被剪）；
//! - `export { a } from 'm'` / re-export → 保守地把 `a` 记为 m 的已用导出（不做跨链传播）。
//!
//! 「移除」的**安全性**由 codegen 侧兜底：只移除「外部未用 + 模块内也未引用 + 无副作用」的
//! 导出声明；否则仅移除 `exports.x = ...` 绑定行（永远安全）。模块的顶层副作用语句一律保留。

use wake_common::{FxHashSet, Interner};
use wake_ecma_ast::{ImportSpecifier, ModuleExportName, Program, Statement};

/// 一个模块对某个 import/export-from 说明符的使用方式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportUse {
    /// 具名使用（`default` 以字符串 `"default"` 表示）。
    Names(Vec<String>),
    /// 整体使用（namespace / 动态 import / require）——不可 shake。
    All,
    /// `export * from 'm'` re-export 全量——仅当下游消费本模块导出时才传播至目标。
    ReexportAll,
}

/// 一个模块的已用导出集合。
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Used {
    /// 尚无已知使用（初值）。
    #[default]
    None,
    /// 仅这些具名导出被使用。
    Names(FxHashSet<String>),
    /// 全部导出被使用——不 shake。
    All,
}

impl Used {
    /// 合并一条使用记录。`All` 吸收一切。
    pub fn merge(&mut self, u: &ImportUse) {
        match (&mut *self, u) {
            (Used::All, _) => {}
            (_, ImportUse::All | ImportUse::ReexportAll) => *self = Used::All,
            (Used::None, ImportUse::Names(ns)) => {
                *self = Used::Names(ns.iter().cloned().collect());
            }
            (Used::Names(set), ImportUse::Names(ns)) => {
                set.extend(ns.iter().cloned());
            }
        }
    }

    /// 转成传给 codegen 的「保留导出名」列表：
    /// - `All` → `None`（不 shake，全保留）；
    /// - `Names`/`None` → `Some(已排序去重名单)`（只保留这些）。
    pub fn to_keep_list(&self) -> Option<Vec<String>> {
        match self {
            Used::All => None,
            Used::None => Some(Vec::new()),
            Used::Names(set) => {
                let mut v: Vec<String> = set.iter().cloned().collect();
                v.sort_unstable();
                Some(v)
            }
        }
    }
}

/// 从一个模块的 AST 提取它对每个**静态** import / export-from 说明符的使用。
///
/// 动态 `import()` / `require()` 不在此处（它们是表达式，且说明符经 [`Used::All`] 处理更简单）——
/// 调用方据依赖种类（`DynamicImport`/`Require`）直接把目标标 `All`。
pub fn collect_static_uses(program: &Program, interner: &Interner) -> Vec<(String, ImportUse)> {
    let mut out: Vec<(String, ImportUse)> = Vec::new();
    for stmt in program.body.iter() {
        match stmt {
            Statement::Import(d) => {
                let source = interner.resolve(d.source);
                let mut names = Vec::new();
                let mut all = false;
                for spec in d.specifiers.iter() {
                    match spec {
                        ImportSpecifier::Default { .. } => names.push("default".to_string()),
                        ImportSpecifier::Namespace { .. } => all = true,
                        ImportSpecifier::Named { imported, .. } => {
                            names.push(export_name_string(imported, interner));
                        }
                    }
                }
                out.push((
                    source,
                    if all {
                        ImportUse::All
                    } else {
                        ImportUse::Names(names)
                    },
                ));
            }
            Statement::ExportNamed(s) => {
                if let Some(src) = s.source {
                    let source = interner.resolve(src);
                    let names = s
                        .specifiers
                        .iter()
                        .map(|sp| export_name_string(&sp.local, interner))
                        .collect();
                    out.push((source, ImportUse::Names(names)));
                }
            }
            Statement::ExportAll(s) => {
                out.push((interner.resolve(s.source), ImportUse::ReexportAll));
            }
            _ => {}
        }
    }
    out
}

fn export_name_string(n: &ModuleExportName, interner: &Interner) -> String {
    match n {
        ModuleExportName::Ident(id) => interner.resolve(id.name),
        ModuleExportName::String(a) => interner.resolve(*a),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn used_merge_all_absorbs() {
        let mut u = Used::Names(["a".to_string()].into_iter().collect());
        u.merge(&ImportUse::All);
        assert!(matches!(u, Used::All));
        // All 之后再并具名仍是 All。
        u.merge(&ImportUse::Names(vec!["b".into()]));
        assert!(matches!(u, Used::All));
    }

    #[test]
    fn used_merge_names_union() {
        let mut u = Used::None;
        u.merge(&ImportUse::Names(vec!["a".into(), "b".into()]));
        u.merge(&ImportUse::Names(vec!["b".into(), "c".into()]));
        let keep = u.to_keep_list().unwrap();
        assert_eq!(keep, vec!["a", "b", "c"]);
    }

    #[test]
    fn to_keep_list_all_is_none() {
        assert!(Used::All.to_keep_list().is_none());
        assert_eq!(Used::None.to_keep_list(), Some(vec![]));
    }
}
