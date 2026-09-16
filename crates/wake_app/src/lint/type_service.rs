//! Native type-service lifecycle and source-bound semantic translation.
mod backend;
mod facts;
mod filesystem;
mod project;
mod transport;
mod wire;

use super::{ConfiguredLint, module_snapshot::SnapshotFs};
use crate::{CancellationToken, WakeError};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::Arc,
};
use wake_lint_core::{Analysis, RuleLevel, TypeSource, TypedSource};

pub(super) fn validate(
    mut value: wake_config::LintTypes,
) -> Result<wake_config::LintTypes, WakeError> {
    if value.compiler.is_empty()
        || value.compiler.contains(['\0', '\\', ':'])
        || value.compiler.chars().any(char::is_whitespace)
        || value.projects.is_empty()
        || value.projects.len() > 64
    {
        return Err(super::failure(
            "lint.types requires a compiler package and 1–64 explicit projects",
        ));
    }
    let mut seen = BTreeSet::new();
    for path in &mut value.projects {
        *path = super::normalize_relative(path)?;
        if path.is_empty()
            || path.contains(['*', '?', '[', ']', '{', '}', '\0'])
            || !seen.insert(path.clone())
        {
            return Err(super::failure(
                "lint.types projects must be distinct relative file paths without globs",
            ));
        }
    }
    Ok(value)
}

pub(super) fn enabled(config: &ConfiguredLint, path: &str) -> bool {
    let levels =
        wake_lint_core::effective_rules(&config.options(path)).expect("validated configuration");
    wake_lint_core::RULES
        .iter()
        .any(|rule| rule.analysis == Analysis::TypeInformation && levels[rule.id] != RuleLevel::Off)
}

pub(super) fn analyze(
    snapshot: Arc<SnapshotFs>,
    root: &Path,
    settings: &wake_config::LintTypes,
    inputs: Vec<TypeSource>,
    cancellation: &CancellationToken,
    dependencies: &mut BTreeSet<PathBuf>,
) -> Result<Vec<TypedSource>, WakeError> {
    let result = (|| {
        let filesystem = filesystem::TypeFileSystem::new(
            snapshot.clone(),
            BTreeMap::new(),
            root,
            cancellation.clone(),
        )?;
        let configurations: Vec<_> = settings
            .projects
            .iter()
            .map(|path| root.join(path))
            .collect();
        let mut project = project::TypeProject::start(
            filesystem,
            root,
            &settings.compiler,
            &configurations,
            cancellation.clone(),
        )?;
        let result = inputs
            .into_iter()
            .map(|input| {
                cancellation.check()?;
                project.type_source(&root.join(input.path()), input)
            })
            .collect();
        dependencies.extend(project.observed_paths());
        result
    })();
    // The shared lower snapshot outlives failed initialization as well as a successful session.
    // Its observations are physical inputs of resolver projections, including failed lookups.
    let environment = wake_resolver::ResolutionEnvironment::new(snapshot.clone());
    dependencies.extend(
        snapshot
            .observed_paths()
            .into_iter()
            .map(|path| environment.watch_path(&path)),
    );
    snapshot.check()?;
    result
}
