//! Native, dependency-light TypeScript props documentation for Wake docs.
//!
//! This is deliberately separate from the normal bundler AST. It resolves the shapes common in
//! React component props and keeps unsupported package types as inherited sources instead of
//! recursively loading all of `node_modules`.

use regex::Regex;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use wake_ecma_parser::parse_declaration_facts;

pub use wake_ecma_parser::{
    DeclarationFactError, DeclarationFacts, DeclarationImportUsage, DeclarationItemFact,
    DeclarationItemKind, DeclarationRequestFact, DeclarationRequestRole, SourceType,
    validate_declaration_module, validate_declaration_module_allow_any,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiDoc {
    pub symbol: String,
    pub source: String,
    pub description: String,
    pub props: Vec<ApiProp>,
    pub inherited: Vec<InheritedGroup>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApiProp {
    pub name: String,
    pub type_text: String,
    pub required: bool,
    pub description: String,
    pub default_value: Option<String>,
    pub deprecated: Option<String>,
    pub since: Option<String>,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InheritedGroup {
    pub name: String,
    pub source: String,
    pub type_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComponentApiDoc {
    pub display_name: String,
    pub description: String,
    pub api: ApiDoc,
}

/// Component API extraction plus the complete set of source files whose contents contributed to
/// it. Product layers use this provenance to keep generated exact outputs disjoint from inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentApiExtraction {
    pub document: ComponentApiDoc,
    pub inputs: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationFile {
    pub source: PathBuf,
    pub file_name: PathBuf,
    pub code: String,
}

/// One explicitly-owned declaration entry in a multi-entry library build.
///
/// `owner` is supplied by the product layer (for example a federation expose key). It is never
/// inferred from a file name, so two entries with the same basename remain distinct.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationEntry {
    pub owner: String,
    pub source: PathBuf,
}

impl DeclarationEntry {
    pub fn new(owner: impl Into<String>, source: impl Into<PathBuf>) -> Self {
        Self {
            owner: owner.into(),
            source: source.into(),
        }
    }
}

/// The declaration files owned by one entry.
///
/// Every bundle has its own `index.d.ts`; the owner is the namespace that disambiguates that
/// filename from another entry's `index.d.ts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclarationBundle {
    pub owner: String,
    pub source: PathBuf,
    pub files: Vec<DeclarationFile>,
}

/// Immutable context passed to declaration request rewriters during pure rendering.
#[derive(Debug, Clone, Copy)]
pub struct DeclarationRenderRequest<'a> {
    pub owner: &'a str,
    pub module_source: &'a Path,
    pub output_file: &'a Path,
    pub specifier: &'a str,
    pub role: DeclarationRequestRole,
    pub resolved_source: Option<&'a Path>,
    pub resolved_output: Option<&'a Path>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclarationTemplateContext {
    Standalone,
    Ambient,
}

#[derive(Debug, Clone)]
struct FrozenDeclarationModule {
    facts: DeclarationFacts,
    requests: Vec<Vec<FrozenDeclarationRequest>>,
    included_items: Vec<bool>,
}

#[derive(Debug, Clone)]
struct FrozenDeclarationRequest {
    resolved_source: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct FrozenDeclarationEntry {
    entry: DeclarationEntry,
    reachable: BTreeSet<PathBuf>,
}

/// A declaration graph whose source reads, parses, request resolution and entry reachability are
/// complete. Rendering this value performs no filesystem access and can be repeated with different
/// request bindings.
#[derive(Debug, Clone)]
pub struct FrozenDeclarationGraph {
    root: PathBuf,
    entries: Vec<FrozenDeclarationEntry>,
    modules: BTreeMap<PathBuf, FrozenDeclarationModule>,
}

impl FrozenDeclarationGraph {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn entries(&self) -> impl ExactSizeIterator<Item = &DeclarationEntry> {
        self.entries.iter().map(|entry| &entry.entry)
    }

    /// Canonical source inputs, in deterministic path order.
    pub fn inputs(&self) -> impl ExactSizeIterator<Item = &Path> {
        self.modules.keys().map(PathBuf::as_path)
    }

    /// Look up facts by one of the canonical paths returned by [`Self::inputs`]. This method is an
    /// exact frozen-map lookup and deliberately performs no path canonicalization or filesystem I/O.
    pub fn module_facts(&self, source: impl AsRef<Path>) -> Option<&DeclarationFacts> {
        self.modules
            .get(source.as_ref())
            .map(|module| &module.facts)
    }

    /// Render every owner with the default relative local request mapping.
    pub fn render(&self) -> Vec<DeclarationBundle> {
        self.render_in_context(DeclarationTemplateContext::Standalone, |_| None)
    }

    /// Render one owner with the default relative local request mapping.
    pub fn render_entry(&self, owner: &str) -> Option<DeclarationBundle> {
        self.render_entry_in_context(owner, DeclarationTemplateContext::Standalone, |_| None)
    }

    /// Render parser-proven bodies for nesting inside `declare module { ... }` blocks.
    pub fn render_ambient(&self) -> Vec<DeclarationBundle> {
        self.render_in_context(DeclarationTemplateContext::Ambient, |_| None)
    }

    /// Render one parser-proven body for nesting inside a `declare module { ... }` block.
    pub fn render_entry_ambient(&self, owner: &str) -> Option<DeclarationBundle> {
        self.render_entry_in_context(owner, DeclarationTemplateContext::Ambient, |_| None)
    }

    /// Purely render every owner. Returning `Some(specifier)` overrides the default binding for a
    /// request; returning `None` preserves external requests and maps local requests to the
    /// corresponding declaration output.
    pub fn render_with<F>(&self, mut rewrite: F) -> Vec<DeclarationBundle>
    where
        F: FnMut(&DeclarationRenderRequest<'_>) -> Option<String>,
    {
        self.render_in_context(DeclarationTemplateContext::Standalone, &mut rewrite)
    }

    /// Purely render ambient-module bodies with an optional request override.
    pub fn render_ambient_with<F>(&self, mut rewrite: F) -> Vec<DeclarationBundle>
    where
        F: FnMut(&DeclarationRenderRequest<'_>) -> Option<String>,
    {
        self.render_in_context(DeclarationTemplateContext::Ambient, &mut rewrite)
    }

    fn render_in_context<F>(
        &self,
        context: DeclarationTemplateContext,
        mut rewrite: F,
    ) -> Vec<DeclarationBundle>
    where
        F: FnMut(&DeclarationRenderRequest<'_>) -> Option<String>,
    {
        self.entries
            .iter()
            .map(|entry| self.render_frozen_entry(entry, context, &mut rewrite))
            .collect()
    }

    /// Purely render one owner with an optional request override.
    pub fn render_entry_with<F>(&self, owner: &str, mut rewrite: F) -> Option<DeclarationBundle>
    where
        F: FnMut(&DeclarationRenderRequest<'_>) -> Option<String>,
    {
        self.render_entry_in_context(owner, DeclarationTemplateContext::Standalone, &mut rewrite)
    }

    /// Purely render one ambient-module body with an optional request override.
    pub fn render_entry_ambient_with<F>(
        &self,
        owner: &str,
        mut rewrite: F,
    ) -> Option<DeclarationBundle>
    where
        F: FnMut(&DeclarationRenderRequest<'_>) -> Option<String>,
    {
        self.render_entry_in_context(owner, DeclarationTemplateContext::Ambient, &mut rewrite)
    }

    fn render_entry_in_context<F>(
        &self,
        owner: &str,
        context: DeclarationTemplateContext,
        mut rewrite: F,
    ) -> Option<DeclarationBundle>
    where
        F: FnMut(&DeclarationRenderRequest<'_>) -> Option<String>,
    {
        self.entries
            .iter()
            .find(|entry| entry.entry.owner == owner)
            .map(|entry| self.render_frozen_entry(entry, context, &mut rewrite))
    }

    fn render_frozen_entry<F>(
        &self,
        entry: &FrozenDeclarationEntry,
        context: DeclarationTemplateContext,
        rewrite: &mut F,
    ) -> DeclarationBundle
    where
        F: FnMut(&DeclarationRenderRequest<'_>) -> Option<String>,
    {
        let output_paths = declaration_output_paths(
            &self.root,
            &entry.entry.source,
            entry.reachable.iter().map(PathBuf::as_path),
        );

        let mut files = entry
            .reachable
            .iter()
            .map(|source| {
                let module = self
                    .modules
                    .get(source)
                    .expect("reachable declaration module is frozen");
                let output_file = &output_paths[source];
                let mut code = String::new();

                for (item_index, item) in module.facts.items().iter().enumerate() {
                    if !module.included_items[item_index] {
                        continue;
                    }
                    let frozen_requests = &module.requests[item_index];
                    debug_assert!(frozen_requests.iter().all(|request| {
                        request
                            .resolved_source
                            .as_ref()
                            .is_none_or(|target| entry.reachable.contains(target))
                    }));

                    let (template, requests) = match context {
                        DeclarationTemplateContext::Standalone => {
                            (item.template(), item.requests())
                        }
                        DeclarationTemplateContext::Ambient => {
                            (item.ambient_template(), item.ambient_requests())
                        }
                    };
                    let mut cursor = 0;
                    for (request, frozen) in requests.iter().zip(frozen_requests) {
                        let range = request.template_range();
                        code.push_str(&template[cursor..range.start]);
                        let resolved_output = frozen
                            .resolved_source
                            .as_ref()
                            .and_then(|target| output_paths.get(target));
                        let context = DeclarationRenderRequest {
                            owner: &entry.entry.owner,
                            module_source: source,
                            output_file,
                            specifier: request.specifier(),
                            role: request.role(),
                            resolved_source: frozen.resolved_source.as_deref(),
                            resolved_output: resolved_output.map(PathBuf::as_path),
                        };
                        let override_specifier = rewrite(&context);
                        if let Some(specifier) = override_specifier {
                            code.push_str(&quote_like(&template[range.clone()], &specifier));
                        } else if let Some(target_output) = resolved_output {
                            let specifier =
                                relative_declaration_specifier(output_file, target_output);
                            code.push_str(&quote_like(&template[range.clone()], &specifier));
                        } else {
                            code.push_str(&template[range.clone()]);
                        }
                        cursor = range.end;
                    }
                    code.push_str(&template[cursor..]);
                    if !template.ends_with('\n') {
                        code.push('\n');
                    }
                }

                if code.is_empty() {
                    code.push_str("export {};\n");
                }
                DeclarationFile {
                    source: source.clone(),
                    file_name: output_file.clone(),
                    code,
                }
            })
            .collect::<Vec<_>>();
        files.sort_by(|left, right| left.file_name.cmp(&right.file_name));
        DeclarationBundle {
            owner: entry.entry.owner.clone(),
            source: entry.entry.source.clone(),
            files,
        }
    }
}

/// Read and parse all declaration inputs once, sharing modules between every explicit owner.
pub fn prepare_library_declarations(
    project_root: impl AsRef<Path>,
    entries: impl IntoIterator<Item = DeclarationEntry>,
) -> Result<FrozenDeclarationGraph, ApiError> {
    prepare_library_declarations_with_file_system(
        project_root.as_ref(),
        entries,
        &OsDeclarationFileSystem,
    )
}

/// Minimal filesystem snapshot consumed while freezing a declaration graph.
///
/// Product layers should adapt the same immutable/recording filesystem used by their generation;
/// mixing this interface with process-global filesystem reads would break snapshot consistency.
pub trait DeclarationFileSystem: Send + Sync {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, String>;
    fn is_file(&self, path: &Path) -> bool;
    fn read_to_string(&self, path: &Path) -> Result<String, String>;
}

/// Process filesystem adapter used by the compatibility API.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsDeclarationFileSystem;

impl DeclarationFileSystem for OsDeclarationFileSystem {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, String> {
        fs::canonicalize(path).map_err(|error| error.to_string())
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn read_to_string(&self, path: &Path) -> Result<String, String> {
        fs::read_to_string(path).map_err(|error| error.to_string())
    }
}

/// Freeze declarations against an explicitly supplied filesystem snapshot.
pub fn prepare_library_declarations_with_file_system(
    project_root: &Path,
    entries: impl IntoIterator<Item = DeclarationEntry>,
    file_system: &(impl DeclarationFileSystem + ?Sized),
) -> Result<FrozenDeclarationGraph, ApiError> {
    let root = declaration_canonical(file_system, project_root)?;
    let mut owners = BTreeSet::new();
    let mut frozen_entries = Vec::new();
    for mut entry in entries {
        if entry.owner.is_empty() {
            return Err(ApiError::InvalidSource(
                entry.source,
                "declaration entry owner cannot be empty".to_string(),
            ));
        }
        if !owners.insert(entry.owner.clone()) {
            return Err(ApiError::InvalidSource(
                entry.source,
                format!("duplicate declaration entry owner `{}`", entry.owner),
            ));
        }
        entry.source = if entry.source.is_absolute() {
            declaration_canonical(file_system, &entry.source)?
        } else {
            declaration_canonical(file_system, &root.join(&entry.source))?
        };
        if !entry.source.starts_with(&root) {
            return Err(ApiError::InvalidSource(
                entry.source,
                "library declaration entry escapes the project root".to_string(),
            ));
        }
        frozen_entries.push(FrozenDeclarationEntry {
            entry,
            reachable: BTreeSet::new(),
        });
    }
    if frozen_entries.is_empty() {
        return Err(ApiError::InvalidSource(
            root,
            "at least one declaration entry is required".to_string(),
        ));
    }

    let mut modules = BTreeMap::<PathBuf, FrozenDeclarationModule>::new();
    for frozen_entry in &mut frozen_entries {
        let entry_source = frozen_entry.entry.source.clone();
        let mut pending = vec![entry_source.clone()];
        while let Some(path) = pending.pop() {
            if !frozen_entry.reachable.insert(path.clone()) {
                continue;
            }
            if !modules.contains_key(&path) {
                let source = file_system
                    .read_to_string(&path)
                    .map_err(|error| ApiError::Io(path.clone(), error))?;
                let source_type = declaration_source_type(&path)?;
                let facts = parse_declaration_facts(&source, source_type)
                    .map_err(|error| ApiError::InvalidSource(path.clone(), error.to_string()))?;
                if facts.contains_forbidden_any() {
                    return Err(ApiError::InvalidSource(
                        path.clone(),
                        "public declarations cannot contain the `any` type".to_string(),
                    ));
                }
                let mut requests = Vec::with_capacity(facts.items().len());
                let mut included_items = Vec::with_capacity(facts.items().len());
                for item in facts.items() {
                    validate_declaration_template(
                        path.as_path(),
                        item.template(),
                        item.requests(),
                    )?;
                    validate_declaration_template(
                        path.as_path(),
                        item.ambient_template(),
                        item.ambient_requests(),
                    )?;
                    if item.requests().len() != item.ambient_requests().len()
                        || item.requests().iter().zip(item.ambient_requests()).any(
                            |(standalone, ambient)| {
                                standalone.specifier() != ambient.specifier()
                                    || standalone.role() != ambient.role()
                                    || standalone.source_span() != ambient.source_span()
                            },
                        )
                    {
                        return Err(ApiError::InvalidSource(
                            path.clone(),
                            "standalone and ambient declaration requests do not match".to_string(),
                        ));
                    }
                    let runtime_side_effect =
                        item.import_usage() == Some(DeclarationImportUsage::RuntimeSideEffect);
                    if runtime_side_effect && !is_declaration_file_path(&path) {
                        requests.push(Vec::new());
                        included_items.push(false);
                        continue;
                    }
                    let mut item_requests = Vec::with_capacity(item.requests().len());
                    let mut include_item = true;
                    for request in item.requests() {
                        let resolved_source = if request.specifier().starts_with('.') {
                            let target = if runtime_side_effect {
                                match resolve_optional_local_declaration_import(
                                    file_system,
                                    &path,
                                    request.specifier(),
                                )? {
                                    Some(target) => target,
                                    None => {
                                        include_item = false;
                                        break;
                                    }
                                }
                            } else {
                                resolve_required_local_declaration_import(
                                    file_system,
                                    &path,
                                    request.specifier(),
                                )?
                            };
                            if !target.starts_with(&root) {
                                return Err(ApiError::InvalidSource(
                                    path.clone(),
                                    format!(
                                        "local declaration dependency `{}` escapes the project root",
                                        request.specifier()
                                    ),
                                ));
                            }
                            Some(target)
                        } else {
                            None
                        };
                        item_requests.push(FrozenDeclarationRequest { resolved_source });
                    }
                    if !include_item {
                        item_requests.clear();
                    }
                    requests.push(item_requests);
                    included_items.push(include_item);
                }
                modules.insert(
                    path.clone(),
                    FrozenDeclarationModule {
                        facts,
                        requests,
                        included_items,
                    },
                );
            }

            let module = modules
                .get(&path)
                .expect("declaration module was inserted before traversal");
            for (item_index, (item, frozen_requests)) in module
                .facts
                .items()
                .iter()
                .zip(&module.requests)
                .enumerate()
            {
                if !module.included_items[item_index] {
                    continue;
                }
                for (_request, frozen) in item.requests().iter().zip(frozen_requests) {
                    if let Some(target) = &frozen.resolved_source {
                        pending.push(target.clone());
                    }
                }
            }
        }
    }

    Ok(FrozenDeclarationGraph {
        root,
        entries: frozen_entries,
        modules,
    })
}

/// Validate a generated declaration body with the same parser-owned facts used by the graph.
pub fn validate_declaration_body(
    source_path: impl AsRef<Path>,
    body: &str,
) -> Result<DeclarationFacts, ApiError> {
    let source_path = source_path.as_ref();
    let source_type = declaration_source_type(source_path)?;
    validate_declaration_module_allow_any(body, source_type)
        .map_err(|error| ApiError::InvalidSource(source_path.to_path_buf(), error.to_string()))
}

/// Validate a body that will be nested inside an already-ambient `declare module` block.
///
/// TypeScript rejects redundant `declare` modifiers in that context. The decision is based on the
/// parser-owned modifier fact rather than scanning declaration text.
pub fn validate_ambient_declaration_body(
    source_path: impl AsRef<Path>,
    body: &str,
) -> Result<DeclarationFacts, ApiError> {
    let source_path = source_path.as_ref();
    let facts = validate_declaration_body(source_path, body)?;
    if let Some(item) = facts
        .items()
        .iter()
        .find(|item| item.has_declare_modifier())
    {
        return Err(ApiError::InvalidSource(
            source_path.to_path_buf(),
            format!(
                "redundant `declare` modifier in ambient body at byte {}..{}",
                item.source_span().lo,
                item.source_span().hi
            ),
        ));
    }
    Ok(facts)
}

/// Emit a dependency-free declaration module set for a library entry.
///
/// Type syntax is copied from the source rather than reconstructed from Wake's runtime AST. Local
/// module specifiers are redirected into the declaration output tree. Public values must carry an
/// explicit annotation; Wake deliberately fails instead of inventing `any`.
pub fn emit_library_declarations(
    project_root: impl AsRef<Path>,
    entry: impl AsRef<Path>,
) -> Result<Vec<DeclarationFile>, ApiError> {
    let graph = prepare_library_declarations(
        project_root,
        [DeclarationEntry::new("entry", entry.as_ref())],
    )?;
    Ok(graph
        .render_entry("entry")
        .expect("compatibility entry is present")
        .files)
}

#[derive(Debug)]
pub enum ApiError {
    Io(PathBuf, String),
    SymbolNotFound { path: PathBuf, symbol: String },
    CircularType(Vec<String>),
    InvalidSource(PathBuf, String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "cannot read `{}`: {error}", path.display()),
            Self::SymbolNotFound { path, symbol } => {
                write!(f, "cannot find type `{symbol}` in `{}`", path.display())
            }
            Self::CircularType(chain) => write!(f, "local type cycle: {}", chain.join(" -> ")),
            Self::InvalidSource(path, error) => {
                write!(
                    f,
                    "invalid type declaration in `{}`: {error}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for ApiError {}

/// Extract an interface or object type alias. Local imports/re-exports are followed. External
/// types are represented by [`InheritedGroup`] values and produce a non-fatal warning.
pub fn extract_api(
    source: impl AsRef<Path>,
    symbol: &str,
    component: Option<&str>,
) -> Result<ApiDoc, ApiError> {
    let source = canonical(source.as_ref())?;
    let mut resolver = Resolver::default();
    let mut resolved = resolver.resolve(&source, symbol, &mut Vec::new())?;
    if let Some(component) = component {
        let defaults = infer_defaults(&read(&source)?, component);
        for prop in &mut resolved.props {
            if prop.default_value.is_none() {
                prop.default_value = defaults.get(&prop.name).cloned();
            }
        }
    }
    resolved.props.sort_by(|a, b| {
        b.required
            .cmp(&a.required)
            .then_with(|| a.name.cmp(&b.name))
    });
    deduplicate(&mut resolved);
    Ok(ApiDoc {
        symbol: symbol.to_string(),
        source: source.to_string_lossy().into_owned(),
        description: resolved.description,
        props: resolved.props,
        inherited: resolved.inherited,
        warnings: resolved.warnings,
    })
}
/// Extract the props shape from the first typed parameter of a demo's default export.
///
/// Wake demos remain valid when they have no parameter or no type annotation; in that case this
/// returns Ok(None). Named local types, relative type imports/re-exports, inline object types and
/// the utility types supported by extract_api are resolved. Types that cannot be flattened are
/// preserved as inherited sources and reported through ApiDoc::warnings.
pub fn extract_demo_props(source: impl AsRef<Path>) -> Result<Option<ApiDoc>, ApiError> {
    let source_path = canonical(source.as_ref())?;
    let source_text = read(&source_path)?;
    let Some(parameter) = default_export_parameter(&source_text) else {
        return Ok(None);
    };
    let Some(colon) = find_top_level(parameter, ':') else {
        return Ok(None);
    };
    let pattern = parameter[..colon].trim();
    let expression = strip_parameter_initializer(parameter[colon + 1..].trim());
    if expression.is_empty() {
        return Ok(None);
    }

    let imports = parse_imports(&source_text);
    let mut resolver = Resolver::default();
    let mut resolved = Resolved::default();
    resolver.merge_expression(
        &source_path,
        &imports,
        expression,
        &mut Vec::new(),
        &mut resolved,
    )?;
    let defaults = infer_parameter_defaults(pattern);
    for prop in &mut resolved.props {
        if prop.default_value.is_none() {
            prop.default_value = defaults.get(&prop.name).cloned();
        }
    }
    resolved.props.sort_by(|a, b| {
        b.required
            .cmp(&a.required)
            .then_with(|| a.name.cmp(&b.name))
    });
    deduplicate(&mut resolved);
    Ok(Some(ApiDoc {
        symbol: expression.to_string(),
        source: source_path.to_string_lossy().into_owned(),
        description: resolved.description,
        props: resolved.props,
        inherited: resolved.inherited,
        warnings: resolved.warnings,
    }))
}

/// Extract the public props shape of a default-exported React component.
///
/// This follows the explicit, fail-closed forms used by Crab components: `FC<Props>`, a typed
/// function/arrow parameter, or `forwardRef<Ref, Props>`. Missing public annotations are errors;
/// this function never invents `any`.
pub fn extract_component_api(source: impl AsRef<Path>) -> Result<ComponentApiDoc, ApiError> {
    extract_component_api_with_provenance(source).map(|extraction| extraction.document)
}

/// Extract a component API and retain authoritative successful-read provenance.
pub fn extract_component_api_with_provenance(
    source: impl AsRef<Path>,
) -> Result<ComponentApiExtraction, ApiError> {
    let source_path = canonical(source.as_ref())?;
    let mut resolver = Resolver::default();
    let source_text = resolver.read_source(&source_path)?;
    let (display_name, declaration_offset) =
        default_component_name(&source_text).ok_or_else(|| {
            ApiError::InvalidSource(
                source_path.clone(),
                "cannot identify the default-exported component".to_string(),
            )
        })?;
    let expression = component_props_expression(&source_text, &display_name).ok_or_else(|| {
        ApiError::InvalidSource(
            source_path.clone(),
            format!("component `{display_name}` needs an explicit public props annotation"),
        )
    })?;

    let imports = parse_imports(&source_text);
    let mut resolved = Resolved::default();
    resolver.merge_expression(
        &source_path,
        &imports,
        &expression,
        &mut Vec::new(),
        &mut resolved,
    )?;
    let defaults = infer_defaults(&source_text, &display_name);
    for prop in &mut resolved.props {
        if prop.default_value.is_none() {
            prop.default_value = defaults.get(&prop.name).cloned();
        }
    }
    resolved.props.sort_by(|left, right| {
        right
            .required
            .cmp(&left.required)
            .then_with(|| left.name.cmp(&right.name))
    });
    deduplicate(&mut resolved);
    let description = preceding_jsdoc(&source_text, declaration_offset).description;
    let document = ComponentApiDoc {
        display_name,
        description: if description.is_empty() {
            resolved.description.clone()
        } else {
            description
        },
        api: ApiDoc {
            symbol: expression,
            source: source_path.to_string_lossy().into_owned(),
            description: resolved.description,
            props: resolved.props,
            inherited: resolved.inherited,
            warnings: resolved.warnings,
        },
    };
    Ok(ComponentApiExtraction {
        document,
        inputs: resolver.inputs.into_iter().collect(),
    })
}

fn default_component_name(source: &str) -> Option<(String, usize)> {
    let function = Regex::new(
        r"(?m)^\s*export\s+default\s+(?:async\s+)?function\s+([A-Za-z_$][A-Za-z0-9_$]*)",
    )
    .expect("valid default function regex");
    if let Some(captures) = function.captures(source) {
        let matched = captures.get(0)?;
        return Some((captures.get(1)?.as_str().to_string(), matched.start()));
    }
    let named = Regex::new(r"(?m)^\s*export\s+default\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*;?")
        .expect("valid default component regex");
    if let Some(captures) = named.captures(source) {
        let name = captures.get(1)?.as_str().to_string();
        let declaration = Regex::new(&format!(
            r"(?m)^\s*(?:export\s+)?(?:const|let|var|function|class)\s+{}\b",
            regex::escape(&name)
        ))
        .expect("valid component declaration regex")
        .find(source)
        .map_or_else(|| captures.get(0).unwrap().start(), |found| found.start());
        return Some((name, declaration));
    }
    None
}

fn component_props_expression(source: &str, component: &str) -> Option<String> {
    let annotated = Regex::new(&format!(
        r"(?s)(?:const|let|var)\s+{}\s*:\s*(?:React\.)?(?:FC|FunctionComponent)\s*<",
        regex::escape(component)
    ))
    .expect("valid component annotation regex");
    if let Some(found) = annotated.find(source) {
        let open = found.end() - 1;
        let close = find_matching(source, open, '<', '>')?;
        return Some(source[open + 1..close].trim().to_string());
    }

    let forward_ref = Regex::new(&format!(
        r"(?s)(?:const|let|var)\s+{}(?:\s*:[^=;]+)?\s*=\s*(?:React\.)?forwardRef\s*<",
        regex::escape(component)
    ))
    .expect("valid forwardRef regex");
    if let Some(found) = forward_ref.find(source) {
        let open = found.end() - 1;
        let close = find_matching(source, open, '<', '>')?;
        let arguments = split_top_level(&source[open + 1..close], ',');
        if arguments.len() >= 2 {
            return Some(arguments[1].trim().to_string());
        }
    }

    let function = Regex::new(&format!(
        r"(?s)(?:export\s+)?(?:async\s+)?function\s+{}\s*(?:<[^{{}};]*>)?\s*\(",
        regex::escape(component)
    ))
    .expect("valid named function regex");
    let parameter = if let Some(found) = function.find(source) {
        let open = found.end() - 1;
        let close = find_matching(source, open, '(', ')')?;
        first_parameter(&source[open + 1..close])
    } else {
        let binding = Regex::new(&format!(
            r"(?s)(?:const|let|var)\s+{}(?:\s*:[^=;]+)?\s*=\s*(?:async\s+)?(?:<[^;{{}}]*>\s*)?\(",
            regex::escape(component)
        ))
        .expect("valid component binding regex");
        let found = binding.find(source)?;
        let open = found.end() - 1;
        let close = find_matching(source, open, '(', ')')?;
        first_parameter(&source[open + 1..close])
    }?;
    let colon = find_top_level(parameter, ':')?;
    let expression = strip_parameter_initializer(parameter[colon + 1..].trim());
    (!expression.is_empty()).then(|| expression.to_string())
}

fn default_export_parameter(source: &str) -> Option<&str> {
    let function =
        Regex::new(r"(?s)export\s+default\s+(?:async\s+)?function(?:\s+[A-Za-z_$][A-Za-z0-9_$]*)?")
            .expect("valid default function regex");
    if let Some(found) = function.find(source)
        && let Some(open_offset) = source[found.end()..].find('(')
    {
        let open = found.end() + open_offset;
        if let Some(close) = find_matching(source, open, '(', ')') {
            return first_parameter(&source[open + 1..close]);
        }
    }

    let direct_arrow = Regex::new(r"(?s)export\s+default\s+(?:async\s+)?(?:<[^;{}]*>\s*)?\(")
        .expect("valid default arrow regex");
    if let Some(found) = direct_arrow.find(source) {
        let open = found.end() - 1;
        if let Some(close) = find_matching(source, open, '(', ')') {
            return first_parameter(&source[open + 1..close]);
        }
    }

    let named = Regex::new(r"(?m)export\s+default\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*;?")
        .expect("valid named default export regex");
    let name = named.captures(source)?.get(1)?.as_str();
    let binding = Regex::new(&format!(
        r"(?s)(?:const|let|var)\s+{}(?:\s*:[^=;]+)?\s*=\s*(?:async\s+)?(?:<[^;{{}}]*>\s*)?\(",
        regex::escape(name)
    ))
    .expect("valid demo binding regex");
    let found = binding.find(source)?;
    let open = found.end() - 1;
    let close = find_matching(source, open, '(', ')')?;
    first_parameter(&source[open + 1..close])
}

fn first_parameter(parameters: &str) -> Option<&str> {
    let end = find_top_level(parameters, ',').unwrap_or(parameters.len());
    let parameter = parameters[..end].trim();
    (!parameter.is_empty()).then_some(parameter)
}

fn strip_parameter_initializer(value: &str) -> &str {
    find_top_level(value, '=')
        .map(|index| value[..index].trim())
        .unwrap_or_else(|| value.trim())
}

fn infer_parameter_defaults(pattern: &str) -> BTreeMap<String, String> {
    let pattern = pattern.trim();
    if !pattern.starts_with('{') {
        return BTreeMap::new();
    }
    let Some(close) = find_matching(pattern, 0, '{', '}') else {
        return BTreeMap::new();
    };
    split_top_level(&pattern[1..close], ',')
        .into_iter()
        .filter_map(|item| {
            let equal = find_top_level(&item, '=')?;
            let mut name = item[..equal].trim();
            if let Some(colon) = find_top_level(name, ':') {
                name = name[colon + 1..].trim();
            }
            let value = item[equal + 1..].trim();
            (is_property_name(name) && is_static_literal(value))
                .then(|| (name.to_string(), value.to_string()))
        })
        .collect()
}

#[derive(Debug, Clone, Default)]
struct Resolved {
    description: String,
    props: Vec<ApiProp>,
    inherited: Vec<InheritedGroup>,
    warnings: Vec<String>,
}

#[derive(Debug, Default)]
struct Resolver {
    cache: BTreeMap<(PathBuf, String), Resolved>,
    inputs: BTreeSet<PathBuf>,
}

#[derive(Debug)]
struct Declaration {
    description: String,
    props: Vec<ApiProp>,
    bases: Vec<String>,
    reexport: Option<(String, String)>,
}

#[derive(Debug)]
struct Import {
    imported: String,
    source: String,
}

impl Resolver {
    fn resolve(
        &mut self,
        path: &Path,
        symbol: &str,
        stack: &mut Vec<(PathBuf, String)>,
    ) -> Result<Resolved, ApiError> {
        let key = (path.to_path_buf(), symbol.to_string());
        if let Some(cached) = self.cache.get(&key) {
            return Ok(cached.clone());
        }
        if let Some(start) = stack.iter().position(|item| item == &key) {
            let mut chain: Vec<String> = stack[start..]
                .iter()
                .map(|(file, name)| format!("{}#{name}", file.display()))
                .collect();
            chain.push(format!("{}#{symbol}", path.display()));
            return Err(ApiError::CircularType(chain));
        }

        stack.push(key.clone());
        let source = self.read_source(path)?;
        let imports = parse_imports(&source);
        let declaration =
            find_declaration(&source, path, symbol)?.ok_or_else(|| ApiError::SymbolNotFound {
                path: path.to_path_buf(),
                symbol: symbol.to_string(),
            })?;
        if let Some((name, specifier)) = declaration.reexport {
            let imported_path = resolve_local_import(path, &specifier)?;
            let result = self.resolve(&imported_path, &name, stack)?;
            stack.pop();
            self.cache.insert(key, result.clone());
            return Ok(result);
        }

        let mut result = Resolved {
            description: declaration.description,
            props: declaration.props,
            ..Resolved::default()
        };
        for expression in declaration.bases {
            self.merge_expression(path, &imports, &expression, stack, &mut result)?;
        }
        stack.pop();
        deduplicate(&mut result);
        self.cache.insert(key, result.clone());
        Ok(result)
    }

    fn merge_expression(
        &mut self,
        path: &Path,
        imports: &BTreeMap<String, Import>,
        expression: &str,
        stack: &mut Vec<(PathBuf, String)>,
        result: &mut Resolved,
    ) -> Result<(), ApiError> {
        let expression = strip_parens(expression.trim());
        for part in split_top_level(expression, '&') {
            let part = strip_parens(part.trim());
            if part.is_empty() {
                continue;
            }
            if let Some((utility, inner, keys)) = parse_utility(part) {
                let mut temporary = Resolved::default();
                self.merge_expression(path, imports, &inner, stack, &mut temporary)?;
                apply_utility(
                    utility,
                    &mut temporary.props,
                    keys.as_deref().unwrap_or_default(),
                );
                merge(result, temporary);
            } else if part.starts_with('{') && part.ends_with('}') {
                result
                    .props
                    .extend(parse_members(&part[1..part.len() - 1], path));
            } else if split_top_level(part, '|').len() > 1 {
                result.inherited.push(InheritedGroup {
                    name: part.to_string(),
                    source: path.to_string_lossy().into_owned(),
                    type_text: part.to_string(),
                });
                result.warnings.push(format!(
                    "union `{part}` cannot be flattened safely; preserved as a type source"
                ));
            } else {
                let name = leading_identifier(part);
                if name.is_empty() {
                    result.warnings.push(format!("unsupported type `{part}`"));
                    continue;
                }
                if let Some(import) = imports.get(name) {
                    if import.source.starts_with('.') {
                        let imported_path = resolve_local_import(path, &import.source)?;
                        merge(
                            result,
                            self.resolve(&imported_path, &import.imported, stack)?,
                        );
                    } else {
                        result.inherited.push(InheritedGroup {
                            name: name.to_string(),
                            source: import.source.clone(),
                            type_text: part.to_string(),
                        });
                    }
                    continue;
                }
                match self.resolve(path, name, stack) {
                    Ok(resolved) => merge(result, resolved),
                    Err(ApiError::SymbolNotFound { .. }) => {
                        result.inherited.push(InheritedGroup {
                            name: name.to_string(),
                            source: path.to_string_lossy().into_owned(),
                            type_text: part.to_string(),
                        });
                        result
                            .warnings
                            .push(format!("cannot expand `{part}`; preserved as source text"));
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }

    fn read_source(&mut self, path: &Path) -> Result<String, ApiError> {
        let source = read(path)?;
        self.inputs.insert(path.to_path_buf());
        Ok(source)
    }
}

fn find_declaration(
    source: &str,
    path: &Path,
    symbol: &str,
) -> Result<Option<Declaration>, ApiError> {
    let escaped = regex::escape(symbol);
    let interface = Regex::new(&format!(
        r"(?m)(?:export\s+)?(?:declare\s+)?interface\s+{escaped}(?:\s*<[^{{]*>)?\s*([^{{]*)\{{"
    ))
    .expect("valid interface regex");
    if let Some(captures) = interface.captures(source) {
        let whole = captures.get(0).expect("interface capture");
        let open = whole.end() - 1;
        let close = find_matching(source, open, '{', '}').ok_or_else(|| {
            ApiError::InvalidSource(
                path.to_path_buf(),
                format!("interface {symbol} has no `}}`"),
            )
        })?;
        let header = captures.get(1).map_or("", |value| value.as_str());
        let bases = header
            .trim()
            .strip_prefix("extends")
            .map(|value| split_top_level(value, ','))
            .unwrap_or_default();
        return Ok(Some(Declaration {
            description: preceding_jsdoc(source, whole.start()).description,
            props: parse_members(&source[open + 1..close], path),
            bases,
            reexport: None,
        }));
    }

    let alias = Regex::new(&format!(
        r"(?m)(?:export\s+)?(?:declare\s+)?type\s+{escaped}(?:\s*<[^=]*>)?\s*="
    ))
    .expect("valid alias regex");
    if let Some(found) = alias.find(source) {
        let end = find_statement_end(source, found.end());
        return Ok(Some(Declaration {
            description: preceding_jsdoc(source, found.start()).description,
            props: Vec::new(),
            bases: vec![source[found.end()..end].trim().to_string()],
            reexport: None,
        }));
    }

    let reexport = Regex::new(r#"(?m)export\s*\{([^}]*)\}\s*from\s*["']([^"']+)["']"#)
        .expect("valid re-export regex");
    for captures in reexport.captures_iter(source) {
        for item in captures[1].split(',') {
            let words: Vec<_> = item.split_whitespace().collect();
            let pair = match words.as_slice() {
                [name] => Some((*name, *name)),
                [name, "as", alias] => Some((*name, *alias)),
                _ => None,
            };
            if let Some((imported, exported)) = pair
                && exported == symbol
            {
                return Ok(Some(Declaration {
                    description: String::new(),
                    props: Vec::new(),
                    bases: Vec::new(),
                    reexport: Some((imported.to_string(), captures[2].to_string())),
                }));
            }
        }
    }
    Ok(None)
}

fn parse_imports(source: &str) -> BTreeMap<String, Import> {
    let regex = Regex::new(r#"(?m)import\s+(?:type\s+)?\{([^}]*)\}\s+from\s+["']([^"']+)["']"#)
        .expect("valid import regex");
    let mut imports = BTreeMap::new();
    for captures in regex.captures_iter(source) {
        for item in captures[1].split(',') {
            let words: Vec<_> = item.split_whitespace().collect();
            let pair = match words.as_slice() {
                [name] => Some((*name, *name)),
                [name, "as", alias] => Some((*name, *alias)),
                ["type", name] => Some((*name, *name)),
                ["type", name, "as", alias] => Some((*name, *alias)),
                _ => None,
            };
            if let Some((imported, local)) = pair {
                imports.insert(
                    local.to_string(),
                    Import {
                        imported: imported.to_string(),
                        source: captures[2].to_string(),
                    },
                );
            }
        }
    }
    imports
}

fn parse_members(body: &str, path: &Path) -> Vec<ApiProp> {
    let mut result = Vec::new();
    let mut cursor = 0;
    while cursor < body.len() {
        while cursor < body.len() && body.as_bytes()[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= body.len() {
            break;
        }
        let start = cursor;
        if body[cursor..].starts_with("/**") {
            let Some(relative_end) = body[cursor + 3..].find("*/") else {
                break;
            };
            cursor += relative_end + 5;
            while cursor < body.len() && body.as_bytes()[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
        }
        let mut depth = Depth::default();
        let mut quote = None;
        let mut escaped = false;
        while cursor < body.len() {
            let ch = body.as_bytes()[cursor] as char;
            if let Some(active) = quote {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == active {
                    quote = None;
                }
            } else if matches!(ch, '\'' | '"' | '`') {
                quote = Some(ch);
            } else if depth.is_zero() && ch == ';' {
                cursor += 1;
                break;
            } else {
                depth.update(ch);
            }
            cursor += 1;
        }
        if let Some(prop) = parse_member(body[start..cursor].trim(), path) {
            result.push(prop);
        }
    }
    result
}

fn parse_member(raw: &str, path: &Path) -> Option<ApiProp> {
    let (comment, declaration) = leading_jsdoc(raw);
    let colon = find_top_level(declaration, ':')?;
    let mut name = declaration[..colon].trim();
    if name.starts_with('[') || name.starts_with('(') {
        return None;
    }
    let required = !name.ends_with('?');
    name = name
        .trim_end_matches('?')
        .trim_matches(|ch| ch == '\'' || ch == '"');
    if !is_property_name(name) {
        return None;
    }
    let docs = parse_jsdoc(comment);
    Some(ApiProp {
        name: name.to_string(),
        type_text: declaration[colon + 1..]
            .trim()
            .trim_end_matches(';')
            .trim()
            .to_string(),
        required,
        description: docs.description,
        default_value: docs.tags.get("default").cloned(),
        deprecated: docs.tags.get("deprecated").cloned(),
        since: docs.tags.get("since").cloned(),
        source: path.to_string_lossy().into_owned(),
    })
}

fn parse_utility(value: &str) -> Option<(&str, String, Option<Vec<String>>)> {
    for name in ["Partial", "Required", "Pick", "Omit"] {
        let prefix = format!("{name}<");
        if value.starts_with(&prefix) && value.ends_with('>') {
            let args = split_top_level(&value[prefix.len()..value.len() - 1], ',');
            if matches!(name, "Partial" | "Required") && args.len() == 1 {
                return Some((name, args[0].trim().to_string(), None));
            }
            if matches!(name, "Pick" | "Omit") && args.len() == 2 {
                let keys = split_top_level(&args[1], '|')
                    .into_iter()
                    .map(|key| {
                        key.trim()
                            .trim_matches(|ch| ch == '\'' || ch == '"')
                            .to_string()
                    })
                    .collect();
                return Some((name, args[0].trim().to_string(), Some(keys)));
            }
        }
    }
    None
}

fn apply_utility(name: &str, props: &mut Vec<ApiProp>, keys: &[String]) {
    match name {
        "Partial" => props.iter_mut().for_each(|prop| prop.required = false),
        "Required" => props.iter_mut().for_each(|prop| prop.required = true),
        "Pick" => props.retain(|prop| keys.contains(&prop.name)),
        "Omit" => props.retain(|prop| !keys.contains(&prop.name)),
        _ => {}
    }
}

fn infer_defaults(source: &str, component: &str) -> BTreeMap<String, String> {
    let regex = Regex::new(&format!(
        r"(?s)(?:function\s+{}|const\s+{}(?:\s*:[^=;]+)?\s*=\s*)[^{{=]*\(\s*\{{",
        regex::escape(component),
        regex::escape(component)
    ))
    .expect("valid component regex");
    let Some(found) = regex.find(source) else {
        return BTreeMap::new();
    };
    let open = found.end() - 1;
    let Some(close) = find_matching(source, open, '{', '}') else {
        return BTreeMap::new();
    };
    split_top_level(&source[open + 1..close], ',')
        .into_iter()
        .filter_map(|item| {
            let equal = find_top_level(&item, '=')?;
            let name = item[..equal].trim();
            let value = item[equal + 1..].trim();
            (is_property_name(name) && is_static_literal(value))
                .then(|| (name.to_string(), value.to_string()))
        })
        .collect()
}

#[derive(Debug, Default)]
struct JsDoc {
    description: String,
    tags: BTreeMap<String, String>,
}

fn preceding_jsdoc(source: &str, offset: usize) -> JsDoc {
    let prefix = source[..offset].trim_end();
    if !prefix.ends_with("*/") {
        return JsDoc::default();
    }
    prefix
        .rfind("/**")
        .map(|start| parse_jsdoc(Some(&prefix[start..])))
        .unwrap_or_default()
}

fn leading_jsdoc(value: &str) -> (Option<&str>, &str) {
    let value = value.trim_start();
    if value.starts_with("/**")
        && let Some(end) = value.find("*/")
    {
        return (Some(&value[..end + 2]), value[end + 2..].trim());
    }
    (None, value)
}

fn parse_jsdoc(comment: Option<&str>) -> JsDoc {
    let Some(comment) = comment else {
        return JsDoc::default();
    };
    let mut docs = JsDoc::default();
    let mut description = Vec::new();
    let mut active_tag: Option<String> = None;
    for raw in comment
        .trim_start_matches("/**")
        .trim_end_matches("*/")
        .lines()
    {
        let line = raw.trim().trim_start_matches('*').trim();
        if let Some(tag) = line.strip_prefix('@') {
            let mut parts = tag.splitn(2, char::is_whitespace);
            let name = parts.next().unwrap_or_default().to_string();
            docs.tags.insert(
                name.clone(),
                parts.next().unwrap_or_default().trim().to_string(),
            );
            active_tag = Some(name);
        } else if !line.is_empty() {
            if let Some(name) = &active_tag {
                let value = docs.tags.entry(name.clone()).or_default();
                if !value.is_empty() {
                    value.push(' ');
                }
                value.push_str(line);
            } else {
                description.push(line);
            }
        }
    }
    docs.description = description.join(" ");
    docs
}

fn resolve_local_import(path: &Path, specifier: &str) -> Result<PathBuf, ApiError> {
    resolve_local_import_with_file_system(&OsDeclarationFileSystem, path, specifier)
}

fn resolve_local_import_with_file_system(
    file_system: &(impl DeclarationFileSystem + ?Sized),
    path: &Path,
    specifier: &str,
) -> Result<PathBuf, ApiError> {
    let (base, candidates) = local_import_candidates(path, specifier);
    candidates
        .into_iter()
        .find(|candidate| file_system.is_file(candidate))
        .map(|candidate| declaration_canonical(file_system, &candidate))
        .transpose()?
        .ok_or_else(|| ApiError::Io(base, format!("cannot resolve local import `{specifier}`")))
}

fn resolve_required_local_declaration_import(
    file_system: &(impl DeclarationFileSystem + ?Sized),
    path: &Path,
    specifier: &str,
) -> Result<PathBuf, ApiError> {
    let (base, candidates) = local_import_candidates(path, specifier);
    resolve_declaration_candidate(file_system, candidates)?
        .ok_or_else(|| ApiError::Io(base, format!("cannot resolve local import `{specifier}`")))
}

fn resolve_optional_local_declaration_import(
    file_system: &(impl DeclarationFileSystem + ?Sized),
    path: &Path,
    specifier: &str,
) -> Result<Option<PathBuf>, ApiError> {
    let (_, candidates) = local_import_candidates(path, specifier);
    resolve_declaration_candidate(file_system, candidates)
}

fn resolve_declaration_candidate(
    file_system: &(impl DeclarationFileSystem + ?Sized),
    candidates: impl IntoIterator<Item = PathBuf>,
) -> Result<Option<PathBuf>, ApiError> {
    for candidate in candidates {
        // A runtime resource may exist at the literal request while a declaration shim such as
        // `theme.scss.d.ts` appears later in the candidate list. Never canonicalize or parse the
        // opaque resource itself; keep searching only among declaration-capable source paths.
        if declaration_source_type(&candidate).is_err() {
            continue;
        }
        if !file_system.is_file(&candidate) {
            continue;
        }
        let candidate = declaration_canonical(file_system, &candidate)?;
        if declaration_source_type(&candidate).is_ok() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn local_import_candidates(path: &Path, specifier: &str) -> (PathBuf, Vec<PathBuf>) {
    let base = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(specifier);
    let mut candidates = Vec::new();
    if let Some(extension) = base.extension().and_then(|value| value.to_str()) {
        let source_base = base.with_extension("");
        match extension {
            "mjs" => candidates
                .extend([".mts", ".d.mts"].map(|suffix| append_path_text(&source_base, suffix))),
            "cjs" => candidates
                .extend([".cts", ".d.cts"].map(|suffix| append_path_text(&source_base, suffix))),
            _ => {}
        }
        candidates.push(base.clone());
        let source_base = if matches!(extension, "js" | "jsx" | "mjs" | "cjs") {
            source_base
        } else {
            base.clone()
        };
        candidates
            .extend([".ts", ".tsx", ".d.ts"].map(|suffix| append_path_text(&source_base, suffix)));
    } else {
        candidates.push(base.clone());
        candidates.extend(["ts", "tsx", "d.ts"].map(|ext| base.with_extension(ext)));
        candidates.extend(["index.ts", "index.tsx", "index.d.ts"].map(|name| base.join(name)));
    }
    (base, candidates)
}

fn declaration_canonical(
    file_system: &(impl DeclarationFileSystem + ?Sized),
    path: &Path,
) -> Result<PathBuf, ApiError> {
    file_system
        .canonicalize(path)
        .map_err(|error| ApiError::Io(path.to_path_buf(), error))
}

fn append_path_text(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn read(path: &Path) -> Result<String, ApiError> {
    fs::read_to_string(path).map_err(|error| ApiError::Io(path.to_path_buf(), error.to_string()))
}

fn canonical(path: &Path) -> Result<PathBuf, ApiError> {
    fs::canonicalize(path).map_err(|error| ApiError::Io(path.to_path_buf(), error.to_string()))
}

fn merge(target: &mut Resolved, source: Resolved) {
    target.props.extend(source.props);
    target.inherited.extend(source.inherited);
    target.warnings.extend(source.warnings);
}

fn deduplicate(result: &mut Resolved) {
    let mut props = BTreeSet::new();
    result.props.retain(|prop| props.insert(prop.name.clone()));
    let mut groups = BTreeSet::new();
    result
        .inherited
        .retain(|group| groups.insert((group.name.clone(), group.source.clone())));
    result.warnings.sort();
    result.warnings.dedup();
}

fn split_top_level(value: &str, delimiter: char) -> Vec<String> {
    let mut result = Vec::new();
    let mut start = 0;
    let mut depth = Depth::default();
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == active {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
        } else if depth.is_zero() && ch == delimiter {
            result.push(value[start..index].to_string());
            start = index + ch.len_utf8();
        } else {
            depth.update(ch);
        }
    }
    result.push(value[start..].to_string());
    result
}

#[derive(Debug, Default)]
struct Depth {
    round: usize,
    square: usize,
    curly: usize,
    angle: usize,
}

impl Depth {
    fn update(&mut self, ch: char) {
        match ch {
            '(' => self.round += 1,
            ')' => self.round = self.round.saturating_sub(1),
            '[' => self.square += 1,
            ']' => self.square = self.square.saturating_sub(1),
            '{' => self.curly += 1,
            '}' => self.curly = self.curly.saturating_sub(1),
            '<' => self.angle += 1,
            '>' => self.angle = self.angle.saturating_sub(1),
            _ => {}
        }
    }

    fn is_zero(&self) -> bool {
        self.round == 0 && self.square == 0 && self.curly == 0 && self.angle == 0
    }
}

fn find_matching(source: &str, start: usize, open: char, close: char) -> Option<usize> {
    let mut depth = 0;
    let mut quote = None;
    let mut escaped = false;
    for (offset, ch) in source[start..].char_indices() {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == active {
                quote = None;
            }
        } else if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
        } else if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                return Some(start + offset);
            }
        }
    }
    None
}

fn find_statement_end(source: &str, start: usize) -> usize {
    let mut depth = Depth::default();
    let mut quote = None;
    for (offset, ch) in source[start..].char_indices() {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
        } else if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
        } else if depth.is_zero() && ch == ';' {
            return start + offset;
        } else {
            depth.update(ch);
        }
    }
    source.len()
}

fn find_top_level(value: &str, needle: char) -> Option<usize> {
    let mut depth = Depth::default();
    let mut quote = None;
    for (index, ch) in value.char_indices() {
        if let Some(active) = quote {
            if ch == active {
                quote = None;
            }
        } else if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
        } else if depth.is_zero() && ch == needle {
            return Some(index);
        } else {
            depth.update(ch);
        }
    }
    None
}

fn strip_parens(mut value: &str) -> &str {
    loop {
        let trimmed = value.trim();
        if trimmed.starts_with('(')
            && trimmed.ends_with(')')
            && find_matching(trimmed, 0, '(', ')') == Some(trimmed.len() - 1)
        {
            value = &trimmed[1..trimmed.len() - 1];
        } else {
            return trimmed;
        }
    }
}

fn declaration_source_type(path: &Path) -> Result<SourceType, ApiError> {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("tsx") => Ok(SourceType::Tsx),
        Some("ts" | "mts" | "cts") => Ok(SourceType::TypeScript),
        _ => Err(ApiError::InvalidSource(
            path.to_path_buf(),
            "library declarations require a .ts, .tsx, .mts, .cts, or .d.ts source".to_string(),
        )),
    }
}

fn is_declaration_file_path(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    name.ends_with(".d.ts") || name.ends_with(".d.mts") || name.ends_with(".d.cts")
}

fn validate_declaration_template(
    path: &Path,
    template: &str,
    requests: &[DeclarationRequestFact],
) -> Result<(), ApiError> {
    let mut previous_end = 0;
    for request in requests {
        let range = request.template_range();
        if range.start < previous_end
            || range.end > template.len()
            || !template.is_char_boundary(range.start)
            || !template.is_char_boundary(range.end)
            || !is_quoted_literal(&template[range.clone()])
        {
            return Err(ApiError::InvalidSource(
                path.to_path_buf(),
                "parser returned an invalid declaration request range".to_string(),
            ));
        }
        previous_end = range.end;
    }
    Ok(())
}

fn declaration_output_paths<'a>(
    root: &Path,
    entry: &Path,
    sources: impl Iterator<Item = &'a Path>,
) -> BTreeMap<PathBuf, PathBuf> {
    let mut allocated = BTreeSet::new();
    let mut output_paths = BTreeMap::new();
    for source in sources {
        let mut output = if source == entry {
            PathBuf::from("index.d.ts")
        } else {
            let relative = source
                .strip_prefix(root)
                .expect("frozen declaration inputs stay inside their project root");
            let mut output = PathBuf::from("_wake").join(relative);
            output.set_extension("d.ts");
            output
        };
        if allocated.contains(&output) {
            let relative = source
                .strip_prefix(root)
                .expect("frozen declaration inputs stay inside their project root");
            output = append_path_text(&PathBuf::from("_wake").join(relative), ".d.ts");
            let mut collision = 2usize;
            while allocated.contains(&output) {
                output = append_path_text(
                    &PathBuf::from("_wake").join(relative),
                    &format!(".{collision}.d.ts"),
                );
                collision += 1;
            }
        }
        allocated.insert(output.clone());
        output_paths.insert(source.to_path_buf(), output);
    }
    output_paths
}

fn relative_declaration_specifier(current: &Path, target: &Path) -> String {
    let from = current.parent().unwrap_or_else(|| Path::new(""));
    let from_components = from.components().collect::<Vec<_>>();
    let target_components = target.components().collect::<Vec<_>>();
    let common = from_components
        .iter()
        .zip(&target_components)
        .take_while(|(left, right)| left == right)
        .count();
    let mut result = PathBuf::new();
    for _ in common..from_components.len() {
        result.push("..");
    }
    for component in &target_components[common..] {
        result.push(component.as_os_str());
    }
    let mut value = result.to_string_lossy().replace('\\', "/");
    if let Some(stem) = value.strip_suffix(".d.ts") {
        value = format!("{stem}.js");
    } else {
        value.push_str(".js");
    }
    if !value.starts_with('.') {
        value.insert_str(0, "./");
    }
    value
}

fn is_quoted_literal(value: &str) -> bool {
    value.len() >= 2
        && matches!(value.as_bytes().first(), Some(b'\'') | Some(b'"'))
        && value.as_bytes().first() == value.as_bytes().last()
}

fn quote_like(original: &str, specifier: &str) -> String {
    let quote = original
        .as_bytes()
        .first()
        .copied()
        .filter(|quote| matches!(quote, b'\'' | b'"'))
        .unwrap_or(b'"');
    let mut literal = String::with_capacity(specifier.len() + 2);
    literal.push(char::from(quote));
    for character in specifier.chars() {
        match character {
            '\\' => literal.push_str("\\\\"),
            '\t' => literal.push_str("\\t"),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            character if character <= '\u{1f}' => {
                use std::fmt::Write;
                write!(literal, "\\u{:04X}", character as u32)
                    .expect("writing a declaration literal cannot fail");
            }
            '\u{2028}' => literal.push_str("\\u2028"),
            '\u{2029}' => literal.push_str("\\u2029"),
            character if character as u32 == u32::from(quote) => {
                literal.push('\\');
                literal.push(character);
            }
            character => literal.push(character),
        }
    }
    literal.push(char::from(quote));
    literal
}

fn leading_identifier(value: &str) -> &str {
    let end = value
        .char_indices()
        .find_map(|(index, ch)| {
            (!ch.is_ascii_alphanumeric() && ch != '_' && ch != '$').then_some(index)
        })
        .unwrap_or(value.len());
    &value[..end]
}

fn is_property_name(value: &str) -> bool {
    let mut chars = value.chars();
    chars
        .next()
        .is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_' || ch == '$')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '$' | '-'))
}

fn is_static_literal(value: &str) -> bool {
    matches!(value, "true" | "false" | "null" | "undefined")
        || value.parse::<f64>().is_ok()
        || (value.starts_with('"') && value.ends_with('"'))
        || (value.starts_with('\'') && value.ends_with('\''))
        || (value.starts_with('[') && value.ends_with(']') && !value.contains("..."))
        || (value.starts_with('{') && value.ends_with('}') && !value.contains("..."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[derive(Debug, Clone, Default, PartialEq, Eq)]
    struct DeclarationFileSystemCalls {
        canonicalize: Vec<PathBuf>,
        is_file: Vec<PathBuf>,
        reads: BTreeMap<PathBuf, usize>,
    }

    #[derive(Default)]
    struct CountingDeclarationFileSystem {
        calls: Mutex<DeclarationFileSystemCalls>,
    }

    impl CountingDeclarationFileSystem {
        fn calls(&self) -> DeclarationFileSystemCalls {
            self.calls.lock().unwrap().clone()
        }
    }

    impl DeclarationFileSystem for CountingDeclarationFileSystem {
        fn canonicalize(&self, path: &Path) -> Result<PathBuf, String> {
            self.calls
                .lock()
                .unwrap()
                .canonicalize
                .push(path.to_path_buf());
            fs::canonicalize(path).map_err(|error| error.to_string())
        }

        fn is_file(&self, path: &Path) -> bool {
            self.calls.lock().unwrap().is_file.push(path.to_path_buf());
            path.is_file()
        }

        fn read_to_string(&self, path: &Path) -> Result<String, String> {
            *self
                .calls
                .lock()
                .unwrap()
                .reads
                .entry(path.to_path_buf())
                .or_default() += 1;
            fs::read_to_string(path).map_err(|error| error.to_string())
        }
    }

    fn fixture(files: &[(&str, &str)]) -> PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!("wake-tsdoc-{id}"));
        fs::create_dir_all(&root).expect("fixture directory");
        for (name, source) in files {
            let path = root.join(name);
            fs::create_dir_all(path.parent().expect("fixture parent"))
                .expect("fixture parent directory");
            fs::write(path, source).expect("fixture file");
        }
        root
    }

    #[test]
    fn extracts_jsdoc_and_static_component_defaults() {
        let root = fixture(&[(
            "button.tsx",
            r#"
                /** Button properties. */
                export interface ButtonProps {
                    /** Visible label. */
                    label: string;
                    /**
                     * Visual intent.
                     * @default "primary"
                     * @since 1.2.0
                     */
                    intent?: "primary" | "danger";
                    /**
                     * Old flag.
                     * @deprecated Use intent.
                     */
                    danger?: boolean;
                    count?: number;
                }
                export function Button({ count = 2 }: ButtonProps) { return count }
            "#,
        )]);
        let doc = extract_api(root.join("button.tsx"), "ButtonProps", Some("Button")).unwrap();
        assert_eq!(doc.description, "Button properties.");
        assert_eq!(doc.props.len(), 4);
        assert_eq!(doc.props[0].name, "label");
        let intent = doc.props.iter().find(|prop| prop.name == "intent").unwrap();
        assert_eq!(intent.default_value.as_deref(), Some("\"primary\""));
        assert_eq!(intent.since.as_deref(), Some("1.2.0"));
        let danger = doc.props.iter().find(|prop| prop.name == "danger").unwrap();
        assert_eq!(danger.deprecated.as_deref(), Some("Use intent."));
        let count = doc.props.iter().find(|prop| prop.name == "count").unwrap();
        assert_eq!(count.default_value.as_deref(), Some("2"));
    }

    #[test]
    fn resolves_local_utilities_and_collapses_package_types() {
        let root = fixture(&[
            (
                "base.ts",
                "export interface BaseProps { id: string; hidden?: boolean; }",
            ),
            (
                "input.tsx",
                r#"
                    import type { BaseProps } from "./base";
                    import type { HTMLAttributes } from "react";
                    export type InputProps = Required<Pick<BaseProps, "id">>
                        & Omit<BaseProps, "id">
                        & HTMLAttributes<HTMLInputElement>
                        & { value?: string };
                "#,
            ),
        ]);
        let doc = extract_api(root.join("input.tsx"), "InputProps", None).unwrap();
        assert!(
            doc.props
                .iter()
                .any(|prop| prop.name == "id" && prop.required)
        );
        assert!(doc.props.iter().any(|prop| prop.name == "hidden"));
        assert!(doc.props.iter().any(|prop| prop.name == "value"));
        assert!(doc.inherited.iter().any(|group| group.source == "react"));
    }

    #[test]
    fn reports_non_converging_local_cycles() {
        let root = fixture(&[(
            "cycle.ts",
            "export type A = B & { a: string }; export type B = A & { b: string };",
        )]);
        assert!(matches!(
            extract_api(root.join("cycle.ts"), "A", None),
            Err(ApiError::CircularType(_))
        ));
    }
    #[test]
    fn extracts_demo_parameter_props_and_defaults() {
        let root = fixture(&[
            (
                "button.ts",
                r#"
                    export interface ButtonProps {
                        /**
                         * Label.
                         * @default "Docs"
                         */
                        label?: string;
                        count?: number;
                        disabled?: boolean;
                    }
                "#,
            ),
            (
                "demo.tsx",
                r#"
                    import type { ButtonProps } from "./button";
                    export const meta = { title: "Typed" };
                    export default function Demo(
                        { label = "Demo", count = 2 }: ButtonProps
                    ) { return <button disabled={false}>{label}{count}</button>; }
                "#,
            ),
        ]);
        let doc = extract_demo_props(root.join("demo.tsx"))
            .unwrap()
            .expect("typed demo");
        assert_eq!(doc.props.len(), 3);
        let label = doc.props.iter().find(|prop| prop.name == "label").unwrap();
        assert_eq!(label.default_value.as_deref(), Some("\"Docs\""));
        let count = doc.props.iter().find(|prop| prop.name == "count").unwrap();
        assert_eq!(count.default_value.as_deref(), Some("2"));
    }

    #[test]
    fn extracts_named_typed_arrow_demo_and_ignores_untyped_demo() {
        let root = fixture(&[
            (
                "typed.tsx",
                r#"
                    interface Props { value: string; enabled?: boolean; }
                    const Demo = (props: Props) => <span>{props.value}</span>;
                    export default Demo;
                "#,
            ),
            (
                "untyped.tsx",
                "export default function Demo() { return <span />; }",
            ),
        ]);
        let typed = extract_demo_props(root.join("typed.tsx"))
            .unwrap()
            .expect("typed arrow demo");
        assert!(typed.props.iter().any(|prop| prop.name == "value"));
        assert!(
            extract_demo_props(root.join("untyped.tsx"))
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn resolves_direct_arrow_demo_through_a_relative_reexport() {
        let root = fixture(&[
            (
                "base.ts",
                "export interface ButtonProps { label?: string; count: number; }",
            ),
            ("index.ts", "export { ButtonProps } from \"./base\";"),
            (
                "demo.tsx",
                r#"
                    import type { ButtonProps } from "./index";
                    export default ({ label = "Create", count }: ButtonProps) => (
                        <button>{label}{count}</button>
                    );
                "#,
            ),
            (
                "broken.tsx",
                "export default (props: MissingProps) => <span />;",
            ),
        ]);
        let doc = extract_demo_props(root.join("demo.tsx"))
            .unwrap()
            .expect("typed direct arrow demo");
        assert!(doc.props.iter().any(|prop| prop.name == "count"));
        let label = doc.props.iter().find(|prop| prop.name == "label").unwrap();
        assert_eq!(label.default_value.as_deref(), Some("\"Create\""));
        let broken = extract_demo_props(root.join("broken.tsx"))
            .unwrap()
            .expect("unresolved typed demo");
        assert!(broken.props.is_empty());
        assert!(!broken.warnings.is_empty());
    }

    #[test]
    fn extracts_default_component_from_fc_and_typed_arrow_forms() {
        let root = fixture(&[
            (
                "button.tsx",
                r#"
                    import type { FC } from "react";
                    /** Primary button. */
                    export interface ButtonProps {
                        /** Text. */
                        label: string;
                        /** Disabled state. */
                        disabled?: boolean;
                    }
                    /** Render a button. */
                    const Button: FC<ButtonProps> = ({ disabled = false, label }) => <button>{label}</button>;
                    export default Button;
                "#,
            ),
            (
                "divider.tsx",
                r#"
                    interface DividerProps { vertical?: boolean; }
                    const Divider = ({ vertical = true }: DividerProps) => <hr />;
                    export default Divider;
                "#,
            ),
            (
                "alert.tsx",
                r#"
                    interface AlertProps { message: string; }
                    export default function Alert(props: AlertProps) { return <div>{props.message}</div>; }
                "#,
            ),
        ]);
        let button = extract_component_api(root.join("button.tsx")).unwrap();
        assert_eq!(button.display_name, "Button");
        assert_eq!(button.description, "Render a button.");
        assert_eq!(button.api.symbol, "ButtonProps");
        assert_eq!(button.api.props.len(), 2);
        assert_eq!(
            button
                .api
                .props
                .iter()
                .find(|prop| prop.name == "disabled")
                .unwrap()
                .default_value
                .as_deref(),
            Some("false")
        );

        let divider = extract_component_api(root.join("divider.tsx")).unwrap();
        assert_eq!(divider.display_name, "Divider");
        assert_eq!(divider.api.symbol, "DividerProps");
        let alert = extract_component_api(root.join("alert.tsx")).unwrap();
        assert_eq!(alert.display_name, "Alert");
        assert_eq!(alert.api.symbol, "AlertProps");
    }

    #[test]
    fn component_extraction_reports_every_transitively_read_source() {
        let root = fixture(&[
            (
                "button.tsx",
                "import type { ButtonProps } from './props.js';\nconst Button = (props: ButtonProps) => null;\nexport default Button;\n",
            ),
            (
                "props.ts",
                "import type { BaseProps } from './base.js';\nexport interface ButtonProps extends BaseProps { label: string; }\n",
            ),
            (
                "base.ts",
                "export interface BaseProps { disabled?: boolean; }\n",
            ),
        ]);

        let extraction = extract_component_api_with_provenance(root.join("button.tsx")).unwrap();

        assert_eq!(extraction.document.display_name, "Button");
        assert_eq!(
            extraction.inputs,
            ["base.ts", "button.tsx", "props.ts"]
                .map(|path| std::fs::canonicalize(root.join(path)).unwrap())
                .into_iter()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn freezes_multi_entry_graph_with_one_shared_read_per_source() {
        let root = fixture(&[
            (
                "src/one/index.ts",
                "import type Shared = require('../../shared.js'); export interface Public { shared: Shared; }",
            ),
            (
                "src/two/index.ts",
                "export type { Shared } from '../../shared.js'; export interface Public { other: Shared; }",
            ),
            (
                "shared.ts",
                "import { Model } from './model.js'; export interface Shared { model: Model; }",
            ),
            ("model.ts", "export interface Model { id: string; }"),
        ]);
        let file_system = CountingDeclarationFileSystem::default();
        let graph = prepare_library_declarations_with_file_system(
            &root,
            [
                DeclarationEntry::new("one", "src/one/index.ts"),
                DeclarationEntry::new("two", "src/two/index.ts"),
            ],
            &file_system,
        )
        .unwrap();

        assert_eq!(graph.inputs().count(), 4);
        let calls_after_prepare = file_system.calls();
        assert_eq!(calls_after_prepare.reads.len(), 4);
        assert!(calls_after_prepare.reads.values().all(|count| *count == 1));
        assert!(!calls_after_prepare.canonicalize.is_empty());
        assert!(!calls_after_prepare.is_file.is_empty());

        let bundles = graph.render();
        assert_eq!(bundles.len(), 2);
        for (bundle, owner) in bundles.iter().zip(["one", "two"]) {
            assert_eq!(bundle.owner, owner);
            assert!(
                bundle
                    .files
                    .iter()
                    .any(|file| file.file_name == Path::new("index.d.ts"))
            );
            let shared = bundle
                .files
                .iter()
                .find(|file| file.file_name == Path::new("_wake/shared.d.ts"))
                .expect("shared dependency declaration");
            assert!(shared.code.contains("import { Model }"));
            assert!(shared.code.contains("./model.js"));
            assert!(
                bundle
                    .files
                    .iter()
                    .any(|file| file.file_name == Path::new("_wake/model.d.ts"))
            );
        }
        assert_eq!(file_system.calls(), calls_after_prepare);
    }

    #[test]
    fn implementation_graph_drops_runtime_resource_edges_but_keeps_type_references() {
        let root = fixture(&[
            (
                "src/index.ts",
                "import './broken.scss';\n\
                 import type {} from './type-augment.js';\n\
                 import logo from './shared.woff2';\n\
                 import { Model } from './model.js';\n\
                 export interface Public { model: Model; }\n\
                 export function renderLogo(): string { return logo; }",
            ),
            ("src/broken.scss", "$broken: true;"),
            ("src/shared.woff2", "not-a-declaration"),
            (
                "src/type-augment.ts",
                "export {}; declare global { interface TypeAugment { value: string; } }",
            ),
            ("src/model.ts", "export interface Model { id: string; }"),
        ]);
        let file_system = CountingDeclarationFileSystem::default();
        let graph = prepare_library_declarations_with_file_system(
            &root,
            [DeclarationEntry::new("resources", "src/index.ts")],
            &file_system,
        )
        .unwrap();

        let inputs = graph.inputs().collect::<Vec<_>>();
        assert_eq!(inputs.len(), 2);
        assert!(inputs.iter().any(|path| path.ends_with("src/index.ts")));
        assert!(inputs.iter().any(|path| path.ends_with("src/model.ts")));
        let calls_after_prepare = file_system.calls();
        assert_eq!(calls_after_prepare.reads.len(), 2);
        assert!(calls_after_prepare.reads.values().all(|count| *count == 1));
        for resource in ["src/broken.scss", "src/shared.woff2", "src/type-augment.ts"] {
            assert!(
                !calls_after_prepare
                    .reads
                    .keys()
                    .any(|path| path.ends_with(resource))
            );
            assert!(
                !calls_after_prepare
                    .canonicalize
                    .iter()
                    .any(|path| path.ends_with(resource))
            );
            assert!(
                !calls_after_prepare
                    .is_file
                    .iter()
                    .any(|path| path.ends_with(resource))
            );
        }

        let bundle = graph.render_entry("resources").unwrap();
        let entry = bundle
            .files
            .iter()
            .find(|file| file.file_name == Path::new("index.d.ts"))
            .unwrap();
        assert!(!entry.code.contains("broken.scss"));
        assert!(!entry.code.contains("type-augment.js"));
        assert!(!entry.code.contains("shared.woff2"));
        assert!(entry.code.contains("import { Model }"));
        assert!(entry.code.contains("model.js"));
        assert!(
            bundle
                .files
                .iter()
                .any(|file| file.source.ends_with("src/model.ts"))
        );
        assert_eq!(file_system.calls(), calls_after_prepare);
    }

    #[test]
    fn generic_type_parameter_shadow_does_not_add_a_declaration_edge() {
        let root = fixture(&[
            (
                "src/index.ts",
                "import { T } from './runtime.js';\n\
                 import { LocalInfer } from './infer-runtime.js';\n\
                 import { LocalKey } from './mapped-runtime.js';\n\
                 import { ParameterValue } from './parameter-runtime.js';\n\
                 import { ArrowSignatureValue } from './arrow-signature-runtime.js';\n\
                 import { AsyncArrowValue } from './async-arrow-runtime.js';\n\
                 import { SingleArrowValue } from './single-arrow-runtime.js';\n\
                 import { AsyncSingleArrowValue } from './async-single-arrow-runtime.js';\n\
                 import type { ForwardType } from './forward-runtime.js';\n\
                 import { type InlineForward } from './inline-forward-runtime.js';\n\
                 export type Box<T> = T;\n\
                 export type Unwrap<Value> = Value extends infer LocalInfer ? LocalInfer : never;\n\
                 export type Mapping<Keys> = { [LocalKey in Keys as LocalKey]: LocalKey };\n\
                 export declare function forward<T extends ForwardType, ForwardType>(): void;\n\
                 export type InlineShadow<InlineForward> = InlineForward;\n\
                 export const callback = (first: typeof ArrowSignatureValue, ArrowSignatureValue: string): typeof ArrowSignatureValue => ArrowSignatureValue;\n\
                 export const asyncCallback = async <T,>(first: typeof AsyncArrowValue, { value: AsyncArrowValue }: { value: string }): Promise<typeof AsyncArrowValue> => AsyncArrowValue;\n\
                 export const single: (value: string) => string = SingleArrowValue => (null as typeof SingleArrowValue);\n\
                 export const asyncSingle: (value: string) => Promise<string> = async AsyncSingleArrowValue => (null as typeof AsyncSingleArrowValue);\n\
                 export declare function inspect(first: typeof ParameterValue, ParameterValue: string): typeof ParameterValue;",
            ),
            ("src/runtime.js", "export const T = run();"),
            ("src/infer-runtime.js", "export const LocalInfer = run();"),
            ("src/mapped-runtime.js", "export const LocalKey = run();"),
            (
                "src/parameter-runtime.js",
                "export const ParameterValue = run();",
            ),
            (
                "src/arrow-signature-runtime.js",
                "export const ArrowSignatureValue = run();",
            ),
            (
                "src/async-arrow-runtime.js",
                "export const AsyncArrowValue = run();",
            ),
            (
                "src/single-arrow-runtime.js",
                "export const SingleArrowValue = run();",
            ),
            (
                "src/async-single-arrow-runtime.js",
                "export const AsyncSingleArrowValue = run();",
            ),
            ("src/forward-runtime.js", "export interface ForwardType {}"),
            (
                "src/inline-forward-runtime.js",
                "export interface InlineForward {}",
            ),
        ]);
        let file_system = CountingDeclarationFileSystem::default();
        let graph = prepare_library_declarations_with_file_system(
            &root,
            [DeclarationEntry::new("shadow", "src/index.ts")],
            &file_system,
        )
        .unwrap();

        assert_eq!(graph.inputs().count(), 1);
        let calls = file_system.calls();
        for shadowed in [
            "runtime.js",
            "infer-runtime.js",
            "mapped-runtime.js",
            "parameter-runtime.js",
            "arrow-signature-runtime.js",
            "async-arrow-runtime.js",
            "single-arrow-runtime.js",
            "async-single-arrow-runtime.js",
            "forward-runtime.js",
            "inline-forward-runtime.js",
        ] {
            assert!(!calls.reads.keys().any(|path| path.ends_with(shadowed)));
            assert!(!calls.is_file.iter().any(|path| path.ends_with(shadowed)));
        }
        let entry = graph
            .render_entry("shadow")
            .unwrap()
            .files
            .into_iter()
            .find(|file| file.file_name == Path::new("index.d.ts"))
            .unwrap();
        assert!(!entry.code.contains("runtime.js"));
        assert!(entry.code.contains("export type Box<T> = T;"));
    }

    #[test]
    fn generic_type_binding_does_not_shadow_typeof_value_dependency() {
        let root = fixture(&[
            (
                "src/index.ts",
                "import { T } from './dep.js'; export type Query<T> = typeof T;",
            ),
            ("src/dep.js", "throw new Error('runtime shadow');"),
            ("src/dep.ts", "export const T = 'token' as const;"),
        ]);
        let file_system = CountingDeclarationFileSystem::default();
        let graph = prepare_library_declarations_with_file_system(
            &root,
            [DeclarationEntry::new("typeof-value", "src/index.ts")],
            &file_system,
        )
        .unwrap();

        let inputs = graph.inputs().collect::<Vec<_>>();
        assert_eq!(inputs.len(), 2);
        assert!(inputs.iter().any(|path| path.ends_with("src/dep.ts")));
        assert!(!inputs.iter().any(|path| path.ends_with("src/dep.js")));
        let calls_after_prepare = file_system.calls();
        assert!(
            !calls_after_prepare
                .reads
                .keys()
                .any(|path| path.ends_with("dep.js"))
        );

        let bundle = graph.render_entry("typeof-value").unwrap();
        let entry = bundle
            .files
            .iter()
            .find(|file| file.file_name == Path::new("index.d.ts"))
            .unwrap();
        assert!(entry.code.contains("typeof T"));
        assert!(entry.code.contains("./_wake/src/dep.js"));
        assert_eq!(file_system.calls(), calls_after_prepare);
    }

    #[test]
    fn type_export_alias_only_follows_the_local_import_binding() {
        let root = fixture(&[
            (
                "src/index.ts",
                "import type { Foo } from './foo.js';\n\
                 import type { Public } from './public.js';\n\
                 export type { Foo as Public };",
            ),
            ("src/foo.ts", "export interface Foo { value: string; }"),
            ("src/public.ts", "export interface Public { wrong: true; }"),
        ]);
        let file_system = CountingDeclarationFileSystem::default();
        let graph = prepare_library_declarations_with_file_system(
            &root,
            [DeclarationEntry::new("type-export", "src/index.ts")],
            &file_system,
        )
        .unwrap();

        let inputs = graph.inputs().collect::<Vec<_>>();
        assert_eq!(inputs.len(), 2);
        assert!(inputs.iter().any(|path| path.ends_with("src/foo.ts")));
        assert!(!inputs.iter().any(|path| path.ends_with("src/public.ts")));
        let calls = file_system.calls();
        assert!(!calls.reads.keys().any(|path| path.ends_with("public.ts")));
        assert!(!calls.is_file.iter().any(|path| path.ends_with("public.ts")));
    }

    #[test]
    fn declaration_side_effect_import_keeps_ts_augmentation_and_drops_resources() {
        let root = fixture(&[
            (
                "types/index.d.ts",
                "import './augment.js';\n\
                 import type {} from './type-augment.js';\n\
                 import './theme.scss';\n\
                 import './missing.scss';\n\
                 export interface Public { value: string; }",
            ),
            (
                "types/augment.d.ts",
                "export interface Augmented { active: true; }",
            ),
            (
                "types/type-augment.d.ts",
                "export {}; declare global { interface WakeGlobal { value: string; } }",
            ),
            ("types/theme.scss", "$theme: true;"),
        ]);
        let file_system = CountingDeclarationFileSystem::default();
        let graph = prepare_library_declarations_with_file_system(
            &root,
            [DeclarationEntry::new("augmentation", "types/index.d.ts")],
            &file_system,
        )
        .unwrap();

        let inputs = graph.inputs().collect::<Vec<_>>();
        assert_eq!(inputs.len(), 3);
        assert!(inputs.iter().any(|path| path.ends_with("types/index.d.ts")));
        assert!(
            inputs
                .iter()
                .any(|path| path.ends_with("types/augment.d.ts"))
        );
        assert!(
            inputs
                .iter()
                .any(|path| path.ends_with("types/type-augment.d.ts"))
        );
        let calls_after_prepare = file_system.calls();
        assert_eq!(calls_after_prepare.reads.len(), 3);
        assert!(calls_after_prepare.reads.values().all(|count| *count == 1));
        assert!(
            !calls_after_prepare
                .reads
                .keys()
                .any(|path| path.ends_with("types/theme.scss"))
        );

        let bundle = graph.render_entry("augmentation").unwrap();
        let entry = bundle
            .files
            .iter()
            .find(|file| file.file_name == Path::new("index.d.ts"))
            .unwrap();
        assert!(entry.code.contains("augment"), "{}", entry.code);
        assert!(entry.code.contains("type-augment"), "{}", entry.code);
        assert!(!entry.code.contains("theme.scss"));
        assert!(!entry.code.contains("missing.scss"));
        assert!(
            bundle
                .files
                .iter()
                .any(|file| file.source.ends_with("types/augment.d.ts"))
        );
        assert_eq!(file_system.calls(), calls_after_prepare);
    }

    #[test]
    fn declaration_graph_resolves_node_next_runtime_extensions_before_legacy_fallbacks() {
        let root = fixture(&[
            (
                "src/index.ts",
                "export type { EsmSource } from './esm.mjs';\n\
                 export type { EsmDeclaration } from './esm-declaration.mjs';\n\
                 export type { CommonSource } from './common.cjs';\n\
                 export type { CommonDeclaration } from './common-declaration.cjs';\n\
                 export type { LegacyEsm } from './legacy-esm.mjs';\n\
                 export type { LegacyCommon } from './legacy-common.cjs';\n\
                 export type { LegacyJs } from './legacy-js.js';",
            ),
            ("src/esm.mts", "export interface EsmSource { esm: true; }"),
            ("src/esm.mjs", "throw new Error('runtime only');"),
            (
                "src/esm.ts",
                "export interface EsmFallback { wrong: true; }",
            ),
            (
                "src/esm-declaration.d.mts",
                "export interface EsmDeclaration { declaration: true; }",
            ),
            ("src/esm-declaration.mjs", "export const runtime = true;"),
            (
                "src/esm-declaration.d.ts",
                "export interface EsmDeclarationFallback { wrong: true; }",
            ),
            (
                "src/common.cts",
                "export interface CommonSource { common: true; }",
            ),
            ("src/common.cjs", "throw new Error('runtime only');"),
            (
                "src/common.ts",
                "export interface CommonFallback { wrong: true; }",
            ),
            (
                "src/common-declaration.d.cts",
                "export interface CommonDeclaration { declaration: true; }",
            ),
            (
                "src/common-declaration.cjs",
                "module.exports = { runtime: true };",
            ),
            (
                "src/common-declaration.d.ts",
                "export interface CommonDeclarationFallback { wrong: true; }",
            ),
            (
                "src/legacy-esm.ts",
                "export interface LegacyEsm { compatible: true; }",
            ),
            (
                "src/legacy-esm.mjs",
                "throw new Error('runtime literal must not shadow TypeScript');",
            ),
            (
                "src/legacy-common.ts",
                "export interface LegacyCommon { compatible: true; }",
            ),
            (
                "src/legacy-common.cjs",
                "throw new Error('runtime literal must not shadow TypeScript');",
            ),
            (
                "src/legacy-js.ts",
                "export interface LegacyJs { compatible: true; }",
            ),
            (
                "src/legacy-js.js",
                "throw new Error('runtime literal must not shadow TypeScript');",
            ),
        ]);
        let file_system = CountingDeclarationFileSystem::default();
        let graph = prepare_library_declarations_with_file_system(
            &root,
            [DeclarationEntry::new("node-next", "src/index.ts")],
            &file_system,
        )
        .unwrap();

        let inputs = graph.inputs().collect::<Vec<_>>();
        for expected in [
            "src/index.ts",
            "src/esm.mts",
            "src/esm-declaration.d.mts",
            "src/common.cts",
            "src/common-declaration.d.cts",
            "src/legacy-esm.ts",
            "src/legacy-common.ts",
            "src/legacy-js.ts",
        ] {
            assert!(
                inputs.iter().any(|path| path.ends_with(expected)),
                "missing declaration input {expected}: {inputs:?}"
            );
        }
        for shadowed in [
            "src/esm.mjs",
            "src/esm.ts",
            "src/esm-declaration.mjs",
            "src/esm-declaration.d.ts",
            "src/common.cjs",
            "src/common.ts",
            "src/common-declaration.cjs",
            "src/common-declaration.d.ts",
            "src/legacy-esm.mjs",
            "src/legacy-common.cjs",
            "src/legacy-js.js",
        ] {
            assert!(
                !inputs.iter().any(|path| path.ends_with(shadowed)),
                "selected shadowed declaration input {shadowed}: {inputs:?}"
            );
        }
        let calls_after_prepare = file_system.calls();
        assert!(calls_after_prepare.reads.values().all(|count| *count == 1));
        assert_eq!(calls_after_prepare.reads.len(), inputs.len());
        let bundle = graph.render_entry("node-next").unwrap();
        assert_eq!(bundle.files.len(), inputs.len());
        assert_eq!(file_system.calls(), calls_after_prepare);
    }

    #[test]
    fn rewrites_only_parser_registered_request_ranges_and_rerenders_without_reads() {
        let root = fixture(&[
            (
                "src/index.ts",
                r#"
                    export interface Exact {
                        literal: "./dep.js";
                        // keep "./dep.js" unchanged
                        nested: import("./dep.js").Value;
                    }
                "#,
            ),
            ("src/dep.ts", "export interface Value { ok: true; }"),
        ]);
        let file_system = CountingDeclarationFileSystem::default();
        let graph = prepare_library_declarations_with_file_system(
            &root,
            [DeclarationEntry::new("exact", "src/index.ts")],
            &file_system,
        )
        .unwrap();
        let calls_after_prepare = file_system.calls();

        let default = graph.render_entry("exact").unwrap();
        let entry = default
            .files
            .iter()
            .find(|file| file.file_name == Path::new("index.d.ts"))
            .unwrap();
        assert!(entry.code.contains("literal: \"./dep.js\""));
        assert!(entry.code.contains("// keep \"./dep.js\" unchanged"));
        assert!(entry.code.contains("import(\"./_wake/src/dep.js\").Value"));

        let first = graph
            .render_entry_with("exact", |request| {
                request
                    .resolved_source
                    .is_some()
                    .then(|| "virtual:first".to_string())
            })
            .unwrap();
        let second = graph
            .render_entry_with("exact", |request| {
                request
                    .resolved_source
                    .is_some()
                    .then(|| "virtual:second".to_string())
            })
            .unwrap();
        let first_entry = first
            .files
            .iter()
            .find(|file| file.file_name == Path::new("index.d.ts"))
            .unwrap();
        let second_entry = second
            .files
            .iter()
            .find(|file| file.file_name == Path::new("index.d.ts"))
            .unwrap();
        assert!(first_entry.code.contains("virtual:first"));
        assert!(!first_entry.code.contains("virtual:second"));
        assert!(second_entry.code.contains("virtual:second"));
        assert_eq!(file_system.calls(), calls_after_prepare);
    }

    #[test]
    fn quoted_request_rewrite_is_escaped_after_unicode_prefixes() {
        let root = fixture(&[
            (
                "src/index.ts",
                "export interface Cafe { snow: '雪原文'; value: import('./dep.js').Value; }",
            ),
            ("src/dep.ts", "export interface Value { ok: true; }"),
        ]);
        let graph =
            prepare_library_declarations(&root, [DeclarationEntry::new("unicode", "src/index.ts")])
                .unwrap();

        let bundle = graph
            .render_entry_with("unicode", |request| {
                request
                    .resolved_source
                    .is_some()
                    .then(|| "@scope/雪'\\module".to_string())
            })
            .unwrap();
        let entry = bundle
            .files
            .iter()
            .find(|file| file.file_name == Path::new("index.d.ts"))
            .unwrap();
        assert!(entry.code.contains("Cafe"));
        assert!(entry.code.contains("snow: '雪原文'"));
        assert!(
            entry
                .code
                .contains("import('@scope/雪\\'\\\\module').Value")
        );
    }

    #[test]
    fn ambient_render_uses_parser_proven_modifier_free_templates() {
        let root = fixture(&[
            (
                "src/index.ts",
                "export type { Shared } from './dependency.js';\n\
                 export declare function exported(value: string): void;\n\
                 export declare class Public { method(): void; }\n\
                 export declare const value: string;",
            ),
            (
                "src/dependency.ts",
                "declare function helper(value: string): void;\n\
                 export interface Shared { value: string; }",
            ),
        ]);
        let graph =
            prepare_library_declarations(&root, [DeclarationEntry::new("ambient", "src/index.ts")])
                .unwrap();
        let bundle = graph.render_entry_ambient("ambient").unwrap();

        let entry = bundle
            .files
            .iter()
            .find(|file| file.file_name == Path::new("index.d.ts"))
            .unwrap();
        assert!(
            entry.code.contains("export function exported"),
            "{}",
            entry.code
        );
        assert!(entry.code.contains("export class Public"));
        assert!(entry.code.contains("export const value"));
        assert!(!entry.code.contains("export declare"));

        let dependency = bundle
            .files
            .iter()
            .find(|file| file.file_name == Path::new("_wake/src/dependency.d.ts"))
            .unwrap();
        assert!(dependency.code.contains("function helper"));
        assert!(!dependency.code.contains("declare function helper"));

        for file in &bundle.files {
            validate_ambient_declaration_body(&file.file_name, &file.code).unwrap();
            let wrapped = format!("declare module \"ambient-test\" {{\n{}\n}}", file.code);
            validate_declaration_body(Path::new("wrapped.d.ts"), &wrapped).unwrap();
        }
        let error = validate_ambient_declaration_body(
            Path::new("invalid.d.ts"),
            "export declare const invalid: string;",
        )
        .unwrap_err();
        assert!(error.to_string().contains("redundant `declare` modifier"));
    }

    #[test]
    fn strict_body_validation_returns_any_as_a_typed_policy_fact() {
        let facts = validate_declaration_body(
            Path::new("remote.d.ts"),
            "export interface Public { value: any; }",
        )
        .unwrap();
        assert!(facts.contains_forbidden_any());

        let facts = validate_ambient_declaration_body(
            Path::new("remote.d.ts"),
            "export interface Public { value: any; }",
        )
        .unwrap();
        assert!(facts.contains_forbidden_any());
        assert!(
            validate_declaration_body(
                Path::new("remote.d.ts"),
                "export const invalid: string = run();",
            )
            .is_err()
        );
        for invalid in [
            "export type InvalidConst<const T> = T;",
            "export type OptionalIndex = { [key: string]?: boolean };",
            "export type ParameterProperty = (public value: string) => void;",
            "export type InvalidReadonlyRemoval = { -readonly value: string };",
            "export interface InvalidReadonlyAddition { +readonly value: string; }",
            "type Keys = 'key'; export type MixedMapped<T> = { [Key in Keys]: T; extra: string };",
        ] {
            assert!(
                validate_declaration_body(Path::new("remote.d.ts"), invalid).is_err(),
                "invalid strict declaration was accepted: {invalid}"
            );
        }
        validate_declaration_body(
            Path::new("remote.d.ts"),
            "export interface ReadonlyNames { readonly?: string; readonly?(): void; }",
        )
        .unwrap();
    }

    #[test]
    fn same_named_entries_keep_distinct_index_ownership() {
        let root = fixture(&[
            ("one/index.ts", "export interface Public { one: true; }"),
            ("two/index.ts", "export interface Public { two: true; }"),
        ]);
        let graph = prepare_library_declarations(
            &root,
            [
                DeclarationEntry::new("expose-one", "one/index.ts"),
                DeclarationEntry::new("expose-two", "two/index.ts"),
            ],
        )
        .unwrap();

        let one = graph.render_entry("expose-one").unwrap();
        let two = graph.render_entry("expose-two").unwrap();
        assert_eq!(one.files.len(), 1);
        assert_eq!(two.files.len(), 1);
        assert_eq!(one.files[0].file_name, Path::new("index.d.ts"));
        assert_eq!(two.files[0].file_name, Path::new("index.d.ts"));
        assert_ne!(one.source, two.source);
        assert!(one.files[0].code.contains("one: true"));
        assert!(two.files[0].code.contains("two: true"));
    }

    #[test]
    fn graph_renders_same_line_overloads_and_anonymous_default_class() {
        let root = fixture(&[(
            "src/index.ts",
            "export interface A { a: string } export type B = A & { b: number };\n\
             export function overload(value: string): string;\n\
             export function overload(value: number): number;\n\
             export function overload(value: string | number): string | number { return value; }\n\
             export default class { readonly value: string = ''; method(input: B): A { return input; } }",
        )]);
        let files = emit_library_declarations(&root, "src/index.ts").unwrap();
        let code = &files
            .iter()
            .find(|file| file.file_name == Path::new("index.d.ts"))
            .unwrap()
            .code;

        assert!(code.contains("export interface A { a: string }"));
        assert!(code.contains("export type B = A & { b: number };"));
        assert_eq!(code.matches("function overload").count(), 2);
        assert!(code.contains("export default class"));
        assert!(code.contains("readonly value: string"));
        assert!(code.contains("method(input: B): A"));
        assert!(!code.contains("return value"));
        assert!(!code.contains("return input"));
    }

    #[test]
    fn frozen_graph_preserves_valid_export_assignment_and_rejects_expressions() {
        let root = fixture(&[
            (
                "src/index.ts",
                "const api = { version: '1' }; export = api;",
            ),
            ("src/invalid.ts", "export = createApi();"),
        ]);
        let graph = prepare_library_declarations(
            &root,
            [DeclarationEntry::new("commonjs", "src/index.ts")],
        )
        .unwrap();

        let standalone = graph.render_entry("commonjs").unwrap();
        let standalone = standalone
            .files
            .iter()
            .find(|file| file.file_name == Path::new("index.d.ts"))
            .unwrap();
        assert!(standalone.code.contains("declare const api:"));
        assert!(standalone.code.contains("export = api;"));

        let ambient = graph.render_entry_ambient("commonjs").unwrap();
        let ambient = ambient
            .files
            .iter()
            .find(|file| file.file_name == Path::new("index.d.ts"))
            .unwrap();
        assert!(ambient.code.contains("const api:"));
        assert!(!ambient.code.contains("declare const api:"));
        assert!(ambient.code.contains("export = api;"));
        validate_ambient_declaration_body(&ambient.file_name, &ambient.code).unwrap();

        let error = prepare_library_declarations(
            &root,
            [DeclarationEntry::new("invalid", "src/invalid.ts")],
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("export assignment requires an identifier or dotted name")
        );
    }

    #[test]
    fn emits_preserved_library_declarations_without_any() {
        let root = fixture(&[
            ("package.json", r#"{"name":"@demo/button","type":"module"}"#),
            (
                "src/index.ts",
                r#"
                    import Button from "./button.js";
                    export type { ButtonProps } from "./button.js";
                    export { default as ButtonGroup } from "./group.js";
                    export type { Shader } from "./shaders/flat.vert.js";
                    export default Button;
                "#,
            ),
            (
                "src/shaders/flat.vert.ts",
                "export interface Shader { source: string; }",
            ),
            (
                "src/group.tsx",
                r#"
                    import { internal } from './internal.js';
                    export interface GroupProps { children: string; }
                    function ButtonGroup(props: GroupProps) { return <div>{internal}{props.children}</div>; }
                    export default ButtonGroup;
                "#,
            ),
            ("src/internal.ts", "export const internal = 42;"),
            (
                "src/button.tsx",
                r#"
                    import type { FC, ReactNode } from "react";
                    export interface ButtonProps { label: ReactNode; }
                    export const Icon = ({ title }: { title: string }) => <span>{title}</span>;
                    export const vars = { 'color': '--button-color', nested: { gap: '4px' } };
                    export const EPS = 1e-6;
                    export const ORDER = ['a', 'b'] as const;
                    export const alias = vars;
                    export const sizeOf = (value: number): string => String(value);
                    const Button: FC<ButtonProps> = (props) => <button>{props.label}</button>;
                    export default Button;
                "#,
            ),
        ]);
        let files = emit_library_declarations(&root, "src/index.ts").unwrap();
        assert_eq!(files.len(), 4);
        let entry = files
            .iter()
            .find(|file| file.file_name == Path::new("index.d.ts"))
            .unwrap();
        assert!(entry.code.contains("./_wake/src/button.js"));
        let button = files
            .iter()
            .find(|file| file.file_name == Path::new("_wake/src/button.d.ts"))
            .unwrap();
        assert!(button.code.contains("export interface ButtonProps"));
        assert!(
            button
                .code
                .contains("declare const Button: FC<ButtonProps>;")
        );
        assert!(button.code.contains(
            "export declare const Icon: ({ title }: { title: string }) => import(\"react\").JSX.Element;"
        ));
        assert!(button.code.contains("export declare const vars: { readonly 'color': string; readonly nested: { readonly gap: string; }; };"));
        assert!(button.code.contains("export declare const EPS: 1e-6;"));
        assert!(
            button
                .code
                .contains("export declare const ORDER: readonly ['a', 'b'];")
        );
        assert!(
            button
                .code
                .contains("export declare const alias: typeof vars;")
        );
        assert!(
            button
                .code
                .contains("export declare const sizeOf: (value: number) => string;")
        );
        assert!(!button.code.contains("any"));
        let group = files
            .iter()
            .find(|file| file.file_name == Path::new("_wake/src/group.d.ts"))
            .unwrap();
        assert!(group.code.contains(
            "declare function ButtonGroup(props: GroupProps): import(\"react\").JSX.Element;"
        ));
        assert!(!group.code.contains("./internal.js"));
        assert!(
            !files
                .iter()
                .any(|file| file.file_name == Path::new("_wake/src/internal.d.ts"))
        );
    }

    #[test]
    fn declarations_reject_untyped_public_values() {
        let root = fixture(&[
            ("package.json", r#"{"name":"demo"}"#),
            ("src/index.ts", "export const answer = compute();"),
        ]);
        let error = emit_library_declarations(&root, "src/index.ts").unwrap_err();
        assert!(error.to_string().contains("explicit type annotation"));
    }
}
