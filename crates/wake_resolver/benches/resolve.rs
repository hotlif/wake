//! resolver 真实文件系统吞吐基准（criterion，`OsFileSystem`）。
//!
//! 运行：`cargo bench -p wake_resolver`
//!
//! 现有 bundler bench 用 `MemoryFileSystem`（内存 HashMap，stat ≈ 0），**测不出解析的真实
//! syscall 成本**。本 bench 在临时目录建一棵合成的 `node_modules` 树，用 `OsFileSystem` 度量：
//!
//! - `cold`：每批全新 `Resolver`（空缓存）→ `is_file`/`is_dir`/`read_to_string` 真实 syscall。
//!   这是 tier-3「resolver 变 `Sync` + 在 `par_request` 内并行 stat」优化的**目标基线**；
//!   Windows 上还含 NTFS + Defender 的 per-stat 成本。
//! - `warm_cached`：预热 `Resolver`（全命中）→ 度量每次 `resolve` 的 `(PathBuf, String)` key
//!   克隆开销（tier-3「raw_entry 借用键探测」优化的目标）。
//!
//! 树在临时目录合成 → 自包含、可复现，不依赖 `fixtures/` 的 `node_modules`（未纳入版本控制）。

use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use wake_common::fs::OsFileSystem;
use wake_resolver::Resolver;

/// 临时目录 RAII：Drop 时递归删除（bench 正常结束或 panic 都清理）。
struct TempTree {
    root: PathBuf,
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// 建一棵合成项目树：深层 `src` 目录（考验 `node_modules` 逐级向上查）+ ~30 个包
/// （含 react/react-dom/lodash/@scope 与若干 `pkg_N`），每包 `package.json`(main+module)
/// + `index.js` + `esm/index.js` + `lib/sub.js`。
fn setup_tree() -> TempTree {
    let root = std::env::temp_dir().join(format!("wake_resolve_bench_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);

    // 深层源码目录 + 相对模块（`.js` 导入走 `.ts` 孪生）。
    write_file(&root.join("src/a/b/c/d/entry.ts"), "// entry\n");
    write_file(&root.join("src/a/b/util.ts"), "// util\n");
    write_file(&root.join("src/a/b/c/helper.ts"), "// helper\n");

    let nm = root.join("node_modules");
    // 具名真实包（含 main/module 分歧、subpath）。
    write_file(
        &nm.join("react/package.json"),
        r#"{"name":"react","main":"index.js","module":"esm/react.js"}"#,
    );
    write_file(&nm.join("react/index.js"), "//cjs\n");
    write_file(&nm.join("react/esm/react.js"), "//esm\n");
    write_file(
        &nm.join("react-dom/package.json"),
        r#"{"name":"react-dom","main":"index.js"}"#,
    );
    write_file(&nm.join("react-dom/index.js"), "//dom\n");
    write_file(&nm.join("react-dom/client.js"), "//client\n");
    write_file(
        &nm.join("lodash/package.json"),
        r#"{"name":"lodash","main":"index.js"}"#,
    );
    write_file(&nm.join("lodash/index.js"), "//lodash\n");
    write_file(&nm.join("lodash/debounce.js"), "//debounce\n");
    write_file(
        &nm.join("@scope/pkg/package.json"),
        r#"{"name":"@scope/pkg","module":"lib/index.js"}"#,
    );
    write_file(&nm.join("@scope/pkg/lib/index.js"), "//scoped\n");
    // 一批填充包，让 node_modules 是棵真实规模的树。
    for i in 0..30 {
        write_file(
            &nm.join(format!("pkg_{i}/package.json")),
            &format!(r#"{{"name":"pkg_{i}","main":"index.js","module":"esm/index.js"}}"#),
        );
        write_file(&nm.join(format!("pkg_{i}/index.js")), "//cjs\n");
        write_file(&nm.join(format!("pkg_{i}/esm/index.js")), "//esm\n");
        write_file(&nm.join(format!("pkg_{i}/lib/sub.js")), "//sub\n");
    }
    TempTree { root }
}

/// 从深层目录解析的一组说明符（裸包走向上查 + package.json；相对走扩展名补全 / `.ts` 孪生）。
const SPECIFIERS: &[&str] = &[
    "react",
    "react-dom/client",
    "lodash/debounce",
    "@scope/pkg",
    "pkg_3",
    "pkg_17/lib/sub",
    "pkg_29",
    "../../util.js", // .js → .ts 孪生
    "../helper.js",
    "pkg_8/esm/index",
];

fn bench_resolve(c: &mut Criterion) {
    let tree = setup_tree();
    let from_dir = tree.root.join("src/a/b/c/d");

    let mut group = c.benchmark_group("resolve_os");

    // 冷：每批新 Resolver（空缓存）→ 真实 syscall（这是并行化优化的目标）。
    group.bench_function("cold", |b| {
        b.iter_batched(
            || Resolver::new(Arc::new(OsFileSystem)),
            |r| {
                for spec in SPECIFIERS {
                    black_box(r.resolve(spec, &from_dir).ok());
                }
            },
            BatchSize::SmallInput,
        );
    });

    // 热：预热 Resolver（全命中）→ 度量每次 resolve 的 key 克隆 + 命中路径。
    let warm = Resolver::new(Arc::new(OsFileSystem));
    for spec in SPECIFIERS {
        let _ = warm.resolve(spec, &from_dir);
    }
    group.bench_function("warm_cached", |b| {
        b.iter(|| {
            for spec in SPECIFIERS {
                black_box(warm.resolve(spec, &from_dir).ok());
            }
        });
    });

    group.finish();
    drop(tree);
}

criterion_group!(benches, bench_resolve);
criterion_main!(benches);
