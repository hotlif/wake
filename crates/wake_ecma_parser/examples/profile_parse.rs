//! 临时性能剖析：读一个真实文件，分别测 tokenize（纯词法）与 parse（词法+建 AST）吞吐。
//! 用法：`cargo run --release --example profile_parse -p wake_ecma_parser -- <file> [iters]`
use std::time::Instant;

use wake_common::Interner;
use wake_ecma_ast::SourceType;

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("需要文件路径");
    let iters: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    let src = std::fs::read_to_string(&path).expect("读文件失败");
    let mb = src.len() as f64 / (1024.0 * 1024.0);

    // 预热
    for _ in 0..3 {
        let (t, _) = wake_ecma_lexer::tokenize(&src);
        std::hint::black_box(t.len());
        let i = Interner::new();
        std::hint::black_box(
            wake_ecma_parser::parse(&src, &i, SourceType::Module)
                .dependencies
                .len(),
        );
    }

    let t0 = Instant::now();
    for _ in 0..iters {
        let (t, _) = wake_ecma_lexer::tokenize(&src);
        std::hint::black_box(t.len());
    }
    let lex = t0.elapsed() / iters;

    // scan_only：不收集 Vec，纯词法核心（≈ parser 驱动 lexer 的成本）。
    let ts = Instant::now();
    for _ in 0..iters {
        let mut lx = wake_ecma_lexer::Lexer::new(&src);
        let mut n = 0u64;
        loop {
            let t = lx.next(false);
            if t.is_eof() {
                break;
            }
            n += 1;
        }
        std::hint::black_box(n);
    }
    let scan = ts.elapsed() / iters;

    let t1 = Instant::now();
    for _ in 0..iters {
        let i = Interner::new();
        let out = wake_ecma_parser::parse(&src, &i, SourceType::Module);
        std::hint::black_box(out.dependencies.len());
    }
    let parse = t1.elapsed() / iters;

    println!("{path}  {:.2} MB", mb);
    println!(
        "  scan_only     {:>7.2?}  = {:>6.1} MB/s  (无 Vec，parser 实付)",
        scan,
        mb / scan.as_secs_f64()
    );
    println!(
        "  tokenize      {:>7.2?}  = {:>6.1} MB/s  (含 Vec 收集)",
        lex,
        mb / lex.as_secs_f64()
    );
    println!(
        "  parse(全)     {:>7.2?}  = {:>6.1} MB/s",
        parse,
        mb / parse.as_secs_f64()
    );
    println!(
        "  建 AST 净额    {:>7.2?}  ({:.0}% 的 parse 时间)",
        parse.saturating_sub(lex),
        100.0 * (parse.as_secs_f64() - lex.as_secs_f64()) / parse.as_secs_f64()
    );

    // token 直方图：按 kind 聚合 数量 + 总字节，找出 lexer 时间去向。
    use std::collections::HashMap;
    let (toks, _) = wake_ecma_lexer::tokenize(&src);
    let mut hist: HashMap<String, (u64, u64)> = HashMap::new();
    for t in &toks {
        let len = (t.span.hi - t.span.lo) as u64;
        let e = hist.entry(format!("{:?}", t.kind)).or_default();
        e.0 += 1;
        e.1 += len;
    }
    let mut rows: Vec<_> = hist.into_iter().collect();
    rows.sort_by_key(|(_, (_, bytes))| std::cmp::Reverse(*bytes));
    println!("  —— token 直方图（按总字节降序，前 10）——");
    for (kind, (count, bytes)) in rows.into_iter().take(10) {
        println!(
            "    {:<16} 数量 {:>7}  字节 {:>8} ({:>4.1}%)",
            kind,
            count,
            bytes,
            100.0 * bytes as f64 / src.len() as f64
        );
    }
}
