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
struct AcceptanceCase {
    name: &'static str,
    source_type: SourceType,
    source: &'static str,
    node_returns: bool,
}

fn acceptance_cases() -> Vec<AcceptanceCase> {
    vec![
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
            node_returns: true,
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
            node_returns: true,
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
            node_returns: true,
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
            node_returns: true,
        },
        AcceptanceCase {
            name: "async-await-generator-yield",
            source_type: SourceType::Script,
            source: r#"
function* values(){yield 1;yield* [2,3]}
async function work(){const collected=[...values()];console.log(collected.join(","));const awaited=await Promise.resolve(4);return {collected,awaited}}
globalThis.__wake_result=work();
"#,
            node_returns: true,
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
            // Node 22 does not parse `using`; both typed outputs must still fail with the same
            // SyntaxError. Parser/codegen round-trip coverage remains unconditional.
            node_returns: false,
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
            node_returns: true,
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
            node_returns: true,
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
            node_returns: true,
        },
    ]
}

struct NoLinker;

impl ModuleLinker for NoLinker {
    fn module_id(&self, _specifier: &str) -> Option<u32> {
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

fn build_typed(source: &str, source_type: SourceType) -> TypedBuild {
    let interner = Interner::new();
    let parsed = parse(source, &interner, source_type);
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
fn typed_pipeline_corpus_reparses_maps_inertly_and_matches_readable_runtime() {
    let run_node = node_available();
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
            if case.node_returns {
                assert!(
                    optimized.contains("\"kind\":\"return\""),
                    "{} did not return in Node: {optimized}\nreadable JS:\n{}\noptimized JS:\n{}",
                    case.name,
                    build.readable,
                    build.optimized
                );
            } else {
                assert!(
                    optimized.contains("\"kind\":\"throw\"") && optimized.contains("SyntaxError"),
                    "{} should expose the engine's unsupported-syntax exception: {optimized}",
                    case.name
                );
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
