//! Format-neutral module graph used by Wake's component-library pipeline.
//!
//! This module deliberately does not reuse the application bundler's CommonJS module table. A
//! runtime dependency is either an internal source module or a bare external. An external may also
//! point at an analysis-only module; that target is available to static CSS evaluation but is never
//! emitted or promoted into the runtime graph.

use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use wake_common::{FileSystem, FxHashMap, FxHashSet, Interner, OsFileSystem, fs::normalize};
use wake_ecma_ast::SourceType;
pub use wake_ecma_codegen::PreserveModuleFormat;
use wake_ecma_codegen::{ModuleSpecifierRewriter, codegen_preserved_optimized};
use wake_ecma_minify::{OptimizeInput, TrustedExpressionEdit, optimize};
use wake_ecma_parser::{ParseOutput, parse};
use wake_resolver::{ModuleIdentity, ResolutionEnvironment, ResolveOptions};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LibraryDependencyTarget {
    Internal(u32),
    External {
        specifier: String,
        analysis_target: Option<u32>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryDependency {
    pub specifier: String,
    pub target: LibraryDependencyTarget,
}

struct LibraryModule {
    path: PathBuf,
    source: String,
    runtime: bool,
    dependencies: Vec<LibraryDependency>,
    parsed: ParseOutput,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryOutputModule {
    pub id: u32,
    pub source: PathBuf,
    pub file_name: String,
    pub code: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryJavaScriptOutput {
    pub entry: String,
    pub modules: Vec<LibraryOutputModule>,
    pub css: Option<String>,
    pub runtime_module_count: usize,
    pub analysis_module_count: usize,
}

#[derive(Clone, Debug)]
pub struct LibraryGraphOptions {
    pub project_root: PathBuf,
    pub entry: PathBuf,
    /// Bare package names that static evaluation is allowed to inspect. They remain runtime
    /// externals even when resolution succeeds.
    pub analysis_packages: Vec<String>,
    pub resolve: ResolveOptions,
}

impl LibraryGraphOptions {
    pub fn new(project_root: impl Into<PathBuf>, entry: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            entry: entry.into(),
            analysis_packages: Vec::new(),
            resolve: ResolveOptions::default(),
        }
    }
}

pub struct LibraryGraph {
    root: PathBuf,
    package_name: String,
    class_prefix: String,
    entry_id: u32,
    interner: Interner,
    modules: FxHashMap<u32, LibraryModule>,
}

impl LibraryGraph {
    pub fn scan(options: LibraryGraphOptions) -> Result<Self, String> {
        let root = normalize(&options.project_root);
        let package_identity = read_package_name(&root)?;
        let package_scope = package_identity
            .strip_prefix('@')
            .and_then(|name| name.split_once('/').map(|(scope, _)| format!("@{scope}/")));
        let class_prefix = package_identity
            .rsplit('/')
            .next()
            .unwrap_or(&package_identity)
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
            .collect::<String>()
            .to_ascii_lowercase();
        if class_prefix.is_empty() {
            return Err(format!(
                "package name `{package_identity}` cannot produce a CSS class prefix"
            ));
        }
        let entry = if options.entry.is_absolute() {
            normalize(&options.entry)
        } else {
            normalize(&root.join(&options.entry))
        };
        if !entry.starts_with(&root) {
            return Err(format!(
                "library entry must stay inside project root: {}",
                entry.display()
            ));
        }

        let os: Arc<dyn FileSystem> = Arc::new(OsFileSystem);
        let environment = ResolutionEnvironment::with_options(os, options.resolve);
        let fs = environment.file_system();
        let resolver = environment.resolver();

        let analysis_packages: FxHashSet<String> = options.analysis_packages.into_iter().collect();
        let interner = Interner::new();
        let entry_identity = resolver.module_identity(&entry);
        let mut identities = FxHashMap::default();
        identities.insert(entry_identity, 0);
        let mut next_id = 1u32;
        let mut modules: FxHashMap<u32, LibraryModule> = FxHashMap::default();
        let mut requested_runtime = FxHashSet::default();
        requested_runtime.insert(0);
        let mut queue = VecDeque::from([(0u32, entry, true)]);

        while let Some((id, path, runtime_request)) = queue.pop_front() {
            if modules.contains_key(&id) {
                let promote_targets = {
                    let module = modules.get_mut(&id).expect("checked above");
                    if runtime_request && !module.runtime {
                        module.runtime = true;
                        module
                            .dependencies
                            .iter()
                            .filter_map(|dependency| match dependency.target {
                                LibraryDependencyTarget::Internal(target) => Some(target),
                                LibraryDependencyTarget::External { .. } => None,
                            })
                            .collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    }
                };
                for target in promote_targets {
                    if requested_runtime.insert(target) {
                        let target_path = modules
                            .get(&target)
                            .map(|module| module.path.clone())
                            .ok_or_else(|| format!("missing module {target}"))?;
                        queue.push_back((target, target_path, true));
                    }
                }
                continue;
            }

            let source = fs
                .read_to_string(&path)
                .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
            let source_type = source_type(&path)?;
            let parsed = parse(&source, &interner, source_type);
            if parsed.has_errors() {
                let messages = parsed
                    .diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(format!("cannot parse `{}`: {messages}", path.display()));
            }

            let issuer = path.parent().unwrap_or(&root);
            let mut dependencies = Vec::with_capacity(parsed.dependencies.len());
            for dependency in &parsed.dependencies {
                let specifier = interner.resolve(dependency.specifier);
                if is_bare_specifier(&specifier) {
                    let package = bare_package_name(&specifier);
                    let same_workspace_scope = package_scope
                        .as_deref()
                        .is_some_and(|scope| package.starts_with(scope));
                    let analysis_target = if !wake_css_in_js::is_css_in_js_source(package)
                        && (analysis_packages.contains(package) || same_workspace_scope)
                    {
                        resolver
                            .resolve_module(&specifier, issuer)
                            .ok()
                            .map(|resolved| {
                                assign_module(
                                    resolved.identity,
                                    resolved.path,
                                    false,
                                    &mut identities,
                                    &mut next_id,
                                    &mut queue,
                                    &mut requested_runtime,
                                )
                            })
                    } else {
                        None
                    };
                    dependencies.push(LibraryDependency {
                        specifier: specifier.clone(),
                        target: LibraryDependencyTarget::External {
                            specifier,
                            analysis_target,
                        },
                    });
                    continue;
                }

                let resolved = resolver
                    .resolve_module(&specifier, issuer)
                    .map_err(|error| {
                        format!(
                            "cannot resolve `{specifier}` from `{}`: {error:?}",
                            path.display()
                        )
                    })?;
                if runtime_request && !resolved.path.starts_with(&root) {
                    return Err(format!(
                        "relative library dependency escapes project root: {}",
                        resolved.path.display()
                    ));
                }
                let target = assign_module(
                    resolved.identity,
                    resolved.path,
                    runtime_request,
                    &mut identities,
                    &mut next_id,
                    &mut queue,
                    &mut requested_runtime,
                );
                dependencies.push(LibraryDependency {
                    specifier,
                    target: LibraryDependencyTarget::Internal(target),
                });
            }

            modules.insert(
                id,
                LibraryModule {
                    path,
                    source,
                    runtime: runtime_request,
                    dependencies,
                    parsed,
                },
            );
        }

        // A module can first be reached for analysis and later be promoted by a runtime edge. The
        // queued promotion above propagates through its already-scanned internal dependencies.
        for id in requested_runtime {
            if let Some(module) = modules.get_mut(&id) {
                module.runtime = true;
            }
        }

        Ok(Self {
            root,
            package_name: package_identity,
            class_prefix,
            entry_id: 0,
            interner,
            modules,
        })
    }

    pub fn dependency(&self, module_id: u32, specifier: &str) -> Option<&LibraryDependency> {
        self.modules
            .get(&module_id)?
            .dependencies
            .iter()
            .find(|dependency| dependency.specifier == specifier)
    }

    pub fn emit(&self, format: PreserveModuleFormat) -> Result<LibraryJavaScriptOutput, String> {
        if format == PreserveModuleFormat::CommonJs
            && self
                .modules
                .values()
                .any(|module| module.runtime && module.parsed.has_top_level_await)
        {
            return Err(
                "CommonJS library output does not support modules with top-level await".to_string(),
            );
        }
        let extension = match format {
            PreserveModuleFormat::EsModule => "mjs",
            PreserveModuleFormat::CommonJs => "cjs",
        };
        let mut output_paths = FxHashMap::default();
        for (&id, module) in &self.modules {
            if !module.runtime {
                continue;
            }
            let output = if id == self.entry_id {
                PathBuf::from(format!("index.{extension}"))
            } else {
                let relative = module.path.strip_prefix(&self.root).map_err(|_| {
                    format!(
                        "runtime module escapes project root: {}",
                        module.path.display()
                    )
                })?;
                let mut output = PathBuf::from("_wake").join(relative);
                output.set_extension(extension);
                output
            };
            output_paths.insert(id, output);
        }

        let mut ids = output_paths.keys().copied().collect::<Vec<_>>();
        ids.sort_unstable();
        let scopes = self.css_scopes();
        let mut output = Vec::with_capacity(ids.len());
        let mut css = String::new();
        for id in ids {
            let module = &self.modules[&id];
            let file_name = &output_paths[&id];
            let rewriter = GraphSpecifierRewriter {
                module,
                current_output: file_name,
                output_paths: &output_paths,
                lower_dynamic_import_to_require: format == PreserveModuleFormat::CommonJs,
            };
            let code = module
                .parsed
                .module
                .with_ast(|program| -> Result<String, String> {
                    let seed = self.css_seed(&module.path);
                    let scope = scopes.get(&id).cloned().unwrap_or_default();
                    let transformed = wake_css_in_js::transform_with_class_prefix(
                        program,
                        &self.interner,
                        &module.source,
                        &seed,
                        &scope,
                        Some(&self.class_prefix),
                    );
                    if transformed
                        .diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic.is_error())
                    {
                        let messages = transformed
                            .diagnostics
                            .iter()
                            .map(|diagnostic| diagnostic.message.as_str())
                            .collect::<Vec<_>>()
                            .join("; ");
                        return Err(format!(
                            "cannot statically compile CSS in `{}`: {messages}",
                            module.path.display()
                        ));
                    }
                    css.push_str(&transformed.css);
                    let mut optimize_input = OptimizeInput::new(&module.source);
                    optimize_input.minify = false;
                    if format == PreserveModuleFormat::CommonJs {
                        optimize_input.set_preserve_commonjs(true);
                    }
                    for (span, replacement) in &transformed.replacements {
                        let parsed_replacement =
                            parse(replacement, &self.interner, SourceType::Module);
                        optimize_input.add_expression_edit(
                            TrustedExpressionEdit::from_parsed_program(
                                *span,
                                &parsed_replacement.module,
                                &self.interner,
                            ),
                        );
                    }
                    optimize_input.extend_statement_removals(
                        transformed.removable_import_spans.iter().copied(),
                    );
                    optimize_input.extend_binding_removals(
                        transformed.removable_import_binding_spans.iter().copied(),
                    );
                    optimize_input.module_name = Some(path_to_slash(&module.path));
                    let optimized = optimize(
                        module.parsed.module.clone(),
                        &self.interner,
                        &optimize_input,
                    )
                    .map_err(|error| error.to_string())?;
                    Ok(codegen_preserved_optimized(
                        &optimized,
                        &self.interner,
                        format,
                        &rewriter,
                    ))
                })?;
            output.push(LibraryOutputModule {
                id,
                source: module.path.clone(),
                file_name: path_to_slash(file_name),
                code,
            });
        }

        Ok(LibraryJavaScriptOutput {
            entry: format!("index.{extension}"),
            css: (!css.is_empty()).then_some(css),
            runtime_module_count: output.len(),
            analysis_module_count: self.modules.len().saturating_sub(output.len()),
            modules: output,
        })
    }

    fn css_scopes(&self) -> FxHashMap<u32, wake_css_in_js::value::Scope> {
        use wake_css_in_js::value::{Scope, collect_imports, collect_static_reexports};

        let mut order = Vec::with_capacity(self.modules.len());
        let mut state = FxHashMap::default();
        let mut roots = self.modules.keys().copied().collect::<Vec<_>>();
        roots.sort_unstable();
        for root in roots {
            let mut stack = vec![(root, false)];
            while let Some((id, expanded)) = stack.pop() {
                if expanded {
                    if state.insert(id, 1u8) != Some(1) {
                        order.push(id);
                    }
                    continue;
                }
                if state.contains_key(&id) {
                    continue;
                }
                state.insert(id, 0u8);
                stack.push((id, true));
                if let Some(module) = self.modules.get(&id) {
                    for dependency in &module.dependencies {
                        if let Some(target) = dependency_analysis_target(dependency)
                            && !state.contains_key(&target)
                        {
                            stack.push((target, false));
                        }
                    }
                }
            }
        }

        let mut exports_of: FxHashMap<u32, wake_css_in_js::StaticExports> = FxHashMap::default();
        let mut scopes: FxHashMap<u32, Scope> = FxHashMap::default();
        for id in order {
            let module = &self.modules[&id];
            let imports = module
                .parsed
                .module
                .with_ast(|program| collect_imports(program, &self.interner));
            let mut scope = Scope::default();
            for (local, specifier, imported_name) in imports {
                let Some(target) = module
                    .dependencies
                    .iter()
                    .find(|dependency| dependency.specifier == specifier)
                    .and_then(dependency_analysis_target)
                else {
                    continue;
                };
                let Some(exports) = exports_of.get(&target) else {
                    continue;
                };
                if imported_name == "*" {
                    let mut namespace = exports
                        .iter()
                        .map(|(name, value)| (name.clone(), value.clone()))
                        .collect::<Vec<_>>();
                    namespace.sort_by(|left, right| left.0.cmp(&right.0));
                    scope.insert(local, wake_css_in_js::StaticValue::Obj(namespace));
                } else if let Some(value) = exports.get(&imported_name) {
                    scope.insert(local, value.clone());
                }
            }

            let seed = self.css_seed(&module.path);
            let mut exports = module.parsed.module.with_ast(|program| {
                wake_css_in_js::collect_static_exports_with_class_prefix(
                    program,
                    &self.interner,
                    &seed,
                    &scope,
                    Some(&self.class_prefix),
                )
            });
            let reexports = module
                .parsed
                .module
                .with_ast(|program| collect_static_reexports(program, &self.interner));
            for reexport in reexports {
                let Some(target) = module
                    .dependencies
                    .iter()
                    .find(|dependency| dependency.specifier == reexport.specifier)
                    .and_then(dependency_analysis_target)
                else {
                    continue;
                };
                let Some(dependency_exports) = exports_of.get(&target) else {
                    continue;
                };
                match (reexport.imported, reexport.exported) {
                    (Some(imported), Some(exported)) => {
                        if let Some(value) = dependency_exports.get(&imported) {
                            exports.insert(exported, value.clone());
                        }
                    }
                    (None, Some(exported)) => {
                        let mut namespace = dependency_exports
                            .iter()
                            .map(|(name, value)| (name.clone(), value.clone()))
                            .collect::<Vec<_>>();
                        namespace.sort_by(|left, right| left.0.cmp(&right.0));
                        exports.insert(exported, wake_css_in_js::StaticValue::Obj(namespace));
                    }
                    (None, None) => {
                        for (name, value) in dependency_exports {
                            exports.entry(name.clone()).or_insert_with(|| value.clone());
                        }
                    }
                    _ => {}
                }
            }
            exports_of.insert(id, exports);
            scopes.insert(id, scope);
        }
        scopes
    }

    fn css_seed(&self, path: &Path) -> String {
        let relative = path.strip_prefix(&self.root).unwrap_or(path);
        format!("{}/{}", self.package_name, path_to_slash(relative))
    }
}

fn dependency_analysis_target(dependency: &LibraryDependency) -> Option<u32> {
    match dependency.target {
        LibraryDependencyTarget::Internal(target) => Some(target),
        LibraryDependencyTarget::External {
            analysis_target, ..
        } => analysis_target,
    }
}

fn assign_module(
    identity: ModuleIdentity,
    path: PathBuf,
    runtime: bool,
    identities: &mut FxHashMap<ModuleIdentity, u32>,
    next_id: &mut u32,
    queue: &mut VecDeque<(u32, PathBuf, bool)>,
    requested_runtime: &mut FxHashSet<u32>,
) -> u32 {
    if let Some(&id) = identities.get(&identity) {
        if runtime && requested_runtime.insert(id) {
            queue.push_back((id, path, true));
        }
        return id;
    }
    let id = *next_id;
    *next_id += 1;
    identities.insert(identity, id);
    if runtime {
        requested_runtime.insert(id);
    }
    queue.push_back((id, path, runtime));
    id
}

struct GraphSpecifierRewriter<'a> {
    module: &'a LibraryModule,
    current_output: &'a Path,
    output_paths: &'a FxHashMap<u32, PathBuf>,
    lower_dynamic_import_to_require: bool,
}

impl ModuleSpecifierRewriter for GraphSpecifierRewriter<'_> {
    fn rewrite(&self, specifier: &str) -> Option<String> {
        let dependency = self
            .module
            .dependencies
            .iter()
            .find(|dependency| dependency.specifier == specifier)?;
        let LibraryDependencyTarget::Internal(target) = dependency.target else {
            return None;
        };
        let target = self.output_paths.get(&target)?;
        Some(relative_specifier(
            self.current_output.parent().unwrap_or(Path::new("")),
            target,
        ))
    }

    fn lower_dynamic_import_to_require(&self) -> bool {
        self.lower_dynamic_import_to_require
    }
}

fn relative_specifier(from: &Path, to: &Path) -> String {
    let from = from.components().collect::<Vec<_>>();
    let to = to.components().collect::<Vec<_>>();
    let common = from
        .iter()
        .zip(&to)
        .take_while(|(left, right)| left == right)
        .count();
    let mut relative = PathBuf::new();
    for _ in common..from.len() {
        relative.push("..");
    }
    for component in &to[common..] {
        relative.push(component.as_os_str());
    }
    let relative = path_to_slash(&relative);
    if relative.starts_with('.') {
        relative
    } else {
        format!("./{relative}")
    }
}

fn source_type(path: &Path) -> Result<SourceType, String> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "ts" | "mts" | "cts" => Ok(SourceType::TypeScript),
        "tsx" => Ok(SourceType::Tsx),
        "jsx" => Ok(SourceType::Jsx),
        "js" | "mjs" | "cjs" => Ok(SourceType::Module),
        extension => Err(format!(
            "unsupported library module extension `{extension}`: {}",
            path.display()
        )),
    }
}

fn is_bare_specifier(specifier: &str) -> bool {
    !specifier.starts_with('.')
        && !specifier.starts_with('/')
        && !Path::new(specifier).is_absolute()
        && !matches!(
            Path::new(specifier).components().next(),
            Some(Component::Prefix(_)) | Some(Component::RootDir)
        )
}

fn bare_package_name(specifier: &str) -> &str {
    if specifier.starts_with('@') {
        specifier
            .match_indices('/')
            .nth(1)
            .map(|(index, _)| &specifier[..index])
            .unwrap_or(specifier)
    } else {
        specifier
            .split_once('/')
            .map(|(package, _)| package)
            .unwrap_or(specifier)
    }
}

fn path_to_slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn read_package_name(root: &Path) -> Result<String, String> {
    let path = root.join("package.json");
    let source = std::fs::read_to_string(&path)
        .map_err(|error| format!("cannot read `{}`: {error}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&source)
        .map_err(|error| format!("cannot parse `{}`: {error}", path.display()))?;
    value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("`{}` must contain a package name", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use tempfile::tempdir;

    use super::*;

    fn write(path: &Path, source: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }

    fn node_available() -> bool {
        Command::new("node")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn write_output(root: &Path, output: &LibraryJavaScriptOutput) {
        for module in &output.modules {
            write(&root.join(&module.file_name), &module.code);
        }
    }

    #[test]
    fn external_analysis_targets_never_enter_runtime_outputs() {
        let project = tempdir().unwrap();
        write(
            &project.path().join("package.json"),
            r#"{"name":"example"}"#,
        );
        write(
            &project.path().join("src/index.ts"),
            r#"import React from "react";
import { token } from "tokens";
import { value } from "./value.js";
export { value };
export default React.createElement("span", null, token.color);"#,
        );
        write(
            &project.path().join("src/value.ts"),
            "export const value: number = 1;",
        );
        write(
            &project.path().join("node_modules/tokens/package.json"),
            r#"{"name":"tokens","version":"1.0.0","module":"index.js"}"#,
        );
        write(
            &project.path().join("node_modules/tokens/index.js"),
            "export { token } from './values.js';",
        );
        write(
            &project.path().join("node_modules/tokens/values.js"),
            "export const token = { color: 'red' };",
        );

        let mut options = LibraryGraphOptions::new(project.path(), "src/index.ts");
        options.analysis_packages.push("tokens".to_string());
        let graph = LibraryGraph::scan(options).unwrap();
        let token = graph.dependency(0, "tokens").unwrap();
        assert!(matches!(
            token.target,
            LibraryDependencyTarget::External {
                analysis_target: Some(_),
                ..
            }
        ));

        let esm = graph.emit(PreserveModuleFormat::EsModule).unwrap();
        assert_eq!(esm.runtime_module_count, 2);
        assert_eq!(esm.analysis_module_count, 2);
        assert_eq!(esm.entry, "index.mjs");
        assert!(esm.modules.iter().all(|module| {
            !module.source.ends_with("node_modules/tokens/index.js")
                && !module.source.ends_with("node_modules/tokens/values.js")
        }));
        let entry = &esm
            .modules
            .iter()
            .find(|module| module.id == 0)
            .unwrap()
            .code;
        assert!(entry.contains("from \"react\""), "{entry}");
        assert!(entry.contains("from \"tokens\""), "{entry}");
        assert!(entry.contains("from \"./_wake/src/value.mjs\""), "{entry}");
        assert!(!entry.contains("__wake_"), "{entry}");

        let cjs = graph.emit(PreserveModuleFormat::CommonJs).unwrap();
        let entry = &cjs
            .modules
            .iter()
            .find(|module| module.id == 0)
            .unwrap()
            .code;
        assert!(entry.contains("require(\"react\")"), "{entry}");
        assert!(entry.contains("require(\"tokens\")"), "{entry}");
        assert!(
            entry.contains("require(\"./_wake/src/value.cjs\")"),
            "{entry}"
        );
        assert!(!entry.contains("__wake_modules__"), "{entry}");
    }

    #[test]
    fn runtime_promotion_propagates_through_an_analysis_first_module() {
        let project = tempdir().unwrap();
        write(
            &project.path().join("package.json"),
            r#"{"name":"example"}"#,
        );
        write(
            &project.path().join("src/index.ts"),
            "import 'tokens'; export { token } from './bridge.js';",
        );
        write(
            &project.path().join("src/bridge.ts"),
            "export { token } from '../node_modules/tokens/index.js';",
        );
        write(
            &project.path().join("node_modules/tokens/package.json"),
            r#"{"name":"tokens","version":"1.0.0","module":"index.js"}"#,
        );
        write(
            &project.path().join("node_modules/tokens/index.js"),
            "export { token } from './values.js';",
        );
        write(
            &project.path().join("node_modules/tokens/values.js"),
            "export const token = 1;",
        );

        let mut options = LibraryGraphOptions::new(project.path(), "src/index.ts");
        options.analysis_packages.push("tokens".to_string());
        let graph = LibraryGraph::scan(options).unwrap();
        let esm = graph.emit(PreserveModuleFormat::EsModule).unwrap();
        assert_eq!(esm.runtime_module_count, 4);
        assert_eq!(esm.analysis_module_count, 0);
        assert!(
            esm.modules
                .iter()
                .any(|module| module.file_name == "_wake/node_modules/tokens/values.mjs")
        );
    }

    #[test]
    fn css_uses_analysis_only_exports_and_is_removed_from_javascript() {
        let project = tempdir().unwrap();
        write(
            &project.path().join("package.json"),
            r#"{"name":"@scope/button"}"#,
        );
        write(
            &project.path().join("src/index.ts"),
            r#"import { css } from "@crab-dev/css";
import { color } from "tokens";
export const button = css`color: ${color};`;
"#,
        );
        write(
            &project.path().join("node_modules/tokens/package.json"),
            r#"{"name":"tokens","version":"1.0.0","module":"index.js"}"#,
        );
        write(
            &project.path().join("node_modules/tokens/index.js"),
            "export const color = 'red';",
        );

        let mut options = LibraryGraphOptions::new(project.path(), "src/index.ts");
        options.analysis_packages.push("tokens".to_string());
        let graph = LibraryGraph::scan(options).unwrap();
        let esm = graph.emit(PreserveModuleFormat::EsModule).unwrap();
        assert_eq!(esm.runtime_module_count, 1);
        assert_eq!(esm.analysis_module_count, 1);
        let entry = &esm.modules[0].code;
        assert!(!entry.contains("@crab-dev/css"), "{entry}");
        assert!(entry.contains("button-"), "{entry}");
        let css = esm.css.unwrap();
        assert!(css.contains("color: red"), "{css}");
        assert!(css.contains(".button-"), "{css}");
    }

    #[test]
    fn library_css_names_are_root_independent_and_package_prefixed() {
        fn compile(root: &Path, package: &str) -> (String, String) {
            write(
                &root.join("package.json"),
                &format!(r#"{{"name":"{package}"}}"#),
            );
            write(
                &root.join("src/index.ts"),
                "import { css } from '@crab-dev/css'; export const root = css`color: red;`;",
            );
            let graph = LibraryGraph::scan(LibraryGraphOptions::new(root, "src/index.ts")).unwrap();
            let output = graph.emit(PreserveModuleFormat::EsModule).unwrap();
            (output.modules[0].code.clone(), output.css.unwrap())
        }

        let first = tempdir().unwrap();
        let second = tempdir().unwrap();
        let third = tempdir().unwrap();
        let button_a = compile(first.path(), "@crab-dev/rc-button");
        let button_b = compile(second.path(), "@crab-dev/rc-button");
        let card = compile(third.path(), "@crab-dev/rc-card");
        assert_eq!(
            button_a, button_b,
            "absolute checkout roots must not affect output"
        );
        assert!(button_a.0.contains("rc-button-"), "{}", button_a.0);
        assert!(button_a.1.contains(".rc-button-"), "{}", button_a.1);
        assert!(card.0.contains("rc-card-"), "{}", card.0);
        assert_ne!(
            button_a, card,
            "different packages must not share class names"
        );
    }

    #[test]
    fn native_esm_preserves_live_bindings_cycles_dynamic_import_and_tla() {
        if !node_available() {
            return;
        }
        let project = tempdir().unwrap();
        write(
            &project.path().join("package.json"),
            r#"{"name":"example","type":"module"}"#,
        );
        write(
            &project.path().join("src/index.ts"),
            r#"export { value, increment } from './state.js';
export { pair } from './a.js';
export async function lazy() { return (await import('./lazy.js')).answer; }"#,
        );
        write(
            &project.path().join("src/state.ts"),
            "export let value = 0; export function increment() { value += 1; }",
        );
        write(
            &project.path().join("src/a.ts"),
            "import { b } from './b.js'; export const a = 'a'; export const pair = () => a + b;",
        );
        write(
            &project.path().join("src/b.ts"),
            "import { a } from './a.js'; export const b = 'b'; export const read = () => a;",
        );
        write(
            &project.path().join("src/lazy.ts"),
            "export const answer = await Promise.resolve(42);",
        );

        let graph =
            LibraryGraph::scan(LibraryGraphOptions::new(project.path(), "src/index.ts")).unwrap();
        let esm = graph.emit(PreserveModuleFormat::EsModule).unwrap();
        let output = project.path().join("out/esm");
        write_output(&output, &esm);
        let entry_url = format!(
            "file:///{}",
            path_to_slash(&output.join("index.mjs")).replace(' ', "%20")
        );
        let script = format!(
            "const m=await import({entry_url:?});if(m.value!==0)throw Error('initial');m.increment();if(m.value!==1)throw Error('live');if(m.pair()!=='ab')throw Error('cycle');if(await m.lazy()!==42)throw Error('tla');process.stdout.write('OK')"
        );
        let result = Command::new("node")
            .args(["--input-type=module", "-e", &script])
            .output()
            .unwrap();
        assert!(
            result.status.success() && result.stdout == b"OK",
            "status={:?} stderr={}",
            result.status.code(),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            graph
                .emit(PreserveModuleFormat::CommonJs)
                .unwrap_err()
                .contains("top-level await")
        );
    }

    #[test]
    fn direct_commonjs_modules_preserve_named_reexports_and_live_reads() {
        if !node_available() {
            return;
        }
        let project = tempdir().unwrap();
        write(
            &project.path().join("package.json"),
            r#"{"name":"example"}"#,
        );
        write(
            &project.path().join("src/index.ts"),
            "import answer from './default.js'; import { value as observed } from './state.js'; export { value, increment } from './state.js'; export { default } from './default.js'; export { pair } from './a.js'; export { read as readA } from './b.js'; export function read() { return observed; } export function readDefault() { return answer(); } export async function load() { return (await import('./lazy.js')).answer; }",
        );
        write(
            &project.path().join("src/state.ts"),
            "export let value = 0; export function increment() { value += 1; }",
        );
        write(
            &project.path().join("src/default.tsx"),
            "import value from './constant.js'; export default function answer() { return value; }",
        );
        write(
            &project.path().join("src/constant.ts"),
            "export default 42;",
        );
        write(
            &project.path().join("src/a.ts"),
            "import { b } from './b.js'; export const a = 'a'; export const pair = () => a + b;",
        );
        write(
            &project.path().join("src/b.ts"),
            "import { a } from './a.js'; export const b = 'b'; export const read = () => a;",
        );
        write(
            &project.path().join("src/lazy.ts"),
            "export const answer = 42;",
        );

        let graph =
            LibraryGraph::scan(LibraryGraphOptions::new(project.path(), "src/index.ts")).unwrap();
        let cjs = graph.emit(PreserveModuleFormat::CommonJs).unwrap();
        let output = project.path().join("out/cjs");
        write_output(&output, &cjs);
        let entry = output
            .join("index.cjs")
            .to_string_lossy()
            .replace('\\', "\\\\");
        let script = format!(
            "const m=require(\"{entry}\");if(m.value!==0||m.read()!==0)throw Error('initial');m.increment();if(m.value!==1||m.read()!==1)throw Error('live');if(m.default()!==42||m.readDefault()!==42)throw Error('default');if(m.pair()!=='ab'||m.readA()!=='a')throw Error('cycle');m.load().then(v=>{{if(v!==42)throw Error('dynamic');process.stdout.write('OK')}}).catch(e=>{{console.error(e);process.exit(2)}})"
        );
        let result = Command::new("node").args(["-e", &script]).output().unwrap();
        assert!(
            result.status.success() && result.stdout == b"OK",
            "status={:?} stderr={}",
            result.status.code(),
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[test]
    fn direct_commonjs_jsx_runtime_replacement_targets_only_the_synthetic_callee() {
        let project = tempdir().unwrap();
        write(
            &project.path().join("package.json"),
            r#"{"name":"example"}"#,
        );
        write(
            &project.path().join("src/index.tsx"),
            "const _wi0_0 = 'answer'; export default <button>{_wi0_0}</button>;",
        );

        let graph =
            LibraryGraph::scan(LibraryGraphOptions::new(project.path(), "src/index.tsx")).unwrap();
        let cjs = graph.emit(PreserveModuleFormat::CommonJs).unwrap();
        let entry = &cjs.modules[0].code;
        assert!(
            entry.contains("require(\"react/jsx-runtime\")")
                && entry.contains("[\"jsx\"])(\"button\"")
                && entry.contains("const _wi0_0 = \"answer\""),
            "the semantic `_jsx` import binding must replace only the call callee:\n{entry}"
        );
        assert!(
            !entry.contains("[\"Fragment\"])(\"button\"")
                && !entry.contains("exports.default = __wake_namespace_0[\"jsx\"];")
                && !entry.contains("exports.default=_wi0_0_1[\"jsx\"];"),
            "the enclosing JSX call must not be replaced by a shared-span edit:\n{entry}"
        );
        let reparsed = parse(entry, &Interner::new(), SourceType::Script);
        assert!(
            !reparsed.has_errors(),
            "{:?}\n{entry}",
            reparsed.diagnostics
        );
    }

    #[test]
    fn direct_commonjs_import_reads_preserve_call_receiver_and_object_shorthand() {
        if !node_available() {
            return;
        }
        let project = tempdir().unwrap();
        write(
            &project.path().join("package.json"),
            r#"{"name":"example"}"#,
        );
        write(
            &project.path().join("src/index.ts"),
            "import { check } from './dep.js'; import * as namespace from './plain.js'; export const direct = check(); export const holder = { check }; export const stable = namespace === namespace; export { namespace };",
        );
        write(
            &project.path().join("src/dep.ts"),
            "export const marker = 'namespace'; export function check() { return this && this.marker || 'value'; }",
        );
        write(
            &project.path().join("src/plain.js"),
            "module.exports = { value: 1 };",
        );

        let graph =
            LibraryGraph::scan(LibraryGraphOptions::new(project.path(), "src/index.ts")).unwrap();
        let cjs = graph.emit(PreserveModuleFormat::CommonJs).unwrap();
        let output = project.path().join("out/cjs");
        write_output(&output, &cjs);
        let entry_code = &cjs
            .modules
            .iter()
            .find(|module| module.id == 0)
            .expect("entry module")
            .code;
        assert!(
            entry_code.contains("const holder = { check: __wake_namespace_0[\"check\"] }"),
            "object shorthand must become a value read without retaining a receiver:\n{entry_code}"
        );

        let entry = output
            .join("index.cjs")
            .to_string_lossy()
            .replace('\\', "\\\\");
        let script = format!(
            "const m=require(\"{entry}\");if(m.direct!=='value'||m.holder.check()!=='value')throw Error('receiver');if(!m.stable||m.namespace!==m.namespace)throw Error('identity');process.stdout.write('OK')"
        );
        let result = Command::new("node").args(["-e", &script]).output().unwrap();
        assert!(
            result.status.success() && result.stdout == b"OK",
            "status={:?} stderr={} entry={entry_code}",
            result.status.code(),
            String::from_utf8_lossy(&result.stderr)
        );
    }

    #[test]
    fn direct_commonjs_import_write_targets_are_never_replaced() {
        let project = tempdir().unwrap();
        write(
            &project.path().join("package.json"),
            r#"{"name":"example"}"#,
        );
        write(
            &project.path().join("src/index.ts"),
            "import { value } from './dep.js'; try { value = 1; value++; for (value in { x: 1 }) break; for (value of [1]) break; } catch {}",
        );
        write(&project.path().join("src/dep.ts"), "export let value = 0;");

        let graph =
            LibraryGraph::scan(LibraryGraphOptions::new(project.path(), "src/index.ts")).unwrap();
        let cjs = graph.emit(PreserveModuleFormat::CommonJs).unwrap();
        let entry = &cjs.modules[0].code;
        assert!(!entry.contains("0,_wi"), "{entry}");
        assert!(entry.contains("value = 1"), "{entry}");
        assert!(entry.contains("value++"), "{entry}");
        assert!(entry.contains("for (value in"), "{entry}");
        assert!(entry.contains("for (value of"), "{entry}");
        let reparsed = parse(entry, &Interner::new(), SourceType::Script);
        assert!(
            !reparsed.has_errors(),
            "{:?}\n{entry}",
            reparsed.diagnostics
        );
    }
}
