//! Cross-crate acceptance for the atomic public module pipeline.
//!
//! Every fixture takes the production sequence: parser -> `optimize` -> optimized codegen. Linker
//! and specifier facts enter only through the public codegen seams; no test can finalize or emit a
//! raw mutable typed program.

use std::fmt::Write as _;
use std::process::Command;

use wake_common::Interner;
use wake_ecma_ast::SourceType;
use wake_ecma_codegen::{
    ModuleLinker, ModuleMappings, ModuleRequestKind, ModuleSpecifierRewriter, PreserveModuleFormat,
    codegen_optimized, codegen_optimized_with_map, codegen_optimized_with_map_and_requests,
    codegen_preserved_optimized, codegen_preserved_optimized_with_map,
};
use wake_ecma_minify::{OptimizeInput, OptimizeStats, optimize};
use wake_ecma_parser::parse;

struct ModuleBuild {
    code: String,
    mapped: String,
    mappings: ModuleMappings,
    stats: OptimizeStats,
    fingerprint: u64,
}

#[derive(Clone, Copy)]
struct LinkedDependency {
    specifier: &'static str,
    module_id: u32,
    is_esm: bool,
    is_async: bool,
    dynamic_chunk: Option<u32>,
}

const fn linked(specifier: &'static str, module_id: u32, is_esm: bool) -> LinkedDependency {
    LinkedDependency {
        specifier,
        module_id,
        is_esm,
        is_async: false,
        dynamic_chunk: None,
    }
}

struct FixtureLinker<'a>(&'a [LinkedDependency]);

impl ModuleLinker for FixtureLinker<'_> {
    fn module_id(&self, specifier: &str, _kind: ModuleRequestKind) -> Option<u32> {
        self.0
            .iter()
            .find(|dependency| dependency.specifier == specifier)
            .map(|dependency| dependency.module_id)
    }

    fn dynamic_chunk(&self, specifier: &str) -> Option<u32> {
        self.0
            .iter()
            .find(|dependency| dependency.specifier == specifier)
            .and_then(|dependency| dependency.dynamic_chunk)
    }

    fn is_async_module(&self, module_id: u32) -> bool {
        self.0
            .iter()
            .any(|dependency| dependency.module_id == module_id && dependency.is_async)
    }
}

struct ProfileRewriter;

impl ModuleSpecifierRewriter for ProfileRewriter {
    fn rewrite(&self, specifier: &str) -> Option<String> {
        match specifier {
            "old-static" => Some("new-static".into()),
            "old-reexport" => Some("new-reexport".into()),
            "old-dynamic" => Some("new-dynamic".into()),
            _ => None,
        }
    }
}

fn build_bundled(
    source: &str,
    dependencies: &[LinkedDependency],
    no_esmodule: bool,
) -> ModuleBuild {
    build_bundled_with_minify(source, dependencies, no_esmodule, true).0
}

fn build_bundled_with_minify(
    source: &str,
    dependencies: &[LinkedDependency],
    no_esmodule: bool,
    minify: bool,
) -> (ModuleBuild, wake_ecma_codegen::GeneratedModuleRuntimeNames) {
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(
        !parsed.has_errors(),
        "module acceptance fixture did not parse:\n{source}\n{:?}",
        parsed.diagnostics
    );
    let mut input = OptimizeInput::new(source);
    input.minify = minify;
    input.set_bundled_commonjs(true);
    input.set_bundled_internal_esm_dependencies(
        dependencies
            .iter()
            .filter(|dependency| dependency.is_esm)
            .flat_map(|dependency| {
                [
                    ModuleRequestKind::StaticImport,
                    ModuleRequestKind::DynamicImport,
                    ModuleRequestKind::Require,
                ]
                .map(|kind| (dependency.specifier.to_owned(), kind))
            }),
    );
    input.reserved_names = vec![
        "exports".into(),
        "require".into(),
        "__wake_require__".into(),
    ];
    let optimized = optimize(parsed.module.clone(), &interner, &input)
        .unwrap_or_else(|error| panic!("public module optimization failed:\n{source}\n{error}"));
    let linker = FixtureLinker(dependencies);
    let code = codegen_optimized(&optimized, &interner, &linker, no_esmodule);
    let (mapped, mappings, _, runtime_names) =
        codegen_optimized_with_map_and_requests(&optimized, &interner, &linker, no_esmodule);
    assert_eq!(code, mapped, "source-map collection changed emitted bytes");
    (
        ModuleBuild {
            code,
            mapped,
            mappings,
            stats: optimized.stats().clone(),
            fingerprint: optimized.fingerprint(),
        },
        runtime_names,
    )
}

fn build_preserved(source: &str, rewriter: &dyn ModuleSpecifierRewriter) -> ModuleBuild {
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let mut input = OptimizeInput::new(source);
    input.minify = true;
    let optimized = optimize(parsed.module.clone(), &interner, &input)
        .unwrap_or_else(|error| panic!("public preserve optimization failed:\n{source}\n{error}"));
    let code = codegen_preserved_optimized(
        &optimized,
        &interner,
        PreserveModuleFormat::EsModule,
        rewriter,
    );
    let (mapped, mappings) = codegen_preserved_optimized_with_map(
        &optimized,
        &interner,
        PreserveModuleFormat::EsModule,
        rewriter,
    );
    assert_eq!(code, mapped, "source-map collection changed emitted bytes");
    ModuleBuild {
        code,
        mapped,
        mappings,
        stats: optimized.stats().clone(),
        fingerprint: optimized.fingerprint(),
    }
}

#[test]
fn bundled_codegen_reports_every_internal_request_with_its_typed_role() {
    let source = r#"
import "side-a";
import value from "used";
import "side-b";
import "async-side";
globalThis.__wake_result=value;
"#;
    let dependencies = [
        linked("side-a", 3, true),
        linked("used", 4, true),
        linked("side-b", 5, true),
        LinkedDependency {
            specifier: "async-side",
            module_id: 6,
            is_esm: true,
            is_async: true,
            dynamic_chunk: None,
        },
    ];
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let mut input = OptimizeInput::new(source);
    input.minify = true;
    input.set_bundled_commonjs(true);
    input.set_bundled_internal_esm_dependencies(dependencies.iter().flat_map(|dependency| {
        [
            ModuleRequestKind::StaticImport,
            ModuleRequestKind::DynamicImport,
            ModuleRequestKind::Require,
        ]
        .map(|kind| (dependency.specifier.to_owned(), kind))
    }));
    input.reserved_names = vec!["__wake_require__".into()];
    let optimized = optimize(parsed.module.clone(), &interner, &input).unwrap();
    let linker = FixtureLinker(&dependencies);
    let (plain, _) = codegen_optimized_with_map(&optimized, &interner, &linker, true);
    let (code, _, requests, runtime_names) =
        codegen_optimized_with_map_and_requests(&optimized, &interner, &linker, true);

    assert_eq!(code, plain, "request metadata changed emitted JavaScript");
    assert!(runtime_names.is_canonical());
    assert_eq!(
        requests
            .iter()
            .map(|request| request.target_module_id)
            .collect::<Vec<_>>(),
        [3, 4, 5, 6],
        "the same codegen walk must report every generated internal request: {code}"
    );
    assert_eq!(
        requests
            .iter()
            .map(|request| request.role)
            .collect::<Vec<_>>(),
        [
            wake_ecma_codegen::GeneratedModuleRequestRole::DiscardedStatic,
            wake_ecma_codegen::GeneratedModuleRequestRole::Value,
            wake_ecma_codegen::GeneratedModuleRequestRole::DiscardedStatic,
            wake_ecma_codegen::GeneratedModuleRequestRole::Value,
        ]
    );
    for request in requests {
        let generated = &code[request.start as usize..request.end as usize];
        assert_eq!(
            generated,
            request.target_module_id.to_string(),
            "range must cover exactly the generated target literal"
        );
    }
}

#[test]
fn bundled_codegen_emits_registered_request_ids_as_canonical_decimal() {
    let source = "import value from 'used';globalThis.__wake_result=[value,1000]";
    let dependencies = [linked("used", 1000, true)];
    let interner = Interner::new();
    let parsed = parse(source, &interner, SourceType::Module);
    assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
    let mut input = OptimizeInput::new(source);
    input.minify = true;
    input.set_bundled_commonjs(true);
    input.set_bundled_internal_esm_dependencies([(
        "used".to_owned(),
        ModuleRequestKind::StaticImport,
    )]);
    input.reserved_names = vec!["__wake_require__".into()];
    let optimized = optimize(parsed.module.clone(), &interner, &input).unwrap();
    let linker = FixtureLinker(&dependencies);

    let (code, _, requests, _) =
        codegen_optimized_with_map_and_requests(&optimized, &interner, &linker, true);

    assert_eq!(requests.len(), 1, "{code}");
    let request = &requests[0];
    assert_eq!(request.target_module_id, 1000);
    assert_eq!(
        &code[request.start as usize..request.end as usize],
        "1000",
        "registered target range must use canonical decimal bytes: {code}"
    );
    assert!(
        code.contains("1e3"),
        "ordinary numeric literals should retain shortest minified spelling: {code}"
    );
}

#[test]
fn bundled_codegen_reports_collision_free_runtime_names_without_renaming_eval_bindings() {
    let source = "const module=1;const exports=2;const __wake_require__=3;export default eval('module+exports+__wake_require__');";
    for minify in [false, true] {
        let interner = Interner::new();
        let parsed = parse(source, &interner, SourceType::Module);
        assert!(!parsed.has_errors(), "{:?}", parsed.diagnostics);
        let mut input = OptimizeInput::new(source);
        input.minify = minify;
        input.set_bundled_commonjs(true);
        input.reserved_names = vec!["module".into(), "exports".into(), "__wake_require__".into()];
        let optimized = optimize(parsed.module.clone(), &interner, &input).unwrap();
        let linker = FixtureLinker(&[]);
        let (code, _, _, runtime_names) =
            codegen_optimized_with_map_and_requests(&optimized, &interner, &linker, true);

        assert_eq!(runtime_names.module, "module$1");
        assert_eq!(runtime_names.exports, "exports$1");
        assert_eq!(runtime_names.require, "__wake_require__$1");
        let compact = code
            .chars()
            .filter(|char| !char.is_whitespace())
            .collect::<String>();
        assert!(compact.contains("module=1"), "{code}");
        assert!(compact.contains("exports=2"), "{code}");
        assert!(compact.contains("__wake_require__=3"), "{code}");
        assert!(
            code.contains("eval('module+exports+__wake_require__')")
                || code.contains("eval(\"module+exports+__wake_require__\")"),
            "direct-eval source must remain byte-semantically owned by the user: {code}"
        );
        assert!(code.contains(&runtime_names.exports), "{code}");
    }
}

fn assert_reparses(name: &str, source: &str, source_type: SourceType) {
    let interner = Interner::new();
    let parsed = parse(source, &interner, source_type);
    assert!(
        !parsed.has_errors(),
        "{name} did not reparse as {source_type:?}:\n{source}\n{:?}",
        parsed.diagnostics
    );
}

fn generated_position(source: &str, byte_offset: usize) -> (u32, u32) {
    let prefix = &source[..byte_offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let column = prefix
        .rsplit_once('\n')
        .map_or(prefix, |(_, tail)| tail)
        .chars()
        .map(char::len_utf16)
        .sum::<usize>() as u32;
    (line, column)
}

fn node_available() -> bool {
    Command::new("node")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn hex(source: &str) -> String {
    source
        .as_bytes()
        .iter()
        .fold(String::with_capacity(source.len() * 2), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("write to String");
            hex
        })
}

fn run_node(source: &str) -> String {
    let encoded = hex(source);
    let script = format!(
        r#"const source=Buffer.from("{encoded}","hex").toString("utf8");
try{{eval(source)}}catch(error){{console.error(error&&error.stack||error);process.exitCode=1}}"#
    );
    let output = Command::new("node")
        .arg("-e")
        .arg(script)
        .output()
        .expect("run Node module acceptance");
    assert!(
        output.status.success(),
        "Node module fixture failed:\n{}\nsource:\n{source}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("Node output is UTF-8")
}

const OBJECT_RUNTIME_CAPABILITIES: &str = r#"
__wake_require__.objectDefineProperty=globalThis.Object.defineProperty;
__wake_require__.objectKeys=globalThis.Object.keys;
__wake_require__.objectAssign=globalThis.Object.assign;
"#;

#[test]
fn bundled_cjs_full_surface_reparses_maps_and_runs() {
    let source = r#"
import "side";
import descriptiveDefault,{descriptiveNamed,changing} from "esm";
import * as descriptiveNamespace from "esm";
export {changing as observed};
export {remote as renamed} from "reexport";
export * from "all";
const loaded=require("cjs");
const lazy=import("lazy");
function double(descriptiveParameter){return descriptiveParameter+descriptiveParameter}
descriptiveDefault();
descriptiveNamed();
globalThis.__wake_result=lazy.then(module=>({
  observed:changing,
  namespace:descriptiveNamespace.label,
  loaded:loaded.value,
  lazy:module.default,
  own:double(globalThis.__wake_seed)
}));
"#;
    let dependencies = [
        linked("side", 1, false),
        linked("esm", 2, true),
        linked("reexport", 3, true),
        linked("all", 4, true),
        linked("cjs", 5, false),
        LinkedDependency {
            specifier: "lazy",
            module_id: 6,
            is_esm: true,
            is_async: false,
            dynamic_chunk: Some(7),
        },
    ];
    let build = build_bundled(source, &dependencies, false);

    assert_eq!(build.code, build.mapped);
    assert_reparses(
        "bundled CommonJS full surface",
        &build.code,
        SourceType::Script,
    );
    assert!(!build.mappings.is_empty());
    assert!(
        build
            .mappings
            .names
            .iter()
            .any(|name| name == "descriptiveParameter"),
        "the map lost the original mangled parameter: {:?}\n{}",
        build.mappings.names,
        build.code
    );
    assert!(
        !build.code.contains("descriptiveParameter"),
        "the final-name pass did not mangle the parameter: {}",
        build.code
    );
    assert_eq!(
        build.code.matches("__wake_require__(").count(),
        6,
        "{}",
        build.code
    );
    assert!(
        build.code.contains("__wake_require__.import(7,6)"),
        "{}",
        build.code
    );

    if node_available() {
        let executable = format!(
            r#"
const events=[];
globalThis.__wake_seed=3;
let changing=1;
const cache=new Map();
function instantiate(id){{
  if(cache.has(id))return cache.get(id);
  let value;
  if(id===1){{events.push("side");value={{}}}}
  else if(id===2){{value={{
    __esModule:true,
    get changing(){{return changing}},
    label:"namespace",
    default:function(){{"use strict";events.push(["default-this",this===undefined]);changing=2}},
    descriptiveNamed:function(){{"use strict";events.push(["named-this",this===undefined])}}
  }}}}
  else if(id===3)value={{__esModule:true,remote:9}};
  else if(id===4)value={{__esModule:true,additional:10,default:"hidden"}};
  else if(id===5)value={{value:11}};
  else if(id===6)value={{__esModule:true,default:12}};
  else throw new Error("unknown module "+id);
  cache.set(id,value);return value;
}}
function __wake_require__(id){{events.push("require:"+id);return instantiate(id)}}
__wake_require__.import=(chunk,id)=>{{events.push("import:"+chunk+":"+id);return Promise.resolve(instantiate(id))}};
{object_runtime}
const exports={{}};
{code}
Promise.resolve(globalThis.__wake_result).then(value=>process.stdout.write(JSON.stringify({{
  value,
  exported:[exports.observed,exports.renamed,exports.additional,exports.__esModule],
  hasDefault:Object.prototype.hasOwnProperty.call(exports,"default"),
  events
}})),error=>{{console.error(error&&error.stack||error);process.exitCode=1}});
"#,
            object_runtime = OBJECT_RUNTIME_CAPABILITIES,
            code = build.code
        );
        assert_eq!(
            run_node(&executable),
            r#"{"value":{"observed":2,"namespace":"namespace","loaded":11,"lazy":12,"own":6},"exported":[2,9,10,true],"hasDefault":false,"events":["require:1","side","require:2","require:2","require:3","require:4","require:5","import:7:6",["default-this",true],["named-this",true]]}"#
        );
    }
}

#[test]
fn parenthesized_arrow_boundary_is_mapped_but_target_safe_export_getter_is_not() {
    let source = "export const descriptiveArrow=()=>0;";
    let build = build_bundled(source, &[], false);
    assert_reparses("mapped arrow export", &build.code, SourceType::Script);

    let arrow_offset = build
        .code
        .find("()=>0")
        .unwrap_or_else(|| panic!("fixture emitted no parenthesized arrow:\n{}", build.code));
    let arrow_position = generated_position(&build.code, arrow_offset);
    let source_offset = source.find("()=>0").expect("source arrow") as u32;
    assert!(
        build.mappings.mappings.iter().any(|mapping| {
            !mapping.is_unmapped
                && (mapping.gen_line, mapping.gen_col) == arrow_position
                && mapping.src_offset == source_offset
        }),
        "source arrow boundary was not mapped exactly: {:?}\n{}",
        build.mappings,
        build.code
    );

    let getter_offset = build
        .code
        .find("get:function()")
        .map(|offset| offset + "get:".len())
        .unwrap_or_else(|| {
            panic!(
                "fixture emitted no synthetic export getter:\n{}",
                build.code
            )
        });
    let getter_position = generated_position(&build.code, getter_offset);
    assert!(
        build.mappings.mappings.iter().all(|mapping| {
            (mapping.gen_line, mapping.gen_col) != getter_position || mapping.is_unmapped
        }),
        "synthetic export getter acquired a source mapping: {:?}\n{}",
        build.mappings,
        build.code
    );
}

#[test]
fn internal_esm_and_plain_cjs_use_distinct_default_and_namespace_interop() {
    let source = r#"
import descriptiveDefault,* as descriptiveNamespace from "dep";
globalThis.__wake_result={
  defaultValue:descriptiveDefault,
  namespace:descriptiveNamespace,
  stableNamespace:descriptiveNamespace===descriptiveNamespace
    };
"#;
    let build = |is_esm: bool| build_bundled(source, &[linked("dep", 1, is_esm)], true);
    let esm = build(true);
    let cjs = build(false);
    assert_reparses("internal ESM interop", &esm.code, SourceType::Script);
    assert_reparses("internal CJS interop", &cjs.code, SourceType::Script);
    assert!(!esm.code.contains(".objectAssign"), "{}", esm.code);
    assert!(
        cjs.code.contains("__wake_require__.objectAssign"),
        "{}",
        cjs.code
    );
    assert!(!cjs.code.contains("Object.assign"), "{}", cjs.code);
    assert!(cjs.code.contains("__esModule"), "{}", cjs.code);

    if node_available() {
        let execute = |code: &str, dependency: &str| {
            run_node(&format!(
                r#"const exports={{}};const dependency={dependency};function __wake_require__(id){{if(id!==1)throw new Error("bad id");return dependency}};{object_runtime}{code};process.stdout.write(JSON.stringify(globalThis.__wake_result));"#,
                object_runtime = OBJECT_RUNTIME_CAPABILITIES,
            ))
        };
        assert_eq!(
            execute(
                &esm.code,
                r#"{__esModule:true,default:"esm-default",named:"esm-named"}"#
            ),
            r#"{"defaultValue":"esm-default","namespace":{"__esModule":true,"default":"esm-default","named":"esm-named"},"stableNamespace":true}"#
        );
        assert_eq!(
            execute(&cjs.code, r#"{named:"plain"}"#),
            r#"{"defaultValue":{"named":"plain"},"namespace":{"named":"plain","default":{"named":"plain"}},"stableNamespace":true}"#
        );
        assert_eq!(
            execute(
                &cjs.code,
                r#"{__esModule:true,default:"wrapped",named:"marked"}"#
            ),
            r#"{"defaultValue":"wrapped","namespace":{"__esModule":true,"default":"wrapped","named":"marked"},"stableNamespace":true}"#
        );
    }
}

#[test]
fn compiler_intrinsics_do_not_capture_local_require_promise_or_object() {
    let source = r#"
import externalDefault from "external";
import * as cjsNamespace from "cjs";
export * from "all";
export const answer=7;
const require={tag:"r"};
const Promise={tag:"p",resolve(){throw new Error("captured promise intrinsic")}};
const Object={
  tag:"o",
  assign(){throw new Error("captured assign intrinsic")},
  keys(){throw new Error("captured keys intrinsic")},
  defineProperty(){throw new Error("captured define intrinsic")}
};
const localTags=eval("require.tag+Promise.tag+Object.tag");
globalThis.__wake_result=import("lazy").then(module=>[
  externalDefault.value,
  cjsNamespace.default.value,
  module.default.value,
  localTags
]);
"#;
    let dependencies = [
        linked("cjs", 1, false),
        linked("all", 2, true),
        linked("lazy", 3, false),
    ];

    for minify in [false, true] {
        let (build, runtime_names) =
            build_bundled_with_minify(source, &dependencies, false, minify);
        let capabilities = &runtime_names.capabilities;
        assert!(capabilities.external_require, "{}", build.code);
        assert!(capabilities.promise_resolve, "{}", build.code);
        assert!(capabilities.object_assign, "{}", build.code);
        assert!(capabilities.object_keys, "{}", build.code);
        assert!(capabilities.object_define_property, "{}", build.code);
        assert!(!capabilities.meta_url, "{}", build.code);
        assert!(
            build.code.contains("__wake_require__.external("),
            "{}",
            build.code
        );
        assert!(
            build.code.contains("__wake_require__.promiseResolve("),
            "{}",
            build.code
        );
        assert!(!build.code.contains("Promise.resolve"), "{}", build.code);
        assert!(!build.code.contains("Object.assign"), "{}", build.code);
        assert_reparses(
            "compiler intrinsic collision fixture",
            &build.code,
            SourceType::Script,
        );

        if node_available() {
            let executable = format!(
                r#"
const exports={{}};
function __wake_require__(id){{
  if(id===1)return {{value:"cjs"}};
  if(id===2)return {{__esModule:true,additional:"all"}};
  if(id===3)return {{value:"lazy"}};
  throw new Error("bad id "+id);
}}
__wake_require__.external=specifier=>{{
  if(specifier!=="external")throw new Error("bad external "+specifier);
  return {{value:"external"}};
}};
__wake_require__.promiseResolve=value=>globalThis.Promise.resolve(value);
{object_runtime}
{code}
globalThis.Promise.resolve(globalThis.__wake_result).then(value=>{{
  process.stdout.write(JSON.stringify([value,exports.answer,exports.additional,exports.__esModule]));
}},error=>{{console.error(error&&error.stack||error);process.exitCode=1}});
"#,
                object_runtime = OBJECT_RUNTIME_CAPABILITIES,
                code = build.code,
            );
            assert_eq!(
                run_node(&executable),
                r#"[["external","cjs","lazy","rpo"],7,"all",true]"#,
                "minify={minify}\n{}",
                build.code
            );
        }
    }
}

#[test]
fn direct_eval_and_with_keep_only_their_visible_import_binding_materialized() {
    let source = r#"
import {visible,setVisible} from "dep";
function readEval(){return eval("visible")}
function readWith(object){with(object){return visible}}
function unrelatedScope(descriptiveParameter){return descriptiveParameter+descriptiveParameter}
setVisible(8);
globalThis.__wake_result=[readEval(),readWith({visible:9}),unrelatedScope(4)];
"#;
    let build = build_bundled(source, &[linked("dep", 1, true)], true);
    assert_reparses("dynamic-scope import", &build.code, SourceType::Script);
    assert!(build.code.contains("visible"), "{}", build.code);
    assert!(
        !build.code.contains("descriptiveParameter"),
        "an unrelated nested scope was frozen: {}",
        build.code
    );
    if node_available() {
        let executable = format!(
            r#"let current=7;const exports={{}};function __wake_require__(id){{if(id!==1)throw new Error("bad id");return {{__esModule:true,get visible(){{return current}},setVisible(value){{current=value}}}}}};{code};process.stdout.write(JSON.stringify(globalThis.__wake_result));"#,
            code = build.code
        );
        assert_eq!(run_node(&executable), "[8,9,8]");
    }
}

#[test]
fn with_materializes_its_visible_name_but_keeps_outside_import_reads_live() {
    let source = r#"
import {visible} from "dep";
let inside;
with({visible:9}){inside=visible}
globalThis.__wake_bump_dependency();
globalThis.__wake_result=[inside,visible];
"#;
    let build = build_bundled(source, &[linked("dep", 1, true)], true);
    assert_reparses("with-scoped live import", &build.code, SourceType::Script);
    assert!(build.code.contains("visible"), "{}", build.code);

    if node_available() {
        let executable = format!(
            r#"let current=7,requireCalls=0;const exports={{}};const dependency={{__esModule:true,get visible(){{return current}}}};globalThis.__wake_bump_dependency=()=>{{current=8}};function __wake_require__(id){{if(id!==1)throw new Error("bad id");requireCalls++;return dependency}};{code};process.stdout.write(JSON.stringify([globalThis.__wake_result,requireCalls]));"#,
            code = build.code
        );
        assert_eq!(run_node(&executable), "[[9,8],1]");
    }
}

#[test]
fn bundled_cycle_keeps_cross_module_live_bindings() {
    let source_a = r#"
import {readB} from "b";
export let valueA=1;
export function readA(){return valueA+readB()}
export function bumpA(){valueA++}
"#;
    let source_b = r#"
import {valueA} from "a";
export let valueB=2;
export function readB(){return valueA+valueB}
export function bumpB(){valueB++}
"#;
    let a = build_bundled(source_a, &[linked("b", 1, true)], false);
    let b = build_bundled(source_b, &[linked("a", 0, true)], false);
    assert_reparses("cycle module A", &a.code, SourceType::Script);
    assert_reparses("cycle module B", &b.code, SourceType::Script);

    if node_available() {
        let executable = format!(
            r#"
const factories=[
  (module,exports,__wake_require__)=>{{{a}}},
  (module,exports,__wake_require__)=>{{{b}}}
];
const cache=[];
function __wake_require__(id){{
  if(cache[id])return cache[id].exports;
  const module={{exports:{{}}}};cache[id]=module;
  factories[id](module,module.exports,__wake_require__);
  return module.exports;
}}
{object_runtime}
const first=__wake_require__(0),second=__wake_require__(1);
first.bumpA();second.bumpB();
process.stdout.write(JSON.stringify([first.readA(),first.valueA,second.valueB]));
"#,
            a = a.code,
            b = b.code,
            object_runtime = OBJECT_RUNTIME_CAPABILITIES,
        );
        assert_eq!(run_node(&executable), "[7,2,3]");
    }
}

#[test]
fn bundled_top_level_await_reports_async_wrapper_and_executes() {
    let source = r#"
import {value} from "dep";
export const answer=await Promise.resolve(value+1);
"#;
    let build = build_bundled(
        source,
        &[LinkedDependency {
            specifier: "dep",
            module_id: 1,
            is_esm: true,
            is_async: true,
            dynamic_chunk: None,
        }],
        false,
    );
    assert!(build.stats.iterations > 0);
    assert!(build.code.contains("await"), "{}", build.code);
    let wrapped = format!(
        "async function module(exports,__wake_require__){{{}}}",
        build.code
    );
    assert_reparses("async bundled wrapper", &wrapped, SourceType::Script);

    if node_available() {
        let executable = format!(
            r#"const exports={{}};async function __wake_require__(id){{if(id!==1)throw new Error("bad id");return {{__esModule:true,value:10}}}};{object_runtime}async function execute(){{{code}}};execute().then(()=>process.stdout.write(JSON.stringify([exports.answer,exports.__esModule])),error=>{{console.error(error&&error.stack||error);process.exitCode=1}});"#,
            object_runtime = OBJECT_RUNTIME_CAPABILITIES,
            code = build.code
        );
        assert_eq!(run_node(&executable), "[11,true]");
    }
}

#[test]
fn preserve_esm_rewrites_static_reexport_and_dynamic_profiles_without_lowering_syntax() {
    let source = r#"
import descriptiveDefault from "old-static";
export {named as renamed} from "old-reexport";
globalThis.__wake_result=import("old-dynamic").then(module=>[descriptiveDefault,module.default]);
"#;
    let build = build_preserved(source, &ProfileRewriter);
    assert_eq!(build.code, build.mapped);
    assert_reparses("preserved ESM", &build.code, SourceType::Module);
    assert!(build.code.contains("new-static"), "{}", build.code);
    assert!(build.code.contains("new-reexport"), "{}", build.code);
    assert!(build.code.contains("new-dynamic"), "{}", build.code);
    assert!(!build.code.contains("old-static"), "{}", build.code);
    assert!(!build.code.contains("old-reexport"), "{}", build.code);
    assert!(!build.code.contains("old-dynamic"), "{}", build.code);
    assert!(build.code.contains("import"), "{}", build.code);
    assert!(build.code.contains("export"), "{}", build.code);
    assert!(!build.mappings.is_empty());
    assert!(build.stats.iterations > 0);
    assert_ne!(build.fingerprint, 0);
}
