use std::collections::BTreeMap;

use wake_browser::{BrowserBuildContext, BrowserBuildOptions};

fn options() -> BrowserBuildOptions {
    serde_json::from_value(serde_json::json!({
        "federation": {
            "enabled": true, "name": "preview",
            "exposes": {"./page": {"entry": "src/page.ts"}}
        }
    }))
    .unwrap()
}

fn project() -> BTreeMap<String, Vec<u8>> {
    [
        ("src/page.ts", b"import data from './data.json'; import styles from './page.module.css'; import image from './image.png'; export const page = { data, styles, image }; export const lazy = () => import('./lazy.ts');".as_slice()),
        ("src/data.json", b"{\"count\":42}".as_slice()),
        ("src/page.module.css", b".root { color: red; background: url('./image.png'); }".as_slice()),
        ("src/image.png", b"\x89PNG\r\n\x1a\n".as_slice()),
        ("src/lazy.ts", b"export const value: number = 7;".as_slice()),
    ].into_iter().map(|(path, bytes)| (path.to_owned(), bytes.to_vec())).collect()
}

#[test]
fn complete_virtual_project_returns_valid_federation_assets() {
    let mut context = BrowserBuildContext::new(options()).unwrap();
    let output = context.build(project()).unwrap();
    output.manifest.validate().unwrap();
    assert_eq!(output.manifest.name.as_str(), "preview");
    assert!(output.files.iter().any(|file| file.path.ends_with(".css")));
    assert!(output.files.iter().any(|file| file.path.ends_with(".png")));
    assert!(output.files.iter().any(|file| file.path.ends_with(".map")));
    assert!(
        !output
            .manifest
            .exposes
            .values()
            .next()
            .unwrap()
            .asynchronous_assets
            .is_empty()
    );
    for file in &output.files {
        assert!(!file.path.starts_with('/'));
        assert!(!file.path.contains('\\'));
    }
}

#[test]
fn failure_and_structural_edits_preserve_accepted_generation() {
    let mut context = BrowserBuildContext::new(options()).unwrap();
    let original = context.build(project()).unwrap();
    let mut broken = project();
    broken.remove("src/lazy.ts");
    assert!(context.build(broken).is_err());
    assert_eq!(
        context.current().unwrap().manifest.build_id,
        original.manifest.build_id
    );
    let mut changed = project();
    changed.insert(
        "src/lazy.ts".into(),
        b"export const value: number = 9;".to_vec(),
    );
    let updated = context.build(changed.clone()).unwrap();
    assert_ne!(updated.manifest.build_id, original.manifest.build_id);
    let cold = BrowserBuildContext::new(options())
        .unwrap()
        .build(changed)
        .unwrap();
    assert_eq!(updated.manifest, cold.manifest);
    assert_eq!(updated.files, cold.files);
}

#[test]
fn unsafe_virtual_paths_and_file_directory_collisions_are_rejected() {
    for invalid in [
        "../escape.js",
        "/absolute.js",
        "C:/absolute.js",
        "src\\page.ts",
        "src/./page.ts",
        "src//page.ts",
        "src/%2fpage.ts",
    ] {
        let mut files = project();
        files.insert(invalid.to_owned(), b"export {};".to_vec());
        assert!(
            BrowserBuildContext::new(options())
                .unwrap()
                .build(files)
                .is_err(),
            "{invalid}"
        );
    }
    let mut files = project();
    files.insert("src".into(), Vec::new());
    assert!(
        BrowserBuildContext::new(options())
            .unwrap()
            .build(files)
            .is_err()
    );
}

#[test]
fn missing_shared_group_fails_closed() {
    let mut options = options();
    let expose = options.federation.exposes.values_mut().next().unwrap();
    expose.mode = wake_federation_contract::ExposeMode::HostRendered;
    expose.scope = "preview".into();
    assert!(BrowserBuildContext::new(options).is_err());
}

#[test]
fn source_map_only_edits_change_the_publication_identity() {
    let mut files = project();
    let mut context = BrowserBuildContext::new(options()).unwrap();
    let before = context.build(files.clone()).unwrap();
    let source = files.get_mut("src/lazy.ts").unwrap();
    source.insert(0, b'\n');
    let after = context.build(files).unwrap();
    assert_ne!(before.files, after.files);
    assert_ne!(before.manifest.build_id, after.manifest.build_id);
}

#[test]
fn custom_asset_base_is_rejected_without_a_matching_store() {
    let mut value = options();
    value.asset_base = "/custom/assets/".into();
    assert!(BrowserBuildContext::new(value).is_err());
}

#[test]
fn unchanged_snapshot_reuses_compilation_without_changing_bytes() {
    let mut context = BrowserBuildContext::new(options()).unwrap();
    let first = context.build(project()).unwrap();
    let second = context.build(project()).unwrap();
    assert_eq!(first.files, second.files);
    assert_eq!(first.manifest, second.manifest);
    assert_eq!(second.updated_module_count, 0);
}

#[test]
fn package_metadata_edits_match_a_cold_build() {
    let mut files = BTreeMap::from([
        (
            "src/page.ts".to_owned(),
            b"export { value } from 'dependency';".to_vec(),
        ),
        (
            "node_modules/dependency/package.json".to_owned(),
            br#"{"name":"dependency","version":"1.0.0","exports":"./one.js"}"#.to_vec(),
        ),
        (
            "node_modules/dependency/one.js".to_owned(),
            b"export const value = 1;".to_vec(),
        ),
        (
            "node_modules/dependency/two.js".to_owned(),
            b"export const value = 2;".to_vec(),
        ),
    ]);
    let mut context = BrowserBuildContext::new(options()).unwrap();
    let original = context.build(files.clone()).unwrap();
    files.insert(
        "node_modules/dependency/package.json".into(),
        br#"{"name":"dependency","version":"1.0.1","exports":"./two.js"}"#.to_vec(),
    );
    let updated = context.build(files.clone()).unwrap();
    let cold = BrowserBuildContext::new(options())
        .unwrap()
        .build(files)
        .unwrap();
    assert_ne!(original.manifest.build_id, updated.manifest.build_id);
    assert_eq!(updated.files, cold.files);
}

#[test]
fn unsupported_isolated_preview_is_rejected_at_configuration() {
    let mut value = options();
    let expose = value.federation.exposes.values_mut().next().unwrap();
    expose.mode = wake_federation_contract::ExposeMode::Isolated;
    expose.shadow = wake_federation_contract::ShadowMode::Open;
    expose.scope = "isolated".into();
    assert!(BrowserBuildContext::new(value).is_err());
}

#[test]
fn host_shared_identity_is_preserved_and_checked_against_virtual_dependencies() {
    let config = serde_json::json!({
        "federation": { "enabled": true, "name": "preview", "exposes": {"./page": {"entry": "page.ts"}},
            "shared": { "dependency": {"fallback": false, "requiredVersion": "1.0.0"} } },
        "hostShared": { "dependency": {"name":"dependency","version":"1.0.0","packageContext":"default","buildVariant":"actual-host-build"} }
    });
    let mut context = BrowserBuildContext::new(serde_json::from_value(config).unwrap()).unwrap();
    let mut files = BTreeMap::from([
        (
            "page.ts".into(),
            b"export { value } from 'dependency';".to_vec(),
        ),
        (
            "node_modules/dependency/package.json".into(),
            br#"{"name":"dependency","version":"1.0.0","main":"index.js"}"#.to_vec(),
        ),
        (
            "node_modules/dependency/index.js".into(),
            b"export const value = 1;".to_vec(),
        ),
    ]);
    let output = context.build(files.clone()).unwrap();
    assert_eq!(
        output.manifest.shared.requirements[0].build_variant,
        "actual-host-build"
    );
    files.insert(
        "node_modules/dependency/package.json".into(),
        br#"{"name":"dependency","version":"2.0.0","main":"index.js"}"#.to_vec(),
    );
    assert_eq!(
        context.build(files).unwrap_err().code,
        "FED_SHARE_UNSATISFIABLE"
    );
}
