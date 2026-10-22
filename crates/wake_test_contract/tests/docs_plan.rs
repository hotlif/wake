use wake_test_contract::TestOptions;

#[test]
fn docs_plan_round_trips_and_regular_tests_omit_it() {
    let value = serde_json::json!({
        "docsPlan": {
            "schemaVersion": "wake.docs-test.v1",
            "outputDir": "build",
            "basePath": "/wake-docs-test-token/",
            "demos": [{"id": "examples/counter.demo.tsx", "path": "docs/examples/counter.demo.tsx", "title": "Counter"}],
            "themes": ["light", "dark"],
            "viewports": [{"width": 390, "height": 844}]
        }
    });
    let options: TestOptions =
        serde_json::from_value(value.clone()).expect("Docs plans are supported");
    assert_eq!(
        serde_json::to_value(options).unwrap()["docsPlan"],
        value["docsPlan"]
    );
    assert!(
        serde_json::to_value(TestOptions::default())
            .unwrap()
            .get("docsPlan")
            .is_none()
    );
}

#[test]
fn docs_plan_rejects_unknown_fields() {
    assert!(
        serde_json::from_value::<TestOptions>(serde_json::json!({
            "docsPlan": {"schemaVersion": "wake.docs-test.v1", "unsupported": true}
        }))
        .is_err()
    );
}
