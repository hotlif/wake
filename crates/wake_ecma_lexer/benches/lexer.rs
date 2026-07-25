//! 词法吞吐基准（criterion，MB/s）——PLAN §1.6 / Gate-1a 底线 ≥150MB/s（SWAR 层）。
//!
//! 运行：`cargo bench -p wake_ecma_lexer`
//! 双值口径（PLAN §1）：底线 150MB/s（本层）/ 目标 400MB/s（P7 显式 SIMD 回补）。

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use wake_ecma_lexer::{Lexer, tokenize};

/// 一段代表性的 JS/TS 源码（标识符、运算符、字符串、模板、注释、数字混合）。
const SNIPPET: &str = r#"
// module: a representative slice of real-world code
import { useState, useEffect } from "react";
const API_BASE = `https://api.example.com/v${2}`;
export function useCounter(initial = 0) {
  const [count, setCount] = useState(initial);
  useEffect(() => {
    const id = setInterval(() => setCount((c) => c + 1), 1000);
    return () => clearInterval(id);
  }, []);
  const isEven = count % 2 === 0 && count > 0;
  const label = isEven ? `even:${count}` : "odd_" + count.toString(16);
  return { count, setCount, isEven, label, big: 9007199254740993n, ratio: 3.14159e-2 };
}
class Widget {
  #id = 0xDEAD_BEEF;
  constructor(name) { this.name = name; this.#id += 1; }
  render() { return `<div class="w">${this.name}</div>`; }
}
"#;

fn make_source(target_bytes: usize) -> String {
    let mut s = String::with_capacity(target_bytes + SNIPPET.len());
    while s.len() < target_bytes {
        s.push_str(SNIPPET);
    }
    s
}

fn bench_lexer(c: &mut Criterion) {
    let source = make_source(256 * 1024); // ~256 KB
    let bytes = source.len() as u64;

    let mut group = c.benchmark_group("lexer");
    group.throughput(Throughput::Bytes(bytes));

    // 完整 tokenize（含 Vec 收集 + regex/div 启发式），贴近真实使用。
    group.bench_function("tokenize", |b| {
        b.iter(|| {
            let (toks, _diags) = tokenize(black_box(&source));
            black_box(toks.len())
        });
    });

    // 纯扫描（不收集 Vec），度量词法核心吞吐。
    group.bench_function("scan_only", |b| {
        b.iter(|| {
            let mut lex = Lexer::new(black_box(&source));
            let mut n = 0u64;
            loop {
                let t = lex.next(false);
                if t.is_eof() {
                    break;
                }
                n += 1;
            }
            black_box(n)
        });
    });

    group.finish();
}

criterion_group!(benches, bench_lexer);
criterion_main!(benches);
