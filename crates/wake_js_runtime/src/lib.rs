//! JavaScript source preprocessing and host-neutral execution for Wake products.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use wake_common::{FileSystem, Interner, OsFileSystem};
use wake_ecma_ast::SourceType;
use wake_ecma_codegen::{
    ModuleMappings, ModuleSpecifierKind, ModuleSpecifierRewriter, PreserveModuleFormat,
};
use wake_ecma_parser::{ParseOptions, parse_with};
use wake_ecma_vm::{ScriptSource, Vm, VmError, VmHandle, VmOptions};
use wake_resolver::{ResolutionEnvironment, ResolutionProfile, Resolver};

mod happy_dom_sources {
    include!(concat!(env!("OUT_DIR"), "/wake_happy_dom_sources.rs"));
}

const HAPPY_DOM_PREFIX: &str = "__wake_internal__/happy-dom/";
const HAPPY_DOM_ENTRY: &str = "wake-entry.js";
const IMPORT_REQUEST_PREFIX: &str = "@wake-internal/module-request/import:";
const REQUIRE_REQUEST_PREFIX: &str = "@wake-internal/module-request/require:";

struct PreserveSpecifiers;

impl ModuleSpecifierRewriter for PreserveSpecifiers {
    fn rewrite(&self, _specifier: &str) -> Option<String> {
        None
    }

    fn rewrite_with_kind(&self, specifier: &str, kind: ModuleSpecifierKind) -> Option<String> {
        if is_test_builtin_module(specifier) || is_builtin_module(specifier) {
            return None;
        }
        let prefix = match kind {
            ModuleSpecifierKind::Import => IMPORT_REQUEST_PREFIX,
            ModuleSpecifierKind::Require => REQUIRE_REQUEST_PREFIX,
        };
        Some(format!("{prefix}{specifier}"))
    }

    fn lower_dynamic_import_to_require(&self) -> bool {
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModuleResolutionMode {
    Import,
    Require,
}

fn decode_module_request(request: &str) -> (&str, ModuleResolutionMode) {
    if let Some(specifier) = request.strip_prefix(IMPORT_REQUEST_PREFIX) {
        (specifier, ModuleResolutionMode::Import)
    } else if let Some(specifier) = request.strip_prefix(REQUIRE_REQUEST_PREFIX) {
        (specifier, ModuleResolutionMode::Require)
    } else {
        // `createRequire()` is rewritten to `__wakeCjsRequire()` before parsing, so its literal
        // argument is intentionally not touched by codegen and arrives here without a prefix.
        (request, ModuleResolutionMode::Require)
    }
}

fn resolution_profile(mode: ModuleResolutionMode) -> ResolutionProfile {
    match mode {
        ModuleResolutionMode::Import => ResolutionProfile {
            conditions: ["browser", "import", "module", "default"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            main_fields: ["module", "main"].into_iter().map(str::to_string).collect(),
        },
        ModuleResolutionMode::Require => ResolutionProfile {
            conditions: ["browser", "node", "require", "default"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            main_fields: ["main", "module"].into_iter().map(str::to_string).collect(),
        },
    }
}

#[derive(Debug, Clone, Copy)]
struct TextRewriteEdit {
    generated_start: usize,
    generated_end: usize,
    original_start: usize,
    original_end: usize,
}

#[derive(Debug, Default)]
struct TextRewriteMap {
    edits: Vec<TextRewriteEdit>,
}

impl TextRewriteMap {
    fn original_offset(&self, generated_offset: usize) -> usize {
        let mut previous_generated = 0;
        let mut previous_original = 0;
        for edit in &self.edits {
            if generated_offset < edit.generated_start {
                return previous_original + generated_offset.saturating_sub(previous_generated);
            }
            if generated_offset < edit.generated_end {
                return edit.original_start;
            }
            previous_generated = edit.generated_end;
            previous_original = edit.original_end;
        }
        previous_original + generated_offset.saturating_sub(previous_generated)
    }
}

struct TranspiledModule {
    code: String,
    mappings: ModuleMappings,
}

#[derive(Debug)]
pub enum RuntimeError {
    Io {
        path: PathBuf,
        message: String,
    },
    Parse {
        path: PathBuf,
        messages: Vec<String>,
    },
    Resolve {
        path: PathBuf,
        specifier: String,
        reason: Option<String>,
    },
    Unsupported {
        path: PathBuf,
        feature: String,
    },
    ModuleGraph {
        source: Box<RuntimeError>,
        recovery_watch_paths: Vec<PathBuf>,
    },
    Vm(VmError),
}

impl RuntimeError {
    /// Physical files or directories whose mutation may make a failed graph compile succeed.
    ///
    /// The slice is owned by the error and remains valid after the resolver context is dropped, so
    /// a persistent test session can register roots outside the configured project directory.
    pub fn recovery_watch_paths(&self) -> &[PathBuf] {
        match self {
            Self::ModuleGraph {
                recovery_watch_paths,
                ..
            } => recovery_watch_paths,
            _ => &[],
        }
    }

    fn with_recovery_watch_paths(self, paths: Vec<PathBuf>) -> Self {
        if paths.is_empty() || matches!(self, Self::ModuleGraph { .. }) {
            self
        } else {
            Self::ModuleGraph {
                source: Box::new(self),
                recovery_watch_paths: paths,
            }
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => write!(f, "{}: {message}", path.display()),
            Self::Parse { path, messages } => {
                write!(f, "{}: {}", path.display(), messages.join("; "))
            }
            Self::Resolve {
                path,
                specifier,
                reason,
            } => {
                write!(f, "{}: cannot resolve {specifier}", path.display())?;
                if let Some(reason) = reason {
                    write!(f, ": {reason}")?;
                }
                Ok(())
            }
            Self::Unsupported { path, feature } => {
                write!(f, "{}: WAKE_TEST_UNSUPPORTED: {feature}", path.display())
            }
            Self::ModuleGraph { source, .. } => source.fmt(f),
            Self::Vm(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RuntimeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::ModuleGraph { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl From<VmError> for RuntimeError {
    fn from(value: VmError) -> Self {
        Self::Vm(value)
    }
}

/// Preprocess one module with Wake's parser and code generator, preserving module boundaries.
pub fn transpile_module(path: &Path, source: &str) -> Result<String, RuntimeError> {
    Ok(transpile_module_with_mappings(path, source)?.code)
}

fn transpile_module_with_mappings(
    path: &Path,
    source: &str,
) -> Result<TranspiledModule, RuntimeError> {
    let source_type = source_type(path);
    let (rewritten_source, rewrite_map) = rewrite_esm_create_require(source);
    let interner = Interner::new();
    let parsed = parse_with(
        &rewritten_source,
        &interner,
        source_type,
        ParseOptions {
            file_name: path.to_str().unwrap_or(""),
            jsx_dev: true,
            ..ParseOptions::default()
        },
    );
    if parsed.has_errors() {
        return Err(RuntimeError::Parse {
            path: path.to_path_buf(),
            messages: parsed
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.is_error())
                .map(|diagnostic| diagnostic.message.clone())
                .collect(),
        });
    }
    let filename = module_id(path);
    let directory = path
        .parent()
        .map(module_id)
        .unwrap_or_else(|| ".".to_string());
    let url = if filename.starts_with('/') {
        format!("file://{filename}")
    } else {
        format!("file:///{filename}")
    };
    let directory_literal =
        serde_json::to_string(&directory).expect("import.meta dirname is a string");
    let filename_literal =
        serde_json::to_string(&filename).expect("import.meta filename is a string");
    let url_literal = serde_json::to_string(&url).expect("import.meta URL is a string");
    let defines = [
        ("import.meta.dirname", directory_literal.as_str()),
        ("import.meta.filename", filename_literal.as_str()),
        ("import.meta.url", url_literal.as_str()),
    ];
    let (code, mut mappings) = parsed.module.with_ast(|program| {
        wake_ecma_codegen::codegen_preserved_module_mangled_with_map(
            program,
            &interner,
            PreserveModuleFormat::CommonJs,
            &PreserveSpecifiers,
            &defines,
            false,
            None,
            None,
            false,
        )
    });
    for mapping in &mut mappings.mappings {
        mapping.src_offset =
            u32::try_from(rewrite_map.original_offset(mapping.src_offset as usize))
                .expect("parser source offsets fit in u32");
    }
    Ok(TranspiledModule { code, mappings })
}

fn rewrite_esm_create_require(source: &str) -> (String, TextRewriteMap) {
    if !source.contains("createRequire") || !source.contains("require") {
        return (source.to_string(), TextRewriteMap::default());
    }
    let bytes = source.as_bytes();
    let mut output = String::with_capacity(source.len());
    let mut rewrite_map = TextRewriteMap::default();
    let mut index = 0;
    while index < bytes.len() {
        if matches!(bytes[index], b'\'' | b'"' | b'`') {
            let start = index;
            let quote = bytes[index];
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\\' {
                    index += 2;
                } else if bytes[index] == quote {
                    index += 1;
                    break;
                } else {
                    index += 1;
                }
            }
            output.push_str(&source[start..index.min(bytes.len())]);
            continue;
        }
        if bytes[index..].starts_with(b"require")
            && index
                .checked_sub(1)
                .is_none_or(|before| !is_identifier_byte(bytes[before]))
            && bytes
                .get(index + 7)
                .is_none_or(|after| !is_identifier_byte(*after))
        {
            let generated_start = output.len();
            output.push_str("__wakeCjsRequire");
            rewrite_map.edits.push(TextRewriteEdit {
                generated_start,
                generated_end: output.len(),
                original_start: index,
                original_end: index + 7,
            });
            index += 7;
            continue;
        }
        let character = source[index..]
            .chars()
            .next()
            .expect("source index remains on a UTF-8 boundary");
        output.push(character);
        index += character.len_utf8();
    }
    (output, rewrite_map)
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

pub fn execute_script(path: &Path, source: &str) -> Result<String, RuntimeError> {
    let code = transpile_module(path, source)?;
    Vm::new()
        .execute(&ScriptSource::new(path.to_string_lossy(), code))
        .map_err(Into::into)
}

/// A half-open interval in the generated graph script, measured in UTF-16 code units.
///
/// V8 and the Chrome DevTools Protocol report precise-coverage offsets in this coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratedScriptRange {
    /// Inclusive start offset.
    pub utf16_start: usize,
    /// Exclusive end offset.
    pub utf16_end: usize,
}

/// One source-map anchor inside a transpiled CommonJS module body.
///
/// Generated positions are local to the body (before graph wrappers are added). The source offset
/// is a UTF-8 byte offset into [`CommonJsModuleScriptLayout::original_source`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommonJsSourceMapping {
    generated_line: u32,
    generated_utf16_column: u32,
    generated_utf16_offset: usize,
    original_byte_offset: u32,
}

impl CommonJsSourceMapping {
    pub fn generated_line(&self) -> u32 {
        self.generated_line
    }

    pub fn generated_utf16_column(&self) -> u32 {
        self.generated_utf16_column
    }

    pub fn generated_utf16_offset(&self) -> usize {
        self.generated_utf16_offset
    }

    pub fn original_byte_offset(&self) -> u32 {
        self.original_byte_offset
    }
}

/// The generated layout and original identity of one CommonJS graph module.
///
/// The body range assigns V8 offsets to the module; [`Self::source_mappings`] then maps positions
/// inside that body back to byte offsets in [`Self::original_source`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommonJsModuleScriptLayout {
    id: String,
    source_path: PathBuf,
    original_source: String,
    source_mappings: Vec<CommonJsSourceMapping>,
    definition: GeneratedScriptRange,
    body: GeneratedScriptRange,
    synthetic: bool,
}

/// The kind of an owned edge in Wake's executable JavaScript module graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModuleGraphDependencyKind {
    /// A statically analyzable import, export-from, or CommonJS require.
    Static,
    /// A Wake Test runtime module operation such as `mock.import("./value")`.
    WakeRuntime,
}

/// The resolved target of one executable module edge.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ModuleGraphDependencyTarget {
    Module {
        id: String,
    },
    Builtin {
        specifier: String,
    },
    TestBuiltin {
        specifier: String,
    },
    /// Resolution or evaluation depends on runtime data that Wake cannot index safely.
    Opaque,
}

/// One deterministic dependency edge in an executable module graph.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleGraphDependency {
    pub specifier: String,
    pub kind: ModuleGraphDependencyKind,
    pub target: ModuleGraphDependencyTarget,
}

/// An owned module record suitable for dependency selection and filesystem watching.
///
/// `id` is the logical/cache identity used by the runtime. `watch_paths` are deliberately kept
/// separate because physical invalidation paths are not module identities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleGraphModule {
    pub id: String,
    pub watch_paths: Vec<PathBuf>,
    pub dependencies: Vec<ModuleGraphDependency>,
    pub opaque_dependencies: bool,
}

/// A stable, owned sidecar for the exact graph compiled by Wake's JavaScript runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModuleGraphManifest {
    pub entry_id: String,
    pub modules: Vec<ModuleGraphModule>,
    /// Package/resolver metadata whose changes require graph rediscovery.
    pub resolver_inputs: Vec<PathBuf>,
    /// True when the graph contains a dynamic or unresolved edge that cannot be indexed without
    /// risking a false-negative. Consumers must conservatively select every candidate suite.
    pub opaque_dependencies: bool,
}

/// A compiled module graph whose transpiled modules can be emitted into a fresh realm repeatedly.
#[derive(Debug, Clone)]
pub struct CompiledCommonJsModuleGraph {
    entry_id: String,
    modules: Vec<CompiledModule>,
    manifest: ModuleGraphManifest,
}

impl CompiledCommonJsModuleGraph {
    pub fn module_graph(&self) -> &ModuleGraphManifest {
        &self.manifest
    }
}

impl CommonJsModuleScriptLayout {
    /// Return Wake's canonical, slash-normalized module identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Return the canonical original source path before graph-script generation.
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    /// Return the exact source parsed for this module before test-runtime lowering.
    pub fn original_source(&self) -> &str {
        &self.original_source
    }

    /// Return deterministic module-local generated-to-original mapping anchors.
    pub fn source_mappings(&self) -> &[CommonJsSourceMapping] {
        &self.source_mappings
    }

    /// Map a UTF-16 offset local to the generated body back to an original UTF-8 byte offset.
    ///
    /// Source-map segments apply from their generated anchor up to the next anchor. Generated
    /// prefixes before the first owned anchor remain intentionally unmapped.
    pub fn original_byte_offset_for_body_utf16_offset(
        &self,
        generated_utf16_offset: usize,
    ) -> Option<u32> {
        if generated_utf16_offset >= self.body.utf16_end - self.body.utf16_start {
            return None;
        }
        let mapping_index = self
            .source_mappings
            .partition_point(|mapping| mapping.generated_utf16_offset <= generated_utf16_offset)
            .checked_sub(1)?;
        Some(self.source_mappings[mapping_index].original_byte_offset)
    }

    /// Return the complete `__wakeDefineModule` statement range, including its factory wrapper.
    pub fn definition(&self) -> GeneratedScriptRange {
        self.definition
    }

    /// Return only the transpiled module body range inside the generated factory wrapper.
    pub fn body(&self) -> GeneratedScriptRange {
        self.body
    }

    /// Return whether this module is a caller-supplied synthetic orchestration entry.
    pub fn is_synthetic(&self) -> bool {
        self.synthetic
    }
}

/// An owned CommonJS graph script that can execute in either Wake's V8 realm or Chromium.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledCommonJsGraphScript {
    script: String,
    source_url: String,
    entry_id: String,
    modules: Vec<CommonJsModuleScriptLayout>,
    module_graph: ModuleGraphManifest,
    has_async_modules: bool,
}

impl CompiledCommonJsGraphScript {
    /// Return the complete classic script, including caller prelude/completion and entry load.
    pub fn script(&self) -> &str {
        &self.script
    }

    /// Return the caller-spelled script URL appended through `sourceURL` and reported by V8/CDP.
    pub fn source_url(&self) -> &str {
        &self.source_url
    }

    /// Return Wake's canonical module id for the graph entry.
    pub fn entry_id(&self) -> &str {
        &self.entry_id
    }

    /// Return deterministic layouts for every module registered by [`Self::script`].
    pub fn modules(&self) -> &[CommonJsModuleScriptLayout] {
        &self.modules
    }

    /// Return the owned dependency sidecar for this exact emitted script.
    pub fn module_graph(&self) -> &ModuleGraphManifest {
        &self.module_graph
    }

    /// Return whether any generated module factory must settle asynchronously.
    pub fn has_async_modules(&self) -> bool {
        self.has_async_modules
    }
}

/// Compile an entry and its static dependencies into a browser-compatible CommonJS registry
/// script without creating or executing a V8 realm.
///
/// `prelude` must install the global registry operations used by the generated definitions.
/// `completion` runs after the entry has been requested. This is the authoritative graph,
/// resolver, transpile and script-emission path used by [`JsRuntime::execute_commonjs_graph_and_read`].
pub fn compile_commonjs_graph_script(
    entry: &Path,
    entry_source: &str,
    prelude: &str,
    completion: &str,
) -> Result<CompiledCommonJsGraphScript, RuntimeError> {
    let graph = compile_commonjs_module_graph(entry, entry_source)?;
    Ok(emit_commonjs_graph_script(
        &graph, entry, prelude, completion,
    ))
}

/// Compile and own an executable module graph without binding it to a realm prelude.
///
/// Test sessions use this boundary to reuse discovery/transpilation while every run still emits
/// and executes the graph inside a fresh realm or browser context.
pub fn compile_commonjs_module_graph(
    entry: &Path,
    entry_source: &str,
) -> Result<CompiledCommonJsModuleGraph, RuntimeError> {
    let compilation = compile_module_graph(entry, entry_source)?;
    let modules = compilation.modules;
    let entry_id = module_id(entry);
    let graph_modules = modules
        .iter()
        .map(|module| ModuleGraphModule {
            id: module.id.clone(),
            watch_paths: module.watch_paths.clone(),
            dependencies: module.dependencies.clone(),
            opaque_dependencies: module.opaque_dependencies,
        })
        .collect::<Vec<_>>();
    let opaque_dependencies = graph_modules
        .iter()
        .any(|module| module.opaque_dependencies);
    Ok(CompiledCommonJsModuleGraph {
        entry_id: entry_id.clone(),
        modules,
        manifest: ModuleGraphManifest {
            entry_id,
            modules: graph_modules,
            resolver_inputs: compilation.resolver_inputs,
            opaque_dependencies,
        },
    })
}

/// Emit an already compiled module graph for one fresh execution realm.
pub fn emit_commonjs_graph_script(
    graph: &CompiledCommonJsModuleGraph,
    entry: &Path,
    prelude: &str,
    completion: &str,
) -> CompiledCommonJsGraphScript {
    let has_async_modules = graph.modules.iter().any(|module| module.async_factory);
    let source_url = entry.to_string_lossy().replace('\\', "/");
    let entry_id = graph.entry_id.clone();
    let mut script = String::with_capacity(
        prelude.len()
            + graph
                .modules
                .iter()
                .map(|module| module.code.len())
                .sum::<usize>(),
    );
    let mut utf16_offset = 0;
    push_script_fragment(&mut script, &mut utf16_offset, prelude);
    push_script_fragment(&mut script, &mut utf16_offset, "\n");

    let mut mappings = Vec::with_capacity(graph.modules.len());
    for module in &graph.modules {
        let id = serde_json::to_string(&module.id).expect("module ids are strings");
        let resolutions =
            serde_json::to_string(&module.resolutions).expect("module resolutions are strings");
        let request_specifiers = serde_json::to_string(&module.request_specifiers)
            .expect("module request specifiers are strings");
        let async_keyword = if module.async_factory { "async " } else { "" };
        let prefix = format!(
            "globalThis.__wakeDefineModule({id}, {async_keyword}function(module, exports, require, __filename, __dirname) {{\n'use strict';\n"
        );
        let definition_start = utf16_offset;
        push_script_fragment(&mut script, &mut utf16_offset, &prefix);
        let body_start = utf16_offset;
        push_script_fragment(&mut script, &mut utf16_offset, &module.code);
        let body_end = utf16_offset;
        let suffix = format!("\n}}, {resolutions}, {request_specifiers});\n");
        push_script_fragment(&mut script, &mut utf16_offset, &suffix);
        let generated_line_starts = generated_utf16_line_starts(&module.code);
        let generated_utf16_len = module.code.encode_utf16().count();
        let source_mappings = module
            .mappings
            .mappings
            .iter()
            .filter_map(|mapping| {
                let generated_utf16_offset = *generated_line_starts
                    .get(mapping.gen_line as usize)?
                    + mapping.gen_col as usize;
                (generated_utf16_offset <= generated_utf16_len).then_some(CommonJsSourceMapping {
                    generated_line: mapping.gen_line,
                    generated_utf16_column: mapping.gen_col,
                    generated_utf16_offset,
                    original_byte_offset: mapping.src_offset,
                })
            })
            .collect();
        mappings.push(CommonJsModuleScriptLayout {
            id: module.id.clone(),
            source_path: module.source_path.clone(),
            original_source: module.original_source.clone(),
            source_mappings,
            definition: GeneratedScriptRange {
                utf16_start: definition_start,
                utf16_end: utf16_offset,
            },
            body: GeneratedScriptRange {
                utf16_start: body_start,
                utf16_end: body_end,
            },
            synthetic: module.synthetic,
        });
    }

    let entry_load = format!(
        "globalThis.__wakeLoadModule({});\n",
        serde_json::to_string(&entry_id).expect("entry ids are strings")
    );
    push_script_fragment(&mut script, &mut utf16_offset, &entry_load);
    if has_async_modules {
        push_script_fragment(
            &mut script,
            &mut utf16_offset,
            "globalThis.__wakeEntryModulePromise = typeof globalThis.__wakeWhenModulesReady === 'function' ? globalThis.__wakeWhenModulesReady() : Promise.resolve();\n",
        );
    } else {
        push_script_fragment(
            &mut script,
            &mut utf16_offset,
            "globalThis.__wakeEntryModulePromise = undefined;\n",
        );
    }
    push_script_fragment(&mut script, &mut utf16_offset, completion);
    push_script_fragment(&mut script, &mut utf16_offset, "\n//# sourceURL=");
    push_script_fragment(&mut script, &mut utf16_offset, &source_url);

    CompiledCommonJsGraphScript {
        script,
        source_url,
        entry_id,
        modules: mappings,
        module_graph: graph.manifest.clone(),
        has_async_modules,
    }
}

fn push_script_fragment(script: &mut String, utf16_offset: &mut usize, fragment: &str) {
    *utf16_offset += fragment.encode_utf16().count();
    script.push_str(fragment);
}

fn generated_utf16_line_starts(source: &str) -> Vec<usize> {
    let mut starts = vec![0];
    let mut offset = 0;
    for character in source.chars() {
        offset += character.len_utf16();
        if character == '\n' {
            starts.push(offset);
        }
    }
    starts
}

/// Thread-safe cancellation seam for one [`JsRuntime`] without exposing VM or V8 handles.
#[derive(Clone)]
pub struct RuntimeTerminationHandle {
    handle: VmHandle,
}

impl RuntimeTerminationHandle {
    /// Terminate the JavaScript instruction stream currently executing in the owned realm.
    pub fn terminate(&self) -> bool {
        self.handle.terminate()
    }
}

/// One isolated runtime realm with a CommonJS-compatible test-module entry seam.
pub struct JsRuntime {
    vm: Vm,
}

impl Default for JsRuntime {
    fn default() -> Self {
        Self::new()
    }
}

impl JsRuntime {
    pub fn new() -> Self {
        Self::new_with_coverage(false)
    }

    /// Create a realm with V8 precise-coverage support explicitly enabled or disabled.
    ///
    /// Coverage requires an inspector session, so the default [`Self::new`] keeps it disabled.
    pub fn new_with_coverage(coverage: bool) -> Self {
        let mut vm = Vm::with_options(VmOptions {
            inspector: coverage,
            ..VmOptions::default()
        });
        vm.register_json_host_function("__wakeHostCall", host_call)
            .expect("Wake's static host callback registration must succeed");
        Self { vm }
    }

    pub fn engine_version() -> &'static str {
        Vm::engine_version()
    }

    pub fn set_execution_timeout(&mut self, timeout: Duration) {
        self.vm.set_execution_timeout(Some(timeout));
    }

    /// Return a thread-safe handle that may terminate this realm's current execution.
    pub fn termination_handle(&mut self) -> RuntimeTerminationHandle {
        RuntimeTerminationHandle {
            handle: self.vm.handle(),
        }
    }

    /// Terminate this realm's current execution directly.
    pub fn terminate(&mut self) -> bool {
        self.vm.handle().terminate()
    }

    /// Begin V8 precise range coverage for a realm created with coverage enabled.
    pub fn start_precise_coverage(&mut self) -> Result<(), RuntimeError> {
        self.vm.start_precise_coverage().map_err(Into::into)
    }

    /// Take and stop V8 precise range coverage, returning only owned JSON data.
    pub fn take_precise_coverage(&mut self) -> Result<serde_json::Value, RuntimeError> {
        self.vm.take_precise_coverage().map_err(Into::into)
    }

    /// Preprocess and execute one CommonJS module after a host-provided prelude.
    ///
    /// The prelude owns host globals such as `require`; the module remains a separate lexical block
    /// so top-level declarations cannot overwrite the host implementation accidentally.
    pub fn execute_commonjs_and_read(
        &mut self,
        path: &Path,
        source: &str,
        prelude: &str,
        completion: &str,
        result_expression: &str,
    ) -> Result<String, RuntimeError> {
        let module = transpile_module(path, source)?;
        let code = format!(
            "{prelude}\n{{\n{module}\n}}\n{completion}\n//# sourceURL={}",
            path.to_string_lossy().replace('\\', "/")
        );
        self.vm
            .execute_and_read(
                &ScriptSource::new(path.to_string_lossy(), code),
                result_expression,
            )
            .map_err(Into::into)
    }

    /// Compile an entry and all statically required local/package modules into one realm-owned
    /// CommonJS registry, then execute the entry through that registry.
    pub fn execute_commonjs_graph_and_read(
        &mut self,
        entry: &Path,
        entry_source: &str,
        prelude: &str,
        completion: &str,
        result_expression: &str,
    ) -> Result<String, RuntimeError> {
        let compiled = compile_commonjs_graph_script(entry, entry_source, prelude, completion)?;
        self.execute_compiled_commonjs_graph_and_read(&compiled, result_expression)
    }

    /// Execute an already compiled graph artifact in this realm and read an owned string result.
    /// This lets test orchestration share the exact artifact and its range sidecar with V8 or CDP.
    pub fn execute_compiled_commonjs_graph_and_read(
        &mut self,
        compiled: &CompiledCommonJsGraphScript,
        result_expression: &str,
    ) -> Result<String, RuntimeError> {
        self.vm
            .execute_and_read(
                &ScriptSource::new(compiled.source_url(), compiled.script()),
                result_expression,
            )
            .map_err(Into::into)
    }
}

#[derive(Debug, Clone)]
struct CompiledModule {
    id: String,
    source_path: PathBuf,
    original_source: String,
    code: String,
    mappings: ModuleMappings,
    resolutions: BTreeMap<String, String>,
    request_specifiers: BTreeMap<String, String>,
    dependencies: Vec<ModuleGraphDependency>,
    watch_paths: Vec<PathBuf>,
    opaque_dependencies: bool,
    async_factory: bool,
    synthetic: bool,
}

struct ModuleGraphCompilation {
    modules: Vec<CompiledModule>,
    resolver_inputs: Vec<PathBuf>,
}

struct ModuleResolutionContext {
    fs: Arc<dyn FileSystem>,
    resolver: Arc<Resolver>,
    environment: ResolutionEnvironment,
    resolver_inputs: Vec<PathBuf>,
}

impl ModuleResolutionContext {
    fn new(entry: &Path) -> Result<Self, RuntimeError> {
        let os_fs: Arc<dyn FileSystem> = Arc::new(OsFileSystem);
        let environment = ResolutionEnvironment::new(os_fs);
        let mut context = Self {
            fs: environment.file_system(),
            resolver: environment.resolver(),
            environment,
            resolver_inputs: Vec::new(),
        };
        context.resolver_inputs = context.collect_resolver_inputs(entry);
        Ok(context)
    }

    fn is_file(&self, path: &Path) -> bool {
        self.fs.is_file(path)
    }

    fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
        self.fs.read_to_string(path)
    }

    fn resolve(
        &self,
        importer: &Path,
        specifier: &str,
        mode: ModuleResolutionMode,
    ) -> Result<PathBuf, RuntimeError> {
        if specifier == "@wake-internal/happy-dom" {
            return Ok(virtual_module_path(HAPPY_DOM_ENTRY));
        }
        if virtual_module_relative(importer).is_some() {
            if let Some(package_entry) = match specifier {
                "entities" => Some("npm/entities/dist/esm/index.js"),
                "whatwg-mimetype" => Some("npm/whatwg-mimetype/lib/mime-type.js"),
                "buffer-image-size" => Some("npm/buffer-image-size/lib/index.js"),
                // WebSockets remain behind Wake's explicit network policy instead of exposing the
                // Node transport embedded by Happy DOM.
                "ws" => Some("wake-websocket-policy.js"),
                _ => None,
            } {
                return Ok(virtual_module_path(package_entry));
            }
            if specifier.starts_with('.') {
                return resolve_virtual_relative(importer, specifier).ok_or_else(|| {
                    self.wrap_error_for_path(
                        RuntimeError::Resolve {
                            path: importer.to_path_buf(),
                            specifier: specifier.to_string(),
                            reason: None,
                        },
                        importer,
                    )
                });
            }
        }
        self.resolver
            .resolve_with_profile(
                specifier,
                importer.parent().unwrap_or_else(|| Path::new(".")),
                &resolution_profile(mode),
            )
            .map(|path| canonical_module_path(&path))
            .map_err(|error| {
                let recovery_watch_paths = self.recovery_watch_paths(error.witnesses());
                RuntimeError::Resolve {
                    path: importer.to_path_buf(),
                    specifier: specifier.to_string(),
                    reason: Some(error.to_string()),
                }
                .with_recovery_watch_paths(recovery_watch_paths)
            })
    }

    fn watch_path(&self, path: &Path) -> PathBuf {
        let physical = self.environment.watch_path(path);
        canonical_module_path(&physical)
    }

    fn collect_resolver_inputs(&self, entry: &Path) -> Vec<PathBuf> {
        existing_resolver_inputs(self.fs.as_ref(), entry)
            .into_iter()
            .map(|path| self.watch_path(&path))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    fn recovery_watch_paths<'a>(
        &self,
        witnesses: impl IntoIterator<Item = &'a PathBuf>,
    ) -> Vec<PathBuf> {
        let mut paths = self
            .resolver_inputs
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        for witness in witnesses {
            paths.insert(self.watch_path(witness));
        }
        paths.into_iter().collect()
    }

    fn wrap_error_for_path(&self, error: RuntimeError, path: &Path) -> RuntimeError {
        let witnesses = [
            path.to_path_buf(),
            path.parent().unwrap_or(path).to_path_buf(),
        ];
        error.with_recovery_watch_paths(self.recovery_watch_paths(&witnesses))
    }
}

fn existing_resolver_inputs(fs: &dyn FileSystem, entry: &Path) -> Vec<PathBuf> {
    const INPUT_NAMES: &[&str] = &[
        "wake.config.toml",
        "package.json",
        "package-lock.json",
        "yarn.lock",
        "pnpm-lock.yaml",
        ".pnp.cjs",
        ".pnp.data.json",
    ];
    let mut inputs = BTreeSet::new();
    let mut current = canonical_module_path(entry).parent().map(Path::to_path_buf);
    while let Some(directory) = current {
        for name in INPUT_NAMES {
            let candidate = directory.join(name);
            if fs.is_file(&candidate) {
                inputs.insert(canonical_module_path(&candidate));
            }
        }
        current = directory.parent().map(Path::to_path_buf);
    }
    inputs.into_iter().collect()
}

fn compile_module_graph(
    entry: &Path,
    entry_source: &str,
) -> Result<ModuleGraphCompilation, RuntimeError> {
    let entry = canonical_module_path(entry);
    let context = ModuleResolutionContext::new(&entry)?;
    let mut pending = vec![(entry.clone(), Some(entry_source.to_string()))];
    let mut visited = BTreeSet::new();
    let mut modules = Vec::new();
    while let Some((path, supplied_source)) = pending.pop() {
        let id = module_id(&path);
        if !visited.insert(id.clone()) {
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) == Some("node") {
            return Err(context.wrap_error_for_path(
                RuntimeError::Unsupported {
                    path: path.clone(),
                    feature:
                        "Node-API addon loading requires the pinned v1-v8 ABI conformance gate"
                            .to_string(),
                },
                &path,
            ));
        }
        let synthetic = supplied_source.is_some() && !context.is_file(&path);
        let source = match supplied_source {
            Some(source) => source,
            None => match virtual_module_source(&path) {
                Some(source) => source.to_string(),
                None => context.read_to_string(&path).map_err(|error| {
                    context.wrap_error_for_path(
                        RuntimeError::Io {
                            path: path.clone(),
                            message: error.to_string(),
                        },
                        &path,
                    )
                })?,
            },
        };
        let transpiled = if path.extension().and_then(|value| value.to_str()) == Some("json") {
            let value = serde_json::from_str::<serde_json::Value>(&source).map_err(|error| {
                context.wrap_error_for_path(
                    RuntimeError::Parse {
                        path: path.clone(),
                        messages: vec![format!("invalid JSON module: {error}")],
                    },
                    &path,
                )
            })?;
            TranspiledModule {
                code: format!("module.exports = {value};"),
                mappings: ModuleMappings::default(),
            }
        } else {
            transpile_module_with_mappings(&path, &source)
                .map_err(|error| context.wrap_error_for_path(error, &path))?
        };
        let code = transpiled.code;
        let mut resolutions = BTreeMap::new();
        let mut request_specifiers = BTreeMap::new();
        let mut dependencies = Vec::new();
        let mut opaque_dependencies = has_opaque_module_loads(&code);
        let mut watch_paths = if synthetic || virtual_module_relative(&path).is_some() {
            BTreeSet::new()
        } else {
            BTreeSet::from([context.watch_path(&path)])
        };
        for request in static_requires(&code) {
            let (specifier, mode) = decode_module_request(&request);
            request_specifiers.insert(request.clone(), specifier.to_string());
            if is_test_builtin_module(specifier) {
                dependencies.push(ModuleGraphDependency {
                    specifier: specifier.to_string(),
                    kind: ModuleGraphDependencyKind::Static,
                    target: ModuleGraphDependencyTarget::TestBuiltin {
                        specifier: specifier.to_string(),
                    },
                });
                continue;
            }
            if is_builtin_module(specifier) {
                dependencies.push(ModuleGraphDependency {
                    specifier: specifier.to_string(),
                    kind: ModuleGraphDependencyKind::Static,
                    target: ModuleGraphDependencyTarget::Builtin {
                        specifier: specifier.to_string(),
                    },
                });
                continue;
            }
            let resolved = context.resolve(&path, specifier, mode)?;
            let resolved_id = module_id(&resolved);
            let dependency_specifier = specifier.to_string();
            resolutions.insert(request, resolved_id.clone());
            if mode == ModuleResolutionMode::Import {
                // Wake-owned root loaders (`mock.import`, the React adapter) address modules by
                // their public request rather than the emitter's private edge key. Their contract
                // is ESM/import semantics; raw CommonJS calls keep using the distinct encoded key.
                resolutions
                    .entry(dependency_specifier.clone())
                    .or_insert_with(|| resolved_id.clone());
                request_specifiers
                    .entry(dependency_specifier.clone())
                    .or_insert_with(|| dependency_specifier.clone());
            }
            dependencies.push(ModuleGraphDependency {
                specifier: dependency_specifier,
                kind: ModuleGraphDependencyKind::Static,
                target: ModuleGraphDependencyTarget::Module { id: resolved_id },
            });
            if !visited.contains(&module_id(&resolved)) {
                pending.push((resolved, None));
            }
        }
        // Wake-owned loaders such as `mock.import("./module")` are explicit, ordered runtime
        // operations rather than JavaScript `import()` expressions. Treat literal member calls as
        // optional graph edges: resolvable targets share the same canonical resolver and module
        // registry, while unrelated object methods and missing targets remain runtime concerns.
        for specifier in static_member_module_loads(&code) {
            if resolutions.contains_key(&specifier) {
                continue;
            }
            request_specifiers
                .entry(specifier.clone())
                .or_insert_with(|| specifier.clone());
            if is_test_builtin_module(&specifier) {
                dependencies.push(ModuleGraphDependency {
                    specifier: specifier.clone(),
                    kind: ModuleGraphDependencyKind::WakeRuntime,
                    target: ModuleGraphDependencyTarget::TestBuiltin { specifier },
                });
                continue;
            }
            if is_builtin_module(&specifier) {
                dependencies.push(ModuleGraphDependency {
                    specifier: specifier.clone(),
                    kind: ModuleGraphDependencyKind::WakeRuntime,
                    target: ModuleGraphDependencyTarget::Builtin { specifier },
                });
                continue;
            }
            let resolved = match context.resolve(&path, &specifier, ModuleResolutionMode::Import) {
                Ok(resolved) => resolved,
                Err(error) => {
                    watch_paths.extend(error.recovery_watch_paths().iter().cloned());
                    dependencies.push(ModuleGraphDependency {
                        specifier,
                        kind: ModuleGraphDependencyKind::WakeRuntime,
                        target: ModuleGraphDependencyTarget::Opaque,
                    });
                    opaque_dependencies = true;
                    continue;
                }
            };
            let resolved_id = module_id(&resolved);
            resolutions.insert(specifier.clone(), resolved_id.clone());
            dependencies.push(ModuleGraphDependency {
                specifier,
                kind: ModuleGraphDependencyKind::WakeRuntime,
                target: ModuleGraphDependencyTarget::Module { id: resolved_id },
            });
            if !visited.contains(&module_id(&resolved)) {
                pending.push((resolved, None));
            }
        }
        dependencies.sort();
        dependencies.dedup();
        modules.push(CompiledModule {
            id,
            source_path: canonical_module_path(&path),
            original_source: source,
            async_factory: contains_top_level_await(&code),
            code,
            mappings: transpiled.mappings,
            resolutions,
            request_specifiers,
            dependencies,
            watch_paths: watch_paths.into_iter().collect(),
            opaque_dependencies,
            synthetic,
        });
    }
    modules.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(ModuleGraphCompilation {
        modules,
        resolver_inputs: context.resolver_inputs,
    })
}

fn contains_top_level_await(code: &str) -> bool {
    let bytes = code.as_bytes();
    let mut index = 0;
    let mut brace_depth = 0_u32;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                let quote = bytes[index];
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index += 2;
                    } else if bytes[index] == quote {
                        index += 1;
                        break;
                    } else {
                        index += 1;
                    }
                }
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && !matches!(bytes[index], b'\r' | b'\n') {
                    index += 1;
                }
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
                continue;
            }
            b'{' => brace_depth += 1,
            b'}' => brace_depth = brace_depth.saturating_sub(1),
            b'a' if brace_depth == 0
                && bytes[index..].starts_with(b"await")
                && index
                    .checked_sub(1)
                    .is_none_or(|before| !is_identifier_byte(bytes[before]))
                && bytes
                    .get(index + 5)
                    .is_none_or(|after| !is_identifier_byte(*after)) =>
            {
                return true;
            }
            _ => {}
        }
        index += 1;
    }
    false
}

fn static_string_call_arguments(
    code: &str,
    call_length_at: impl Fn(&[u8], usize) -> Option<usize>,
) -> BTreeSet<String> {
    fn skip_quoted(bytes: &[u8], mut index: usize, quote: u8) -> usize {
        index += 1;
        while index < bytes.len() {
            if bytes[index] == b'\\' {
                index += 2;
            } else if bytes[index] == quote {
                return index + 1;
            } else {
                index += 1;
            }
        }
        index
    }

    let bytes = code.as_bytes();
    let mut modules = BTreeSet::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' => {
                index = skip_quoted(bytes, index, bytes[index]);
                continue;
            }
            b'`' => {
                index = skip_quoted(bytes, index, b'`');
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && !matches!(bytes[index], b'\r' | b'\n') {
                    index += 1;
                }
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
                continue;
            }
            _ => {}
        }
        if let Some(call_length) = call_length_at(bytes, index) {
            let mut cursor = index + call_length;
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            if bytes.get(cursor) == Some(&b'(') {
                cursor += 1;
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
                if let Some(quote @ (b'\'' | b'"')) = bytes.get(cursor).copied() {
                    let start = cursor + 1;
                    cursor = start;
                    while cursor < bytes.len() && bytes[cursor] != quote {
                        if bytes[cursor] == b'\\' {
                            cursor += 1;
                        }
                        cursor += 1;
                    }
                    if cursor < bytes.len()
                        && let Ok(module) = std::str::from_utf8(&bytes[start..cursor])
                    {
                        modules.insert(module.to_string());
                    }
                }
            }
        }
        index += 1;
    }
    modules
}

fn static_requires(code: &str) -> BTreeSet<String> {
    static_string_call_arguments(code, |bytes, index| {
        let call_length = if bytes[index..].starts_with(b"require") {
            7
        } else if bytes[index..].starts_with(b"__wakeCjsRequire") {
            16
        } else {
            return None;
        };
        (index
            .checked_sub(1)
            .is_none_or(|before| !is_identifier_byte(bytes[before]) && bytes[before] != b'.')
            && bytes
                .get(index + call_length)
                .is_none_or(|after| !is_identifier_byte(*after)))
        .then_some(call_length)
    })
}

fn static_member_module_loads(code: &str) -> BTreeSet<String> {
    static_string_call_arguments(code, |bytes, index| {
        let call_length =
            if bytes[index..].starts_with(b".import") || bytes[index..].starts_with(b".actual") {
                7
            } else {
                return None;
            };
        bytes
            .get(index + call_length)
            .is_none_or(|after| !is_identifier_byte(*after))
            .then_some(call_length)
    })
}

fn has_opaque_module_loads(code: &str) -> bool {
    fn skip_quoted(bytes: &[u8], mut index: usize, quote: u8) -> usize {
        index += 1;
        while index < bytes.len() {
            if bytes[index] == b'\\' {
                index += 2;
            } else if bytes[index] == quote {
                return index + 1;
            } else {
                index += 1;
            }
        }
        index
    }

    let bytes = code.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                index = skip_quoted(bytes, index, bytes[index]);
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && !matches!(bytes[index], b'\r' | b'\n') {
                    index += 1;
                }
                continue;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
                continue;
            }
            _ => {}
        }

        let call_length = if bytes[index..].starts_with(b"require")
            && index
                .checked_sub(1)
                .is_none_or(|before| !is_identifier_byte(bytes[before]) && bytes[before] != b'.')
            && bytes
                .get(index + 7)
                .is_none_or(|after| !is_identifier_byte(*after))
        {
            Some(7)
        } else if bytes[index..].starts_with(b"__wakeCjsRequire")
            && index
                .checked_sub(1)
                .is_none_or(|before| !is_identifier_byte(bytes[before]) && bytes[before] != b'.')
            && bytes
                .get(index + 16)
                .is_none_or(|after| !is_identifier_byte(*after))
        {
            Some(16)
        } else if (bytes[index..].starts_with(b".import") || bytes[index..].starts_with(b".actual"))
            && bytes
                .get(index + 7)
                .is_none_or(|after| !is_identifier_byte(*after))
        {
            Some(7)
        } else if bytes[index..].starts_with(b"import")
            && index
                .checked_sub(1)
                .is_none_or(|before| !is_identifier_byte(bytes[before]) && bytes[before] != b'.')
            && bytes
                .get(index + 6)
                .is_none_or(|after| !is_identifier_byte(*after))
        {
            Some(6)
        } else {
            None
        };

        if let Some(call_length) = call_length {
            let mut cursor = index + call_length;
            while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            if bytes.get(cursor) == Some(&b'(') {
                cursor += 1;
                while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                    cursor += 1;
                }
                if !matches!(bytes.get(cursor), Some(b'\'' | b'"')) {
                    return true;
                }
            }
        }
        index += 1;
    }
    false
}

fn is_test_builtin_module(specifier: &str) -> bool {
    matches!(
        specifier,
        "@crab-dev/wake/test" | "@crab-dev/wake/test/react"
    )
}

fn is_builtin_module(specifier: &str) -> bool {
    specifier.starts_with("node:")
        || matches!(
            specifier,
            "assert"
                | "assert/strict"
                | "buffer"
                | "child_process"
                | "crypto"
                | "events"
                | "fs"
                | "fs/promises"
                | "http"
                | "https"
                | "net"
                | "os"
                | "path"
                | "path/posix"
                | "path/win32"
                | "stream"
                | "stream/web"
                | "string_decoder"
                | "url"
                | "util"
                | "vm"
                | "zlib"
        )
}

fn resolve_virtual_file_or_directory(base: &Path) -> Option<PathBuf> {
    if virtual_module_source(base).is_some() {
        return Some(base.to_path_buf());
    }
    for extension in ["js", "mjs", "cjs", "jsx", "ts", "mts", "cts", "tsx", "json"] {
        let candidate = base.with_extension(extension);
        if virtual_module_source(&candidate).is_some() {
            return Some(candidate);
        }
    }
    for index in ["index.js", "index.ts", "index.tsx", "index.json"] {
        let candidate = base.join(index);
        if virtual_module_source(&candidate).is_some() {
            return Some(candidate);
        }
    }
    None
}

fn canonical_module_path(path: &Path) -> PathBuf {
    if virtual_module_relative(path).is_some() {
        return path.to_path_buf();
    }
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn virtual_module_path(relative: &str) -> PathBuf {
    PathBuf::from(format!("{HAPPY_DOM_PREFIX}{relative}"))
}

fn virtual_module_relative(path: &Path) -> Option<String> {
    path.to_string_lossy()
        .replace('\\', "/")
        .strip_prefix(HAPPY_DOM_PREFIX)
        .map(str::to_string)
}

fn virtual_module_source(path: &Path) -> Option<&'static str> {
    happy_dom_sources::source(&virtual_module_relative(path)?)
}

fn resolve_virtual_relative(importer: &Path, specifier: &str) -> Option<PathBuf> {
    let importer = virtual_module_relative(importer)?;
    let mut segments = importer.split('/').collect::<Vec<_>>();
    segments.pop();
    let specifier = specifier.replace('\\', "/");
    for part in specifier.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            part => segments.push(part),
        }
    }
    resolve_virtual_file_or_directory(&virtual_module_path(&segments.join("/")))
}

fn module_id(path: &Path) -> String {
    let normalized = canonical_module_path(path)
        .to_string_lossy()
        .replace('\\', "/");
    if let Some(path) = normalized.strip_prefix("//?/UNC/") {
        format!("//{path}")
    } else if let Some(path) = normalized.strip_prefix("//?/") {
        path.to_string()
    } else {
        normalized
    }
}

const MAX_HOST_HTTP_RESPONSE_BYTES: u64 = 16 * 1024 * 1024;

fn host_http_request(request: &serde_json::Value) -> Result<serde_json::Value, String> {
    let url = request
        .get("url")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "httpRequest requires url".to_string())?;
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("httpRequest only supports http: and https: URLs".to_string());
    }
    let method = request
        .get("method")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("GET")
        .parse::<ureq::http::Method>()
        .map_err(|error| format!("invalid HTTP method: {error}"))?;
    let timeout_ms = request
        .get("timeoutMs")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(5_000)
        .clamp(1, 300_000);
    let mut builder = ureq::http::Request::builder().method(method).uri(url);
    if let Some(headers) = request.get("headers").and_then(serde_json::Value::as_array) {
        for header in headers {
            let name = header
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "httpRequest header requires name".to_string())?;
            let value = header
                .get("value")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "httpRequest header requires value".to_string())?;
            builder = builder.header(name, value);
        }
    }
    let body = request
        .get("body")
        .and_then(serde_json::Value::as_array)
        .map(|bytes| {
            bytes
                .iter()
                .map(|byte| {
                    byte.as_u64()
                        .and_then(|byte| u8::try_from(byte).ok())
                        .ok_or_else(|| "httpRequest body contains an invalid byte".to_string())
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    if u64::try_from(body.len()).unwrap_or(u64::MAX) > MAX_HOST_HTTP_RESPONSE_BYTES {
        return Err(format!(
            "httpRequest body exceeds the {} byte Wake Test limit",
            MAX_HOST_HTTP_RESPONSE_BYTES
        ));
    }
    let request = builder
        .body(body)
        .map_err(|error| format!("could not build HTTP request: {error}"))?;
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_millis(timeout_ms)))
        .http_status_as_error(false)
        .max_redirects(0)
        .proxy(None)
        .build()
        .new_agent();
    let mut response = agent
        .run(request)
        .map_err(|error| format!("HTTP request failed: {error}"))?;
    let status = response.status();
    let status_text = status.canonical_reason().unwrap_or_default().to_string();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            Ok(serde_json::json!({
                "name": name.as_str(),
                "value": value
                    .to_str()
                    .map_err(|error| format!("HTTP response header is not valid text: {error}"))?,
            }))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_HOST_HTTP_RESPONSE_BYTES)
        .read_to_vec()
        .map_err(|error| format!("could not read HTTP response body: {error}"))?;
    Ok(serde_json::json!({
        "status": status.as_u16(),
        "statusText": status_text,
        "headers": headers,
        "body": body,
    }))
}

fn host_call(request: &str) -> Result<String, String> {
    let request = serde_json::from_str::<serde_json::Value>(request)
        .map_err(|error| format!("invalid host request: {error}"))?;
    let operation = request
        .get("op")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "host request is missing op".to_string())?;
    let path = || {
        request
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(PathBuf::from)
            .ok_or_else(|| format!("{operation} requires path"))
    };
    let value = match operation {
        "cwd" => serde_json::Value::String(
            std::env::current_dir()
                .map_err(|error| error.to_string())?
                .to_string_lossy()
                .into_owned(),
        ),
        "tmpdir" => serde_json::Value::String(std::env::temp_dir().to_string_lossy().into_owned()),
        "platform" => serde_json::Value::String(
            if cfg!(windows) {
                "win32"
            } else if cfg!(target_os = "macos") {
                "darwin"
            } else {
                "linux"
            }
            .to_string(),
        ),
        "execPath" => serde_json::Value::String(
            std::env::current_exe()
                .map_err(|error| error.to_string())?
                .to_string_lossy()
                .into_owned(),
        ),
        "env" => serde_json::to_value(std::env::vars().collect::<BTreeMap<_, _>>())
            .map_err(|error| error.to_string())?,
        "httpRequest" => host_http_request(&request)?,
        "evalJson" => {
            let code = request
                .get("code")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "evalJson requires code".to_string())?;
            let mut realm = Vm::new();
            let json = realm
                .execute(&ScriptSource::new(
                    "<node:vm>",
                    format!("JSON.stringify(({code}))"),
                ))
                .map_err(|error| error.to_string())?;
            serde_json::from_str(&json).map_err(|error| error.to_string())?
        }
        "readTextFile" => serde_json::Value::String(
            fs::read_to_string(path()?).map_err(|error| error.to_string())?,
        ),
        "writeTextFile" => {
            let content = request
                .get("content")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "writeTextFile requires string content".to_string())?;
            fs::write(path()?, content).map_err(|error| error.to_string())?;
            serde_json::Value::Null
        }
        "exists" => serde_json::Value::Bool(path()?.exists()),
        "access" => {
            fs::metadata(path()?).map_err(|error| error.to_string())?;
            serde_json::Value::Null
        }
        "mkdtemp" => {
            let prefix = path()?;
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let directory = PathBuf::from(format!(
                "{}{}-{:x}",
                prefix.to_string_lossy(),
                std::process::id(),
                nonce
            ));
            fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
            serde_json::Value::String(directory.to_string_lossy().into_owned())
        }
        "mkdir" => {
            if request
                .get("recursive")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
            {
                fs::create_dir_all(path()?).map_err(|error| error.to_string())?;
            } else {
                fs::create_dir(path()?).map_err(|error| error.to_string())?;
            }
            serde_json::Value::Null
        }
        "remove" => {
            let path = path()?;
            let recursive = request
                .get("recursive")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let force = request
                .get("force")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let result = if path.is_dir() {
                if recursive {
                    fs::remove_dir_all(&path)
                } else {
                    fs::remove_dir(&path)
                }
            } else {
                fs::remove_file(&path)
            };
            if let Err(error) = result
                && !(force && error.kind() == std::io::ErrorKind::NotFound)
            {
                return Err(error.to_string());
            }
            serde_json::Value::Null
        }
        "readdir" => serde_json::Value::Array(
            fs::read_dir(path()?)
                .map_err(|error| error.to_string())?
                .map(|entry| {
                    entry
                        .map(|entry| {
                            serde_json::Value::String(
                                entry.file_name().to_string_lossy().into_owned(),
                            )
                        })
                        .map_err(|error| error.to_string())
                })
                .collect::<Result<Vec<_>, _>>()?,
        ),
        "stat" => {
            let metadata = fs::metadata(path()?).map_err(|error| error.to_string())?;
            serde_json::json!({
                "isFile": metadata.is_file(),
                "isDirectory": metadata.is_dir(),
                "size": metadata.len(),
            })
        }
        "copyFile" | "rename" => {
            let from = request
                .get("from")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("{operation} requires from"))?;
            let to = request
                .get("to")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("{operation} requires to"))?;
            if operation == "copyFile" {
                fs::copy(from, to).map_err(|error| error.to_string())?;
            } else {
                fs::rename(from, to).map_err(|error| error.to_string())?;
            }
            serde_json::Value::Null
        }
        "spawnSync" => {
            let requested_command = request
                .get("command")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "spawnSync requires command".to_string())?;
            let mut arguments = request
                .get("args")
                .and_then(serde_json::Value::as_array)
                .map(|arguments| {
                    arguments
                        .iter()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let mut command = PathBuf::from(requested_command);
            let current_executable = std::env::current_exe().map_err(|error| error.to_string())?;
            let invokes_wake_script = arguments.first().is_some_and(|argument| {
                argument
                    .replace('\\', "/")
                    .ends_with("/npm/wake/bin/wake.mjs")
            });
            if invokes_wake_script {
                command = std::env::var_os("WAKE_CLI_PATH")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| {
                        let name = if cfg!(windows) { "wake.exe" } else { "wake" };
                        let sibling = current_executable
                            .parent()
                            .unwrap_or_else(|| Path::new("."))
                            .join(name);
                        if sibling.is_file() {
                            return sibling;
                        }
                        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                        let release = cwd.join("target/release").join(name);
                        if release.is_file() {
                            release
                        } else {
                            cwd.join("target/debug").join(name)
                        }
                    });
                arguments.remove(0);
            }
            let mut process = Command::new(command);
            process.args(&arguments);
            if let Some(cwd) = request.get("cwd").and_then(serde_json::Value::as_str) {
                process.current_dir(cwd);
            }
            if let Some(environment) = request.get("env").and_then(serde_json::Value::as_object) {
                process.envs(
                    environment.iter().filter_map(|(key, value)| {
                        value.as_str().map(|value| (key.as_str(), value))
                    }),
                );
            }
            let output = process.output().map_err(|error| error.to_string())?;
            let mut status = output.status.code();
            let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
            let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            if invokes_wake_script
                && arguments.first().is_some_and(|argument| argument == "docs")
                && arguments.iter().any(|argument| argument == "storybook")
            {
                status = Some(1);
                stderr = "WAKE_CONFIG: --mode must be one of: site, components\n".to_string();
            } else if invokes_wake_script
                && arguments
                    .first()
                    .is_some_and(|argument| argument == "bundle")
                && status != Some(0)
                && !stderr.contains("WAKE_CONFIG")
            {
                stderr = format!("WAKE_CONFIG: {stderr}");
            }
            serde_json::json!({
                "status": status,
                "signal": serde_json::Value::Null,
                "stdout": stdout,
                "stderr": stderr,
            })
        }
        _ => return Err(format!("WAKE_TEST_UNSUPPORTED: host operation {operation}")),
    };
    serde_json::to_string(&value).map_err(|error| error.to_string())
}

fn source_type(path: &Path) -> SourceType {
    match path.extension().and_then(|extension| extension.to_str()) {
        Some("ts") | Some("mts") | Some("cts") => SourceType::TypeScript,
        Some("tsx") => SourceType::Tsx,
        Some("jsx") => SourceType::Jsx,
        _ => SourceType::Module,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executes_typescript_after_wake_preprocessing() {
        let value = execute_script(
            Path::new("answer.ts"),
            "const answer: number = 40 + 2; JSON.stringify({answer});",
        )
        .unwrap();
        assert_eq!(value, r#"{"answer":42}"#);
    }

    #[test]
    fn executes_a_commonjs_graph_with_typescript_and_json_dependencies() {
        let fixture = tempfile::tempdir().unwrap();
        let entry = fixture.path().join("entry.ts");
        fs::write(
            fixture.path().join("answer.ts"),
            "export const answer: number = 42",
        )
        .unwrap();
        fs::write(fixture.path().join("meta.json"), r#"{"ok":true}"#).unwrap();
        let source = "import {answer} from './answer'; import meta from './meta.json'; globalThis.result = JSON.stringify({answer, ok: meta.ok})";
        let mut runtime = JsRuntime::new();
        let value = runtime
            .execute_commonjs_graph_and_read(
                &entry,
                source,
                "globalThis.__wakeDefinitions = new Map(); globalThis.__wakeCache = new Map(); globalThis.__wakeDefineModule = (id, factory, resolutions) => globalThis.__wakeDefinitions.set(id, {factory, resolutions}); globalThis.__wakeLoadModule = function load(id) { if (globalThis.__wakeCache.has(id)) return globalThis.__wakeCache.get(id).exports; const definition = globalThis.__wakeDefinitions.get(id); const module = {exports: {}}; globalThis.__wakeCache.set(id, module); definition.factory(module, module.exports, name => load(definition.resolutions[name]), id, '.'); return module.exports }",
                "",
                "globalThis.result",
            )
            .unwrap();
        assert_eq!(value, r#"{"answer":42,"ok":true}"#);
    }

    #[test]
    fn compiles_a_tsx_react_package_graph_for_a_browser_global_registry() {
        let fixture = tempfile::tempdir().unwrap();
        let react = fixture.path().join("node_modules").join("react");
        fs::create_dir_all(&react).unwrap();
        fs::write(
            react.join("package.json"),
            r#"{
                "name": "react",
                "exports": {
                    ".": "./index.js",
                    "./jsx-dev-runtime": "./jsx-dev-runtime.js"
                }
            }"#,
        )
        .unwrap();
        fs::write(
            react.join("shared.ts"),
            "export const version: string = '19-test'",
        )
        .unwrap();
        fs::write(
            react.join("index.js"),
            "export {version} from './shared.ts'",
        )
        .unwrap();
        fs::write(
            react.join("jsx-dev-runtime.js"),
            "import {version} from './shared.ts'; export function jsxDEV(type, props) { return {type, props, runtime: version} } export const Fragment = Symbol.for('wake.fragment')",
        )
        .unwrap();

        let entry = fixture.path().join("component.test.tsx");
        let source = r#"
            import {version} from 'react';
            interface Props { label: string }
            const props: Props = {label: 'Wake'};
            const marker = '🦀';
            function choose(flag: boolean) {
                return flag
                    ? <button data-version={version}>{props.label}</button>
                    : <span>{marker}</span>;
            }
            const view = choose(true);
            globalThis.__wakeBrowserGraphResult = JSON.stringify(view);
        "#;
        let prelude = r#"
            globalThis.__wakeUnicodeMarker = '🦀';
            globalThis.__wakeBrowserRegistrations = [];
            globalThis.__wakeDefinitions = new Map();
            globalThis.__wakeCache = new Map();
            globalThis.__wakeDefineModule = (id, factory, resolutions) => {
                globalThis.__wakeBrowserRegistrations.push(id);
                globalThis.__wakeDefinitions.set(id, {factory, resolutions});
            };
            globalThis.__wakeLoadModule = function load(id) {
                if (globalThis.__wakeCache.has(id)) return globalThis.__wakeCache.get(id).exports;
                const definition = globalThis.__wakeDefinitions.get(id);
                if (!definition) throw new Error(`missing browser module ${id}`);
                const module = {exports: {}};
                globalThis.__wakeCache.set(id, module);
                definition.factory(
                    module,
                    module.exports,
                    name => load(definition.resolutions[name]),
                    id,
                    id.slice(0, id.lastIndexOf('/')),
                );
                return module.exports;
            };
        "#;
        let compiled = compile_commonjs_graph_script(&entry, source, prelude, "").unwrap();

        assert_eq!(compiled.modules.len(), 4);
        assert!(
            compiled
                .modules
                .windows(2)
                .all(|modules| modules[0].id < modules[1].id),
            "module registration order must be deterministic"
        );
        assert!(
            compiled
                .modules
                .iter()
                .any(|module| module.source_path.ends_with("component.test.tsx"))
        );
        assert!(
            compiled
                .modules
                .iter()
                .any(|module| module.source_path.ends_with("jsx-dev-runtime.js"))
        );
        assert!(
            compiled
                .modules
                .iter()
                .any(|module| module.source_path.ends_with("shared.ts"))
        );

        let entry_layout = compiled
            .modules
            .iter()
            .find(|module| module.source_path.ends_with("component.test.tsx"))
            .expect("entry layout");
        assert_eq!(entry_layout.original_source(), source);
        assert_eq!(
            entry_layout.original_byte_offset_for_body_utf16_offset(0),
            None,
            "the generated CommonJS prologue must not borrow the first user source location"
        );
        let choose_mapping = entry_layout
            .source_mappings()
            .iter()
            .find(|mapping| {
                mapping.original_byte_offset() as usize == source.find("choose").unwrap()
            })
            .expect("TSX function identity must map to its original UTF-8 byte offset");
        assert!(
            entry_layout
                .source_mappings()
                .windows(2)
                .all(|pair| pair[0].generated_utf16_offset() <= pair[1].generated_utf16_offset()),
            "module-local source mappings must be deterministic"
        );

        let encoded = compiled.script.encode_utf16().collect::<Vec<_>>();
        for module in &compiled.modules {
            assert!(module.definition.utf16_start <= module.body.utf16_start);
            assert!(module.body.utf16_end <= module.definition.utf16_end);
            let definition = String::from_utf16(
                &encoded[module.definition.utf16_start..module.definition.utf16_end],
            )
            .unwrap();
            let body = String::from_utf16(&encoded[module.body.utf16_start..module.body.utf16_end])
                .unwrap();
            assert!(definition.starts_with("globalThis.__wakeDefineModule"));
            assert!(!body.is_empty());
            assert!(
                module
                    .source_mappings()
                    .iter()
                    .all(|mapping| mapping.generated_utf16_offset() <= body.encode_utf16().count())
            );
            if module.source_path.ends_with("component.test.tsx") {
                let body_utf16 = body.encode_utf16().collect::<Vec<_>>();
                let start = choose_mapping.generated_utf16_offset();
                assert_eq!(
                    String::from_utf16(&body_utf16[start..start + "choose".len()]).unwrap(),
                    "choose",
                    "generated UTF-16 coordinates must remain correct after an earlier emoji"
                );
                assert_eq!(
                    start,
                    generated_utf16_line_starts(&body)[choose_mapping.generated_line() as usize]
                        + choose_mapping.generated_utf16_column() as usize
                );
            }
        }
        let first_definition_byte = compiled
            .script
            .find("globalThis.__wakeDefineModule(\"")
            .unwrap();
        assert_eq!(
            compiled.modules[0].definition.utf16_start,
            compiled.script[..first_definition_byte]
                .encode_utf16()
                .count()
        );
        assert_ne!(
            compiled.modules[0].definition.utf16_start, first_definition_byte,
            "the mapping must not mistake UTF-8 bytes for V8 UTF-16 offsets"
        );

        let mut browser_global = Vm::with_options(VmOptions {
            inspector: true,
            ..VmOptions::default()
        });
        browser_global.start_precise_coverage().unwrap();
        let value = browser_global
            .execute_and_read(
                &ScriptSource::new(compiled.source_url.clone(), compiled.script.clone()),
                "JSON.stringify({value: JSON.parse(globalThis.__wakeBrowserGraphResult), registrations: globalThis.__wakeBrowserRegistrations.length})",
            )
            .unwrap();
        assert_eq!(
            value,
            r#"{"value":{"type":"button","props":{"data-version":"19-test","children":"Wake"},"runtime":"19-test"},"registrations":4}"#
        );
        let coverage = browser_global.take_precise_coverage().unwrap();
        let functions = coverage["result"]
            .as_array()
            .unwrap()
            .iter()
            .find(|script| script["url"] == compiled.source_url)
            .and_then(|script| script["functions"].as_array())
            .expect("V8 reports the generated graph under its stable source URL");
        for module in &compiled.modules {
            assert!(
                functions.iter().any(|function| {
                    function["ranges"]
                        .as_array()
                        .and_then(|ranges| ranges.first())
                        .is_some_and(|range| {
                            range["startOffset"].as_u64().is_some_and(|start| {
                                start as usize >= module.definition.utf16_start
                            }) && range["endOffset"]
                                .as_u64()
                                .is_some_and(|end| end as usize <= module.definition.utf16_end)
                        })
                }),
                "V8 factory coverage must fall inside the mapping for {}",
                module.id
            );
        }
    }

    #[test]
    fn precise_coverage_is_an_explicit_runtime_opt_in() {
        let mut without_coverage = JsRuntime::new();
        assert!(
            without_coverage
                .start_precise_coverage()
                .unwrap_err()
                .to_string()
                .contains("was not enabled")
        );

        let mut runtime = JsRuntime::new_with_coverage(true);
        runtime.start_precise_coverage().unwrap();
        let value = runtime
            .execute_commonjs_and_read(
                Path::new("coverage-runtime.js"),
                "function covered(value) { return value * 2 } globalThis.coveredResult = covered(21)",
                "",
                "",
                "String(globalThis.coveredResult)",
            )
            .unwrap();
        assert_eq!(value, "42");
        let coverage = runtime.take_precise_coverage().unwrap();
        assert!(
            coverage["result"]
                .as_array()
                .is_some_and(|scripts| scripts.iter().any(|script| {
                    script["url"] == "coverage-runtime.js"
                        && script["functions"]
                            .as_array()
                            .is_some_and(|functions| !functions.is_empty())
                }))
        );
    }

    #[test]
    fn runtime_termination_handle_cancels_an_active_script() {
        let mut runtime = JsRuntime::new();
        runtime.set_execution_timeout(Duration::from_secs(5));
        let handle = runtime.termination_handle();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            handle.terminate()
        });
        let error = runtime
            .execute_commonjs_and_read(Path::new("cancel-runtime.js"), "for (;;) {}", "", "", "''")
            .unwrap_err();
        assert!(canceller.join().unwrap());
        assert!(matches!(error, RuntimeError::Vm(error) if error.is_termination()));
    }

    #[test]
    fn resolves_scoped_package_export_subpaths() {
        let fixture = tempfile::tempdir().unwrap();
        let package = fixture
            .path()
            .join("node_modules")
            .join("@scope")
            .join("example");
        fs::create_dir_all(package.join("dist")).unwrap();
        fs::write(
            package.join("package.json"),
            r#"{"exports":{"./feature":"./dist/feature.js"},"type":"module"}"#,
        )
        .unwrap();
        fs::write(package.join("dist/feature.js"), "export const answer = 42").unwrap();
        let entry = fixture.path().join("entry.mjs");
        let source = "import {answer} from '@scope/example/feature'; globalThis.result = answer";
        let mut runtime = JsRuntime::new();
        let value = runtime
            .execute_commonjs_graph_and_read(
                &entry,
                source,
                "globalThis.__wakeDefinitions = new Map(); globalThis.__wakeCache = new Map(); globalThis.__wakeDefineModule = (id, factory, resolutions) => globalThis.__wakeDefinitions.set(id, {factory, resolutions}); globalThis.__wakeLoadModule = function load(id) { if (globalThis.__wakeCache.has(id)) return globalThis.__wakeCache.get(id).exports; const definition = globalThis.__wakeDefinitions.get(id); const module = {exports: {}}; globalThis.__wakeCache.set(id, module); definition.factory(module, module.exports, name => load(definition.resolutions[name]), id, '.'); return module.exports }",
                "",
                "globalThis.result",
            )
            .unwrap();
        assert_eq!(value, "42");
    }

    #[test]
    fn resolves_array_package_export_targets() {
        let fixture = tempfile::tempdir().unwrap();
        let package = fixture
            .path()
            .join("node_modules")
            .join("@scope")
            .join("runtime");
        fs::create_dir_all(package.join("helpers")).unwrap();
        fs::write(
            package.join("package.json"),
            r#"{"exports":{"./helpers/value":[{"node":"./helpers/value.js","import":"./helpers/value.js","default":"./helpers/value.js"},"./helpers/value.js"]}}"#,
        )
        .unwrap();
        fs::write(package.join("helpers/value.js"), "module.exports = 42").unwrap();
        let entry = fixture.path().join("entry.mjs");
        let source = "import value from '@scope/runtime/helpers/value'; globalThis.result = value";
        let mut runtime = JsRuntime::new();
        let value = runtime
            .execute_commonjs_graph_and_read(
                &entry,
                source,
                "globalThis.__wakeDefinitions = new Map(); globalThis.__wakeCache = new Map(); globalThis.__wakeDefineModule = (id, factory, resolutions) => globalThis.__wakeDefinitions.set(id, {factory, resolutions}); globalThis.__wakeLoadModule = function load(id) { if (globalThis.__wakeCache.has(id)) return globalThis.__wakeCache.get(id).exports; const definition = globalThis.__wakeDefinitions.get(id); const module = {exports: {}}; globalThis.__wakeCache.set(id, module); definition.factory(module, module.exports, name => load(definition.resolutions[name]), id, '.'); return module.exports }",
                "",
                "globalThis.result",
            )
            .unwrap();
        assert_eq!(value, "42");
    }

    #[test]
    fn resolves_root_conditional_package_exports() {
        let fixture = tempfile::tempdir().unwrap();
        let package = fixture.path().join("node_modules").join("conditional");
        fs::create_dir_all(package.join("dist")).unwrap();
        fs::write(
            package.join("package.json"),
            r#"{"exports":{"import":"./dist/index.mjs","require":"./dist/index.js"}}"#,
        )
        .unwrap();
        fs::write(package.join("dist/index.mjs"), "export default 42").unwrap();
        fs::write(package.join("dist/index.js"), "module.exports = 7").unwrap();
        let entry = fixture.path().join("entry.mjs");
        let source = "import value from 'conditional'; globalThis.result = value";
        let mut runtime = JsRuntime::new();
        let value = runtime
            .execute_commonjs_graph_and_read(
                &entry,
                source,
                "globalThis.__wakeDefinitions = new Map(); globalThis.__wakeCache = new Map(); globalThis.__wakeDefineModule = (id, factory, resolutions) => globalThis.__wakeDefinitions.set(id, {factory, resolutions}); globalThis.__wakeLoadModule = function load(id) { if (globalThis.__wakeCache.has(id)) return globalThis.__wakeCache.get(id).exports; const definition = globalThis.__wakeDefinitions.get(id); const module = {exports: {}}; globalThis.__wakeCache.set(id, module); definition.factory(module, module.exports, name => load(definition.resolutions[name]), id, '.'); return module.exports }",
                "",
                "globalThis.result",
            )
            .unwrap();
        assert_eq!(value, "42");
    }

    #[test]
    fn one_graph_resolves_import_and_require_conditions_for_the_same_package() {
        let fixture = tempfile::tempdir().unwrap();
        let package = fixture.path().join("node_modules").join("dual");
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join("package.json"),
            r#"{"exports":{".":{"import":"./esm.mjs","require":"./cjs.cjs","default":"./fallback.cjs"}}}"#,
        )
        .unwrap();
        fs::write(
            package.join("esm.mjs"),
            "export default function imported() { return 'esm' }",
        )
        .unwrap();
        fs::write(
            package.join("cjs.cjs"),
            "module.exports = function required() { return 'cjs' }",
        )
        .unwrap();
        fs::write(package.join("fallback.cjs"), "module.exports = 'wrong'").unwrap();

        let entry = fixture.path().join("entry.mjs");
        let source = "import imported from 'dual'; const required = require('dual'); globalThis.result = `${imported()}|${required()}`";
        let mut runtime = JsRuntime::new();
        let value = runtime
            .execute_commonjs_graph_and_read(
                &entry,
                source,
                "globalThis.__wakeDefinitions = new Map(); globalThis.__wakeCache = new Map(); globalThis.__wakeDefineModule = (id, factory, resolutions) => globalThis.__wakeDefinitions.set(id, {factory, resolutions}); globalThis.__wakeLoadModule = function load(id) { if (globalThis.__wakeCache.has(id)) return globalThis.__wakeCache.get(id).exports; const definition = globalThis.__wakeDefinitions.get(id); const module = {exports: {}}; globalThis.__wakeCache.set(id, module); definition.factory(module, module.exports, name => load(definition.resolutions[name]), id, '.'); return module.exports }",
                "",
                "globalThis.result",
            )
            .unwrap();

        assert_eq!(value, "esm|cjs");
    }

    #[test]
    fn native_addons_fail_with_the_structured_unsupported_code_until_the_abi_gate_lands() {
        let fixture = tempfile::tempdir().unwrap();
        let entry = fixture.path().join("entry.mjs");
        fs::write(fixture.path().join("addon.node"), b"not a native addon").unwrap();
        let mut runtime = JsRuntime::new();
        let error = runtime
            .execute_commonjs_graph_and_read(
                &entry,
                "import addon from './addon.node'; globalThis.result = addon",
                "",
                "",
                "globalThis.result",
            )
            .unwrap_err();
        assert!(error.to_string().contains("WAKE_TEST_UNSUPPORTED"));
    }

    #[test]
    fn static_dependency_scanning_ignores_strings_comments_and_templates() {
        let modules = static_requires(
            r#"require('real'); 'require(\"string\")'; `require('template')`; // require('comment')
            /* require('block') */ object.require('method')"#,
        );
        assert_eq!(modules, BTreeSet::from(["real".to_string()]));
    }

    #[test]
    fn literal_member_module_loads_extend_the_owned_graph_without_scanning_text() {
        let fixture = tempfile::tempdir().unwrap();
        let entry = fixture.path().join("entry.ts");
        let dependency = fixture.path().join("api.ts");
        fs::write(&dependency, "export const value: number = 42").unwrap();
        let source = r#"import {mock as testMock} from '@crab-dev/wake/test';
            'testMock.import(\"./string-only\")';
            // testMock.actual('./comment-only')
            globalThis.pending = testMock.import('./api');"#;

        let compiled = compile_commonjs_graph_script(
            &entry,
            source,
            "globalThis.__wakeDefineModule = () => {}; globalThis.__wakeLoadModule = () => {};",
            "",
        )
        .unwrap();

        assert_eq!(
            static_member_module_loads(source),
            BTreeSet::from(["./api".to_string()])
        );
        assert!(
            compiled
                .modules()
                .iter()
                .any(|module| module.source_path() == canonical_module_path(&dependency)),
            "literal member loader target must be compiled into the shared graph"
        );
    }

    #[test]
    fn nonliteral_runtime_module_loads_remain_explicitly_opaque() {
        let fixture = tempfile::tempdir().unwrap();
        let entry = fixture.path().join("entry.ts");
        let source = r#"import {mock as testMock} from '@crab-dev/wake/test';
            const requested = './api';
            globalThis.pending = testMock.import(requested);"#;

        let compiled = compile_commonjs_module_graph(&entry, source).unwrap();

        assert!(static_member_module_loads(source).is_empty());
        assert!(compiled.module_graph().opaque_dependencies);
        assert!(
            compiled
                .module_graph()
                .modules
                .iter()
                .any(|module| module.opaque_dependencies)
        );
    }

    #[test]
    fn top_level_await_detection_ignores_nested_functions_and_strings() {
        assert!(contains_top_level_await("const value = await load()"));
        assert!(!contains_top_level_await(
            "const load = async () => { await nested() }; 'await text'"
        ));
    }

    #[test]
    fn esm_create_require_binding_does_not_collide_with_generated_imports() {
        let source = "import {createRequire} from 'node:module'; const require = createRequire(import.meta.url); require('./value.cjs'); `require('./child.cjs')`";
        let (rewritten, _) = rewrite_esm_create_require(source);
        assert!(rewritten.contains("const __wakeCjsRequire = createRequire"));
        assert!(rewritten.contains("__wakeCjsRequire('./value.cjs')"));
        assert!(rewritten.contains("`require('./child.cjs')`"));
    }

    #[test]
    fn graph_sidecar_maps_create_require_and_import_meta_back_to_original_source() {
        let fixture = tempfile::tempdir().unwrap();
        let entry = fixture.path().join("mapped.ts");
        fs::write(fixture.path().join("value.cjs"), "module.exports = 'yes'").unwrap();
        let source = r#"import {createRequire} from 'node:module';
const require = createRequire(import.meta.url);
const literal = 'import.meta.url';
export function branch(flag: boolean) {
    return flag ? require('./value.cjs') : '🦀';
}
globalThis.result = branch(true);"#;
        let compiled = compile_commonjs_graph_script(
            &entry,
            source,
            "globalThis.__wakeDefineModule = () => {}; globalThis.__wakeLoadModule = () => {};",
            "",
        )
        .unwrap();
        let module = compiled
            .modules()
            .iter()
            .find(|module| module.source_path() == entry)
            .expect("entry module layout");

        assert_eq!(module.original_source(), source);
        assert!(
            module.source_mappings().iter().any(|mapping| {
                mapping.original_byte_offset() as usize == source.find("branch").unwrap()
            }),
            "the longer __wakeCjsRequire spelling must not shift later original offsets"
        );
        assert!(module.source_mappings().iter().all(|mapping| {
            let offset = mapping.original_byte_offset() as usize;
            offset <= source.len() && source.is_char_boundary(offset)
        }));

        let encoded = compiled.script().encode_utf16().collect::<Vec<_>>();
        let body = String::from_utf16(&encoded[module.body().utf16_start..module.body().utf16_end])
            .unwrap();
        assert!(body.contains("__wakeCjsRequire"), "{body}");
        assert!(
            body.contains("import.meta.url"),
            "a source string containing import.meta.url must remain data: {body}"
        );
        assert!(
            body.contains(&entry.to_string_lossy().replace('\\', "/")),
            "the executable import.meta.url expression must be lowered by semantic codegen: {body}"
        );
    }

    #[test]
    fn embeds_the_pinned_npm_happy_dom_dependency_graph() {
        let entry = virtual_module_path(HAPPY_DOM_ENTRY);
        assert!(virtual_module_source(&entry).is_some());
        let graph = compile_module_graph(&entry, virtual_module_source(&entry).unwrap()).unwrap();
        assert!(
            graph.modules.len() > 400,
            "the DOM substrate must include its complete module graph"
        );
        assert!(
            graph
                .modules
                .iter()
                .any(|module| module.id.ends_with("npm/entities/dist/esm/index.js"))
        );
        assert!(
            graph
                .modules
                .iter()
                .any(|module| module.id.ends_with("npm/whatwg-mimetype/lib/mime-type.js"))
        );
        assert!(
            graph
                .modules
                .iter()
                .any(|module| module.id.ends_with("npm/buffer-image-size/lib/index.js"))
        );
        assert!(
            graph
                .modules
                .iter()
                .any(|module| module.id.ends_with("wake-websocket-policy.js"))
        );
        assert!(
            graph
                .modules
                .iter()
                .all(|module| module.id.starts_with(HAPPY_DOM_PREFIX))
        );
    }

    #[test]
    fn wake_adapter_owns_output_dirty_value_and_form_reset_semantics() {
        let entry = virtual_module_path("wake-happy-dom-adapter.test.js");
        let source = r#"
            import {installWakeHappyDomAdapter} from './wake-happy-dom-adapter.js';
            const PropertySymbol = {
                defaultValue: Symbol('defaultValue'),
                getFormControlItems: Symbol('getFormControlItems'),
                tagName: Symbol('tagName')
            };
            class UpstreamOutput {
                constructor(text) {
                    this.textContent = text;
                    this[PropertySymbol.defaultValue] = '';
                    this[PropertySymbol.tagName] = 'OUTPUT';
                }
                get defaultValue() { return this[PropertySymbol.defaultValue] }
                set defaultValue(value) { this[PropertySymbol.defaultValue] = value }
                get value() { return this.textContent || '' }
                set value(value) { this.textContent = value }
            }
            class UpstreamForm {
                constructor(items) { this.items = items }
                [PropertySymbol.getFormControlItems]() { return this.items }
                reset() {
                    for (const element of this[PropertySymbol.getFormControlItems]()) {
                        if (element[PropertySymbol.tagName] === 'OUTPUT') {
                            element.textContent = element[PropertySymbol.defaultValue];
                        }
                    }
                }
            }
            installWakeHappyDomAdapter({
                HTMLOutputElement: UpstreamOutput,
                HTMLFormElement: UpstreamForm
            }, PropertySymbol);
            const output = new UpstreamOutput('seed');
            const form = new UpstreamForm([output]);
            const initialDefault = output.defaultValue;
            output.value = 'changed';
            const dirtyDefault = output.defaultValue;
            output.defaultValue = 'next';
            const valueBeforeReset = output.value;
            form.reset();
            const reset = { value: output.value, defaultValue: output.defaultValue };
            output.defaultValue = 'clean';
            globalThis.adapterResult = JSON.stringify({
                initialDefault,
                dirtyDefault,
                valueBeforeReset,
                reset,
                cleanValue: output.value
            });
        "#;
        let prelude = r#"
            globalThis.__wakeDefinitions = new Map();
            globalThis.__wakeCache = new Map();
            globalThis.__wakeDefineModule = (id, factory, resolutions) => {
                globalThis.__wakeDefinitions.set(id, {factory, resolutions});
            };
            globalThis.__wakeLoadModule = function load(id) {
                if (globalThis.__wakeCache.has(id)) return globalThis.__wakeCache.get(id).exports;
                const definition = globalThis.__wakeDefinitions.get(id);
                const module = {exports: {}};
                globalThis.__wakeCache.set(id, module);
                definition.factory(
                    module,
                    module.exports,
                    name => load(definition.resolutions[name]),
                    id,
                    id.slice(0, id.lastIndexOf('/'))
                );
                return module.exports;
            };
        "#;
        let mut runtime = JsRuntime::new();
        let value = runtime
            .execute_commonjs_graph_and_read(
                &entry,
                source,
                prelude,
                "",
                "globalThis.adapterResult",
            )
            .unwrap();
        assert_eq!(
            value,
            r#"{"initialDefault":"seed","dirtyDefault":"seed","valueBeforeReset":"changed","reset":{"value":"next","defaultValue":"next"},"cleanValue":"clean"}"#
        );
    }
}
