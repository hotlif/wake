//! wake_ecma_transform — 转换管线的稳定边界。
//!
//! 当前 TS/JSX lowering 仍由 parser/codegen 承担，本 crate 暂不暴露虚假的转换 API。
//! 是否迁移为独立 pass pipeline，按 `docs/ROADMAP.md` 的阶段边界另行决策。

#[cfg(test)]
mod tests {
    #[test]
    fn crate_compiles() {
        // 保证保留的架构边界可独立编译。
    }
}
