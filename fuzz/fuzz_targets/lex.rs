//! cargo-fuzz 目标：词法分析器不 panic / 不 OOM（PLAN §1.5 / Gate-1a）。
//!
//! 运行：`cargo +nightly fuzz run lex`
//!
//! 校验的不变量与 `tests/fuzz_smoke.rs` 一致：末尾恒为 Eof；所有 span 落在 `[0, len]`、
//! 落在 UTF-8 边界、可安全切片。libfuzzer 会用覆盖率引导生成刁钻输入。

#![no_main]

use libfuzzer_sys::fuzz_target;
use wake_ecma_lexer::tokenize;

fuzz_target!(|data: &[u8]| {
    let Ok(src) = std::str::from_utf8(data) else {
        return;
    };
    let (tokens, _diags) = tokenize(src);

    assert!(tokens.last().map(|t| t.is_eof()).unwrap_or(false), "末尾必须是 Eof");

    let len = src.len() as u32;
    let mut prev_hi = 0u32;
    for t in &tokens {
        assert!(t.span.lo <= t.span.hi);
        assert!(t.span.hi <= len);
        assert!(src.is_char_boundary(t.span.lo as usize));
        assert!(src.is_char_boundary(t.span.hi as usize));
        assert!(t.span.lo >= prev_hi || t.is_eof());
        prev_hi = t.span.hi.max(prev_hi);
        let _ = &src[t.span.lo as usize..t.span.hi as usize];
    }
});
