//! Private development entry inventory; HTTP/build ownership stays with the application/server.
//!
//! Shared adapters are CommonJS modules (ADR 0046), including their loader dependency. ESM default,
//! named and namespace imports and CommonJS consumers must all observe the host's original object,
//! including consumers in dynamically imported chunks. ESM syntax in an adapter would incorrectly
//! advertise an ESM default export while `module.exports` actually returns the shared object.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevModule {
    pub key: String,
    pub entry_relative: PathBuf,
    pub base_path: String,
}

pub const DEV_SHARED_REQUESTS: &[&str] = &[
    "react",
    "react/jsx-runtime",
    "react/jsx-dev-runtime",
    "react-dom",
    "react-dom/client",
    "@@wake/docs/runtime/app.tsx",
];

pub fn dev_module_key(kind: &str, id: &str) -> String {
    let mut key = format!("{kind}-");
    for byte in id.as_bytes() {
        use std::fmt::Write;
        write!(key, "{byte:02x}").expect("string write");
    }
    key
}

/// Render a Site for a server-owned fixed lazy inventory. `None` discovers the initial inventory;
/// newly added entries outside `selected` keep ordinary imports until the server restarts.
pub fn render_for_dev_server(
    root: impl AsRef<Path>,
    options: &DocsOptions,
    selected: Option<&BTreeSet<String>>,
) -> Result<RenderedProject, DocsError> {
    render_prepared(
        prepare_render_inputs(root.as_ref(), options)?,
        options,
        BuildMode::Development,
        DocsMode::Site,
        Some(selected),
    )
}

pub(super) fn render_dev_entries(
    generation: &mut RenderedFileTreeBuilder,
    pages: &[(PathBuf, CompiledPage)],
    demos: &[DemoInfo],
    base_path: &str,
    selected: Option<&BTreeSet<String>>,
) -> Result<Vec<DevModule>, DocsError> {
    let mut entries = Vec::new();
    for (_, page) in pages {
        let key = dev_module_key("page", &page.route.id);
        if selected.is_some_and(|selected| !selected.contains(&key)) {
            continue;
        }
        let source = format!(
            "import * as value from {};\nimport {{ publishDevModule }} from '@@wake/docs/runtime/dev-loader.mjs';\npublishDevModule({}, {}, value);\n",
            js_string(&format!("@@wake/docs/{}", page.identity.generated_module)),
            js_string(base_path),
            js_string(&key),
        );
        insert_entry(generation, &mut entries, base_path, key, source)?;
    }
    for demo in demos {
        let key = dev_module_key("demo", &demo.id);
        if selected.is_some_and(|selected| !selected.contains(&key)) {
            continue;
        }
        let source = format!(
            "import * as demo from {};\nimport * as source from {};\nimport {{ publishDevModule }} from '@@wake/docs/runtime/dev-loader.mjs';\npublishDevModule({}, {}, {{ demo, source }});\n",
            js_string(&demo.import_path),
            js_string(&format!("@@wake/docs/demo-source/{}", demo.source_module)),
            js_string(base_path),
            js_string(&key),
        );
        insert_entry(generation, &mut entries, base_path, key, source)?;
    }
    insert_generation_file(
        generation,
        PathBuf::from("runtime/dev-loader.mjs"),
        include_bytes!("../runtime/dev-loader.mjs"),
    )?;
    let mut host =
        String::from("import { docsDevContext } from '@@wake/docs/runtime/dev-loader.mjs';\n");
    for (index, request) in DEV_SHARED_REQUESTS.iter().enumerate() {
        host.push_str(&format!(
            "import * as shared{index} from {};\n",
            js_string(request)
        ));
    }
    host.push_str(&format!(
        "const context = docsDevContext({});\n",
        js_string(base_path)
    ));
    for (index, request) in DEV_SHARED_REQUESTS.iter().enumerate() {
        host.push_str(&format!(
            "context.shared.set({}, shared{index}.default || shared{index});\n",
            js_string(request)
        ));
        let shim = format!(
            "const {{ docsDevContext }} = require('@@wake/docs/runtime/dev-loader.mjs');\nmodule.exports = docsDevContext({}).shared.get({});\nif (!module.exports) throw new Error('Docs host shared module is unavailable');\n",
            js_string(base_path),
            js_string(request),
        );
        insert_generation_file(
            generation,
            PathBuf::from(format!("dev/shared/{index}.js")),
            shim.as_bytes(),
        )?;
    }
    insert_generation_file(generation, PathBuf::from("dev/host.ts"), host.as_bytes())?;
    Ok(entries)
}

fn insert_entry(
    generation: &mut RenderedFileTreeBuilder,
    entries: &mut Vec<DevModule>,
    base_path: &str,
    key: String,
    source: String,
) -> Result<(), DocsError> {
    let entry_relative = PathBuf::from(format!("dev/entries/{key}.ts"));
    insert_generation_file(generation, entry_relative.clone(), source.as_bytes())?;
    entries.push(DevModule {
        base_path: format!("{}/@wake/docs/{key}/", base_path.trim_end_matches('/')),
        key,
        entry_relative,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demand_inventory_keeps_new_entries_eager_and_production_self_contained() {
        let root = crate::tests::fixture();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();
        crate::tests::write_fixture_navigation(&root);
        let options = DocsOptions {
            base_path: "/docs/".into(),
            ..DocsOptions::default()
        };
        let first = render_for_dev_server(&root, &options, None).unwrap();
        let key = dev_module_key("page", "index");
        assert!(
            first.dev_modules.iter().any(|module| module.key == key
                && module.base_path == format!("/docs/@wake/docs/{key}/"))
        );
        let snapshot = crate::tests::rendered_file_snapshot(&first);
        let registry = std::str::from_utf8(&snapshot["registry.ts"]).unwrap();
        assert!(registry.contains("loadDevModule"));
        assert!(!registry.contains("import(\"@@wake/docs/pages/index.tsx\")"));
        let empty = BTreeSet::new();
        let fallback = render_for_dev_server(&root, &options, Some(&empty)).unwrap();
        let snapshot = crate::tests::rendered_file_snapshot(&fallback);
        assert!(
            std::str::from_utf8(&snapshot["registry.ts"])
                .unwrap()
                .contains("import(\"@@wake/docs/pages/index.tsx\")")
        );
        let production =
            render_with_mode(&root, &options, BuildMode::Production, DocsMode::Site).unwrap();
        assert!(production.dev_modules.is_empty());
        assert!(
            !production
                .files
                .inventory()
                .any(|path| path.as_path().starts_with("dev")
                    || path.as_path().ends_with("dev-loader.mjs"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn demand_keys_preserve_unicode_and_kind_identity() {
        assert_ne!(dev_module_key("page", "a/b"), dev_module_key("page", "a-b"));
        assert_ne!(
            dev_module_key("page", "首页"),
            dev_module_key("demo", "首页")
        );
        assert_eq!(dev_module_key("page", "首页"), "page-e9a696e9a1b5");
    }
}
