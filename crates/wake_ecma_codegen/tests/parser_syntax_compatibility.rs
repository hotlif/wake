//! Runtime regressions for parser-owned expression boundaries (COMPATIBILITY M6).
//! Both readable and minified production IR output must preserve the same evaluation order.

use std::process::Command;

use wake_common::Interner;
use wake_ecma_ast::SourceType;
use wake_ecma_codegen::{ModuleLinker, codegen_optimized, codegen_optimized_with_map};
use wake_ecma_minify::{OptimizeInput, optimize};

struct NoLinker;

impl ModuleLinker for NoLinker {
    fn module_id(
        &self,
        _specifier: &str,
        _kind: wake_ecma_codegen::ModuleRequestKind,
    ) -> Option<u32> {
        None
    }
}

fn assert_runtime(source: &str, source_type: SourceType, expected: &str) {
    let interner = Interner::new();
    let parsed = wake_ecma_parser::parse(source, &interner, source_type);
    assert!(!parsed.has_errors(), "{source}\n{:?}", parsed.diagnostics);
    for minify in [false, true] {
        let mut input = OptimizeInput::new(source);
        input.minify = minify;
        input.set_bundled_commonjs(true);
        let optimized = optimize(parsed.module.clone(), &interner, &input)
            .unwrap_or_else(|error| panic!("minify={minify}: {error}\n{source}"));
        let code = codegen_optimized(&optimized, &interner, &NoLinker, true);
        let (mapped, _) = codegen_optimized_with_map(&optimized, &interner, &NoLinker, true);
        assert_eq!(code, mapped);
        let output = Command::new("node")
            .arg("-e")
            .arg(format!(
                "{code}\nprocess.stdout.write(JSON.stringify(globalThis.result));"
            ))
            .output()
            .expect("Node is required for expression runtime regressions");
        assert!(
            output.status.success(),
            "minify={minify}: {}\n{code}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout),
            expected,
            "minify={minify}\n{code}"
        );
    }
}

#[test]
fn constructor_non_null_assertions_keep_the_argument_list() {
    for source_type in [SourceType::TypeScript, SourceType::Tsx] {
        assert_runtime(
            r#"
const events: string[] = [];
class C { constructor(public value: number) { events.push('construct'); } }
const ctor: typeof C | null = C;
const holder = { get ctor() { events.push('get'); return ctor; } };
const first = new ctor!(7);
const second = new holder.ctor!!(8);
const third = new (ctor!)(9);
globalThis.result = [first.value, second.value, third.value, events];
"#,
            source_type,
            r#"[7,8,9,["construct","get","construct","construct"]]"#,
        );
    }
}

#[test]
fn constructor_tagged_templates_keep_the_tag_inside_new() {
    assert_runtime(
        r#"
const events = [];
const holder = {
  tag(strings, value) {
    events.push(this === holder ? 'receiver' : 'lost');
    events.push(strings[0], value);
    return class { constructor(value = 1) { this.value = value; events.push('construct'); } };
  }
};
const first = new holder.tag`ok${7}`;
const second = new holder.tag`next${8}`(9);
const third = new (holder.tag`group${10}`)(11);
globalThis.result = [first.value, second.value, third.value, events];
"#,
        SourceType::Script,
        r#"[1,9,11,["receiver","ok",7,"construct","receiver","next",8,"construct","receiver","group",10,"construct"]]"#,
    );
}
