//! Private-brand syntax must retain its lexical name and observable exceptions through the
//! public parser -> optimizer -> mapped/unmapped codegen path (WAKE-COMPATIBILITY M5/M6).

use std::process::Command;

use wake_common::Interner;
use wake_ecma_ast::SourceType;
use wake_ecma_codegen::{ModuleLinker, codegen, codegen_optimized, codegen_optimized_with_map};
use wake_ecma_minify::{OptimizeInput, optimize};
use wake_ecma_parser::parse;

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

fn node_result(source: &str) -> String {
    let result = Command::new("node")
        .arg("-e")
        .arg(format!(
            "{source}\nconsole.log(JSON.stringify(globalThis.result));"
        ))
        .output()
        .expect("Node is required for private-brand runtime acceptance");
    assert!(
        result.status.success(),
        "{}\n{source}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).expect("UTF-8 runtime result")
}

#[test]
fn private_brand_checks_preserve_names_precedence_and_effects() {
    let source = r#"
let calls=0;
class Outer {
  #longField=7;
  #longMethod(){}
  static #longStatic=1;
  static has(value){return [#longField in value,#longMethod in value,#longStatic in value]}
  static precedence(value){return [#longField in value===true,!(#longField in value),#longField in value?7:0]}
  static once(value){return #longField in (++calls,value)}
  static discarded(value){#longField in value;return "missing exception"}
  static loops(value){let n=0;for(let has=(#longField in value);has;has=false){n++}for(let has=("has" in Outer);has;has=false){n++}return n}
  static nested(value){
    class Shadow {
      #longField=2;
      has(other){return [#longField in other,#longMethod in other]}
    }
    const inner=new Shadow();
    return [inner.has(inner),inner.has(value)];
  }
}
const outer=new Outer();
class Captured {
  #outerBrand;
  static create(){return new class Inner {
    #innerBrand;
    has(value){return [#innerBrand in value,#outerBrand in value]}
  }()}
}
const nested=Captured.create();
const errors=[];
for(const primitive of [null,undefined,0,"x",false,1n]){
  try{Outer.discarded(primitive)}catch(error){errors.push(error.name)}
}
globalThis.result=[Outer.has(outer),Outer.has(Outer),Outer.has({}),Outer.precedence(outer),Outer.once(outer),calls,Outer.loops(outer),Outer.nested(outer),nested.has(nested),nested.has(new Captured()),errors];
"#;
    assert_roundtrip(source, Some("longField"));
}

fn assert_roundtrip(source: &str, renamed_private: Option<&str>) {
    let expected = node_result(source);
    for minify in [false, true] {
        let interner = Interner::new();
        let parsed = parse(source, &interner, SourceType::Script);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        let ast_generated = parsed
            .module
            .with_ast(|program| codegen(program, &interner));
        assert_eq!(node_result(&ast_generated), expected, "plain AST emission");
        let mut input = OptimizeInput::new(source);
        input.minify = minify;
        input.set_bundled_commonjs(true);
        let optimized = optimize(parsed.module, &interner, &input).expect("private-brand lowering");
        let plain = codegen_optimized(&optimized, &interner, &NoLinker, true);
        let (mapped, mappings) = codegen_optimized_with_map(&optimized, &interner, &NoLinker, true);
        assert_eq!(plain, mapped);
        assert!(
            !parse(&plain, &interner, SourceType::Script).has_errors(),
            "{plain}"
        );
        assert_eq!(node_result(&plain), expected, "minify={minify}\n{plain}");
        if minify && let Some(name) = renamed_private {
            assert!(
                !plain.contains(&format!("#{name}")),
                "private names were not compressed: {plain}"
            );
            assert!(mappings.names.iter().any(|original| original == name));
        }
    }
}

#[test]
fn private_brand_in_heritage_uses_the_outer_class_environment() {
    assert_roundtrip(
        "class Outer{#longName;make(o){return class Inner extends (#longName in o?Object:Array){#longName;has(o){return #longName in o}}}}const out=new Outer();const Inner=out.make(out);globalThis.result=[new Inner() instanceof Array,new Inner().has(new Inner())];",
        Some("longName"),
    );
}

#[test]
fn private_brand_in_yield_and_arrow_body_keeps_no_in_grouping() {
    for source in [
        "class C{#x;static *check(o){for(let value=yield (#x in o);;){return value}}}const gen=C.check(new C());globalThis.result=[gen.next().value,gen.next(7).value];",
        "class C{#x;static check(o){for(let has=()=> (#x in o);;){return has()}}}globalThis.result=C.check(new C());",
    ] {
        assert_roundtrip(source, None);
    }
}

#[test]
fn private_names_are_not_general_expressions_or_no_in_initializers() {
    for expression in ["#x", "#x + 1", "!#x in obj", "1 + #x in obj", "(#x) in obj"] {
        let source = format!("class C{{#x;static test(obj){{return {expression}}}}}");
        assert!(
            parse(&source, &Interner::new(), SourceType::Script).has_errors(),
            "accepted {source}"
        );
    }
    let source = "class C{#x;static test(obj){for(let has=#x in obj;;){}}}";
    assert!(parse(source, &Interner::new(), SourceType::Script).has_errors());
}
