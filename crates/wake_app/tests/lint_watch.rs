use std::{
    fs, thread,
    time::{Duration, Instant},
};
use wake_app::{
    CancellationToken, LintContext, LintDocument, LintProjectOptions, LintWatchEvent, LintWatcher,
};

#[track_caller]
fn await_event(
    watcher: &LintWatcher,
    mut accepts: impl FnMut(&LintWatchEvent) -> bool,
) -> LintWatchEvent {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut recent = std::collections::VecDeque::new();
    loop {
        for event in watcher.drain_events() {
            if accepts(&event) {
                return event;
            }
            recent.push_back(format!("{event:?}"));
            if recent.len() > 8 {
                recent.pop_front();
            }
        }
        assert!(
            Instant::now() < deadline,
            "watch did not publish the expected result: {recent:?}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn module_watch_recovers_failed_external_pnp_and_retires_old_dependencies() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("project");
    let external = parent.path().join("external");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&external).unwrap();
    fs::write(
        root.join("wake.config.toml"),
        "[lint]\nrecommended=false\n[lint.rules]\n'import/no-unresolved'='error'",
    )
    .unwrap();
    fs::write(root.join("a.ts"), "import '../external/b';").unwrap();
    fs::write(external.join("b.ts"), "export {};").unwrap();
    fs::write(parent.path().join(".pnp.cjs"), "broken").unwrap();
    let context = LintContext::create(LintProjectOptions {
        root: root.clone(),
        paths: vec!["a.ts".into()],
        cache: true,
        ..Default::default()
    })
    .unwrap();
    let watcher = LintWatcher::start(context.clone()).unwrap();
    await_event(&watcher, |event| {
        matches!(event, LintWatchEvent::Diagnostic { error, .. }
        if error.code == "WAKE_LINT_ANALYSIS" && error.message.contains("PnP"))
    });
    fs::remove_file(parent.path().join(".pnp.cjs")).unwrap();
    await_event(&watcher, |event| {
        matches!(event, LintWatchEvent::Checked { snapshot }
        if snapshot.result.error_count == 0)
    });
    fs::write(root.join("a.ts"), "export {};").unwrap();
    await_event(&watcher, |event| {
        matches!(event, LintWatchEvent::Checked { snapshot }
        if snapshot.result.error_count == 0)
    });
    thread::sleep(Duration::from_millis(300));
    watcher.drain_events();
    let generation = context.generation();
    fs::write(external.join("b.ts"), "import 'retired';").unwrap();
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        context.generation(),
        generation,
        "retired dependency still observed"
    );
    fs::create_dir_all(root.join(".wake")).unwrap();
    fs::write(root.join(".wake/output.json"), "{}").unwrap();
    thread::sleep(Duration::from_millis(500));
    assert_eq!(context.generation(), generation);
    assert!(watcher.drain_events().is_empty());
    watcher.stop();
    context.close();
}

#[test]
fn module_watch_tracks_external_negative_and_ignored_dependency_paths() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("project");
    let external = parent.path().join("external");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&external).unwrap();
    fs::write(root.join("wake.config.toml"),
        "[lint]\nrecommended=false\n[lint.rules]\n'import/no-cycle'='error'\n'import/no-unresolved'='error'").unwrap();
    fs::write(root.join("a.ts"), "import '../external/b';").unwrap();
    fs::write(external.join("b.ts"), "export {};").unwrap();
    let context = LintContext::create(LintProjectOptions {
        root: root.clone(),
        paths: vec!["a.ts".into()],
        ..Default::default()
    })
    .unwrap();
    let watcher = LintWatcher::start(context.clone()).unwrap();
    let checked = |errors| {
        move |event: &LintWatchEvent| {
            matches!(event,
        LintWatchEvent::Checked { snapshot } if snapshot.result.error_count == errors)
        }
    };
    await_event(&watcher, checked(0));
    fs::write(external.join("b.ts"), "import '../project/a';").unwrap();
    await_event(&watcher, checked(1));
    fs::remove_file(external.join("b.ts")).unwrap();
    await_event(&watcher, checked(2));
    fs::write(external.join("b.ts"), "export {};").unwrap();
    await_event(&watcher, checked(0));
    fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
    fs::write(
        root.join("node_modules/pkg/package.json"),
        r#"{"main":"index.js"}"#,
    )
    .unwrap();
    fs::write(root.join("node_modules/pkg/index.js"), "export {};").unwrap();
    fs::write(root.join("a.ts"), "import 'pkg';").unwrap();
    await_event(&watcher, checked(0));
    fs::write(root.join("node_modules/pkg/index.js"), "import '../../a';").unwrap();
    await_event(&watcher, checked(1));
    fs::write(
        root.join("node_modules/pkg/package.json"),
        r#"{"main":"other.js"}"#,
    )
    .unwrap();
    await_event(&watcher, checked(2));
    fs::write(root.join("node_modules/pkg/other.js"), "export {};").unwrap();
    await_event(&watcher, checked(0));
    watcher.stop();
    context.close();
}

#[test]
fn watches_disk_configuration_and_unsaved_documents_and_can_stop_without_closing_context() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.js"), "debugger;").unwrap();
    let context = LintContext::create(LintProjectOptions {
        root: root.path().into(),
        cache: true,
        ..Default::default()
    })
    .unwrap();
    let watcher = LintWatcher::start(context.clone()).unwrap();
    await_event(
        &watcher,
        |event| matches!(event, LintWatchEvent::Checked { snapshot } if snapshot.result.error_count == 1),
    );
    fs::write(root.path().join("a.js"), "run();").unwrap();
    await_event(
        &watcher,
        |event| matches!(event, LintWatchEvent::Checked { snapshot } if snapshot.result.error_count == 0),
    );
    context
        .update_document(LintDocument {
            filename: "new.ts".into(),
            version: 1,
            text: "debugger;".into(),
        })
        .unwrap();
    await_event(
        &watcher,
        |event| matches!(event, LintWatchEvent::Checked { snapshot } if snapshot.documents.get("new.ts") == Some(&1) && snapshot.result.error_count == 1),
    );
    fs::write(root.path().join("wake.config.toml"), "[lint\n").unwrap();
    await_event(
        &watcher,
        |event| matches!(event, LintWatchEvent::Diagnostic { error, .. } if error.code == "WAKE_LINT_CONFIG"),
    );
    fs::write(
        root.path().join("wake.config.toml"),
        "[lint]\nrecommended=false\n",
    )
    .unwrap();
    await_event(
        &watcher,
        |event| matches!(event, LintWatchEvent::Checked { snapshot } if snapshot.result.error_count == 0),
    );
    watcher.stop();
    watcher.stop();
    assert!(!watcher.is_watching());
    assert!(watcher.drain_events().is_empty());
    assert!(!context.is_closed());
    assert_eq!(
        context
            .check(CancellationToken::default())
            .unwrap()
            .result
            .error_count,
        0
    );
    context.close();
}

#[test]
fn watches_created_renamed_and_deleted_sources_and_recovers_a_recreated_root() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().join("project");
    fs::create_dir(&root).unwrap();
    let context = LintContext::create(LintProjectOptions {
        root: root.clone(),
        ..Default::default()
    })
    .unwrap();
    let watcher = LintWatcher::start(context.clone()).unwrap();
    await_event(
        &watcher,
        |event| matches!(event, LintWatchEvent::Checked { snapshot } if snapshot.result.files.is_empty()),
    );
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/a.js"), "debugger;").unwrap();
    await_event(
        &watcher,
        |event| matches!(event, LintWatchEvent::Checked { snapshot } if snapshot.result.files.iter().any(|file| file.path == "src/a.js")),
    );
    fs::rename(root.join("src/a.js"), root.join("src/b.js")).unwrap();
    await_event(
        &watcher,
        |event| matches!(event, LintWatchEvent::Checked { snapshot } if snapshot.result.files.iter().any(|file| file.path == "src/b.js")),
    );
    fs::remove_file(root.join("src/b.js")).unwrap();
    fs::remove_dir(root.join("src")).unwrap();
    fs::remove_dir(&root).unwrap();
    await_event(&watcher, |event| {
        matches!(event, LintWatchEvent::Diagnostic { .. })
    });
    fs::create_dir(&root).unwrap();
    fs::write(root.join("recreated.js"), "debugger;").unwrap();
    await_event(
        &watcher,
        |event| matches!(event, LintWatchEvent::Checked { snapshot } if snapshot.result.files.iter().any(|file| file.path == "recreated.js")),
    );
    context.close();
    watcher.stop();
    assert!(!watcher.is_watching());
}

#[test]
fn initial_invalid_configuration_recovers_and_stop_preserves_a_newer_manual_request() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("wake.config.toml"), "[lint\n").unwrap();
    let options = LintProjectOptions {
        root: root.path().into(),
        ..Default::default()
    };
    assert!(LintContext::create(options.clone()).is_err());
    let context = LintContext::create_for_watch(options).unwrap();
    let watcher = LintWatcher::start(context.clone()).unwrap();
    await_event(
        &watcher,
        |event| matches!(event, LintWatchEvent::Diagnostic { error, .. } if error.code == "WAKE_LINT_CONFIG"),
    );
    fs::write(root.path().join("wake.config.toml"), "[lint]\n").unwrap();
    await_event(&watcher, |event| {
        matches!(event, LintWatchEvent::Checked { .. })
    });
    let request = context.prepare_check(CancellationToken::default()).unwrap();
    watcher.stop();
    assert!(request.run().is_ok());
    context.close();
}

#[test]
fn watches_explicit_baseline_even_inside_an_ignored_directory() {
    use wake_app::{LintBaselineMode, LintBaselineOptions, lint_project};
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join(".wake")).unwrap();
    fs::write(root.path().join("a.js"), "debugger;").unwrap();
    let mut options = LintProjectOptions {
        root: root.path().into(),
        cache: true,
        baseline: Some(LintBaselineOptions {
            path: ".wake/baseline.json".into(),
            mode: LintBaselineMode::Generate,
        }),
        ..Default::default()
    };
    lint_project(options.clone(), &CancellationToken::default()).unwrap();
    options.baseline.as_mut().unwrap().mode = LintBaselineMode::Check;
    let context = LintContext::create(options).unwrap();
    let watcher = LintWatcher::start(context.clone()).unwrap();
    await_event(
        &watcher,
        |event| matches!(event, LintWatchEvent::Checked { snapshot } if snapshot.result.error_count == 0 && snapshot.result.baseline.as_ref().unwrap().suppressed == 1),
    );
    fs::write(
        root.path().join(".wake/baseline.json"),
        r#"{"schema":"wake.lint.baseline.v1","entries":[]}"#,
    )
    .unwrap();
    // Generation cached the same empty baseline before adding the suppression. Reusing that raw
    // diagnostic is valid; the watcher must stop applying the removed entry immediately.
    await_event(
        &watcher,
        |event| matches!(event, LintWatchEvent::Checked { snapshot } if snapshot.result.error_count == 1 && snapshot.result.baseline.as_ref().unwrap().suppressed == 0),
    );
    watcher.stop();
    context.close();
}
