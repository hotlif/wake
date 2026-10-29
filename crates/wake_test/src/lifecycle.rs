//! Run-wide lifecycle stays in the test owner, with a fresh realm for every run.
use super::*;

pub(super) struct RunLifecycle {
    root: PathBuf,
    config: wake_config::Test,
    runtime: Option<JsRuntime>,
    initialized: bool,
    finished: bool,
    watch_paths: BTreeSet<PathBuf>,
}

impl RunLifecycle {
    pub(super) fn new(root: &Path, config: &wake_config::Test, selected: bool) -> Self {
        Self {
            root: root.to_path_buf(),
            config: config.clone(),
            runtime: (selected
                && (!config.global_setup.is_empty() || !config.global_teardown.is_empty()))
            .then(JsRuntime::new),
            initialized: false,
            finished: false,
            watch_paths: BTreeSet::new(),
        }
    }

    pub(super) fn setup(
        &mut self,
        cancellation: &TestCancellationToken,
        diagnostics: &mut Vec<TestDiagnostic>,
    ) -> bool {
        if self.runtime.is_none() {
            return true;
        }
        for (index, module) in self.config.global_setup.clone().iter().enumerate() {
            if let Err(message) = self.invoke(module, "setup", index, cancellation) {
                self.diagnostic(module, message, diagnostics);
                return false;
            }
        }
        true
    }

    pub(super) fn finish(&mut self, diagnostics: &mut Vec<TestDiagnostic>) {
        if self.finished {
            return;
        }
        self.finished = true;
        if self.runtime.is_none() {
            return;
        }
        // Cancellation must not prevent cleanup; each call still has its own VM deadline.
        for (index, module) in self.config.global_teardown.clone().iter().enumerate().rev() {
            if let Err(message) =
                self.invoke(module, "teardown", index, &TestCancellationToken::default())
            {
                self.diagnostic(module, message, diagnostics);
            }
        }
        if self.initialized {
            let runtime = self.runtime.as_mut().expect("active lifecycle has a realm");
            let _ = runtime.execute_commonjs_and_read(
                &self.root.join(".wake-test-global-close.ts"),
                "",
                "",
                "globalThis.__wakeCloseLifecycle()",
                "''",
            );
        }
    }

    pub(super) fn watch_paths(&self) -> BTreeSet<PathBuf> {
        self.watch_paths.clone()
    }

    fn diagnostic(&self, module: &str, message: String, diagnostics: &mut Vec<TestDiagnostic>) {
        diagnostics.push(TestDiagnostic {
            severity: DiagnosticSeverity::Error,
            code: "WAKE_TEST_LIFECYCLE".into(),
            message,
            path: Some(normalize_path(&self.root.join(module))),
            location: None,
            notes: Vec::new(),
        });
    }

    fn invoke(
        &mut self,
        module: &str,
        phase: &str,
        index: usize,
        cancellation: &TestCancellationToken,
    ) -> Result<(), String> {
        let module_path = self
            .root
            .join(module.replace("<rootDir>", &normalize_path(&self.root)));
        self.watch_paths.insert(module_path.clone());
        let entry = self
            .root
            .join(format!(".wake-test-global-{phase}-{index}.ts"));
        let dom = if self.initialized {
            ""
        } else {
            "import '@wake-internal/happy-dom';"
        };
        let source = format!(
            "{dom} import callback from {}; globalThis.__wakeLifecycleCallback = callback;",
            serde_json::to_string(&normalize_path(&module_path)).unwrap()
        );
        let graph = compile_commonjs_module_graph_interruptible(&entry, &source, &|| {
            cancellation.is_cancelled()
        })
        .map_err(|error| {
            self.watch_paths
                .extend(error.recovery_watch_paths().iter().cloned());
            error.to_string()
        })?
        .ok_or_else(|| "Global lifecycle was cancelled".to_string())?;
        for module in &graph.module_graph().modules {
            self.watch_paths.extend(
                module
                    .watch_paths
                    .iter()
                    .filter(|path| path.is_absolute())
                    .cloned(),
            );
        }
        let prelude = if self.initialized {
            String::new()
        } else {
            format!(
                "{WAKE_TEST_RUNTIME}\nglobalThis.__wakeConfigureTest({});",
                serde_json::json!({"timeoutMs": self.config.timeout_ms, "networkMode": if self.config.network.mode == wake_config::TestNetworkMode::Allow {"allow"} else {"deny"}, "networkAllowHosts": self.config.network.allow_hosts})
            )
        };
        let completion = r#"
            globalThis.__wakeLifecycleResult = undefined;
            Promise.resolve(globalThis.__wakeEntryModulePromise).then(() =>
                globalThis.__wakeRunLifecyclePhase(globalThis.__wakeLifecycleCallback, $TIMEOUT)
            ).then(() => { globalThis.__wakeLifecycleResult = ''; }, error => {
                globalThis.__wakeLifecycleResult = String(error && error.stack || error);
            });
        "#
        .replace("$TIMEOUT", &self.config.timeout_ms.max(1).to_string());
        let compiled = emit_commonjs_graph_script(&graph, &entry, &prelude, &completion);
        let runtime = self.runtime.as_mut().expect("active lifecycle has a realm");
        runtime.set_execution_timeout(Duration::from_millis(self.config.timeout_ms.max(1)));
        let _guard = RuntimeCancellationGuard::arm(runtime, cancellation);
        self.initialized = true;
        let message = match runtime
            .execute_compiled_commonjs_graph_and_read(&compiled, "globalThis.__wakeLifecycleResult")
        {
            Ok(value) => value,
            Err(error) => {
                let _ = runtime.execute_commonjs_and_read(
                    &self.root.join(".wake-test-global-recovery.ts"),
                    "",
                    "",
                    "globalThis.__wakePauseLifecycle()",
                    "''",
                );
                return Err(error.to_string());
            }
        };
        if message.is_empty() {
            Ok(())
        } else {
            Err(message)
        }
    }
}

impl Drop for RunLifecycle {
    fn drop(&mut self) {
        // Also close resources if suite execution or reporting returns early with an infrastructure error.
        self.finish(&mut Vec::new());
    }
}
