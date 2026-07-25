//! 随机不 panic 冒烟测试（PLAN §1.5）——常驻 CI 的轻量 fuzz。
//!
//! 完整 fuzz（cargo-fuzz，跑 1h 不 panic/OOM）见 `fuzz/`。此处用确定性 PRNG 生成大量随机输入，
//! 断言：① 永不 panic；② 所有 token span 落在 `[0, len]` 内且单调推进；③ 末尾恒为 `Eof`；
//! ④ 错误恢复——非法输入产出诊断但仍完整扫到 Eof。

use wake_ecma_lexer::{TokenKind, tokenize};

/// 极小确定性 PRNG（xorshift64*）。
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// 偏向 JS 语法的字符集（含易触发边角的字符）。
const CHARSET: &[u8] = b"abcXYZ_$09 \t\n\r/*+-=<>!&|^~?:;,.(){}[]\"'`\\#en.eExXoObB0123456789u{}";

fn random_source(rng: &mut Rng, len: usize) -> String {
    let mut bytes = Vec::with_capacity(len);
    for _ in 0..len {
        let idx = (rng.next() as usize) % CHARSET.len();
        bytes.push(CHARSET[idx]);
    }
    // 保证是合法 UTF-8（CHARSET 全 ASCII）。
    String::from_utf8(bytes).unwrap()
}

fn check_invariants(src: &str) {
    let (tokens, _diags) = tokenize(src);
    let len = src.len() as u32;

    assert!(!tokens.is_empty(), "至少应有 Eof");
    assert!(tokens.last().unwrap().is_eof(), "末尾必须是 Eof");

    let mut prev_hi = 0u32;
    for t in &tokens {
        assert!(t.span.lo <= t.span.hi, "span 反向: {:?}", t.span);
        assert!(t.span.hi <= len, "span 越界: {:?} > {len}", t.span);
        // 除 Eof 外，token 起点不早于上一个终点（允许相等：如零宽 Eof）。
        assert!(
            t.span.lo >= prev_hi || t.is_eof(),
            "span 回退: {:?} < {prev_hi}",
            t.span
        );
        prev_hi = t.span.hi.max(prev_hi);
        // 每个 token 必须落在合法 UTF-8 边界（span 用于切片）。
        assert!(src.is_char_boundary(t.span.lo as usize));
        assert!(src.is_char_boundary(t.span.hi as usize));
        let _ = &src[t.span.lo as usize..t.span.hi as usize];
    }
}

#[test]
fn random_inputs_never_panic() {
    let mut rng = Rng::new(0xDEAD_BEEF_CAFE_1234);
    for _ in 0..20_000 {
        let len = (rng.next() as usize) % 64;
        let src = random_source(&mut rng, len);
        check_invariants(&src);
    }
}

#[test]
fn longer_random_inputs() {
    let mut rng = Rng::new(0x0BAD_F00D_1357_9BDF);
    for _ in 0..500 {
        let len = 200 + (rng.next() as usize) % 2000;
        let src = random_source(&mut rng, len);
        check_invariants(&src);
    }
}

#[test]
fn error_recovery_reaches_eof() {
    // 一批「一定含非法/未闭合结构」的输入，确认恢复后仍完整扫到 Eof。
    let cases = [
        "\"unterminated",
        "`unterminated ${",
        "/* unclosed",
        "0xZZ",
        "1__2",
        "\\q",
        "'\n'",
        "#123",
        "\u{0}\u{1}\u{7}",
        "/regex_no_close",
    ];
    for src in cases {
        let (tokens, _diags) = tokenize(src);
        assert!(tokens.last().unwrap().is_eof(), "{src:?} 未扫到 Eof");
        check_invariants(src);
    }
}

#[test]
fn all_tokens_have_describe() {
    // 确保每个产出的 kind 都有非空描述（tokenize 输出健壮性）。
    let (tokens, _) = tokenize("const x = `a${1}b`; class C { #p = /re/g; } 0n ??= 1;");
    for t in &tokens {
        assert!(!t.kind.describe().is_empty());
    }
    assert!(
        tokens
            .iter()
            .any(|t| matches!(t.kind, TokenKind::TemplateHead))
    );
}
