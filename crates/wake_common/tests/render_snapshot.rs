//! 诊断渲染的快照测试（insta）——0.4 测试基建的样例：编译器/诊断输出用 snapshot 验收。
//!
//! 更新快照：`cargo insta review`（或 `INSTA_UPDATE=always cargo test -p wake_common`）。

use wake_common::{Diagnostic, RenderStyle, SourceFile, Span, render};

#[test]
fn snapshot_error_with_source_context() {
    let src = "let a = 1;\nlet b = 2\nfoo(bar\n";
    let sf = SourceFile::new("a.js", src);
    let bar = src.find("bar").unwrap() as u32;
    let diag = Diagnostic::error("unexpected token, expected `)`")
        .with_code("WAKE0001")
        .with_primary(Span::new(bar, bar + 3), "expected `)` before this")
        .with_note("每条诊断都带源码上下文（对标 rustc）");

    // 快照 plain 形态（无 ANSI），稳定可读。
    insta::assert_snapshot!(render(&diag, &sf, RenderStyle::plain()));
}

#[test]
fn snapshot_warning_multi_label() {
    let src = "import { x } from './a';\nconst y = x + z;\n";
    let sf = SourceFile::new("mod.js", src);
    let z = src.find("z;").unwrap() as u32;
    let x = src.rfind("x +").unwrap() as u32;
    let diag = Diagnostic::warning("`z` 未定义")
        .with_code("WAKE0100")
        .with_primary(Span::new(z, z + 1), "此处使用了未声明的 `z`")
        .with_secondary(Span::new(x, x + 1), "`x` 在此导入");

    insta::assert_snapshot!(render(&diag, &sf, RenderStyle::plain()));
}
