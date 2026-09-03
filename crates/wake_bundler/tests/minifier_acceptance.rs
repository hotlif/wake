use std::process::{Command, Output};
use std::sync::Arc;

use wake_bundler::{BuildOptions, BuildOutput, BuildRequest, BuildSession};
use wake_common::{Interner, MemoryFileSystem};
use wake_ecma_ast::SourceType;

const JSX_RUNTIME: &str = r#"
function element(type, props) { return { type, props: props || {} }; }
exports.jsx = element;
exports.jsxs = element;
exports.Fragment = "owned-fragment";
"#;

fn build(files: &[(&str, &str)], entry: &str, minify: bool, source_map: bool) -> BuildOutput {
    let fs = MemoryFileSystem::new();
    for (path, source) in files {
        fs.insert(path, source.as_bytes().to_vec());
    }
    let output = BuildSession::new_one_shot(
        Arc::new(fs),
        BuildOptions {
            minify,
            source_map,
            ..BuildOptions::default()
        },
    )
    .build_once(BuildRequest::new(entry));
    assert!(
        !output.has_errors(),
        "[{entry}/minify={minify}/map={source_map}] build failed: {:?}",
        output.diagnostics
    );
    output
}

fn build_with_boolean_define(
    files: &[(&str, &str)],
    entry: &str,
    minify: bool,
    flag: bool,
) -> BuildOutput {
    let fs = MemoryFileSystem::new();
    for (path, source) in files {
        fs.insert(path, source.as_bytes().to_vec());
    }
    let output = BuildSession::new_one_shot(
        Arc::new(fs),
        BuildOptions {
            define: vec![("FLAG".into(), flag.to_string())],
            minify,
            dead_module_elimination: true,
            ..BuildOptions::default()
        },
    )
    .build_once(BuildRequest::new(entry));
    assert!(
        !output.has_errors(),
        "[{entry}/minify={minify}/FLAG={flag}] build failed: {:?}",
        output.diagnostics
    );
    output
}

fn assert_reparses(case: &str, mode: &str, code: &str) {
    let interner = Interner::new();
    let parsed = wake_ecma_parser::parse(code, &interner, SourceType::Script);
    assert!(
        !parsed.has_errors(),
        "[{case}/{mode}] emitted JavaScript must reparse: {:?}\n--- code ---\n{code}",
        parsed.diagnostics
    );
}

fn normalized_javascript(code: &str) -> &str {
    let body = code
        .rfind("//# sourceMappingURL=")
        .map_or(code, |trailer| &code[..trailer]);
    body.trim_end_matches(['\r', '\n'])
}

/// Extracts the sole optimized module body from Wake's runtime table. Runtime, wrapper headers,
/// and any source-map trailer are deliberately outside the size budget.
fn normalized_single_module_payload(code: &str) -> &str {
    let code = normalized_javascript(code);
    let table = code
        .find("var t={")
        .expect("minified bundle must contain the runtime module table");
    let wrapper = code[table..]
        .find(":function(")
        .map(|offset| table + offset)
        .expect("single-module table must contain its function wrapper");
    let payload_start = code[wrapper..]
        .find("){")
        .map(|offset| wrapper + offset + 2)
        .expect("module wrapper must have a body");
    let payload_end = matching_closing_brace(code, payload_start - 1)
        .expect("module wrapper must have a balanced body");
    assert!(
        payload_start <= payload_end,
        "invalid module payload bounds"
    );
    &code[payload_start..payload_end]
}

fn matching_closing_brace(code: &str, opening: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut index = opening;
    while index < bytes.len() {
        let byte = bytes[index];
        let next = bytes.get(index + 1).copied();
        if line_comment {
            if byte == b'\n' || byte == b'\r' {
                line_comment = false;
            }
        } else if block_comment {
            if byte == b'*' && next == Some(b'/') {
                block_comment = false;
                index += 1;
            }
        } else if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == delimiter {
                quote = None;
            }
        } else if byte == b'/' && next == Some(b'/') {
            line_comment = true;
            index += 1;
        } else if byte == b'/' && next == Some(b'*') {
            block_comment = true;
            index += 1;
        } else if matches!(byte, b'\'' | b'"' | b'`') {
            quote = Some(byte);
        } else if byte == b'{' {
            depth += 1;
        } else if byte == b'}' {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn execute_and_observe_exports(bundle: &str) -> Output {
    let observer = r#"
Promise.resolve(module.exports).then(
  value => process.stdout.write("__WAKE_EXPORT__" + JSON.stringify(value)),
  error => { process.stderr.write("__WAKE_REJECTION__" + error.name); process.exitCode = 23; }
);
"#;
    Command::new("node")
        .arg("-e")
        .arg(format!("{bundle}\n{observer}"))
        .output()
        .expect("node was available immediately before execution")
}

fn assert_runtime_differential(
    case: &str,
    files: &[(&str, &str)],
    entry: &str,
    expected_stdout: &str,
) {
    let readable = build(files, entry, false, false);
    let optimized = build(files, entry, true, false);
    assert_reparses(case, "readable", &readable.bundle);
    assert_reparses(case, "optimized", &optimized.bundle);

    if !node_available() {
        eprintln!("node unavailable; [{case}] retained mandatory build/reparse coverage");
        return;
    }

    let readable_observation = execute_and_observe_exports(&readable.bundle);
    let optimized_observation = execute_and_observe_exports(&optimized.bundle);
    assert_eq!(
        optimized_observation.status.code(),
        readable_observation.status.code(),
        "[{case}] exception/exit behavior changed\nreadable stderr: {}\noptimized stderr: {}",
        String::from_utf8_lossy(&readable_observation.stderr),
        String::from_utf8_lossy(&optimized_observation.stderr)
    );
    assert_eq!(
        optimized_observation.stdout, readable_observation.stdout,
        "[{case}] logs, exports, or side-effect order changed"
    );
    assert_eq!(
        optimized_observation.stderr, readable_observation.stderr,
        "[{case}] stderr changed"
    );
    assert!(
        readable_observation.status.success(),
        "[{case}] readable bundle failed: {}",
        String::from_utf8_lossy(&readable_observation.stderr)
    );
    assert_eq!(
        String::from_utf8(readable_observation.stdout)
            .expect("fixture output is UTF-8")
            .replace("\r\n", "\n"),
        expected_stdout
    );
}

fn assert_defined_runtime_differential(
    case: &str,
    files: &[(&str, &str)],
    entry: &str,
    flag: bool,
    expected_stdout: &str,
) {
    let readable = build_with_boolean_define(files, entry, false, flag);
    let optimized = build_with_boolean_define(files, entry, true, flag);
    assert_reparses(case, "readable", &readable.bundle);
    assert_reparses(case, "optimized", &optimized.bundle);
    assert_eq!(
        readable.module_count, 2,
        "only the selected dependency is live"
    );
    assert_eq!(
        optimized.module_count, 2,
        "only the selected dependency is live"
    );

    if !node_available() {
        return;
    }
    let readable_observation = execute_and_observe_exports(&readable.bundle);
    let optimized_observation = execute_and_observe_exports(&optimized.bundle);
    assert_eq!(optimized_observation.status, readable_observation.status);
    assert_eq!(optimized_observation.stdout, readable_observation.stdout);
    assert_eq!(optimized_observation.stderr, readable_observation.stderr);
    assert!(
        readable_observation.status.success(),
        "[{case}] readable bundle failed: {}",
        String::from_utf8_lossy(&readable_observation.stderr)
    );
    assert_eq!(
        String::from_utf8(readable_observation.stdout).expect("fixture output is UTF-8"),
        expected_stdout
    );
}

#[test]
fn scope_concat_keeps_sequence_head_require_syntactically_valid() {
    let files = [
        (
            "src/index.js",
            "import './hooks.js'; export default globalThis.__reg.hooks + globalThis.__reg.runtime;",
        ),
        (
            "src/hooks.js",
            "import { unused } from './runtime.js'; globalThis.__reg || (globalThis.__reg = {}); globalThis.__reg.hooks = 2;",
        ),
        (
            "src/runtime.js",
            "globalThis.__reg || (globalThis.__reg = {}); globalThis.__reg.runtime = 1; export function unused() {}",
        ),
    ];
    assert_runtime_differential(
        "scope-concat-sequence-head-require",
        &files,
        "src/index.js",
        "__WAKE_EXPORT__{\"default\":3}",
    );
}

#[test]
fn eager_registry_imports_stay_conservative_and_keep_sourcemap_bytes_stable() {
    let files = [
        (
            "src/index.js",
            r#"import "./a.js";import "./b.js";export default [globalThis.__reg.a,globalThis.__reg.b];"#,
        ),
        (
            "src/a.js",
            "globalThis.__reg || (globalThis.__reg = {});globalThis.__reg.a = 1;",
        ),
        (
            "src/b.js",
            "globalThis.__reg || (globalThis.__reg = {});globalThis.__reg.b = 2;",
        ),
    ];
    let optimized = build(&files, "src/index.js", true, false);
    let mapped = build(&files, "src/index.js", true, true);

    assert_reparses(
        "structured-eager-registry-request",
        "optimized",
        &optimized.bundle,
    );
    assert_eq!(
        normalized_javascript(&mapped.bundle),
        normalized_javascript(&optimized.bundle),
        "source-map collection changed the optimized JavaScript body"
    );
    assert!(
        optimized
            .bundle
            .contains("__wake_require__(1),__wake_require__(2)"),
        "non-ESM side-effect modules must retain their typed eager requests: {}",
        optimized.bundle
    );
    if node_available() {
        let observation = execute_and_observe_exports(&optimized.bundle);
        assert!(
            observation.status.success(),
            "optimized registry fixture failed: {}",
            String::from_utf8_lossy(&observation.stderr)
        );
        assert_eq!(
            String::from_utf8(observation.stdout).unwrap(),
            r#"__WAKE_EXPORT__{"default":[1,2]}"#
        );
    }
}

#[test]
fn owned_lowering_matrix_builds_minifies_and_reparses() {
    type OwnedCase<'a> = (&'a str, &'a str, &'a [(&'a str, &'a str)]);
    let cases: &[OwnedCase<'_>] = &[
        (
            "javascript-control-and-expressions",
            "src/control.js",
            &[(
                "src/control.js",
                r#"
class Counter {
  #value = 1;
  step(delta = 1) { this.#value += delta; return this.#value; }
}
function* sequence(seed = 1) { yield seed; yield* [seed + 1]; }
async function later({ value } = { value: 1 }) { return await Promise.resolve(value); }
let total = 0;
outer: for (let index = 0; index < 4; index++) {
  try {
    switch (index) {
      case 0: total += 1; continue outer;
      case 1: total += 10; break;
      default: total += 100; break outer;
    }
  } finally { total += 1000; }
}
const input = { nested: { value: 2 } };
export const result = {
  total,
  bigint: (40n + 2n).toString(),
  nan: Number.isNaN(NaN),
  negativeZero: Object.is(-0, -0),
  optional: input?.nested?.value ?? 0,
  counter: new Counter().step(),
  iterator: sequence().next().value,
  later
};
"#,
            )],
        ),
        (
            "typescript-decorators-and-erasure",
            "src/model.ts",
            &[(
                "src/model.ts",
                r#"
interface Named { name: string }
type Pair<T> = readonly [T, T];
enum Direction { Left = 1, Right = 2 }
const seen: string[] = [];
function mark(value: any, context: any): any { seen.push(context.kind + ":" + context.name); return value; }
@mark class Model<T extends Named> {
  constructor(public readonly value: T) {}
  @mark read({ name = this.value.name }: Partial<Named> = {}): Pair<string> { return [name, name]; }
}
export const result: Pair<string> = new Model({ name: "wake" }).read();
export const direction: number = Direction.Right;
export { seen };
"#,
            )],
        ),
        (
            "jsx-automatic-runtime",
            "src/view.jsx",
            &[
                (
                    "src/view.jsx",
                    r#"
const items = ["one", "two"];
export const view = <article data-kind="owned">{items.map(item => <span key={item}>{item}</span>)}</article>;
"#,
                ),
                ("node_modules/react/jsx-runtime.js", JSX_RUNTIME),
            ],
        ),
        (
            "tsx-generic-and-jsx-lowering",
            "src/view.tsx",
            &[
                (
                    "src/view.tsx",
                    r#"
interface Item { id: string; label?: string }
const identity = <T extends Item>(item: T): T => item;
const item = identity({ id: "wake", label: "Wake" });
export const view = <section id={item.id}>{item.label ?? "missing"}</section>;
"#,
                ),
                ("node_modules/react/jsx-runtime.js", JSX_RUNTIME),
            ],
        ),
        (
            "explicit-resource-management",
            "src/resources.ts",
            &[(
                "src/resources.ts",
                r#"
export async function consume(resource: Disposable, asyncResource: AsyncDisposable): Promise<number> {
  using first = resource;
  await using second = asyncResource;
  void first;
  void second;
  return 42;
}
"#,
            )],
        ),
    ];

    for (name, entry, files) in cases {
        for (mode, minify) in [("readable", false), ("optimized", true)] {
            let output = build(files, entry, minify, false);
            assert_reparses(name, mode, &output.bundle);
        }
    }
}

#[test]
fn optimized_runtime_matches_readable_for_control_closures_and_primitives() {
    let files = &[(
        "src/index.js",
        r#"
const events = [];
function classify({ limit = 4 } = {}) {
  let total = 0;
  outer: for (let index = 0; index < limit; index++) {
    try {
      switch (index) {
        case 0: total += 1; continue outer;
        case 1: total += 10; break;
        default: total += 100; break outer;
      }
      total += 2;
    } finally {
      events.push("finally:" + index);
      total += 1000;
    }
  }
  return total;
}
function defaults({ left = 2, nested: { right = 3 } = {} } = {}, sum = left + right) {
  return sum;
}
function* sequence(seed = 2) {
  try { yield seed; return seed + 1; }
  finally { events.push("generator"); }
}
class Counter {
  #value;
  constructor(value) { this.#value = value; }
  increment() { return ++this.#value; }
}
let exception;
try { null.missing; } catch (error) { exception = error.name; }
const iterator = sequence();
const first = iterator.next();
const second = iterator.next();
const source = { nested: { value: 7 } };
export const snapshot = {
  total: classify(),
  counter: new Counter(41).increment(),
  defaulted: defaults(),
  optional: source?.nested?.value ?? 0,
  bigint: (40n + 2n).toString(),
  nan: Number.isNaN(NaN),
  negativeZero: Object.is(-0, -0),
  exception,
  first,
  second,
  events
};
console.log("events=" + events.join(","));
"#,
    )];
    assert_runtime_differential(
        "control-closures-primitives",
        files,
        "src/index.js",
        concat!(
            "events=generator,finally:0,finally:1,finally:2\n",
            "__WAKE_EXPORT__{\"snapshot\":{\"total\":3113,\"counter\":42,",
            "\"defaulted\":5,\"optional\":7,\"bigint\":\"42\",\"nan\":true,",
            "\"negativeZero\":true,\"exception\":\"TypeError\",",
            "\"first\":{\"value\":2,\"done\":false},",
            "\"second\":{\"value\":3,\"done\":true},",
            "\"events\":[\"generator\",\"finally:0\",\"finally:1\",\"finally:2\"]}}"
        ),
    );
}

#[test]
fn optimized_runtime_matches_readable_for_esm_cycles() {
    let esm_cycle = &[
        (
            "src/index.js",
            "import { fromA } from './a.js'; export const result = fromA();",
        ),
        (
            "src/a.js",
            "import { fromB } from './b.js'; export const tokenA = 'A'; export function fromA() { return 'A' + fromB(); }",
        ),
        (
            "src/b.js",
            "import { tokenA } from './a.js'; export function fromB() { return 'B' + tokenA; }",
        ),
    ];
    assert_runtime_differential(
        "esm-cycle",
        esm_cycle,
        "src/index.js",
        "__WAKE_EXPORT__{\"result\":\"ABA\"}",
    );
}

#[test]
fn bundled_default_import_tracks_a_mutable_internal_esm_binding() {
    let files = &[
        (
            "src/index.js",
            "import current, { set } from './state.js'; set(1); export const result = current;",
        ),
        (
            "src/state.js",
            "let current = 0; export { current as default }; export function set(value) { current = value; }",
        ),
    ];
    assert_runtime_differential(
        "mutable-default-esm-binding",
        files,
        "src/index.js",
        "__WAKE_EXPORT__{\"result\":1}",
    );

    let unmapped = build(files, "src/index.js", true, false);
    let mapped = build(files, "src/index.js", true, true);
    assert_eq!(
        normalized_javascript(&mapped.bundle),
        normalized_javascript(&unmapped.bundle),
        "mapping a live default import must not alter JavaScript bytes"
    );
    let map: serde_json::Value = serde_json::from_str(
        mapped
            .entry()
            .source_map
            .as_deref()
            .expect("live default import build must emit a source map"),
    )
    .expect("valid source-map JSON");
    assert!(
        map["sources"]
            .as_array()
            .is_some_and(|sources| sources.len() == 2),
        "both the importing and exporting modules must remain represented in the map: {map}"
    );
}

#[test]
fn direct_eval_observes_a_live_internal_esm_import() {
    let files = &[
        (
            "src/index.js",
            "import { value, set } from './state.js'; set(1); export const result = eval('value');",
        ),
        (
            "src/state.js",
            "export let value = 0; export function set(next) { value = next; }",
        ),
    ];
    assert_runtime_differential(
        "direct-eval-live-import",
        files,
        "src/index.js",
        "__WAKE_EXPORT__{\"result\":1}",
    );
}

#[test]
fn define_selected_dynamic_import_retains_and_executes_only_the_live_target() {
    let files = &[
        (
            "src/index.js",
            "module.exports = (FLAG ? import('./live.js') : import('./dead.js')).then(module => ({ result: module.value }));",
        ),
        ("src/live.js", "export const value = 'live';"),
        ("src/dead.js", "export const value = 'dead';"),
    ];
    for (flag, expected) in [(true, "live"), (false, "dead")] {
        assert_defined_runtime_differential(
            "define-selected-dynamic-import",
            files,
            "src/index.js",
            flag,
            &format!("__WAKE_EXPORT__{{\"result\":\"{expected}\"}}"),
        );
    }
}

#[test]
fn define_selected_require_retains_and_executes_only_the_live_target() {
    let files = &[
        (
            "src/index.js",
            "module.exports = { result: (FLAG ? require('./live.js') : require('./dead.js')).value };",
        ),
        ("src/live.js", "exports.value = 'live';"),
        ("src/dead.js", "exports.value = 'dead';"),
    ];
    for (flag, expected) in [(true, "live"), (false, "dead")] {
        assert_defined_runtime_differential(
            "define-selected-require",
            files,
            "src/index.js",
            flag,
            &format!("__WAKE_EXPORT__{{\"result\":\"{expected}\"}}"),
        );
    }
}

#[test]
fn static_import_edges_remain_conservative_when_a_binding_branch_folds() {
    let files = &[
        (
            "src/index.js",
            "import { value as live } from './live.js'; import { value as dead } from './dead.js'; export const result = FLAG ? live : dead; export const effects = globalThis.__static_effects;",
        ),
        (
            "src/live.js",
            "globalThis.__static_effects = (globalThis.__static_effects || '') + 'L'; export const value = 'live';",
        ),
        (
            "src/dead.js",
            "globalThis.__static_effects = (globalThis.__static_effects || '') + 'D'; export const value = 'dead';",
        ),
    ];
    for flag in [true, false] {
        for minify in [false, true] {
            let output = build_with_boolean_define(files, "src/index.js", minify, flag);
            assert_eq!(
                output.module_count, 3,
                "without a module-side-effect proof, both static import edges stay live"
            );
            if node_available() {
                let observed = execute_and_observe_exports(&output.bundle);
                assert!(
                    observed.status.success(),
                    "{}",
                    String::from_utf8_lossy(&observed.stderr)
                );
                let expected = if flag { "live" } else { "dead" };
                assert_eq!(
                    String::from_utf8(observed.stdout).unwrap(),
                    format!("__WAKE_EXPORT__{{\"result\":\"{expected}\",\"effects\":\"LD\"}}")
                );
            }
        }
    }
}

#[test]
fn shaken_export_from_keeps_the_target_modules_top_level_effects() {
    let files = &[
        (
            "src/index.js",
            "import { result } from './barrel.js'; export const observed = [result, globalThis.__reexport_effect];",
        ),
        (
            "src/barrel.js",
            "export { unused } from './effect.js'; export const result = 42;",
        ),
        (
            "src/effect.js",
            "globalThis.__reexport_effect = 'ran'; export const unused = 1;",
        ),
    ];
    for minify in [false, true] {
        let fs = MemoryFileSystem::new();
        for (path, source) in files {
            fs.insert(path, source.as_bytes().to_vec());
        }
        let output = BuildSession::new_one_shot(
            Arc::new(fs),
            BuildOptions {
                minify,
                tree_shaking: true,
                dead_module_elimination: true,
                ..BuildOptions::default()
            },
        )
        .build_once(BuildRequest::new("src/index.js"));
        assert!(!output.has_errors(), "{:?}", output.diagnostics);
        assert_eq!(
            output.module_count, 3,
            "removing a re-export binding is not proof that its target module is side-effect free"
        );
        if minify {
            assert!(
                !output.bundle.contains("\"unused\""),
                "an unobserved source re-export must not emit a public getter:\n{}",
                output.bundle
            );
        }
        if node_available() {
            let observed = execute_and_observe_exports(&output.bundle);
            assert!(
                observed.status.success(),
                "{}",
                String::from_utf8_lossy(&observed.stderr)
            );
            assert_eq!(
                String::from_utf8(observed.stdout).unwrap(),
                "__WAKE_EXPORT__{\"observed\":[42,\"ran\"]}"
            );
        }
    }
}

#[test]
fn shaken_side_effect_only_export_star_drops_forwarding_but_executes_target() {
    let files = &[
        (
            "src/index.js",
            "import './barrel.js'; export const observed = { get effect() { return globalThis.__star_effect; } };",
        ),
        ("src/barrel.js", "export * from './effect.js';"),
        (
            "src/effect.js",
            "globalThis.__star_effect = 'ran'; export const unused = 1;",
        ),
    ];
    let fs = MemoryFileSystem::new();
    for (path, source) in files {
        fs.insert(path, source.as_bytes().to_vec());
    }
    let output = BuildSession::new_one_shot(
        Arc::new(fs),
        BuildOptions {
            minify: true,
            tree_shaking: true,
            dead_module_elimination: true,
            ..BuildOptions::default()
        },
    )
    .build_once(BuildRequest::new("src/index.js"));

    assert!(!output.has_errors(), "{:?}", output.diagnostics);
    assert_eq!(output.module_count, 3);
    assert!(
        !output.bundle.contains("Object.keys("),
        "an exact empty export proof should not emit runtime star forwarding:\n{}",
        output.bundle
    );
    if node_available() {
        let observed = execute_and_observe_exports(&output.bundle);
        assert!(
            observed.status.success(),
            "{}",
            String::from_utf8_lossy(&observed.stderr)
        );
        assert_eq!(
            String::from_utf8(observed.stdout).unwrap(),
            "__WAKE_EXPORT__{\"observed\":{\"effect\":\"ran\"}}",
            "{}",
            output.bundle
        );
    }
}

#[test]
fn named_import_through_export_star_keeps_barrel_forwarding() {
    let files = &[
        (
            "src/index.js",
            "import { value } from './barrel.js'; export const observed = [value, globalThis.__star_named_effect];",
        ),
        ("src/barrel.js", "export * from './effect.js';"),
        (
            "src/effect.js",
            "globalThis.__star_named_effect = 'ran'; export const value = 42; export const unused = 0;",
        ),
    ];
    let fs = MemoryFileSystem::new();
    for (path, source) in files {
        fs.insert(path, source.as_bytes().to_vec());
    }
    let output = BuildSession::new_one_shot(
        Arc::new(fs),
        BuildOptions {
            minify: true,
            tree_shaking: true,
            dead_module_elimination: true,
            ..BuildOptions::default()
        },
    )
    .build_once(BuildRequest::new("src/index.js"));

    assert!(!output.has_errors(), "{:?}", output.diagnostics);
    if node_available() {
        let observed = execute_and_observe_exports(&output.bundle);
        assert!(
            observed.status.success(),
            "{}",
            String::from_utf8_lossy(&observed.stderr)
        );
        assert_eq!(
            String::from_utf8(observed.stdout).unwrap(),
            "__WAKE_EXPORT__{\"observed\":[42,\"ran\"]}"
        );
    }
}

#[test]
fn minified_single_bundle_keeps_cjs_default_and_namespace_interop() {
    let files = &[
        (
            "src/index.js",
            "import legacy from './legacy.cjs'; import * as namespace from './legacy.cjs'; export const result = [legacy.value, namespace.default.value, namespace.named];",
        ),
        (
            "src/legacy.cjs",
            "module.exports = { value: 41, named: 42 };",
        ),
    ];
    assert_runtime_differential(
        "single-bundle-cjs-default-and-namespace",
        files,
        "src/index.js",
        "__WAKE_EXPORT__{\"result\":[41,41,42]}",
    );
}

#[test]
fn scope_concat_preserves_export_star_target_namespace_identity() {
    let files = &[
        (
            "src/index.js",
            r#"
import { named } from "./values.js";
export { named } from "./values.js";
export * from "./star.js";
export * as starNamespace from "./star.js";
export const observation = named;
"#,
        ),
        (
            "src/values.js",
            r#"
export const named = 1;
export const local = 2;
export { local as "hyphen-name" };
export const extra = 3;
export default 4;
"#,
        ),
        ("src/star.js", "export const starValue = 5;"),
    ];

    for minify in [false, true] {
        let output = build(files, "src/index.js", minify, false);
        assert_reparses(
            "export-star-namespace-identity",
            if minify { "optimized" } else { "readable" },
            &output.bundle,
        );
        if !node_available() {
            continue;
        }
        let observer = r#"
Promise.resolve(module.exports).then(value => {
  const publicKeys = Object.keys(value).filter(key => key !== "__esModule").sort();
  const namespaceKeys = Object.keys(value.starNamespace)
    .filter(key => key !== "__esModule")
    .sort();
  process.stdout.write(JSON.stringify([publicKeys, namespaceKeys, value.observation]));
});
"#;
        let observed = Command::new("node")
            .arg("-e")
            .arg(format!("{}\n{observer}", output.bundle))
            .output()
            .expect("node was available immediately before execution");
        assert!(
            observed.status.success(),
            "minify={minify}: {}\n{}",
            String::from_utf8_lossy(&observed.stderr),
            output.bundle
        );
        assert_eq!(
            String::from_utf8(observed.stdout).unwrap(),
            r#"[["named","observation","starNamespace","starValue"],["starValue"],1]"#,
            "minify={minify} leaked an unrelated module through export-star"
        );
    }
}

#[test]
fn explicit_reexport_overrides_same_source_export_star_in_either_order() {
    for (case, barrel) in [
        (
            "star-before-explicit",
            "export * from './values.js'; export { value } from './values.js';",
        ),
        (
            "explicit-before-star",
            "export { value } from './values.js'; export * from './values.js';",
        ),
    ] {
        let files = &[
            (
                "src/index.js",
                "import * as barrel from './barrel.js'; export const result = [barrel.value, barrel.sibling, Object.keys(barrel).sort().join(',')];",
            ),
            ("src/barrel.js", barrel),
            (
                "src/values.js",
                "export const value = 'explicit'; export const sibling = 'sibling';",
            ),
        ];
        assert_runtime_differential(
            case,
            files,
            "src/index.js",
            "__WAKE_EXPORT__{\"result\":[\"explicit\",\"sibling\",\"sibling,value\"]}",
        );
    }
}

#[test]
fn export_star_resolution_is_not_gated_by_tree_shaking() {
    let files = &[
        (
            "src/index.js",
            "import * as barrel from './barrel.js'; export const result = [barrel.value, barrel.sibling];",
        ),
        (
            "src/barrel.js",
            "export * from './values.js'; export { value } from './values.js';",
        ),
        (
            "src/values.js",
            "export const value = 'value'; export const sibling = 'sibling';",
        ),
    ];
    for minify in [false, true] {
        let fs = MemoryFileSystem::new();
        for (path, source) in files {
            fs.insert(path, source.as_bytes().to_vec());
        }
        let output = BuildSession::new_one_shot(
            Arc::new(fs),
            BuildOptions {
                minify,
                tree_shaking: false,
                ..BuildOptions::default()
            },
        )
        .build_once(BuildRequest::new("src/index.js"));
        assert!(!output.has_errors(), "{:?}", output.diagnostics);
        assert_reparses(
            "export-star-without-tree-shaking",
            if minify { "optimized" } else { "readable" },
            &output.bundle,
        );
        if node_available() {
            let observed = execute_and_observe_exports(&output.bundle);
            assert!(
                observed.status.success(),
                "minify={minify}: {}",
                String::from_utf8_lossy(&observed.stderr)
            );
            assert_eq!(
                String::from_utf8(observed.stdout).unwrap(),
                "__WAKE_EXPORT__{\"result\":[\"value\",\"sibling\"]}"
            );
        }
    }
}

#[test]
fn explicit_reexport_overrides_a_different_export_star_source() {
    let files = &[
        (
            "src/index.js",
            "import './setup.js'; import * as barrel from './barrel.js'; export const result = [barrel.value, barrel.starOnly, globalThis.__export_order.join(',')];",
        ),
        ("src/setup.js", "globalThis.__export_order = [];"),
        (
            "src/barrel.js",
            "export * from './star.js'; export { value } from './explicit.js';",
        ),
        (
            "src/star.js",
            "globalThis.__export_order.push('star'); export const value = 'star'; export const starOnly = 'star-only';",
        ),
        (
            "src/explicit.js",
            "globalThis.__export_order.push('explicit'); export const value = 'explicit';",
        ),
    ];
    assert_runtime_differential(
        "explicit-overrides-different-star-source",
        files,
        "src/index.js",
        "__WAKE_EXPORT__{\"result\":[\"explicit\",\"star-only\",\"star,explicit\"]}",
    );
}

#[test]
fn explicit_reexport_resolves_a_name_ambiguous_between_two_export_stars() {
    let files = &[
        (
            "src/index.js",
            "import * as barrel from './barrel.js'; export const result = [barrel.value, barrel.onlyA, barrel.onlyB, Object.keys(barrel).sort().join(',')];",
        ),
        (
            "src/barrel.js",
            "export * from './a.js'; export * from './b.js'; export { value } from './explicit.js';",
        ),
        (
            "src/a.js",
            "export const value = 'a'; export const onlyA = 'only-a';",
        ),
        (
            "src/b.js",
            "export const value = 'b'; export const onlyB = 'only-b';",
        ),
        ("src/explicit.js", "export const value = 'explicit';"),
    ];
    assert_runtime_differential(
        "explicit-resolves-two-ambiguous-export-stars",
        files,
        "src/index.js",
        "__WAKE_EXPORT__{\"result\":[\"explicit\",\"only-a\",\"only-b\",\"onlyA,onlyB,value\"]}",
    );
}

#[test]
fn explicit_reexport_overrides_opaque_commonjs_export_star() {
    let files = &[
        (
            "src/index.js",
            "import * as barrel from './barrel.js'; export const result = [barrel.value, barrel.cjsOnly];",
        ),
        (
            "src/barrel.js",
            "export * from './legacy.cjs'; export { value } from './explicit.js';",
        ),
        (
            "src/legacy.cjs",
            "exports.value = 'legacy'; exports.cjsOnly = 'cjs-only';",
        ),
        ("src/explicit.js", "export const value = 'explicit';"),
    ];
    assert_runtime_differential(
        "explicit-overrides-opaque-commonjs-star",
        files,
        "src/index.js",
        "__WAKE_EXPORT__{\"result\":[\"explicit\",\"cjs-only\"]}",
    );
}

#[test]
fn conflicting_export_stars_omit_the_ambiguous_namespace_name() {
    let files = &[
        (
            "src/index.js",
            "import * as barrel from './barrel.js'; export const result = ['value' in barrel, barrel.onlyA, barrel.onlyB, Object.keys(barrel).sort().join(',')];",
        ),
        (
            "src/barrel.js",
            "export * from './a.js'; export * from './b.js';",
        ),
        (
            "src/a.js",
            "export const value = 'a'; export const onlyA = 'only-a';",
        ),
        (
            "src/b.js",
            "export const value = 'b'; export const onlyB = 'only-b';",
        ),
    ];
    assert_runtime_differential(
        "ambiguous-export-star-name",
        files,
        "src/index.js",
        "__WAKE_EXPORT__{\"result\":[false,\"only-a\",\"only-b\",\"onlyA,onlyB\"]}",
    );
}

#[test]
fn duplicate_export_star_paths_to_one_binding_emit_one_namespace_name() {
    let files = &[
        (
            "src/index.js",
            "import * as barrel from './barrel.js'; export const result = [barrel.value, Object.keys(barrel).sort().join(',')];",
        ),
        (
            "src/barrel.js",
            "export * from './left.js'; export * from './right.js';",
        ),
        ("src/left.js", "export * from './value.js';"),
        ("src/right.js", "export * from './value.js';"),
        ("src/value.js", "export const value = 42;"),
    ];
    assert_runtime_differential(
        "same-binding-through-two-export-stars",
        files,
        "src/index.js",
        "__WAKE_EXPORT__{\"result\":[42,\"value\"]}",
    );
}

#[test]
fn export_star_cycles_publish_names_declared_after_the_cycle_edge() {
    let files = &[
        (
            "src/index.js",
            "import * as a from './a.js'; import * as b from './b.js'; export const result = [a.a, a.b, b.a, b.b, Object.keys(a).sort().join(','), Object.keys(b).sort().join(',')];",
        ),
        ("src/a.js", "export * from './b.js'; export const a = 'a';"),
        ("src/b.js", "export * from './a.js'; export const b = 'b';"),
    ];
    assert_runtime_differential(
        "export-star-cycle-late-names",
        files,
        "src/index.js",
        "__WAKE_EXPORT__{\"result\":[\"a\",\"b\",\"a\",\"b\",\"a,b\",\"a,b\"]}",
    );
}

#[test]
fn optimized_runtime_matches_readable_for_commonjs_cycles() {
    let cjs_cycle = &[
        (
            "src/index.js",
            "const a = require('./a.js'); module.exports = { result: a.read() };",
        ),
        (
            "src/a.js",
            "exports.name = 'A'; const b = require('./b.js'); exports.read = () => exports.name + b.name + ':' + b.seen;",
        ),
        (
            "src/b.js",
            "const a = require('./a.js'); exports.seen = a.name; exports.name = 'B';",
        ),
    ];
    assert_runtime_differential(
        "commonjs-cycle",
        cjs_cycle,
        "src/index.js",
        "__WAKE_EXPORT__{\"result\":\"AB:A\"}",
    );
}

#[test]
fn optimized_runtime_matches_readable_for_top_level_await() {
    let top_level_await = &[
        (
            "src/index.js",
            "import { value } from './value.js'; console.log('tla-ready'); export const result = value + 2;",
        ),
        (
            "src/value.js",
            "export const value = await Promise.resolve(40);",
        ),
    ];
    assert_runtime_differential(
        "top-level-await",
        top_level_await,
        "src/index.js",
        "tla-ready\n__WAKE_EXPORT__{\"result\":42}",
    );
}

#[test]
fn explicit_resource_management_runtime_is_differential_when_node_supports_it() {
    let files = &[(
        "src/index.js",
        r#"
const events = [];
function resource(name) { return { [Symbol.dispose]() { events.push(name); } }; }
function asyncResource(name) { return { async [Symbol.asyncDispose]() { events.push(name); } }; }
{
  using first = resource("first");
  await using second = asyncResource("second");
  events.push("body");
  void first;
  void second;
}
export const result = events.join(",");
"#,
    )];

    // Build and parser acceptance are unconditional; only the host-runtime differential is gated.
    let readable = build(files, "src/index.js", false, false);
    let optimized = build(files, "src/index.js", true, false);
    assert_reparses("using-await-using", "readable", &readable.bundle);
    assert_reparses("using-await-using", "optimized", &optimized.bundle);

    let probe = Command::new("node")
        .arg("-e")
        .arg(
            "async function probe(){using a={ [Symbol.dispose](){} };await using b={ async [Symbol.asyncDispose](){} };}",
        )
        .output();
    let Ok(probe) = probe else {
        eprintln!("node unavailable; using retained mandatory build/reparse coverage");
        return;
    };
    if !probe.status.success() {
        eprintln!("node lacks using/await using syntax; runtime portion skipped");
        return;
    }

    let readable_observation = execute_and_observe_exports(&readable.bundle);
    let optimized_observation = execute_and_observe_exports(&optimized.bundle);
    assert_eq!(optimized_observation.status, readable_observation.status);
    assert_eq!(optimized_observation.stdout, readable_observation.stdout);
    assert_eq!(optimized_observation.stderr, readable_observation.stderr);
    assert_eq!(
        String::from_utf8(readable_observation.stdout).unwrap(),
        "__WAKE_EXPORT__{\"result\":\"body,second,first\"}"
    );
}

#[test]
fn wake_owned_payload_corpus_stays_within_the_structured_runtime_ceiling() {
    // These byte counts were frozen from the removed legacy minifier for this Wake-owned corpus.
    // They measure only the emitted module body, never Wake's runtime or wrapper syntax. The final
    // column freezes the reviewed structured-runtime payload: the property-definition service adds
    // 22 bytes to the tiny constant fixture, while normal optimizer wins save 31/11/23 bytes in the
    // other fixtures. The aggregate payload remains 43 bytes below the 570-byte legacy baseline.
    let cases = [
        (
            "constant-fold-and-dce",
            "const veryLongFoldedValue = 1 + 2 * 3; const definitelyUnused = 99; export const result = veryLongFoldedValue;",
            90usize,
            112usize,
        ),
        (
            "control-flow",
            "function choose(veryLongCondition) { if (veryLongCondition) return 10; return 20; } export const result = choose(true);",
            140,
            109,
        ),
        (
            "captured-closure",
            "function outer(veryLongArgument) { const capturedValue = veryLongArgument + 1; return function inner(secondLongArgument) { return capturedValue + secondLongArgument; }; } export const result = outer(2)(3);",
            190,
            179,
        ),
        (
            "closed-object-shape",
            "const localRecord = { descriptiveProperty: 40, secondDescriptiveProperty: 2 }; export const result = localRecord.descriptiveProperty + localRecord.secondDescriptiveProperty;",
            150,
            127,
        ),
    ];

    let legacy_total: usize = cases.iter().map(|(_, _, bytes, _)| bytes).sum();
    let mut optimized_total = 0usize;
    for (name, source, legacy_bytes, structured_runtime_ceiling) in cases {
        let output = build(&[("src/index.js", source)], "src/index.js", true, false);
        let payload = normalized_single_module_payload(&output.bundle);
        assert_reparses(name, "optimized", &output.bundle);
        assert!(
            payload.len() <= structured_runtime_ceiling,
            "[{name}] payload grew beyond the explicit structured-runtime ceiling: new={} legacy={legacy_bytes} ceiling={structured_runtime_ceiling}\n{payload}",
            payload.len()
        );
        optimized_total += payload.len();
    }
    assert!(
        optimized_total < legacy_total,
        "owned corpus must improve in aggregate: new={optimized_total} legacy={legacy_total}"
    );
}

#[test]
fn mapped_and_unmapped_minification_have_identical_javascript_payloads() {
    let files = &[(
        "src/index.js",
        r#"
const foldedConstant = 1 + 2 * 3;
function compute(veryLongParameterName) {
  const singleUseLocal = veryLongParameterName + foldedConstant;
  return singleUseLocal;
}
export const result = compute(35);
"#,
    )];
    let unmapped = build(files, "src/index.js", true, false);
    let mapped = build(files, "src/index.js", true, true);

    assert!(unmapped.entry().source_map.is_none());
    let map = mapped
        .entry()
        .source_map
        .as_deref()
        .expect("minify + source map must emit a map");
    let parsed_map: serde_json::Value = serde_json::from_str(map).expect("valid source-map JSON");
    assert_eq!(parsed_map["version"], 3);
    assert!(
        parsed_map["sources"]
            .as_array()
            .is_some_and(|sources| !sources.is_empty()),
        "source map must identify the Wake-owned source"
    );
    assert!(
        parsed_map["mappings"]
            .as_str()
            .is_some_and(|mappings| !mappings.is_empty()),
        "source map must contain mappings"
    );

    assert_eq!(
        normalized_javascript(&mapped.bundle),
        normalized_javascript(&unmapped.bundle),
        "source-map collection must not alter JavaScript bytes"
    );
    assert_eq!(
        normalized_single_module_payload(&mapped.bundle),
        normalized_single_module_payload(&unmapped.bundle)
    );
    assert_reparses("mapped-minification", "mapped", &mapped.bundle);
    assert_reparses("mapped-minification", "unmapped", &unmapped.bundle);
}
