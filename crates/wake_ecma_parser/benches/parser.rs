//! 解析吞吐基准（criterion，MB/s）——PLAN §4 Gate-1 底线 ≥80MB/s（目标 ≥150MB/s）。
//!
//! 运行：`cargo bench -p wake_ecma_parser`

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use wake_common::Interner;
use wake_ecma_ast::SourceType;
use wake_ecma_parser::parse;

/// 一段代表性的真实风格 JS/TS 模块（import/hooks/类/箭头/模板/解构/可选链混合）。
const SNIPPET: &str = r#"
import { useState, useEffect, useMemo } from "react";
import { fetchData, postData } from "./api.js";

export function useResource(id, options = {}) {
  const [state, setState] = useState({ loading: true, data: null, error: null });
  const config = useMemo(() => ({ ...options, id, ts: Date.now }), [id, options]);
  useEffect(() => {
    let cancelled = false;
    fetchData(config).then((data) => {
      if (!cancelled) setState({ loading: false, data, error: null });
    }).catch((error) => {
      if (!cancelled) setState({ loading: false, data: null, error });
    });
    return () => { cancelled = true; };
  }, [config]);
  const label = state.loading ? "loading..." : `loaded ${state.data?.length ?? 0} items`;
  return { ...state, label, retry: () => setState((s) => ({ ...s, loading: true })) };
}

export class Store {
  #items = new Map();
  static instances = 0;
  constructor(initial = []) {
    for (const [k, v] of initial) this.#items.set(k, v);
    Store.instances += 1;
  }
  get(key) { return this.#items.get(key) ?? null; }
  set(key, value) { this.#items.set(key, value); return this; }
  get size() { return this.#items.size; }
}

const handlers = {
  add: (a, b) => a + b,
  sub(a, b) { return a - b; },
  async load(url) { const m = await import(url); return m.default; },
};
export default handlers;
"#;

fn make_source(target_bytes: usize) -> String {
    let mut s = String::with_capacity(target_bytes + SNIPPET.len());
    while s.len() < target_bytes {
        s.push_str(SNIPPET);
    }
    s
}

fn bench_parser(c: &mut Criterion) {
    let source = make_source(256 * 1024); // ~256 KB
    let mut group = c.benchmark_group("parser");
    group.throughput(Throughput::Bytes(source.len() as u64));
    group.bench_function("parse_module", |b| {
        b.iter(|| {
            let interner = Interner::new();
            let out = parse(black_box(&source), &interner, SourceType::Module);
            black_box(out.dependencies.len())
        });
    });
    group.finish();
}

criterion_group!(benches, bench_parser);
criterion_main!(benches);
