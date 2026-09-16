use super::{GraphRequest, GraphSource, analysis};
use crate::lint::{
    CompletedFile, ConfiguredLint, FileTask, LintDocument, LintFixMode, analysis_failure,
    execution::Batch, module_snapshot::SnapshotFs, present_result, source_type, type_service,
};
use crate::{CancellationToken, WakeError};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wake_lint_core::{
    Analysis, LintOptions, ModuleGraph, ModuleId, RuleLevel, TypeSource, TypedSource,
};
use wake_resolver::ResolveOptions;

pub(in crate::lint) struct Project<'a> {
    pub root: &'a Path,
    pub documents: &'a BTreeMap<String, Arc<LintDocument>>,
    pub resolve: &'a ResolveOptions,
    pub dependencies: &'a mut BTreeSet<PathBuf>,
}

pub(in crate::lint) fn enabled(config: &ConfiguredLint, path: &str) -> bool {
    let levels = wake_lint_core::effective_rules(&config.options(path))
        .expect("all configuration layers were validated");
    wake_lint_core::RULES
        .iter()
        .any(|rule| rule.analysis == Analysis::ModuleGraph && levels[rule.id] != RuleLevel::Off)
}

pub(in crate::lint) fn needs_project(config: &ConfiguredLint, path: &str) -> bool {
    if config.processor_for(path).is_some() {
        // Processors currently provide a virtual single-file source only. Project and type
        // analyses require a resolver identity for the host file and are rejected by core.
        return false;
    }
    enabled(config, path) || type_service::enabled(config, path)
}

struct Prepared {
    task: FileTask,
    options: LintOptions,
    module: bool,
    typed: bool,
    text: Arc<str>,
    passes: u8,
}

impl Prepared {
    fn new(mut task: FileTask) -> Result<Self, WakeError> {
        let module = enabled(&task.config, &task.path);
        let typed = type_service::enabled(&task.config, &task.path);
        if typed && task.config.types.is_none() {
            return Err(analysis(
                "Enabled type rules require [lint.types] with explicit projects",
            ));
        }
        let options = task.config.options(&task.path);
        if (module || typed || task.fix != LintFixMode::Off)
            && let Some(cache) = &mut task.cache
        {
            cache.bypass();
        }
        let (text, passes) = if task.fix == LintFixMode::Off {
            (task.text.clone(), 0)
        } else {
            // Graph rules have no fixes. Their findings are added against the final owned source.
            let mut local = options.clone();
            for rule in wake_lint_core::RULES.iter().filter(|rule| {
                matches!(
                    rule.analysis,
                    Analysis::ModuleGraph | Analysis::TypeInformation
                )
            }) {
                local.rules.insert(rule.id.into(), RuleLevel::Off.into());
            }
            let language = source_type(&task.path)?;
            let fixed = if let Some(baseline) = &task.baseline {
                wake_lint_core::fix_text_with_baseline(
                    &task.text,
                    language,
                    &local,
                    &baseline.entries(),
                )
                .map(|(fixed, _)| fixed)
            } else {
                wake_lint_core::fix_text(&task.text, language, &local)
            }
            .map_err(|error| analysis_failure(error, &task.path, true))?;
            (fixed.output, fixed.passes)
        };
        Ok(Self {
            task,
            options,
            module,
            typed,
            text: Arc::from(text),
            passes,
        })
    }

    fn finish(
        mut self,
        graph: Arc<ModuleGraph>,
        id: Option<ModuleId>,
        typed: Option<TypedSource>,
    ) -> Result<CompletedFile, WakeError> {
        let language = source_type(&self.task.path)?;
        let entries = self
            .task
            .baseline
            .as_ref()
            .map(|baseline| baseline.entries())
            .unwrap_or_default();
        let mut result = if let Some(typed) = typed {
            let result = if let Some(id) = id {
                typed.lint_with_module(&graph, id, &self.options)
            } else {
                typed.lint(&self.options)
            };
            result.map_err(|error| {
                analysis_failure(error, &self.task.path, self.task.fix != LintFixMode::Off)
            })?
        } else if let Some(id) = id {
            graph.lint(id, &self.options).map_err(|error| {
                analysis_failure(error, &self.task.path, self.task.fix != LintFixMode::Off)
            })?
        } else if self.task.fix == LintFixMode::Off
            && let Some(cache) = &mut self.task.cache
        {
            cache.check(
                &self.task.path,
                &self.text,
                language,
                &self.options,
                &entries,
            )?
        } else {
            wake_lint_core::lint_text(&self.text, language, &self.options).map_err(|error| {
                analysis_failure(error, &self.task.path, self.task.fix != LintFixMode::Off)
            })?
        };
        if let Some(baseline) = &mut self.task.baseline {
            baseline.apply(&self.text, &mut result)?;
        }
        let result = present_result(
            self.task.path,
            &self.task.text,
            self.text.to_string(),
            self.passes,
            result,
        );
        Ok(CompletedFile {
            result,
            cache: self.task.cache,
            baseline: self.task.baseline,
            snapshot: self.task.snapshot,
        })
    }
}

pub(in crate::lint) fn analyze(
    tasks: Vec<FileTask>,
    batch: &Batch<'_>,
    cancellation: &CancellationToken,
    project: Project<'_>,
) -> Result<Vec<Result<CompletedFile, WakeError>>, WakeError> {
    if tasks.len() > ModuleGraph::MAX_FILES {
        return Err(analysis("Module selected file budget exceeded"));
    }
    let mut input_bytes = 0usize;
    for task in &tasks {
        if task.text.len() > ModuleGraph::MAX_SOURCE_BYTES.saturating_sub(input_bytes) {
            return Err(analysis("Module selected source byte budget exceeded"));
        }
        input_bytes += task.text.len();
    }
    let mut tasks = tasks.into_iter();
    let mut prepared = Vec::new();
    let mut final_bytes = 0usize;
    loop {
        cancellation.check()?;
        let jobs: Vec<_> = tasks
            .by_ref()
            .take(batch.batch_size())
            .map(|task| move || Prepared::new(task))
            .collect();
        if jobs.is_empty() {
            break;
        }
        for file in batch.run_window(jobs, cancellation)? {
            let file = file?;
            if file.text.len() > ModuleGraph::MAX_SOURCE_BYTES.saturating_sub(final_bytes) {
                return Err(analysis(
                    "Module final selected source byte budget exceeded",
                ));
            }
            final_bytes += file.text.len();
            prepared.push(file);
        }
    }
    let mut overlays: BTreeMap<_, _> = project
        .documents
        .iter()
        .map(|(path, document)| (project.root.join(path), Arc::from(document.text.as_str())))
        .collect();
    let mut sources = Vec::new();
    for file in &prepared {
        let path = project.root.join(&file.task.path);
        overlays.insert(path.clone(), file.text.clone());
        if file.module {
            sources.push(GraphSource {
                path,
                text: file.text.clone(),
            });
        }
    }
    let snapshot = Arc::new(SnapshotFs::new(
        Arc::new(wake_common::OsFileSystem),
        overlays.clone(),
        cancellation.clone(),
    )?);
    let mut typed_inputs = Vec::new();
    let mut type_settings = None;
    for file in &prepared {
        if file.typed {
            type_settings = file.task.config.types.as_ref();
            typed_inputs.push(
                TypeSource::new(
                    file.task.path.clone(),
                    file.text.clone(),
                    source_type(&file.task.path)?,
                )
                .map_err(|error| {
                    analysis_failure(error, &file.task.path, file.task.fix != LintFixMode::Off)
                })?,
            );
        }
    }
    let built = super::build(
        snapshot.clone(),
        GraphRequest {
            root: project.root.into(),
            sources,
            overlays,
            resolve: project.resolve.clone(),
        },
        batch,
        cancellation,
    );
    project.dependencies.extend(built.watch_paths);
    let ready = built.result?;
    let typed = if let Some(settings) = type_settings {
        type_service::analyze(
            snapshot,
            project.root,
            settings,
            typed_inputs,
            cancellation,
            project.dependencies,
        )?
    } else {
        Vec::new()
    };
    let mut typed = typed.into_iter();
    let graph = Arc::new(ready.graph);
    let mut selected = ready.selected.into_iter();
    let mut prepared = prepared.into_iter();
    let mut completed = Vec::new();
    loop {
        cancellation.check()?;
        let jobs: Vec<_> = prepared
            .by_ref()
            .take(batch.batch_size())
            .map(|file| {
                let id = file.module.then(|| {
                    selected
                        .next()
                        .expect("every module root has an owned graph file")
                });
                let graph = graph.clone();
                let typed = file
                    .typed
                    .then(|| typed.next().expect("each typed input was analyzed"));
                move || file.finish(graph, id, typed)
            })
            .collect();
        if jobs.is_empty() {
            break;
        }
        completed.extend(batch.run_window(jobs, cancellation)?);
    }
    Ok(completed)
}
