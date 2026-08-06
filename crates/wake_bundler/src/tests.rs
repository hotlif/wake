//! 打包器测试：结构断言 + node 端到端执行（3.6）。

use std::path::Path;
use std::sync::Arc;

use wake_common::{MemoryFileSystem, OsFileSystem};

use crate::{Bundler, IncrementalBundler};

/// 一个多模块 ESM fixture：index 依赖 math + msg。
fn fixture() -> MemoryFileSystem {
    MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import { add } from './math.js';\n\
             import msg from './msg.js';\n\
             export const result = add(2, 3) + msg.length;",
        ),
        ("src/math.js", "export function add(a, b) { return a + b; }"),
        ("src/msg.js", "export default 'hello';"),
    ])
}

#[test]
fn bundles_multi_module_esm() {
    let out = Bundler::new(Arc::new(fixture())).build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(out.module_count, 3);

    // runtime + 函数包装 + ESM→CJS 改写落地。
    assert!(out.bundle.contains("__wake_require__"));
    assert!(
        out.bundle
            .contains("function(module, exports, __wake_require__)")
    );
    assert!(out.bundle.contains("exports[\"add\"] = function (a, b)"));
    assert!(out.bundle.contains("exports.default = \"hello\";"));
    assert!(out.bundle.contains("exports[\"result\"] = result;"));
}

// ============================================================
// 增量打包（引擎接入，PLAN §3.2）
// ============================================================

#[test]
fn incremental_bundles_correctly() {
    let mut bundler = IncrementalBundler::new(Arc::new(fixture()));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(out.module_count, 3);
    // 与直接打包同样的 runtime + 函数包装 + ESM→CJS 改写。
    assert!(out.bundle.contains("__wake_require__"));
    assert!(out.bundle.contains("exports[\"add\"] = function (a, b)"));
    assert!(out.bundle.contains("exports.default = \"hello\";"));
    assert!(out.bundle.contains("exports[\"result\"] = result;"));
}

#[test]
fn direct_and_incremental_default_paths_are_equivalent() {
    let direct = Bundler::new(Arc::new(fixture())).build(Path::new("src/index.js"));
    let mut incremental = IncrementalBundler::new(Arc::new(fixture()));
    let incremental = incremental.build(Path::new("src/index.js"));

    assert!(!direct.has_errors(), "{:?}", direct.diagnostics);
    assert!(!incremental.has_errors(), "{:?}", incremental.diagnostics);
    assert_eq!(direct.module_count, incremental.module_count);
    assert_eq!(
        direct.bundle, incremental.bundle,
        "默认构建的两条编排路径必须产生相同字节"
    );
}

#[test]
fn minify_uses_dot_access_for_identifier_export_names() {
    let fs = MemoryFileSystem::from_files([(
        "src/index.js",
        "const value = 1; export { value as answer };",
    )]);
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    bundler.enable_minify().enable_mangle();
    let output = bundler.build(Path::new("src/index.js"));

    assert!(!output.has_errors(), "{:?}", output.diagnostics);
    assert!(output.bundle.contains("$.answer="), "{}", output.bundle);
    assert!(
        !output.bundle.contains("$[\"answer\"]"),
        "{}",
        output.bundle
    );
}

#[test]
fn set_define_dev_replaces_node_env() {
    // dev 口径：set_define 覆盖默认 prod，`process.env.NODE_ENV` → `"development"`（WAKE-COMPATIBILITY §M3）。
    let fs =
        MemoryFileSystem::from_files([("src/index.js", "export const m = process.env.NODE_ENV;")]);
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    bundler.set_define(vec![(
        "process.env.NODE_ENV".to_string(),
        "\"development\"".to_string(),
    )]);
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(out.bundle.contains("\"development\""), "{}", out.bundle);
    assert!(
        !out.bundle.contains("process.env.NODE_ENV"),
        "{}",
        out.bundle
    );
}

#[test]
fn default_bundle_lowers_import_meta_for_classic_script_chunks() {
    let fs = MemoryFileSystem::from_files([(
        "src/index.js",
        "export const hot = import.meta.hot;\n\
         export const url = import.meta.url;",
    )]);
    let out = Bundler::new(Arc::new(fs)).build(Path::new("src/index.js"));

    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(out.bundle.contains("const hot = false;"), "{}", out.bundle);
    assert!(!out.bundle.contains("import.meta"), "{}", out.bundle);
    assert!(
        out.bundle
            .contains("const url = __wake_require__.metaUrl();"),
        "{}",
        out.bundle
    );
}

#[test]
fn css_extraction_and_asset_threshold() {
    // prod：CSS 抽取为独立 `.css`（不注入 <style>）+ 超阈值资源独立产物（WAKE-COMPATIBILITY §M3）。
    let big = "X".repeat(5000);
    let files: Vec<(String, String)> = vec![
        (
            "src/index.js".to_string(),
            "import './a.css';\nimport img from './big.png';\nexport const u = img;".to_string(),
        ),
        ("src/a.css".to_string(), ".x { color: blue; }".to_string()),
        ("src/big.png".to_string(), big),
    ];
    let mut bundler = IncrementalBundler::new(Arc::new(MemoryFileSystem::from_files(files)));
    bundler
        .enable_css_extraction()
        .set_asset_inline_limit(4096)
        .set_public_path("/");
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    // 抽取的 CSS 产物，含 .x{color:blue}。
    let css = out
        .assets
        .iter()
        .find(|a| a.is_css)
        .expect("应有抽取的 CSS 产物");
    assert!(
        String::from_utf8_lossy(&css.bytes).contains("color: blue"),
        "{}",
        String::from_utf8_lossy(&css.bytes)
    );
    assert!(css.file_name.starts_with("styles.") && css.file_name.ends_with(".css"));

    // 超阈值资源独立产物（hash 命名）。
    let asset = out
        .assets
        .iter()
        .find(|a| !a.is_css)
        .expect("应有独立资源产物");
    assert!(asset.file_name.starts_with("big.") && asset.file_name.ends_with(".png"));
    assert_eq!(asset.bytes.len(), 5000);

    // bundle 不再运行时注入 <style>；资源导出 URL 而非 base64。
    assert!(
        !out.bundle.contains("createElement(\"style\")"),
        "{}",
        out.bundle
    );
    assert!(out.bundle.contains("/big."), "{}", out.bundle);
}

#[test]
fn dead_module_elimination_drops_unreachable() {
    // minify(DCE) 剥离 `if(false)` 里的死 `require` 后，DME 从 entry 重算可达 → 丢弃不可达 dev 模块。
    let fs = MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import { g } from './router.js';\nexport const r = g();",
        ),
        (
            "src/router.js",
            "let impl;\n\
             if (process.env.NODE_ENV === 'production') { impl = require('./prod.js'); }\n\
             else { impl = require('./dev.js'); }\n\
             export const g = impl.g;",
        ),
        ("src/prod.js", "exports.g = function () { return 'PROD'; };"),
        (
            "src/dev.js",
            "exports.g = function () { return 'DEVDEVDEV'; };",
        ),
    ]);
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    bundler.enable_minify().enable_dead_module_elimination();
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    // dev 模块不可达（其 require 在 `if(false)` 分支被 DCE 剥离）→ 丢弃。
    assert!(
        !out.bundle.contains("DEVDEVDEV"),
        "dev 模块应被 DME 丢弃:\n{}",
        out.bundle
    );
    assert!(out.bundle.contains("PROD"), "{}", out.bundle);
    // index + router + prod = 3（dev 删）。
    assert_eq!(out.module_count, 3, "dev 模块应被 DME 移除");
}

#[test]
fn node_builtin_is_externalized() {
    // Node 内置模块（含 `node:` 前缀）不打进 bundle，保留为外部 require，且不报错。
    let fs = MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import { read } from './lib.js';\nexport const r = read();",
        ),
        (
            "src/lib.js",
            "const fs = require('fs');\n\
             const p = require('node:path');\n\
             export function read() { return typeof fs + typeof p; }",
        ),
    ]);
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(
        !out.has_errors(),
        "Node 内置应外部化而非报错: {:?}",
        out.diagnostics
    );
    // 保留为外部 require（未改写为 __wake_require__）。
    assert!(out.bundle.contains("require(\"fs\")"), "{}", out.bundle);
    assert!(
        out.bundle.contains("require(\"node:path\")"),
        "{}",
        out.bundle
    );
    // 只有 index + lib 两个模块（fs / node:path 未进图）。
    assert_eq!(out.module_count, 2);
}

#[test]
fn second_build_is_fully_cached() {
    let mut bundler = IncrementalBundler::new(Arc::new(fixture()));

    let out1 = bundler.build(Path::new("src/index.js"));
    assert!(!out1.has_errors());
    assert_eq!(out1.updated_module_count, out1.module_count);
    assert_eq!(out1.cached_module_count, 0);
    let after_first = bundler.task_exec_count();
    assert_eq!(
        after_first, 6,
        "首遍应执行 3 parse + 3 codegen 任务（3 模块）"
    );

    // 同实例、同内容再打一遍：parse 与 codegen 任务全部浅绿命中，零重执行（Gate-3 缓存命中）。
    let out2 = bundler.build(Path::new("src/index.js"));
    assert!(!out2.has_errors());
    assert_eq!(out2.updated_module_count, 0);
    assert_eq!(out2.cached_module_count, out2.module_count);
    assert_eq!(
        bundler.task_exec_count(),
        after_first,
        "第二遍构建应 100% 命中缓存，parse + codegen 零重执行"
    );
    // 产物一致。
    assert_eq!(out1.bundle, out2.bundle, "两遍产物应完全一致");
}

#[test]
fn edit_reparses_only_changed_module() {
    let fs = Arc::new(fixture());
    let mut bundler = IncrementalBundler::new(fs.clone());

    let _ = bundler.build(Path::new("src/index.js"));
    let after_first = bundler.task_exec_count(); // 6

    // 只改 math.js（语义不变，AST 变）→ 只有 math 重解析 + 重 codegen，index/msg 全命中缓存。
    // index 的 deps 未变 → 其 linker cell no-op → index codegen 也命中（codegen 与 id 解耦）。
    fs.insert("src/math.js", "export function add(a, b) { return b + a; }");
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors());
    assert_eq!(
        bundler.task_exec_count() - after_first,
        2,
        "只改一个模块应只重解析 + 重 codegen 该模块（精确失效）"
    );
}

#[test]
fn cyclic_dependency_does_not_deadlock() {
    // a ↔ b 互相 import：模块依赖图有环。分层 BFS 的 visited 集合处理环，parse 任务互不
    // 递归请求（任务图无环）→ 并行 scan 不死锁、不 panic。若死锁，测试会挂起超时。
    let fs = MemoryFileSystem::from_files([
        (
            "a.js",
            "import { b } from './b.js';\nexport const a = 1;\nexport function useB() { return b; }",
        ),
        (
            "b.js",
            "import { a } from './a.js';\nexport const b = 2;\nexport function useA() { return a; }",
        ),
    ]);
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    let out = bundler.build(Path::new("a.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(out.module_count, 2, "循环依赖的两个模块都应被扫描");
    // 第二遍仍全缓存命中（环不影响增量）。
    let before = bundler.task_exec_count();
    let _ = bundler.build(Path::new("a.js"));
    assert_eq!(
        bundler.task_exec_count(),
        before,
        "循环依赖项目第二遍也应缓存命中"
    );
}

#[test]
fn wide_graph_builds_in_parallel() {
    // entry 依赖 24 个独立模块：一层内 24 个 parse 并行执行（工作窃取扇出）。
    let mut files: Vec<(String, String)> = Vec::new();
    let mut imports = String::new();
    let mut sum = String::new();
    for i in 0..24 {
        imports.push_str(&format!("import m{i} from './m{i}.js';\n"));
        if i > 0 {
            sum.push_str(" + ");
        }
        sum.push_str(&format!("m{i}"));
        files.push((format!("m{i}.js"), format!("export default {i};")));
    }
    files.push((
        "index.js".to_string(),
        format!("{imports}export const total = {sum};"),
    ));
    let fs = MemoryFileSystem::from_files(files);
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    let out = bundler.build(Path::new("index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(out.module_count, 25, "index + 24 叶子");
    // 25 模块 × (parse + codegen) = 50 任务。
    assert_eq!(bundler.task_exec_count(), 50);
    // 第二遍全缓存命中。
    let before = bundler.task_exec_count();
    let _ = bundler.build(Path::new("index.js"));
    assert_eq!(bundler.task_exec_count(), before);
}

#[test]
fn thousand_modules_build_and_cache() {
    // 1000 模块合成项目（二叉树扇出 + 全体共享 util）：验证规模正确性 + 全缓存命中。
    // 时间不在此断言（避免 CI flaky）；<1.5s 由 benches/bundle.rs 度量（实测冷构建 ~7ms）。
    const N: usize = 1000;
    let util = N - 1;
    let mut files: Vec<(String, String)> = Vec::with_capacity(N);
    for i in 0..N {
        let mut src = String::new();
        if i != util {
            src.push_str(&format!("import u from './m{util}.js';\n"));
        }
        let (l, r) = (2 * i + 1, 2 * i + 2);
        if l < util {
            src.push_str(&format!("import a from './m{l}.js';\n"));
        }
        if r < util {
            src.push_str(&format!("import b from './m{r}.js';\n"));
        }
        src.push_str(&format!("export default {i};\n"));
        files.push((format!("m{i}.js"), src));
    }
    let mut bundler = IncrementalBundler::new(Arc::new(MemoryFileSystem::from_files(files)));

    let out = bundler.build(Path::new("m0.js"));
    assert!(
        !out.has_errors(),
        "{:?}",
        &out.diagnostics[..out.diagnostics.len().min(3)]
    );
    assert_eq!(out.module_count, N, "应扫描全部 1000 模块");
    // 共享 util 被约 1000 个模块引用，single-flight 保证只 parse/codegen 一次：
    // 首遍任务数 = 1000 parse + 1000 codegen = 2000。
    assert_eq!(
        bundler.task_exec_count(),
        2 * N as u64,
        "single-flight 去重：每模块恰一次 parse+codegen"
    );

    // 第二遍全缓存命中。
    let before = bundler.task_exec_count();
    let _ = bundler.build(Path::new("m0.js"));
    assert_eq!(
        bundler.task_exec_count(),
        before,
        "1000 模块第二遍应 100% 缓存命中"
    );
}

/// 混合 fixture：ESM 入口默认 + 命名导入一个纯 CJS 包（`module.exports = {...}`，即 React 形态）。
fn cjs_interop_fixture() -> MemoryFileSystem {
    MemoryFileSystem::from_files([
        (
            "index.js",
            "import lib from './lib.js';\n\
             import { helper } from './lib.js';\n\
             import * as ns from './lib.js';\n\
             export const r = lib.value + helper() + ns.default.value;",
        ),
        // 纯 CJS 模块：无 ESM 语法 → 不标记 __esModule → 默认导入取整个 exports。
        (
            "lib.js",
            "module.exports = { value: 40, helper: function() { return 2; } };",
        ),
    ])
}

#[test]
fn cjs_interop_structure() {
    let out = IncrementalBundler::new(Arc::new(cjs_interop_fixture())).build(Path::new("index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    // 默认导入走 interop helper。
    assert!(out.bundle.contains("__wake_interop_default("));
    assert!(out.bundle.contains("__wake_interop_star("));
    // ESM 入口标记 __esModule；纯 CJS 的 lib 不标记（整个 bundle 只此一处 defineProperty 标记）。
    assert_eq!(
        out.bundle
            .matches("Object.defineProperty(exports, \"__esModule\"")
            .count(),
        1,
        "只有 ESM 模块 index 应被标记 __esModule，纯 CJS 的 lib 不标记"
    );
}

#[test]
fn cjs_default_import_runs_in_node() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("node 不可用，跳过 CJS interop e2e");
        return;
    }
    let out = IncrementalBundler::new(Arc::new(cjs_interop_fixture())).build(Path::new("index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_bundle_cjs_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();

    // r = lib.value(40，默认导入取整个 CJS exports) + helper()(2) + ns.default.value(40) = 82。
    let script = format!(
        "const r = require({:?}); if (r.r !== 82) {{ console.error('r=', r.r); process.exit(2); }} process.stdout.write('OK');",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn incremental_bundle_runs_in_node() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("node 不可用，跳过 e2e 执行断言");
        return;
    }
    let mut bundler = IncrementalBundler::new(Arc::new(fixture()));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_bundle_incr_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();

    let script = format!(
        "const r = require({:?}); if (r.result !== 10) {{ console.error('result=', r.result); process.exit(2); }} process.stdout.write('OK');",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// mangle 正确性 e2e：开 minify+mangle 后，被重命名的**函数声明**（导出的 `compute`、本地 `helper`）
/// 在 node 真实运行须结果正确——覆盖「活函数体内跨函数引用一致重命名」（fixture 里函数多为死代码
/// 不被调用，无法覆盖此路径）。曾因语义模型把函数声明名重复声明进 own scope 致兄弟函数同名碰撞。
#[test]
fn mangled_functions_run_correctly_in_node() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("node 不可用，跳过 mangle e2e");
        return;
    }
    let fs = MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import { compute } from './lib.js';\n\
             export const result = compute(4);",
        ),
        (
            "src/lib.js",
            // helper（本地）与 compute（导出）都是顶层函数声明 → 都被 mangle；compute 调用 helper。
            "function helper(n) { return n * 2; }\n\
             export function compute(n) { return helper(n) + helper(n + 1); }",
        ),
    ]);
    // compute(4) = helper(4) + helper(5) = 8 + 10 = 18
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    bundler.enable_minify().enable_mangle();
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_bundle_mangle_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r = require({:?}); if (r.result !== 18) {{ console.error('result=', r.result); process.exit(2); }} process.stdout.write('OK');",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "mangle 后 node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle,
    );
}

/// TypeScript 项目：类型注解 / interface / type / 泛型 全擦除后打包。
fn ts_fixture() -> MemoryFileSystem {
    MemoryFileSystem::from_files([
        (
            "index.ts",
            "import { add } from './math.ts';\n\
             interface Point { x: number; y: number; }\n\
             type Pair = [number, number];\n\
             const p: Point = { x: 2, y: 3 };\n\
             function total<T extends number>(a: T, b: T): number { return add(a, b); }\n\
             export const result: number = total(p.x, p.y);",
        ),
        (
            "math.ts",
            "export function add(a: number, b: number): number {\n\
             const sum: number = a + b;\n\
             return sum;\n\
             }",
        ),
    ])
}

#[test]
fn typescript_project_erases_types() {
    let out = IncrementalBundler::new(Arc::new(ts_fixture())).build(Path::new("index.ts"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(out.module_count, 2);
    // 类型语法擦除、值语义保留。
    assert!(!out.bundle.contains("interface"), "残留 interface");
    assert!(!out.bundle.contains(": number"), "残留类型注解");
    assert!(!out.bundle.contains("<T extends"), "残留泛型");
    assert!(out.bundle.contains("exports[\"result\"] = result;"));
}

#[test]
fn browserslist_target_lowers_exponentiation() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const base = 2; console.log(base ** 3);",
    )]));
    let mut old = IncrementalBundler::new(fs.clone());
    old.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "49"),
    ]));
    let lowered = old.build(Path::new("src/index.js"));
    assert!(!lowered.has_errors(), "{:?}", lowered.diagnostics);
    assert!(
        lowered.bundle.contains("Math.pow(base, 3)"),
        "{}",
        lowered.bundle
    );
    assert!(!lowered.bundle.contains("base**3"), "{}", lowered.bundle);

    let mut modern = IncrementalBundler::new(fs);
    modern.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "120"),
    ]));
    let preserved = modern.build(Path::new("src/index.js"));
    assert!(!preserved.has_errors(), "{:?}", preserved.diagnostics);
    assert!(
        preserved.bundle.contains("base ** 3"),
        "{}",
        preserved.bundle
    );
}

#[test]
fn browserslist_target_lowers_safe_nullish_and_logical_assignments() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "let a; let b = a ?? 2; a ||= 3; a &&= 4; a ??= 5; export { a, b };",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "49"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("??"), "{}", out.bundle);
    assert!(!out.bundle.contains("||="), "{}", out.bundle);
    assert!(!out.bundle.contains("&&="), "{}", out.bundle);
    assert!(out.bundle.contains("a !== null"), "{}", out.bundle);
}

#[test]
fn browserslist_target_lowers_array_call_and_new_spread() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const iterable=new Set([2,3]); \
         const values=[1,...iterable,4]; \
         const receiver={base:10,add(a,b,c,d){return this.base+a+b+c+d}}; \
         const called=receiver.add(...values); \
         let receiverCalls=0,keyCalls=0; \
         function getReceiver(){receiverCalls++;return receiver} \
         function getKey(){keyCalls++;return 'add'} \
         const complex=getReceiver()[getKey()](...values); \
         function Box(a,b,c,d){this.total=a+b+c+d} \
         const built=new Box(...values); \
         let closed=false; \
         const bad={ [Symbol.iterator](){return {next(){return {done:false,get value(){throw Error('stop')}}},return(){closed=true;return {done:true}}}}}; \
         try{[...bad]}catch(error){} \
         export {values,called,complex,receiverCalls,keyCalls,built,closed};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("..."), "{}", out.bundle);
    assert!(!out.bundle.contains("Array.from"), "{}", out.bundle);
    assert!(
        out.bundle.contains("Symbol.iterator")
            && out.bundle.contains("value.slice(0, limit)")
            && out.bundle.contains("(iterable)"),
        "iterator helper must be injected and used: {}",
        out.bundle
    );
    assert!(
        out.bundle.contains(".add") && out.bundle.contains(".apply(__wake_t"),
        "method receiver and function value must be captured: {}",
        out.bundle
    );
    assert!(
        out.bundle.contains("Function.prototype.bind.apply"),
        "constructor spread must use bind/apply: {}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_spread_transform_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?}); \
         if(JSON.stringify(r.values)!=='[1,2,3,4]'||r.called!==20||r.complex!==20||r.receiverCalls!==1||r.keyCalls!==1||r.built.total!==10||!r.closed)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "spread runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn super_member_spread_preserves_receiver_order_and_direct_super_call() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const order=[]; \
         function key(){order.push('key');return 'collect'} \
         const iterable={\
           [Symbol.iterator](){\
             order.push('iterator');let index=0;\
             return {next(){order.push('next'+index);return index<2?{done:false,value:++index}:{done:true}}}\
           }\
         };\
         class Base {\
           constructor(value){this.base=value}\
           collect(a,b){order.push('call');return [this.base,a,b,this instanceof Derived]}\
         }\
         class Derived extends Base {\
           constructor(...args){super(...args)}\
           simple(args){return super.collect(...args)}\
           computed(){return super[key()](...iterable)}\
         }\
         const value=new Derived(10);\
         const simple=value.simple([3,4]);\
         order.length=0;\
         const computed=value.computed();\
         export {simple,computed,order};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        out.bundle.contains("super.collect.apply(this"),
        "simple super member call must use lexical this: {}",
        out.bundle
    );
    assert!(
        out.bundle.contains("super[key()].apply(this"),
        "computed super member call must use lexical this: {}",
        out.bundle
    );
    assert!(
        out.bundle.contains("super(...args)"),
        "direct super spread must remain syntactic until class lowering: {}",
        out.bundle
    );
    assert!(
        !out.bundle.contains("super.apply"),
        "direct super spread must never become super.apply: {}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_super_spread_transform_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});\
         if(JSON.stringify(r.simple)!=='[10,3,4,true]'||\
            JSON.stringify(r.computed)!=='[10,1,2,true]'||\
            JSON.stringify(r.order)!=='[\"key\",\"iterator\",\"next0\",\"next1\",\"next2\",\"call\"]')process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "super spread runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn optional_calls_with_spread_short_circuit_and_preserve_receiver() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "let iterations=0,receiverCalls=0; \
         const args={ [Symbol.iterator](){let done=false;return {next(){iterations++;if(done)return {done:true};done=true;return {done:false,value:4}}}}}; \
         const missing=null; const skipped=missing?.(...args); \
         const receiver={base:3,method(value){return this.base+value}}; \
         function getReceiver(){receiverCalls++;return receiver} \
         const result=getReceiver().method?.(...args); \
         export {skipped,result,iterations,receiverCalls};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    // Chrome 45 supports arrows, but still needs spread and optional-chaining lowering.
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "45"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("?."), "{}", out.bundle);
    assert!(!out.bundle.contains("...args"), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_optional_spread_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(r.skipped!==undefined||r.result!==7||r.iterations!==2||r.receiverCalls!==1)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "optional spread runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn variable_destructuring_lowers_nested_defaults_and_array_rest() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "let rhsCalls=0,nestedGets=0; \
         function source(){rhsCalls++;return {a:2,get nested(){nestedGets++;return {b:undefined}}}} \
         const {a,nested:{b=5}}=source(); \
         const iterable=new Set([10,20,30,40]); \
         const [first,,third,...tail]=iterable; \
         export {a,b,first,third,tail,rhsCalls,nestedGets};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("const {"), "{}", out.bundle);
    assert!(!out.bundle.contains("const ["), "{}", out.bundle);
    assert!(out.bundle.contains("Symbol.iterator"), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_destructuring_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(r.a!==2||r.b!==5||r.first!==10||r.third!==30||JSON.stringify(r.tail)!=='[40]'||r.rhsCalls!==1||r.nestedGets!==1)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "destructuring runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn destructuring_materializes_only_needed_iterator_values() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        const log=[];
        function tracked(name,values){
          return {[Symbol.iterator](){
            log.push(name+":iterator");
            let index=0;
            return {
              next(){
                log.push(name+":next"+index);
                return index<values.length
                  ? {done:false,value:values[index++]}
                  : {done:true};
              },
              return(){log.push(name+":return");return {done:true}}
            };
          }};
        }
        const []=tracked("empty",[1]);
        const [,second,third=30]=tracked("limited",[10,20,undefined,40]);
        const [head,...tail]=tracked("rest",[1,2,3]);
        let assigned;
        [assigned]=tracked("assignment",[9,10]);
        let generatorClosed=false,generatorYieldedSecond=false;
        function* generated(){try{yield 11;generatorYieldedSecond=true;yield 12}finally{generatorClosed=true}}
        const [generatedFirst]=generated();
        const [unicodeFirst]="😀x";
        const originalSymbol=Symbol;
        global.Symbol=undefined;
        const [unicodeFallback]="😀x";
        global.Symbol=originalSymbol;
        let arrayTailReads=0;
        const arrayValue=[4];
        Object.defineProperty(arrayValue,1,{get(){arrayTailReads++;return 5}});
        const [arrayFirst]=arrayValue;
        const [...[nestedFirst,nestedSecond]]=tracked("nested",[6,7,8]);
        export {log,second,third,head,tail,assigned,generatorClosed,generatorYieldedSecond,generatedFirst,unicodeFirst,unicodeFallback,arrayFirst,arrayTailReads,nestedFirst,nestedSecond};
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(out.bundle.contains("(value, limit)"), "{}", out.bundle);
    assert!(out.bundle.contains(", 0)"), "{}", out.bundle);
    assert!(out.bundle.contains(", 3)"), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_destructuring_iterator_limit_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        r#"const r=require({:?});
        const expected=["empty:iterator","empty:return","limited:iterator","limited:next0","limited:next1","limited:next2","limited:return","rest:iterator","rest:next0","rest:next1","rest:next2","rest:next3","assignment:iterator","assignment:next0","assignment:return","nested:iterator","nested:next0","nested:next1","nested:next2","nested:next3"];
        if(JSON.stringify(r.log)!==JSON.stringify(expected)||r.second!==20||r.third!==30||r.head!==1||JSON.stringify(r.tail)!=='[2,3]'||r.assigned!==9||!r.generatorClosed||r.generatorYieldedSecond||r.generatedFirst!==11||r.unicodeFirst!=="😀"||r.unicodeFallback!=="😀"||r.arrayFirst!==4||r.arrayTailReads!==0||r.nestedFirst!==6||r.nestedSecond!==7)process.exit(2);"#,
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "limited destructuring runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn complex_destructuring_parameters_lower_in_functions_and_methods() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "function sum({a,nested:{b=2}}={a:1,nested:{}},[first,...rest]=[3,4,5]){return a+b+first+rest.length} \
         const object={method({value}){return value*2}}; \
         const direct=sum({a:10,nested:{}},new Set([20,30,40])); \
         const defaults=sum(); \
         const method=object.method({value:6}); \
         export {direct,defaults,method};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("function sum({"), "{}", out.bundle);
    assert!(
        !out.bundle.contains("method: function ({"),
        "{}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_complex_params_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(r.direct!==34||r.defaults!==8||r.method!==12)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "complex parameters runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn object_spread_copies_enumerable_string_and_symbol_properties() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const symbol=Symbol('value');let gets=0; \
         const source={get a(){gets++;return 2},[symbol]:3}; \
         Object.defineProperty(source,'hidden',{value:9,enumerable:false}); \
         const result={before:1,...null,...source,a:4,after:5}; \
         export {result,symbol,gets};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("...source"), "{}", out.bundle);
    assert!(
        out.bundle.contains("getOwnPropertySymbols"),
        "{}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_object_spread_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(r.result.before!==1||r.result.a!==4||r.result.after!==5||r.result[r.symbol]!==3||'hidden' in r.result||r.gets!==1)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "object spread runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn object_spread_preserves_explicit_descriptors_and_proto_setters() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "let getCalls=0,setCalls=0,pairSet=0; \
         const baseA={base:'a'},baseB={base:'b'},inherited={}; \
         Object.defineProperty(inherited,'blocked',{set(v){setCalls++}}); \
         const sym=Symbol('s'),order=[]; \
         const key={toString(){order.push('key');return 'computed'}}; \
         function makeSpread(){order.push('spread');return {spread:1,blocked:4}} \
         const result={__proto__:inherited,...makeSpread(),get kept(){getCalls++;return 7},set pair(v){pairSet=v},get pair(){return 6},method(){return this.spread+10},blocked:5,[key]:(order.push('value'),2),[sym]:(order.push('symbol'),3)}; \
         const builtGetCalls=getCalls,builtSetCalls=setCalls,kept=Object.getOwnPropertyDescriptor(result,'kept'); \
         result.pair=8;const firstPairSet=pairSet,pairValue=result.pair; \
         const overwritten={...{},get x(){getCalls++;return 1},...{x:9}}; \
         const overwrittenDescriptor=Object.getOwnPropertyDescriptor(overwritten,'x'); \
         const pairedAcrossSpread={...{},get y(){return 4},...{},set y(v){pairSet=v}}; \
         const acrossValue=pairedAcrossSpread.y;pairedAcrossSpread.y=12; \
         const acrossPairSet=pairSet,acrossDescriptor=Object.getOwnPropertyDescriptor(pairedAcrossSpread,'y'); \
         const before={__proto__:baseA,...{x:1}},after={...{x:1},__proto__:baseB}; \
         const ignored={...{},__proto__:1},nil={...{},__proto__:null}; \
         const functionProto=function(){};const functionBased={...{},__proto__:functionProto}; \
         const computed={...{},['__proto__']:23}; \
         const __proto__=31,shorthand={...{},__proto__}; \
         const protoMethod={...{},__proto__(){return 33}}; \
         const ownKeys=Reflect.ownKeys(result); \
         export {result,sym,order,builtGetCalls,builtSetCalls,kept,firstPairSet,pairValue,overwrittenDescriptor,getCalls,acrossValue,acrossPairSet,acrossDescriptor,before,after,baseA,baseB,ignored,nil,functionBased,functionProto,computed,shorthand,protoMethod,ownKeys};",
    )]));

    let mut old = IncrementalBundler::new(fs.clone());
    old.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let lowered = old.build(Path::new("src/index.js"));
    assert!(!lowered.has_errors(), "{:?}", lowered.diagnostics);
    assert!(
        !lowered.bundle.contains("...makeSpread"),
        "{}",
        lowered.bundle
    );
    assert!(
        lowered.bundle.contains(".define = function"),
        "{}",
        lowered.bundle
    );
    assert!(
        lowered.bundle.contains(".proto = function"),
        "{}",
        lowered.bundle
    );

    let modern = IncrementalBundler::new(fs).build(Path::new("src/index.js"));
    assert!(!modern.has_errors(), "{:?}", modern.diagnostics);
    assert!(modern.bundle.contains("...makeSpread"), "{}", modern.bundle);
    assert!(
        !modern.bundle.contains(".define = function"),
        "{}",
        modern.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_object_spread_descriptors_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &lowered.bundle).unwrap();
    let script = format!(
        "const r=require({:?});const own=Object.prototype.hasOwnProperty;\
         if(r.builtGetCalls!==0||r.builtSetCalls!==0||typeof r.kept.get!=='function'||'value' in r.kept||\
            r.firstPairSet!==8||r.pairValue!==6||r.result.method()!==11||r.result.blocked!==5||\
            r.overwrittenDescriptor.value!==9||r.getCalls!==0||r.acrossValue!==4||r.acrossPairSet!==12||\
            typeof r.acrossDescriptor.get!=='function'||typeof r.acrossDescriptor.set!=='function'||\
            Object.getPrototypeOf(r.before)!==r.baseA||Object.getPrototypeOf(r.after)!==r.baseB||\
            Object.getPrototypeOf(r.ignored)!==Object.prototype||Object.getPrototypeOf(r.nil)!==null||\
            Object.getPrototypeOf(r.functionBased)!==r.functionProto||\
            !own.call(r.computed,'__proto__')||r.computed.__proto__!==23||Object.getPrototypeOf(r.computed)!==Object.prototype||\
            !own.call(r.shorthand,'__proto__')||r.shorthand.__proto__!==31||\
            !own.call(r.protoMethod,'__proto__')||r.protoMethod.__proto__()!==33||\
            r.order.join(',')!=='spread,key,value,symbol'||r.ownKeys[r.ownKeys.length-1]!==r.sym)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "object descriptor/proto spread runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        lowered.bundle
    );
}

#[test]
fn jsx_attribute_spread_follows_object_spread_target_and_order() {
    let fs = Arc::new(MemoryFileSystem::from_files([
        (
            "src/index.jsx",
            "const symbol=Symbol('value');let gets=0; \
             function Widget(){} \
             const source={get value(){gets++;return 2},[symbol]:3}; \
             Object.defineProperty(source,'hidden',{value:9,enumerable:false}); \
             const element=<Widget before={1} {...null} {...source} value={4}>child</Widget>; \
             export {element,symbol,gets};",
        ),
        (
            "node_modules/react/jsx-runtime.js",
            "function h(type,props,key){return {type:type,props:props||{},key:key}} \
             exports.jsx=h;exports.jsxs=h;exports.Fragment='#frag';",
        ),
    ]));

    let mut old = IncrementalBundler::new(fs.clone());
    old.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let lowered = old.build(Path::new("src/index.jsx"));
    assert!(!lowered.has_errors(), "{:?}", lowered.diagnostics);
    assert!(!lowered.bundle.contains("...source"), "{}", lowered.bundle);
    assert!(
        lowered.bundle.contains("getOwnPropertySymbols"),
        "{}",
        lowered.bundle
    );

    let modern = IncrementalBundler::new(fs).build(Path::new("src/index.jsx"));
    assert!(!modern.has_errors(), "{:?}", modern.diagnostics);
    assert!(modern.bundle.contains("...source"), "{}", modern.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_jsx_spread_transform_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &lowered.bundle).unwrap();
    let script = format!(
        "const r=require({:?}),p=r.element.props;if(p.before!==1||p.value!==4||p.children!=='child'||p[r.symbol]!==3||'hidden' in p||r.gets!==1)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "JSX spread runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        lowered.bundle
    );
}

#[test]
fn object_rest_excludes_static_computed_and_symbol_keys_once() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const symbol=Symbol('s');let keyCalls=0,gets=0; \
         function key(){keyCalls++;return 'a'} \
         const source={get a(){gets++;return 1},b:2,[symbol]:3}; \
         const {[key()]:a,[symbol]:s,...rest}=source; \
         function pick({x,...tail}){return [x,tail]} \
         const picked=pick({x:4,y:5}); \
         export {a,s,rest,picked,keyCalls,gets,symbol};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("...rest"), "{}", out.bundle);
    assert!(!out.bundle.contains("...tail"), "{}", out.bundle);
    assert!(out.bundle.contains(".rest("), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_object_rest_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(r.a!==1||r.s!==3||r.rest.b!==2||'a' in r.rest||r.symbol in r.rest||r.picked[0]!==4||r.picked[1].y!==5||'x' in r.picked[1]||r.keyCalls!==1||r.gets!==1)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "object rest runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn empty_object_binding_patterns_throw_before_later_work() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "let calls=0;function source(value){calls++;return value} \
         function throws(callback){try{callback();return false}catch(error){return error instanceof TypeError}} \
         const variableThrows=throws(function(){const {}=source(null)}); \
         function parameter({}){} \
         const parameterThrows=throws(function(){parameter(source(undefined))}); \
         const catchThrows=throws(function(){try{throw source(null)}catch({}){}}); \
         const loopThrows=throws(function(){for(const {} of [source(undefined)]){}}); \
         export {calls,variableThrows,parameterThrows,catchThrows,loopThrows};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("const {}"), "{}", out.bundle);
    assert!(
        !out.bundle.contains("function parameter({})"),
        "{}",
        out.bundle
    );
    assert!(!out.bundle.contains("catch ({})"), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_empty_object_bindings_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(r.calls!==4||!r.variableThrows||!r.parameterThrows||!r.catchThrows||!r.loopThrows)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "empty object binding runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn object_rest_uses_property_keys_for_numeric_symbol_and_object_keys() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const symbol=Symbol('symbol');let keyCalls=0,conversions=0,hints=[]; \
         const objectKey={};objectKey[Symbol.toPrimitive]=function(hint){conversions++;hints.push(hint);return 'chosen'}; \
         function key(){keyCalls++;return objectKey} \
         const source={1:'one',chosen:'selected',keep:3,[symbol]:4}; \
         const {1:one,[key()]:chosen,[symbol]:symbolValue,...rest}=source; \
         export {one,chosen,symbolValue,rest,symbol,keyCalls,conversions,hints};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "55"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("...rest"), "{}", out.bundle);
    assert!(out.bundle.contains("String("), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_object_rest_property_keys_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(r.one!=='one'||r.chosen!=='selected'||r.symbolValue!==4||r.rest.keep!==3||'1' in r.rest||'chosen' in r.rest||r.symbol in r.rest||r.keyCalls!==1||r.conversions!==1||JSON.stringify(r.hints)!=='[\"string\"]')process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "object-rest property-key runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn destructuring_assignments_lower_in_order_and_return_the_rhs() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const log=[];let sourceObject,memberHits=0,assigned=0;const holder={get value(){return assigned},set value(value){log.push('set');assigned=value}}; \
         function rhs(){log.push('rhs');sourceObject={get chosen(){log.push('get');return undefined},nested:{value:8},keep:9};return sourceObject} \
         function key(){log.push('key');return 'chosen'} \
         function fallback(){log.push('default');return 4} \
         function target(){log.push('target');memberHits++;return holder} \
         let nested,rest;const returned=({[key()]:target().value=fallback(),nested:{value:nested},...rest}=rhs()); \
         let first,nestedArray,tail;const arraySource=new Set([undefined,{deep:6},7,8]); \
         const arrayReturned=([first=5,{deep:nestedArray},...tail]=arraySource); \
         let rawNested,rawRest;[{deep:rawNested,...rawRest}]=[{deep:10,extra:11}]; \
         export {log,holder,nested,rest,memberHits,first,nestedArray,tail,returned,sourceObject,arrayReturned,arraySource,rawNested,rawRest};",
    )]));

    let mut old = IncrementalBundler::new(fs.clone());
    old.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let lowered = old.build(Path::new("src/index.js"));
    assert!(!lowered.has_errors(), "{:?}", lowered.diagnostics);
    assert!(!lowered.bundle.contains("...rest"), "{}", lowered.bundle);
    assert!(!lowered.bundle.contains("...tail"), "{}", lowered.bundle);
    assert!(!lowered.bundle.contains("...rawRest"), "{}", lowered.bundle);
    assert!(!lowered.bundle.contains("=>"), "{}", lowered.bundle);
    assert!(lowered.bundle.contains(".rest("), "{}", lowered.bundle);

    let modern = IncrementalBundler::new(fs).build(Path::new("src/index.js"));
    assert!(!modern.has_errors(), "{:?}", modern.diagnostics);
    assert!(modern.bundle.contains("...rest"), "{}", modern.bundle);
    assert!(modern.bundle.contains("...tail"), "{}", modern.bundle);
    assert!(modern.bundle.contains("...rawRest"), "{}", modern.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_assignment_destructuring_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &lowered.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(JSON.stringify(r.log)!=='[\"rhs\",\"key\",\"target\",\"get\",\"default\",\"set\"]'||r.holder.value!==4||r.nested!==8||r.rest.keep!==9||'chosen' in r.rest||'nested' in r.rest||r.memberHits!==1||r.first!==5||r.nestedArray!==6||JSON.stringify(r.tail)!=='[7,8]'||r.returned!==r.sourceObject||r.arrayReturned!==r.arraySource||r.rawNested!==10||r.rawRest.extra!==11||'deep' in r.rawRest)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "assignment destructuring runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        lowered.bundle
    );
}

#[test]
fn destructuring_assignment_for_heads_lower_for_in_of_and_for_await() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "let first,tail,keyValue,objectRest;const member={};let defaultHits=0;const __wake_t0='collision'; \
         for ([first=++defaultHits,...tail] of [[undefined,2,3]]) {} \
         for ({chosen:member.value,...objectRest} of [{chosen:4,keep:5}]) {} \
         for ([keyValue] in {xy:1}) {} \
         const pending=(async function(){let awaited,awaitRest;for await ({value:awaited,...awaitRest} of [{value:6,extra:7}]){}return [awaited,awaitRest.extra]})(); \
         export {first,tail,keyValue,objectRest,member,defaultHits,__wake_t0,pending};",
    )]));

    let mut old = IncrementalBundler::new(fs.clone());
    old.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let lowered = old.build(Path::new("src/index.js"));
    assert!(!lowered.has_errors(), "{:?}", lowered.diagnostics);
    assert!(!lowered.bundle.contains("for (["), "{}", lowered.bundle);
    assert!(!lowered.bundle.contains("for ({"), "{}", lowered.bundle);
    assert!(
        !lowered.bundle.contains("for await ({"),
        "{}",
        lowered.bundle
    );
    assert!(
        lowered.bundle.contains("for (var __wake_t"),
        "{}",
        lowered.bundle
    );
    assert!(lowered.bundle.contains(".rest("), "{}", lowered.bundle);
    assert!(!lowered.bundle.contains("=>"), "{}", lowered.bundle);

    let mut object_rest_only = IncrementalBundler::new(fs.clone());
    object_rest_only.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "55"),
    ]));
    let rest_lowered = object_rest_only.build(Path::new("src/index.js"));
    assert!(!rest_lowered.has_errors(), "{:?}", rest_lowered.diagnostics);
    assert!(
        rest_lowered.bundle.contains("...tail"),
        "{}",
        rest_lowered.bundle
    );
    assert!(
        !rest_lowered.bundle.contains("...objectRest"),
        "{}",
        rest_lowered.bundle
    );
    assert!(
        !rest_lowered.bundle.contains("...awaitRest"),
        "{}",
        rest_lowered.bundle
    );

    let modern = IncrementalBundler::new(fs).build(Path::new("src/index.js"));
    assert!(!modern.has_errors(), "{:?}", modern.diagnostics);
    assert!(modern.bundle.contains("...tail"), "{}", modern.bundle);
    assert!(modern.bundle.contains("...objectRest"), "{}", modern.bundle);
    assert!(modern.bundle.contains("...awaitRest"), "{}", modern.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_for_target_destructuring_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &lowered.bundle).unwrap();
    let script = format!(
        "(async()=>{{const r=require({:?});const pending=await r.pending;if(r.first!==1||JSON.stringify(r.tail)!=='[2,3]'||r.keyValue!=='x'||r.member.value!==4||r.objectRest.keep!==5||'chosen' in r.objectRest||r.defaultHits!==1||r.__wake_t0!=='collision'||JSON.stringify(pending)!=='[6,7]')process.exit(2)}})().catch(error=>{{console.error(error);process.exit(3)}});",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "for-head destructuring runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        lowered.bundle
    );
}

#[test]
fn destructuring_assignment_for_heads_preserve_super_and_await_context() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "class Base{set slot(value){this.seen=value}}class Derived extends Base{run(values){for ({value:super.slot} of values){}return this.seen}} \
         async function pick(values){let value;for ([value=await Promise.resolve(3)] of values){}return value} \
         export const result=new Derived().run([{value:2}]);export const pending=pick([[undefined]]);",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("for ({"), "{}", out.bundle);
    assert!(!out.bundle.contains("for (["), "{}", out.bundle);
    assert!(out.bundle.contains("super.slot ="), "{}", out.bundle);
    assert!(
        out.bundle.contains("await Promise.resolve(3)"),
        "{}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_for_target_context_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "Promise.resolve(require({:?}).pending).then(value=>{{const r=require({:?});if(r.result!==2||value!==3)process.exit(2)}}).catch(error=>{{console.error(error);process.exit(3)}});",
        bundle_path.to_string_lossy(),
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "for-head context runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn destructuring_assignment_preserves_await_and_yield_context() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "async function pick(source){let value;const returned=({value=await Promise.resolve(3)}=source);return [value,returned===source]} \
         function* choose(source){let value;const returned=({value=yield 4}=source);return [value,returned===source]} \
         const iterator=choose({});const yielded=iterator.next();const resumed=iterator.next(5);export {pick,yielded,resumed};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        out.bundle.contains("await Promise.resolve(3)"),
        "{}",
        out.bundle
    );
    assert!(out.bundle.contains("yield 4"), "{}", out.bundle);
    assert!(!out.bundle.contains("{ value = await"), "{}", out.bundle);
    assert!(!out.bundle.contains("{ value = yield"), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_assignment_suspension_context_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "Promise.resolve(require({:?}).pick({{}})).then(value=>{{const r=require({:?});if(JSON.stringify(value)!=='[3,true]'||r.yielded.value!==4||r.yielded.done||JSON.stringify(r.resumed.value)!=='[5,true]'||!r.resumed.done)process.exit(2)}}).catch(error=>{{console.error(error);process.exit(3)}});",
        bundle_path.to_string_lossy(),
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "assignment suspension runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn scoped_destructuring_assignment_preserves_arrow_lexical_context() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const source={chosen:undefined,keep:9};function Box(fallback){this.keyHits=0;this.key=function(){this.keyHits++;return 'chosen'}; \
         const assign=input=>({[this.key()]:this.value=new.target===Box?arguments[0]:0,...this.rest}=input); \
         const returned=assign(source);this.result=[returned===source,this.value,this.rest.keep,this.keyHits]}export const result=new Box(7).result;",
    )]));

    let mut arrow_capable = IncrementalBundler::new(fs.clone());
    arrow_capable.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "45"),
    ]));
    let lowered = arrow_capable.build(Path::new("src/index.js"));
    assert!(!lowered.has_errors(), "{:?}", lowered.diagnostics);
    assert!(lowered.bundle.contains("=>"), "{}", lowered.bundle);
    assert!(
        !lowered.bundle.contains("...this.rest"),
        "{}",
        lowered.bundle
    );
    assert!(lowered.bundle.contains(".rest("), "{}", lowered.bundle);
    assert!(lowered.bundle.contains("new.target"), "{}", lowered.bundle);
    assert!(
        lowered.bundle.contains("arguments[0]"),
        "{}",
        lowered.bundle
    );
    assert!(
        lowered.bundle.contains("var __wake_t"),
        "{}",
        lowered.bundle
    );

    let modern = IncrementalBundler::new(fs).build(Path::new("src/index.js"));
    assert!(!modern.has_errors(), "{:?}", modern.diagnostics);
    assert!(modern.bundle.contains("...this.rest"), "{}", modern.bundle);
    assert!(!modern.bundle.contains(".rest("), "{}", modern.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_assignment_arrow_lexical_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &lowered.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(JSON.stringify(r.result)!=='[true,7,9,1]')process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "arrow lexical assignment runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        lowered.bundle
    );
}

#[test]
fn destructuring_assignment_keeps_conservative_regions_helper_free() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "let value,rest;const source={value:1,keep:2}; \
         function parameter(arg=({value,...rest}=source)){return arg} \
         const parameterArrow=(arg=({value,...rest}=source))=>arg; \
         class Holder{field=({value,...rest}=source);static{({value,...rest}=source)}} \
         const asyncArrow=async()=>({value,...rest}=source);export {parameter,parameterArrow,Holder,asyncArrow};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(out.bundle.matches("...rest").count() >= 3, "{}", out.bundle);
    assert!(
        !out.bundle.contains("getOwnPropertySymbols"),
        "{}",
        out.bundle
    );
    assert!(!out.bundle.contains(".rest("), "{}", out.bundle);
}

#[test]
fn arrow_complex_parameters_lower_with_object_and_array_rest() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const context={base:10,run(){const calculate=({a,...rest}={a:1,b:2},[first,...tail]=new Set([3,4]))=>this.base+a+rest.b+first+tail.length;return calculate()}}; \
         export const result=context.run();",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("...rest"), "{}", out.bundle);
    assert!(!out.bundle.contains("...tail"), "{}", out.bundle);
    assert!(out.bundle.contains(".bind(this)"), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_arrow_complex_params_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(r.result!==17)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "arrow complex parameters runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn mixed_parameter_lowering_preserves_left_to_right_initialization() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const log=[];function mark(value){log.push(value);return value} \
         function mixed(a=mark('a'),{b=mark('b')}={},c=mark('c'),...rest){return [a,b,c,rest.length]} \
         const result=mixed(undefined,undefined,undefined,1,2);export {log,result};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_mixed_params_order_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(JSON.stringify(r.log)!=='[\"a\",\"b\",\"c\"]'||JSON.stringify(r.result)!=='[\"a\",\"b\",\"c\",2]')process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "mixed parameter order failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn catch_and_declared_loop_destructuring_lower_with_separate_body_scope() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "let outer=7,loopTotal=0,keys='',caught; \
         for(const {a=outer,...rest} of [{a:undefined,b:2},{a:3,b:4}]){let outer=100;loopTotal+=a+rest.b} \
         for(const [first,...tail] in {ab:1,cd:1}){keys+=first+tail.join('')} \
         try{throw {message:undefined,code:5,extra:9}}catch({message=outer,code,...rest}){let outer=100;caught=[message,code,rest.extra]} \
         export {loopTotal,keys,caught};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("for (const {"), "{}", out.bundle);
    assert!(!out.bundle.contains("for (const ["), "{}", out.bundle);
    assert!(!out.bundle.contains("catch ({"), "{}", out.bundle);
    assert!(out.bundle.contains(".rest("), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_loop_catch_destructuring_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(r.loopTotal!==16||r.keys!=='abcd'||JSON.stringify(r.caught)!=='[7,5,9]')process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "loop/catch destructuring runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn object_rest_only_target_lowers_every_binding_context() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const source={a:1,b:2,c:3}; \
         const {a,...variableRest}=source; \
         function pick({b,...functionRest}){return b+functionRest.a+functionRest.c} \
         const arrow=({c,...arrowRest})=>c+arrowRest.a+arrowRest.b; \
         let catchResult=0;try{throw source}catch({a:caughtA,...catchRest}){catchResult=caughtA+catchRest.b+catchRest.c} \
         let loopResult=0;for(const {b:loopB,...loopRest} of [source]){loopResult+=loopB+loopRest.a+loopRest.c} \
         export const results=[a+variableRest.b+variableRest.c,pick(source),arrow(source),catchResult,loopResult];",
    )]));

    // Chrome 55 supports ordinary destructuring but not object rest/spread.
    let mut old = IncrementalBundler::new(fs.clone());
    old.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "55"),
    ]));
    let lowered = old.build(Path::new("src/index.js"));
    assert!(!lowered.has_errors(), "{:?}", lowered.diagnostics);
    assert!(!lowered.bundle.contains("..."), "{}", lowered.bundle);
    assert!(lowered.bundle.contains(".rest("), "{}", lowered.bundle);

    let modern = IncrementalBundler::new(fs).build(Path::new("src/index.js"));
    assert!(!modern.has_errors(), "{:?}", modern.diagnostics);
    assert!(
        modern.bundle.contains("...variableRest"),
        "{}",
        modern.bundle
    );
    assert!(
        modern.bundle.contains("...functionRest"),
        "{}",
        modern.bundle
    );
    assert!(modern.bundle.contains("...arrowRest"), "{}", modern.bundle);
    assert!(modern.bundle.contains("...catchRest"), "{}", modern.bundle);
    assert!(modern.bundle.contains("...loopRest"), "{}", modern.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_object_rest_only_bindings_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &lowered.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(JSON.stringify(r.results)!=='[6,6,6,6,6]')process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "object-rest-only binding runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        lowered.bundle
    );
}

#[test]
fn nullish_side_effect_operand_is_evaluated_through_lexical_iife() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "let calls=0; function get(){ calls++; return null; } \
         const value=get() ?? 7; export { calls, value };",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    // Chrome 70 supports arrows but not nullish coalescing.
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "70"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("??"), "{}", out.bundle);
    assert!(out.bundle.contains("__wake_t0) =>"), "{}", out.bundle);
    assert_eq!(out.bundle.matches(")(get())").count(), 1, "{}", out.bundle);
}

#[test]
fn optional_member_chain_lowers_as_one_short_circuit_unit() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "let hits=0; function get(){hits++;return {x:{y:3},m(){return this.x.y}}} \
         const a=get()?.x.y; const b=get()?.m(); const c=null?.x; \
         const obj={x:9,m(){return this.x}}; const d=obj.m?.(); \
         const e=get().m?.(); export {a,b,c,d,e,hits};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "70"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("?."), "{}", out.bundle);
    assert!(out.bundle.contains("__wake_t"), "{}", out.bundle);
    assert!(out.bundle.contains(".x.y"), "suffix must stay in alternate");
    assert!(
        out.bundle.contains(".m()"),
        "method call must preserve receiver"
    );
    assert!(
        out.bundle.matches(".call(__wake_t").count() >= 2,
        "optional method calls must use their captured receivers: {}",
        out.bundle
    );
}

#[test]
fn parenthesized_optional_method_calls_preserve_receiver_and_argument_order() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        "use strict";
        const order=[];
        const replacement={tag:"replacement"};
        let holder;
        const original={
          tag:"original",
          get method(){order.push("getter:normal");holder=replacement;return function(value){order.push("call:"+this.tag);return this.tag+":"+value}}
        };
        holder=original;
        const normal=(holder?.method)((order.push("arg:normal"),"normal"));

        let spreadHolder;
        const spreadReplacement={tag:"spread-replacement"};
        const spreadOriginal={
          tag:"spread-original",
          get method(){order.push("getter:spread");spreadHolder=spreadReplacement;return function(value){order.push("call:"+this.tag);return this.tag+":"+value}}
        };
        spreadHolder=spreadOriginal;
        const spreadArgs={
          [Symbol.iterator](){order.push("iterator:spread");let done=false;return {next(){order.push(done?"next:spread:done":"next:spread:value");if(done)return {done:true};done=true;return {done:false,value:"spread"}}}}
        };
        const spread=(spreadHolder?.method)(...spreadArgs);

        let nullCallThrew=false;
        try{(null?.method)((order.push("arg:null"),0))}catch(error){nullCallThrew=error instanceof TypeError}
        const nullArgs={
          [Symbol.iterator](){order.push("iterator:null");let done=false;return {next(){order.push(done?"next:null:done":"next:null:value");if(done)return {done:true};done=true;return {done:false,value:0}}}}
        };
        let nullSpreadThrew=false;
        try{(null?.method)(...nullArgs)}catch(error){nullSpreadThrew=error instanceof TypeError}
        let optionalArgumentCalls=0;
        const optionalSkipped=(null?.method)?.(optionalArgumentCalls++);
        export {normal,spread,nullCallThrew,nullSpreadThrew,optionalArgumentCalls,optionalSkipped,order};
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("?."), "{}", out.bundle);
    assert!(!out.bundle.contains("...spreadArgs"), "{}", out.bundle);
    assert!(!out.bundle.contains("...nullArgs"), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_parenthesized_optional_call_chrome40_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let expected = r#"["getter:normal","arg:normal","call:original","getter:spread","iterator:spread","next:spread:value","next:spread:done","call:spread-original","arg:null","iterator:null","next:null:value","next:null:done"]"#;
    let script = format!(
        "const r=require({:?});\
         if(r.normal!=='original:normal'||r.spread!=='spread-original:spread'||\
            !r.nullCallThrew||!r.nullSpreadThrew||r.optionalArgumentCalls!==0||\
            r.optionalSkipped!==undefined||JSON.stringify(r.order)!=={:?})process.exit(2);",
        bundle_path.to_string_lossy(),
        expected
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "parenthesized optional-call runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn modern_parenthesized_optional_method_call_keeps_chain_boundary() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        "use strict";
        const obj={tag:"native",method(value){return this.tag+value}};
        let argumentCalls=0;
        const value=(obj?.method)("!");
        let threw=false;
        try{(null?.method)(argumentCalls++)}catch(error){threw=error instanceof TypeError}
        let optionalArgumentCalls=0;
        const optionalSkipped=(null?.method)?.(optionalArgumentCalls++);
        export {value,threw,argumentCalls,optionalArgumentCalls,optionalSkipped};
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "120"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        out.bundle.contains("(obj?.method)(\"!\")"),
        "{}",
        out.bundle
    );
    assert!(
        out.bundle.contains("(null?.method)(argumentCalls++)"),
        "{}",
        out.bundle
    );
    assert!(!out.bundle.contains("__wake_t"), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_parenthesized_optional_call_modern_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(r.value!=='native!'||!r.threw||r.argumentCalls!==1||\
         r.optionalArgumentCalls!==0||r.optionalSkipped!==undefined)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "modern parenthesized optional-call runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn typescript_erasure_keeps_parenthesized_optional_call_receiver() {
    let source = r#"
        "use strict";
        const order=[];
        const replacement={tag:"replacement"};
        let holder;
        const original={
          tag:"original",
          get method(){order.push("getter");holder=replacement;return function(value){order.push("call:"+this.tag);return this.tag+":"+value}}
        };
        holder=original;
        const asserted=(holder?.method)!("asserted");
        holder=original;
        const generic=(holder?.method)<string>("generic");
        let argumentCalls=0;
        let threw=false;
        try{(null?.method)!(argumentCalls++)}catch(error){threw=error instanceof TypeError}
        export {asserted,generic,argumentCalls,threw,order};
    "#;
    let fs = Arc::new(MemoryFileSystem::from_files([("src/index.ts", source)]));
    let mut old = IncrementalBundler::new(fs);
    old.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let old_out = old.build(Path::new("src/index.ts"));
    assert!(!old_out.has_errors(), "{:?}", old_out.diagnostics);
    assert!(!old_out.bundle.contains("?."), "{}", old_out.bundle);
    assert!(!old_out.bundle.contains("<string>"), "{}", old_out.bundle);

    let modern_fs = Arc::new(MemoryFileSystem::from_files([("src/index.ts", source)]));
    let modern_out = IncrementalBundler::new(modern_fs).build(Path::new("src/index.ts"));
    assert!(!modern_out.has_errors(), "{:?}", modern_out.diagnostics);
    assert!(
        modern_out.bundle.matches("(holder?.method)(").count() >= 2,
        "{}",
        modern_out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_ts_parenthesized_optional_call_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &old_out.bundle).unwrap();
    let expected = r#"["getter","call:original","getter","call:original"]"#;
    let script = format!(
        "const r=require({:?});if(r.asserted!=='original:asserted'||r.generic!=='original:generic'||\
         r.argumentCalls!==1||!r.threw||JSON.stringify(r.order)!=={:?})process.exit(2);",
        bundle_path.to_string_lossy(),
        expected
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "TS parenthesized optional-call runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        old_out.bundle
    );
}

#[test]
fn parenthesized_optional_tag_and_constructor_keep_chain_boundaries() {
    let source = r#"
        "use strict";
        const order=[];
        const replacement={name:"replacement"};
        function Box(value){this.value=value}
        let holder;
        const original={
          name:"original",
          get tag(){order.push("getter:tag");holder=replacement;return function(strings,value){order.push("tag:"+this.name);return this.name+":"+value}},
          get Ctor(){order.push("getter:ctor");holder=replacement;return Box}
        };
        holder=original;
        const tagged=(holder?.tag)`prefix:${(order.push("sub:tag"),"value")}`;
        holder=original;
        const made=new (holder?.Ctor)((order.push("arg:new"),7));
        let nullTagThrew=false;
        try{(null?.tag)`missing:${(order.push("sub:null-tag"),0)}`}catch(error){nullTagThrew=error instanceof TypeError}
        let nullNewThrew=false;
        try{new (null?.Ctor)((order.push("arg:null-new"),0))}catch(error){nullNewThrew=error instanceof TypeError}
        export {tagged,made,nullTagThrew,nullNewThrew,order};
    "#;
    let fs = Arc::new(MemoryFileSystem::from_files([("src/index.js", source)]));
    let mut old = IncrementalBundler::new(fs);
    old.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let old_out = old.build(Path::new("src/index.js"));
    assert!(!old_out.has_errors(), "{:?}", old_out.diagnostics);
    assert!(!old_out.bundle.contains("?."), "{}", old_out.bundle);

    let modern_fs = Arc::new(MemoryFileSystem::from_files([("src/index.js", source)]));
    let modern_out = IncrementalBundler::new(modern_fs).build(Path::new("src/index.js"));
    assert!(!modern_out.has_errors(), "{:?}", modern_out.diagnostics);
    assert!(
        modern_out.bundle.contains("(holder?.tag)`"),
        "{}",
        modern_out.bundle
    );
    assert!(
        modern_out.bundle.contains("new (holder?.Ctor)("),
        "{}",
        modern_out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_parenthesized_optional_tag_new_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &old_out.bundle).unwrap();
    let expected = r#"["getter:tag","sub:tag","tag:original","getter:ctor","arg:new","sub:null-tag","arg:null-new"]"#;
    let script = format!(
        "const r=require({:?});if(r.tagged!=='original:value'||r.made.value!==7||\
         !r.nullTagThrew||!r.nullNewThrew||JSON.stringify(r.order)!=={:?})process.exit(2);",
        bundle_path.to_string_lossy(),
        expected
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "parenthesized optional tag/new runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        old_out.bundle
    );
}

#[test]
fn chrome_40_optional_chains_use_scope_temporaries_and_preserve_semantics() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        "use strict";
        let baseCalls=0,keyCalls=0,argCalls=0,iteratorSteps=0;
        function make(value){baseCalls++;return value}
        const skipped=make(null)?.[(()=>{keyCalls++;return "method"})()](argCalls++);
        const nested=make(null)?.deep.value;
        let holder;
        const replacement={tag:"replacement"};
        const original={tag:"original",get method(){holder=replacement;return function(suffix){return this.tag+suffix}}};
        holder=original;
        const rebound=holder.method?.("!");
        function scoped(value){"use strict";return make(value)?.deep?.value}
        const scopedValues=[scoped({deep:{value:8}}),scoped(null)];
        const arrow=value=>make(value)?.deep?.value;
        const arrowValue=arrow({deep:{value:9}});
        const runner={prefix:"method",run(value){return make(value)?.call(this)}};
        const callable=function(){return this.prefix};
        const methodResult=runner.run(callable);
        const iterable={
          [Symbol.iterator](){
            let done=false;
            return {next(){iteratorSteps++;if(done)return {done:true};done=true;return {done:false,value:"?"}}}
          }
        };
        let spreadHolder;
        const spreadReplacement={tag:"spread-replacement"};
        const spreadOriginal={tag:"spread-original",get method(){spreadHolder=spreadReplacement;return function(suffix){return this.tag+suffix}}};
        spreadHolder=spreadOriginal;
        const spreadResult=spreadHolder?.method(...iterable);
        const spreadSkipped=null?.method(...iterable);
        class Base{get optional(){return function(){return this.kind}}}
        class Child extends Base{constructor(){super();this.kind="child"}run(){return super.optional?.()}}
        const superResult=new Child().run();
        const doomed={x:1};
        const deleted=delete doomed?.x;
        const deleteSkipped=delete (null)?.[(()=>{keyCalls++;return "x"})()];
        const leaked=Object.keys(globalThis).some(function(key){return key.indexOf("__wake_t")===0});
        export {skipped,nested,rebound,scopedValues,arrowValue,methodResult,spreadResult,spreadSkipped,superResult,deleted,deleteSkipped,doomed,baseCalls,keyCalls,argCalls,iteratorSteps,leaked};
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("?."), "{}", out.bundle);
    assert!(
        out.bundle.matches("var __wake_t").count() >= 5,
        "Program/functions/methods/sync arrows must own their temps: {}",
        out.bundle
    );
    let strict = out.bundle.find("\"use strict\";").expect("directive");
    let program_temp = out.bundle[strict..]
        .find("var __wake_t")
        .map(|offset| strict + offset)
        .expect("program temp declaration");
    assert!(
        strict < program_temp,
        "directive must precede temps: {}",
        out.bundle
    );
    assert!(
        out.bundle
            .contains("function scoped(value) {\n    \"use strict\";\n    var __wake_t"),
        "function directives must precede scoped temps: {}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_scoped_optional_chrome40_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});\
         if(r.skipped!==undefined||r.nested!==undefined||r.rebound!=='original!'||\
            JSON.stringify(r.scopedValues)!=='[8,null]'||r.arrowValue!==9||r.methodResult!=='method'||\
            r.spreadResult!=='spread-original?'||r.spreadSkipped!==undefined||r.superResult!=='child'||\
            r.deleted!==true||r.deleteSkipped!==true||Object.prototype.hasOwnProperty.call(r.doomed,'x')||\
            r.baseCalls!==6||r.keyCalls!==0||r.argCalls!==0||r.iteratorSteps!==2||r.leaked)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "scoped-optional runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn minify_keeps_live_scoped_transform_temps_with_dummy_spans() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        "use strict";
        let calls=0;
        function make(value){calls++;return value}
        function scoped(value){"use strict";return [typeof this,make(value)?.value]}
        const present=make({value:7})?.value;
        const missing=make(null)?.value;
        const scopedResult=scoped.call(1,{value:9});
        export {present,missing,scopedResult,calls};
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    bundler.enable_minify().enable_mangle();
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("?."), "{}", out.bundle);
    assert!(
        out.bundle.contains("var __wake_t"),
        "a live transform temp must not be removed because an unused sibling shares DUMMY: {}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_minified_scoped_temp_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(r.present!==7||r.missing!==undefined||JSON.stringify(r.scopedResult)!=='[\"number\",9]'||r.calls!==3)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "minified scoped-temp runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn optional_chain_temps_are_absent_for_modern_and_conservative_regions() {
    let modern_fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const obj={x:1};const value=obj?.x;const removed=delete obj?.x;export {value,removed,obj};",
    )]));
    let mut modern = IncrementalBundler::new(modern_fs);
    modern.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "120"),
    ]));
    let modern_out = modern.build(Path::new("src/index.js"));
    assert!(!modern_out.has_errors(), "{:?}", modern_out.diagnostics);
    assert!(
        modern_out.bundle.contains("obj?.x"),
        "{}",
        modern_out.bundle
    );
    assert!(
        modern_out.bundle.contains("delete obj?.x"),
        "modern delete optional chain must remain native: {}",
        modern_out.bundle
    );
    assert!(
        !modern_out.bundle.contains("__wake_t"),
        "{}",
        modern_out.bundle
    );

    let conservative_fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "let calls=0;function get(){calls++;return {x:1}}\
         const covered=(get()?.x);\
         const removed=delete (get()?.x);\
         const asyncArrow=async()=>((await Promise.resolve(get()))?.x);\
         function defaulted(value=get()?.x){return value}\
         const arrowDefault=(value=get()?.x)=>value;\
         class Box{field=get()?.x;static{this.value=get()?.x}}\
         export {covered,removed,asyncArrow,defaulted,arrowDefault,Box,calls};",
    )]));
    let mut conservative = IncrementalBundler::new(conservative_fs);
    conservative.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let conservative_out = conservative.build(Path::new("src/index.js"));
    assert!(
        !conservative_out.has_errors(),
        "{:?}",
        conservative_out.diagnostics
    );
    assert!(
        conservative_out.bundle.matches("?.").count() >= 6,
        "cover/default/class/async-arrow chains are intentionally preserved: {}",
        conservative_out.bundle
    );
    assert!(
        conservative_out.bundle.contains("delete get()?.x"),
        "conservative delete must retain the native delete operator: {}",
        conservative_out.bundle
    );
    assert!(
        !conservative_out.bundle.contains("var __wake_t"),
        "conservative regions must not leak temps into Program: {}",
        conservative_out.bundle
    );
}

#[test]
fn parenthesized_optional_delete_keeps_its_chain_boundary() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        let calls=0;
        const direct={x:1};
        function getDirect(){calls++;return direct}
        const deletedDirect=delete (getDirect()?.x);
        const child={y:2};
        const parent={x:child};
        function getParent(){calls++;return parent}
        const deletedSuffix=delete (getParent()?.x).y;
        let threw=false;
        try{delete (null?.x).y}catch(error){threw=error instanceof TypeError}
        const deletedMissing=delete (null?.x);
        export {calls,direct,parent,child,deletedDirect,deletedSuffix,deletedMissing,threw};
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    // Chrome 70 retains the existing lexical-arrow capture path while still needing optional
    // chaining lowering. Parentheses must not turn `delete member` into `delete conditional`.
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "70"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("?."), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_parenthesized_optional_delete_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});\
         if(r.calls!==2||r.deletedDirect!==true||Object.prototype.hasOwnProperty.call(r.direct,'x')||\
            r.deletedSuffix!==true||!Object.prototype.hasOwnProperty.call(r.parent,'x')||\
            Object.prototype.hasOwnProperty.call(r.child,'y')||r.deletedMissing!==true||!r.threw)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "parenthesized optional delete runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn complex_member_assignments_preserve_evaluation_order() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        const log=[];
        let backing=2;
        const target={
          get value(){log.push("get");return backing},
          set value(v){log.push("set:"+v);backing=v}
        };
        function object(){log.push("object");return target}
        function key(){log.push("key");return "value"}
        object()[key()] **= (log.push("rhs-exp"),3);
        object()[key()] ||= (log.push("rhs-or"),9);
        backing=0;
        object()[key()] ||= (log.push("rhs-or2"),5);
        backing=null;
        object()[key()] ??= (log.push("rhs-null"),7);
        export default JSON.stringify({backing,log});
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "49"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("**="), "{}", out.bundle);
    assert!(!out.bundle.contains("||="), "{}", out.bundle);
    assert!(!out.bundle.contains("??="), "{}", out.bundle);
    assert!(out.bundle.contains("Math.pow"), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_transform_member_assignment");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("bundle.js");
    std::fs::write(
        &path,
        format!("{}\nconsole.log(module.exports.default);", out.bundle),
    )
    .unwrap();
    let output = std::process::Command::new("node")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let expected = r#"{"backing":7,"log":["object","key","get","rhs-exp","set:8","object","key","get","object","key","get","rhs-or2","set:5","object","key","get","rhs-null","set:7"]}"#;
    assert_eq!(stdout.trim(), expected);
}

#[test]
fn chrome_40_member_assignments_use_execution_scope_temporaries() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        "use strict";
        const log=[];
        const results=[];
        let backing=2;
        let current;
        const second={set value(v){log.push("set:second:"+v)}};
        const first={
          get value(){log.push("get:first");current=second;return backing},
          set value(v){log.push("set:first:"+v);backing=v}
        };
        const key={
          [Symbol.toPrimitive](hint){log.push("key:"+hint);return "value"}
        };
        current=first;
        results.push(current[key] **= (log.push("rhs:exp"),3));
        current=first;backing=0;
        results.push(current[key] &&= (log.push("rhs:and"),4));
        current=first;backing=0;
        results.push(current[key] ||= (log.push("rhs:or"),5));
        current=first;backing=null;
        results.push(current[key] ??= (log.push("rhs:null"),7));
        function normal(box){return box.value ||= 9}
        const holder={method(box){return box.value &&= 10}};
        const arrow=box=>{return box.value ??= 11};
        normal({value:1});holder.method({value:1});arrow({value:null});
        export default JSON.stringify({backing,results,log});
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("**="), "{}", out.bundle);
    assert!(!out.bundle.contains("&&="), "{}", out.bundle);
    assert!(!out.bundle.contains("||="), "{}", out.bundle);
    assert!(!out.bundle.contains("??="), "{}", out.bundle);
    assert!(!out.bundle.contains("=>"), "{}", out.bundle);
    assert!(out.bundle.contains("Math.pow"), "{}", out.bundle);
    assert!(
        out.bundle.matches("var __wake_t").count() >= 4,
        "Program, function, method and arrow must own their temporaries: {}",
        out.bundle
    );
    let directive = out.bundle.find("\"use strict\"").unwrap();
    let first_temp = out.bundle.find("var __wake_t").unwrap();
    assert!(directive < first_temp, "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_scoped_member_assignment_chrome40");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("bundle.js");
    std::fs::write(
        &path,
        format!("{}\nconsole.log(module.exports.default);", out.bundle),
    )
    .unwrap();
    let output = std::process::Command::new("node")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = r#"{"backing":7,"results":[8,0,5,7],"log":["key:string","get:first","rhs:exp","set:first:8","key:string","get:first","key:string","get:first","rhs:or","set:first:5","key:string","get:first","rhs:null","set:first:7"]}"#;
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
}

#[test]
fn scoped_member_assignments_preserve_super_private_await_and_yield() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        const log=[];
        const key={
          [Symbol.toPrimitive](hint){log.push("key:"+hint);return "value"}
        };
        const proto={
          get value(){log.push("super-get:"+this.name);return this._value},
          set value(v){log.push("super-set:"+v+":"+this.name);this._value=v}
        };
        const child={
          __proto__:proto,name:"child",_value:null,
          run(){return super[key] ??= (log.push("super-rhs"),4)}
        };
        class Box{
          #value=null;
          update(){return this.#value ??= 5}
        }
        async function asyncUpdate(object){
          return object.value ??= await Promise.resolve(6)
        }
        function* generatorUpdate(object){
          return object.value ??= yield 7
        }
        const superValue=child.run();
        const box=new Box();
        const privateFirst=box.update();
        const privateSecond=box.update();
        const generatorObject={value:null};
        const iterator=generatorUpdate(generatorObject);
        const first=iterator.next();
        const second=iterator.next(8);
        const asyncObject={value:null};
        export default asyncUpdate(asyncObject).then(function(asyncValue){
          return JSON.stringify({
            superValue,childValue:child._value,privateFirst,privateSecond,
            yielded:[first.value,first.done,second.value,second.done],
            generatorValue:generatorObject.value,asyncValue,asyncValueOnObject:asyncObject.value,log
          })
        });
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "80"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("??="), "{}", out.bundle);
    assert!(
        !out.bundle.contains("=>"),
        "member lowering must not synthesize arrows: {}",
        out.bundle
    );
    assert!(out.bundle.contains(".#value"), "{}", out.bundle);
    assert!(
        out.bundle.contains("await Promise.resolve(6)"),
        "{}",
        out.bundle
    );
    assert!(out.bundle.contains("yield 7"), "{}", out.bundle);
    assert!(out.bundle.contains("super["), "{}", out.bundle);
    assert!(out.bundle.contains("var __wake_t"), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_scoped_member_assignment_contexts");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("bundle.js");
    std::fs::write(
        &path,
        format!(
            "{}\nPromise.resolve(module.exports.default).then(function(value){{console.log(value)}},function(error){{console.error(error);process.exitCode=1}});",
            out.bundle
        ),
    )
    .unwrap();
    let output = std::process::Command::new("node")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = r#"{"superValue":4,"childValue":4,"privateFirst":5,"privateSecond":5,"yielded":[7,false,8,true],"generatorValue":8,"asyncValue":6,"asyncValueOnObject":6,"log":["key:string","super-get:child","super-rhs","super-set:4:child"]}"#;
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
}

#[test]
fn member_assignment_scoped_temps_are_gated_and_do_not_leak_from_cover_regions() {
    let modern_fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const box={value:null};box.value ??= 1;export default box.value;",
    )]));
    let mut modern = IncrementalBundler::new(modern_fs);
    modern.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "120"),
    ]));
    let modern_out = modern.build(Path::new("src/index.js"));
    assert!(!modern_out.has_errors(), "{:?}", modern_out.diagnostics);
    assert!(modern_out.bundle.contains("??="), "{}", modern_out.bundle);
    assert!(
        !modern_out.bundle.contains("__wake_t"),
        "{}",
        modern_out.bundle
    );

    let conservative_fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const box={value:null};const covered=(box.value ??= 1);export {covered,box};",
    )]));
    let mut conservative = IncrementalBundler::new(conservative_fs);
    conservative.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let conservative_out = conservative.build(Path::new("src/index.js"));
    assert!(
        !conservative_out.has_errors(),
        "{:?}",
        conservative_out.diagnostics
    );
    assert!(
        conservative_out.bundle.contains("??="),
        "cover grammar has no safe temp scope and must preserve syntax: {}",
        conservative_out.bundle
    );
    assert!(
        !conservative_out.bundle.contains("var __wake_t"),
        "a failed cover transform must not register a Program temp: {}",
        conservative_out.bundle
    );
}

#[test]
fn arrow_functions_lower_and_preserve_lexical_this() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        const add=(a,b)=>a+b;
        const obj={x:4,method(){const read=()=>this.x;return read()}};
        export default add(obj.method(),3);
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("=>"), "{}", out.bundle);
    assert!(out.bundle.contains(".bind(this)"), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_transform_arrow");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("bundle.js");
    std::fs::write(
        &path,
        format!("{}\nconsole.log(module.exports.default);", out.bundle),
    )
    .unwrap();
    let output = std::process::Command::new("node")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "7");
}

#[test]
fn untagged_template_literals_lower_without_numeric_addition() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const x=2; export default `a${x}${3}z`;",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains('`'), "{}", out.bundle);
    assert!(out.bundle.contains(".concat("), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_transform_template");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("bundle.js");
    std::fs::write(
        &path,
        format!("{}\nconsole.log(module.exports.default);", out.bundle),
    )
    .unwrap();
    let output = std::process::Command::new("node")
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "a23z");
}

#[test]
fn shorthand_properties_and_optional_catch_binding_lower() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const x=1; const o={x,m(){return this.x}}; let caught=false; \
         try{throw 1}catch{caught=true} export default o.m()+\":\"+caught;",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(out.bundle.contains("{ x: x, m: function"), "{}", out.bundle);
    assert!(out.bundle.contains("catch (__wake_t"), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_transform_shorthand_catch");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("bundle.js");
    std::fs::write(
        &path,
        format!("{}\nconsole.log(module.exports.default);", out.bundle),
    )
    .unwrap();
    let output = std::process::Command::new("node")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "1:true");
}

#[test]
fn object_methods_with_lexical_super_keep_home_object_on_old_targets() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        const base={key:"picked",value:4,read(value){return this.value+value}};
        const child={
          __proto__:base,
          plain(){return 3},
          arrowOnly(){const read=()=>super.value;return read()},
          run(value=super.value,{[super.key]:picked}={picked:5}){
            return super.read(value)+picked;
          }
        };
        export default child.plain()+":"+child.run()+":"+child.arrowOnly();
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        out.bundle.contains("plain: function"),
        "ordinary methods must still lower: {}",
        out.bundle
    );
    assert!(
        out.bundle.contains("read: function"),
        "ordinary prototype methods must still lower: {}",
        out.bundle
    );
    assert!(
        out.bundle.contains("run(") && !out.bundle.contains("run: function"),
        "a method using lexical super must keep method syntax: {}",
        out.bundle
    );
    assert!(
        out.bundle.contains("arrowOnly(") && !out.bundle.contains("arrowOnly: function"),
        "super inherited by a nested arrow must keep the containing method syntax: {}",
        out.bundle
    );
    assert!(
        out.bundle.contains("super.value")
            && out.bundle.contains("super.key")
            && out.bundle.contains("super.read(value)"),
        "super in defaults, computed binding keys and the body must survive: {}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_object_method_super_transform");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("bundle.js");
    std::fs::write(
        &path,
        format!("{}\nconsole.log(module.exports.default);", out.bundle),
    )
    .unwrap();
    let output = std::process::Command::new("node")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "object method super runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "3:13:4");
}

#[test]
fn default_and_rest_parameters_lower_across_functions_methods_and_arrows() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        function f(a=2,...rest){return [a,rest.join(",")]}
        const obj={m(x=4,...xs){return x+xs.length}};
        const arrow=(x=3,...xs)=>x+xs.length;
        export default JSON.stringify([f(undefined,5,6),obj.m(undefined,1,2),arrow(undefined,8)]);
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("...rest"), "{}", out.bundle);
    assert!(!out.bundle.contains("...xs"), "{}", out.bundle);
    assert!(
        out.bundle.contains("Array.prototype.slice.call(arguments"),
        "{}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_transform_parameters");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("bundle.js");
    std::fs::write(
        &path,
        format!("{}\nconsole.log(module.exports.default);", out.bundle),
    )
    .unwrap();
    let output = std::process::Command::new("node")
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "[[2,\"5,6\"],6,4]"
    );
}

#[test]
fn typescript_bundle_runs_in_node() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("node 不可用，跳过 TS e2e");
        return;
    }
    let out = IncrementalBundler::new(Arc::new(ts_fixture())).build(Path::new("index.ts"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_bundle_ts_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();

    // result = total(2, 3) = add(2, 3) = 5。
    let script = format!(
        "const r = require({:?}); if (r.result !== 5) {{ console.error('result=', r.result); process.exit(2); }} process.stdout.write('OK');",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

/// 一个真实的 JSX（.tsx）项目：入口用 JSX，自带极简 `react/jsx-runtime`（node_modules 解析）。
fn jsx_fixture() -> MemoryFileSystem {
    MemoryFileSystem::from_files([
        (
            "index.tsx",
            "import { render } from './r';\n\
             const el = <div id=\"root\"><span>hi</span>{2 + 3}</div>;\n\
             export const out = render(el);",
        ),
        (
            "r.ts",
            "export function render(n: any): string {\n\
               if (n == null || n === false) return \"\";\n\
               if (typeof n === \"string\" || typeof n === \"number\") return String(n);\n\
               if (Array.isArray(n)) return n.map(render).join(\"\");\n\
               const t = n.type; const p = n.props || {}; const kids = p.children;\n\
               const inner = Array.isArray(kids) ? kids.map(render).join(\"\") : render(kids);\n\
               if (t === \"#frag\") return inner;\n\
               if (typeof t === \"function\") return render(t(p));\n\
               return \"<\" + t + \">\" + inner + \"</\" + t + \">\";\n\
             }",
        ),
        (
            "node_modules/react/jsx-runtime.js",
            "function h(type, props) { return { type: type, props: props || {} }; }\n\
             exports.jsx = h;\n\
             exports.jsxs = h;\n\
             exports.Fragment = \"#frag\";",
        ),
    ])
}

#[test]
fn jsx_project_bundles() {
    let mut bundler = IncrementalBundler::new(Arc::new(jsx_fixture()));
    let out = bundler.build(Path::new("index.tsx"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    // index.tsx + r.ts + react/jsx-runtime = 3 模块（JSX runtime 被自动扇出）。
    assert_eq!(out.module_count, 3, "JSX runtime 应作为依赖进入模块图");
    assert!(out.bundle.contains("_jsx"), "{}", out.bundle);
}

#[test]
fn jsx_bundle_runs_in_node() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("node 不可用，跳过 JSX e2e");
        return;
    }
    let mut bundler = IncrementalBundler::new(Arc::new(jsx_fixture()));
    let out = bundler.build(Path::new("index.tsx"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_bundle_jsx_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();

    // out = render(<div id="root"><span>hi</span>{2+3}</div>) = "<div><span>hi</span>5</div>"
    let script = format!(
        "const r = require({:?}); const want='<div><span>hi</span>5</div>'; if (r.out !== want) {{ console.error('out=', r.out); process.exit(2); }} process.stdout.write('OK');",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn edit_string_literal_updates_bundle() {
    // 回归：只改字符串字面量（不改 AST 结构）时，早期截断不得误判「输出未变」而跳过 codegen。
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "index.js",
        "export const msg = 'HELLO_ORIGINAL';",
    )]));
    let mut b = IncrementalBundler::new(fs.clone());
    let out1 = b.build(Path::new("index.js"));
    assert!(out1.bundle.contains("HELLO_ORIGINAL"));

    fs.insert("index.js", "export const msg = 'HELLO_CHANGED';");
    let out2 = b.build(Path::new("index.js"));
    assert!(
        out2.bundle.contains("HELLO_CHANGED"),
        "字符串字面量改动未反映到产物（早期截断误判）:\n{}",
        out2.bundle
    );
    assert!(
        !out2.bundle.contains("HELLO_ORIGINAL"),
        "残留旧字符串:\n{}",
        out2.bundle
    );
}

#[test]
fn ts_value_transforms_run_in_node() {
    // enum（正/反向）+ namespace（export 成员）+ 参数属性（this 赋值）在 node 真实运行。
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("node 不可用，跳过 TS 值转换 e2e");
        return;
    }
    let fs = MemoryFileSystem::from_files([(
        "index.ts",
        "enum Color { Red, Green, Blue }\n\
         namespace Geo { export const PI = 3; export function area(r: number): number { return PI * r * r; } }\n\
         class Point { constructor(public x: number, public y: number) {} sum(): number { return this.x + this.y; } }\n\
         const p = new Point(3, 4);\n\
         export const result = Color.Blue * 100 + Geo.area(2) + p.sum() + (Color[2] === \"Blue\" ? 1 : 0);",
    )]);
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    let out = bundler.build(Path::new("index.ts"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_bundle_tsvalue_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();

    // Color.Blue=2 →200; Geo.area(2)=3*4=12; p.sum()=7; Color[2]==="Blue" →1; 合计 220。
    let script = format!(
        "const r = require({:?}); if (r.result !== 220) {{ console.error('result=', r.result); process.exit(2); }} process.stdout.write('OK');",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn unresolved_dependency_reports_error() {
    let fs = MemoryFileSystem::from_files([("a.js", "import x from './missing.js';")]);
    let out = Bundler::new(Arc::new(fs)).build(Path::new("a.js"));
    assert!(out.has_errors());
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.message.contains("missing.js")
                && d.path.as_deref().is_some_and(|path| path.ends_with("a.js")))
    );
}

#[test]
fn parser_diagnostic_reports_its_module_path() {
    let fs = MemoryFileSystem::from_files([("broken.tsx", "export const view = <div>;")]);
    let out = Bundler::new(Arc::new(fs)).build(Path::new("broken.tsx"));
    let diagnostic = out
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.is_error())
        .unwrap();
    assert_eq!(diagnostic.path.as_deref(), Some("broken.tsx"));
}

#[test]
fn bundle_runs_in_node() {
    // 需要 node；不可用则跳过（仍保留结构测试覆盖）。
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("node 不可用，跳过 e2e 执行断言");
        return;
    }

    let out = Bundler::new(Arc::new(fixture())).build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_bundle_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();

    // require 产物，断言入口导出的 result === 10（add(2,3)=5 + "hello".length=5）。
    let script = format!(
        "const r = require({:?}); if (r.result !== 10) {{ console.error('result=', r.result); process.exit(2); }} process.stdout.write('OK');",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败\nstatus={:?}\nstdout={}\nstderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

// ============================================================
// JSON / 静态资源导入（Phase 6.4）
// ============================================================

#[test]
fn json_import_runs_in_node() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("node 不可用，跳过 JSON e2e");
        return;
    }
    let fs = MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import cfg from './data.json';\n\
             export const result = cfg.value + cfg.list.length;",
        ),
        ("src/data.json", "{ \"value\": 40, \"list\": [1, 2] }"),
    ]);
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(out.module_count, 2);

    let dir = std::env::temp_dir().join("wake_bundle_json_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();

    // cfg.value(40) + cfg.list.length(2) = 42。
    let script = format!(
        "const r = require({:?}); if (r.result !== 42) {{ console.error('result=', r.result); process.exit(2); }} process.stdout.write('OK');",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn asset_import_inlines_as_data_uri() {
    // PNG 魔数字节 + 少量数据；应内联为 data URI 字符串默认导出。
    let png_bytes: &[u8] = &[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x01, 0x02];
    let fs = MemoryFileSystem::new();
    fs.insert(
        "src/index.js",
        "import logo from './logo.png';\nexport const url = logo;",
    );
    fs.insert("src/logo.png", png_bytes.to_vec());

    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(out.module_count, 2);
    assert!(
        out.bundle.contains("data:image/png;base64,"),
        "资源应内联为 data URI:\n{}",
        out.bundle
    );
}

// ============================================================
// CSS 打包（Phase 6.1 + 6.2 dev 切片）
// ============================================================

/// index.js 导入 styles.css，后者 `@import` base.css。
fn css_fixture() -> MemoryFileSystem {
    MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import './styles.css';\nexport const ok = 1;",
        ),
        (
            "src/styles.css",
            "@import \"./base.css\";\n.title { color: red; }",
        ),
        ("src/base.css", ".base { margin: 0; }"),
    ])
}

#[test]
fn css_import_bundles() {
    let mut bundler = IncrementalBundler::new(Arc::new(css_fixture()));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    // index + styles.css + base.css = 3 模块（@import 建立依赖边）。
    assert_eq!(out.module_count, 3, "CSS @import 应进入模块图");
    // CSS 文本被注入为 JS 字符串。
    assert!(
        out.bundle.contains(".title { color: red; }"),
        "{}",
        out.bundle
    );
    assert!(
        out.bundle.contains(".base { margin: 0; }"),
        "{}",
        out.bundle
    );
    // 生成的样式注入代码。
    assert!(out.bundle.contains("document.createElement(\"style\")"));
}

#[test]
fn crab_component_styles_auto_import_in_dependency_order() {
    let fs = MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import Card from '@crab-dev/rc-card'; export default Card();",
        ),
        (
            "node_modules/@crab-dev/rc-card/package.json",
            r#"{"module":"esm/index.mjs","main":"cjs/index.cjs"}"#,
        ),
        (
            "node_modules/@crab-dev/rc-card/esm/index.mjs",
            "import Skeleton from '@crab-dev/rc-skeleton'; export default function Card() { return Skeleton(); }",
        ),
        (
            "node_modules/@crab-dev/rc-card/css/index.css",
            ".card { color: purple; }",
        ),
        (
            "node_modules/@crab-dev/rc-skeleton/package.json",
            r#"{"module":"esm/index.mjs","main":"cjs/index.cjs"}"#,
        ),
        (
            "node_modules/@crab-dev/rc-skeleton/esm/index.mjs",
            "export default function Skeleton() { return 1; }",
        ),
        (
            "node_modules/@crab-dev/rc-skeleton/css/index.css",
            ".skeleton { color: gray; }",
        ),
    ]);
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    bundler.enable_css_extraction();
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(
        out.module_count, 5,
        "入口、两个组件和两份 CSS 都应进入模块图"
    );
    let css = out
        .assets
        .iter()
        .find(|asset| asset.is_css)
        .map(|asset| String::from_utf8_lossy(&asset.bytes))
        .expect("应生成 CSS 产物");
    let skeleton = css.find(".skeleton").expect("子组件样式");
    let card = css.find(".card").expect("父组件样式");
    assert!(skeleton < card, "依赖样式应先于父组件样式:\n{css}");
}

#[test]
fn css_bundle_runs_in_node() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("node 不可用，跳过 CSS e2e");
        return;
    }
    let mut bundler = IncrementalBundler::new(Arc::new(css_fixture()));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_bundle_css_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();

    // 用 document mock 收集注入的样式；断言 base 先于 title 注入（@import 依赖先行）。
    let script = format!(
        "const styles = [];\n\
         global.document = {{\n\
           head: {{ appendChild(el) {{ styles.push(el.textContent); }} }},\n\
           createElement() {{ return {{ textContent: '' }}; }}\n\
         }};\n\
         require({:?});\n\
         if (styles.length !== 2) {{ console.error('count=', styles.length, styles); process.exit(2); }}\n\
         if (!styles[0].includes('.base')) {{ console.error('order0=', styles[0]); process.exit(3); }}\n\
         if (!styles[1].includes('.title')) {{ console.error('order1=', styles[1]); process.exit(4); }}\n\
         process.stdout.write('OK');",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

// ============================================================
// Tree Shaking（Phase 6.6）
// ============================================================

/// index 只用 util 的 `used`；util 另有未用的 `unused` / `helper`。
fn shake_fixture() -> MemoryFileSystem {
    MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import { used } from './util.js';\nexport const result = used + 1;",
        ),
        (
            "src/util.js",
            "export const used = 10;\n\
             export const unused = 20;\n\
             export function helper() { return 99; }",
        ),
    ])
}

#[test]
fn tree_shaking_removes_unused_exports() {
    let mut bundler = IncrementalBundler::new(Arc::new(shake_fixture()));
    bundler.enable_tree_shaking();
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    // used 保留；unused / helper 被移除。
    assert!(out.bundle.contains("used"), "used 应保留:\n{}", out.bundle);
    assert!(
        !out.bundle.contains("unused"),
        "unused 应被 tree-shaking 移除:\n{}",
        out.bundle
    );
    assert!(
        !out.bundle.contains("helper"),
        "helper 应被 tree-shaking 移除:\n{}",
        out.bundle
    );
}

#[test]
fn tree_shaking_off_keeps_all_exports() {
    // 默认关闭 → 全保留（对照）。
    let mut bundler = IncrementalBundler::new(Arc::new(shake_fixture()));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(out.bundle.contains("unused"), "关闭时应保留 unused");
    assert!(out.bundle.contains("helper"), "关闭时应保留 helper");
}

#[test]
fn tree_shaking_preserves_correctness_in_node() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("node 不可用，跳过 tree-shaking e2e");
        return;
    }
    let mut bundler = IncrementalBundler::new(Arc::new(shake_fixture()));
    bundler.enable_tree_shaking();
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_bundle_shake_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();

    // result = used(10) + 1 = 11——shake 掉 unused/helper 后语义不变。
    let script = format!(
        "const r = require({:?}); if (r.result !== 11) {{ console.error('result=', r.result); process.exit(2); }} process.stdout.write('OK');",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn tree_shaking_keeps_side_effect_module() {
    // 仅副作用导入的模块：其未用导出被剪，但顶层副作用语句保留、模块仍进图。
    let fs = MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import './effect.js';\nexport const ok = 1;",
        ),
        (
            "src/effect.js",
            "globalThis.__hit = true;\nexport const unusedExport = 42;",
        ),
    ]);
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    bundler.enable_tree_shaking();
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(out.module_count, 2, "副作用模块仍进图");
    // 副作用保留；未用导出移除。
    assert!(out.bundle.contains("globalThis.__hit"), "{}", out.bundle);
    assert!(
        !out.bundle.contains("unusedExport"),
        "未用导出应移除:\n{}",
        out.bundle
    );
}

#[test]
fn tree_shaking_second_build_still_cached() {
    // Tree Shaking 开启下，第二遍构建仍应全缓存命中（keep 集稳定 → linker cell no-op）。
    let mut bundler = IncrementalBundler::new(Arc::new(shake_fixture()));
    bundler.enable_tree_shaking();
    let _ = bundler.build(Path::new("src/index.js"));
    let after_first = bundler.task_exec_count();
    let out2 = bundler.build(Path::new("src/index.js"));
    assert!(!out2.has_errors());
    assert_eq!(
        bundler.task_exec_count(),
        after_first,
        "第二遍构建（tree-shaking 开）应 100% 命中缓存"
    );
}

// ============================================================
// CSS Modules（Phase 6.3）
// ============================================================

#[test]
fn css_modules_scopes_and_exports() {
    let fs = MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import styles from './app.module.css';\nexport const cls = styles.title;",
        ),
        (
            "src/app.module.css",
            ".title { color: red; }\n.title:hover { color: blue; }",
        ),
    ]);
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(out.module_count, 2);
    // 作用域名形如 title_xxxxxx；裸 `.title {` 不再出现。
    assert!(
        out.bundle.contains("title_"),
        "应含作用域化类名:\n{}",
        out.bundle
    );
    // 导出映射存在。
    assert!(
        out.bundle.contains("\"title\":"),
        "应导出 title 映射:\n{}",
        out.bundle
    );
}

#[test]
fn css_modules_run_in_node() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("node 不可用，跳过 CSS Modules e2e");
        return;
    }
    let fs = MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import styles from './app.module.css';\n\
             export const title = styles.title;\n\
             export const box = styles['data-box'];",
        ),
        (
            "src/app.module.css",
            ".title { color: red; }\n.data-box { padding: 4px; }",
        ),
    ]);
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_bundle_cssmod_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();

    // styles.title / styles['data-box'] 都应是作用域化字符串（以局部名打头 + 下划线）。
    let script = format!(
        "const styles = [];\n\
         global.document = {{ head: {{ appendChild(el){{ styles.push(el.textContent); }} }}, createElement(){{ return {{ textContent: '' }}; }} }};\n\
         const r = require({:?});\n\
         if (!/^title_[0-9a-f]{{6}}$/.test(r.title)) {{ console.error('title=', r.title); process.exit(2); }}\n\
         if (!/^data-box_[0-9a-f]{{6}}$/.test(r.box)) {{ console.error('box=', r.box); process.exit(3); }}\n\
         if (!styles[0].includes(r.title)) {{ console.error('注入 CSS 未含作用域名'); process.exit(4); }}\n\
         process.stdout.write('OK');",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

// ============================================================
// 代码分割（Phase 6.5）
// ============================================================

/// 把全部 chunk 写盘并返回 entry chunk 的绝对路径（供 node require）。
fn write_chunks(out: &crate::BuildOutput, dir: &Path) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    for c in &out.chunks {
        std::fs::write(dir.join(&c.file_name), &c.code).unwrap();
    }
    dir.join(&out.entry().file_name)
}

fn node_available() -> bool {
    std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_ok()
}

/// Fixture A：入口动态 import lazy。
fn split_fixture() -> MemoryFileSystem {
    MemoryFileSystem::from_files([
        (
            "src/index.js",
            "export const eager = 1;\n\
             export async function load() { const m = await import('./lazy.js'); return m.value + m.helper(); }",
        ),
        (
            "src/lazy.js",
            "export const value = 40;\nexport function helper() { return 2; }",
        ),
    ])
}

#[test]
fn code_splitting_emits_async_chunk() {
    let mut b = IncrementalBundler::new(Arc::new(split_fixture()));
    b.enable_code_splitting();
    let out = b.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(out.chunks.len(), 2, "entry + async");
    assert!(out.entry().is_entry && out.entry().chunk_id == 0);
    assert!(
        out.bundle.contains("__wake_require__.import("),
        "entry 应发出懒加载:\n{}",
        out.bundle
    );
    assert!(out.bundle.contains("__wake__.f = "), "{}", out.bundle);
    assert!(
        !out.bundle.contains("exports[\"value\"] = value;"),
        "lazy 不应在 entry chunk:\n{}",
        out.bundle
    );
    let async_chunk = out.chunks.iter().find(|c| !c.is_entry).unwrap();
    assert!(async_chunk.code.contains("__wake__.register("));
    assert!(async_chunk.code.contains("exports[\"value\"] = value;"));
    assert!(
        !async_chunk.code.contains(".js"),
        "async chunk 体不应含文件名:\n{}",
        async_chunk.code
    );
    assert!(
        out.bundle.contains(&async_chunk.file_name),
        "entry.f 应含 async 文件名 {}",
        async_chunk.file_name
    );
}

#[test]
fn code_splitting_second_build_cached() {
    let mut b = IncrementalBundler::new(Arc::new(split_fixture()));
    b.enable_code_splitting();
    let _ = b.build(Path::new("src/index.js"));
    let after_first = b.task_exec_count();
    assert_eq!(after_first, 4, "2 parse + 2 codegen");
    let _ = b.build(Path::new("src/index.js"));
    assert_eq!(
        b.task_exec_count(),
        after_first,
        "分割不应破坏增量缓存（第二遍全命中）"
    );
}

#[test]
fn code_splitting_lazy_loads_in_node() {
    if !node_available() {
        eprintln!("node 不可用，跳过代码分割 e2e");
        return;
    }
    let mut b = IncrementalBundler::new(Arc::new(split_fixture()));
    b.enable_code_splitting();
    let out = b.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_split_basic_e2e");
    let _ = std::fs::remove_dir_all(&dir);
    let entry = write_chunks(&out, &dir);

    let script = format!(
        "const app = require({:?});\n\
         if (app.eager !== 1) {{ console.error('eager=', app.eager); process.exit(4); }}\n\
         app.load().then(function(v) {{ if (v !== 42) {{ console.error('v=', v); process.exit(2); }} process.stdout.write('OK'); }})\n\
                   .catch(function(e) {{ console.error(e); process.exit(3); }});",
        entry.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}\n--- entry ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

/// Fixture B：入口动态 import a、b；a、b 都静态依赖 shared。
fn shared_fixture() -> MemoryFileSystem {
    MemoryFileSystem::from_files([
        (
            "src/index.js",
            "export async function loadA(){ const m = await import('./a.js'); return m.run(); }\n\
             export async function loadB(){ const m = await import('./b.js'); return m.run(); }",
        ),
        (
            "src/a.js",
            "import { bump } from './shared.js';\nexport function run(){ return 'A' + bump(); }",
        ),
        (
            "src/b.js",
            "import { bump } from './shared.js';\nexport function run(){ return 'B' + bump(); }",
        ),
        (
            "src/shared.js",
            "export let count = 0;\nexport function bump(){ count++; return count; }",
        ),
    ])
}

#[test]
fn shared_chunk_extracted() {
    let mut b = IncrementalBundler::new(Arc::new(shared_fixture()));
    b.enable_code_splitting();
    let out = b.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(out.chunks.len(), 4, "entry + a + b + shared");
    let n = |k: crate::ChunkKind| out.chunks.iter().filter(|c| c.kind == k).count();
    assert_eq!(n(crate::ChunkKind::Initial), 1);
    assert_eq!(n(crate::ChunkKind::Async), 2);
    assert_eq!(n(crate::ChunkKind::Shared), 1);
    let shared = out
        .chunks
        .iter()
        .find(|c| c.kind == crate::ChunkKind::Shared)
        .unwrap();
    let asyncs: Vec<_> = out
        .chunks
        .iter()
        .filter(|c| c.kind == crate::ChunkKind::Async)
        .collect();
    for a in &asyncs {
        assert!(
            a.imports.contains(&shared.file_name),
            "async chunk {} 应依赖 shared {}",
            a.name,
            shared.file_name
        );
    }
}

#[test]
fn shared_chunk_singleton_in_node() {
    if !node_available() {
        eprintln!("node 不可用，跳过 shared chunk e2e");
        return;
    }
    let mut b = IncrementalBundler::new(Arc::new(shared_fixture()));
    b.enable_code_splitting();
    let out = b.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_split_shared_e2e");
    let _ = std::fs::remove_dir_all(&dir);
    let entry = write_chunks(&out, &dir);

    let script = format!(
        "const api = require({:?});\n\
         (async function() {{\n\
           const a = await api.loadA();\n\
           const b = await api.loadB();\n\
           if (a !== 'A1' || b !== 'B2') {{ console.error('a=', a, 'b=', b); process.exit(2); }}\n\
           process.stdout.write('OK');\n\
         }})().catch(function(e) {{ console.error(e); process.exit(3); }});",
        entry.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn concurrent_dynamic_import_dedup_in_node() {
    if !node_available() {
        eprintln!("node 不可用，跳过并发去重 e2e");
        return;
    }
    let fs = MemoryFileSystem::from_files([
        (
            "src/index.js",
            "export function run(){ return Promise.all([import('./lazy.js'), import('./lazy.js')]).then(function(r){ return [r[0].value, globalThis.__c]; }); }",
        ),
        (
            "src/lazy.js",
            "globalThis.__c = (globalThis.__c || 0) + 1;\nexport const value = 40;",
        ),
    ]);
    let mut b = IncrementalBundler::new(Arc::new(fs));
    b.enable_code_splitting();
    let out = b.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_split_dedup_e2e");
    let _ = std::fs::remove_dir_all(&dir);
    let entry = write_chunks(&out, &dir);

    let script = format!(
        "const api = require({:?});\n\
         api.run().then(function(r) {{ if (r[0] !== 40 || r[1] !== 1) {{ console.error('r=', r); process.exit(2); }} process.stdout.write('OK'); }})\n\
                  .catch(function(e) {{ console.error(e); process.exit(3); }});",
        entry.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn inline_import_when_target_in_entry() {
    // 目标既被静态又被动态 import → 已在 entry 闭包 → 退回单包，import() 内联。
    let fs = MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import { v } from './shared.js';\n\
             export async function get(){ const m = await import('./shared.js'); return m.v + v; }",
        ),
        ("src/shared.js", "export const v = 21;"),
    ]);
    let mut b = IncrementalBundler::new(Arc::new(fs));
    b.enable_code_splitting();
    let out = b.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(out.chunks.len(), 1, "目标在 entry 闭包 → 单包");
    assert!(
        out.bundle.contains("Promise.resolve(__wake_require__("),
        "应内联而非切 chunk:\n{}",
        out.bundle
    );
}

#[test]
fn no_dynamic_import_single_chunk() {
    // 无动态 import 开分割 → 单 chunk、bundle.js、与不开分割逐字节一致。
    let mut split = IncrementalBundler::new(Arc::new(fixture()));
    split.enable_code_splitting();
    let out_split = split.build(Path::new("src/index.js"));
    assert_eq!(out_split.chunks.len(), 1);
    assert_eq!(out_split.entry().file_name, "bundle.js");

    let mut plain = IncrementalBundler::new(Arc::new(fixture()));
    let out_plain = plain.build(Path::new("src/index.js"));
    assert_eq!(
        out_split.bundle, out_plain.bundle,
        "无动态 import 时分割应逐字节等于单包"
    );
}

// ============================================================
// 持久化构建缓存（PLAN §7.1）——全新进程冷构建跳过未变模块的 parse + codegen
// ============================================================

/// 造一个和 `fixture` 结构一致、但可改 msg 内容的 fs（用于失效测试）。
fn fixture_with_msg(msg: &str) -> MemoryFileSystem {
    MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import { add } from './math.js';\n\
             import msg from './msg.js';\n\
             export const result = add(2, 3) + msg.length;",
        ),
        ("src/math.js", "export function add(a, b) { return a + b; }"),
        ("src/msg.js", msg),
    ])
}

#[test]
fn persistent_cache_skips_parse_and_codegen_on_fresh_process() {
    let cache_path = std::env::temp_dir().join("wake_pcache_fresh.bin");
    let _ = std::fs::remove_file(&cache_path);

    // 参照：无缓存产物。
    let ref_out = IncrementalBundler::new(Arc::new(fixture())).build(Path::new("src/index.js"));
    assert!(!ref_out.has_errors());

    // 首遍（冷缓存）：正常 parse+codegen，落盘缓存；产物须与无缓存逐字节一致。
    let mut b1 = IncrementalBundler::new(Arc::new(fixture()));
    b1.enable_persistent_cache(cache_path.clone());
    let out1 = b1.build(Path::new("src/index.js"));
    assert!(!out1.has_errors());
    assert_eq!(out1.bundle, ref_out.bundle, "开缓存首遍须与无缓存产物一致");
    assert_eq!(b1.task_exec_count(), 6, "首遍冷缓存：3 parse + 3 codegen");

    // 第二遍：**全新 bundler（新引擎，内存 memo 为空）**，从磁盘载入热缓存。
    let mut b2 = IncrementalBundler::new(Arc::new(fixture()));
    b2.enable_persistent_cache(cache_path.clone());
    let out2 = b2.build(Path::new("src/index.js"));
    assert!(!out2.has_errors());
    assert_eq!(out2.bundle, ref_out.bundle, "热缓存新进程产物须逐字节一致");
    assert_eq!(
        b2.task_exec_count(),
        0,
        "热缓存：parse + codegen 全部跳过（磁盘命中），零引擎任务"
    );

    let _ = std::fs::remove_file(&cache_path);
}

#[test]
fn persistent_path_index_skips_unchanged_os_source_reads() {
    let unique = format!(
        "wake_path_index_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    );
    let root = std::env::temp_dir().join(unique);
    std::fs::create_dir_all(&root).unwrap();
    let entry = root.join("index.js");
    let dep = root.join("dep.js");
    std::fs::write(
        &entry,
        "import { value } from './dep.js'; export default value;",
    )
    .unwrap();
    std::fs::write(&dep, "export const value = 42;").unwrap();
    let cache_path = root.join("cache.bin");
    let mut b1 = IncrementalBundler::new(Arc::new(OsFileSystem));
    b1.enable_persistent_cache(cache_path.clone());
    let out1 = b1.build(&entry);
    assert!(!out1.has_errors(), "{:?}", out1.diagnostics);
    assert_eq!(b1.load_exec_count(), 2);
    let mut b2 = IncrementalBundler::new(Arc::new(OsFileSystem));
    b2.enable_persistent_cache(cache_path.clone());
    let out2 = b2.build(&entry);
    assert!(!out2.has_errors(), "{:?}", out2.diagnostics);
    assert_eq!(out2.bundle, out1.bundle);
    assert_eq!(b2.load_exec_count(), 0, "热缓存应只 stat，不重新读取源码");
    std::fs::write(&dep, "export const value = 4300;").unwrap();
    let mut b3 = IncrementalBundler::new(Arc::new(OsFileSystem));
    b3.enable_persistent_cache(cache_path);
    let out3 = b3.build(&entry);
    assert!(!out3.has_errors(), "{:?}", out3.diagnostics);
    assert_eq!(b3.load_exec_count(), 1, "只有变化的依赖需要重新读取");
    assert!(out3.bundle.contains("4300"), "产物必须反映源码变化");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn persistent_cache_invalidates_on_content_change() {
    let cache_path = std::env::temp_dir().join("wake_pcache_invalidate.bin");
    let _ = std::fs::remove_file(&cache_path);

    // 用原内容填充缓存。
    let mut b1 = IncrementalBundler::new(Arc::new(fixture_with_msg("export default 'hello';")));
    b1.enable_persistent_cache(cache_path.clone());
    let out1 = b1.build(Path::new("src/index.js"));
    assert!(!out1.has_errors());

    // 全新 bundler，msg.js 内容改了，载入同一缓存文件。
    let mut b2 =
        IncrementalBundler::new(Arc::new(fixture_with_msg("export default 'HELLO_WORLD';")));
    b2.enable_persistent_cache(cache_path.clone());
    let out2 = b2.build(Path::new("src/index.js"));
    assert!(!out2.has_errors());

    // 改动模块 content_key 变 → 摘要+产物均未命中 → 重 parse + 重 codegen（>0 任务）。
    // math.js / index.js 未变仍命中，故不是全量。
    assert!(
        b2.task_exec_count() > 0,
        "改了 msg.js 至少要重算它的 parse+codegen"
    );
    // 产物反映新内容。
    assert!(
        out2.bundle.contains("HELLO_WORLD"),
        "产物须含改后的字符串: {}",
        out2.bundle
    );
    // 与「无缓存直接构建改后内容」逐字节一致（证明缓存不产出陈旧结果）。
    let fresh =
        IncrementalBundler::new(Arc::new(fixture_with_msg("export default 'HELLO_WORLD';")))
            .build(Path::new("src/index.js"));
    assert_eq!(
        out2.bundle, fresh.bundle,
        "热缓存+改动的产物须等于无缓存重建"
    );

    let _ = std::fs::remove_file(&cache_path);
}

#[test]
fn persistent_cache_preserves_tree_shaking_output() {
    let unique = format!(
        "wake_pcache_tree_shaking_{}_{}.bin",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间应晚于 Unix epoch")
            .as_nanos()
    );
    let cache_path = std::env::temp_dir().join(unique);

    let mut cold = IncrementalBundler::new(Arc::new(shake_fixture()));
    cold.enable_tree_shaking()
        .enable_minify()
        .enable_persistent_cache(cache_path.clone());
    let cold_out = cold.build(Path::new("src/index.js"));
    assert!(!cold_out.has_errors(), "{:?}", cold_out.diagnostics);

    let mut warm = IncrementalBundler::new(Arc::new(shake_fixture()));
    warm.enable_tree_shaking()
        .enable_minify()
        .enable_persistent_cache(cache_path.clone());
    let warm_out = warm.build(Path::new("src/index.js"));
    assert!(!warm_out.has_errors(), "{:?}", warm_out.diagnostics);

    assert_eq!(
        warm_out.bundle, cold_out.bundle,
        "持久化缓存冷热状态不得改变 tree-shaking 产物"
    );
    assert_eq!(warm_out.chunks.len(), cold_out.chunks.len());
    assert_eq!(warm_out.assets.len(), cold_out.assets.len());
    assert_eq!(
        warm.task_exec_count(),
        0,
        "tree-shaking 热缓存应同时跳过 parse 与 codegen"
    );

    let _ = std::fs::remove_file(cache_path);
}

// ============================================================
// Yarn PnP（Plug'n'Play）解析（hermetic：内存 FS 模拟 PnP 项目，不依赖真实 Yarn 缓存）
// ============================================================

/// 构造一个模拟 PnP 项目的内存 FS：
/// - `.pnp.cjs` 内嵌 `RAW_RUNTIME_STATE`（依赖图）；
/// - 包体以普通目录留在 `.pnp-store/`（无 zip，聚焦解析算法 + 虚拟路径）；
/// - `dep-b` 走**虚拟路径**（`.yarn/__virtual__/…/1/…`），验证 `resolveVirtual` 经
///   [`PnpFileSystem`] 透明命中 —— depth=1 恰好把 `.yarn` 抵消回 cwd。
fn pnp_fixture() -> MemoryFileSystem {
    // 依赖图：顶层依赖 dep-a、dep-b（虚拟）；dep-a 传递依赖 dep-c。
    let pnp_data = r#"{
        "enableTopLevelFallback": true,
        "fallbackExclusionList": [],
        "fallbackPool": [],
        "packageRegistryData": [
            [null, [[null, {
                "packageLocation": "./",
                "packageDependencies": [
                    ["dep-a", "npm:1.0.0"],
                    ["dep-b", "virtual:hb#npm:1.0.0"]
                ],
                "linkType": "SOFT"
            }]]],
            ["dep-a", [["npm:1.0.0", {
                "packageLocation": "./.pnp-store/dep-a/",
                "packageDependencies": [["dep-a", "npm:1.0.0"], ["dep-c", "npm:1.0.0"]],
                "linkType": "HARD"
            }]]],
            ["dep-b", [
                ["npm:1.0.0", {
                    "packageLocation": "./.pnp-store/dep-b/",
                    "packageDependencies": [["dep-b", "npm:1.0.0"]],
                    "linkType": "HARD"
                }],
                ["virtual:hb#npm:1.0.0", {
                    "packageLocation": "./.yarn/__virtual__/dep-b-virtual-hb/1/.pnp-store/dep-b/",
                    "packageDependencies": [["dep-b", "virtual:hb#npm:1.0.0"]],
                    "linkType": "HARD"
                }]
            ]],
            ["dep-c", [["npm:1.0.0", {
                "packageLocation": "./.pnp-store/dep-c/",
                "packageDependencies": [["dep-c", "npm:1.0.0"]],
                "linkType": "HARD"
            }]]]
        ]
    }"#;
    // JSON 无单引号/反斜杠，可直接内联进 JS 单引号字符串。
    let pnp_cjs = format!("#!/usr/bin/env node\nconst RAW_RUNTIME_STATE =\n'{pnp_data}';\n");

    MemoryFileSystem::from_files([
        (".pnp.cjs".to_string(), pnp_cjs),
        (
            "src/index.js".to_string(),
            "import { a } from \"dep-a\";\n\
             import { sub } from \"dep-b/sub.js\";\n\
             export const result = a() + sub();"
                .to_string(),
        ),
        (
            ".pnp-store/dep-a/package.json".to_string(),
            r#"{"name":"dep-a","main":"main.js"}"#.to_string(),
        ),
        (
            ".pnp-store/dep-a/main.js".to_string(),
            "import { c } from \"dep-c\";\nexport function a() { return c() + 1; }".to_string(),
        ),
        (
            // dep-c 无 package.json → 走目录 index 解析。
            ".pnp-store/dep-c/index.js".to_string(),
            "export function c() { return 40; }".to_string(),
        ),
        (
            ".pnp-store/dep-b/sub.js".to_string(),
            "export function sub() { return 1; }".to_string(),
        ),
    ])
}

#[test]
fn pnp_project_resolves_via_manifest() {
    let mut bundler = IncrementalBundler::new(Arc::new(pnp_fixture()));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(bundler.is_pnp(), "应检测到 .pnp.cjs 并启用 PnP");
    assert!(!out.has_errors(), "PnP 解析应无错: {:?}", out.diagnostics);
    // index + dep-a + dep-c + dep-b/sub = 4 模块（全部经 PnP 依赖图定位，无 node_modules）。
    assert_eq!(out.module_count, 4, "应打包 4 个模块");
    assert!(out.bundle.contains("__wake_require__"));
}

#[test]
fn pnp_second_build_is_fully_cached() {
    // PnP 只探测一次；第二遍构建全管线缓存命中（parse+codegen 零重执行）。
    let mut bundler = IncrementalBundler::new(Arc::new(pnp_fixture()));
    let _ = bundler.build(Path::new("src/index.js"));
    let after_first = bundler.task_exec_count();
    let _ = bundler.build(Path::new("src/index.js"));
    assert_eq!(
        bundler.task_exec_count(),
        after_first,
        "第二遍不应新增任务执行（缓存命中）"
    );
}

#[test]
fn pnp_bundle_runs_in_node() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("node 不可用，跳过 PnP e2e");
        return;
    }
    let mut bundler = IncrementalBundler::new(Arc::new(pnp_fixture()));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_bundle_pnp_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();

    // result = a()(c()+1 = 41) + sub()(1) = 42。dep-c 经 dep-a 传递解析、dep-b 经虚拟路径。
    let script = format!(
        "const r = require({:?}); if (r.result !== 42) {{ console.error('result=', r.result); process.exit(2); }} process.stdout.write('OK');",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

// ======================================================================
// M4d — bundle 级 SourceMap
// ======================================================================

/// 解码 Base64 VLQ 段序列 → `(gen_line, gen_col, src_index, src_line, src_col)` 列表。
/// 测试自带解码器：不引第三方依赖也能独立验证编码正确性（避免"自己编自己解"的同义反复，
/// 解码逻辑按 Source Map V3 规范独立实现）。
fn decode_mappings(s: &str) -> Vec<(u32, u32, u32, u32, u32)> {
    fn b64(c: u8) -> i64 {
        match c {
            b'A'..=b'Z' => (c - b'A') as i64,
            b'a'..=b'z' => (c - b'a') as i64 + 26,
            b'0'..=b'9' => (c - b'0') as i64 + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("非法 base64 字符: {}", c as char),
        }
    }
    let mut out = Vec::new();
    let (mut gl, mut si, mut sl, mut sc) = (0i64, 0i64, 0i64, 0i64);
    for line in s.split(';') {
        // 产物列每行归零（规范：仅此字段跨行重置，其余三者全局连续差分）
        let mut gc = 0i64;
        if line.is_empty() {
            gl += 1;
            continue;
        }
        for seg in line.split(',') {
            if seg.is_empty() {
                continue;
            }
            let bytes = seg.as_bytes();
            let mut vals = Vec::new();
            let mut i = 0;
            while i < bytes.len() {
                let (mut result, mut shift) = (0i64, 0);
                loop {
                    let d = b64(bytes[i]);
                    i += 1;
                    result |= (d & 0x1f) << shift;
                    shift += 5;
                    if d & 0x20 == 0 {
                        break;
                    }
                }
                // 最低位是符号位
                let neg = result & 1 == 1;
                let v = result >> 1;
                vals.push(if neg { -v } else { v });
            }
            assert_eq!(vals.len(), 4, "本实现只发 4 字段段: {seg}");
            gc += vals[0];
            si += vals[1];
            sl += vals[2];
            sc += vals[3];
            out.push((gl as u32, gc as u32, si as u32, sl as u32, sc as u32));
        }
        gl += 1;
    }
    out
}

/// 从 map JSON 里取一个字符串数组字段（简易提取，仅用于测试断言）。
fn json_field<'a>(json: &'a str, key: &str) -> &'a str {
    let pat = format!("\"{key}\":");
    let start = json
        .find(&pat)
        .unwrap_or_else(|| panic!("缺字段 {key}: {json}"))
        + pat.len();
    &json[start..]
}

/// 端到端：bundle 级映射必须把产物位置正确指回**各自源文件**的对应 token。
#[test]
fn sourcemap_bundle_maps_back_to_original_sources() {
    let mut bundler = IncrementalBundler::new(Arc::new(fixture()));
    bundler.enable_sourcemap();
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let chunk = out.entry();
    let map = chunk
        .source_map
        .as_ref()
        .expect("启用 sourcemap 后入口 chunk 应带 map");

    assert!(map.contains("\"version\":3"), "须为 V3: {map}");
    // 三个源文件都应登记，且路径用正斜杠
    for f in ["src/index.js", "src/math.js", "src/msg.js"] {
        assert!(map.contains(f), "map 应含源文件 {f}: {map}");
    }

    // 取出 mappings 串并解码
    let m = json_field(map, "mappings");
    let mappings_str = m
        .trim_start_matches('"')
        .split('"')
        .next()
        .expect("mappings 值");
    let decoded = decode_mappings(mappings_str);
    assert!(!decoded.is_empty(), "应有映射: {map}");

    // 源文件内容（与 fixture 一致），按 map 中 sources 的顺序对齐
    let contents: std::collections::HashMap<&str, &str> = [
        (
            "src/index.js",
            "import { add } from './math.js';\n\
             import msg from './msg.js';\n\
             export const result = add(2, 3) + msg.length;",
        ),
        ("src/math.js", "export function add(a, b) { return a + b; }"),
        ("src/msg.js", "export default 'hello';"),
    ]
    .into_iter()
    .collect();

    // sources 数组顺序：按 map JSON 中出现次序解析
    let src_list = json_field(map, "sources");
    let src_names: Vec<&str> = src_list
        .trim_start_matches('[')
        .split(']')
        .next()
        .unwrap()
        .split(',')
        .map(|s| s.trim().trim_matches('"'))
        .collect();

    let bundle_lines: Vec<&str> = chunk.code.lines().collect();
    let ident_of = |s: &str| -> String {
        s.chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect()
    };

    let mut checked = 0;
    for (gl, gc, si, sl, sc) in &decoded {
        let name = src_names
            .get(*si as usize)
            .unwrap_or_else(|| panic!("源下标 {si} 越界: {src_names:?}"));
        let content = contents
            .get(name)
            .unwrap_or_else(|| panic!("未知源文件 {name}"));

        // 产物侧 token
        let gline = bundle_lines
            .get(*gl as usize)
            .unwrap_or_else(|| panic!("产物行 {gl} 越界"));
        let gtok = ident_of(&gline[(*gc as usize).min(gline.len())..]);

        // 源侧 token（按行列定位）
        let sline = content.lines().nth(*sl as usize).unwrap_or("");
        let stok = ident_of(&sline[(*sc as usize).min(sline.len())..]);

        // 关键字两侧可**合法地不同**：ESM→CJS 链接把 `import ... from 'x'` 改写成
        // `const _wm0 = __wake_require__(1)`，此时产物 `const` 映射到源 `import` 是正确的。
        // 故只对**非关键字标识符**做等值比对——这些 token 经转换后应原样保留。
        const KEYWORDS: &[&str] = &[
            "import",
            "export",
            "const",
            "let",
            "var",
            "function",
            "return",
            "default",
            "class",
            "new",
            "this",
            "typeof",
            "void",
            "in",
            "of",
            "from",
            "await",
            "async",
            "true",
            "false",
            "null",
            "undefined",
        ];
        let is_kw = |t: &str| KEYWORDS.contains(&t);
        if !gtok.is_empty() && !stok.is_empty() && !is_kw(&gtok) && !is_kw(&stok) {
            assert_eq!(
                gtok, stok,
                "bundle 映射错位：产物 ({gl},{gc}) 是 `{gtok}`，\
                 但映射指向 {name} ({sl},{sc}) 处的 `{stok}`"
            );
            checked += 1;
        }
    }
    assert!(checked >= 5, "有效比对过少（{checked}），映射可能未生效");
}

/// minify 路径不得产出（错位的）map——宁缺毋错。
#[test]
fn sourcemap_absent_under_minify() {
    let mut bundler = IncrementalBundler::new(Arc::new(fixture()));
    bundler.enable_sourcemap();
    bundler.enable_minify();
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        out.entry().source_map.is_none(),
        "minify 路径会改写模块体文本，不应产出错位的 map"
    );
}

/// 不启用时不产 map，且产物与启用前**逐字节相同**（sourcemap 不得影响产物）。
#[test]
fn sourcemap_opt_in_does_not_change_bundle() {
    let plain = IncrementalBundler::new(Arc::new(fixture())).build(Path::new("src/index.js"));
    assert!(plain.entry().source_map.is_none());

    let mut b2 = IncrementalBundler::new(Arc::new(fixture()));
    b2.enable_sourcemap();
    let mapped = b2.build(Path::new("src/index.js"));

    assert_eq!(plain.bundle, mapped.bundle, "启用 sourcemap 不得改变产物");
    assert!(mapped.entry().source_map.is_some());
}

/// `sources` 路径规整：去 Windows `\\?\` 扩展长度前缀、相对化、统一正斜杠。
#[test]
fn sourcemap_source_names_are_normalized() {
    use crate::incremental::map_source_name;

    // 扩展长度前缀必须去掉（否则泄漏成 `//?/C:/…`，DevTools 无法定位源文件）
    let p = Path::new(r"\\?\C:\proj\src\a.js");
    let cwd = Path::new(r"\\?\C:\proj");
    assert_eq!(map_source_name(p, Some(cwd)), "src/a.js");

    // cwd 未加前缀、路径加了前缀（dev server 的真实情形）也应相对化
    assert_eq!(map_source_name(p, Some(Path::new(r"C:\proj"))), "src/a.js");

    // 相对化不能依赖 runner 的宿主路径语义；UNC 与普通 Unix 路径都应稳定。
    assert_eq!(
        map_source_name(
            Path::new(r"\\?\UNC\server\share\proj\src\a.js"),
            Some(Path::new(r"\\server\share\proj")),
        ),
        "src/a.js"
    );
    assert_eq!(
        map_source_name(Path::new("/proj/src/a.js"), Some(Path::new("/proj"))),
        "src/a.js"
    );

    // 只允许在路径段边界相对化，不能把 `project` 当成 `proj` 的子路径。
    assert_eq!(
        map_source_name(
            Path::new(r"C:\project\src\a.js"),
            Some(Path::new(r"C:\proj")),
        ),
        "C:/project/src/a.js"
    );

    // 不在 cwd 之下 → 保留绝对路径，但仍去前缀并转正斜杠
    let outside = map_source_name(p, Some(Path::new(r"C:\other")));
    assert!(
        !outside.contains(r"\\?\") && !outside.starts_with("//?/"),
        "不应残留 verbatim 前缀: {outside}"
    );
    assert!(!outside.contains('\\'), "应统一正斜杠: {outside}");
    assert!(outside.ends_with("src/a.js"), "{outside}");

    // 无 cwd 时不 panic，且同样去前缀
    let no_cwd = map_source_name(p, None);
    assert!(!no_cwd.starts_with("//?/"), "{no_cwd}");
}

// ======================================================================
// M5 — 零运行时 CSS-in-JS（Linaria 子集）
// ======================================================================

/// `@linaria/core` 桩包：真实项目里这是已安装的依赖。`css` 会被构建期完全消解，
/// 但同包的 `cx`（类名拼接）仍是运行时函数，故不能整体外部化——保留正常解析。
const LINARIA_PKG_JSON: &str = r#"{"name":"@linaria/core","version":"6.0.0","main":"index.js"}"#;
const LINARIA_INDEX: &str = "export const css = () => { throw new Error('css should be compiled away'); };\n\
     export const cx = (...a) => a.filter(Boolean).join(' ');\n";

/// crab-dev 的真实形态：token 模块导出嵌套对象，组件模块用 `${token.a.b}` 插值。
fn linaria_fixture() -> MemoryFileSystem {
    MemoryFileSystem::from_files([
        ("node_modules/@linaria/core/package.json", LINARIA_PKG_JSON),
        ("node_modules/@linaria/core/index.js", LINARIA_INDEX),
        (
            "src/index.tsx",
            "import { css } from '@linaria/core';\n\
             import token from './token.js';\n\
             const box = css`\n\
             \x20 display: flex;\n\
             \x20 padding: ${token.container.padding};\n\
             \x20 &:hover { color: red; }\n\
             `;\n\
             export default box;",
        ),
        (
            "src/token.js",
            "const vars = { 'container.padding': '--c-pad' };\n\
             const token = { container: { padding: `var(${vars['container.padding']}, 24px)` } };\n\
             export default token;",
        ),
    ])
}

#[test]
fn css_in_js_extracts_styles_and_replaces_with_class() {
    let mut b = IncrementalBundler::new(Arc::new(linaria_fixture()));
    b.enable_css_in_js();
    b.enable_css_extraction();
    let out = b.build(Path::new("src/index.tsx"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    // 1) 产出了 CSS 资源
    let css_asset = out
        .assets
        .iter()
        .find(|a| a.is_css)
        .expect("应产出抽取的 CSS");
    let css = String::from_utf8(css_asset.bytes.clone()).unwrap();

    // 2) 跨模块 token 求值成功（`var(--c-pad, 24px)`）
    assert!(
        css.contains("padding: var(--c-pad, 24px)"),
        "跨模块 design token 应被求值: {css}"
    );
    assert!(css.contains("display: flex"), "{css}");

    // 3) 嵌套被展开为独立规则
    assert!(css.contains(":hover{color: red;}"), "嵌套未展开: {css}");

    // 4) bundle 里标签模板已被类名字符串替换，不再有 css`` 调用
    assert!(
        !out.bundle.contains("css`"),
        "标签模板应已在构建期消解: {}",
        &out.bundle[..out.bundle.len().min(600)]
    );
    // 类名以变量名 box 为前缀，且 CSS 与 JS 用的是同一个类名
    let cls = css
        .split('{')
        .next()
        .unwrap()
        .trim_start_matches('.')
        .to_string();
    assert!(cls.starts_with("box_"), "类名应含变量名前缀: {cls}");
    assert!(
        out.bundle.contains(&format!("\"{cls}\"")),
        "bundle 应引用同一类名 {cls}"
    );
}

#[test]
fn css_in_js_lowers_safe_cx_and_eliminates_linaria_runtime_module() {
    let fs = MemoryFileSystem::from_files([
        ("node_modules/@linaria/core/package.json", LINARIA_PKG_JSON),
        ("node_modules/@linaria/core/index.js", LINARIA_INDEX),
        (
            "src/index.tsx",
            "import { css, cx as merge } from '@linaria/core';\n\
             const base = css`color:red;`;\n\
             const active = css`font-weight:bold;`;\n\
             const enabled = false;\n\
             export const className = merge(base, enabled && active);",
        ),
    ]);
    let mut b = IncrementalBundler::new(Arc::new(fs));
    b.enable_css_in_js();
    b.enable_css_extraction();
    b.enable_dead_module_elimination();
    b.enable_minify();
    b.enable_mangle();
    let out = b.build(Path::new("src/index.tsx"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        !out.bundle.contains("css should be compiled away"),
        "已完全消解的 @linaria/core 不应进入产物: {}",
        out.bundle
    );
    assert!(!out.bundle.contains("@linaria/core"), "{}", out.bundle);
    assert!(out.bundle.contains("filter(Boolean).join(\" \")"));
    assert_eq!(out.module_count, 1, "Linaria 模块应被 DME 删除");
}

#[test]
fn css_in_js_dev_injects_style_tag() {
    // dev（不开抽取）：CSS 随模块体以 <style> 注入，不产 .css 资源
    let mut b = IncrementalBundler::new(Arc::new(linaria_fixture()));
    b.enable_css_in_js();
    let out = b.build(Path::new("src/index.tsx"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    assert!(
        out.assets.iter().all(|a| !a.is_css),
        "dev 不应产出 .css 资源"
    );
    assert!(
        out.bundle.contains("createElement(\"style\")"),
        "dev 应注入 <style>"
    );
    assert!(
        out.bundle.contains("var(--c-pad, 24px)"),
        "注入的样式应含求值结果"
    );
}

#[test]
fn css_in_js_disabled_leaves_source_untouched() {
    let out =
        IncrementalBundler::new(Arc::new(linaria_fixture())).build(Path::new("src/index.tsx"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(out.assets.iter().all(|a| !a.is_css));
    // 未启用时不应有类名替换发生
    assert!(
        !out.bundle.contains("box_"),
        "未启用 CSS-in-JS 时不应生成类名"
    );
}

#[test]
fn css_in_js_warns_on_unevaluatable_interpolation() {
    let fs = MemoryFileSystem::from_files([
        ("node_modules/@linaria/core/package.json", LINARIA_PKG_JSON),
        ("node_modules/@linaria/core/index.js", LINARIA_INDEX),
        (
            "src/index.tsx",
            "import { css } from '@linaria/core';\n\
             const box = css`color: red; width: ${compute()}; height: 1px;`;\n\
             export default box;",
        ),
    ]);
    let mut b = IncrementalBundler::new(Arc::new(fs));
    b.enable_css_in_js();
    b.enable_css_extraction();
    let out = b.build(Path::new("src/index.tsx"));

    // 警告但不失败
    assert!(!out.has_errors(), "求值失败应是警告而非错误");
    assert!(
        out.diagnostics
            .iter()
            .any(|d| d.message.contains("无法在构建期求值")),
        "应报出求值失败警告: {:?}",
        out.diagnostics
    );
    let css = out
        .assets
        .iter()
        .find(|a| a.is_css)
        .map(|a| String::from_utf8(a.bytes.clone()).unwrap())
        .unwrap_or_default();
    // 其余声明保留，产物是合法 CSS
    assert!(css.contains("color: red"), "{css}");
    assert!(css.contains("height: 1px"), "{css}");
    assert!(!css.contains("compute"), "不得残留原表达式: {css}");
}

#[test]
fn css_in_js_runs_in_node_with_correct_class_name() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("node 不可用，跳过 CSS-in-JS e2e");
        return;
    }
    let mut b = IncrementalBundler::new(Arc::new(linaria_fixture()));
    b.enable_css_in_js();
    b.enable_css_extraction();
    let out = b.build(Path::new("src/index.tsx"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_cij_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("bundle.cjs");
    std::fs::write(&p, &out.bundle).unwrap();

    // 默认导出应是类名字符串（运行时零样式计算）
    let script = format!(
        "const r = require({:?}); const v = r.default ?? r; \
         if (typeof v !== 'string' || !v.startsWith('box_')) {{ console.error('got', v); process.exit(2); }} \
         process.stdout.write('OK');",
        p.to_string_lossy()
    );
    let o = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        o.status.success() && o.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}",
        o.status.code(),
        String::from_utf8_lossy(&o.stderr)
    );
}

#[test]
fn css_in_js_cross_module_class_reference() {
    // 跨模块 css 互相引用：base 模块导出一个 css 类，wrap 模块用 `.${base}` 当选择器
    let fs = MemoryFileSystem::from_files([
        ("node_modules/@linaria/core/package.json", LINARIA_PKG_JSON),
        ("node_modules/@linaria/core/index.js", LINARIA_INDEX),
        (
            "src/base.tsx",
            "import { css } from '@linaria/core';\n\
             export const base = css`color: red;`;",
        ),
        (
            "src/index.tsx",
            "import { css } from '@linaria/core';\n\
             import { base } from './base.js';\n\
             const wrap = css`.${base} { margin: 4px; }`;\n\
             export default { base, wrap };",
        ),
    ]);
    let mut b = IncrementalBundler::new(Arc::new(fs));
    b.enable_css_in_js();
    b.enable_css_extraction();
    let out = b.build(Path::new("src/index.tsx"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        out.diagnostics.is_empty(),
        "跨模块引用不应报警: {:?}",
        out.diagnostics
    );

    let css = out
        .assets
        .iter()
        .find(|a| a.is_css)
        .map(|a| String::from_utf8(a.bytes.clone()).unwrap())
        .expect("应产出 CSS");

    // 取 base 的类名（来自 base 模块的规则）
    let base_cls = css
        .split('{')
        .find(|s| s.contains("base_"))
        .map(|s| {
            s.rsplit('.')
                .next()
                .unwrap()
                .trim()
                .trim_start_matches('.')
                .to_string()
        })
        .expect("应有 base 类名");
    assert!(base_cls.starts_with("base_"), "{base_cls} / {css}");

    // wrap 的规则应把 base 的类名当作后代选择器
    assert!(
        css.contains(&format!(".{base_cls}{{margin: 4px;}}")),
        "跨模块类名引用未生效\nbase={base_cls}\ncss={css}"
    );
}
/// Runtime factory parameters must be allocated outside the identifiers already emitted by a
/// module. Real packages commonly import `jsxDEV as m`; `$` and `_r` are legal aliases as well.
#[test]
fn runtime_factory_names_avoid_existing_import_bindings() {
    let fs = MemoryFileSystem::from_files([
        (
            "src/dep.js",
            "export const a=1;export const b=2;export const c=3;",
        ),
        (
            "src/index.js",
            "import {a as m,b as $,c as _r} from './dep.js';export default m+$+_r;",
        ),
    ]);
    let mut b = IncrementalBundler::new(Arc::new(fs));
    b.enable_minify().enable_mangle();
    let out = b.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    assert!(
        out.bundle.contains("function(m1,$1,_r1)"),
        "factory parameters should avoid source bindings:\n{}",
        &out.bundle[..out.bundle.len().min(1200)]
    );
    assert!(
        out.bundle.contains("r.m=t;r.c=c"),
        "runtime should expose module and cache namespaces:\n{}",
        &out.bundle[..out.bundle.len().min(1200)]
    );

    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let dir = std::env::temp_dir().join("wake_runtime_names_regression");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("bundle.cjs");
    std::fs::write(&p, &out.bundle).unwrap();
    let syntax = std::process::Command::new("node")
        .arg("--check")
        .arg(&p)
        .output()
        .unwrap();
    assert!(
        syntax.status.success(),
        "{}",
        String::from_utf8_lossy(&syntax.stderr)
    );
    let script = format!(
        "const r=require({:?});if((r.default??r)!==6)process.exit(2);",
        p.to_string_lossy()
    );
    let run = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

#[test]
fn destructured_export_uses_the_mangled_binding_name() {
    let fs = MemoryFileSystem::from_files([
        (
            "src/dep.js",
            "const source={cancel(){return 7}};const {cancel:cancelFrame}=source;export {cancelFrame};",
        ),
        (
            "src/index.js",
            "import {cancelFrame} from './dep.js';export default cancelFrame();",
        ),
    ]);
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    bundler.enable_minify().enable_mangle();
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let dir = std::env::temp_dir().join("wake_destructured_export_mangle_regression");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if((r.default??r)!==7)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let run = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
}

/// 回归：`module.exports = X` 形态的 CJS 模块在 minify 路径下必须真正导出。
///
/// 曾因 `compact_body_names` 把 `module.exports` 改写成 `m.$`（`$` 是 exports 的**值**，
/// `m` 才是 module）导致赋值落到无关属性上，模块导出恒为空对象——任何
/// `module.exports = X` 的 CJS 包（如 `@linaria/core` 的 `cx`）整包失效。
#[test]
fn cjs_module_exports_assignment_survives_minify() {
    let fs = MemoryFileSystem::from_files([
        (
            "src/lib.js",
            "var out = {};\nout.hello = function () { return 42; };\nmodule.exports = out;",
        ),
        (
            "src/index.js",
            "const lib = require('./lib.js');\nexport default lib.hello();",
        ),
    ]);
    let mut b = IncrementalBundler::new(Arc::new(fs));
    b.enable_minify();
    b.enable_mangle();
    let out = b.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    assert!(
        !out.bundle.contains("m.$="),
        "`module.exports` 不得改写为 `m.$`（会写到无关属性上）:\n{}",
        &out.bundle[..out.bundle.len().min(800)]
    );
    assert!(
        out.bundle.contains("m.exports="),
        "`module.exports` 应改写为 `m.exports`:\n{}",
        &out.bundle[..out.bundle.len().min(800)]
    );

    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let dir = std::env::temp_dir().join("wake_cjs_exports_regression");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("bundle.cjs");
    std::fs::write(&p, &out.bundle).unwrap();
    let script = format!(
        "const r = require({:?}); const v = r.default ?? r; \
         if (v !== 42) {{ console.error('got', v); process.exit(2); }} process.stdout.write('OK');",
        p.to_string_lossy()
    );
    let o = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        o.status.success() && o.stdout == b"OK",
        "CJS 导出未生效 stderr={}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// 回归：整体重新赋值 `module.exports` 的 CJS 模块不得并入 scope-hoist 的 concat。
///
/// concat 让被合并模块共享同一个 exports 对象 `$`；而 `module.exports = X` 会把导出**换成
/// 另一个对象**，此后该模块的导出与其它模块写入的 `$` 分属两处，必丢其一。曾致
/// `@linaria/core` 与同组 ESM 模块的导出互相覆盖（`cx` 或 ESM 导出二者只能活一个）。
#[test]
fn cjs_module_exports_module_not_merged_into_concat() {
    let fs = MemoryFileSystem::from_files([
        (
            "node_modules/pkg/package.json",
            r#"{"name":"pkg","main":"index.js"}"#,
        ),
        (
            "node_modules/pkg/index.js",
            "var api = {};\napi.join = function () { \
             return Array.prototype.slice.call(arguments).filter(Boolean).join(' '); };\n\
             module.exports = api;",
        ),
        ("src/base.js", "export const icon = 'icon_' + String(1);"),
        (
            "src/index.js",
            "import pkg from 'pkg';\n\
             import { icon } from './base.js';\n\
             export default pkg.join('a', icon);",
        ),
    ]);
    let mut b = IncrementalBundler::new(Arc::new(fs));
    b.enable_minify();
    b.enable_mangle();
    let out = b.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let dir = std::env::temp_dir().join("wake_cjs_concat_regression");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("bundle.cjs");
    std::fs::write(&p, &out.bundle).unwrap();
    // CJS 的函数导出与 ESM 的常量导出必须**同时**可用
    let script = format!(
        "const r = require({:?}); const v = r.default ?? r; \
         if (v !== 'a icon_1') {{ console.error('got', JSON.stringify(v)); process.exit(2); }} \
         process.stdout.write('OK');",
        p.to_string_lossy()
    );
    let o = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        o.status.success() && o.stdout == b"OK",
        "CJS 与 ESM 导出未能共存 stderr={}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// 跨模块**多层**常量传播：`base → mid → leaf` 三层链，css 插值须能取到最深处的值。
#[test]
fn css_in_js_multi_level_constant_propagation() {
    let fs = MemoryFileSystem::from_files([
        ("node_modules/@linaria/core/package.json", LINARIA_PKG_JSON),
        ("node_modules/@linaria/core/index.js", LINARIA_INDEX),
        // 第 3 层：最底层的原始常量
        (
            "src/base.js",
            "export const UNIT = 4;\nexport const HUE = 'oklch(0.7 0.1 250)';",
        ),
        // 第 2 层：引用第 3 层，组合出新常量
        (
            "src/mid.js",
            "import { UNIT, HUE } from './base.js';\n\
             export const space = { sm: `${UNIT}px`, lg: `${UNIT * 3}px` };\n\
             export const theme = { color: HUE, pad: space.sm };",
        ),
        // 第 1 层：css 插值引用第 2 层（其值又来自第 3 层）
        (
            "src/index.tsx",
            "import { css } from '@linaria/core';\n\
             import { theme } from './mid.js';\n\
             const box = css`color: ${theme.color}; padding: ${theme.pad};`;\n\
             export default box;",
        ),
    ]);
    let mut b = IncrementalBundler::new(Arc::new(fs));
    b.enable_css_in_js();
    b.enable_css_extraction();
    let out = b.build(Path::new("src/index.tsx"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        out.diagnostics.is_empty(),
        "多层传播不应报警: {:?}",
        out.diagnostics
    );

    let css = out
        .assets
        .iter()
        .find(|a| a.is_css)
        .map(|a| String::from_utf8(a.bytes.clone()).unwrap())
        .expect("应产出 CSS");
    // color 穿过 2 层（index ← mid ← base）
    assert!(
        css.contains("color: oklch(0.7 0.1 250)"),
        "第 3 层常量未传播: {css}"
    );
    // pad 穿过 2 层且经过模块内二次引用（space.sm ← UNIT）
    assert!(css.contains("padding: 4px"), "模板内算式未传播: {css}");
}

/// `@keyframes` 作用域化 + `:global()` 逃逸的端到端。
#[test]
fn css_in_js_keyframes_scoped_and_global_escapes() {
    let fs = MemoryFileSystem::from_files([
        ("node_modules/@linaria/core/package.json", LINARIA_PKG_JSON),
        ("node_modules/@linaria/core/index.js", LINARIA_INDEX),
        (
            "src/index.tsx",
            "import { css } from '@linaria/core';\n\
             const spinner = css`\n\
             \x20 animation: spin 1s linear infinite;\n\
             \x20 @keyframes spin { from { transform: rotate(0deg); } to { transform: rotate(360deg); } }\n\
             \x20 :global() { html, body { margin: 0; } }\n\
             `;\n\
             export default spinner;",
        ),
    ]);
    let mut b = IncrementalBundler::new(Arc::new(fs));
    b.enable_css_in_js();
    b.enable_css_extraction();
    let out = b.build(Path::new("src/index.tsx"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let css = out
        .assets
        .iter()
        .find(|a| a.is_css)
        .map(|a| String::from_utf8(a.bytes.clone()).unwrap())
        .expect("应产出 CSS");

    // 类名后缀 = 类名去掉 `.`
    let cls = css
        .split('{')
        .next()
        .unwrap()
        .trim_start_matches('.')
        .to_string();
    assert!(cls.starts_with("spinner_"), "{cls} / {css}");

    // 关键帧被作用域化，且 animation 引用同步改写
    assert!(
        css.contains(&format!("@keyframes spin-{cls}")),
        "关键帧未作用域化: {css}"
    );
    assert!(
        css.contains(&format!("animation: spin-{cls} 1s linear infinite")),
        "animation 引用未改写: {css}"
    );
    // :global() 内容逃逸出类作用域
    assert!(css.contains("html,body{margin: 0;}"), "{css}");
    assert!(
        !css.contains(&format!(".{cls} html")),
        ":global() 内容不应带类前缀: {css}"
    );
}

/// TS `namespace` 的点分名 / 声明合并 / 嵌套，以及 `enum` 合并——须在 **prod 压缩路径**下正确。
///
/// 回归点：各嵌套层曾共用同一 span，而压缩器侧表以 span 为键 → 对某层的「未用绑定消除」
/// 判定连带命中其它层，把整个嵌套削平（第二个 `namespace A.B.C` 块整体消失、合并失效）。
#[test]
fn ts_namespace_dotted_merged_and_nested_survive_minify() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let fs = MemoryFileSystem::from_files([(
        "src/index.ts",
        "namespace A.B.C { export const x = 1; export function f() { return x + 1; } }\n\
         namespace A.B.C { export const y = 2; }\n\
         enum E { P = 1 }\n\
         enum E { Q = 2 }\n\
         namespace Outer { export namespace Inner { export const z = 3; } \
         const priv = 4; export const pub = priv; }\n\
         export default JSON.stringify({ x: A.B.C.x, y: A.B.C.y, f: A.B.C.f(), \
         eP: E.P, eQ: E.Q, rev: E[1], inner: Outer.Inner.z, pub: Outer.pub });",
    )]);
    let mut b = IncrementalBundler::new(Arc::new(fs));
    b.enable_minify();
    b.enable_mangle();
    b.enable_tree_shaking();
    let out = b.build(Path::new("src/index.ts"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_ns_regression");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join("bundle.cjs");
    std::fs::write(&p, &out.bundle).unwrap();
    let script = format!(
        "const r = require({:?}); const v = JSON.parse(r.default ?? r); \
         const want = {{x:1,y:2,f:2,eP:1,eQ:2,rev:'P',inner:3,pub:4}}; \
         for (const k of Object.keys(want)) if (v[k] !== want[k]) {{ \
           console.error(k, '=', v[k], 'want', want[k]); process.exit(2); }} \
         process.stdout.write('OK');",
        p.to_string_lossy()
    );
    let o = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        o.status.success() && o.stdout == b"OK",
        "namespace/enum 降级在压缩后语义不符 stderr={}",
        String::from_utf8_lossy(&o.stderr)
    );
}

/// JSX dev runtime 经 bundler 接线：dev 口径产 `jsxDEV` + 源位置，prod 口径不受影响。
#[test]
fn jsx_dev_runtime_wired_through_bundler() {
    let files = [
        (
            "node_modules/react/package.json",
            r#"{"name":"react","main":"index.js"}"#,
        ),
        ("node_modules/react/index.js", "exports.x = 1;"),
        (
            "node_modules/react/jsx-runtime.js",
            "exports.jsx=()=>0;exports.jsxs=()=>0;exports.Fragment=0;",
        ),
        (
            "node_modules/react/jsx-dev-runtime.js",
            "exports.jsxDEV=()=>0;exports.Fragment=0;",
        ),
        (
            "src/index.tsx",
            "export default <div className=\"x\">hi</div>;",
        ),
    ];

    // dev 口径
    let mut dev = IncrementalBundler::new(Arc::new(MemoryFileSystem::from_files(files)));
    dev.set_jsx_runtime(true, "react");
    let out = dev.build(Path::new("src/index.tsx"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(out.bundle.contains("jsxDEV"), "dev 应用 jsxDEV");
    assert!(
        out.bundle.contains("src/index.tsx"),
        "dev 应带 fileName（正斜杠）"
    );
    assert!(out.bundle.contains("lineNumber"), "dev 应带源位置");

    // prod 口径（默认）——不应出现 dev runtime
    let out2 = IncrementalBundler::new(Arc::new(MemoryFileSystem::from_files(files)))
        .build(Path::new("src/index.tsx"));
    assert!(!out2.has_errors(), "{:?}", out2.diagnostics);
    assert!(!out2.bundle.contains("jsxDEV"), "prod 不应用 dev runtime");
    assert!(!out2.bundle.contains("lineNumber"), "prod 不应带源位置");
}

/// TC39 **Stage-3 装饰器**端到端：编译产物在 node 中的**可观察行为**须与 tsc 一致。
///
/// 覆盖：类/方法/静态方法/取值器/设值器/字段/静态字段装饰器、多装饰器倒序应用、
/// 装饰器返回值替换、`addInitializer`、继承、显式构造函数、字段 extraInitializers 串联。
/// 期望值取自 `tsc --target es2022` 对同一源码产物的实际运行结果。
#[test]
fn stage3_decorators_match_tsc_behavior() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let cases: &[(&str, &str, &str)] = &[
        (
            "kinds_and_order",
            "const log=[];\
             function mdec(v,c){log.push('dec:'+c.kind+':'+c.name);return v;}\
             @mdec class C{ @mdec m(){return 'm';} @mdec static sm(){return 'sm';} \
             @mdec f='f'; @mdec get g(){return 'g';} }\
             const c=new C();\
             export default JSON.stringify({log,m:c.m(),sm:C.sm(),f:c.f,g:c.g});",
            r#"{"log":["dec:method:sm","dec:method:m","dec:getter:g","dec:field:f","dec:class:C"],"m":"m","sm":"sm","f":"f","g":"g"}"#,
        ),
        (
            "replace_init_inherit",
            "const log=[];\
             function twice(v,c){return function(...a){return v.apply(this,a)+v.apply(this,a);};}\
             function init(v,c){c.addInitializer(function(){log.push('init:'+c.name);});return v;}\
             function plus(v,c){return (x)=>x+'!';}\
             function tag(n){return (v,c)=>{log.push('apply:'+n);return v;};}\
             class Base{ base(){return 'B';} }\
             @tag('c1') @tag('c2') class D extends Base{\
               @twice m(){return 'x';} @init n(){return 'n';} @plus f='f';\
               @tag('m1') @tag('m2') k(){return 'k';} }\
             const d=new D();\
             export default JSON.stringify({log,m:d.m(),f:d.f,base:d.base(),k:d.k()});",
            r#"{"log":["apply:m2","apply:m1","apply:c2","apply:c1","init:n"],"m":"xx","f":"f!","base":"B","k":"k"}"#,
        ),
        (
            "static_field_and_explicit_ctor",
            "const log=[];\
             function d(v,c){log.push(c.kind+':'+c.name+':'+c.static);return v;}\
             function sfd(v,c){return (x)=>'S'+x;}\
             class E{ @d static sf='sf'; @d static get sg(){return 'sg';} \
               @d set s(v){log.push('set:'+v);} @sfd g1='a'; @sfd g2='b';\
               constructor(){log.push('ctor');} }\
             const e=new E(); e.s='v';\
             export default JSON.stringify({log,sf:E.sf,sg:E.sg,g1:e.g1,g2:e.g2});",
            r#"{"log":["getter:sg:true","setter:s:false","field:sf:true","ctor","set:v"],"sf":"sf","sg":"sg","g1":"Sa","g2":"Sb"}"#,
        ),
    ];

    for (name, src, expected) in cases {
        let fs = MemoryFileSystem::from_files([("src/index.ts", *src)]);
        let mut b = IncrementalBundler::new(Arc::new(fs));
        b.enable_minify();
        b.enable_mangle();
        let out = b.build(Path::new("src/index.ts"));
        assert!(!out.has_errors(), "[{name}] {:?}", out.diagnostics);

        let dir = std::env::temp_dir().join(format!("wake_stage3_{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("bundle.cjs");
        std::fs::write(&p, &out.bundle).unwrap();
        let script = format!(
            "const r=require({:?});process.stdout.write(String(r.default??r));",
            p.to_string_lossy()
        );
        let o = std::process::Command::new("node")
            .arg("-e")
            .arg(&script)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "[{name}] node 执行失败: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let got = String::from_utf8_lossy(&o.stdout);
        assert_eq!(got, *expected, "[{name}] 行为与 tsc 不一致");
    }
}

/// `.scss`/`.sass`/`.less` 须**明确报错**，而不是当普通 CSS 透传。
///
/// 回归点：曾把这些扩展名并入 `is_css_path`，嵌套/变量/mixin 会原样落进产物，
/// 形成非法 CSS 或静默错误样式——「看似构建成功」比构建失败更危险。
#[test]
fn sass_and_less_are_rejected_with_clear_error() {
    for (file, ext) in [("src/a.scss", "scss"), ("src/b.less", "less")] {
        let fs = MemoryFileSystem::from_files([
            (file, "$c: red;\n.a { color: $c; .b { margin: 0 } }"),
            (
                "src/index.ts",
                &format!(
                    "import \"./{}\";\nexport default 1;",
                    std::path::Path::new(file)
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                ),
            ),
        ]);
        let out = IncrementalBundler::new(Arc::new(fs)).build(Path::new("src/index.ts"));
        assert!(out.has_errors(), "`.{ext}` 应报错而非静默透传");
        let d = out
            .diagnostics
            .iter()
            .find(|d| d.code.as_deref() == Some("WAKE0302"))
            .unwrap_or_else(|| panic!("应有 WAKE0302 诊断: {:?}", out.diagnostics));
        // 提示必须可操作：说明不内置预处理器 + 给出替代做法
        let note = d.notes.join(" ");
        assert!(note.contains("预处理器"), "{note}");
        assert!(note.contains(".css"), "应指出改用 .css: {note}");
        // 产物里不得残留未编译的预处理器语法
        assert!(!out.bundle.contains("$c"), "不得把 SCSS 变量透传进产物");
    }
}

/// 普通 `.css` 不受上一条影响。
#[test]
fn plain_css_still_loads() {
    let fs = MemoryFileSystem::from_files([
        ("src/a.css", ".p { color: blue }"),
        ("src/index.ts", "import \"./a.css\";\nexport default 1;"),
    ]);
    let out = IncrementalBundler::new(Arc::new(fs)).build(Path::new("src/index.ts"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(out.bundle.contains("color: blue"));
}

// ======================================================================
// import= / export= / import attributes / using 的端到端验证
// ======================================================================

/// `import x = require(..)` + `export = ..` + `.json` 引入属性的混合项目。
fn ts_cjs_interop_fixture() -> MemoryFileSystem {
    MemoryFileSystem::from_files([
        (
            "index.ts",
            // `import = require` 拿到的是目标模块的整个 exports（即 `export =` 赋出的那个对象），
            // 与 TS/Node 的 CJS 语义一致。
            //
            // JSON 走普通默认导入 + 引入属性：wake 的 loader 把 `.json` 转成
            // `export default <json>`（loader.rs `json_to_js_module`），故 `require('./x.json')`
            // 得到的是 `{ default: … }` 而非裸对象——JSON 应当用 ESM 导入，属性子句仅作标注。
            "import api = require('./api.ts');\n\
             import cfg from './cfg.json' with { type: 'json' };\n\
             export const sum: number = api.add(2, 3) + cfg.bonus;",
        ),
        (
            "api.ts",
            "const api = { add(a: number, b: number): number { return a + b; } };\n\
             export = api;",
        ),
        ("cfg.json", "{ \"bonus\": 4 }"),
    ])
}

#[test]
fn ts_import_equals_and_export_assign_bundle_in_node() {
    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("node 不可用，跳过 import=/export= e2e");
        return;
    }
    let out =
        IncrementalBundler::new(Arc::new(ts_cjs_interop_fixture())).build(Path::new("index.ts"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    // `import = require('./api.ts')` 走既有 Require 依赖链路 → 目标进模块图并被改写。
    assert_eq!(out.module_count, 3, "api.ts / cfg.json 都应进模块图");
    assert!(
        !out.bundle.contains("require(\"./api.ts\")"),
        "内部 require 未被改写为 __wake_require__:\n{}",
        out.bundle
    );

    let dir = std::env::temp_dir().join("wake_bundle_import_equals_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();

    // sum = add(2,3) + 4 = 9。
    let script = format!(
        "const r = require({:?}); if (r.sum !== 9) {{ console.error('sum=', r.sum); process.exit(2); }} process.stdout.write('OK');",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

/// `using` / `await using`：资源在离开作用域时必须被 dispose——包括**零引用**的绑定。
fn using_fixture() -> MemoryFileSystem {
    MemoryFileSystem::from_files([(
        "index.ts",
        "const log: string[] = [];\n\
         function res(name: string) { return { [Symbol.dispose]() { log.push(name); } }; }\n\
         function ares(name: string) { return { async [Symbol.asyncDispose]() { log.push(name); } }; }\n\
         export async function run(): Promise<string> {\n\
         {\n\
           using a = res('a');\n\
           using _unused = res('unused');\n\
           await using b = ares('b');\n\
           log.push('body');\n\
           void a;\n\
         }\n\
         return log.join(',');\n\
         }",
    )])
}

#[test]
fn using_declarations_dispose_in_node() {
    // 不能只检查 node 是否存在：Node 22 暴露 Symbol.dispose，但解析器仍不接受
    // using / await using。先探测测试真正依赖的语法能力，避免 runner 预装版本变化
    // 导致与 bundler 无关的 SyntaxError。
    let probe = std::process::Command::new("node")
        .arg("-e")
        .arg(
            "async function __wake_using_probe(){using a={ [Symbol.dispose](){} };await using b={ async [Symbol.asyncDispose](){} };}",
        )
        .output();
    let Ok(probe) = probe else {
        eprintln!("node 不可用，跳过 using e2e");
        return;
    };
    if !probe.status.success() {
        eprintln!(
            "当前 node 不支持 using / await using，跳过 e2e：{}",
            String::from_utf8_lossy(&probe.stderr)
        );
        return;
    }
    // 关闭/开启 minify+mangle 各跑一遍：未用绑定消除、变量重命名都不得破坏 dispose 语义。
    for (label, minify) in [("plain", false), ("minified", true)] {
        let mut b = IncrementalBundler::new(Arc::new(using_fixture()));
        if minify {
            b.enable_minify().enable_mangle();
        }
        let out = b.build(Path::new("index.ts"));
        assert!(!out.has_errors(), "[{label}] {:?}", out.diagnostics);

        let dir = std::env::temp_dir().join(format!("wake_bundle_using_e2e_{label}"));
        std::fs::create_dir_all(&dir).unwrap();
        let bundle_path = dir.join("bundle.cjs");
        std::fs::write(&bundle_path, &out.bundle).unwrap();

        // dispose 逆声明序执行：body → b → unused → a。
        let script = format!(
            "require({:?}).run().then(s => {{ if (s !== 'body,b,unused,a') {{ console.error('got=', s); process.exit(2); }} process.stdout.write('OK'); }}).catch(e => {{ console.error(e); process.exit(3); }});",
            bundle_path.to_string_lossy()
        );
        let output = std::process::Command::new("node")
            .arg("-e")
            .arg(&script)
            .output()
            .unwrap();
        assert!(
            output.status.success() && output.stdout == b"OK",
            "[{label}] node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr),
            out.bundle
        );
    }
}

// ======================================================================
// 字体/图片资源 loader（CSS `url()`）+ CSS prod 抽取顺序 + concat 导出名冲突
// ======================================================================

#[test]
fn concat_does_not_clobber_duplicate_export_names() {
    // 回归：scope-hoist 的 concat 让所有成员共享同一个 exports 对象 `$`，两个模块写同一个
    // 导出名会互相覆盖。`export default` 必撞——曾使 `a + b` 产出 "BB" 而非 "AB"，
    // 且**每个资源模块恰好都是 `export default "<url>"`**，两张图片的 URL 就此串掉。
    let fs = MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import a from './a.js';\nimport b from './b.js';\nimport { v } from './c.js';\n\
             export const info = a + b + v;",
        ),
        ("src/a.js", "export default 'A';"),
        ("src/b.js", "export default 'B';"),
        ("src/c.js", "export const v = 'C';"),
    ]);
    let mut bundler = IncrementalBundler::new(Arc::new(fs));
    bundler.enable_minify().enable_mangle();
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    if std::process::Command::new("node")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("node 不可用，跳过 concat 导出名 e2e");
        return;
    }
    let dir = std::env::temp_dir().join("wake_bundle_concat_dup_export");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r = require({:?}); if (r.info !== 'ABC') {{ console.error('info=', r.info); process.exit(2); }} process.stdout.write('OK');",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn extracted_css_follows_dependency_order() {
    // CSS 层叠靠顺序定胜负。prod 抽取必须与 dev 的 `<style>` 注入顺序一致——都是模块求值序
    // （依赖先行）。此前按模块 id（BFS 发现序）排，`base.css` 被排到 `styles.css` 之后，
    // 覆盖关系整个反过来。dev 侧的顺序断言见 `css_bundle_runs_in_node`。
    let mut bundler = IncrementalBundler::new(Arc::new(css_fixture()));
    bundler.enable_css_extraction();
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let css = out
        .assets
        .iter()
        .find(|a| a.is_css)
        .expect("应有抽取的 CSS 产物");
    let text = String::from_utf8_lossy(&css.bytes);
    let base = text.find(".base").expect("缺 .base");
    let title = text.find(".title").expect("缺 .title");
    assert!(
        base < title,
        "被 @import 的 base.css 必须排在 styles.css 之前:\n{text}"
    );
}

/// CSS 通过 `url()` 引用字体与图片——这是字体/图片**主要**的引用方式，不是 JS import。
fn css_url_fixture() -> MemoryFileSystem {
    MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import './theme.css';\nexport const ok = 1;",
        ),
        (
            "src/theme.css",
            "@font-face{font-family:\"F\";src:url(\"./fonts/big.woff2\") format(\"woff2\"),\
             url(./fonts/small.woff) format(\"woff\")}\
             .hero{background-image:url(../img/dot.png)}\
             .ext{background:url(https://cdn.example/x.png)}\
             .frag{filter:url(#blur)}",
        ),
        ("src/fonts/big.woff2", &"F".repeat(5000)),
        ("src/fonts/small.woff", "SMALLFONT"),
        ("img/dot.png", "PNGBYTES"),
    ])
}

#[test]
fn css_url_assets_are_emitted_and_rewritten() {
    let mut bundler = IncrementalBundler::new(Arc::new(css_url_fixture()));
    bundler
        .enable_css_extraction()
        .set_asset_inline_limit(4096)
        .set_public_path("/");
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let css = out
        .assets
        .iter()
        .find(|a| a.is_css)
        .expect("应有抽取的 CSS 产物");
    let text = String::from_utf8_lossy(&css.bytes);

    // 超阈值字体 → 独立产物 + publicPath URL；产物字节与源一致。
    let font = out
        .assets
        .iter()
        .find(|a| a.file_name.starts_with("big.") && a.file_name.ends_with(".woff2"))
        .expect("超阈值字体应产出独立文件");
    assert_eq!(font.bytes.len(), 5000);
    assert!(
        text.contains(&format!("/{}", font.file_name)),
        "CSS 里的字体 url 应改写为产物 URL:\n{text}"
    );
    // 阈值内的字体与图片 → data URI（MIME 按扩展名）。
    assert!(text.contains("data:font/woff;base64,"), "{text}");
    assert!(text.contains("data:image/png;base64,"), "{text}");
    // 原相对路径全部消失，否则产物里是死链。
    assert!(!text.contains("./fonts/big.woff2"), "{text}");
    assert!(!text.contains("./fonts/small.woff"), "{text}");
    assert!(!text.contains("../img/dot.png"), "{text}");
    // 反例：外部 URL 与 SVG 片段引用不得被改写。
    assert!(text.contains("https://cdn.example/x.png"), "{text}");
    assert!(text.contains("url(#blur)"), "{text}");
}

#[test]
fn css_url_assets_work_in_dev_injection_path() {
    // dev（不抽取、全内联）下同样要改写——`<style>` 注入后相对路径的基准变成 HTML 文档，
    // 不改写必然 404。
    let mut bundler = IncrementalBundler::new(Arc::new(css_url_fixture()));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        out.bundle.contains("data:font/woff2;base64,"),
        "{}",
        out.bundle
    );
    assert!(
        out.bundle.contains("data:image/png;base64,"),
        "{}",
        out.bundle
    );
    assert!(!out.bundle.contains("./fonts/big.woff2"), "{}", out.bundle);
    // dev 默认全内联 → 无独立资源产物。
    assert!(
        out.assets.iter().all(|a| a.is_css),
        "dev 不应产出独立资源文件"
    );
}

#[test]
fn public_path_injected_into_chunk_loader() {
    // `set_public_path` 必须贯穿到 async chunk 的加载 URL：否则子路径部署下 import() 按当前
    // 页面 URL 相对解析 → 404（WAKE-COMPATIBILITY 切片 2）。
    let mut b = IncrementalBundler::new(Arc::new(split_fixture()));
    b.enable_code_splitting().set_public_path("/app/");
    let out = b.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    assert!(
        out.bundle.contains("__wake__.publicPath = \"/app/\";"),
        "entry chunk 应注入 publicPath:\n{}",
        out.bundle
    );
    // 注入点与消费点对齐：运行时确实用 publicPath 拼 chunk 的 script.src。
    assert!(
        out.bundle.contains("s.src = W.publicPath + file;"),
        "运行时应用 publicPath 拼 chunk URL:\n{}",
        out.bundle
    );
    // async chunk 体内不含文件名/前缀（保持 hash 无环）——URL 只在 entry 的 f 映射里拼。
    let async_chunk = out.chunks.iter().find(|c| !c.is_entry).unwrap();
    assert!(!async_chunk.code.contains("/app/"), "{}", async_chunk.code);
}

#[test]
fn public_path_defaults_and_escapes() {
    // 默认 `/`（与 wake_html 注入 `<script src>` 的默认一致）。
    let mut b = IncrementalBundler::new(Arc::new(split_fixture()));
    b.enable_code_splitting();
    let out = b.build(Path::new("src/index.js"));
    assert!(
        out.bundle.contains("__wake__.publicPath = \"/\";"),
        "{}",
        out.bundle
    );

    // 值作为 JS 字符串字面量发出 → 反斜杠/引号须转义，不能截断字面量。
    let mut b = IncrementalBundler::new(Arc::new(split_fixture()));
    b.enable_code_splitting().set_public_path("/a\"b\\c/");
    let out = b.build(Path::new("src/index.js"));
    assert!(
        out.bundle
            .contains("__wake__.publicPath = \"/a\\\"b\\\\c/\";"),
        "{}",
        out.bundle
    );
}

/// 浏览器形态 e2e 的 runner：在 vm 沙箱里跑 entry chunk（无 `process`/`require` → 走 `<script>`
/// 分支），用桩 `document` 捕获 chunk URL 并从盘上加载，最后校验 URL 前缀与懒加载结果。
const PUBLIC_PATH_RUNNER_JS: &str = r#"const fs = require("fs");
const path = require("path");
const vm = require("vm");

const PUBLIC = "/app/";
const entryFile = process.argv[2];
const requested = [];

function run(ctx, file) {
  const code = fs.readFileSync(path.join(__dirname, file), "utf8");
  vm.runInContext(code, ctx, { filename: file });
}

const document = {
  createElement: function () { return {}; },
  head: {
    appendChild: function (s) {
      requested.push(s.src);
      if (s.src.slice(0, PUBLIC.length) !== PUBLIC) {
        console.error("chunk URL 缺少 publicPath 前缀:", s.src);
        return s.onerror();
      }
      try { run(ctx, s.src.slice(PUBLIC.length)); s.onload(); }
      catch (e) { console.error(e); s.onerror(); }
    },
  },
};
// 沙箱里不放 process/require：运行时据此判定为浏览器，走 script 标签加载。
const ctx = vm.createContext({ console: console, document: document });

run(ctx, entryFile);
const app = ctx.__wake_entry__;
if (!app || app.eager !== 1) { console.error("entry exports=", app); process.exit(4); }
app.load().then(function (v) {
  if (v !== 42) { console.error("v=", v); process.exit(2); }
  if (requested.length !== 1) { console.error("requested=", requested); process.exit(5); }
  process.stdout.write("OK " + requested.join(","));
}).catch(function (e) { console.error(e); process.exit(3); });
"#;

#[test]
fn public_path_chunk_url_loads_in_browser_like_env() {
    if !node_available() {
        eprintln!("node 不可用，跳过 publicPath chunk URL e2e");
        return;
    }
    let mut b = IncrementalBundler::new(Arc::new(split_fixture()));
    b.enable_code_splitting().set_public_path("/app/");
    let out = b.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_split_public_path_e2e");
    let _ = std::fs::remove_dir_all(&dir);
    let entry = write_chunks(&out, &dir);
    let entry_file = entry.file_name().unwrap().to_string_lossy().into_owned();
    let runner = dir.join("run-public-path.js");
    std::fs::write(&runner, PUBLIC_PATH_RUNNER_JS).unwrap();

    let output = std::process::Command::new("node")
        .arg(&runner)
        .arg(&entry_file)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "node 执行失败 status={:?} stdout={} stderr={}",
        output.status.code(),
        stdout,
        String::from_utf8_lossy(&output.stderr),
    );

    let async_chunk = out.chunks.iter().find(|c| !c.is_entry).unwrap();
    assert_eq!(
        stdout,
        format!("OK /app/{}", async_chunk.file_name),
        "chunk 应经 `/app/` 前缀加载"
    );
}

// ============================================================
// 顶层 await（DESIGN §6.1.1）
// ============================================================

/// index → cfg（顶层 await）→ raw。async 子图 = {index, cfg}，raw 保持同步。
fn tla_fixture() -> MemoryFileSystem {
    MemoryFileSystem::from_files([
        (
            "src/index.js",
            "import { port } from './cfg.js';\n\
             export const url = 'http://host:' + port;",
        ),
        (
            "src/cfg.js",
            "import { raw } from './raw.js';\n\
             const loaded = await Promise.resolve(raw);\n\
             export const port = loaded + 1;",
        ),
        ("src/raw.js", "export const raw = 8079;"),
    ])
}

/// 消费 async 入口：`module.exports` 是 Promise，await 后断言导出值。
fn tla_entry_script(bundle_path: &Path) -> String {
    format!(
        "Promise.resolve(require({:?})).then(m => {{ \
           if (m.url !== 'http://host:8080') {{ console.error('url=', m.url); process.exit(2); }} \
           process.stdout.write('OK'); \
         }}).catch(e => {{ console.error(e); process.exit(3); }});",
        bundle_path.to_string_lossy()
    )
}

#[test]
fn top_level_await_marks_async_subgraph() {
    let out = IncrementalBundler::new(Arc::new(tla_fixture())).build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(out.module_count, 3);
    // cfg（含 TLA）与 index（静态导入 cfg）是 async；raw 不受影响。
    assert!(
        out.bundle
            .contains("0: async function(module, exports, __wake_require__)"),
        "index 应为 async 包装:\n{}",
        out.bundle
    );
    assert!(
        out.bundle
            .contains("1: async function(module, exports, __wake_require__)"),
        "cfg 应为 async 包装:\n{}",
        out.bundle
    );
    assert!(
        out.bundle
            .contains("2: function(module, exports, __wake_require__)"),
        "raw 应保持同步包装:\n{}",
        out.bundle
    );
    // 静态导入点插入 await；同步依赖不插。
    assert!(
        out.bundle
            .contains("const _wm0 = (await __wake_require__(1));"),
        "index 对 cfg 的导入应 await:\n{}",
        out.bundle
    );
    assert!(
        out.bundle.contains("const _wm0 = __wake_require__(2);"),
        "cfg 对 raw 的导入不应 await:\n{}",
        out.bundle
    );
    // runtime 换成 Promise 感知版。
    assert!(out.bundle.contains("return cached.p || cached.exports;"));
}

#[test]
fn no_top_level_await_keeps_sync_runtime() {
    // 无顶层 await 的项目：产物不得出现 async 包装 / Promise 感知 runtime（旧路径逐字节不变）。
    let out = IncrementalBundler::new(Arc::new(fixture())).build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        !out.bundle.contains("async function(module"),
        "{}",
        out.bundle
    );
    assert!(!out.bundle.contains("cached.p"), "{}", out.bundle);
    assert!(
        !out.bundle.contains("(await __wake_require__"),
        "{}",
        out.bundle
    );
}

#[test]
fn top_level_await_bundle_runs_in_node() {
    if !node_available() {
        eprintln!("node 不可用，跳过顶层 await e2e");
        return;
    }
    let out = IncrementalBundler::new(Arc::new(tla_fixture())).build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);

    let dir = std::env::temp_dir().join("wake_bundle_tla_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();

    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(tla_entry_script(&bundle_path))
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn top_level_await_minified_bundle_runs_in_node() {
    if !node_available() {
        eprintln!("node 不可用，跳过顶层 await minify e2e");
        return;
    }
    let mut b = IncrementalBundler::new(Arc::new(tla_fixture()));
    b.enable_minify();
    b.enable_mangle();
    b.enable_tree_shaking();
    let out = b.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    // 含顶层 await → 退出模块合并，回到逐模块注册表 + async 包装。
    assert!(
        out.bundle.contains("async function(m,$,_r)"),
        "minify 路径应有 async 包装:\n{}",
        out.bundle
    );

    let dir = std::env::temp_dir().join("wake_bundle_tla_min_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();

    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(tla_entry_script(&bundle_path))
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn dynamic_import_of_tla_module_keeps_importer_sync() {
    if !node_available() {
        eprintln!("node 不可用，跳过动态 import TLA e2e");
        return;
    }
    // 动态 import 不传染 async：入口保持同步，`import()` 的 Promise 自动展平到目标求值完成。
    let fs = MemoryFileSystem::from_files([
        (
            "src/index.js",
            "export function load() { return import('./slow.js').then(m => m.value); }",
        ),
        (
            "src/slow.js",
            "const v = await Promise.resolve(41);\nexport const value = v + 1;",
        ),
    ]);
    let out = IncrementalBundler::new(Arc::new(fs)).build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        out.bundle
            .contains("0: function(module, exports, __wake_require__)"),
        "入口不应因动态 import 变 async:\n{}",
        out.bundle
    );

    let dir = std::env::temp_dir().join("wake_bundle_tla_dyn_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();

    let script = format!(
        "require({:?}).load().then(v => {{ \
           if (v !== 42) {{ console.error('value=', v); process.exit(2); }} \
           process.stdout.write('OK'); \
         }}).catch(e => {{ console.error(e); process.exit(3); }});",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}\n--- bundle ---\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn top_level_await_across_code_split_chunks_runs_in_node() {
    if !node_available() {
        eprintln!("node 不可用，跳过顶层 await 代码分割 e2e");
        return;
    }
    // async chunk 里的模块含顶层 await：`__wake_require__.import` 需等它求值完成再取命名空间。
    let fs = MemoryFileSystem::from_files([
        (
            "src/index.js",
            "export const eager = 1;\n\
             export function load() { return import('./lazy.js').then(m => m.value); }",
        ),
        (
            "src/lazy.js",
            "const base = await Promise.resolve(40);\nexport const value = base + 2;",
        ),
    ]);
    let mut b = IncrementalBundler::new(Arc::new(fs));
    b.enable_code_splitting();
    let out = b.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    let async_chunk = out.chunks.iter().find(|c| !c.is_entry).unwrap();
    assert!(
        async_chunk
            .code
            .contains("async function(module, exports, __wake_require__)"),
        "async chunk 内的 TLA 模块应 async 包装:\n{}",
        async_chunk.code
    );

    let dir = std::env::temp_dir().join("wake_split_tla_e2e");
    let _ = std::fs::remove_dir_all(&dir);
    let entry = write_chunks(&out, &dir);

    let script = format!(
        "const app = require({:?});\n\
         app.load().then(v => {{ \
           if (v !== 42) {{ console.error('value=', v); process.exit(2); }} \
           process.stdout.write('OK'); \
         }}).catch(e => {{ console.error(e); process.exit(3); }});",
        entry.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(&script)
        .output()
        .unwrap();
    assert!(
        output.status.success() && output.stdout == b"OK",
        "node 执行失败 status={:?} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn top_level_await_survives_persistent_cache() {
    // 关键路径：摘要命中 → 跳过 parse，`has_top_level_await` 只能来自缓存摘要。
    // 若摘要不带该标志，热缓存产物会退回同步包装 → 加载即 SyntaxError。
    let cache_path = std::env::temp_dir().join("wake_pcache_tla.bin");
    let _ = std::fs::remove_file(&cache_path);

    let ref_out = IncrementalBundler::new(Arc::new(tla_fixture())).build(Path::new("src/index.js"));
    assert!(!ref_out.has_errors(), "{:?}", ref_out.diagnostics);

    let mut b1 = IncrementalBundler::new(Arc::new(tla_fixture()));
    b1.enable_persistent_cache(cache_path.clone());
    let out1 = b1.build(Path::new("src/index.js"));
    assert!(!out1.has_errors(), "{:?}", out1.diagnostics);
    assert_eq!(out1.bundle, ref_out.bundle, "开缓存首遍须与无缓存产物一致");

    // 全新 bundler（内存 memo 空），从磁盘载入热缓存。
    let mut b2 = IncrementalBundler::new(Arc::new(tla_fixture()));
    b2.enable_persistent_cache(cache_path.clone());
    let out2 = b2.build(Path::new("src/index.js"));
    assert!(!out2.has_errors(), "{:?}", out2.diagnostics);
    assert_eq!(out2.bundle, ref_out.bundle, "热缓存新进程产物须逐字节一致");
    assert_eq!(b2.task_exec_count(), 0, "热缓存：parse + codegen 全部跳过");

    let _ = std::fs::remove_file(&cache_path);
}

#[test]
fn top_level_await_in_non_incremental_bundler() {
    // 非增量 `Bundler`（MVP 直接执行路径）同样要产出 async 包装，否则是加载即报错的坏包。
    let out = Bundler::new(Arc::new(tla_fixture())).build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(out.module_count, 3);
    assert!(
        out.bundle
            .contains("async function(module, exports, __wake_require__)"),
        "应有 async 包装:\n{}",
        out.bundle
    );
    assert!(
        out.bundle.contains("(await __wake_require__("),
        "静态导入点应 await:\n{}",
        out.bundle
    );
    assert!(out.bundle.contains("return cached.p || cached.exports;"));
}

#[test]
fn synthetic_cjs_export_uses_mangled_binding_name() {
    let fs = MemoryFileSystem::from_files([
        (
            "index.js",
            "const pkg=require('./pkg.js');console.log(pkg.css());",
        ),
        (
            "pkg.js",
            r#"
var __defProp=Object.defineProperty;
var __export=(target,all)=>{for(var name in all)__defProp(target,name,{get:all[name],enumerable:true})};
var src_exports={};
__export(src_exports,{css:()=>css_default});
module.exports=src_exports;
var css=()=>42;
var css_default=css;
"#,
        ),
    ]);
    let out = IncrementalBundler::new(Arc::new(fs)).build(Path::new("index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        !out.bundle.contains("=css_default"),
        "合成 CJS 导出不得引用已被 mangle 的旧名:\n{}",
        out.bundle
    );
}

#[test]
fn structured_rest_parameters_lower_for_chrome_40() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const log=[];function mark(value){log.push(value);return value} \
         function old(a=mark('a'),{b=mark('b')}={},...[c=mark('c'),...tail]){return [this.base,a,b,c,tail.length]} \
         const functionResult=old.call({base:10},undefined,undefined,undefined,4,5); \
         const holder={base:20,method(...[x,y]){return this.base+x+y},run(){const arrow=(...{length,...rest})=>this.base+length+rest[1]+Object.keys(rest).length;return arrow(3,4,5)}}; \
         const methodResult=holder.method(1,2);const arrowResult=holder.run(); \
         export {log,functionResult,methodResult,arrowResult};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("...["), "{}", out.bundle);
    assert!(!out.bundle.contains("...{"), "{}", out.bundle);
    assert!(
        out.bundle.contains("slice.call(arguments"),
        "structured rest must read the owning function's arguments: {}",
        out.bundle
    );
    assert!(out.bundle.contains(".bind(this)"), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_structured_rest_chrome40_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(JSON.stringify(r.log)!=='[\"a\",\"b\",\"c\"]'||JSON.stringify(r.functionResult)!=='[10,\"a\",\"b\",\"c\",2]'||r.methodResult!==23||r.arrowResult!==30)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "chrome 40 structured-rest runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn structured_rest_parameters_keep_native_collection_for_chrome_55() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "function collect(...{0:first,...rest}){return [this.tag,first,rest[1]]} \
         const functionResult=collect.call({tag:'f'},1,2); \
         const holder={tag:'m',method(...{0:first,...rest}){return [this.tag,first,rest[1]]},run(...values){const arrow=(...{0:first,...rest})=>[this.tag,first,rest[1]];return arrow(...values)}}; \
         const methodResult=holder.method(3,4);const arrowResult=holder.run(5,6); \
         export {functionResult,methodResult,arrowResult};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "55"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("...{"), "{}", out.bundle);
    assert!(
        out.bundle.contains("...__wake_t"),
        "native rest should collect into a collision-free binding: {}",
        out.bundle
    );
    assert!(
        out.bundle.contains("=>"),
        "arrow syntax should remain native: {}",
        out.bundle
    );
    assert!(!out.bundle.contains(".bind(this)"), "{}", out.bundle);

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_structured_rest_chrome55_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(JSON.stringify(r.functionResult)!=='[\"f\",1,2]'||JSON.stringify(r.methodResult)!=='[\"m\",3,4]'||JSON.stringify(r.arrowResult)!=='[\"m\",5,6]')process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "chrome 55 structured-rest runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn structured_rest_parameters_are_preserved_for_modern_targets() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "function collect(...{length,...rest}){return length+rest[1]}const arrow=(...[first,second])=>first*second;export const result=collect(2,3)+arrow(4,5);",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "120"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(out.bundle.contains("...{"), "{}", out.bundle);
    assert!(out.bundle.contains("...["), "{}", out.bundle);
    assert!(
        !out.bundle.contains("slice.call(arguments"),
        "{}",
        out.bundle
    );
    assert!(
        !out.bundle.contains("getOwnPropertySymbols"),
        "{}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_structured_rest_modern_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(r.result!==25)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "modern structured-rest runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn async_arrow_binding_parameters_lower_for_chrome_55() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "function build(){const fn=async ({head,...rest}={head:this.prefix,value:3},...{0:[first],1:second,...tail})=>[this.prefix,head,rest.value,first,second,tail[2],await Promise.resolve(7)];return fn(undefined,[4],5,6)}export const result=build.call({prefix:'P'});",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "55"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(out.bundle.contains("async"), "{}", out.bundle);
    assert!(out.bundle.contains("=>"), "{}", out.bundle);
    assert!(
        !out.bundle.contains("...{"),
        "structured async-arrow rest must lower: {}",
        out.bundle
    );
    assert!(
        out.bundle.contains("getOwnPropertySymbols"),
        "object rest helper must be emitted: {}",
        out.bundle
    );
    assert!(
        !out.bundle.contains("slice.call(arguments"),
        "native async-arrow rest collection must not read lexical arguments: {}",
        out.bundle
    );
    assert!(
        !out.bundle.contains(".bind(this)"),
        "the async arrow itself must remain lexical: {}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_async_arrow_params_chrome55_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "Promise.resolve(require({:?}).result).then(value=>{{if(JSON.stringify(value)!=='[\"P\",\"P\",3,4,5,6,7]')process.exit(2)}}).catch(error=>{{console.error(error);process.exit(3)}});",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "chrome 55 async-arrow parameter runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn async_arrow_binding_parameters_stay_conservative_for_chrome_40() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const fn=async ({head,...rest}={head:1,value:2},...{0:[first],...tail})=>[head,rest.value,first,tail[1]];export const result=fn(undefined,[3],4);",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(out.bundle.contains("async"), "{}", out.bundle);
    assert!(out.bundle.contains("=>"), "{}", out.bundle);
    assert!(
        out.bundle.contains("...{"),
        "unsupported async/arrow/parameter syntax must retain the whole parameter list: {}",
        out.bundle
    );
    assert!(
        !out.bundle.contains("slice.call(arguments"),
        "async-arrow rest must never be emulated through lexical arguments: {}",
        out.bundle
    );
    assert!(
        !out.bundle.contains("getOwnPropertySymbols"),
        "the conservative path must not allocate an unused object-rest helper: {}",
        out.bundle
    );
}

#[test]
fn async_arrow_binding_parameters_are_preserved_for_modern_targets() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const fn=async ({head,...rest}={head:1,value:2},...{0:[first],...tail})=>[head,rest.value,first,tail[1]];export const result=fn(undefined,[3],4);",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "120"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(out.bundle.contains("async"), "{}", out.bundle);
    assert!(out.bundle.contains("=>"), "{}", out.bundle);
    assert!(out.bundle.contains("...{"), "{}", out.bundle);
    assert!(
        !out.bundle.contains("getOwnPropertySymbols"),
        "{}",
        out.bundle
    );
    assert!(
        !out.bundle.contains("slice.call(arguments"),
        "{}",
        out.bundle
    );
}

#[test]
fn transformed_helpers_preserve_directive_prologue_in_minified_bundle() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        "wake-prologue";
        "use strict";
        var values=[0,...new Set([1,2])];
        var copy={...{value:3}};
        var strictThis=(function(){return this})()===void 0;
        var undeclaredThrows=false;
        try{__wake_directive_probe__=1}catch(error){undeclaredThrows=error instanceof ReferenceError}
        module.exports={values:values,copy:copy,strictThis:strictThis,undeclaredThrows:undeclaredThrows};
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler
        .set_target_env(wake_ecma_transform::TargetEnv::new(vec![
            wake_ecma_transform::BrowserTarget::new("chrome", "40"),
        ]))
        .enable_minify();
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("..."), "{}", out.bundle);

    let marker = out
        .bundle
        .find("\"wake-prologue\";")
        .expect("marker directive");
    let strict = out.bundle[marker..]
        .find("\"use strict\";")
        .map(|offset| marker + offset)
        .expect("source strict directive");
    let iterator = out.bundle[strict..]
        .find("(value, limit) {")
        .map(|offset| strict + offset)
        .expect("iterator helper");
    let object = out.bundle[iterator..]
        .find("(target) {for (var sourceIndex")
        .map(|offset| iterator + offset)
        .expect("object-spread helper");
    let body = out.bundle[object..]
        .find("var values=")
        .map(|offset| object + offset)
        .expect("first source body statement");
    assert!(
        marker < strict && strict < iterator && iterator < object && object < body,
        "directives, helpers, and source body are out of order:\n{}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_directive_helper_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(JSON.stringify(r.values)!=='[0,1,2]'||r.copy.value!==3||!r.strictThis||!r.undeclaredThrows)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "directive/helper runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn chrome_40_nullish_temporaries_are_local_to_each_execution_scope() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "\"use strict\";\
         let calls=0;function read(value){calls++;return value}\
         const programValue=read(null)??'program';\
         function outer(fallback){\"use strict\";\
           const local=read(null)??fallback;\
           function nested(){return read(null)??'nested'}\
           const arrow=()=>read(null)??'arrow';\
           return [local,nested(),arrow()]}\
         function context(arg){return [read(this.missing)??'this',read(arguments[0])??'arguments',new.target??'target']}\
         async function asynchronous(){return (await Promise.resolve(null))??'async'}\
         function* generated(){return (yield null)??'generator'}\
         const outerValue=outer('local');const contextValue=context.call({},null);\
         const iterator=generated();iterator.next();const generatorValue=iterator.next(null).value;\
         const asyncValue=asynchronous();\
         export {programValue,outerValue,contextValue,generatorValue,asyncValue,calls};",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("??"), "{}", out.bundle);
    assert!(
        out.bundle.matches("var __wake_t").count() >= 7,
        "each program/function/arrow execution scope must own its temp: {}",
        out.bundle
    );
    assert!(
        out.bundle.contains("  \"use strict\";\n  var __wake_t"),
        "program directive must precede its temp declaration: {}",
        out.bundle
    );
    assert!(
        out.bundle
            .contains("function outer(fallback) {\n    \"use strict\";\n    var __wake_t"),
        "function directive must precede its temp declaration: {}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_scoped_nullish_chrome40_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "(async()=>{{const r=require({:?});const a=await r.asyncValue;if(r.programValue!=='program'||JSON.stringify(r.outerValue)!=='[\"local\",\"nested\",\"arrow\"]'||JSON.stringify(r.contextValue)!=='[\"this\",\"arguments\",\"target\"]'||r.generatorValue!=='generator'||a!=='async'||r.calls!==6)process.exit(2)}})().catch(e=>{{console.error(e);process.exit(3)}});",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "scoped-nullish runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn parenthesized_concise_arrows_lower_inside_cover_without_leaking_parameter_temps() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        "use strict";
        let calls=0,holder;
        function read(value){calls++;return value}
        const nullish=((value)=>read(value)??"fallback");
        const optional=((value)=>value?.deep?.item);
        const replacement={tag:"replacement"};
        const original={
          tag:"original",
          get method(){holder=replacement;return function(a,b){return [this.tag,a,b]}}
        };
        const spread=((args)=>holder.method("head",...args));
        const parameter=((value=read(null)??"parameter")=>value);
        holder=original;
        const values=[
          nullish(null),nullish("value"),optional(null),optional({deep:{item:7}}),
          spread(["tail"]),parameter()
        ];
        export {values,calls};
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert_eq!(
        out.bundle.matches("??").count(),
        1,
        "only the default-parameter cover must remain conservative: {}",
        out.bundle
    );
    assert!(!out.bundle.contains("?."), "{}", out.bundle);
    assert!(!out.bundle.contains("...args"), "{}", out.bundle);
    assert!(
        out.bundle.matches("var __wake_t").count() >= 3,
        "each nested concise arrow must own its transform temps: {}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_cover_concise_arrow_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(JSON.stringify(r.values)!=='[\"fallback\",\"value\",null,7,[\"original\",\"head\",\"tail\"],\"parameter\"]'||r.calls!==3)process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cover concise-arrow runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn nullish_scoped_temporaries_are_absent_for_modern_and_conservative_regions() {
    let modern_fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const program=get()??1;function nested(){return get()??2}const arrow=()=>get()??3;export {program,nested,arrow};",
    )]));
    let mut modern = IncrementalBundler::new(modern_fs);
    modern.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "120"),
    ]));
    let modern_out = modern.build(Path::new("src/index.js"));
    assert!(!modern_out.has_errors(), "{:?}", modern_out.diagnostics);
    assert!(modern_out.bundle.contains("??"), "{}", modern_out.bundle);
    assert!(
        !modern_out.bundle.contains("__wake_t"),
        "modern output must not allocate transform temps: {}",
        modern_out.bundle
    );

    let conservative_fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const covered=(get()??1);const asyncArrow=async()=>((await get())??2);function defaulted(value=get()??3){return value}const arrowDefault=(value=get()??4)=>value;class Box{field=get()??5;static{this.value=get()??6}}export {covered,asyncArrow,defaulted,arrowDefault,Box};",
    )]));
    let mut conservative = IncrementalBundler::new(conservative_fs);
    conservative.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let conservative_out = conservative.build(Path::new("src/index.js"));
    assert!(
        !conservative_out.has_errors(),
        "{:?}",
        conservative_out.diagnostics
    );
    assert!(
        conservative_out.bundle.contains("??"),
        "cover/async-arrow/class regions are intentionally preserved until they own a safe scope: {}",
        conservative_out.bundle
    );
    assert!(
        !conservative_out.bundle.contains("var __wake_t"),
        "conservative regions must not leak scoped temps into Program: {}",
        conservative_out.bundle
    );
}

#[test]
fn arrow_parameter_initializers_preserve_lexical_context() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const holder={value:7,run(){const bound=(value=this.value)=>value;return bound()}};\
         function outer(value){const lexical=(item=arguments[0])=>item;return lexical()}\
         export const result=[holder.run(),outer(9)];",
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        out.bundle.contains(".bind(this)"),
        "a default using lexical this must bind the lowered function: {}",
        out.bundle
    );
    assert!(
        out.bundle.contains("=>"),
        "an arguments-dependent default must remain an arrow until lexical capture is available: {}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_arrow_parameter_lexical_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(JSON.stringify(r.result)!=='[7,9]')process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "arrow parameter lexical runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn chrome_40_member_spread_calls_use_scope_temps_and_preserve_native_order() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        "use strict";
        let holder,order=[],coercions=0;
        const replacement={tag:"replacement"};
        const original={
          tag:"original",
          get method(){
            order.push("get");
            holder=replacement;
            return function(a,b,c){order.push("call");return {receiver:this.tag,values:[a,b,c]}}
          }
        };
        const iterable={
          [Symbol.iterator](){
            order.push("iterator");
            let index=0;
            return {next(){order.push("next"+index);return index++===0?{done:false,value:"spread"}:{done:true}}}
          }
        };
        holder=original;
        const rebound=holder.method(...iterable);
        const reboundOrder=order.slice();
        order.length=0;
        const key={
          [Symbol.toPrimitive](hint){coercions++;order.push("key:"+hint);return "method"}
        };
        holder=original;
        const computed=holder[key](...iterable);
        const computedOrder=order.slice();
        function strictDirect(){"use strict";return this}
        const directThis=strictDirect(...[]);
        function context(first){
          if(new.target&&!this.tag)this.tag="constructed";
          holder=original;
          return holder.method(...[this.tag,arguments[0],new.target?"new":"call"])
        }
        const lexical=context.call({tag:"context"},"argument");
        const constructed=new context("constructor");
        async function asyncContext(value){
          holder=original;
          return holder.method(...[await value,"async","done"])
        }
        const pending=asyncContext(Promise.resolve("awaited"));
        function* generatorContext(){
          holder=original;
          return holder.method(...[yield "pause","generator","done"])
        }
        const generator=generatorContext();
        const yielded=generator.next().value;
        const generated=generator.next("resumed").value;
        let skippedSteps=0;
        const skippedIterable={
          [Symbol.iterator](){return {next(){skippedSteps++;return {done:true}}}}
        };
        const shortCircuited=false&&holder.method(...skippedIterable);
        const optionalSkipped=null?.method(...skippedIterable);
        export {rebound,reboundOrder,computed,computedOrder,coercions,directThis,lexical,constructed,pending,yielded,generated,shortCircuited,optionalSkipped,skippedSteps};
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("..."), "{}", out.bundle);
    assert!(!out.bundle.contains("?."), "{}", out.bundle);
    assert!(
        out.bundle.contains("var __wake_t") && out.bundle.contains(".apply(void 0"),
        "spread calls must use scope-owned vars and an undefined direct-call receiver: {}",
        out.bundle
    );
    assert!(
        !out.bundle.contains("(function (__wake_t"),
        "member spread calls must not use ordinary-function capture IIFEs: {}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_scoped_member_spread_chrome40_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});Promise.resolve(r.pending).then(p=>{{\
         const rebound={{receiver:'original',values:['spread',null,null]}};\
         const lexical={{receiver:'original',values:['context','argument','call']}};\
         const constructed={{receiver:'original',values:['constructed','constructor','new']}};\
         const pending={{receiver:'original',values:['awaited','async','done']}};\
         const generated={{receiver:'original',values:['resumed','generator','done']}};\
         if(JSON.stringify(r.rebound)!==JSON.stringify(rebound)||JSON.stringify(r.computed)!==JSON.stringify(rebound)||\
            JSON.stringify(r.reboundOrder)!=='[\"get\",\"iterator\",\"next0\",\"next1\",\"call\"]'||\
            JSON.stringify(r.computedOrder)!=='[\"key:string\",\"get\",\"iterator\",\"next0\",\"next1\",\"call\"]'||\
            r.coercions!==1||r.directThis!==undefined||JSON.stringify(r.lexical)!==JSON.stringify(lexical)||\
            JSON.stringify(r.constructed)!==JSON.stringify(constructed)||JSON.stringify(p)!==JSON.stringify(pending)||\
            r.yielded!=='pause'||JSON.stringify(r.generated)!==JSON.stringify(generated)||r.shortCircuited!==false||\
            r.optionalSkipped!==undefined||r.skippedSteps!==0)process.exit(2);\
         }}).catch(error=>{{console.error(error);process.exit(3)}});",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "scoped member-spread runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn arrow_capable_target_keeps_spread_captures_in_the_arrow_scope() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        const owner={
          tag:"outer",
          make(first){
            let holder;
            const replacement={tag:"replacement"};
            const original={tag:"original",get method(){holder=replacement;return function(a,b,c){return [this.tag,a,b,c]}}};
            holder=original;
            return args=>holder.method(this.tag,arguments[0],...args)
          }
        };
        const arrow=owner.make("argument");
        export const result=arrow(["tail"]);
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    // Chrome 45 supports arrows but still requires call-spread lowering.
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "45"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("...args"), "{}", out.bundle);
    assert!(
        out.bundle.contains("=> {") && out.bundle.contains("var __wake_t"),
        "the concise arrow must become a block that owns its spread temps: {}",
        out.bundle
    );
    assert!(
        !out.bundle.contains("(function (__wake_t"),
        "arrow-capable targets must not use an ordinary capture IIFE: {}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_scoped_member_spread_arrow_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(JSON.stringify(r.result)!=='[\"original\",\"outer\",\"argument\",\"tail\"]')process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "arrow-scoped member-spread runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn spread_calls_are_conservative_without_a_scope_and_native_for_modern_targets() {
    let conservative_fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        const holder={method(){return 1}},args=[];
        const covered=(holder.method(...args));
        function parameter(value=holder.method(...args)){return value}
        const asyncArrow=async()=>holder.method(...args);
        const asyncOptional=async()=>holder?.method(...args);
        class Box{field=holder.method(...args);static{holder.method(...args)}}
        export {covered,parameter,asyncArrow,asyncOptional,Box};
        "#,
    )]));
    let mut conservative = IncrementalBundler::new(conservative_fs);
    conservative.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let conservative_out = conservative.build(Path::new("src/index.js"));
    assert!(
        !conservative_out.has_errors(),
        "{:?}",
        conservative_out.diagnostics
    );
    assert!(
        conservative_out.bundle.matches("...args").count() >= 6
            && conservative_out.bundle.contains("holder?.method(...args)"),
        "cover/parameter/class/async-arrow spread calls must remain native: {}",
        conservative_out.bundle
    );
    assert!(
        !conservative_out.bundle.contains("(value, limit) {")
            && !conservative_out.bundle.contains("var __wake_t"),
        "conservative spread sites must not allocate unused helpers or temps: {}",
        conservative_out.bundle
    );

    let modern_fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "const holder={method(value){return value}},args=[1];export const result=holder.method(...args);",
    )]));
    let mut modern = IncrementalBundler::new(modern_fs);
    modern.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "120"),
    ]));
    let modern_out = modern.build(Path::new("src/index.js"));
    assert!(!modern_out.has_errors(), "{:?}", modern_out.diagnostics);
    assert!(
        modern_out.bundle.contains("holder.method(...args)"),
        "{}",
        modern_out.bundle
    );
    assert!(
        !modern_out.bundle.contains("(value, limit) {")
            && !modern_out.bundle.contains("var __wake_t"),
        "modern output must not allocate spread infrastructure: {}",
        modern_out.bundle
    );
}

#[test]
fn arrow_capable_optional_spread_keeps_await_and_yield_in_their_owner_scope() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        let holder,steps=0;
        const replacement={tag:"replacement"};
        const original={
          tag:"original",
          get method(){holder=replacement;return function(a,b,c){return {receiver:this.tag,values:[a,b,c]}}}
        };
        async function asyncRun(value){
          holder=original;
          return holder.method?.(...[await Promise.resolve(value),this.tag,arguments[0]])
        }
        function* generatorRun(value){
          holder=original;
          return holder.method?.(...[yield "pause",this.tag,arguments[0]])
        }
        async function asyncSkip(){return null?.method(...[await Promise.resolve(++steps)])}
        function* generatorSkip(){return null?.method(...[yield "bad"])}
        const pending=asyncRun.call({tag:"async-this"},"awaited");
        const generator=generatorRun.call({tag:"generator-this"},"argument");
        const yielded=generator.next().value;
        const generated=generator.next("resumed").value;
        const skipPending=asyncSkip();
        const skippedGenerator=generatorSkip().next();
        export {pending,yielded,generated,skipPending,skippedGenerator,steps};
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    // Chrome 70 supports arrows, async/generators and spread, but not optional chaining.
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "70"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(!out.bundle.contains("?."), "{}", out.bundle);
    assert!(
        out.bundle.contains("async function asyncRun")
            && out.bundle.contains("function* generatorRun")
            && out.bundle.contains("var __wake_t"),
        "async/generator functions must own optional-spread captures: {}",
        out.bundle
    );
    assert!(
        out.bundle.contains("...")
            && !out.bundle.contains("(value, limit) {")
            && !out.bundle.contains("=>"),
        "native spread must stay in place without a helper or capture arrow: {}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_optional_spread_async_generator_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});Promise.all([r.pending,r.skipPending]).then(values=>{{\
         const asyncValue={{receiver:'original',values:['awaited','async-this','awaited']}};\
         const generated={{receiver:'original',values:['resumed','generator-this','argument']}};\
         if(JSON.stringify(values[0])!==JSON.stringify(asyncValue)||values[1]!==undefined||\
            r.yielded!=='pause'||JSON.stringify(r.generated)!==JSON.stringify(generated)||\
            !r.skippedGenerator.done||r.skippedGenerator.value!==undefined||r.steps!==0)process.exit(2);\
         }}).catch(error=>{{console.error(error);process.exit(3)}});",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "optional-spread async/generator runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn parameter_lowering_preserves_function_body_environment_boundaries() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        r#"
        const outer="outer";
        function lexical(value=outer){let outer="inner";return value}
        function variable(value=outer){var outer="inner";return value}
        function declaration(value=outer){function outer(){return "inner"}return value}
        const arrow=(value=outer)=>{const outer="inner";return value};
        const holder={
          method({value=outer}={}){class outer{};return value},
          computed({[outer]:value}={outer:9}){let outer="inner";return value}
        };
        export const results=[lexical(),variable(),declaration(),arrow(),holder.method(),holder.computed()];
        "#,
    )]));
    let mut bundler = IncrementalBundler::new(fs);
    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let out = bundler.build(Path::new("src/index.js"));
    assert!(!out.has_errors(), "{:?}", out.diagnostics);
    assert!(
        out.bundle.matches("value = outer").count() >= 5
            && out.bundle.contains("[outer]")
            && out.bundle.contains("=>"),
        "conflicting parameter lists and arrow syntax must remain native: {}",
        out.bundle
    );
    assert!(
        !out.bundle.contains("var __wake_t")
            && !out.bundle.contains("(value, limit) {")
            && !out.bundle.contains("getOwnPropertySymbols"),
        "a conservative parameter site must not allocate lowering infrastructure: {}",
        out.bundle
    );

    if !node_available() {
        return;
    }
    let dir = std::env::temp_dir().join("wake_parameter_environment_chrome40_e2e");
    std::fs::create_dir_all(&dir).unwrap();
    let bundle_path = dir.join("bundle.cjs");
    std::fs::write(&bundle_path, &out.bundle).unwrap();
    let script = format!(
        "const r=require({:?});if(JSON.stringify(r.results)!=='[\"outer\",\"outer\",\"outer\",\"outer\",\"outer\",9]')process.exit(2);",
        bundle_path.to_string_lossy()
    );
    let output = std::process::Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "parameter environment runtime failed: {}\n{}",
        String::from_utf8_lossy(&output.stderr),
        out.bundle
    );
}

#[test]
fn async_arrow_parameter_environment_conflict_is_target_gated() {
    let conflict_source = r#"
        let outer="outer";
        const fn=async ({x=outer,...rest}={})=>{let outer="inner";return [x,Object.keys(rest).length]};
        export const result=fn();
    "#;
    let mut conflicting = IncrementalBundler::new(Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        conflict_source,
    )])));
    conflicting.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "55"),
    ]));
    let conflict_out = conflicting.build(Path::new("src/index.js"));
    assert!(!conflict_out.has_errors(), "{:?}", conflict_out.diagnostics);
    assert!(
        conflict_out.bundle.contains("...rest") && conflict_out.bundle.contains("async"),
        "the conflicting Chrome 55 async-arrow parameter must remain native: {}",
        conflict_out.bundle
    );
    assert!(
        !conflict_out.bundle.contains("getOwnPropertySymbols")
            && !conflict_out.bundle.contains("var __wake_t"),
        "the conflict gate must run before helper/temp allocation: {}",
        conflict_out.bundle
    );

    let safe_source = r#"
        let outer="outer";
        const fn=async (first=outer,{x="safe",...rest}={extra:1})=>{let outer="inner";return [first,x,rest.extra]};
        export const result=fn();
    "#;
    let mut chrome_55 = IncrementalBundler::new(Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        safe_source,
    )])));
    chrome_55.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "55"),
    ]));
    let chrome_55_out = chrome_55.build(Path::new("src/index.js"));
    assert!(
        !chrome_55_out.has_errors(),
        "{:?}",
        chrome_55_out.diagnostics
    );
    assert!(
        !chrome_55_out.bundle.contains("...rest")
            && chrome_55_out.bundle.contains("getOwnPropertySymbols")
            && chrome_55_out.bundle.contains("first = outer"),
        "an unrelated native default must not block safe Chrome 55 object-rest lowering: {}",
        chrome_55_out.bundle
    );

    let mut modern = IncrementalBundler::new(Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        safe_source,
    )])));
    modern.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "120"),
    ]));
    let modern_out = modern.build(Path::new("src/index.js"));
    assert!(!modern_out.has_errors(), "{:?}", modern_out.diagnostics);
    assert!(
        modern_out.bundle.contains("...rest")
            && !modern_out.bundle.contains("getOwnPropertySymbols")
            && !modern_out.bundle.contains("var __wake_t"),
        "modern targets must keep native parameters without lowering infrastructure: {}",
        modern_out.bundle
    );

    if !node_available() {
        return;
    }
    for (name, output, expected) in [
        ("conflict", &conflict_out, "[\"outer\",0]"),
        ("safe", &chrome_55_out, "[\"outer\",\"safe\",1]"),
        ("modern", &modern_out, "[\"outer\",\"safe\",1]"),
    ] {
        let dir = std::env::temp_dir().join(format!("wake_parameter_environment_{name}_e2e"));
        std::fs::create_dir_all(&dir).unwrap();
        let bundle_path = dir.join("bundle.cjs");
        std::fs::write(&bundle_path, &output.bundle).unwrap();
        let script = format!(
            "Promise.resolve(require({:?}).result).then(value=>{{if(JSON.stringify(value)!=={:?})process.exit(2)}}).catch(error=>{{console.error(error);process.exit(3)}});",
            bundle_path.to_string_lossy(),
            expected
        );
        let result = std::process::Command::new("node")
            .arg("-e")
            .arg(script)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{name} async-arrow parameter runtime failed: {}\n{}",
            String::from_utf8_lossy(&result.stderr),
            output.bundle
        );
    }
}

#[test]
fn synchronous_for_of_lowering_preserves_iterator_close_labels_bindings_and_tdz() {
    let source = r#"
        function tracked(values,options){
          options=options||{};
          let index=0;
          const stats={iteratorCalls:0,nextGets:0,nextCalls:0,returnGets:0,returnCalls:0,nextThis:true,returnThis:true};
          let iterator;
          iterator={
            get next(){
              stats.nextGets++;
              return function(){
                stats.nextThis=stats.nextThis&&this===iterator;
                stats.nextCalls++;
                if(options.nextThrow)throw new Error("next");
                if(options.primitiveStep)return 1;
                if(index>=values.length)return {done:true};
                const value=values[index++];
                if(options.valueThrow)return {done:false,get value(){throw new Error("value")}};
                return {done:false,value};
              };
            },
            get return(){
              stats.returnGets++;
              return function(){
                stats.returnThis=stats.returnThis&&this===iterator;
                stats.returnCalls++;
                if(options.closeThrow)throw new Error("close");
                if(options.returnPrimitive)return 1;
                return {done:true};
              };
            }
          };
          const iterable={
            [Symbol.iterator](){stats.iteratorCalls++;return iterator}
          };
          return {iterable,stats};
        }

        const normal=tracked([1,2]);
        const normalValues=[];
        for(const value of normal.iterable)normalValues.push(value);

        const broken=tracked([1,2]);
        for(const value of broken.iterable){if(value===1)break}

        const continued=tracked([1,2]);
        for(const value of continued.iterable){if(value===1)continue}

        const returned=tracked([7,8]);
        function first(){for(const value of returned.iterable)return value}
        const firstValue=first();

        const bodyFailure=tracked([1],{closeThrow:true});
        let bodyMessage="";
        try{for(const value of bodyFailure.iterable){throw new Error("body")}}catch(error){bodyMessage=error.message}

        const closeFailure=tracked([1],{closeThrow:true});
        let closeMessage="";
        try{for(const value of closeFailure.iterable)break}catch(error){closeMessage=error.message}

        const closePrimitive=tracked([1],{returnPrimitive:true});
        let closePrimitiveType=false;
        try{for(const value of closePrimitive.iterable)break}catch(error){closePrimitiveType=error instanceof TypeError}

        const nextFailure=tracked([1],{nextThrow:true});
        let nextMessage="";
        try{for(const value of nextFailure.iterable){}}catch(error){nextMessage=error.message}

        const primitiveStep=tracked([1],{primitiveStep:true});
        let primitiveStepType=false;
        try{for(const value of primitiveStep.iterable){}}catch(error){primitiveStepType=error instanceof TypeError}

        const valueFailure=tracked([1],{valueThrow:true});
        let valueMessage="";
        try{for(const value of valueFailure.iterable){}}catch(error){valueMessage=error.message}

        const bindingFailure=tracked([null]);
        let bindingFailureType=false;
        try{for(const {} of bindingFailure.iterable){}}catch(error){bindingFailureType=error instanceof TypeError}

        const nested=[];
        outer: for(const outerValue of [1,2]){
          const current=tracked([1,2]);
          nested.push(current.stats);
          inner: for(const innerValue of current.iterable){
            if(innerValue===1)continue outer;
          }
        }
        const sameLoop=tracked([1,2]);
        firstLabel: secondLabel: for(const value of sameLoop.iterable){
          if(value===1)continue firstLabel;
        }

        const closures=[];
        for(let value of [1,2])closures.push(()=>value);
        var varValue=0;
        for(var varValue of [3,4]){}
        let assigned=0;
        for(assigned of [5,6]){}
        let destructured=0;
        for({x:destructured} of [{x:7}]){}

        let shadow=[1];
        let directTdz=false;
        try{for(let shadow of shadow){}}catch(error){directTdz=error instanceof ReferenceError}
        let captured;
        for(let lexical of (captured=()=>lexical,[1]))break;
        let closureTdz=false;
        try{captured()}catch(error){closureTdz=error instanceof ReferenceError}

        let rhsCalls=0;
        function rhs(){rhsCalls++;return [1,2]}
        for(const value of rhs()){}

        const baseStatsOk=record=>record.stats.iteratorCalls===1&&record.stats.nextGets===1&&record.stats.nextThis&&record.stats.returnThis;
        const ok=JSON.stringify(normalValues)==="[1,2]"&&baseStatsOk(normal)&&normal.stats.nextCalls===3&&normal.stats.returnCalls===0&&
          baseStatsOk(broken)&&broken.stats.nextCalls===1&&broken.stats.returnGets===1&&broken.stats.returnCalls===1&&
          baseStatsOk(continued)&&continued.stats.nextCalls===3&&continued.stats.returnCalls===0&&
          firstValue===7&&baseStatsOk(returned)&&returned.stats.returnCalls===1&&
          bodyMessage==="body"&&bodyFailure.stats.returnCalls===1&&closeMessage==="close"&&closeFailure.stats.returnCalls===1&&
          closePrimitiveType&&closePrimitive.stats.returnCalls===1&&nextMessage==="next"&&nextFailure.stats.returnCalls===0&&
          primitiveStepType&&primitiveStep.stats.returnCalls===0&&valueMessage==="value"&&valueFailure.stats.returnCalls===0&&
          bindingFailureType&&bindingFailure.stats.returnCalls===1&&
          nested.length===2&&nested.every(stats=>stats.returnCalls===1)&&sameLoop.stats.returnCalls===0&&
          JSON.stringify(closures.map(read=>read()))==="[1,2]"&&varValue===4&&assigned===6&&destructured===7&&
          directTdz&&closureTdz&&rhsCalls===1;
        export {ok};
    "#;
    let mut readable = IncrementalBundler::new(Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        source,
    )])));
    readable.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "50"),
    ]));
    let readable_out = readable.build(Path::new("src/index.js"));
    assert!(!readable_out.has_errors(), "{:?}", readable_out.diagnostics);
    assert!(
        readable_out
            .bundle
            .contains("Iterator result is not an object"),
        "for-of helper must be injected: {}",
        readable_out.bundle
    );
    assert!(
        !readable_out.bundle.contains(" of "),
        "all synchronous for-of statements should be lowered: {}",
        readable_out.bundle
    );

    let mut minified = IncrementalBundler::new(Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        source,
    )])));
    minified
        .set_target_env(wake_ecma_transform::TargetEnv::new(vec![
            wake_ecma_transform::BrowserTarget::new("chrome", "50"),
        ]))
        .enable_minify()
        .enable_mangle();
    let minified_out = minified.build(Path::new("src/index.js"));
    assert!(!minified_out.has_errors(), "{:?}", minified_out.diagnostics);

    if !node_available() {
        return;
    }
    for (name, output) in [("readable", &readable_out), ("minified", &minified_out)] {
        let dir = std::env::temp_dir().join(format!("wake_for_of_{name}_e2e"));
        std::fs::create_dir_all(&dir).unwrap();
        let bundle_path = dir.join("bundle.cjs");
        std::fs::write(&bundle_path, &output.bundle).unwrap();
        let script = format!(
            "const result=require({:?});if(!result.ok){{console.error(result);process.exit(2)}}",
            bundle_path.to_string_lossy()
        );
        let result = std::process::Command::new("node")
            .arg("-e")
            .arg(script)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{name} for-of runtime failed: {}\n{}",
            String::from_utf8_lossy(&result.stderr),
            output.bundle
        );
    }
}

#[test]
fn for_of_target_gate_and_conservative_async_using_boundaries() {
    let sync_source = "let total=0;for(const value of [1,2])total+=value;export {total};";
    let mut old = IncrementalBundler::new(Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        sync_source,
    )])));
    old.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "50"),
    ]));
    let old_out = old.build(Path::new("src/index.js"));
    assert!(!old_out.has_errors(), "{:?}", old_out.diagnostics);
    assert!(old_out.bundle.contains("Iterator result is not an object"));
    assert!(!old_out.bundle.contains(" of "), "{}", old_out.bundle);

    let mut native = IncrementalBundler::new(Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        sync_source,
    )])));
    native.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "51"),
    ]));
    let native_out = native.build(Path::new("src/index.js"));
    assert!(!native_out.has_errors(), "{:?}", native_out.diagnostics);
    assert!(native_out.bundle.contains(" of "), "{}", native_out.bundle);
    assert!(
        !native_out
            .bundle
            .contains("Iterator result is not an object")
    );

    let boundary_source = r#"
        async function consume(values){for await(const value of values){use(value)}}
        for(using resource of resources){use(resource)}
        export {consume};
    "#;
    let mut boundaries = IncrementalBundler::new(Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        boundary_source,
    )])));
    boundaries.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "40"),
    ]));
    let boundary_out = boundaries.build(Path::new("src/index.js"));
    assert!(!boundary_out.has_errors(), "{:?}", boundary_out.diagnostics);
    assert!(boundary_out.bundle.contains("for await ("));
    assert!(
        boundary_out
            .bundle
            .contains("for (using resource of resources)")
    );
    assert!(
        !boundary_out
            .bundle
            .contains("Iterator result is not an object"),
        "preserved async/using loops must not allocate the sync helper: {}",
        boundary_out.bundle
    );
}

#[test]
fn default_browser_baseline_and_runtime_target_switch_do_not_reuse_old_ast() {
    let fs = Arc::new(MemoryFileSystem::from_files([(
        "src/index.js",
        "let total=0;for(const value of [1,2])total+=value;const nested={value:total};export const result=nested?.value;",
    )]));
    let mut bundler = IncrementalBundler::new(fs);

    let baseline = bundler.build(Path::new("src/index.js"));
    assert!(!baseline.has_errors(), "{:?}", baseline.diagnostics);
    assert!(baseline.bundle.contains(" of "), "{}", baseline.bundle);
    assert!(baseline.bundle.contains("?."), "{}", baseline.bundle);
    assert!(
        !baseline.bundle.contains("Iterator result is not an object"),
        "{}",
        baseline.bundle
    );

    bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
        wake_ecma_transform::BrowserTarget::new("chrome", "50"),
    ]));
    let legacy = bundler.build(Path::new("src/index.js"));
    assert!(!legacy.has_errors(), "{:?}", legacy.diagnostics);
    assert!(!legacy.bundle.contains(" of "), "{}", legacy.bundle);
    assert!(!legacy.bundle.contains("?."), "{}", legacy.bundle);
    assert!(
        legacy.bundle.contains("Iterator result is not an object"),
        "{}",
        legacy.bundle
    );

    bundler.set_target_env(wake_ecma_transform::TargetEnv::default());
    let restored = bundler.build(Path::new("src/index.js"));
    assert!(!restored.has_errors(), "{:?}", restored.diagnostics);
    assert!(restored.bundle.contains(" of "), "{}", restored.bundle);
    assert!(restored.bundle.contains("?."), "{}", restored.bundle);
    assert!(
        !restored.bundle.contains("Iterator result is not an object"),
        "{}",
        restored.bundle
    );
}

#[test]
fn lowered_for_of_destructuring_head_preserves_rhs_tdz() {
    let source = r#"
        let shadow=[{shadow:1}];
        let directTdz=false;
        try{
          for(let {shadow} of shadow){}
        }catch(error){
          directTdz=error instanceof ReferenceError;
        }

        let captured;
        for(let {value:inner} of (captured=()=>inner,[{value:1}]))break;
        let closureTdz=false;
        try{
          captured();
        }catch(error){
          closureTdz=error instanceof ReferenceError;
        }

        export const ok=directTdz&&closureTdz;
    "#;

    for (name, minify) in [("readable", false), ("minified", true)] {
        let mut bundler = IncrementalBundler::new(Arc::new(MemoryFileSystem::from_files([(
            "src/index.js",
            source,
        )])));
        bundler.set_target_env(wake_ecma_transform::TargetEnv::new(vec![
            wake_ecma_transform::BrowserTarget::new("chrome", "40"),
        ]));
        if minify {
            bundler.enable_minify().enable_mangle();
        }
        let output = bundler.build(Path::new("src/index.js"));
        assert!(!output.has_errors(), "{:?}", output.diagnostics);
        assert!(
            !output.bundle.contains(" of "),
            "sync for-of must be lowered after destructuring: {}",
            output.bundle
        );

        if !node_available() {
            continue;
        }
        let dir = std::env::temp_dir().join(format!("wake_for_of_destructuring_tdz_{name}_e2e"));
        std::fs::create_dir_all(&dir).unwrap();
        let bundle_path = dir.join("bundle.cjs");
        std::fs::write(&bundle_path, &output.bundle).unwrap();
        let script = format!(
            "const result=require({:?});if(!result.ok){{console.error(result);process.exit(2)}}",
            bundle_path.to_string_lossy()
        );
        let result = std::process::Command::new("node")
            .arg("-e")
            .arg(script)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{name} destructuring for-of TDZ runtime failed: {}\n{}",
            String::from_utf8_lossy(&result.stderr),
            output.bundle
        );
    }
}
