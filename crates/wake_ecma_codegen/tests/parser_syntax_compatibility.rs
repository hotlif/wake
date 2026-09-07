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

#[test]
fn destructuring_assignment_defaults_preserve_writes_and_lazy_evaluation() {
    assert_runtime(
        r#"
function run() {
  let calls = 0;
  const fallback = () => { calls++; return 7; };
  let descriptiveTarget = 0;
  ({ descriptiveTarget = fallback() } = {});
  const first = descriptiveTarget;
  ({ descriptiveTarget = fallback() } = { descriptiveTarget: 9 });
  const second = descriptiveTarget;
  ({ nested: { descriptiveTarget = fallback() } } = { nested: {} });
  for ({ descriptiveTarget = fallback() } of [{}, { descriptiveTarget: 11 }]) {}
  return [first, second, descriptiveTarget, calls];
}
globalThis.result = run();
"#,
        SourceType::Script,
        "[7,9,11,3]",
    );
}

#[test]
fn generic_optional_calls_preserve_receiver_and_short_circuit_arguments() {
    for source_type in [SourceType::TypeScript, SourceType::Tsx] {
        assert_runtime(
            r#"
let calls = 0;
const value = () => { calls++; return 7; };
const object = { base: 1, method: function<T>(n: T) { return [this.base, n]; } };
const absent = null as typeof object.method | null;
const absentObject = null as typeof object | null;
const first = object.method?.<number>(value());
const second = absent?.<number>(value());
const third = absentObject?.method<number>(value());
const fourth = (object?.method)?.<number>(value());
globalThis.result = [first, second, third, fourth, calls];
"#,
            source_type,
            "[[1,7],null,null,[1,7],2]",
        );
    }
}

#[test]
fn standalone_instantiations_and_bare_constructors_preserve_runtime() {
    for source_type in [SourceType::TypeScript, SourceType::Tsx] {
        assert_runtime(
            r#"
function identity<T>(value: T): T { return value; }
const numberIdentity = identity<number>;
const functions = [identity<string>, identity<number>];
class Box<T> { value = 7; valueOf() { return this.value; } }
const bare = new Box<Map<string, number>>;
const sum = new Box<number> + 2;
const greater = new Box<number> > 6;
const a=1, b=2, c=3;
const comparisons = [a < b > c, a < b >= c, a < b >> c, a < b >>> c, a < b > +c, a < b > -c];
globalThis.result = [numberIdentity(7), functions[0]('ok'), functions[1](8), bare.value, sum, greater, comparisons];
"#,
            source_type,
            r#"[7,"ok",8,7,9,true,[false,false,false,false,false,true]]"#,
        );
    }
}

#[test]
fn generic_constructor_tag_keeps_types_inside_its_member_expression() {
    for source_type in [SourceType::TypeScript, SourceType::Tsx] {
        assert_runtime(
            r#"
function tag<T>(strings: TemplateStringsArray, value: T) {
  return class { value = value; };
}
const first = new tag<number>`ok${7}`;
const second = new tag<string>`ok${'x'}`();
globalThis.result = [first.value, second.value];
"#,
            source_type,
            r#"[7,"x"]"#,
        );
    }
}
