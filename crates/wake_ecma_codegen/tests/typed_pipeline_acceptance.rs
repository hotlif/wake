//! End-to-end acceptance for the atomic public optimizer/codegen path.
//!
//! Parser-owned input crosses the public `optimize` boundary once; every emitted byte then comes
//! from the resulting `OptimizedProgram`, so these tests cannot assemble a raw typed pipeline or
//! pair a mutable IR with independent codegen decisions.

use std::process::Command;

use wake_common::Interner;
use wake_ecma_ast::SourceType;
use wake_ecma_codegen::{
    ModuleLinker, ModuleMappings, codegen_optimized, codegen_optimized_with_map,
};
use wake_ecma_minify::{OptimizeInput, OptimizeStats, optimize};
use wake_ecma_parser::parse;

#[derive(Clone, Copy)]
enum NodeExpectation {
    Return,
    ExplicitResourceManagement(&'static str),
}

#[derive(Clone, Copy)]
struct AcceptanceCase {
    name: &'static str,
    source_type: SourceType,
    source: &'static str,
    node_expectation: NodeExpectation,
}

fn acceptance_cases() -> Vec<AcceptanceCase> {
    vec![
        AcceptanceCase {
            name: "sloppy-block-function-binding-copies",
            source_type: SourceType::Script,
            source: r#"
function live(){const before=typeof fn;{const local=fn;function fn(){return 2}if(local!==fn)throw 1}return [before,fn()]}
function dead(){if(false){function never(){return 9}}return never}
function parameter(fn){{function fn(){return 3}}return fn}
function lexical(){let fn=4;{function fn(){return 5}}return fn}
function siblings(){const values=[];{function fn(){return 6}values.push(fn())}{function fn(){return 7}values.push(fn())}return [values,fn()]}
function immutableBlock(){let read;{function fn(){return 8}read=()=>fn;fn=9}return [read(),fn()]}
function strict(){'use strict';{function local(){}}return typeof local}
function asynchronous(){{async function local(){}}return typeof local}
function generator(){{function* local(){}}return typeof local}
function direct(flag){if(flag)function fn(){return 1}else function fn(){return 2}return fn()}
function duplicates(){{function fn(){return 1}function fn(){return 2}}return fn()}
function ancestor(){{function fn(){return 1}{function fn(){return 2}}}return fn()}
function caught(){try{throw 1}catch(fn){{function fn(){return 3}}}return fn()}
function caughtPattern(){try{throw {fn:1}}catch({fn}){{function fn(){}}}return typeof fn}
function argumentsCopy(p=1){const before=arguments;const read=()=>arguments;{function arguments(){}}return [typeof before,typeof read(),typeof arguments]}
globalThis.__wake_result=[live(),dead(),parameter(3),lexical(),siblings(),immutableBlock(),strict(),asynchronous(),generator(),direct(true),direct(false),duplicates(),ancestor(),caught(),caughtPattern(),argumentsCopy()];
"#,
            node_expectation: NodeExpectation::Return,
        },
        AcceptanceCase {
            name: "implicit-arguments-and-mapped-parameters",
            source_type: SourceType::Script,
            source: r#"
var arguments=99;
function implicit(){return [arguments[0],(()=>arguments[1])()]}
function same(){var arguments;return arguments[0]}
function copied(value=1){var arguments;return arguments[0]}
function mapped(value){arguments[0]=4;const first=value;value=5;return [first,arguments[0]]}
function assigned(value){value=2;arguments[0]=3;return value}
function aliased(value){const args=arguments;value=2;args[0]=3;return value}
function captured(value){value=2;(()=>arguments[0]=3)();return value}
function length(flag){if(flag)return arguments.length;return 0}
function unmapped(value){'use strict';arguments[0]=4;return value}
function separate(value=1){arguments[0]=4;return value}
function parameter(arguments){return arguments}
globalThis.__wake_result=[implicit(2,3),same(6),copied(7),mapped(2),unmapped(2),separate(2),parameter(8),arguments,assigned(1),aliased(1),captured(1),length(true)];
"#,
            node_expectation: NodeExpectation::Return,
        },
        AcceptanceCase {
            name: "named-function-expression-environments",
            source_type: SourceType::Script,
            source: r#"
const bodyVar=function self(){var self;return typeof self};
const parameter=function self(self){return self};
const lexical=function self(){let self=2;return self};
const defaults=function self(read=()=>self){var self=3;return [typeof read(),self]};
const immutable=function self(){self=3;return typeof self};
const strictImmutable=function self(){'use strict';try{self=3;return 'miss'}catch(error){return error.name}};
globalThis.__wake_result=[bodyVar(),parameter(4),lexical(),defaults(),immutable(),strictImmutable(),typeof self];
"#,
            node_expectation: NodeExpectation::Return,
        },
        AcceptanceCase {
            name: "parameter-and-body-var-bindings",
            source_type: SourceType::Script,
            source: r#"
function same(value){var value;return value}
function destructured({value}){var value;return value}
function separate(value,read=()=>value){var value=3;return [read(),value]}
function replaced(value){function value(){return 7}return value()}
function copied(value=5){var value;return value}
const key='item';
function computed({[key]:value}){var value;return value}
function rest(...values){var values;return values}
const copiedArrow=(value=6)=>{var value;return value};
globalThis.__wake_result=[same(2),destructured({value:4}),separate(1),replaced(0),copied(),computed({item:8}),rest(1,2),copiedArrow()];
"#,
            node_expectation: NodeExpectation::Return,
        },
        AcceptanceCase {
            name: "strict-block-function-bindings",
            source_type: SourceType::Script,
            source: r#"
"use strict";
const values=[];
{values.push(local());function local(){return "left"}}
{function local(){return "right"}values.push(local())}
function enclosing(){"use strict";{function hidden(){return 3}values.push(hidden())}return typeof hidden}
globalThis.__wake_result=[values,typeof local,enclosing()];
"#,
            node_expectation: NodeExpectation::Return,
        },
        AcceptanceCase {
            name: "class-static-block-var-environments",
            source_type: SourceType::Script,
            source: r#"
const values=[];
class C {
    static { values.push(typeof value, helper()); { var value=3; } function helper(){return "first"} values.push(value); }
    static { values.push(typeof value); var value=4; values.push(value,typeof helper); }
}
globalThis.__wake_result=[values,typeof value,typeof helper];
"#,
            node_expectation: NodeExpectation::Return,
        },
        AcceptanceCase {
            name: "control-flow-labels-switch-loops-try-finally",
            source_type: SourceType::Script,
            source: r#"
const events=[];
function record(value){events.push(value);console.log(value);return value}
let total=0;
outer:for(let index=0;index<4;index++){
  switch(index){
    case 0:record("switch-0");continue;
    case 1:total+=index;break;
    case 2:break outer;
    default:total+=100;
  }
}
try{record("try");throw new Error("boom")}
catch(error){record(error.name)}
finally{record("finally")}
let loop=0;
while(loop<2){loop++}
do{loop--}while(loop>1);
globalThis.__wake_result={total,loop,events};
"#,
            node_expectation: NodeExpectation::Return,
        },
        AcceptanceCase {
            name: "closure-tdz-eval-with",
            source_type: SourceType::Script,
            source: r#"
const events=[];
function record(value){events.push(value);console.log(value);return value}
function closure(seed){const captured=seed+1;return function(delta){return captured+delta}}
let temporalDeadZone="none";
try{{record(typeof later);let later=1}}catch(error){temporalDeadZone=error.name;record(error.name)}
function dynamic(){let visible=3;eval("visible += 2");return visible}
function unrelated(){let stable=4;return stable+stable}
function withScope(object){let visible=1;with(object){visible+=2}return [visible,object.visible]}
const object={visible:5};
globalThis.__wake_result={closure:closure(2)(4),temporalDeadZone,dynamic:dynamic(),unrelated:unrelated(),withScope:withScope(object),events};
"#,
            node_expectation: NodeExpectation::Return,
        },
        AcceptanceCase {
            name: "bigint-nan-negative-zero-optional-chain",
            source_type: SourceType::Script,
            source: r#"
const object={nested:{value:3},nil:null};
let mixedBigInt="none";
try{1n+1}catch(error){mixedBigInt=error.name;console.log(error.name)}
const negativeZero=-0;
const notANumber=0/0;
globalThis.__wake_result={bigint:(4n+5n).toString(),mixedBigInt,negativeZero:Object.is(negativeZero,-0),notANumber:Number.isNaN(notANumber),optional:[object?.nested?.value,object.nil?.value]};
"#,
            node_expectation: NodeExpectation::Return,
        },
        AcceptanceCase {
            name: "classes-and-private-names",
            source_type: SourceType::Script,
            source: r#"
class Counter{
  #descriptiveValue=1;
  static #descriptiveSeed=4;
  increment(){this.#descriptiveValue++;console.log(this.#descriptiveValue);return this.#descriptiveValue}
  static seed(){return this.#descriptiveSeed}
}
const counter=new Counter();
globalThis.__wake_result=[counter.increment(),counter.increment(),Counter.seed()];
"#,
            node_expectation: NodeExpectation::Return,
        },
        AcceptanceCase {
            name: "async-await-generator-yield",
            source_type: SourceType::Script,
            source: r#"
function* values(){yield 1;yield* [2,3]}
async function work(){const collected=[...values()];console.log(collected.join(","));const awaited=await Promise.resolve(4);return {collected,awaited}}
globalThis.__wake_result=work();
"#,
            node_expectation: NodeExpectation::Return,
        },
        AcceptanceCase {
            name: "explicit-resource-management",
            source_type: SourceType::Script,
            source: r#"
const events=[];
{
  using resource={value:7,[Symbol.dispose](){events.push("disposed");console.log("disposed")}};
  events.push(resource.value);
}
globalThis.__wake_result=events;
"#,
            // Older Node versions reject `using`, while newer versions execute it natively.
            // Parser/codegen round-trip and readable/optimized equivalence remain unconditional.
            node_expectation: NodeExpectation::ExplicitResourceManagement(
                r#"{"kind":"return","value":[7,"disposed"],"logs":[["disposed"]]}"#,
            ),
        },
        AcceptanceCase {
            name: "typescript-lowering",
            source_type: SourceType::TypeScript,
            source: r#"
interface Value { amount:number }
type Result=number|string;
const read=<T extends Value>(value:T):number=>value.amount+1;
const current=({amount:2} as Value) satisfies Value;
const answer:Result=read(current!);
console.log(answer);
globalThis.__wake_result=answer;
"#,
            node_expectation: NodeExpectation::Return,
        },
        AcceptanceCase {
            name: "jsx-lowering",
            source_type: SourceType::Jsx,
            source: r#"
const label="ready";
const view=<section id="card"><span>{label}</span><b>done</b></section>;
console.log(view.type);
globalThis.__wake_result=[view.type,view.props.id,view.props.children[0].props.children];
"#,
            node_expectation: NodeExpectation::Return,
        },
        AcceptanceCase {
            name: "tsx-lowering",
            source_type: SourceType::Tsx,
            source: r#"
type Props={title:string};
const View=({title}:Props)=><main><h1>{title}</h1></main>;
const view=View({title:"typed"});
console.log(view.type);
globalThis.__wake_result=[view.type,view.props.children.type,view.props.children.props.children];
"#,
            node_expectation: NodeExpectation::Return,
        },
    ]
}

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

struct TypedBuild {
    readable: String,
    optimized: String,
    mapped: String,
    mappings: ModuleMappings,
    stats: OptimizeStats,
    fingerprint: u64,
}

#[test]
fn utf16_import_attributes_reach_native_module_linker_unchanged() {
    use wake_ecma_codegen::{
        ModuleSpecifierRewriter, PreserveModuleFormat, codegen_preserved_optimized,
        codegen_preserved_optimized_with_map,
    };
    struct Unchanged;
    impl ModuleSpecifierRewriter for Unchanged {
        fn rewrite(&self, _specifier: &str) -> Option<String> {
            None
        }
    }
    fn observe(source: &str) -> String {
        let script = r#"
const vm = require('node:vm');
(async () => {
  const seen = [];
  const root = new vm.SourceTextModule(process.argv[1]);
  await root.link((specifier, referring, extra) => {
    const units = text => Array.from({length:text.length}, (_,i) => text.charCodeAt(i));
    seen.push([specifier, Object.keys(extra.attributes).sort().map(key => [units(key), units(extra.attributes[key])])]);
    return new vm.SyntheticModule(['value','default'], function() { this.setExport('value', 1); this.setExport('default', 1); });
  });
  await root.evaluate();
  console.log(JSON.stringify(seen));
})().catch(error => { console.error(error); process.exitCode = 1; });
"#;
        let output = Command::new("node")
            .args(["--experimental-vm-modules", "-e", script, source])
            .output()
            .expect("Node required");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }
    let source = r#"
import value from 'first' with {"\ud800":"\udfff",type:"\ufffd", "👍":"\0"};
export {value} from 'second' with {"\ud801":"\udffe"};
export * from 'third' with {"\ud802":"\udffd"};
"#;
    let expected = observe(source);
    assert!(expected.contains("55296"));
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let readable = parsed
        .module
        .with_ast(|program| wake_ecma_codegen::codegen(program, &interner));
    assert_eq!(observe(&readable), expected);
    for minify in [false, true] {
        let mut input = OptimizeInput::new(source);
        input.minify = minify;
        let optimized = optimize(parsed.module.clone(), &interner, &input).unwrap();
        let generated = codegen_preserved_optimized(
            &optimized,
            &interner,
            PreserveModuleFormat::EsModule,
            &Unchanged,
        );
        let (mapped, _) = codegen_preserved_optimized_with_map(
            &optimized,
            &interner,
            PreserveModuleFormat::EsModule,
            &Unchanged,
        );
        assert_eq!(generated, mapped);
        assert_eq!(
            observe(&generated),
            expected,
            "minify={minify}\n{generated}"
        );
    }
}

fn build_typed(source: &str, source_type: SourceType) -> TypedBuild {
    build_typed_with_options(
        source,
        source_type,
        wake_ecma_parser::ParseOptions::default(),
    )
}

fn build_typed_with_options(
    source: &str,
    source_type: SourceType,
    options: wake_ecma_parser::ParseOptions<'_>,
) -> TypedBuild {
    let interner = Interner::new();
    let parsed = wake_ecma_parser::parse_with(source, &interner, source_type, options);
    assert!(
        !parsed.has_errors(),
        "acceptance fixture failed to parse as {source_type:?}:\n{source}\n{:?}",
        parsed.diagnostics
    );
    let mut readable_input = OptimizeInput::new(source);
    readable_input.minify = false;
    readable_input.set_bundled_commonjs(true);
    let readable_program = optimize(parsed.module.clone(), &interner, &readable_input)
        .unwrap_or_else(|error| {
            panic!("readable optimization failed for {source_type:?}: {error}")
        });
    let readable = codegen_optimized(&readable_program, &interner, &NoLinker, true);

    let mut optimized_input = OptimizeInput::new(source);
    optimized_input.minify = true;
    optimized_input.set_bundled_commonjs(true);
    let optimized_program = optimize(parsed.module.clone(), &interner, &optimized_input)
        .unwrap_or_else(|error| panic!("optimized pipeline failed for {source_type:?}: {error}"));
    let optimized = codegen_optimized(&optimized_program, &interner, &NoLinker, true);
    let (mapped, mappings) =
        codegen_optimized_with_map(&optimized_program, &interner, &NoLinker, true);
    TypedBuild {
        readable,
        optimized,
        mapped,
        mappings,
        stats: optimized_program.stats().clone(),
        fingerprint: optimized_program.fingerprint(),
    }
}

fn assert_reparses(name: &str, generated: &str) {
    let interner = Interner::new();
    let parsed = parse(generated, &interner, SourceType::Script);
    assert!(
        !parsed.has_errors(),
        "{name} typed output did not reparse:\n{generated}\n{:?}",
        parsed.diagnostics
    );
}

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn node_supports_explicit_resource_management() -> bool {
    Command::new("node")
        .arg("-e")
        .arg(
            r#"const vm=require("vm");vm.runInNewContext("{using resource={[Symbol.dispose](){}};}");"#,
        )
        .output()
        .is_ok_and(|output| output.status.success())
}

fn hex_source(source: &str) -> String {
    use std::fmt::Write as _;

    source
        .as_bytes()
        .iter()
        .fold(String::with_capacity(source.len() * 2), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("write to String");
            hex
        })
}

fn execute_in_node(source: &str) -> String {
    // JSX/TSX parsing injects `react/jsx-runtime`; the bundled public path correctly lowers it to
    // an external require, so provide that dependency through the VM sandbox instead of editing
    // emitted JavaScript.
    let encoded = hex_source(source);
    let harness = format!(
        r#"
const vm=require("vm");
const source=Buffer.from("{encoded}","hex").toString("utf8");
const logs=[];
const jsx=(type,props)=>({{type,props}});
const sandbox={{
  console:{{log:(...values)=>logs.push(values)}},
  require:specifier=>{{
    if(specifier==="react/jsx-runtime"||specifier==="react/jsx-dev-runtime")return {{__esModule:true,jsx,jsxs:jsx,jsxDEV:jsx,Fragment:Symbol.for("wake.fragment")}};
    throw new Error(`unexpected external ${{specifier}}`);
  }}
}};
const __wake_require__=id=>{{throw new Error(`unexpected internal ${{id}}`)}};
__wake_require__.external=sandbox.require;
__wake_require__.promiseResolve=value=>Promise.resolve(value);
__wake_require__.objectAssign=Object.assign;
__wake_require__.objectKeys=Object.keys;
__wake_require__.objectDefineProperty=Object.defineProperty;
sandbox.__wake_require__=__wake_require__;
const normalize=value=>JSON.parse(JSON.stringify(value,(_key,item)=>{{
  if(typeof item==="bigint")return `${{item}}n`;
  if(typeof item==="number"&&Number.isNaN(item))return "NaN";
  if(typeof item==="number"&&Object.is(item,-0))return "-0";
  if(typeof item==="undefined")return "<undefined>";
  return item;
}}));
const done=(kind,value)=>process.stdout.write(JSON.stringify({{kind,value:normalize(value),logs:normalize(logs)}}));
try{{
  vm.runInNewContext(source,sandbox);
  Promise.resolve(sandbox.__wake_result).then(value=>done("return",value),error=>done("throw",error&&error.name||"Error"));
}}catch(error){{done("throw",error&&error.name||"Error")}}
"#
    );
    let output = Command::new("node")
        .arg("-e")
        .arg(harness)
        .output()
        .expect("run Node typed-pipeline acceptance");
    assert!(
        output.status.success(),
        "Node harness failed:\n{}\nsource:\n{source}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("Node acceptance output is UTF-8")
}

#[test]
fn typed_pipeline_template_raw_text_and_expression_separators_match_source() {
    let source = r#"
function inspect(strings,...values){return {raw:strings.raw,cooked:[...strings],values}}
function render(value,other){
  return [
    `x${value}`,
    `x${`y${value}`}z`,
    inspect`x\n${value}y${other}`,
    inspect`0${value}_${other}$${value}`,
    inspect`+${value}-${other}/${value} ${other}${value}`,
    inspect`中${value}`,
    `sum${value+ +other}difference${value- -other}type${typeof value}regex${/x/.test("x")}`
  ];
}
globalThis.__wake_result=render(7,2);
"#;
    let build = build_typed(source, SourceType::Script);
    assert_eq!(build.optimized, build.mapped);
    assert_reparses("template-raw-readable", &build.readable);
    assert_reparses("template-raw-optimized", &build.optimized);
    if node_available() {
        let expected = execute_in_node(source);
        assert!(expected.contains("\"kind\":\"return\""));
        for generated in [&build.readable, &build.optimized] {
            assert_eq!(
                execute_in_node(generated),
                expected,
                "template raw text or substitution token semantics changed:\n{generated}"
            );
        }
    }
}

#[test]
fn typed_pipeline_template_line_terminators_match_source_with_downlevel() {
    assert!(
        node_available(),
        "Node is required for template runtime regressions"
    );
    let raw = "a\r\nb\rc\n\\r\\n\\\r\n\\\u{2028}\\\u{2029}d\u{2028}e\u{2029}f";
    let source = format!(
        "function inspect(strings,...values){{return [strings.raw,[...strings],values]}}\n\
         const value=7;\n\
         globalThis.__wake_result=[`{raw}`,`{raw}${{value}}{raw}${{value}}{raw}`,\
         inspect`{raw}${{value}}{raw}`,\"a\\\u{2028}b\\\u{2029}c\"];"
    );
    let expected = execute_in_node(&source);
    assert!(expected.contains("\"kind\":\"return\""));
    for downlevel in [false, true] {
        let mut options = wake_ecma_parser::ParseOptions::default();
        if downlevel {
            options
                .transform_features
                .insert(wake_ecma_transform::EcmaFeature::TemplateLiteral);
        }
        let build = build_typed_with_options(&source, SourceType::Script, options);
        assert_eq!(build.optimized, build.mapped);
        for generated in [&build.readable, &build.optimized] {
            assert_reparses("template-line-terminators", generated);
            assert_eq!(
                execute_in_node(generated),
                expected,
                "downlevel={downlevel}\n{generated}"
            );
        }
    }
}

#[test]
fn typed_pipeline_tagged_invalid_escapes_preserve_raw_and_undefined_cooked() {
    assert!(
        node_available(),
        "Node is required for template runtime regressions"
    );
    let source = r#"
function inspect(strings,...values){return [strings.raw,[...strings],values,Object.isFrozen(strings),Object.isFrozen(strings.raw)]}
const value=7;
globalThis.__wake_result=[
  inspect`\1`,inspect`\8`,inspect`\00`,inspect`\09`,inspect`\x`,inspect`\x0`,
  inspect`\u`,inspect`\u000`,inspect`\u{`,inspect`\u{}`,inspect`\u{z}`,
  inspect`\u{123z}`,inspect`\u{110000}`,inspect`\u{ffffffffffffffffffffffffffffffff}`,
  inspect`\x${value}valid\n`,inspect`valid\n${value}\8${value}valid\n`,
  inspect`valid\n${value}\u`,inspect`\u${inspect`\9`}ok`,
  inspect`\\x|\\u{}|\`|\${literal}|\0|\x41|\u0042|\u{00000043}|\u{d800}|\q|\👍`,
  inspect`\\\u{z}`,inspect`\x\`${value}\u\${literal}`
];
"#;
    let expected = execute_in_node(source);
    assert!(expected.contains("\"kind\":\"return\""), "{expected}");
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Script);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let legacy = parsed
        .module
        .with_ast(|program| wake_ecma_codegen::codegen(program, &interner));
    assert_eq!(execute_in_node(&legacy), expected);
    for downlevel in [false, true] {
        let mut options = wake_ecma_parser::ParseOptions::default();
        if downlevel {
            options
                .transform_features
                .insert(wake_ecma_transform::EcmaFeature::TemplateLiteral);
        }
        let build = build_typed_with_options(source, SourceType::Script, options);
        assert_eq!(build.optimized, build.mapped);
        for generated in [&build.readable, &build.optimized] {
            assert_reparses("tagged-invalid-escapes", generated);
            assert_eq!(
                execute_in_node(generated),
                expected,
                "downlevel={downlevel}\n{generated}"
            );
        }
    }
}

#[test]
fn typed_pipeline_utf16_strings_match_source_with_downlevel() {
    assert!(
        node_available(),
        "Node is required for UTF-16 runtime regressions"
    );
    let source = r#"
function inspect(strings,...values){return [strings.raw,[...strings],values]}
const high="\ud800",low="\udfff";
const object={"\ud800":1,"\udfff"(){return 2},get "\u{d800}x"(){return 3}};
class Keys {"\ud800"(){return 4}}
const {"\ud800":picked,...rest}=object;
globalThis.__wake_result=[
  high,low,"\u{d800}","a\ud800b\udfffc",high.length,high.charCodeAt(0),
  "\ud83d"+"\udc4d", "\ud800"+"x", "\ud800"==="\ufffd",!!"\ud800",
  "\udc00\ud800", "\ud800\ud800\udc00", "\ud800\u0000\ufffd",
  object[high],object[low](),object["\ud800x"],picked,rest[low](),new Keys()[high](),
  `\ud800`, `x\ud800${low}z\udfff`,inspect`\ud800${high}\u{dfff}`,
  "\ud800"<"\ud801", "\u{10000}"<"\ue000"
];
"#;
    let expected = execute_in_node(source);
    assert!(expected.contains("\"kind\":\"return\""));
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Script);
    let legacy = parsed
        .module
        .with_ast(|program| wake_ecma_codegen::codegen(program, &interner));
    assert_eq!(execute_in_node(&legacy), expected);
    for downlevel in [false, true] {
        let mut options = wake_ecma_parser::ParseOptions::default();
        if downlevel {
            options
                .transform_features
                .insert(wake_ecma_transform::EcmaFeature::TemplateLiteral);
        }
        let build = build_typed_with_options(source, SourceType::Script, options);
        assert_eq!(build.optimized, build.mapped);
        for generated in [&build.readable, &build.optimized] {
            assert_reparses("utf16-strings", generated);
            assert_eq!(
                execute_in_node(generated),
                expected,
                "downlevel={downlevel}\n{generated}"
            );
        }
    }
}

#[test]
fn typed_and_readable_decorators_preserve_utf16_property_names() {
    let source = r#"
const names=[];
function record(value,context){names.push(context.name);return value}
class Example { @record "\ud800"(){return 1} @record "\udfff"(){return 2} }
const value=new Example();
globalThis.__wake_result=[names,value["\ud800"](),value["\udfff"]()];
"#;
    let expected = r#"{"kind":"return","value":[["\ud800","\udfff"],1,2],"logs":[]}"#;
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::TypeScript);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let readable = parsed
        .module
        .with_ast(|program| wake_ecma_codegen::codegen(program, &interner));
    assert_eq!(execute_in_node(&readable).trim(), expected);
    let build = build_typed(source, SourceType::TypeScript);
    for generated in [&build.readable, &build.optimized] {
        assert_eq!(execute_in_node(generated).trim(), expected, "{generated}");
    }
}

#[test]
fn typed_pipeline_utf16_enum_members_and_jsx_values_remain_distinct() {
    let source = r#"
enum Values { "\ud800"=1, "\udfff"="\ud800" }
const view=<div title={"\ud800"}>{"\udfff"}</div>;
globalThis.__wake_result=[Values["\ud800"],Values[1],Values["\udfff"],view.props.title,view.props.children];
"#;
    let expected = r#"{"kind":"return","value":[1,"\ud800","\ud800","\ud800","\udfff"],"logs":[]}"#;
    let build = build_typed(source, SourceType::Tsx);
    for generated in [&build.readable, &build.optimized] {
        assert_eq!(execute_in_node(generated).trim(), expected, "{generated}");
    }
}

#[test]
fn typed_pipeline_corpus_reparses_maps_inertly_and_matches_readable_runtime() {
    let run_node = node_available();
    let node_supports_explicit_resource_management =
        run_node && node_supports_explicit_resource_management();
    for case in acceptance_cases() {
        let build = build_typed(case.source, case.source_type);
        assert_eq!(
            build.optimized, build.mapped,
            "{} mapping changed the JS body",
            case.name
        );
        assert_reparses(case.name, &build.readable);
        assert_reparses(case.name, &build.optimized);
        assert!(
            !build.mappings.is_empty(),
            "{} mapped output had no mappings",
            case.name
        );
        if run_node {
            let readable = execute_in_node(&build.readable);
            let optimized = execute_in_node(&build.optimized);
            assert_eq!(
                optimized, readable,
                "{} changed return/exception/log behavior\nreadable JS:\n{}\noptimized JS:\n{}",
                case.name, build.readable, build.optimized
            );
            match case.node_expectation {
                NodeExpectation::Return => assert!(
                    optimized.contains("\"kind\":\"return\""),
                    "{} did not return in Node: {optimized}\nreadable JS:\n{}\noptimized JS:\n{}",
                    case.name,
                    build.readable,
                    build.optimized
                ),
                NodeExpectation::ExplicitResourceManagement(expected_return) => {
                    if node_supports_explicit_resource_management {
                        assert_eq!(
                            optimized, expected_return,
                            "{} returned an unexpected value",
                            case.name
                        );
                    } else {
                        assert!(
                            optimized.contains("\"kind\":\"throw\"")
                                && optimized.contains("SyntaxError"),
                            "{} should expose the engine's unsupported-syntax exception: {optimized}",
                            case.name
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn typed_pipeline_maps_fold_identity_argument_and_original_renamed_names() {
    let source = r#"
function folded(){return 1+2}
function identity(value){return value}
function retained(descriptiveParameter){return descriptiveParameter+descriptiveParameter}
const foldedResult=folded();
const identityResult=identity(73);
globalThis.__wake_result=[foldedResult,identityResult,retained(4)];
"#;
    let build = build_typed(source, SourceType::Script);
    assert_eq!(build.optimized, build.mapped);
    assert_reparses("mapping-origins", &build.optimized);

    let folded_origin = source.find("1+2").expect("folded source") as u32;
    assert!(
        build
            .mappings
            .mappings
            .iter()
            .any(|mapping| !mapping.is_unmapped && mapping.src_offset == folded_origin),
        "folded definition origin is absent: {:?}\n{}",
        build.mappings,
        build.mapped
    );
    let argument_origin = source.find("73").expect("identity argument") as u32;
    assert!(
        build
            .mappings
            .mappings
            .iter()
            .any(|mapping| !mapping.is_unmapped && mapping.src_offset == argument_origin),
        "identity argument call-site origin is absent: {:?}\n{}",
        build.mappings,
        build.mapped
    );
    assert!(
        !build.optimized.contains("descriptiveParameter"),
        "identifier was not mangled: {}",
        build.optimized
    );
    assert!(
        build
            .mappings
            .names
            .iter()
            .any(|name| name == "descriptiveParameter"),
        "renamed original name missing from map: {:?}",
        build.mappings
    );
}

#[test]
fn typed_pipeline_large_input_is_deterministic_and_does_not_disable_mangling() {
    let source = format!(
        "/*{}*/function calculate(descriptiveParameter){{const descriptiveLocal=descriptiveParameter+1;return descriptiveLocal+descriptiveLocal}}globalThis.__wake_result=calculate(20);",
        "owned-large-input".repeat(400)
    );
    assert!(source.len() > 4096);
    let first = build_typed(&source, SourceType::Script);
    let second = build_typed(&source, SourceType::Script);
    assert_eq!(first.optimized, first.mapped);
    assert_eq!(first.optimized, second.optimized);
    assert_eq!(first.mappings.mappings, second.mappings.mappings);
    assert_eq!(first.mappings.names, second.mappings.names);
    assert_eq!(first.stats, second.stats);
    assert_eq!(first.fingerprint, second.fingerprint);
    assert!(!first.optimized.contains("descriptiveParameter"));
    assert!(!first.optimized.contains("descriptiveLocal"));
    assert_reparses("large-deterministic", &first.optimized);
    if node_available() {
        assert_eq!(
            execute_in_node(&first.optimized),
            execute_in_node(&first.readable)
        );
    }
}

#[test]
fn typed_pipeline_owned_payload_never_grows_and_shrinks_in_aggregate() {
    // These exact payloads were captured from the compatibility optimizer before the atomic
    // production cutover. Calling the public optimizer here would now measure the typed path
    // against itself, so the old body and byte count are deliberately frozen together. The typed
    // value remains a non-regression ceiling so future improvements do not weaken the gate.
    let cases = [
        (
            "fold-and-inline",
            "const folded=1+2;globalThis.__wake_result=folded;",
            "3;globalThis.__wake_result=3;",
            29usize,
            27usize,
        ),
        (
            "identity-call",
            "function identity(value){return value}globalThis.__wake_result=identity(73);",
            "function a(b){return b;}globalThis.__wake_result=a(73);",
            55,
            28,
        ),
        (
            "dead-and-control",
            "const unused=9;function choose(flag){if(flag)return 10;return 20}globalThis.__wake_result=choose(true);",
            "function a(b){return b?10:20;}globalThis.__wake_result=a(!0);",
            61,
            28,
        ),
        (
            "local-names",
            "function calculate(descriptiveParameter){const descriptiveLocal=descriptiveParameter+1;return descriptiveLocal+descriptiveLocal}globalThis.__wake_result=calculate(20);",
            "function a(b){const c=b+1;return c+c;}globalThis.__wake_result=a(20);",
            69,
            69,
        ),
    ];

    let mut legacy_total = 0usize;
    let mut typed_total = 0usize;
    for (name, source, legacy, frozen_legacy, typed_ceiling) in cases {
        let typed = build_typed(source, SourceType::Script).optimized;
        assert_eq!(
            legacy.len(),
            frozen_legacy,
            "{name} legacy payload baseline changed; measured legacy={} typed={}\nlegacy: {legacy}\ntyped: {typed}",
            legacy.len(),
            typed.len()
        );
        assert!(
            typed.len() <= typed_ceiling,
            "{name} typed payload exceeded its frozen ceiling: ceiling={typed_ceiling} measured={}\n{typed}",
            typed.len()
        );
        assert!(
            typed.len() <= frozen_legacy,
            "{name} typed payload grew: legacy={} typed={}\nlegacy: {legacy}\ntyped: {typed}",
            frozen_legacy,
            typed.len()
        );
        legacy_total += frozen_legacy;
        typed_total += typed.len();
    }
    assert!(
        typed_total < legacy_total,
        "typed corpus must shrink in aggregate: legacy={legacy_total} typed={typed_total}"
    );
}
