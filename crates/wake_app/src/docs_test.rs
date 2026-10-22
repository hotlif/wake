//! Product orchestration for visual tests of the actual Docs production output.
use super::*;
pub use wake_test_contract::DocsTestViewport;
use wake_test_contract::{DocsTestDemo, DocsTestPlan};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct DocsTestOptions {
    pub root: Option<PathBuf>,
    pub themes: Vec<String>,
    pub viewports: Vec<DocsTestViewport>,
    pub update_snapshots: bool,
    pub browser_path: Option<PathBuf>,
    pub headful: bool,
}

impl Default for DocsTestOptions {
    fn default() -> Self {
        Self {
            root: None,
            themes: vec!["light".into(), "dark".into()],
            viewports: vec![
                DocsTestViewport {
                    width: 1280,
                    height: 720,
                },
                DocsTestViewport {
                    width: 390,
                    height: 844,
                },
            ],
            update_snapshots: false,
            browser_path: None,
            headful: false,
        }
    }
}

impl DocsTestOptions {
    fn validate(&self) -> Result<(), WakeError> {
        if self.themes.is_empty()
            || self.themes.len() > 2
            || self
                .themes
                .iter()
                .any(|theme| !matches!(theme.as_str(), "light" | "dark"))
            || self.themes.iter().collect::<BTreeSet<_>>().len() != self.themes.len()
        {
            return Err(WakeError::new(
                "WAKE_TEST_CONFIG",
                "themes must contain unique light/dark values",
            ));
        }
        if self.viewports.is_empty()
            || self.viewports.len() > 8
            || self
                .viewports
                .iter()
                .any(|v| !(1..=8192).contains(&v.width) || !(1..=8192).contains(&v.height))
            || self
                .viewports
                .iter()
                .map(|v| (v.width, v.height))
                .collect::<BTreeSet<_>>()
                .len()
                != self.viewports.len()
        {
            return Err(WakeError::new(
                "WAKE_TEST_CONFIG",
                "viewports must contain 1 to 8 unique width/height pairs, each from 1 to 8192",
            ));
        }
        Ok(())
    }
}

/// Build and visually test every Demo in a standalone Docs site.
pub fn test_docs(
    options: DocsTestOptions,
    cancellation: &CancellationToken,
) -> Result<TestRunResult, WakeError> {
    test_docs_with_host(options, None, cancellation)
}

/// The Node loader supplies the matching, independently packaged test host.
pub fn test_docs_with_host(
    options: DocsTestOptions,
    host_path: Option<&Path>,
    cancellation: &CancellationToken,
) -> Result<TestRunResult, WakeError> {
    options.validate()?;
    cancellation.check()?;
    let cwd = options
        .root
        .clone()
        .map_or_else(std::env::current_dir, Ok)
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))?;
    let cwd = canonical_project_root(&cwd)?;
    let config = wake_config::load(&cwd)
        .map_err(|error| WakeError::new("WAKE_TEST_CONFIG", error.to_string()).at(&cwd))?;
    if !config.docs.workspace.is_empty() {
        return Err(WakeError::new(
            "WAKE_TEST_CONFIG",
            "docs test currently requires a standalone site; run each workspace root separately",
        ));
    }
    let root = canonical_project_root(&config.resolved_root(&cwd))?;
    let output = tempfile::Builder::new()
        .prefix("wake-docs-test-")
        .tempdir()
        .map_err(|error| WakeError::new("WAKE_IO", error.to_string()))?;
    let outdir = output.path().join("site");
    let base_path = normalize_public_path(&config.docs.base_path);
    let built = build_docs(
        DocsBuildOptions {
            project: ProjectOptions {
                cwd: Some(cwd.clone()),
                ..ProjectOptions::default()
            },
            outdir: Some(outdir.clone()),
            base_path: Some(base_path.clone()),
            presentation: Some(DocsPresentation::Standalone),
        },
        cancellation,
    )?;
    if built.demos.is_empty() {
        return Err(WakeError::new("WAKE_TEST_DISCOVERY", "No Docs demos found").at(&root));
    }
    let plan = DocsTestPlan {
        schema_version: "wake.docs-test.v1".into(),
        output_dir: outdir,
        base_path,
        demos: built
            .demos
            .into_iter()
            .map(|demo| DocsTestDemo {
                path: root.join(&demo.id).to_string_lossy().into_owned(),
                id: demo.id,
                title: demo.title,
            })
            .collect(),
        themes: options.themes,
        viewports: options.viewports,
    };
    run_tests_with_host(
        TestOptions {
            root: Some(cwd),
            docs_plan: Some(plan),
            update_snapshots: Some(
                if options.update_snapshots {
                    "all"
                } else {
                    "none"
                }
                .into(),
            ),
            browser_path: options.browser_path,
            headful: options.headful,
            serial: true,
            ..TestOptions::default()
        },
        host_path,
        cancellation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn matrix_validation_is_bounded_and_deterministic() {
        assert!(DocsTestOptions::default().validate().is_ok());
        for themes in [
            vec![],
            vec!["system".into()],
            vec!["dark".into(), "dark".into()],
        ] {
            assert!(
                DocsTestOptions {
                    themes,
                    ..DocsTestOptions::default()
                }
                .validate()
                .is_err()
            );
        }
        for viewports in [
            vec![],
            vec![DocsTestViewport {
                width: 0,
                height: 720,
            }],
            vec![
                DocsTestViewport {
                    width: 390,
                    height: 844
                };
                2
            ],
        ] {
            assert!(
                DocsTestOptions {
                    viewports,
                    ..DocsTestOptions::default()
                }
                .validate()
                .is_err()
            );
        }
    }
}
