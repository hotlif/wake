//! Dependency-owned suite selection for related, changed, and watch runs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use wake_js_runtime::ModuleGraphManifest;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelatedOrigin {
    Explicit,
    Changed,
    Watch,
}

/// A physical test file may be executed by more than one configured project. Keep the project
/// discriminator in every cache and graph key so those executions never share environment/setup
/// state or overwrite each other's dependency edges.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SuiteIdentity {
    pub(crate) path: PathBuf,
    pub(crate) project: Option<String>,
}

impl SuiteIdentity {
    pub(crate) fn new(root: &Path, path: &Path, project: Option<&str>) -> Self {
        Self {
            path: absolute_path(root, path),
            project: project.map(str::to_string),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SelectionOutcome {
    pub(crate) suites: BTreeSet<SuiteIdentity>,
    pub(crate) conservative: bool,
    pub(crate) reasons: Vec<String>,
}

/// The reverse dependency index belongs to Wake Test, not the runtime or filesystem watcher.
#[derive(Debug, Clone, Default)]
pub(crate) struct SuiteGraphIndex {
    suites: BTreeSet<SuiteIdentity>,
    forward: BTreeMap<SuiteIdentity, BTreeSet<String>>,
    reverse: BTreeMap<String, BTreeSet<SuiteIdentity>>,
    resolver_inputs: BTreeSet<String>,
    opaque_suites: BTreeSet<SuiteIdentity>,
}

impl SuiteGraphIndex {
    pub(crate) fn record(
        &mut self,
        root: &Path,
        suite: &Path,
        project: Option<&str>,
        manifest: &ModuleGraphManifest,
    ) {
        let suite = SuiteIdentity::new(root, suite, project);
        self.remove(&suite);
        self.suites.insert(suite.clone());
        let mut watched = BTreeSet::from([path_key(&suite.path)]);
        for module in &manifest.modules {
            for path in &module.watch_paths {
                watched.insert(path_key(&absolute_path(root, path)));
            }
        }
        for key in &watched {
            self.reverse
                .entry(key.clone())
                .or_default()
                .insert(suite.clone());
        }
        self.forward.insert(suite.clone(), watched);
        self.resolver_inputs.extend(
            manifest
                .resolver_inputs
                .iter()
                .map(|path| path_key(&absolute_path(root, path))),
        );
        if manifest.opaque_dependencies {
            self.opaque_suites.insert(suite);
        }
    }

    pub(crate) fn record_opaque(&mut self, root: &Path, suite: &Path, project: Option<&str>) {
        let suite = SuiteIdentity::new(root, suite, project);
        self.remove(&suite);
        self.suites.insert(suite.clone());
        let key = path_key(&suite.path);
        self.forward
            .insert(suite.clone(), BTreeSet::from([key.clone()]));
        self.reverse.entry(key).or_default().insert(suite.clone());
        self.opaque_suites.insert(suite);
    }

    pub(crate) fn select(
        &self,
        root: &Path,
        paths: &[PathBuf],
        origin: RelatedOrigin,
    ) -> SelectionOutcome {
        let mut outcome = SelectionOutcome::default();
        if paths.is_empty() {
            // A clean Git work tree has no related suites. Watch callers use an explicit
            // full-run command (rather than an empty invalidation list) when they want to rerun
            // every suite, so the two states remain unambiguous.
            if origin == RelatedOrigin::Watch {
                outcome.suites = self.suites.clone();
            }
            return outcome;
        }

        if !self.opaque_suites.is_empty() {
            outcome.suites = self.suites.clone();
            outcome.conservative = true;
            outcome.reasons.push(format!(
                "{} suite graph(s) contain opaque dynamic or unresolved dependencies",
                self.opaque_suites.len()
            ));
            return outcome;
        }

        for path in paths {
            let absolute = absolute_path(root, path);
            let key = path_key(&absolute);
            if is_structural_path(&absolute) || self.resolver_inputs.contains(&key) {
                outcome.suites = self.suites.clone();
                outcome.conservative = true;
                outcome.reasons.push(format!(
                    "resolver or test topology input changed: {}",
                    display_path(&absolute)
                ));
                return outcome;
            }

            let mut matched = false;
            if let Some(suites) = self.reverse.get(&key) {
                outcome.suites.extend(suites.iter().cloned());
                matched = true;
            }
            let prefix = format!("{}/", key.trim_end_matches('/'));
            for (watched, suites) in self.reverse.range(prefix.clone()..) {
                if !watched.starts_with(&prefix) {
                    break;
                }
                outcome.suites.extend(suites.iter().cloned());
                matched = true;
            }
            if !matched && origin != RelatedOrigin::Explicit {
                outcome.suites = self.suites.clone();
                outcome.conservative = true;
                outcome.reasons.push(format!(
                    "changed path is not present in the compiled graph: {}",
                    display_path(&absolute)
                ));
                return outcome;
            }
        }
        outcome
    }

    fn remove(&mut self, suite: &SuiteIdentity) {
        if let Some(keys) = self.forward.remove(suite) {
            for key in keys {
                if let Some(suites) = self.reverse.get_mut(&key) {
                    suites.remove(suite);
                    if suites.is_empty() {
                        self.reverse.remove(&key);
                    }
                }
            }
        }
        self.opaque_suites.remove(suite);
    }
}

pub(crate) fn is_structural_path(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some(
            "wake.config.toml"
                | "package.json"
                | "package-lock.json"
                | "yarn.lock"
                | "pnpm-lock.yaml"
                | ".pnp.cjs"
                | ".pnp.data.json"
        )
    )
}

fn absolute_path(root: &Path, path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    path.canonicalize()
        .unwrap_or_else(|_| lexical_normalize(&path))
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    normalized.pop();
                }
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn path_key(path: &Path) -> String {
    let value = display_path(path);
    #[cfg(windows)]
    {
        value.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        value
    }
}

fn display_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wake_js_runtime::{ModuleGraphManifest, ModuleGraphModule};

    fn manifest(
        paths: &[PathBuf],
        opaque: bool,
        resolver_inputs: Vec<PathBuf>,
    ) -> ModuleGraphManifest {
        ModuleGraphManifest {
            entry_id: "entry".to_string(),
            modules: paths
                .iter()
                .enumerate()
                .map(|(index, path)| ModuleGraphModule {
                    id: format!("module-{index}"),
                    watch_paths: vec![path.clone()],
                    dependencies: Vec::new(),
                    opaque_dependencies: opaque,
                })
                .collect(),
            resolver_inputs,
            opaque_dependencies: opaque,
        }
    }

    fn suite(root: &Path, path: &Path) -> SuiteIdentity {
        SuiteIdentity::new(root, path, None)
    }

    #[test]
    fn direct_transitive_and_shared_dependencies_select_stably() {
        let root = tempfile::tempdir().unwrap();
        let button = root.path().join("button.test.tsx");
        let form = root.path().join("form.test.tsx");
        let shared = root.path().join("src/shared.ts");
        let button_source = root.path().join("src/button.tsx");
        let mut index = SuiteGraphIndex::default();
        index.record(
            root.path(),
            &button,
            None,
            &manifest(
                &[button.clone(), button_source.clone(), shared.clone()],
                false,
                vec![],
            ),
        );
        index.record(
            root.path(),
            &form,
            None,
            &manifest(&[form.clone(), shared.clone()], false, vec![]),
        );

        assert_eq!(
            index
                .select(root.path(), &[button_source], RelatedOrigin::Explicit)
                .suites,
            BTreeSet::from([suite(root.path(), &button)])
        );
        assert_eq!(
            index
                .select(root.path(), &[shared], RelatedOrigin::Explicit)
                .suites,
            BTreeSet::from([suite(root.path(), &button), suite(root.path(), &form)])
        );
    }

    #[test]
    fn explicit_unknown_is_empty_but_changed_unknown_is_conservative() {
        let root = tempfile::tempdir().unwrap();
        let suite = root.path().join("view.test.tsx");
        let mut index = SuiteGraphIndex::default();
        index.record(
            root.path(),
            &suite,
            None,
            &manifest(std::slice::from_ref(&suite), false, vec![]),
        );
        let unknown = root.path().join("src/new-file.ts");
        assert!(
            index
                .select(
                    root.path(),
                    std::slice::from_ref(&unknown),
                    RelatedOrigin::Explicit
                )
                .suites
                .is_empty()
        );
        let selected = index.select(root.path(), &[unknown], RelatedOrigin::Changed);
        assert_eq!(
            selected.suites,
            BTreeSet::from([self::suite(root.path(), &suite)])
        );
        assert!(selected.conservative);
    }

    #[test]
    fn empty_changed_selection_is_empty_but_empty_watch_selection_is_full() {
        let root = tempfile::tempdir().unwrap();
        let suite = root.path().join("view.test.tsx");
        let mut index = SuiteGraphIndex::default();
        index.record(
            root.path(),
            &suite,
            None,
            &manifest(std::slice::from_ref(&suite), false, vec![]),
        );

        let changed = index.select(root.path(), &[], RelatedOrigin::Changed);
        assert!(changed.suites.is_empty());
        assert!(!changed.conservative);

        let watch = index.select(root.path(), &[], RelatedOrigin::Watch);
        assert_eq!(
            watch.suites,
            BTreeSet::from([self::suite(root.path(), &suite)])
        );
        assert!(!watch.conservative);
    }

    #[test]
    fn opaque_and_structural_inputs_never_false_negative() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a.test.ts");
        let b = root.path().join("b.test.ts");
        let mut index = SuiteGraphIndex::default();
        index.record(
            root.path(),
            &a,
            None,
            &manifest(std::slice::from_ref(&a), true, vec![]),
        );
        index.record(
            root.path(),
            &b,
            None,
            &manifest(std::slice::from_ref(&b), false, vec![]),
        );
        let outcome = index.select(
            root.path(),
            &[root.path().join("unrelated.ts")],
            RelatedOrigin::Watch,
        );
        assert_eq!(
            outcome.suites,
            BTreeSet::from([suite(root.path(), &a), suite(root.path(), &b)])
        );
        assert!(outcome.conservative);

        let mut precise = SuiteGraphIndex::default();
        precise.record(
            root.path(),
            &a,
            None,
            &manifest(std::slice::from_ref(&a), false, vec![]),
        );
        precise.record(
            root.path(),
            &b,
            None,
            &manifest(std::slice::from_ref(&b), false, vec![]),
        );
        let outcome = precise.select(
            root.path(),
            &[root.path().join("package.json")],
            RelatedOrigin::Watch,
        );
        assert_eq!(
            outcome.suites,
            BTreeSet::from([suite(root.path(), &a), suite(root.path(), &b)])
        );
        assert!(outcome.conservative);
    }
}
