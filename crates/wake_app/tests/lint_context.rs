use std::fs;
use wake_app::{CancellationToken, LintContext, LintDocument, LintProjectOptions};

#[test]
fn context_recovers_from_hook_analysis_limits_using_new_document_versions() {
    let root = tempfile::tempdir().unwrap();
    let context = LintContext::create(LintProjectOptions {
        root: root.path().into(),
        cache: true,
        rules: [(
            "react-hooks/rules-of-hooks".into(),
            serde_json::json!("error"),
        )]
        .into(),
        ..Default::default()
    })
    .unwrap();
    context
        .update_document(LintDocument {
            filename: "app.tsx".into(),
            version: 1,
            text: format!("function App() {{ {} }}", "ordinary();".repeat(60_000)),
        })
        .unwrap();
    assert_eq!(
        context
            .check(CancellationToken::default())
            .unwrap_err()
            .code,
        "WAKE_LINT_ANALYSIS"
    );
    context
        .update_document(LintDocument {
            filename: "app.tsx".into(),
            version: 2,
            text: "import {useState as h} from 'react'; function App() { h(0); }".into(),
        })
        .unwrap();
    let checked = context.check(CancellationToken::default()).unwrap();
    assert_eq!(checked.documents["app.tsx"], 2);
    assert_eq!(checked.result.error_count, 0);
    assert!(!root.path().join(".wake").exists());
    context.close();
}

#[test]
fn context_overlays_multiple_documents_and_reloads_configuration_and_disk_on_close() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.js"), "debugger;").unwrap();
    let context = LintContext::create(LintProjectOptions {
        root: root.path().into(),
        cache: true,
        ..Default::default()
    })
    .unwrap();
    context
        .update_document(LintDocument {
            filename: "a.js".into(),
            version: 1,
            text: "run();".into(),
        })
        .unwrap();
    context
        .update_document(LintDocument {
            filename: "unsaved.ts".into(),
            version: 5,
            text: "debugger;".into(),
        })
        .unwrap();
    let first = context.check(CancellationToken::default()).unwrap();
    assert_eq!(first.result.error_count, 1);
    assert_eq!(first.result.files.len(), 2);
    assert_eq!(first.documents["a.js"], 1);
    assert_eq!(first.result.cache.unwrap().bypassed, 2);
    assert!(!root.path().join(".wake").exists());
    assert_eq!(
        fs::read_to_string(root.path().join("a.js")).unwrap(),
        "debugger;"
    );
    context.close_document("a.js", 2).unwrap();
    context.close_document("unsaved.ts", 6).unwrap();
    let second = context.check(CancellationToken::default()).unwrap();
    assert!(second.generation > first.generation);
    assert_eq!(second.result.files.len(), 1);
    assert_eq!(second.result.error_count, 1);
    fs::write(
        root.path().join("wake.config.toml"),
        "[lint]\nrecommended=false\n",
    )
    .unwrap();
    assert_eq!(
        context
            .check(CancellationToken::default())
            .unwrap()
            .result
            .error_count,
        0
    );
    context.close();
    context.close();
    assert!(context.is_closed());
    assert!(context.check(CancellationToken::default()).is_err());
}

#[test]
fn document_versions_are_strict_and_tombstones_prevent_stale_reopening() {
    let root = tempfile::tempdir().unwrap();
    let context = LintContext::create(LintProjectOptions {
        root: root.path().into(),
        ..Default::default()
    })
    .unwrap();
    let document = |version| LintDocument {
        filename: "new.ts".into(),
        version,
        text: "debugger;".into(),
    };
    context.update_document(document(10)).unwrap();
    assert!(context.update_document(document(10)).is_err());
    assert!(context.close_document("new.ts", 9).is_err());
    context.close_document("new.ts", 11).unwrap();
    assert!(context.update_document(document(10)).is_err());
    context.update_document(document(12)).unwrap();
    assert!(
        context
            .update_document(document(9_007_199_254_740_992))
            .is_err()
    );
    let cancelled = CancellationToken::default();
    cancelled.cancel();
    assert_eq!(context.check(cancelled).unwrap_err().code, "WAKE_CANCELLED");
    assert_eq!(
        context
            .check(CancellationToken::default())
            .unwrap()
            .documents["new.ts"],
        12
    );
    context.close();
    assert!(context.update_document(document(13)).is_err());
    assert!(context.invalidate().is_err());
}

#[test]
fn admission_precedes_worker_execution_and_supersedes_queued_requests() {
    let root = tempfile::tempdir().unwrap();
    let context = LintContext::create(LintProjectOptions {
        root: root.path().into(),
        ..Default::default()
    })
    .unwrap();
    let first = context.prepare_check(CancellationToken::default()).unwrap();
    let second = context.prepare_check(CancellationToken::default()).unwrap();
    assert_eq!(first.run().unwrap_err().code, "WAKE_CANCELLED");
    let current = second.run().unwrap();
    assert_eq!(current.generation, context.generation());
    let last = context.prepare_check(CancellationToken::default()).unwrap();
    context.request_close();
    assert_eq!(last.run().unwrap_err().code, "WAKE_CANCELLED");
    context.close();
}
