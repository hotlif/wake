//! Native lint throughput baseline.
//!
//! Run with `cargo bench -p wake_lint_core --bench lint`. The benchmark is
//! intentionally source based so it covers parser-owned facts and all enabled
//! recommended rules without depending on a project filesystem or TypeScript.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use wake_ecma_ast::SourceType;
use wake_lint_core::{LintOptions, lint_text};

const SNIPPET: &str = r#"
import { useEffect, useMemo, useState } from "react";

export function useResource(id, options = {}) {
  const [state, setState] = useState({ loading: true, data: null });
  const config = useMemo(() => ({ ...options, id }), [id, options]);
  useEffect(() => {
    let cancelled = false;
    fetchData(config).then((data) => {
      if (!cancelled) setState({ loading: false, data });
    });
    return () => { cancelled = true; };
  }, [config]);
  return { ...state, retry: () => setState((value) => ({ ...value, loading: true })) };
}

const value = condition ? `${name}:${count}` : "empty";
if (value == null) debugger;
"#;

fn make_source(target_bytes: usize) -> String {
    let mut source = String::with_capacity(target_bytes + SNIPPET.len());
    while source.len() < target_bytes {
        source.push_str(SNIPPET);
    }
    source
}

fn bench_lint(c: &mut Criterion) {
    let mut group = c.benchmark_group("lint");
    for target_bytes in [16 * 1024, 128 * 1024] {
        let source = make_source(target_bytes);
        group.throughput(Throughput::Bytes(source.len() as u64));
        group.bench_with_input(
            BenchmarkId::new("lint_text", target_bytes),
            &source,
            |b, source| {
                let options = LintOptions::default();
                b.iter(|| {
                    let result = lint_text(black_box(source), SourceType::Module, &options)
                        .expect("benchmark source must parse");
                    black_box(result.diagnostics.len())
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, bench_lint);
criterion_main!(benches);
