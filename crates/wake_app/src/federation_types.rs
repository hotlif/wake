//! Deterministic, fail-closed declaration packaging for Wake Federation remotes.
//!
//! This module deliberately builds on `wake_tsdoc` instead of introducing a second declaration
//! emitter. The JSON bundle is the transport artifact; `ambient_declaration` is the exact file a
//! host can publish at `.wake/federation/types/<remote>/<buildId>/index.d.ts`.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use wake_common::FileSystem;
use wake_federation_contract::{BuildId, ContainerName, ExposeKey};

pub(super) const FEDERATION_TYPE_BUNDLE_SCHEMA_VERSION: &str = "wake.federation.types.v1";

/// Transport and editor-facing forms of one build-bound remote declaration package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FederationTypeOutput {
    /// Deterministic JSON bytes published as the manifest's declaration bundle.
    pub bundle_json: Vec<u8>,
    /// Ambient modules ready for the host's build-scoped `index.d.ts`.
    pub ambient_declaration: String,
}

/// One source-complete declaration generation.
///
/// The graph owns every parsed declaration fact. `identity_bytes` contains no build id, and
/// [`Self::bind`] performs no filesystem access, parsing, or whole-text replacement.
#[derive(Debug, Clone)]
pub(super) struct FrozenFederationTypes {
    container: ContainerName,
    graph: wake_tsdoc::FrozenDeclarationGraph,
    public_specifiers: BTreeMap<String, String>,
    identity_bytes: Vec<u8>,
}

impl FrozenFederationTypes {
    pub(super) fn identity_bytes(&self) -> &[u8] {
        &self.identity_bytes
    }

    pub(super) fn bind(
        &self,
        build_id: &BuildId,
    ) -> Result<FederationTypeOutput, FederationTypeError> {
        validate_build_id(build_id)?;
        let modules = self.render_modules(ModuleBinding::Build(build_id))?;
        let bundle = DeclarationBundle {
            schema_version: FEDERATION_TYPE_BUNDLE_SCHEMA_VERSION.to_owned(),
            name: self.container.as_str().to_owned(),
            build_id: build_id.as_str().to_owned(),
            exposes: self.public_specifiers.clone(),
            modules,
        };
        let ambient_declaration =
            render_ambient_declaration(&self.container, build_id, &bundle.modules);
        let mut bundle_json = serde_json::to_vec_pretty(&bundle).map_err(|error| {
            FederationTypeError::new(format!(
                "cannot serialize federation declaration bundle: {error}"
            ))
        })?;
        bundle_json.push(b'\n');
        Ok(FederationTypeOutput {
            bundle_json,
            ambient_declaration,
        })
    }

    fn render_modules(
        &self,
        binding: ModuleBinding<'_>,
    ) -> Result<BTreeMap<String, String>, FederationTypeError> {
        let source_specifiers = self
            .graph
            .inputs()
            .map(|source| {
                source_module_specifier_for_binding(
                    self.graph.root(),
                    &self.container,
                    binding,
                    source,
                )
                .map(|specifier| (source.to_path_buf(), specifier))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let owner_sources = self
            .graph
            .entries()
            .map(|entry| (entry.owner.clone(), entry.source.clone()))
            .collect::<BTreeMap<_, _>>();

        let bundles = self.graph.render_ambient_with(|request| {
            let source = request.resolved_source?;
            if owner_sources
                .get(request.owner)
                .is_some_and(|entry| entry == source)
            {
                return self.public_specifiers.get(request.owner).cloned();
            }
            source_specifiers.get(source).cloned()
        });

        let mut modules = BTreeMap::new();
        for bundle in bundles {
            let public_specifier = self.public_specifiers.get(&bundle.owner).ok_or_else(|| {
                FederationTypeError::new(format!(
                    "declaration graph returned unknown owner `{}`",
                    bundle.owner
                ))
            })?;
            let mut found_entry = false;
            for file in bundle.files {
                let specifier = if file.source == bundle.source {
                    found_entry = true;
                    public_specifier.clone()
                } else {
                    source_specifiers
                        .get(&file.source)
                        .cloned()
                        .ok_or_else(|| {
                            FederationTypeError::new(format!(
                                "declaration graph returned unknown source `{}`",
                                file.source.display()
                            ))
                        })?
                };
                insert_deduplicated_module(&mut modules, specifier, file.code)?;
            }
            if !found_entry {
                return Err(FederationTypeError::new(format!(
                    "declaration graph did not render its entry for `{public_specifier}`"
                )));
            }
        }
        Ok(modules)
    }
}

#[derive(Clone, Copy)]
enum ModuleBinding<'a> {
    Identity,
    Build(&'a BuildId),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FederationTypeIdentity<'a> {
    schema_version: &'static str,
    name: &'a str,
    exposes: &'a BTreeMap<String, String>,
    modules: BTreeMap<String, String>,
}

const FEDERATION_TYPE_IDENTITY_SCHEMA_VERSION: &str = "wake.federation.types.identity.v1";

/// Adapter that keeps declaration discovery and reads on the build generation's filesystem view.
pub(super) struct GenerationDeclarationFileSystem {
    file_system: Arc<dyn FileSystem>,
}

impl GenerationDeclarationFileSystem {
    pub(super) fn new(file_system: Arc<dyn FileSystem>) -> Self {
        Self { file_system }
    }
}

impl wake_tsdoc::DeclarationFileSystem for GenerationDeclarationFileSystem {
    fn canonicalize(&self, path: &Path) -> Result<PathBuf, String> {
        self.file_system
            .canonicalize(path)
            .map_err(|error| error.to_string())
    }

    fn is_file(&self, path: &Path) -> bool {
        self.file_system.is_file(path)
    }

    fn read_to_string(&self, path: &Path) -> Result<String, String> {
        self.file_system
            .read_to_string(path)
            .map_err(|error| error.to_string())
    }
}

/// A declaration failure is always fatal. Wake never substitutes `any` or keeps a partial bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FederationTypeError {
    message: String,
}

impl FederationTypeError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for FederationTypeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for FederationTypeError {}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct DeclarationBundle {
    pub(super) schema_version: String,
    pub(super) name: String,
    pub(super) build_id: String,
    pub(super) exposes: BTreeMap<String, String>,
    pub(super) modules: BTreeMap<String, String>,
}

/// Read and parse all exposed declaration closures exactly once.
#[cfg(test)]
pub(super) fn prepare_federation_types(
    project_root: &Path,
    container: &ContainerName,
    exposes: &[(ExposeKey, PathBuf)],
) -> Result<FrozenFederationTypes, FederationTypeError> {
    prepare_federation_types_with_file_system(
        project_root,
        container,
        exposes,
        &wake_tsdoc::OsDeclarationFileSystem,
    )
}

/// Freeze Federation declarations against an explicit product-generation filesystem.
pub(super) fn prepare_federation_types_with_file_system(
    project_root: &Path,
    container: &ContainerName,
    exposes: &[(ExposeKey, PathBuf)],
    file_system: &(impl wake_tsdoc::DeclarationFileSystem + ?Sized),
) -> Result<FrozenFederationTypes, FederationTypeError> {
    validate_container(container)?;
    if exposes.is_empty() {
        return Err(FederationTypeError::new(
            "federation declaration package requires at least one expose",
        ));
    }

    let mut ordered = exposes.iter().collect::<Vec<_>>();
    ordered.sort_by(|(left_key, left_entry), (right_key, right_entry)| {
        left_key
            .cmp(right_key)
            .then_with(|| left_entry.cmp(right_entry))
    });

    let mut seen_exposes = BTreeSet::new();
    let mut public_specifiers = BTreeMap::new();
    let mut declaration_entries = Vec::with_capacity(ordered.len());
    for (expose, entry) in ordered {
        validate_expose(expose)?;
        if !seen_exposes.insert(expose.clone()) {
            return Err(FederationTypeError::new(format!(
                "duplicate federation declaration expose `{expose}`"
            )));
        }

        let public_specifier = public_module_specifier(container, expose);
        public_specifiers.insert(expose.as_str().to_owned(), public_specifier);
        declaration_entries.push(wake_tsdoc::DeclarationEntry::new(
            expose.as_str(),
            entry.clone(),
        ));
    }

    let graph = wake_tsdoc::prepare_library_declarations_with_file_system(
        project_root,
        declaration_entries,
        file_system,
    )
    .map_err(|error| {
        FederationTypeError::new(format!("cannot prepare federation declarations: {error}"))
    })?;
    let mut generation = FrozenFederationTypes {
        container: container.clone(),
        graph,
        public_specifiers,
        identity_bytes: Vec::new(),
    };
    let identity = FederationTypeIdentity {
        schema_version: FEDERATION_TYPE_IDENTITY_SCHEMA_VERSION,
        name: generation.container.as_str(),
        exposes: &generation.public_specifiers,
        modules: generation.render_modules(ModuleBinding::Identity)?,
    };
    let mut identity_bytes = serde_json::to_vec(&identity).map_err(|error| {
        FederationTypeError::new(format!(
            "cannot serialize federation declaration identity: {error}"
        ))
    })?;
    identity_bytes.push(b'\n');
    generation.identity_bytes = identity_bytes;
    Ok(generation)
}

fn validate_container(container: &ContainerName) -> Result<(), FederationTypeError> {
    let mut characters = container.as_str().chars();
    let valid_container = characters
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic())
        && container.as_str().len() <= 64
        && characters
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'));
    if !valid_container {
        return Err(FederationTypeError::new(format!(
            "invalid federation declaration container `{container}`"
        )));
    }
    Ok(())
}

fn validate_build_id(build_id: &BuildId) -> Result<(), FederationTypeError> {
    let valid_build_id = !build_id.as_str().is_empty()
        && build_id.as_str().len() <= 256
        && build_id.as_str().chars().all(|character| {
            character.is_ascii() && !character.is_whitespace() && character != '\\'
        });
    if !valid_build_id {
        return Err(FederationTypeError::new(format!(
            "invalid federation declaration build id `{build_id}`"
        )));
    }
    Ok(())
}

fn validate_expose(expose: &ExposeKey) -> Result<(), FederationTypeError> {
    let Some(path) = expose.as_str().strip_prefix("./") else {
        return Err(FederationTypeError::new(format!(
            "federation declaration expose `{expose}` must start with `./`"
        )));
    };
    let valid = !path.is_empty()
        && expose.as_str().len() <= 256
        && !path.contains(['\\', '?', '#'])
        && path.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '/' | '@' | '_' | '-' | '.')
        })
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && !matches!(segment, "." | ".."));
    if !valid {
        return Err(FederationTypeError::new(format!(
            "invalid federation declaration expose `{expose}`"
        )));
    }
    Ok(())
}

fn public_module_specifier(container: &ContainerName, expose: &ExposeKey) -> String {
    format!(
        "{}/{}",
        container.as_str(),
        expose.as_str().trim_start_matches("./")
    )
}

fn declaration_output_path(path: &Path) -> Result<String, FederationTypeError> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_str().ok_or_else(|| {
                FederationTypeError::new(format!(
                    "declaration output path `{}` is not valid Unicode",
                    path.display()
                ))
            })?),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(FederationTypeError::new(format!(
                    "declaration output path `{}` is not project relative",
                    path.display()
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(FederationTypeError::new("declaration output path is empty"));
    }
    Ok(parts.join("/"))
}

fn source_module_specifier_for_binding(
    root: &Path,
    container: &ContainerName,
    binding: ModuleBinding<'_>,
    source: &Path,
) -> Result<String, FederationTypeError> {
    let relative = source.strip_prefix(root).map_err(|_| {
        FederationTypeError::new(format!(
            "declaration source `{}` escapes project root `{}`",
            source.display(),
            root.display()
        ))
    })?;
    let relative = declaration_output_path(relative)?;
    let namespace = match binding {
        ModuleBinding::Identity => identity_source_module_namespace(container),
        ModuleBinding::Build(build_id) => source_module_namespace(container, build_id),
    };
    Ok(format!("{namespace}{}", percent_encode(&relative, true)))
}

fn identity_source_module_namespace(container: &ContainerName) -> String {
    format!(
        "wake-federation-identity:{}/source/",
        percent_encode(container.as_str(), false),
    )
}

pub(super) fn source_module_namespace(container: &ContainerName, build_id: &BuildId) -> String {
    format!(
        "wake-federation:{}@{}/source/",
        percent_encode(container.as_str(), false),
        percent_encode(build_id.as_str(), false),
    )
}

fn percent_encode(value: &str, preserve_slashes: bool) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || (preserve_slashes && byte == b'/')
        {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
    }
    encoded
}

fn insert_deduplicated_module(
    modules: &mut BTreeMap<String, String>,
    specifier: String,
    code: String,
) -> Result<(), FederationTypeError> {
    if let Some(previous) = modules.get(&specifier) {
        if previous != &code {
            return Err(FederationTypeError::new(format!(
                "declaration module `{specifier}` differs between federation exposes"
            )));
        }
        return Ok(());
    }
    modules.insert(specifier, code);
    Ok(())
}

pub(super) fn render_ambient_declaration(
    container: &ContainerName,
    build_id: &BuildId,
    modules: &BTreeMap<String, String>,
) -> String {
    let mut output = format!(
        "// Generated by Wake Federation for {}@{}; do not edit.\n",
        container.as_str(),
        build_id.as_str()
    );
    for (specifier, code) in modules {
        let quoted = serde_json::to_string(specifier).expect("module specifier is serializable");
        output.push_str("declare module ");
        output.push_str(&quoted);
        output.push_str(" {\n");
        for line in code.lines() {
            if !line.is_empty() {
                output.push_str("  ");
                output.push_str(line);
            }
            output.push('\n');
        }
        output.push_str("}\n");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct CountingDeclarationFileSystem {
        reads: Mutex<BTreeMap<PathBuf, usize>>,
    }

    impl wake_tsdoc::DeclarationFileSystem for CountingDeclarationFileSystem {
        fn canonicalize(&self, path: &Path) -> Result<PathBuf, String> {
            std::fs::canonicalize(path).map_err(|error| error.to_string())
        }

        fn is_file(&self, path: &Path) -> bool {
            path.is_file()
        }

        fn read_to_string(&self, path: &Path) -> Result<String, String> {
            *self
                .reads
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .entry(path.to_path_buf())
                .or_default() += 1;
            std::fs::read_to_string(path).map_err(|error| error.to_string())
        }
    }

    fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let path = directory.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, contents).unwrap();
        }
        directory
    }

    fn exposed(entries: &[(&str, &str)]) -> Vec<(ExposeKey, PathBuf)> {
        entries
            .iter()
            .map(|(key, entry)| (ExposeKey::new(*key), PathBuf::from(entry)))
            .collect()
    }

    #[test]
    fn multiple_exposes_deduplicate_dependencies_and_use_stable_specifiers() {
        let project = fixture(&[
            (
                "src/shared.ts",
                "export interface SharedProps { label: string; }",
            ),
            (
                "src/button.ts",
                "export type { SharedProps } from './shared.js';\nexport interface ButtonProps { disabled: boolean; }",
            ),
            (
                "src/card.ts",
                "export type { SharedProps } from './shared.js';\nexport interface CardProps { raised: boolean; }",
            ),
        ]);
        let prepared = prepare_federation_types(
            project.path(),
            &ContainerName::new("catalog"),
            &exposed(&[("./Button", "src/button.ts"), ("./Card", "src/card.ts")]),
        )
        .unwrap();
        let result = prepared.bind(&BuildId::new("build-a")).unwrap();

        assert!(
            result
                .ambient_declaration
                .contains("declare module \"catalog/Button\"")
        );
        assert!(
            result
                .ambient_declaration
                .contains("from 'wake-federation:catalog@build-a/source/src/shared.ts'")
        );
        assert_eq!(
            result
                .ambient_declaration
                .matches("declare module \"wake-federation:catalog@build-a/source/src/shared.ts\"")
                .count(),
            1
        );
        assert!(!result.ambient_declaration.contains("from './"));

        let json: serde_json::Value = serde_json::from_slice(&result.bundle_json).unwrap();
        assert_eq!(json["modules"].as_object().unwrap().len(), 3);
        assert_eq!(json["schemaVersion"], FEDERATION_TYPE_BUNDLE_SCHEMA_VERSION);
        assert_eq!(json["exposes"]["./Button"], "catalog/Button");
        assert_eq!(json["exposes"]["./Card"], "catalog/Card");
    }

    #[test]
    fn output_is_deterministic_when_expose_order_changes() {
        let project = fixture(&[
            ("src/a.ts", "export interface A { value: string; }"),
            ("src/b.ts", "export interface B { value: number; }"),
        ]);
        let forward = prepare_federation_types(
            project.path(),
            &ContainerName::new("catalog"),
            &exposed(&[("./A", "src/a.ts"), ("./B", "src/b.ts")]),
        )
        .unwrap();
        let reverse = prepare_federation_types(
            project.path(),
            &ContainerName::new("catalog"),
            &exposed(&[("./B", "src/b.ts"), ("./A", "src/a.ts")]),
        )
        .unwrap();
        let build_id = BuildId::new("build-a");
        let forward_output = forward.bind(&build_id).unwrap();
        let reverse_output = reverse.bind(&build_id).unwrap();

        assert_eq!(forward.identity_bytes(), reverse.identity_bytes());
        assert_eq!(forward_output, reverse_output);
    }

    #[test]
    fn multi_expose_generation_reads_shared_sources_once_and_binding_is_pure() {
        let project = fixture(&[
            (
                "src/shared.ts",
                "export interface Shared { value: string; }",
            ),
            (
                "src/a.ts",
                "export type { Shared } from './shared.js'; export interface A { ok: true; }",
            ),
            (
                "src/b.ts",
                "export type { Shared } from './shared.js'; export interface B { ok: true; }",
            ),
        ]);
        let file_system = CountingDeclarationFileSystem::default();
        let prepared = prepare_federation_types_with_file_system(
            project.path(),
            &ContainerName::new("catalog"),
            &exposed(&[("./A", "src/a.ts"), ("./B", "src/b.ts")]),
            &file_system,
        )
        .unwrap();
        let reads_after_prepare = file_system
            .reads
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();

        assert_eq!(reads_after_prepare.len(), 3);
        assert!(reads_after_prepare.values().all(|reads| *reads == 1));
        prepared.bind(&BuildId::new("build-one")).unwrap();
        prepared.bind(&BuildId::new("build-two")).unwrap();
        assert_eq!(
            *file_system
                .reads
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            reads_after_prepare
        );
    }

    #[test]
    fn generation_overlay_only_declaration_dependency_is_frozen_from_logical_bytes() {
        let project = fixture(&[(
            "src/index.ts",
            "export type { Shared } from '../.wake/generated/shared.js';\n\
             export interface Public { shared: Shared; }",
        )]);
        let logical_dependency = project.path().join(".wake/generated/shared.ts");
        assert!(!logical_dependency.exists());

        let mut generated = wake_common::OwnedFileTreeBuilder::new();
        generated
            .insert(
                wake_common::ProjectedRelativePath::new("generated/shared.ts").unwrap(),
                &b"export interface Shared { overlay: true; }"[..],
            )
            .unwrap();
        let generation: Arc<dyn FileSystem> = Arc::new(
            wake_common::OwnedOverlayFileSystem::try_new(
                Arc::new(wake_common::OsFileSystem),
                project.path().join(".wake"),
                generated.seal(),
            )
            .unwrap(),
        );
        let file_system = GenerationDeclarationFileSystem::new(generation);

        let prepared = prepare_federation_types_with_file_system(
            project.path(),
            &ContainerName::new("catalog"),
            &exposed(&[("./Public", "src/index.ts")]),
            &file_system,
        )
        .unwrap();
        let output = prepared.bind(&BuildId::new("build-overlay")).unwrap();
        let bundle = String::from_utf8(output.bundle_json).unwrap();

        assert!(bundle.contains("overlay: true"), "{bundle}");
        assert!(
            bundle
                .contains("wake-federation:catalog@build-overlay/source/.wake/generated/shared.ts"),
            "{bundle}"
        );
        assert!(!logical_dependency.exists());
    }

    #[test]
    fn prepared_declarations_bind_deterministically_without_a_placeholder() {
        let project = fixture(&[
            (
                "src/shared.ts",
                "export interface Shared { value: string; }",
            ),
            (
                "src/index.ts",
                "export type { Shared } from './shared.js'; export interface Public { ready: boolean; }",
            ),
        ]);
        let container = ContainerName::new("catalog");
        let final_id = BuildId::new("sha384-final-generation");
        let entries = exposed(&[("./Public", "src/index.ts")]);
        let prepared = prepare_federation_types(project.path(), &container, &entries).unwrap();
        let first = prepared.bind(&final_id).unwrap();
        let second = prepared.bind(&final_id).unwrap();

        assert_eq!(first, second);
        assert!(
            !prepared
                .identity_bytes()
                .windows(final_id.as_str().len())
                .any(|window| { window == final_id.as_str().as_bytes() })
        );
    }

    #[test]
    fn binding_changes_only_parser_proven_module_requests() {
        let project = fixture(&[
            (
                "src/shared.ts",
                "export interface Shared { value: string; }",
            ),
            (
                "src/index.ts",
                "export type { Shared } from './shared.js';\n\
                 export interface Public {\n\
                   marker: \"wake-federation:catalog@type-identity-placeholder/source/user-text\";\n\
                 }",
            ),
        ]);
        let container = ContainerName::new("catalog");
        let final_id = BuildId::new("sha384-final-generation");
        let prepared = prepare_federation_types(
            project.path(),
            &container,
            &exposed(&[("./Public", "src/index.ts")]),
        )
        .unwrap();

        let bound = prepared.bind(&final_id).unwrap();
        let bundle = serde_json::from_slice::<DeclarationBundle>(&bound.bundle_json).unwrap();
        let public = &bundle.modules["catalog/Public"];

        assert!(
            public
                .contains("\"wake-federation:catalog@type-identity-placeholder/source/user-text\"")
        );
        assert!(bundle.modules.values().any(|body| body.contains(
            "from 'wake-federation:catalog@sha384-final-generation/source/src/shared.ts'"
        )));
    }

    #[test]
    fn public_implicit_any_is_rejected_by_the_canonical_emitter() {
        let project = fixture(&[("src/index.ts", "export const unsafe = compute();")]);
        let error = prepare_federation_types(
            project.path(),
            &ContainerName::new("catalog"),
            &exposed(&[("./Unsafe", "src/index.ts")]),
        )
        .unwrap_err();

        assert!(error.to_string().contains("explicit type annotation"));
    }

    #[test]
    fn explicit_public_any_is_also_rejected() {
        let project = fixture(&[("src/index.ts", "export const unsafe: any = undefined;")]);
        let error = prepare_federation_types(
            project.path(),
            &ContainerName::new("catalog"),
            &exposed(&[("./Unsafe", "src/index.ts")]),
        )
        .unwrap_err();

        assert!(error.to_string().contains("cannot contain the `any` type"));
    }
}
