//! Single-module compiler throughput benchmark. CI may compile this target as a smoke check;
//! local performance work can run it with `cargo bench -p wake_compiler --bench transpile`.

use std::fmt::Write as _;
use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use wake_compiler::{
    AutomaticJsxOptions, Language, SourceText, TranspileOptions, transpile_module,
};

fn representative_tsx_module(component_count: usize) -> String {
    let mut source = String::with_capacity(component_count * 160);
    source.push_str("import type { MouseEvent } from './events.js';\n");
    for index in 0..component_count {
        writeln!(
            source,
            "export const View{index} = (props: {{ label: string; active?: boolean }}) => <section data-active={{props.active}}><strong>{{props.label}}</strong><span>{{{index}}}</span></section>;"
        )
        .expect("writing to String cannot fail");
    }
    source
}

fn bench_transpile(c: &mut Criterion) {
    let source = representative_tsx_module(256);
    let options =
        TranspileOptions::new(Language::TypeScript).with_jsx(AutomaticJsxOptions::production());
    let mut group = c.benchmark_group("compiler");
    group.throughput(Throughput::Bytes(source.len() as u64));
    group.bench_function("transpile_tsx_module", |bencher| {
        bencher.iter(|| {
            let output = transpile_module(
                SourceText::new("src/benchmark.tsx", black_box(&source)),
                black_box(&options),
            )
            .expect("benchmark fixture must transpile");
            black_box(output.code().len())
        });
    });
    group.finish();
}

criterion_group!(benches, bench_transpile);
criterion_main!(benches);
