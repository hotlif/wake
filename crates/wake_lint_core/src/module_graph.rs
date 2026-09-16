//! Immutable, source-bound project facts. Resolution and filesystem authority stay in the app.
mod check;
pub(crate) mod parameters;
mod topology;

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use crate::{LintError, LintOptions, LintResult, ModuleRequest, SourceType, inspect_module};
use wake_common::Interner;
use wake_ecma_ast::{SourceIdentifierRole, SourceNodeKind, SourceValueBindingKind};
use wake_ecma_parser::{ParseOptions, parse_source};

pub(crate) const RULE_IDS: &[&str] = &[
    "import/no-unresolved",
    "import/no-cycle",
    "import/no-duplicates",
    "import/no-restricted-paths",
    "import/no-extraneous-dependencies",
    "import/order",
];
pub(crate) const GROUPS: &[&str] = &[
    "builtin", "external", "internal", "parent", "sibling", "index", "type", "unknown",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ModuleId(pub usize);

/// Declaration facts for this request's issuer, not the target package's dependencies.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackageDependency {
    pub name: String,
    pub production: bool,
    pub development: bool,
    pub optional: bool,
    pub peer: bool,
    pub self_reference: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleResolution {
    File {
        target: ModuleId,
        package: Option<PackageDependency>,
    },
    Builtin(String),
    /// A resolved non-code resource with no JavaScript module dependencies.
    Resource {
        identity: String,
        path: String,
        package: Option<PackageDependency>,
    },
    /// Resolved code which has no analyzable source facts. Never an acyclic leaf proof.
    Opaque {
        identity: String,
        path: String,
        package: Option<PackageDependency>,
    },
    Unresolved {
        reason: String,
        package: Option<PackageDependency>,
    },
    Unknown(String),
}

impl ModuleResolution {
    pub(crate) fn package(&self) -> Option<&PackageDependency> {
        match self {
            Self::Unresolved { package, .. }
            | Self::File { package, .. }
            | Self::Resource { package, .. }
            | Self::Opaque { package, .. } => package.as_ref(),
            _ => None,
        }
    }

    pub(crate) fn external(&self) -> bool {
        self.package()
            .is_some_and(|package| !package.self_reference)
    }
}

/// The constructor parses the retained source itself. Request ranges cannot be supplied separately.
pub struct ModuleFile {
    identity: String,
    path: String,
    source: Arc<str>,
    source_type: SourceType,
    requests: Vec<ModuleRequest>,
    resolutions: Vec<Option<ModuleResolution>>,
    incomplete: bool,
}

impl ModuleFile {
    pub fn new(
        identity: String,
        path: String,
        source: Arc<str>,
        source_type: SourceType,
    ) -> Result<Self, LintError> {
        if identity.is_empty() || path.is_empty() || path.contains('\\') {
            return Err(invalid(
                "Module identity and normalized path must be nonempty",
            ));
        }
        if source.len() > Limits::DEFAULT.bytes {
            return Err(invalid("Module source byte budget exceeded"));
        }
        let facts = inspect_module(&source, source_type);
        Ok(Self {
            identity,
            path,
            source,
            source_type,
            resolutions: vec![None; facts.requests.len()],
            requests: facts.requests,
            incomplete: facts.incomplete,
        })
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    pub fn source(&self) -> &Arc<str> {
        &self.source
    }
    pub fn source_type(&self) -> SourceType {
        self.source_type
    }
    pub fn requests(&self) -> &[ModuleRequest] {
        &self.requests
    }

    pub fn resolve(&mut self, index: usize, resolution: ModuleResolution) -> Result<(), LintError> {
        let request = self
            .requests
            .get(index)
            .ok_or_else(|| invalid("Unknown module request"))?;
        if request.specifier.is_none() && !matches!(resolution, ModuleResolution::Unknown(_)) {
            return Err(invalid(
                "A dynamic request cannot have an invented static resolution",
            ));
        }
        if self.resolutions[index].is_some() {
            return Err(invalid("Module request resolution is already frozen"));
        }
        self.resolutions[index] = Some(resolution);
        Ok(())
    }

    pub(crate) fn edges(&self) -> impl Iterator<Item = (&ModuleRequest, &ModuleResolution)> {
        self.requests
            .iter()
            .zip(&self.resolutions)
            .map(|(request, resolution)| {
                (
                    request,
                    resolution
                        .as_ref()
                        .expect("graph validates all resolutions"),
                )
            })
    }
}

/// Shared by all per-file checks in one project generation. Topology and project-level global
/// ambient value names are memoized by the immutable graph.
pub struct ModuleGraph {
    files: Vec<ModuleFile>,
    global_ambient_names: Vec<String>,
    cycles: [OnceLock<Result<topology::Cycles, LintError>>; 16],
    limits: Limits,
}

#[derive(Clone, Copy)]
struct Limits {
    files: usize,
    requests: usize,
    bytes: usize,
    work: usize,
}
impl Limits {
    const DEFAULT: Self = Self {
        files: 20_000,
        requests: 200_000,
        bytes: 128 * 1024 * 1024,
        work: 10_000_000,
    };
}

impl ModuleGraph {
    pub const MAX_FILES: usize = Limits::DEFAULT.files;
    pub const MAX_REQUESTS: usize = Limits::DEFAULT.requests;
    pub const MAX_SOURCE_BYTES: usize = Limits::DEFAULT.bytes;

    pub fn new(files: Vec<ModuleFile>) -> Result<Self, LintError> {
        Self::with_limits(files, Limits::DEFAULT)
    }

    fn with_limits(files: Vec<ModuleFile>, limits: Limits) -> Result<Self, LintError> {
        let mut identities = HashSet::new();
        let (mut bytes, mut requests) = (0usize, 0usize);
        if files.len() > limits.files {
            return Err(invalid("Module file budget exceeded"));
        }
        for file in &files {
            if !identities.insert(&file.identity) {
                return Err(invalid("Duplicate module identity"));
            }
            bytes = bytes
                .checked_add(file.source.len())
                .ok_or_else(|| invalid("Module source byte budget exceeded"))?;
            requests = requests
                .checked_add(file.requests.len())
                .ok_or_else(|| invalid("Module request budget exceeded"))?;
            if bytes > limits.bytes || requests > limits.requests {
                return Err(invalid("Module source or request budget exceeded"));
            }
            for resolution in &file.resolutions {
                match resolution {
                    None => return Err(invalid("Unresolved module graph slot")),
                    Some(ModuleResolution::File { target, .. }) if target.0 >= files.len() => {
                        return Err(invalid("Module target is outside the graph"));
                    }
                    _ => (),
                }
            }
        }
        let global_ambient_names = collect_global_ambient_names(&files);
        Ok(Self {
            files,
            global_ambient_names,
            limits,
            cycles: std::array::from_fn(|_| OnceLock::new()),
        })
    }

    pub fn file(&self, id: ModuleId) -> Result<&ModuleFile, LintError> {
        self.files
            .get(id.0)
            .ok_or_else(|| invalid("Module is outside the graph"))
    }

    pub(crate) fn global_ambient_names(&self) -> &[String] {
        &self.global_ambient_names
    }

    pub fn lint(&self, id: ModuleId, options: &LintOptions) -> Result<LintResult, LintError> {
        let file = self.file(id)?;
        crate::lint_text_inner(
            &file.source,
            file.source_type,
            options,
            Some((self, id)),
            None,
        )
    }

    pub(crate) fn check(
        &self,
        id: ModuleId,
        configuration: &std::collections::BTreeMap<String, crate::EffectiveRule>,
        diagnostics: &mut Vec<crate::LintDiagnostic>,
    ) -> Result<(), LintError> {
        check::check(self, id, configuration, diagnostics)
    }
}

fn collect_global_ambient_names(files: &[ModuleFile]) -> Vec<String> {
    let mut names = HashSet::new();
    for file in files {
        if !file.source_type.is_typescript() {
            continue;
        }
        let interner = Interner::new();
        let parsed = parse_source(
            &file.source,
            &interner,
            file.source_type,
            ParseOptions::default(),
        );
        if parsed.parsed.has_errors() {
            continue;
        }
        let is_script = parsed.imports.is_empty() && parsed.exports.is_empty();
        for namespace in &parsed.namespaces {
            let node = parsed.syntax.get(namespace.node);
            let inside_global = node.is_some_and(|node| {
                parsed.syntax.iter().any(|candidate| {
                    candidate.kind == SourceNodeKind::TsGlobalAugmentation
                        && candidate.span.lo <= node.span.lo
                        && node.span.hi <= candidate.span.hi
                }) && !parsed.syntax.iter().any(|candidate| {
                    candidate.kind == SourceNodeKind::TsAmbientModule
                        && candidate.span.lo <= node.span.lo
                        && node.span.hi <= candidate.span.hi
                })
            });
            if (inside_global || (is_script && namespace.is_ambient))
                && let Some(root) = namespace.names.first()
            {
                names.insert(root.name.clone());
            }
        }
        for identifier in parsed
            .identifiers
            .iter()
            .filter(|identifier| identifier.role == SourceIdentifierRole::ValueBinding)
        {
            let inside = |kind: SourceNodeKind| {
                parsed.syntax.iter().any(|node| {
                    node.kind == kind
                        && node.span.lo <= identifier.span.lo
                        && identifier.span.hi <= node.span.hi
                })
            };
            let inside_erased_signature = parsed.functions.iter().any(|function| {
                function.body.is_none()
                    && function.scope.lo <= identifier.span.lo
                    && identifier.span.hi <= function.scope.hi
                    && identifier.value_kind != Some(SourceValueBindingKind::Function)
            });
            let inside_global = inside(SourceNodeKind::TsGlobalAugmentation)
                && !inside(SourceNodeKind::TsNamespace)
                && !inside(SourceNodeKind::TsAmbientModule)
                && !inside(SourceNodeKind::TsSignature)
                && !inside(SourceNodeKind::JsFunctionBody)
                && !inside(SourceNodeKind::JsBlock)
                && !inside(SourceNodeKind::JsSwitchBody)
                && !inside_erased_signature;
            let script_declaration = is_script
                && inside(SourceNodeKind::TsDeclare)
                && !inside(SourceNodeKind::TsNamespace)
                && !inside(SourceNodeKind::TsAmbientModule)
                && !inside(SourceNodeKind::TsGlobalAugmentation)
                && !inside(SourceNodeKind::TsSignature)
                && !inside(SourceNodeKind::JsBlock)
                && !inside(SourceNodeKind::JsFunctionBody)
                && !inside(SourceNodeKind::JsSwitchBody)
                && !inside_erased_signature;
            if inside_global || script_declaration {
                names.insert(identifier.name.clone());
            }
        }
    }
    let mut names: Vec<_> = names.into_iter().collect();
    names.sort();
    names
}

fn invalid(message: &str) -> LintError {
    LintError::Analysis(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn node() -> ModuleFile {
        let mut file = ModuleFile::new(
            "a".into(),
            "a.ts".into(),
            Arc::from("import './a';"),
            SourceType::Module,
        )
        .unwrap();
        file.resolve(
            0,
            ModuleResolution::File {
                target: ModuleId(0),
                package: None,
            },
        )
        .unwrap();
        file
    }

    #[test]
    fn every_project_budget_fails_without_partial_graph_results() {
        for limits in [
            Limits {
                files: 0,
                ..Limits::DEFAULT
            },
            Limits {
                requests: 0,
                ..Limits::DEFAULT
            },
            Limits {
                bytes: 1,
                ..Limits::DEFAULT
            },
        ] {
            assert!(matches!(
                ModuleGraph::with_limits(vec![node()], limits),
                Err(LintError::Analysis(_))
            ));
        }
        let graph = ModuleGraph::with_limits(
            vec![node()],
            Limits {
                work: 1,
                ..Limits::DEFAULT
            },
        )
        .unwrap();
        let mut options = LintOptions {
            recommended: false,
            ..Default::default()
        };
        options
            .rules
            .insert("import/no-cycle".into(), crate::RuleLevel::Error.into());
        for _ in 0..2 {
            assert!(matches!(
                graph.lint(ModuleId(0), &options),
                Err(LintError::Analysis(_))
            ));
        }
    }
}
