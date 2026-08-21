//! Wake-native React component documentation compiler.
//!
//! MDX is parsed to markdown-rs mdast, rendered to TSX, and then handed to Wake's existing
//! compiler. Generated modules live under .wake/docs/generated for dev and production parity.

use markdown::mdast::{AttributeContent, AttributeValue, Node};
use markdown::{Constructs, ParseOptions};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{
    Mutex, OnceLock,
    atomic::{AtomicU64, Ordering},
};

const RUNTIME_APP: &str = include_str!("../runtime/app.tsx");
const RUNTIME_COMPONENTS: &str = include_str!("../runtime/components.tsx");
const RUNTIME_COMPONENT_STATE: &str = include_str!("../runtime/components-state.mjs");
const RUNTIME_SITE_ENTRY: &str = include_str!("../runtime/site-entry.tsx");
const RUNTIME_COMPONENTS_ENTRY: &str = include_str!("../runtime/components-entry.tsx");
const RUNTIME_STYLE: &str = include_str!("../runtime/styles.css");
const RUNTIME_COMPONENT_STYLE: &str = include_str!("../runtime/components.css");
const MINIMUM_REACT_MAJOR: u64 = 19;
static ATOMIC_WRITE_LOCK: Mutex<()> = Mutex::new(());
static NEXT_ATOMIC_WRITE_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildMode {
    Development,
    Production,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocsMode {
    #[default]
    Site,
    Components,
}

impl DocsMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Site => "site",
            Self::Components => "components",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DocsOptions {
    pub source_dir: PathBuf,
    pub title: String,
    pub description: String,
    pub locale: String,
    pub logo: Option<String>,
    pub repository_url: Option<String>,
    pub base_path: String,
    pub preview: Option<PathBuf>,
    pub theme_css: Option<PathBuf>,
    pub default_theme: String,
    pub accent_color: Option<String>,
}

impl Default for DocsOptions {
    fn default() -> Self {
        Self {
            source_dir: PathBuf::from("docs"),
            title: "Wake Docs".to_string(),
            description: String::new(),
            locale: "zh-CN".to_string(),
            logo: None,
            repository_url: None,
            base_path: "/".to_string(),
            preview: None,
            theme_css: None,
            default_theme: "system".to_string(),
            accent_color: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GeneratedProject {
    pub root: PathBuf,
    pub generated_dir: PathBuf,
    pub entry: PathBuf,
    pub aliases: Vec<(String, PathBuf)>,
    pub watch_roots: Vec<PathBuf>,
    pub routes: Vec<RouteInfo>,
    pub mode: DocsMode,
    pub demos: Vec<DemoDescriptor>,
    pub warnings: Vec<String>,
    pub changed_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouteInfo {
    pub id: String,
    pub file: String,
    pub title: String,
    pub description: String,
    pub kind: String,
    pub group: String,
    pub group_id: String,
    pub section: String,
    pub section_id: String,
    pub slug: String,
    pub status: String,
    pub draft: bool,
    pub hidden: bool,
    pub headings: Vec<HeadingInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeadingInfo {
    pub depth: u8,
    pub title: String,
    pub id: String,
}

#[derive(Debug)]
pub enum DocsError {
    Io(PathBuf, String),
    Mdx(PathBuf, String),
    Frontmatter(PathBuf, String),
    Navigation(PathBuf, String),
    InvalidMacro {
        path: PathBuf,
        line: usize,
        column: usize,
        message: String,
    },
    DuplicateRoute {
        slug: String,
        first: PathBuf,
        second: PathBuf,
    },
    InvalidConfig(String),
    PublicCollision(PathBuf),
    Api(PathBuf, String),
}

impl fmt::Display for DocsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(path, error) => write!(f, "cannot access `{}`: {error}", path.display()),
            Self::Mdx(path, error) => write!(f, "{}: {error}", path.display()),
            Self::Frontmatter(path, error) => write!(
                f,
                "invalid TOML frontmatter in `{}`: {error}",
                path.display()
            ),
            Self::Navigation(path, error) => write!(
                f,
                "invalid documentation navigation `{}`: {error}",
                path.display()
            ),
            Self::InvalidMacro {
                path,
                line,
                column,
                message,
            } => write!(f, "{}:{line}:{column}: {message}", path.display()),
            Self::DuplicateRoute {
                slug,
                first,
                second,
            } => write!(
                f,
                "duplicate docs route `{slug}` from `{}` and `{}`",
                first.display(),
                second.display()
            ),
            Self::InvalidConfig(message) => write!(f, "invalid docs configuration: {message}"),
            Self::PublicCollision(path) => write!(
                f,
                "public asset collides with build output `{}`",
                path.display()
            ),
            Self::Api(path, error) => {
                write!(f, "API docs failed for `{}`: {error}", path.display())
            }
        }
    }
}

impl std::error::Error for DocsError {}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Frontmatter {
    title: Option<String>,
    description: String,
    kind: Option<String>,
    status: Option<String>,
    draft: bool,
    hidden: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct NavigationConfig {
    #[serde(rename = "group")]
    groups: Vec<NavigationGroup>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct NavigationGroup {
    id: String,
    title: String,
    #[serde(default)]
    pages: Vec<String>,
    #[serde(default, rename = "section")]
    sections: Vec<NavigationSection>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct NavigationSection {
    id: String,
    title: String,
    pages: Vec<String>,
}

#[derive(Debug)]
struct CompiledPage {
    route: RouteInfo,
    module: String,
    source_map: String,
    api_entries: Vec<ApiEntry>,
}

#[derive(Debug, Serialize)]
struct ApiEntry {
    key: String,
    value: wake_tsdoc::ApiDoc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DemoDescriptor {
    pub id: String,
    pub title: String,
    pub group: String,
    pub component: String,
    pub order: i32,
    pub control_count: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DemoControl {
    name: String,
    type_text: String,
    kind: String,
    required: bool,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_value: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deprecated: Option<String>,
    options: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize)]
struct DemoInfo {
    id: String,
    title: String,
    group: String,
    component: String,
    order: i32,
    controls: Vec<DemoControl>,
    warnings: Vec<String>,
    source: String,
    source_module: String,
    import_path: String,
}

impl DemoInfo {
    fn descriptor(&self) -> DemoDescriptor {
        DemoDescriptor {
            id: self.id.clone(),
            title: self.title.clone(),
            group: self.group.clone(),
            component: self.component.clone(),
            order: self.order,
            control_count: self
                .controls
                .iter()
                .filter(|control| control.kind != "readonly")
                .count(),
            warnings: self.warnings.clone(),
        }
    }
}

/// Generate the normal documentation site. This compatibility wrapper keeps the pre-workbench
/// API and behavior unchanged.
pub fn generate(
    project_root: impl AsRef<Path>,
    options: &DocsOptions,
    mode: BuildMode,
) -> Result<GeneratedProject, DocsError> {
    generate_with_mode(project_root, options, mode, DocsMode::Site)
}
/// Scan, compile, and atomically materialize the generated docs module tree.
pub fn generate_with_mode(
    project_root: impl AsRef<Path>,
    options: &DocsOptions,
    mode: BuildMode,
    docs_mode: DocsMode,
) -> Result<GeneratedProject, DocsError> {
    validate_options(options)?;
    let root = canonical_dir(project_root.as_ref())?;
    validate_react_dependencies(&root)?;
    let source_dir = absolute_from(&root, &options.source_dir);
    if !source_dir.is_dir() {
        return Err(DocsError::Io(
            source_dir,
            "docs source directory does not exist".to_string(),
        ));
    }
    let source_dir = fs::canonicalize(&source_dir).unwrap_or(source_dir);
    let generated_dir = root.join(".wake/docs/generated");
    fs::create_dir_all(&generated_dir)
        .map_err(|error| DocsError::Io(generated_dir.clone(), error.to_string()))?;

    let mut mdx_files = Vec::new();
    let mut demo_files = Vec::new();
    scan_files(&source_dir, &mut mdx_files, &mut demo_files)?;
    mdx_files.sort();
    demo_files.sort();
    if docs_mode == DocsMode::Components {
        mdx_files.clear();
    }

    let mut pages = Vec::new();
    for path in &mdx_files {
        let page = compile_page(&root, &source_dir, path)?;
        pages.push((path.clone(), page));
    }
    if docs_mode == DocsMode::Site {
        apply_navigation(&source_dir, &mut pages)?;
        for (_, page) in &mut pages {
            sync_page_metadata(page);
        }
    }
    if mode == BuildMode::Production {
        pages.retain(|(_, page)| !page.route.draft);
    }
    ensure_unique_routes(&pages)?;

    let demos = compile_demos(&root, &source_dir, &demo_files, docs_mode)?;
    let mut changed_files = Vec::new();
    let mut generated_files = BTreeSet::new();
    for (source_path, page) in &pages {
        let relative = source_path.strip_prefix(&source_dir).map_err(|_| {
            DocsError::InvalidConfig(format!(
                "page `{}` is outside source_dir",
                source_path.display()
            ))
        })?;
        let output = generated_dir
            .join("pages")
            .join(relative)
            .with_extension("tsx");
        let map = output.with_extension("tsx.map");
        if atomic_write_if_changed(&output, page.module.as_bytes())? {
            changed_files.push(output.clone());
        }
        if atomic_write_if_changed(&map, page.source_map.as_bytes())? {
            changed_files.push(map.clone());
        }
        generated_files.insert(output);
        generated_files.insert(map);
    }
    for demo in &demos {
        let output = generated_dir.join("demo-source").join(&demo.source_module);
        let language = normalize_code_language(
            Path::new(&demo.id)
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("text"),
        );
        let highlighted = render_highlighted_code_lines(&demo.source, &language, &BTreeSet::new());
        let module = format!(
            "export default {};\nexport const language = {};\nexport const highlighted = <>{highlighted}</>;\n",
            js_string(&demo.source),
            js_string(&language)
        );
        if atomic_write_if_changed(&output, module.as_bytes())? {
            changed_files.push(output.clone());
        }
        generated_files.insert(output);
    }

    let routes: Vec<_> = pages.iter().map(|(_, page)| page.route.clone()).collect();
    let api_entries: Vec<_> = pages
        .iter()
        .flat_map(|(_, page)| page.api_entries.iter())
        .collect();
    let registry = render_registry(&source_dir, &pages, &demos, &api_entries)?;
    let config = render_config(&root, options, docs_mode)?;
    let mut fixed = vec![
        ("registry.ts", registry.as_str()),
        ("config.tsx", config.as_str()),
        ("runtime/app.tsx", RUNTIME_APP),
        ("runtime/styles.css", RUNTIME_STYLE),
    ];
    let entry_relative = match docs_mode {
        DocsMode::Site => {
            fixed.push(("runtime/site-entry.tsx", RUNTIME_SITE_ENTRY));
            "runtime/site-entry.tsx"
        }
        DocsMode::Components => {
            fixed.extend([
                ("runtime/components.tsx", RUNTIME_COMPONENTS),
                ("runtime/components-state.mjs", RUNTIME_COMPONENT_STATE),
                ("runtime/components-entry.tsx", RUNTIME_COMPONENTS_ENTRY),
                ("runtime/components.css", RUNTIME_COMPONENT_STYLE),
            ]);
            "runtime/components-entry.tsx"
        }
    };
    for (relative, content) in fixed {
        let path = generated_dir.join(relative);
        if atomic_write_if_changed(&path, content.as_bytes())? {
            changed_files.push(path.clone());
        }
        generated_files.insert(path);
    }

    remove_stale_generated_files(&generated_dir, &generated_files, &mut changed_files)?;
    let manifest_path = generated_dir.join("manifest.json");
    let manifest_files: Vec<_> = generated_files
        .iter()
        .filter_map(|path| path.strip_prefix(&generated_dir).ok())
        .map(slash_path)
        .collect();
    let manifest = serde_json::to_string_pretty(&json!({ "files": manifest_files }))
        .expect("serializable manifest");
    if atomic_write_if_changed(&manifest_path, manifest.as_bytes())? {
        changed_files.push(manifest_path);
    }

    let mut watch_roots = vec![source_dir.clone(), root.join("src")];
    if let Some(preview) = &options.preview {
        watch_roots.push(absolute_from(&root, preview));
    }
    if let Some(theme) = &options.theme_css {
        watch_roots.push(absolute_from(&root, theme));
    }
    watch_roots.sort();
    watch_roots.dedup();

    let demo_descriptors = demos.iter().map(DemoInfo::descriptor).collect();
    let warnings = demos
        .iter()
        .flat_map(|demo| demo.warnings.iter().cloned())
        .collect();

    Ok(GeneratedProject {
        root: root.clone(),
        generated_dir: generated_dir.clone(),
        entry: generated_dir.join(entry_relative),
        aliases: vec![
            ("@wake/docs".to_string(), generated_dir),
            ("@wake/docs-project".to_string(), root),
        ],
        watch_roots,
        routes,
        mode: docs_mode,
        demos: demo_descriptors,
        warnings,
        changed_files,
    })
}

fn sync_page_metadata(page: &mut CompiledPage) {
    const PREFIX: &str = "export const __wakeMeta = ";
    let Some(start) = page.module.find(PREFIX) else {
        return;
    };
    let value_start = start + PREFIX.len();
    let Some(value_end) = page.module[value_start..].find(";\n") else {
        return;
    };
    let value_end = value_start + value_end;
    let metadata = serde_json::to_string(&page.route).expect("serializable route");
    page.module.replace_range(value_start..value_end, &metadata);
}

fn compile_page(root: &Path, source_dir: &Path, path: &Path) -> Result<CompiledPage, DocsError> {
    let source = fs::read_to_string(path)
        .map_err(|error| DocsError::Io(path.to_path_buf(), error.to_string()))?;
    let (esm, markdown_source) = extract_esm(&source);
    let mut constructs = Constructs::gfm();
    constructs.autolink = false;
    constructs.code_indented = false;
    constructs.html_flow = false;
    constructs.html_text = false;
    constructs.frontmatter = true;
    constructs.mdx_esm = false;
    constructs.mdx_expression_flow = true;
    constructs.mdx_expression_text = true;
    constructs.mdx_jsx_flow = true;
    constructs.mdx_jsx_text = true;
    let ast = markdown::to_mdast(
        &markdown_source,
        &ParseOptions {
            constructs,
            ..ParseOptions::default()
        },
    )
    .map_err(|error| DocsError::Mdx(path.to_path_buf(), error.to_string()))?;
    validate_compile_components(path, &ast)?;

    let frontmatter = find_frontmatter(path, &ast)?;
    let headings = collect_headings(&ast);
    let relative = path.strip_prefix(source_dir).map_err(|_| {
        DocsError::InvalidConfig(format!("page `{}` is outside source_dir", path.display()))
    })?;
    let title = frontmatter
        .title
        .clone()
        .or_else(|| headings.first().map(|heading| heading.title.clone()))
        .unwrap_or_else(|| {
            title_case(
                path.file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("Page"),
            )
        });
    let kind = frontmatter.kind.unwrap_or_else(|| "guide".to_string());
    if !matches!(
        kind.as_str(),
        "overview" | "tutorial" | "guide" | "reference" | "component"
    ) {
        return Err(DocsError::Frontmatter(
            path.to_path_buf(),
            format!("kind `{kind}` must be overview, tutorial, guide, reference, or component"),
        ));
    }
    let slug = normalize_slug(&derive_slug(relative));
    let status = frontmatter.status.unwrap_or_else(|| "stable".to_string());
    if !matches!(
        status.as_str(),
        "stable" | "beta" | "experimental" | "deprecated"
    ) {
        return Err(DocsError::Frontmatter(
            path.to_path_buf(),
            format!("status `{status}` must be stable, beta, experimental, or deprecated"),
        ));
    }
    let file = slash_path(path.strip_prefix(root).unwrap_or(path));
    let route = RouteInfo {
        id: slash_path(relative.with_extension("")),
        file,
        title,
        description: frontmatter.description,
        kind,
        group: String::new(),
        group_id: String::new(),
        section: String::new(),
        section_id: String::new(),
        slug,
        status,
        draft: frontmatter.draft,
        hidden: frontmatter.hidden,
        headings,
    };

    let mut renderer = Renderer::new(&route.file);
    let body = renderer.render_root(&ast);
    let rewritten_esm = rewrite_relative_imports(root, path, &esm);
    let meta_json = serde_json::to_string(&route).expect("serializable route");
    let mut module = format!(
        "import {{ MdxPage, Demo, Demos, API, CodeBlock }} from \"@wake/docs/runtime/app.tsx\";\n{rewritten_esm}\nexport const __wakeMeta = {meta_json};\nexport default function WakeMdxContent() {{\n  return <MdxPage meta={{__wakeMeta}}>\n{body}  </MdxPage>;\n}}\n"
    );
    let map_name = path
        .with_extension("tsx.map")
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("page.tsx.map")
        .to_string();
    module.push_str(&format!("//# sourceMappingURL={map_name}\n"));
    let source_map = render_source_map(path, &source, &module);
    let api_entries = collect_api_entries(root, path, &route.file, &ast)?;
    Ok(CompiledPage {
        route,
        module,
        source_map,
        api_entries,
    })
}

fn find_frontmatter(path: &Path, ast: &Node) -> Result<Frontmatter, DocsError> {
    for node in ast.children().map(Vec::as_slice).unwrap_or_default() {
        if let Node::Toml(frontmatter) = node {
            return toml::from_str(&frontmatter.value)
                .map_err(|error| DocsError::Frontmatter(path.to_path_buf(), error.to_string()));
        }
    }
    Ok(Frontmatter::default())
}

fn apply_navigation(
    source_dir: &Path,
    pages: &mut [(PathBuf, CompiledPage)],
) -> Result<(), DocsError> {
    let path = source_dir.join("navigation.toml");
    let source = fs::read_to_string(&path)
        .map_err(|error| DocsError::Navigation(path.clone(), error.to_string()))?;
    let navigation: NavigationConfig = toml::from_str(&source)
        .map_err(|error| DocsError::Navigation(path.clone(), error.to_string()))?;
    if navigation.groups.is_empty() {
        return Err(DocsError::Navigation(
            path,
            "at least one [[group]] is required".to_string(),
        ));
    }

    let mut group_ids = BTreeSet::new();
    let mut placements = BTreeMap::<String, (usize, String, String, String, String)>::new();
    let mut order = 0usize;
    for group in &navigation.groups {
        validate_navigation_id(&path, "group", &group.id)?;
        if group.title.trim().is_empty() {
            return Err(DocsError::Navigation(
                path.clone(),
                format!("group `{}` must have a title", group.id),
            ));
        }
        if !group_ids.insert(group.id.clone()) {
            return Err(DocsError::Navigation(
                path.clone(),
                format!("duplicate group id `{}`", group.id),
            ));
        }
        for page in &group.pages {
            insert_navigation_page(&path, &mut placements, page, order, group, None)?;
            order += 1;
        }
        let mut section_ids = BTreeSet::new();
        for section in &group.sections {
            validate_navigation_id(&path, "section", &section.id)?;
            if section.title.trim().is_empty() || section.pages.is_empty() {
                return Err(DocsError::Navigation(
                    path.clone(),
                    format!(
                        "section `{}/{}` must have a title and at least one page",
                        group.id, section.id
                    ),
                ));
            }
            if !section_ids.insert(section.id.clone()) {
                return Err(DocsError::Navigation(
                    path.clone(),
                    format!("duplicate section id `{}/{}`", group.id, section.id),
                ));
            }
            for page in &section.pages {
                insert_navigation_page(&path, &mut placements, page, order, group, Some(section))?;
                order += 1;
            }
        }
    }

    let known = pages
        .iter()
        .map(|(_, page)| page.route.id.clone())
        .collect::<BTreeSet<_>>();
    for id in placements.keys() {
        if !known.contains(id) {
            return Err(DocsError::Navigation(
                path.clone(),
                format!("navigation references missing page `{id}`"),
            ));
        }
    }
    for (_, page) in pages.iter_mut() {
        if let Some((_, group_id, group, section_id, section)) = placements.get(&page.route.id) {
            page.route.group_id.clone_from(group_id);
            page.route.group.clone_from(group);
            page.route.section_id.clone_from(section_id);
            page.route.section.clone_from(section);
        } else if !page.route.hidden {
            return Err(DocsError::Navigation(
                path.clone(),
                format!(
                    "page `{}` is not listed in navigation.toml; list it or set hidden = true",
                    page.route.id
                ),
            ));
        }
    }
    pages.sort_by_key(|(_, page)| {
        placements
            .get(&page.route.id)
            .map_or(usize::MAX, |placement| placement.0)
    });
    Ok(())
}

fn validate_navigation_id(path: &Path, kind: &str, id: &str) -> Result<(), DocsError> {
    let valid = !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
    if valid {
        Ok(())
    } else {
        Err(DocsError::Navigation(
            path.to_path_buf(),
            format!("{kind} id `{id}` must use lowercase ASCII letters, digits, and hyphens"),
        ))
    }
}

fn normalized_page_id(value: &str) -> String {
    let value = value.trim().trim_matches('/').replace('\\', "/");
    value.strip_suffix(".mdx").unwrap_or(&value).to_string()
}

fn insert_navigation_page(
    path: &Path,
    placements: &mut BTreeMap<String, (usize, String, String, String, String)>,
    page: &str,
    order: usize,
    group: &NavigationGroup,
    section: Option<&NavigationSection>,
) -> Result<(), DocsError> {
    let id = normalized_page_id(page);
    if id.is_empty() {
        return Err(DocsError::Navigation(
            path.to_path_buf(),
            format!("group `{}` contains an empty page id", group.id),
        ));
    }
    let placement = (
        order,
        group.id.clone(),
        group.title.clone(),
        section.map_or_else(String::new, |section| section.id.clone()),
        section.map_or_else(String::new, |section| section.title.clone()),
    );
    if placements.insert(id.clone(), placement).is_some() {
        return Err(DocsError::Navigation(
            path.to_path_buf(),
            format!("page `{id}` appears more than once"),
        ));
    }
    Ok(())
}

fn collect_headings(ast: &Node) -> Vec<HeadingInfo> {
    let mut headings = Vec::new();
    visit(ast, &mut |node| {
        if let Node::Heading(heading) = node {
            let title = heading
                .children
                .iter()
                .map(Node::to_string)
                .collect::<String>();
            headings.push(HeadingInfo {
                depth: heading.depth,
                id: slugify(&title),
                title,
            });
        }
    });
    headings
}

fn validate_compile_components(path: &Path, ast: &Node) -> Result<(), DocsError> {
    let mut error = None;
    visit(ast, &mut |node| {
        let (name, attributes) = match node {
            Node::MdxJsxFlowElement(element) => (element.name.as_deref(), &element.attributes),
            Node::MdxJsxTextElement(element) => (element.name.as_deref(), &element.attributes),
            _ => return,
        };
        let Some(name @ ("Demo" | "Demos" | "API")) = name else {
            return;
        };
        if error.is_some() {
            return;
        }
        if attributes
            .iter()
            .any(|attribute| matches!(attribute, AttributeContent::Expression(_)))
        {
            error = Some(macro_error(
                path,
                node,
                format!("<{name}> does not accept spread attributes at compile time"),
            ));
            return;
        }
        let required: &[&str] = match name {
            "Demo" => &["src"],
            "Demos" => &["glob"],
            "API" => &["source", "symbol"],
            _ => &[],
        };
        for key in required {
            let attribute = attributes.iter().find_map(|attribute| match attribute {
                AttributeContent::Property(property) if property.name == *key => Some(property),
                _ => None,
            });
            if attribute
                .and_then(|attribute| static_string(attribute.value.as_ref()))
                .is_none()
            {
                error = Some(macro_error(
                    path,
                    node,
                    format!("<{name}> attribute `{key}` must be a static string literal"),
                ));
                return;
            }
        }
        for attribute in attributes {
            let AttributeContent::Property(attribute) = attribute else {
                continue;
            };
            let compile_time = matches!(
                (name, attribute.name.as_str()),
                ("Demo", "src")
                    | ("Demos", "glob")
                    | ("Demos", "columns")
                    | ("API", "source")
                    | ("API", "symbol")
                    | ("API", "component")
            );
            if compile_time && !is_static_value(attribute.value.as_ref()) {
                error = Some(macro_error(
                    path,
                    node,
                    format!(
                        "<{name}> attribute `{}` must be a static literal",
                        attribute.name
                    ),
                ));
                return;
            }
        }
    });
    error.map_or(Ok(()), Err)
}

fn macro_error(path: &Path, node: &Node, message: String) -> DocsError {
    let point = node.position().map(|position| &position.start);
    DocsError::InvalidMacro {
        path: path.to_path_buf(),
        line: point.map_or(1, |point| point.line),
        column: point.map_or(1, |point| point.column),
        message,
    }
}

fn collect_api_entries(
    root: &Path,
    page_path: &Path,
    page_file: &str,
    ast: &Node,
) -> Result<Vec<ApiEntry>, DocsError> {
    let mut requests = Vec::new();
    visit(ast, &mut |node| {
        let attributes = match node {
            Node::MdxJsxFlowElement(element) if element.name.as_deref() == Some("API") => {
                Some(&element.attributes)
            }
            Node::MdxJsxTextElement(element) if element.name.as_deref() == Some("API") => {
                Some(&element.attributes)
            }
            _ => None,
        };
        let Some(attributes) = attributes else {
            return;
        };
        let source = literal_attribute(attributes, "source");
        let symbol = literal_attribute(attributes, "symbol");
        let component = literal_attribute(attributes, "component");
        if let (Some(source), Some(symbol)) = (source, symbol) {
            requests.push((source, symbol, component));
        }
    });

    let mut entries = Vec::new();
    for (source, symbol, component) in requests {
        let source_path = page_path.parent().unwrap_or(root).join(&source);
        let mut doc = wake_tsdoc::extract_api(&source_path, &symbol, component.as_deref())
            .map_err(|error| DocsError::Api(source_path.clone(), error.to_string()))?;
        relativize_api_sources(root, &mut doc);
        entries.push(ApiEntry {
            key: format!("{page_file}|{source}|{symbol}"),
            value: doc,
        });
    }
    Ok(entries)
}

fn relativize_api_sources(root: &Path, doc: &mut wake_tsdoc::ApiDoc) {
    fn relative(root: &Path, value: &str) -> String {
        let path = Path::new(value);
        if path.is_absolute() {
            slash_path(path.strip_prefix(root).unwrap_or(path))
        } else {
            value.replace('\\', "/")
        }
    }
    doc.source = relative(root, &doc.source);
    for prop in &mut doc.props {
        prop.source = relative(root, &prop.source);
    }
    for group in &mut doc.inherited {
        group.source = relative(root, &group.source);
    }
}

#[derive(Debug, Default)]
struct StaticDemoMeta {
    title: Option<String>,
    group: Option<String>,
    component: Option<String>,
    order: i32,
}

fn compile_demos(
    root: &Path,
    source_dir: &Path,
    files: &[PathBuf],
    docs_mode: DocsMode,
) -> Result<Vec<DemoInfo>, DocsError> {
    let mut demos = Vec::new();
    for path in files {
        let source = fs::read_to_string(path)
            .map_err(|error| DocsError::Io(path.clone(), error.to_string()))?;
        let relative = path.strip_prefix(root).map_err(|_| {
            DocsError::InvalidConfig(format!("demo {} is outside project root", path.display()))
        })?;
        let source_relative = path.strip_prefix(source_dir).map_err(|_| {
            DocsError::InvalidConfig(format!(
                "demo {} is outside docs source_dir",
                path.display()
            ))
        })?;
        let id = slash_path(relative);
        let source_module = slash_path(relative.with_extension("source.tsx"));
        let stem = path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("Demo")
            .trim_end_matches(".demo");
        let static_meta = extract_static_demo_meta(&source);
        let (derived_group, derived_component) = derive_demo_location(source_relative, stem);
        let mut warnings = Vec::new();
        if docs_mode == DocsMode::Components
            && static_demo_args(&source).is_some_and(|value| !is_json_literal(value))
        {
            warnings.push(format!(
                "{id}: meta.args contains a dynamic or non-JSON value; it will be ignored"
            ));
        }
        let controls = if docs_mode == DocsMode::Components {
            match wake_tsdoc::extract_demo_props(path) {
                Ok(Some(mut api)) => {
                    relativize_api_sources(root, &mut api);
                    warnings.extend(
                        api.warnings
                            .iter()
                            .map(|warning| format!("{id}: {warning}")),
                    );
                    api.props.iter().map(control_from_prop).collect()
                }
                Ok(None) => Vec::new(),
                Err(error) => {
                    warnings.push(format!("{id}: cannot infer demo props: {error}"));
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        demos.push(DemoInfo {
            id: id.clone(),
            title: static_meta.title.unwrap_or_else(|| title_case(stem)),
            group: static_meta.group.unwrap_or(derived_group),
            component: static_meta.component.unwrap_or(derived_component),
            order: static_meta.order,
            controls,
            warnings,
            source,
            source_module,
            import_path: format!("@wake/docs-project/{id}"),
        });
    }
    demos.sort_by(|left, right| {
        left.group
            .to_ascii_lowercase()
            .cmp(&right.group.to_ascii_lowercase())
            .then_with(|| {
                left.component
                    .to_ascii_lowercase()
                    .cmp(&right.component.to_ascii_lowercase())
            })
            .then_with(|| left.order.cmp(&right.order))
            .then_with(|| left.title.cmp(&right.title))
    });
    Ok(demos)
}

fn derive_demo_location(relative: &Path, stem: &str) -> (String, String) {
    let mut directories = relative
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .filter(|value| !value.eq_ignore_ascii_case("demos"))
        .map(title_case)
        .collect::<Vec<_>>();
    let component = directories.pop().unwrap_or_else(|| title_case(stem));
    let group = if directories.is_empty() {
        "Other".to_string()
    } else {
        directories.join(" / ")
    };
    (group, component)
}

fn extract_static_demo_meta(source: &str) -> StaticDemoMeta {
    let Some(body) = static_demo_meta_body(source) else {
        return StaticDemoMeta::default();
    };
    StaticDemoMeta {
        title: static_demo_string(body, "title"),
        group: static_demo_string(body, "group"),
        component: static_demo_string(body, "component"),
        order: static_demo_i32(body, "order").unwrap_or_default(),
    }
}

fn static_demo_meta_body(source: &str) -> Option<&str> {
    let pattern = Regex::new(r"(?s)export\s+const\s+meta\s*=\s*\{").expect("valid demo meta regex");
    let found = pattern.find(source)?;
    let open = found.end() - 1;
    let close = find_js_matching(source, open, '{', '}')?;
    Some(&source[open + 1..close])
}

fn static_demo_property<'a>(source: &'a str, key: &str) -> Option<&'a str> {
    let bytes = source.as_bytes();
    let mut start = 0usize;
    let mut offset = 0usize;
    let mut depths = [0usize; 3];
    let mut quote = None;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;

    while offset < bytes.len() {
        let byte = bytes[offset];
        let next = bytes.get(offset + 1).copied();
        if line_comment {
            line_comment = byte != b'\n';
            offset += 1;
            continue;
        }
        if block_comment {
            if byte == b'*' && next == Some(b'/') {
                block_comment = false;
                offset += 2;
            } else {
                offset += 1;
            }
            continue;
        }
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == active {
                quote = None;
            }
            offset += 1;
            continue;
        }

        match (byte, next) {
            (b'/', Some(b'/')) => {
                line_comment = true;
                offset += 2;
                continue;
            }
            (b'/', Some(b'*')) => {
                block_comment = true;
                offset += 2;
                continue;
            }
            _ => {}
        }
        match byte {
            b'"' | b'\'' | b'`' => quote = Some(byte),
            b'{' => depths[0] += 1,
            b'}' => depths[0] = depths[0].saturating_sub(1),
            b'[' => depths[1] += 1,
            b']' => depths[1] = depths[1].saturating_sub(1),
            b'(' => depths[2] += 1,
            b')' => depths[2] = depths[2].saturating_sub(1),
            b',' if depths == [0, 0, 0] => {
                if let Some(value) = static_demo_entry(&source[start..offset], key) {
                    return Some(value);
                }
                start = offset + 1;
            }
            _ => {}
        }
        offset += 1;
    }
    static_demo_entry(&source[start..], key)
}

fn static_demo_entry<'a>(entry: &'a str, key: &str) -> Option<&'a str> {
    let mut entry = entry.trim_start();
    loop {
        if let Some(comment) = entry.strip_prefix("//") {
            entry = comment.split_once('\n')?.1.trim_start();
        } else if let Some(comment) = entry.strip_prefix("/*") {
            entry = comment.split_once("*/")?.1.trim_start();
        } else {
            break;
        }
    }
    entry
        .strip_prefix(key)?
        .trim_start()
        .strip_prefix(':')
        .map(str::trim)
}

fn static_demo_string(source: &str, key: &str) -> Option<String> {
    let value = static_demo_property(source, key)?;
    if value.starts_with('"') && value.ends_with('"') {
        serde_json::from_str(value).ok()
    } else if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
        Some(
            value[1..value.len() - 1]
                .replace("\\'", "'")
                .replace("\\\\", "\\"),
        )
    } else {
        None
    }
}

fn static_demo_i32(source: &str, key: &str) -> Option<i32> {
    static_demo_property(source, key)?.parse().ok()
}

fn static_demo_args(source: &str) -> Option<&str> {
    let body = static_demo_meta_body(source)?;
    static_demo_property(body, "args")
}

fn is_json_literal(source: &str) -> bool {
    let mut parser = JsonLiteralParser { source, offset: 0 };
    parser.value() && {
        parser.trivia();
        parser.offset == source.len()
    }
}

struct JsonLiteralParser<'a> {
    source: &'a str,
    offset: usize,
}

impl JsonLiteralParser<'_> {
    fn value(&mut self) -> bool {
        self.trivia();
        match self.peek() {
            Some('{') => self.object(),
            Some('[') => self.array(),
            Some('"' | '\'') => self.string(),
            Some('-' | '0'..='9') => self.number(),
            Some(_) => ["true", "false", "null"]
                .into_iter()
                .any(|keyword| self.keyword(keyword)),
            None => false,
        }
    }

    fn object(&mut self) -> bool {
        self.bump();
        self.trivia();
        if self.consume('}') {
            return true;
        }
        loop {
            self.trivia();
            if matches!(self.peek(), Some('"' | '\'')) {
                if !self.string() {
                    return false;
                }
            } else if !self.identifier() {
                return false;
            }
            self.trivia();
            if !self.consume(':') || !self.value() {
                return false;
            }
            self.trivia();
            if self.consume('}') {
                return true;
            }
            if !self.consume(',') {
                return false;
            }
            self.trivia();
            if self.consume('}') {
                return true;
            }
        }
    }

    fn array(&mut self) -> bool {
        self.bump();
        self.trivia();
        if self.consume(']') {
            return true;
        }
        loop {
            if !self.value() {
                return false;
            }
            self.trivia();
            if self.consume(']') {
                return true;
            }
            if !self.consume(',') {
                return false;
            }
            self.trivia();
            if self.consume(']') {
                return true;
            }
        }
    }

    fn string(&mut self) -> bool {
        let Some(quote @ ('"' | '\'')) = self.bump() else {
            return false;
        };
        let mut escaped = false;
        while let Some(character) = self.bump() {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == quote {
                return true;
            }
        }
        false
    }

    fn number(&mut self) -> bool {
        let start = self.offset;
        while self.peek().is_some_and(|character| {
            character.is_ascii_digit() || matches!(character, '-' | '+' | '.' | 'e' | 'E')
        }) {
            self.bump();
        }
        self.source[start..self.offset].parse::<f64>().is_ok()
    }

    fn keyword(&mut self, keyword: &str) -> bool {
        if self.source[self.offset..].starts_with(keyword) {
            self.offset += keyword.len();
            true
        } else {
            false
        }
    }

    fn identifier(&mut self) -> bool {
        let Some(first) = self.peek() else {
            return false;
        };
        if !first.is_ascii_alphabetic() && !matches!(first, '_' | '$') {
            return false;
        }
        self.bump();
        while self.peek().is_some_and(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '$')
        }) {
            self.bump();
        }
        true
    }

    fn trivia(&mut self) {
        loop {
            while self.peek().is_some_and(char::is_whitespace) {
                self.bump();
            }
            if self.source[self.offset..].starts_with("//") {
                self.offset += 2;
                while self.peek().is_some_and(|character| character != '\n') {
                    self.bump();
                }
            } else if self.source[self.offset..].starts_with("/*") {
                let Some(end) = self.source[self.offset + 2..].find("*/") else {
                    self.offset = self.source.len();
                    return;
                };
                self.offset += end + 4;
            } else {
                return;
            }
        }
    }

    fn consume(&mut self, expected: char) -> bool {
        if self.peek() == Some(expected) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn peek(&self) -> Option<char> {
        self.source[self.offset..].chars().next()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek()?;
        self.offset += character.len_utf8();
        Some(character)
    }
}
fn find_js_matching(source: &str, open: usize, open_char: char, close_char: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for (offset, character) in source[open..].char_indices() {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == active {
                quote = None;
            }
            continue;
        }
        if matches!(character, '\'' | '"') || character == '\u{60}' {
            quote = Some(character);
        } else if character == open_char {
            depth += 1;
        } else if character == close_char {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(open + offset);
            }
        }
    }
    None
}
fn control_from_prop(prop: &wake_tsdoc::ApiProp) -> DemoControl {
    let type_text = prop.type_text.trim();
    let options = literal_union_options(type_text);
    let kind = if !options.is_empty() {
        "select"
    } else if type_text == "boolean" {
        "boolean"
    } else if type_text == "string" {
        "string"
    } else if type_text == "number" {
        "number"
    } else if type_text.starts_with('{')
        || type_text.starts_with('[')
        || type_text.ends_with("[]")
        || type_text.starts_with("Array<")
        || type_text.starts_with("ReadonlyArray<")
        || type_text.starts_with("Record<")
    {
        "json"
    } else {
        "readonly"
    };
    DemoControl {
        name: prop.name.clone(),
        type_text: prop.type_text.clone(),
        kind: kind.to_string(),
        required: prop.required,
        description: prop.description.clone(),
        default_value: prop
            .default_value
            .as_deref()
            .and_then(parse_static_json_value),
        deprecated: prop.deprecated.clone(),
        options,
    }
}

fn literal_union_options(type_text: &str) -> Vec<serde_json::Value> {
    let parts = type_text.split('|').map(str::trim).collect::<Vec<_>>();
    if parts.len() < 2 {
        return Vec::new();
    }
    let literal_parts = parts
        .iter()
        .copied()
        .filter(|part| *part != "undefined" && *part != "null")
        .collect::<Vec<_>>();
    let options = literal_parts
        .iter()
        .filter_map(|part| parse_static_json_value(part))
        .collect::<Vec<_>>();
    if options.len() == literal_parts.len() {
        options
    } else {
        Vec::new()
    }
}

fn parse_static_json_value(value: &str) -> Option<serde_json::Value> {
    let value = value.trim();
    if value == "undefined" {
        return None;
    }
    if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
        return Some(serde_json::Value::String(
            value[1..value.len() - 1]
                .replace("\\'", "'")
                .replace("\\\\", "\\"),
        ));
    }
    serde_json::from_str(value).ok()
}

fn render_registry(
    source_dir: &Path,
    pages: &[(PathBuf, CompiledPage)],
    demos: &[DemoInfo],
    api_entries: &[&ApiEntry],
) -> Result<String, DocsError> {
    let mut output = String::from("// Generated by Wake. Do not edit.\nexport const pages = [\n");
    for (source_path, page) in pages {
        let relative = source_path
            .strip_prefix(source_dir)
            .map_err(|_| DocsError::InvalidConfig("page escaped source_dir".to_string()))?
            .with_extension("tsx");
        let meta = serde_json::to_string(&page.route).expect("serializable route");
        output.push_str(&format!(
            "  {{ ...{meta}, load: () => import(\"@wake/docs/pages/{}\") }},\n",
            slash_path(relative)
        ));
    }
    output.push_str("] as const;\nexport const demos = [\n");
    for demo in demos {
        let metadata = serde_json::to_string(&json!({
            "id": demo.id,
            "title": demo.title,
            "group": demo.group,
            "component": demo.component,
            "order": demo.order,
            "controls": demo.controls,
            "warnings": demo.warnings,
        }))
        .expect("serializable demo metadata");
        output.push_str(&format!(
            "  {{ ...{metadata}, load: () => import({}), loadSource: () => import(\"@wake/docs/demo-source/{}\") }},\n",
            js_string(&demo.import_path),
            demo.source_module
        ));
    }
    output.push_str("] as const;\nexport const apiDocs = ");
    let map: BTreeMap<_, _> = api_entries
        .iter()
        .map(|entry| (entry.key.as_str(), &entry.value))
        .collect();
    output.push_str(&serde_json::to_string(&map).expect("serializable API docs"));
    output.push_str(" as const;\n");
    Ok(output)
}

fn render_config(
    root: &Path,
    options: &DocsOptions,
    docs_mode: DocsMode,
) -> Result<String, DocsError> {
    let mut output = String::from("import React from \"react\";\n");
    if let Some(css) = &options.theme_css {
        output.push_str(&format!(
            "import {};\n",
            js_string(&root_relative_alias(root, &absolute_from(root, css))?)
        ));
    }
    if let Some(preview) = &options.preview {
        output.push_str(&format!(
            "export {{ default as Preview }} from {};\n",
            js_string(&root_relative_alias(root, &absolute_from(root, preview))?)
        ));
    } else {
        output.push_str("export const Preview = React.Fragment;\n");
    }
    let base_path = normalize_base(&options.base_path);
    let logo = options
        .logo
        .as_deref()
        .map(|value| public_asset_url(&base_path, value));
    let config = json!({
        "title": options.title, "description": options.description, "locale": options.locale, "logo": logo,
        "repositoryUrl": options.repository_url, "basePath": base_path,
        "defaultTheme": options.default_theme, "accentColor": options.accent_color,
        "mode": docs_mode.as_str(),
    });
    output.push_str(&format!(
        "export const siteConfig = {} as const;\n",
        serde_json::to_string(&config).expect("serializable config")
    ));
    Ok(output)
}

fn public_asset_url(base_path: &str, value: &str) -> String {
    if value.starts_with("http://")
        || value.starts_with("https://")
        || value.starts_with("//")
        || value.starts_with("data:")
        || value.starts_with('#')
    {
        return value.to_string();
    }
    let asset = value.trim_start_matches('/');
    if base_path == "/" {
        format!("/{asset}")
    } else if value.starts_with(base_path) {
        value.to_string()
    } else {
        format!("{base_path}{asset}")
    }
}

struct Renderer<'a> {
    page_file: &'a str,
    heading_ids: BTreeMap<String, usize>,
}

impl<'a> Renderer<'a> {
    fn new(page_file: &'a str) -> Self {
        Self {
            page_file,
            heading_ids: BTreeMap::new(),
        }
    }

    fn render_root(&mut self, node: &Node) -> String {
        let mut output = String::new();
        if let Some(children) = node.children() {
            for child in children {
                if !matches!(child, Node::Toml(_) | Node::Yaml(_) | Node::MdxjsEsm(_)) {
                    output.push_str("    ");
                    output.push_str(&self.render(child));
                    output.push('\n');
                }
            }
        }
        output
    }

    fn render(&mut self, node: &Node) -> String {
        match node {
            Node::Root(value) => self.render_children(&value.children),
            Node::Paragraph(value) => self.render_paragraph(&value.children),
            Node::Heading(value) => {
                let title = value
                    .children
                    .iter()
                    .map(Node::to_string)
                    .collect::<String>();
                let base = slugify(&title);
                let count = self.heading_ids.entry(base.clone()).or_insert(0);
                let id = if *count == 0 {
                    base
                } else {
                    format!("{base}-{count}")
                };
                *count += 1;
                let content = self.render_children(&value.children);
                format!(
                    "<h{} id={} aria-label={}>{}<a className=\"heading-anchor\" href={} aria-label={}>#</a></h{}>",
                    value.depth,
                    js_braced(&id),
                    js_braced(&title),
                    content,
                    js_braced(&format!("#{id}")),
                    js_braced(&format!("Section: {title}")),
                    value.depth
                )
            }
            Node::Text(value) => js_braced(&value.value),
            Node::Emphasis(value) => format!("<em>{}</em>", self.render_children(&value.children)),
            Node::Strong(value) => {
                format!("<strong>{}</strong>", self.render_children(&value.children))
            }
            Node::Delete(value) => format!("<del>{}</del>", self.render_children(&value.children)),
            Node::InlineCode(value) => format!("<code>{}</code>", js_braced(&value.value)),
            Node::InlineMath(value) => format!(
                "<code className=\"math-inline\">{}</code>",
                js_braced(&value.value)
            ),
            Node::Break(_) => "<br />".to_string(),
            Node::Blockquote(value) => format!(
                "<blockquote>{}</blockquote>",
                self.render_children(&value.children)
            ),
            Node::List(value) => {
                let tag = if value.ordered { "ol" } else { "ul" };
                let class_name = if value
                    .children
                    .iter()
                    .any(|node| matches!(node, Node::ListItem(item) if item.checked.is_some()))
                {
                    " className=\"task-list\""
                } else {
                    ""
                };
                let start = value
                    .start
                    .filter(|start| *start != 1)
                    .map_or(String::new(), |start| format!(" start={{{start}}}"));
                format!(
                    "<{tag}{class_name}{start}>{}</{tag}>",
                    self.render_children(&value.children)
                )
            }
            Node::ListItem(value) => {
                let checkbox = value.checked.map_or(String::new(), |checked| {
                    format!("<input type=\"checkbox\" checked={{{checked}}} disabled />")
                });
                let class_name = if value.checked.is_some() {
                    " className=\"task-list-item\""
                } else {
                    ""
                };
                format!(
                    "<li{class_name}>{checkbox}{}</li>",
                    self.render_children(&value.children)
                )
            }
            Node::Code(value) => {
                render_code_block(value.lang.as_deref(), value.meta.as_deref(), &value.value)
            }
            Node::Math(value) => format!(
                "<pre className=\"math-block\"><code>{}</code></pre>",
                js_braced(&value.value)
            ),
            Node::ThematicBreak(_) => "<hr />".to_string(),
            Node::Link(value) => {
                let title = value.title.as_deref().map_or(String::new(), |title| {
                    format!(" title={}", js_braced(title))
                });
                format!(
                    "<a href={}{title}>{}</a>",
                    js_braced(&value.url),
                    self.render_children(&value.children)
                )
            }
            Node::Image(value) => {
                let title = value.title.as_deref().map_or(String::new(), |title| {
                    format!(" title={}", js_braced(title))
                });
                format!(
                    "<img src={} alt={}{title} />",
                    js_braced(&value.url),
                    js_braced(&value.alt)
                )
            }
            Node::Table(value) => self.render_table(&value.children),
            Node::TableRow(value) => format!("<tr>{}</tr>", self.render_children(&value.children)),
            Node::TableCell(value) => format!("<td>{}</td>", self.render_children(&value.children)),
            Node::MdxTextExpression(value) => render_expression(&value.value),
            Node::MdxFlowExpression(value) => render_expression(&value.value),
            Node::MdxJsxFlowElement(value) => {
                self.render_jsx(value.name.as_deref(), &value.attributes, &value.children)
            }
            Node::MdxJsxTextElement(value) => {
                self.render_jsx(value.name.as_deref(), &value.attributes, &value.children)
            }
            Node::Html(value) => value.value.clone(),
            Node::LinkReference(value) => format!(
                "<a href={}>{}</a>",
                js_braced(&format!("#{}", value.identifier)),
                self.render_children(&value.children)
            ),
            Node::ImageReference(value) => format!(
                "<img alt={} data-reference={} />",
                js_braced(&value.alt),
                js_braced(&value.identifier)
            ),
            Node::FootnoteReference(value) => format!(
                "<sup><a href={}>{}</a></sup>",
                js_braced(&format!("#fn-{}", value.identifier)),
                js_braced(&value.identifier)
            ),
            Node::FootnoteDefinition(value) => format!(
                "<aside id={} className=\"footnote\">{}</aside>",
                js_braced(&format!("fn-{}", value.identifier)),
                self.render_children(&value.children)
            ),
            Node::Definition(_) | Node::Toml(_) | Node::Yaml(_) | Node::MdxjsEsm(_) => {
                String::new()
            }
        }
    }

    fn render_children(&mut self, children: &[Node]) -> String {
        children.iter().map(|child| self.render(child)).collect()
    }

    fn render_paragraph(&mut self, children: &[Node]) -> String {
        let jsx_only = children
            .iter()
            .filter(|child| !is_whitespace_text(child))
            .all(is_jsx_node);
        if !children
            .iter()
            .any(|child| is_paragraph_block_boundary(child, jsx_only))
        {
            return format!("<p>{}</p>", self.render_children(children));
        }

        let mut output = String::new();
        let mut phrasing = Vec::new();
        for child in children {
            if is_paragraph_block_boundary(child, jsx_only) {
                self.flush_paragraph_phrasing(&mut output, &mut phrasing);
                output.push_str(&self.render(child));
            } else {
                phrasing.push(child);
            }
        }
        self.flush_paragraph_phrasing(&mut output, &mut phrasing);
        output
    }

    fn flush_paragraph_phrasing(&mut self, output: &mut String, phrasing: &mut Vec<&Node>) {
        if phrasing.iter().any(|child| !is_whitespace_text(child)) {
            output.push_str("<p>");
            for child in phrasing.iter() {
                output.push_str(&self.render(child));
            }
            output.push_str("</p>");
        }
        phrasing.clear();
    }

    fn render_table(&mut self, rows: &[Node]) -> String {
        let Some((head, body)) = rows.split_first() else {
            return "<div className=\"table-scroll\"><table /></div>".to_string();
        };
        let head = self.render_table_row(head, "th");
        let body = body
            .iter()
            .map(|row| self.render_table_row(row, "td"))
            .collect::<String>();
        format!(
            "<div className=\"table-scroll\"><table><thead>{head}</thead><tbody>{body}</tbody></table></div>"
        )
    }

    fn render_table_row(&mut self, row: &Node, cell: &str) -> String {
        let Node::TableRow(row) = row else {
            return self.render(row);
        };
        let cells = row
            .children
            .iter()
            .map(|node| match node {
                Node::TableCell(value) => {
                    format!("<{cell}>{}</{cell}>", self.render_children(&value.children))
                }
                _ => self.render(node),
            })
            .collect::<String>();
        format!("<tr>{cells}</tr>")
    }

    fn render_jsx(
        &mut self,
        name: Option<&str>,
        attributes: &[AttributeContent],
        children: &[Node],
    ) -> String {
        let Some(name) = name else {
            return format!("<>{}</>", self.render_children(children));
        };
        let mut attrs = render_attributes(attributes);
        if matches!(name, "Demo" | "Demos" | "API") {
            attrs.push_str(&format!(" __wakePage={}", js_braced(self.page_file)));
        }
        if children.is_empty() {
            format!("<{name}{attrs} />")
        } else {
            format!("<{name}{attrs}>{}</{name}>", self.render_children(children))
        }
    }
}

fn is_whitespace_text(node: &Node) -> bool {
    matches!(node, Node::Text(value) if value.value.trim().is_empty())
}

fn is_jsx_node(node: &Node) -> bool {
    matches!(
        node,
        Node::MdxJsxFlowElement(_) | Node::MdxJsxTextElement(_)
    )
}

fn is_paragraph_block_boundary(node: &Node, jsx_only: bool) -> bool {
    match node {
        Node::MdxJsxFlowElement(_) => true,
        Node::MdxJsxTextElement(element) => match element.name.as_deref() {
            Some(name) if is_html_block_element(name) => true,
            Some(name) => jsx_only && is_custom_jsx_name(name),
            None => {
                jsx_only
                    || element
                        .children
                        .iter()
                        .any(|child| is_paragraph_block_boundary(child, false))
            }
        },
        _ => false,
    }
}

fn is_custom_jsx_name(name: &str) -> bool {
    name.split('.').next().is_some_and(|head| {
        head.chars()
            .next()
            .is_some_and(|character| character.is_ascii_uppercase() || character == '_')
    })
}

fn is_html_block_element(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "body"
            | "caption"
            | "col"
            | "colgroup"
            | "dd"
            | "details"
            | "dialog"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "head"
            | "header"
            | "hgroup"
            | "hr"
            | "html"
            | "legend"
            | "li"
            | "main"
            | "menu"
            | "nav"
            | "ol"
            | "p"
            | "pre"
            | "search"
            | "section"
            | "summary"
            | "table"
            | "tbody"
            | "td"
            | "tfoot"
            | "th"
            | "thead"
            | "tr"
            | "ul"
    )
}

fn render_code_block(language: Option<&str>, meta: Option<&str>, code: &str) -> String {
    let language = normalize_code_language(language.unwrap_or("text"));
    let line_count = code.lines().count().max(1);
    let highlighted_lines = code_highlighted_lines(meta, line_count);
    let content = render_highlighted_code_lines(code, &language, &highlighted_lines);
    let title = code_title(meta)
        .map(|title| format!(" title={}", js_braced(&title)))
        .unwrap_or_default();
    format!(
        "<CodeBlock language={} code={}{title}>{content}</CodeBlock>",
        js_braced(&language),
        js_braced(code)
    )
}

fn render_highlighted_code_lines(
    code: &str,
    language: &str,
    highlighted_lines: &BTreeSet<usize>,
) -> String {
    let lines = highlight_code(code, language);
    let mut content = String::new();
    for (index, line) in lines.iter().enumerate() {
        let number = index + 1;
        let class = if highlighted_lines.contains(&number) {
            "code-line is-highlighted"
        } else {
            "code-line"
        };
        content.push_str(&format!(
            "<span className=\"{class}\" data-line={{{number}}}>{line}</span>"
        ));
    }
    content
}

fn normalize_code_language(language: &str) -> String {
    match language.trim().to_ascii_lowercase().as_str() {
        "js" => "javascript",
        "ts" => "typescript",
        "sh" | "shell" | "zsh" => "bash",
        "ps1" | "pwsh" => "powershell",
        "rs" => "rust",
        "py" => "python",
        "md" => "markdown",
        "yml" => "yaml",
        "htm" | "xml" => "html",
        "" => "text",
        value => value,
    }
    .to_string()
}

fn highlight_code(code: &str, language: &str) -> Vec<String> {
    let mut lines = vec![String::new()];
    if !supports_code_highlighting(language) {
        push_code_segment(&mut lines, None, code);
        return lines;
    }
    let pattern = code_token_pattern(language);
    let mut cursor = 0;
    for matched in pattern.find_iter(code) {
        push_code_segment(&mut lines, None, &code[cursor..matched.start()]);
        let kind = classify_code_token(
            language,
            code,
            matched.start(),
            matched.end(),
            matched.as_str(),
        );
        push_code_segment(&mut lines, kind, matched.as_str());
        cursor = matched.end();
    }
    push_code_segment(&mut lines, None, &code[cursor..]);
    lines
}

fn push_code_segment(lines: &mut Vec<String>, kind: Option<&str>, value: &str) {
    for (index, part) in value.split('\n').enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        let part = part.strip_suffix('\r').unwrap_or(part);
        if part.is_empty() {
            continue;
        }
        let rendered = js_braced(part);
        if let Some(kind) = kind {
            lines.last_mut().expect("one code line").push_str(&format!(
                "<span className=\"syntax-{kind}\">{rendered}</span>"
            ));
        } else {
            lines.last_mut().expect("one code line").push_str(&rendered);
        }
    }
}

fn supports_code_highlighting(language: &str) -> bool {
    matches!(
        language,
        "javascript"
            | "typescript"
            | "jsx"
            | "tsx"
            | "rust"
            | "bash"
            | "python"
            | "powershell"
            | "sql"
            | "json"
            | "jsonc"
            | "toml"
            | "yaml"
            | "css"
            | "scss"
            | "html"
            | "mdx"
            | "markdown"
            | "c"
            | "cpp"
            | "java"
            | "go"
    )
}

fn code_token_pattern(language: &str) -> &'static Regex {
    static DEFAULT: OnceLock<Regex> = OnceLock::new();
    static HASH_COMMENT: OnceLock<Regex> = OnceLock::new();
    static MARKUP_COMMENT: OnceLock<Regex> = OnceLock::new();
    static SQL_COMMENT: OnceLock<Regex> = OnceLock::new();
    let (cache, extra_comment, template_literal) =
        if matches!(language, "bash" | "powershell" | "python" | "toml" | "yaml") {
            (&HASH_COMMENT, r"|#[^\n]*", r"|`(?:\\.|[^`\\])*`")
        } else if matches!(language, "html" | "mdx" | "markdown") {
            (&MARKUP_COMMENT, r"|(?s:<!--.*?-->)", "")
        } else if language == "sql" {
            (&SQL_COMMENT, r"|--[^\n]*", r"|`(?:\\.|[^`\\])*`")
        } else {
            (&DEFAULT, "", r"|`(?:\\.|[^`\\])*`")
        };
    cache.get_or_init(|| {
        Regex::new(&format!(
            r#"(?s:/\*.*?\*/)|//[^\n]*{extra_comment}|"(?:\\.|[^"\\])*"|'(?:\\.|[^'\\])*'{template_literal}|#[0-9A-Fa-f]{{3,8}}\b|\b(?:0x[0-9A-Fa-f]+|\d+(?:\.\d+)?)\b|[A-Za-z_$][A-Za-z0-9_$-]*|[{{}}()\[\];,.<>:=+\-*/%!&|?~^@]+"#
        ))
        .expect("valid code token regex")
    })
}

fn classify_code_token<'a>(
    language: &str,
    code: &str,
    start: usize,
    end: usize,
    token: &'a str,
) -> Option<&'a str> {
    let hash_comments = matches!(language, "bash" | "powershell" | "python" | "toml" | "yaml");
    if token.starts_with("//")
        || token.starts_with("/*")
        || token.starts_with("<!--")
        || (language == "sql" && token.starts_with("--"))
        || (hash_comments && token.starts_with('#'))
    {
        return Some("comment");
    }
    let next = code[end..].chars().find(|value| !value.is_whitespace());
    let previous = code[..start]
        .chars()
        .rev()
        .find(|value| !value.is_whitespace());
    if token.starts_with('"') || token.starts_with(char::from(39u8)) || token.starts_with('`') {
        return Some(if next == Some(':') {
            "property"
        } else {
            "string"
        });
    }
    if token.starts_with('#')
        || token
            .chars()
            .next()
            .is_some_and(|value| value.is_ascii_digit())
    {
        return Some("number");
    }
    if token
        .chars()
        .next()
        .is_some_and(|value| value.is_ascii_alphabetic() || matches!(value, '_' | '$'))
    {
        if is_code_literal(token) {
            return Some("literal");
        }
        if is_code_keyword(language, token) {
            return Some("keyword");
        }
        if is_builtin_type(token) || token.chars().next().is_some_and(char::is_uppercase) {
            return Some("type");
        }
        if token.starts_with('$') {
            return Some("variable");
        }
        if next == Some('(') {
            return Some("function");
        }
        if previous == Some('.')
            || next == Some(':')
            || (matches!(
                language,
                "html" | "mdx" | "json" | "jsonc" | "toml" | "yaml" | "css" | "scss"
            ) && next == Some('='))
        {
            return Some("property");
        }
        return None;
    }
    if token.chars().any(|value| "=+-*/%!&|?~^<>".contains(value)) {
        Some("operator")
    } else {
        Some("punctuation")
    }
}

fn is_code_literal(value: &str) -> bool {
    matches!(
        value,
        "true" | "false" | "null" | "undefined" | "None" | "Some" | "Ok" | "Err"
    )
}

fn is_builtin_type(value: &str) -> bool {
    matches!(
        value,
        "any"
            | "boolean"
            | "never"
            | "number"
            | "object"
            | "string"
            | "symbol"
            | "unknown"
            | "void"
            | "bool"
            | "char"
            | "str"
            | "String"
            | "usize"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "f32"
            | "f64"
    )
}

fn is_code_keyword(language: &str, value: &str) -> bool {
    match language {
        "javascript" | "typescript" | "jsx" | "tsx" | "mdx" => matches!(
            value,
            "as" | "async"
                | "await"
                | "break"
                | "case"
                | "catch"
                | "class"
                | "const"
                | "continue"
                | "debugger"
                | "declare"
                | "default"
                | "delete"
                | "do"
                | "else"
                | "enum"
                | "export"
                | "extends"
                | "finally"
                | "for"
                | "from"
                | "function"
                | "get"
                | "if"
                | "implements"
                | "import"
                | "in"
                | "infer"
                | "instanceof"
                | "interface"
                | "is"
                | "keyof"
                | "let"
                | "namespace"
                | "new"
                | "of"
                | "private"
                | "protected"
                | "public"
                | "readonly"
                | "return"
                | "satisfies"
                | "set"
                | "static"
                | "super"
                | "switch"
                | "throw"
                | "try"
                | "type"
                | "typeof"
                | "var"
                | "while"
                | "with"
                | "yield"
        ),
        "rust" => matches!(
            value,
            "as" | "async"
                | "await"
                | "break"
                | "const"
                | "continue"
                | "crate"
                | "dyn"
                | "else"
                | "enum"
                | "extern"
                | "fn"
                | "for"
                | "if"
                | "impl"
                | "in"
                | "let"
                | "loop"
                | "match"
                | "mod"
                | "move"
                | "mut"
                | "pub"
                | "ref"
                | "return"
                | "self"
                | "Self"
                | "static"
                | "struct"
                | "super"
                | "trait"
                | "type"
                | "unsafe"
                | "use"
                | "where"
                | "while"
        ),
        "bash" => matches!(
            value,
            "case"
                | "do"
                | "done"
                | "elif"
                | "else"
                | "esac"
                | "fi"
                | "for"
                | "function"
                | "if"
                | "in"
                | "select"
                | "then"
                | "time"
                | "until"
                | "while"
        ),
        "python" => matches!(
            value,
            "and"
                | "as"
                | "assert"
                | "async"
                | "await"
                | "break"
                | "class"
                | "continue"
                | "def"
                | "del"
                | "elif"
                | "else"
                | "except"
                | "finally"
                | "for"
                | "from"
                | "global"
                | "if"
                | "import"
                | "in"
                | "is"
                | "lambda"
                | "nonlocal"
                | "not"
                | "or"
                | "pass"
                | "raise"
                | "return"
                | "try"
                | "while"
                | "with"
                | "yield"
        ),
        "powershell" => matches!(
            value.to_ascii_lowercase().as_str(),
            "begin"
                | "break"
                | "catch"
                | "class"
                | "continue"
                | "data"
                | "do"
                | "dynamicparam"
                | "else"
                | "elseif"
                | "end"
                | "enum"
                | "filter"
                | "finally"
                | "for"
                | "foreach"
                | "from"
                | "function"
                | "if"
                | "in"
                | "param"
                | "process"
                | "return"
                | "switch"
                | "throw"
                | "trap"
                | "try"
                | "until"
                | "using"
                | "while"
        ),
        "sql" => matches!(
            value.to_ascii_lowercase().as_str(),
            "alter"
                | "and"
                | "as"
                | "asc"
                | "begin"
                | "by"
                | "case"
                | "create"
                | "delete"
                | "desc"
                | "distinct"
                | "drop"
                | "else"
                | "end"
                | "from"
                | "group"
                | "having"
                | "in"
                | "insert"
                | "into"
                | "is"
                | "join"
                | "limit"
                | "not"
                | "null"
                | "on"
                | "or"
                | "order"
                | "select"
                | "set"
                | "table"
                | "then"
                | "union"
                | "update"
                | "values"
                | "when"
                | "where"
        ),
        "c" | "cpp" | "java" | "go" => matches!(
            value,
            "break"
                | "case"
                | "class"
                | "const"
                | "continue"
                | "default"
                | "defer"
                | "do"
                | "else"
                | "extends"
                | "final"
                | "for"
                | "func"
                | "goto"
                | "if"
                | "implements"
                | "import"
                | "interface"
                | "new"
                | "package"
                | "private"
                | "protected"
                | "public"
                | "return"
                | "static"
                | "struct"
                | "switch"
                | "throw"
                | "throws"
                | "try"
                | "typedef"
                | "var"
                | "while"
        ),
        _ => false,
    }
}

fn code_highlighted_lines(meta: Option<&str>, maximum: usize) -> BTreeSet<usize> {
    let mut lines = BTreeSet::new();
    let Some(meta) = meta else {
        return lines;
    };
    let pattern = Regex::new(r"\{([0-9,\-\s]+)\}").expect("valid line metadata regex");
    let Some(captures) = pattern.captures(meta) else {
        return lines;
    };
    for part in captures[1].split(',').map(str::trim) {
        if let Some((start, end)) = part.split_once('-') {
            if let (Ok(start), Ok(end)) =
                (start.trim().parse::<usize>(), end.trim().parse::<usize>())
            {
                let start = start.max(1).min(maximum);
                let end = end.max(1).min(maximum);
                if start <= end {
                    lines.extend(start..=end);
                }
            }
        } else if let Ok(line) = part.parse::<usize>()
            && (1..=maximum).contains(&line)
        {
            lines.insert(line);
        }
    }
    lines
}

fn code_title(meta: Option<&str>) -> Option<String> {
    let meta = meta?;
    let double = Regex::new(r#"title\s*=\s*"([^"]+)""#).expect("valid title regex");
    if let Some(captures) = double.captures(meta) {
        return Some(captures[1].to_string());
    }
    let single = Regex::new(r"title\s*=\s*'([^']+)'").expect("valid title regex");
    single
        .captures(meta)
        .map(|captures| captures[1].to_string())
}

fn render_attributes(attributes: &[AttributeContent]) -> String {
    let mut output = String::new();
    for attribute in attributes {
        match attribute {
            AttributeContent::Expression(expression) => {
                output.push_str(" {...(");
                output.push_str(&expression.value);
                output.push_str(")}");
            }
            AttributeContent::Property(property) => {
                output.push(' ');
                output.push_str(&property.name);
                if let Some(value) = &property.value {
                    match value {
                        AttributeValue::Literal(value) => {
                            output.push('=');
                            output.push_str(&js_braced(value));
                        }
                        AttributeValue::Expression(value) => {
                            output.push_str("={");
                            output.push_str(if value.value.trim().is_empty() {
                                "undefined"
                            } else {
                                &value.value
                            });
                            output.push('}');
                        }
                    }
                }
            }
        }
    }
    output
}

fn render_expression(value: &str) -> String {
    if value.trim().is_empty() {
        "{null}".to_string()
    } else {
        format!("{{{value}}}")
    }
}

fn extract_esm(source: &str) -> (String, String) {
    let mut output = source.as_bytes().to_vec();
    let mut esm = String::new();
    let mut offset = 0;
    let lines: Vec<&str> = source.split_inclusive('\n').collect();
    let mut index = 0;
    let mut fence: Option<(u8, usize)> = None;
    while index < lines.len() {
        let line = lines[index];
        if let Some((marker, length)) = markdown_fence_marker(line) {
            match fence {
                Some((active, minimum)) if marker == active && length >= minimum => fence = None,
                None => fence = Some((marker, length)),
                _ => {}
            }
            offset += line.len();
            index += 1;
            continue;
        }
        if fence.is_some() {
            offset += line.len();
            index += 1;
            continue;
        }
        let trimmed = line.trim_start();
        if !trimmed.starts_with("import ") && !trimmed.starts_with("export ") {
            offset += line.len();
            index += 1;
            continue;
        }
        let start = offset;
        let mut statement = String::new();
        loop {
            let current = lines[index];
            statement.push_str(current);
            offset += current.len();
            index += 1;
            if javascript_statement_complete(&statement) || index == lines.len() {
                break;
            }
        }
        let end = offset;
        esm.push_str(statement.trim());
        esm.push('\n');
        for byte in &mut output[start..end] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
    }
    (
        esm,
        String::from_utf8(output).expect("space replacement preserves UTF-8"),
    )
}

fn markdown_fence_marker(line: &str) -> Option<(u8, usize)> {
    let trimmed = line.trim_start_matches([' ', '\t']);
    let marker = *trimmed.as_bytes().first()?;
    if !matches!(marker, b'`' | b'~') {
        return None;
    }
    let length = trimmed
        .as_bytes()
        .iter()
        .take_while(|candidate| **candidate == marker)
        .count();
    (length >= 3).then_some((marker, length))
}
fn javascript_statement_complete(value: &str) -> bool {
    let trimmed = value.trim_end();
    balanced_javascript(trimmed)
        && (trimmed.ends_with(';')
            || trimmed.ends_with('}')
            || (trimmed.starts_with("import ")
                && (trimmed.ends_with('\'') || trimmed.ends_with('"'))))
}

fn balanced_javascript(value: &str) -> bool {
    let mut stack = Vec::new();
    let mut quote = None;
    let mut escaped = false;
    for ch in value.chars() {
        if let Some(active) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == active {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
        } else if matches!(ch, '(' | '[' | '{') {
            stack.push(ch);
        } else if let Some(expected) = match ch {
            ')' => Some('('),
            ']' => Some('['),
            '}' => Some('{'),
            _ => None,
        } && stack.pop() != Some(expected)
        {
            return false;
        }
    }
    quote.is_none() && stack.is_empty()
}

fn rewrite_relative_imports(root: &Path, page: &Path, esm: &str) -> String {
    let regex = Regex::new(r#"(["'])(\.\.?/[^"']+)(["'])"#).expect("valid specifier regex");
    regex
        .replace_all(esm, |captures: &regex::Captures<'_>| {
            let resolved = normalize_path(&page.parent().unwrap_or(root).join(&captures[2]));
            if let Ok(relative) = resolved.strip_prefix(root) {
                format!(
                    "{}@wake/docs-project/{}{}",
                    &captures[1],
                    slash_path(relative),
                    &captures[3]
                )
            } else {
                captures[0].to_string()
            }
        })
        .into_owned()
}

fn render_source_map(source_path: &Path, source: &str, module: &str) -> String {
    let generated_lines = module.lines().count();
    let source_lines = source.lines().count().max(1);
    let mut mappings = String::new();
    let mut previous_line = 0i64;
    for line in 0..generated_lines {
        if line > 0 {
            mappings.push(';');
        }
        let original = line.min(source_lines - 1) as i64;
        for value in [0, 0, original - previous_line, 0] {
            encode_vlq(value, &mut mappings);
        }
        previous_line = original;
    }
    serde_json::to_string(&json!({
        "version": 3, "file": "page.tsx", "sources": [slash_path(source_path)],
        "sourcesContent": [source], "names": [], "mappings": mappings,
    }))
    .expect("serializable source map")
}

fn encode_vlq(value: i64, output: &mut String) {
    const BASE64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut value = if value < 0 {
        ((-value) as u64) * 2 + 1
    } else {
        value as u64 * 2
    };
    loop {
        let mut digit = value & 31;
        value >>= 5;
        if value > 0 {
            digit |= 32;
        }
        output.push(BASE64[digit as usize] as char);
        if value == 0 {
            break;
        }
    }
}

fn ensure_unique_routes(pages: &[(PathBuf, CompiledPage)]) -> Result<(), DocsError> {
    let mut routes = BTreeMap::new();
    for (path, page) in pages {
        if let Some(first) = routes.insert(page.route.slug.clone(), path.clone()) {
            return Err(DocsError::DuplicateRoute {
                slug: page.route.slug.clone(),
                first,
                second: path.clone(),
            });
        }
    }
    Ok(())
}

fn validate_react_dependencies(root: &Path) -> Result<(), DocsError> {
    let package_path = root.join("package.json");
    let source = fs::read_to_string(&package_path).map_err(|error| {
        DocsError::InvalidConfig(format!(
            "React {MINIMUM_REACT_MAJOR}+ docs require a project package.json: {error}"
        ))
    })?;
    let package: serde_json::Value = serde_json::from_str(&source).map_err(|error| {
        DocsError::InvalidConfig(format!("invalid project package.json: {error}"))
    })?;
    let lower_bound = Regex::new(r"(?:^|[|,\s@:])(?:\^|~|>=?|=)?\s*v?(\d+)")
        .expect("valid package version regex");
    for dependency in ["react", "react-dom"] {
        let requirement = ["dependencies", "peerDependencies", "devDependencies"]
            .into_iter()
            .find_map(|section| package.get(section)?.get(dependency)?.as_str())
            .ok_or_else(|| {
                DocsError::InvalidConfig(format!(
                    "Wake docs requires {dependency} {MINIMUM_REACT_MAJOR}+ in dependencies, peerDependencies, or devDependencies"
                ))
            })?;
        let declared_majors: Vec<u64> = lower_bound
            .captures_iter(requirement)
            .filter_map(|capture| capture[1].parse().ok())
            .collect();
        if declared_majors.is_empty()
            || declared_majors
                .iter()
                .any(|major| *major < MINIMUM_REACT_MAJOR)
        {
            return Err(DocsError::InvalidConfig(format!(
                "Wake docs requires {dependency} {MINIMUM_REACT_MAJOR}+; found requirement '{requirement}'"
            )));
        }
    }
    Ok(())
}

fn validate_options(options: &DocsOptions) -> Result<(), DocsError> {
    if options.locale.trim().is_empty() {
        return Err(DocsError::InvalidConfig(
            "locale must not be empty".to_string(),
        ));
    }
    if !matches!(options.default_theme.as_str(), "light" | "dark" | "system") {
        return Err(DocsError::InvalidConfig(
            "default_theme must be light, dark, or system".to_string(),
        ));
    }
    if let Some(accent_color) = &options.accent_color {
        let color = Regex::new(r"^#[0-9a-fA-F]{6}$").expect("valid color regex");
        if !color.is_match(accent_color) {
            return Err(DocsError::InvalidConfig(
                "accent_color must be a six-digit hex color".to_string(),
            ));
        }
    }
    Ok(())
}

fn scan_files(
    directory: &Path,
    mdx_files: &mut Vec<PathBuf>,
    demo_files: &mut Vec<PathBuf>,
) -> Result<(), DocsError> {
    let entries = fs::read_dir(directory)
        .map_err(|error| DocsError::Io(directory.to_path_buf(), error.to_string()))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| DocsError::Io(directory.to_path_buf(), error.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            scan_files(&path, mdx_files, demo_files)?;
        } else {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or_default();
            if name.ends_with(".demo.tsx") {
                demo_files.push(path);
            } else if path.extension().and_then(|value| value.to_str()) == Some("mdx") {
                mdx_files.push(path);
            }
        }
    }
    Ok(())
}

fn remove_stale_generated_files(
    generated_dir: &Path,
    current: &BTreeSet<PathBuf>,
    changed: &mut Vec<PathBuf>,
) -> Result<(), DocsError> {
    let manifest = generated_dir.join("manifest.json");
    let Ok(source) = fs::read_to_string(&manifest) else {
        return Ok(());
    };
    let parsed: serde_json::Value = serde_json::from_str(&source).unwrap_or_default();
    let Some(files) = parsed.get("files").and_then(serde_json::Value::as_array) else {
        return Ok(());
    };
    for relative in files.iter().filter_map(serde_json::Value::as_str) {
        let path = generated_dir.join(relative);
        if !current.contains(&path) && path.is_file() {
            fs::remove_file(&path)
                .map_err(|error| DocsError::Io(path.clone(), error.to_string()))?;
            changed.push(path);
        }
    }
    Ok(())
}

fn atomic_write_if_changed(path: &Path, content: &[u8]) -> Result<bool, DocsError> {
    let _write_guard = ATOMIC_WRITE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if fs::read(path).is_ok_and(|current| current == content) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| DocsError::Io(parent.to_path_buf(), error.to_string()))?;
    }

    let operation_id = NEXT_ATOMIC_WRITE_ID.fetch_add(1, Ordering::Relaxed);
    let sibling = |marker: &str| {
        path.with_extension(format!(
            "{}.{marker}-{}-{operation_id}",
            path.extension()
                .and_then(|value| value.to_str())
                .unwrap_or("tmp"),
            std::process::id()
        ))
    };
    let temporary = sibling("wake-next");
    fs::write(&temporary, content)
        .map_err(|error| DocsError::Io(temporary.clone(), error.to_string()))?;

    if path.exists() {
        let backup = sibling("wake-previous");
        if backup.exists() {
            fs::remove_file(&backup)
                .map_err(|error| DocsError::Io(backup.clone(), error.to_string()))?;
        }
        if let Err(error) = fs::rename(path, &backup) {
            let _ = fs::remove_file(&temporary);
            return Err(DocsError::Io(path.to_path_buf(), error.to_string()));
        }
        if let Err(error) = fs::rename(&temporary, path) {
            let _ = fs::rename(&backup, path);
            let _ = fs::remove_file(&temporary);
            return Err(DocsError::Io(path.to_path_buf(), error.to_string()));
        }
        fs::remove_file(&backup).map_err(|error| DocsError::Io(backup, error.to_string()))?;
    } else if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(DocsError::Io(path.to_path_buf(), error.to_string()));
    }
    Ok(true)
}

/// Copy public after bundling, rejecting collisions with generated output.
pub fn copy_public_assets(root: &Path, outdir: &Path) -> Result<(), DocsError> {
    let public = root.join("public");
    if public.is_dir() {
        copy_public_directory(&public, &public, outdir)?;
    }
    Ok(())
}

fn copy_public_directory(base: &Path, directory: &Path, outdir: &Path) -> Result<(), DocsError> {
    for entry in fs::read_dir(directory)
        .map_err(|error| DocsError::Io(directory.to_path_buf(), error.to_string()))?
    {
        let entry =
            entry.map_err(|error| DocsError::Io(directory.to_path_buf(), error.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            copy_public_directory(base, &path, outdir)?;
        } else {
            let destination = outdir.join(path.strip_prefix(base).expect("public descendant"));
            if destination.exists() {
                return Err(DocsError::PublicCollision(destination));
            }
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)
                    .map_err(|error| DocsError::Io(parent.to_path_buf(), error.to_string()))?;
            }
            fs::copy(&path, &destination)
                .map_err(|error| DocsError::Io(destination, error.to_string()))?;
        }
    }
    Ok(())
}

/// Replicate route-aware HTML shells for direct static-host access to every page.
pub fn write_route_shells(
    outdir: &Path,
    routes: &[RouteInfo],
    index_html: &str,
    site_title: &str,
    site_description: &str,
    locale: &str,
) -> Result<(), DocsError> {
    let root_route = routes.iter().find(|route| route.slug == "/");
    let root_title = root_route
        .map(|route| route_page_title(site_title, &route.title))
        .unwrap_or_else(|| site_title.to_string());
    let root_description = root_route
        .map(|route| route.description.as_str())
        .filter(|description| !description.is_empty())
        .unwrap_or(site_description);
    let root_html = route_html(index_html, &root_title, root_description, locale);
    atomic_write_if_changed(&outdir.join("index.html"), root_html.as_bytes())?;

    let not_found_description = if locale.to_ascii_lowercase().starts_with("zh") {
        "找不到请求的文档页面。"
    } else {
        "The requested documentation page could not be found."
    };
    let not_found_html = route_html(
        index_html,
        &format!("404 · {site_title}"),
        not_found_description,
        locale,
    );
    atomic_write_if_changed(&outdir.join("404.html"), not_found_html.as_bytes())?;

    for route in routes {
        let relative = route.slug.trim_matches('/');
        if relative.is_empty() {
            continue;
        }
        let description = if route.description.is_empty() {
            site_description
        } else {
            &route.description
        };
        let html = route_html(
            index_html,
            &route_page_title(site_title, &route.title),
            description,
            locale,
        );
        atomic_write_if_changed(&outdir.join(relative).join("index.html"), html.as_bytes())?;
    }
    Ok(())
}

fn route_page_title(site_title: &str, page_title: &str) -> String {
    if page_title.trim().is_empty() || page_title == site_title {
        site_title.to_string()
    } else {
        format!("{page_title} · {site_title}")
    }
}

fn route_html(index_html: &str, title: &str, description: &str, locale: &str) -> String {
    let mut html = index_html.to_string();
    let escaped_title = escape_html(title);
    if let Some(start) = html.find("<title>") {
        if let Some(relative_end) = html[start + 7..].find("</title>") {
            let end = start + 7 + relative_end + 8;
            html.replace_range(start..end, &format!("<title>{escaped_title}</title>"));
        }
    } else if let Some(head_end) = html.find("</head>") {
        html.insert_str(head_end, &format!("<title>{escaped_title}</title>\n"));
    }

    let metadata = format!(
        "<meta name=\"description\" content=\"{}\">\n",
        escape_html(description)
    );
    if let Some(head_end) = html.find("</head>") {
        html.insert_str(head_end, &metadata);
    }

    if let Some(start) = html.find("<html")
        && let Some(relative_end) = html[start..].find('>')
    {
        let end = start + relative_end + 1;
        let tag = &html[start..end];
        let language = format!(" lang=\"{}\"", escape_html(locale));
        let language_pattern =
            Regex::new(r#"(?i)\s+lang\s*=\s*(?:\"[^\"]*\"|'[^']*')"#).expect("valid lang regex");
        let updated = if language_pattern.is_match(tag) {
            language_pattern.replace(tag, language).into_owned()
        } else {
            tag.replacen("<html", &format!("<html{language}"), 1)
        };
        html.replace_range(start..end, &updated);
    }
    html
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
}

fn visit(node: &Node, visitor: &mut impl FnMut(&Node)) {
    visitor(node);
    if let Some(children) = node.children() {
        for child in children {
            visit(child, visitor);
        }
    }
}

fn literal_attribute(attributes: &[AttributeContent], name: &str) -> Option<String> {
    attributes.iter().find_map(|attribute| match attribute {
        AttributeContent::Property(property) if property.name == name => {
            static_string(property.value.as_ref())
        }
        _ => None,
    })
}

fn static_string(value: Option<&AttributeValue>) -> Option<String> {
    match value {
        Some(AttributeValue::Literal(value)) => Some(value.clone()),
        Some(AttributeValue::Expression(value)) => {
            let value = value.value.trim();
            ((value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\'')))
            .then(|| value[1..value.len() - 1].to_string())
        }
        None => None,
    }
}

fn is_static_value(value: Option<&AttributeValue>) -> bool {
    match value {
        None | Some(AttributeValue::Literal(_)) => true,
        Some(AttributeValue::Expression(value)) => {
            let value = value.value.trim();
            let quoted = (value.starts_with('"') && value.ends_with('"'))
                || (value.starts_with('\'') && value.ends_with('\''));
            quoted
                || matches!(value, "true" | "false" | "null" | "undefined")
                || value.parse::<f64>().is_ok()
        }
    }
}

fn derive_slug(relative: &Path) -> String {
    let mut path = relative.with_extension("");
    if path.file_name().and_then(|value| value.to_str()) == Some("index") {
        path.pop();
    }
    format!("/{}", slash_path(path))
}

fn normalize_slug(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() || value == "/" {
        "/".to_string()
    } else {
        format!("/{}", value.trim_matches('/'))
    }
}

fn normalize_base(value: &str) -> String {
    if value.trim().is_empty() || value == "/" {
        "/".to_string()
    } else {
        format!("/{}/", value.trim_matches('/'))
    }
}

fn slugify(value: &str) -> String {
    let mut result = String::new();
    let mut dash = false;
    for ch in value.trim().chars() {
        if ch.is_alphanumeric() || ch == '_' {
            if dash && !result.is_empty() {
                result.push('-');
            }
            dash = false;
            result.extend(ch.to_lowercase());
        } else {
            dash = true;
        }
    }
    if result.is_empty() {
        "section".to_string()
    } else {
        result
    }
}

fn title_case(value: &str) -> String {
    value
        .split(['-', '_', '.'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            chars.next().map_or(String::new(), |first| {
                first.to_uppercase().collect::<String>() + chars.as_str()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn js_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization")
}
fn js_braced(value: &str) -> String {
    format!("{{{}}}", js_string(value))
}
fn slash_path(path: impl AsRef<Path>) -> String {
    let value = path.as_ref().to_string_lossy().replace('\\', "/");
    if let Some(rest) = value.strip_prefix("//?/UNC/") {
        format!("//{rest}")
    } else if let Some(rest) = value.strip_prefix("//?/") {
        rest.to_string()
    } else {
        value
    }
}

fn root_relative_alias(root: &Path, path: &Path) -> Result<String, DocsError> {
    let normalized = normalize_path(path);
    let relative = normalized.strip_prefix(root).map_err(|_| {
        DocsError::InvalidConfig(format!("`{}` must be inside project root", path.display()))
    })?;
    Ok(format!("@wake/docs-project/{}", slash_path(relative)))
}

fn absolute_from(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(&root.join(path))
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    result
}

fn canonical_dir(path: &Path) -> Result<PathBuf, DocsError> {
    fs::canonicalize(path).map_err(|error| DocsError::Io(path.to_path_buf(), error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    #[test]
    fn component_preview_scopes_configured_accent_without_mutating_document_root() {
        assert!(
            RUNTIME_STYLE.contains(".workbench-shell { --wake-accent: var(--workbench-accent); }")
        );
        assert!(
            !RUNTIME_STYLE.contains(":root:has(.demo-frame-root)"),
            "Demo 不能在文档根节点重写语义颜色"
        );
        let demo_scope = RUNTIME_STYLE
            .split_once(".demo-frame-root {")
            .expect("Demo token scope")
            .1
            .split_once('}')
            .expect("Demo token scope end")
            .0;
        assert!(
            !demo_scope.contains("--wake-accent:"),
            "Demo 必须继承配置的 --wake-accent，不能重置为工作台中性色"
        );
        for expected in [
            "--token-semantic-color-background-hover-subtle: color-mix(in srgb, var(--wake-accent)",
            "--token-semantic-color-background-active-subtle: color-mix(in srgb, var(--wake-accent)",
            "--token-semantic-color-border-focus: var(--wake-accent);",
            "--token-semantic-color-brand-primary: var(--wake-accent);",
            "--token-semantic-color-brand-primary-hover: color-mix(in srgb, var(--wake-accent)",
            "--token-semantic-color-brand-primary-active: color-mix(in srgb, var(--wake-accent)",
            "--token-semantic-color-text-link: var(--wake-accent);",
            "--token-semantic-color-text-link-hover: color-mix(in srgb, var(--wake-accent)",
            "--token-semantic-shadow-focus-ring: 0 0 0 3px color-mix(in srgb, var(--wake-accent)",
        ] {
            assert!(
                demo_scope.contains(expected),
                "缺少 Demo 强调色映射: {expected}"
            );
        }
        for runtime in [RUNTIME_APP, RUNTIME_COMPONENTS] {
            assert!(
                runtime.contains("if (siteConfig.accentColor)"),
                "Docs runtime 必须只在显式配置时写入强调色"
            );
        }
    }

    #[test]
    fn slash_paths_strip_windows_verbatim_prefixes() {
        assert_eq!(
            slash_path(Path::new(r"\\?\C:\proj\docs\index.mdx")),
            "C:/proj/docs/index.mdx"
        );
        assert_eq!(
            slash_path(Path::new(r"\\?\UNC\server\share\docs\index.mdx")),
            "//server/share/docs/index.mdx"
        );
    }

    #[test]
    fn concurrent_atomic_writes_do_not_share_staging_files() {
        let output = fixture().join(".wake/docs/generated/pages/shared.tsx");
        let workers = 16;
        let barrier = Arc::new(Barrier::new(workers));
        let handles: Vec<_> = (0..workers)
            .map(|worker| {
                let output = output.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    for round in 0..16 {
                        let content = format!("worker={worker};round={round}");
                        atomic_write_if_changed(&output, content.as_bytes()).unwrap();
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let content = fs::read_to_string(&output).unwrap();
        assert!(content.starts_with("worker="), "{content}");
        let leftovers: Vec<_> = fs::read_dir(output.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| name != "shared.tsx")
            .collect();
        assert!(leftovers.is_empty(), "staging files leaked: {leftovers:?}");
    }

    fn fixture() -> PathBuf {
        let root = loop {
            let id = NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed);
            let candidate =
                std::env::temp_dir().join(format!("wake-docs-{}-{id}", std::process::id()));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("fixture root: {error}"),
            }
        };
        fs::create_dir_all(root.join("docs/demos")).expect("fixture docs");
        fs::create_dir_all(root.join("src")).expect("fixture src");
        fs::write(
            root.join("package.json"),
            r#"{"dependencies":{"react":"^19.0.0","react-dom":"^19.0.0"}}"#,
        )
        .expect("fixture package");
        fs::write(root.join("src/button.tsx"), "export interface ButtonProps { /** Label. */ label: string; } export function Button({ label = \"Save\" }: ButtonProps) { return label; }").expect("props source");
        fs::write(root.join("docs/demos/basic.demo.tsx"), "export const meta = { title: \"Basic\" }; export default function Demo() { return <button>OK</button>; }").expect("demo");
        root
    }

    fn write_fixture_navigation(root: &Path) {
        let docs = root.join("docs");
        let mut mdx_files = Vec::new();
        let mut demo_files = Vec::new();
        scan_files(&docs, &mut mdx_files, &mut demo_files).expect("scan fixture pages");
        let mut pages = mdx_files
            .into_iter()
            .map(|path| {
                slash_path(
                    path.strip_prefix(&docs)
                        .expect("fixture page beneath docs")
                        .with_extension(""),
                )
            })
            .collect::<Vec<_>>();
        pages.sort();
        let pages = pages
            .iter()
            .map(|page| format!("\"{page}\""))
            .collect::<Vec<_>>()
            .join(", ");
        fs::write(
            docs.join("navigation.toml"),
            format!("[[group]]\nid = \"test\"\ntitle = \"Test\"\npages = [{pages}]\n"),
        )
        .expect("fixture navigation");
    }

    fn generate_fixture(
        root: &Path,
        options: &DocsOptions,
        mode: BuildMode,
    ) -> Result<GeneratedProject, DocsError> {
        write_fixture_navigation(root);
        generate(root, options, mode)
    }

    #[test]
    fn leaves_imports_inside_fenced_code_as_documentation_text() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            "# Home\n\n```tsx\nimport \"./styles.css\";\nexport default function Example() {}\n```\n",
        )
        .unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();
        assert!(page.contains(r#"import \"./styles.css\";"#));
        assert!(!page.contains("@wake/docs-project/docs/styles.css"));
    }

    #[test]
    fn rejects_react_versions_older_than_19() {
        let root = fixture();
        fs::write(
            root.join("package.json"),
            r#"{"dependencies":{"react":"^18.3.1","react-dom":"^19.0.0"}}"#,
        )
        .unwrap();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();
        let error =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(
            matches!(error, DocsError::InvalidConfig(message) if message.contains("react 19+") && message.contains("^18.3.1"))
        );
    }

    #[test]
    fn compiles_gfm_mdx_esm_macros_and_frontmatter() {
        let root = fixture();
        fs::write(
            root.join("docs/button.mdx"),
            r#"+++
title = "Button"
description = "Actions"
kind = "guide"
status = "stable"
+++

import Badge from "../src/badge.tsx"

# Button

| State | Meaning |
| --- | --- |
| stable | Ready |

<Badge>{1 + 1}</Badge>
<Demo src="./demos/basic.demo.tsx" />
<Demos glob="./demos/*.demo.tsx" columns={2} />
<API source="../src/button.tsx" symbol="ButtonProps" component="Button" />
"#,
        )
        .expect("mdx");
        fs::write(root.join("src/badge.tsx"), "export default () => null").expect("badge");
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        assert_eq!(generated.routes.len(), 1);
        assert_eq!(generated.routes[0].slug, "/button");
        let page = fs::read_to_string(generated.generated_dir.join("pages/button.tsx")).unwrap();
        assert!(page.contains("@wake/docs-project/src/badge.tsx"));
        assert!(page.contains("<table>"));
        assert!(page.contains("<Badge>{1 + 1}</Badge>"));
        assert!(page.contains("__wakePage"));
        assert!(page.contains("sourceMappingURL=button.tsx.map"));
        let registry = fs::read_to_string(generated.generated_dir.join("registry.ts")).unwrap();
        assert!(registry.contains("basic.demo.tsx"));
        assert!(registry.contains("ButtonProps"));
        assert!(!registry.contains(&root.to_string_lossy().to_string()));
    }

    #[test]
    fn lifts_block_jsx_out_of_markdown_paragraphs() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            r#"# Blocks

<div className="cards">
  <div className="card">One</div>
  <div className="card">Two</div>
</div>

<div className="callout"><strong>Title</strong> Content</div>

<section>
Text <em>inline</em>

<div className="mixed-block">Block</div>

Tail
</section>
"#,
        )
        .unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();

        assert!(page.contains(r#"<div className={"cards"}><div className={"card"}>"#));
        assert!(page.contains(r#"<div className={"callout"}><strong>"#));
        assert!(page.contains(r#"<section><p>"#));
        assert!(page.contains(r#"</p><div className={"mixed-block"}>"#));
        for invalid in ["<p><div", "<p><section", "<p><article"] {
            assert!(
                !page.contains(invalid),
                "generated invalid JSX: {invalid}\n{page}"
            );
        }
    }

    #[test]
    fn keeps_markdown_children_but_lifts_standalone_components_and_fragments() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            r#"# Components

<Panel>

Markdown **strong**.

</Panel>

<Card />

<>
  <div>One</div>
  <article>Two</article>
</>

Inline <Badge>status</Badge> text.
"#,
        )
        .unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();

        assert!(page.contains("<Panel><p>"), "{page}");
        assert!(page.contains("<Card />"), "{page}");
        assert!(!page.contains("<p><Card />"), "{page}");
        assert!(page.contains("<><div>"), "{page}");
        assert!(!page.contains("<p><div"), "{page}");
        assert!(page.contains("<p>{\"Inline \"}<Badge>"), "{page}");
    }

    #[test]
    fn keeps_jsx_blocks_valid_next_to_markdown_flow_nodes() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            r#"# Adjacent

> Quote

<div>After quote</div>

- Item

<section>After list</section>

| A | B |
| --- | --- |
| 1 | 2 |

<article>After table</article>
"#,
        )
        .unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();

        assert!(page.contains("</blockquote>\n    <div>"), "{page}");
        assert!(page.contains("</ul>\n    <section>"), "{page}");
        assert!(page.contains("</div>\n    <article>"), "{page}");
        assert!(!page.contains("<p><div"), "{page}");
        assert!(!page.contains("<p><section"), "{page}");
        assert!(!page.contains("<p><article"), "{page}");
    }

    #[test]
    fn highlights_fenced_code_and_renders_semantic_tables() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            r####"# Code

## Example

```tsx title="button.tsx" {2}
import React from "react"
const count: number = 2
// Comment
```

| Name | Type |
| --- | --- |
| count | number |
"####,
        )
        .unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();
        assert!(page.contains("<CodeBlock"));
        assert!(page.contains("syntax-keyword"));
        assert!(page.contains("syntax-number"));
        assert!(page.contains("syntax-comment"));
        assert!(page.contains("code-line is-highlighted"));
        assert!(page.contains("button.tsx"));
        assert!(page.contains("heading-anchor"));
        assert!(page.contains("aria-label={\"Example\"}"));
        assert!(page.contains("<thead><tr><th>"));

        let windows_lines = highlight_code("const first = 1;\r\nconst second = 2;", "tsx");
        assert!(windows_lines.iter().all(|line| !line.contains('\r')));
        assert!(
            highlight_code("Get-ChildItem # Files", "powershell")[0].contains("syntax-comment")
        );
        assert!(highlight_code("SELECT * FROM docs -- Pages", "sql")[0].contains("syntax-keyword"));
        assert_eq!(
            code_highlighted_lines(Some("{1-999999}"), 2),
            BTreeSet::from([1, 2])
        );
    }

    #[test]
    fn renders_task_lists_with_stable_runtime_hooks() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            "# Checklist\n\n- [ ] Pending\n- [x] Complete\n",
        )
        .unwrap();
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let page = fs::read_to_string(generated.generated_dir.join("pages/index.tsx")).unwrap();
        assert!(page.contains("<ul className=\"task-list\">"));
        assert!(page.contains("<li className=\"task-list-item\">"));
        assert!(page.contains("checked={false} disabled"));
        assert!(page.contains("checked={true} disabled"));
    }

    #[test]
    fn rejects_dynamic_compile_time_attributes_with_location() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            "# Home\n\n<Demo src={chooseDemo()} />\n",
        )
        .expect("mdx");
        let error =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(matches!(error, DocsError::InvalidMacro { line: 3, .. }));
    }

    #[test]
    fn derives_routes_from_paths_and_excludes_production_drafts() {
        let root = fixture();
        fs::write(root.join("docs/a.mdx"), "# A").unwrap();
        fs::write(root.join("docs/b.mdx"), "+++\ndraft = true\n+++\n# B").unwrap();
        let development =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        assert_eq!(
            development
                .routes
                .iter()
                .map(|route| route.slug.as_str())
                .collect::<Vec<_>>(),
            vec!["/a", "/b"]
        );
        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Production).unwrap();
        assert_eq!(generated.routes.len(), 1);
        assert_eq!(generated.routes[0].slug, "/a");
    }

    #[test]
    fn navigation_manifest_controls_group_section_and_page_order() {
        let root = fixture();
        fs::write(
            root.join("docs/install.mdx"),
            "+++\ntitle = \"安装\"\n+++\n# Install\n",
        )
        .expect("install");
        fs::write(
            root.join("docs/build.mdx"),
            "+++\ntitle = \"构建\"\n+++\n# Build\n",
        )
        .expect("build");
        fs::write(
            root.join("docs/navigation.toml"),
            r#"[[group]]
id = "app"
title = "应用开发"
pages = ["install"]

[[group.section]]
id = "release"
title = "构建与发布"
pages = ["build"]
"#,
        )
        .expect("navigation");
        let generated = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        assert_eq!(
            generated
                .routes
                .iter()
                .map(|route| (
                    route.title.as_str(),
                    route.group.as_str(),
                    route.section.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![("安装", "应用开发", ""), ("构建", "应用开发", "构建与发布")]
        );
        let build_page =
            fs::read_to_string(generated.generated_dir.join("pages/build.tsx")).unwrap();
        assert!(build_page.contains(r#""group":"应用开发""#));
        assert!(build_page.contains(r#""section":"构建与发布""#));
    }

    #[test]
    fn rejects_retired_frontmatter_fields() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            "+++\ntitle = \"Home\"\nslug = \"/legacy\"\n+++\n# Home\n",
        )
        .unwrap();
        let error =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(
            matches!(error, DocsError::Frontmatter(_, message) if message.contains("unknown field `slug`"))
        );
    }

    #[test]
    fn navigation_rejects_missing_duplicate_and_unlisted_pages() {
        let root = fixture();
        fs::write(root.join("docs/a.mdx"), "# A").unwrap();
        fs::write(root.join("docs/b.mdx"), "# B").unwrap();
        fs::write(
            root.join("docs/navigation.toml"),
            "[[group]]\nid = \"test\"\ntitle = \"Test\"\npages = [\"a\", \"a\", \"missing\"]\n",
        )
        .unwrap();
        let error = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(
            matches!(error, DocsError::Navigation(_, message) if message.contains("appears more than once"))
        );

        fs::write(
            root.join("docs/navigation.toml"),
            "[[group]]\nid = \"test\"\ntitle = \"Test\"\npages = [\"a\", \"missing\"]\n",
        )
        .unwrap();
        let error = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(
            matches!(error, DocsError::Navigation(_, message) if message.contains("missing page `missing`"))
        );

        fs::write(
            root.join("docs/navigation.toml"),
            "[[group]]\nid = \"test\"\ntitle = \"Test\"\npages = [\"a\"]\n",
        )
        .unwrap();
        let error = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(
            matches!(error, DocsError::Navigation(_, message) if message.contains("page `b` is not listed"))
        );
    }

    #[test]
    fn hidden_pages_do_not_need_navigation_entries() {
        let root = fixture();
        fs::write(root.join("docs/a.mdx"), "# A").unwrap();
        fs::write(
            root.join("docs/hidden.mdx"),
            "+++\nhidden = true\n+++\n# Hidden\n",
        )
        .unwrap();
        fs::write(
            root.join("docs/navigation.toml"),
            "[[group]]\nid = \"test\"\ntitle = \"Test\"\npages = [\"a\"]\n",
        )
        .unwrap();
        let generated = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        assert_eq!(generated.routes.len(), 2);
        assert!(generated.routes.iter().any(|route| route.hidden));
    }

    #[test]
    fn sidebar_keeps_status_in_the_page_header_instead_of_navigation_items() {
        let runtime = include_str!("../runtime/app.tsx");
        let sidebar_start = runtime.find("function Sidebar").expect("sidebar");
        let sidebar_end = runtime[sidebar_start..]
            .find("function TableOfContents")
            .map(|offset| sidebar_start + offset)
            .expect("table of contents");
        let sidebar = &runtime[sidebar_start..sidebar_end];

        assert!(!sidebar.contains("StatusBadge"));
        assert!(sidebar.contains("aria-current={active ? \"page\" : undefined}"));
        assert!(runtime.contains("className=\"breadcrumbs\""));
        assert!(runtime.contains("<StatusBadge status={meta.status} />"));
        assert!(runtime.contains("sessionStorage"));
        assert!(runtime.contains("wake-docs-user-expanded-sections"));
        assert!(!runtime.contains("wake-docs-expanded-sections"));
        assert!(!runtime.contains("setExpanded((current) => current.has(activeKey)"));
    }

    #[test]
    fn page_edit_only_rewrites_the_page_and_its_source_map() {
        let root = fixture();
        let page = root.join("docs/index.mdx");
        fs::write(&page, "# Home\n\nFirst paragraph.\n").unwrap();
        let first =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let unchanged =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        assert!(unchanged.changed_files.is_empty());

        fs::write(&page, "# Home\n\nSecond paragraph.\n").unwrap();
        let changed =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let relative: BTreeSet<_> = changed
            .changed_files
            .iter()
            .filter_map(|path| path.strip_prefix(&first.generated_dir).ok())
            .map(slash_path)
            .collect();
        assert_eq!(
            relative,
            BTreeSet::from([
                "pages/index.tsx".to_string(),
                "pages/index.tsx.map".to_string(),
            ])
        );
    }

    #[test]
    fn demo_edit_only_rewrites_its_lazy_source_module() {
        let root = fixture();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();
        let first =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        fs::write(
            root.join("docs/demos/basic.demo.tsx"),
            "export default function Demo() { return <button>Changed</button>; }",
        )
        .unwrap();
        let changed =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let relative: BTreeSet<_> = changed
            .changed_files
            .iter()
            .filter_map(|path| path.strip_prefix(&first.generated_dir).ok())
            .map(slash_path)
            .collect();
        assert_eq!(
            relative,
            BTreeSet::from(["demo-source/docs/demos/basic.demo.source.tsx".to_string()])
        );
        let source_module = fs::read_to_string(
            first
                .generated_dir
                .join("demo-source/docs/demos/basic.demo.source.tsx"),
        )
        .unwrap();
        assert!(source_module.contains("export const language = \"tsx\""));
        assert!(source_module.contains("export const highlighted = <>"));
        assert!(source_module.contains("syntax-keyword"));
        let registry = fs::read_to_string(first.generated_dir.join("registry.ts")).unwrap();
        assert!(registry.contains("loadSource"));
        assert!(!registry.contains("Changed</button>"));
    }

    #[test]
    fn static_route_shells_include_page_metadata_and_locale() {
        let root = fixture();
        let outdir = root.join("dist");
        let shell = "<!doctype html><html lang=\"en-US\"><head><title>wake app</title></head><body></body></html>";
        let route = |title: &str, description: &str, slug: &str| RouteInfo {
            id: slug.to_string(),
            file: format!("docs/{}.mdx", slug.trim_matches('/')),
            title: title.to_string(),
            description: description.to_string(),
            kind: "guide".to_string(),
            group: "指南".to_string(),
            group_id: "guide".to_string(),
            section: String::new(),
            section_id: String::new(),
            slug: slug.to_string(),
            status: "stable".to_string(),
            draft: false,
            hidden: false,
            headings: Vec::new(),
        };
        let routes = vec![
            route("Wake & Docs", "中文 <首页>", "/"),
            route(
                "创建 React 应用",
                "使用 \"Wake\" 构建",
                "/start/create-react-app",
            ),
        ];

        write_route_shells(&outdir, &routes, shell, "Wake & Docs", "默认描述", "zh-CN").unwrap();

        let root_html = fs::read_to_string(outdir.join("index.html")).unwrap();
        assert!(root_html.contains("<html lang=\"zh-CN\">"));
        assert!(root_html.contains("<title>Wake &amp; Docs</title>"));
        assert!(root_html.contains("content=\"中文 &lt;首页&gt;\""));
        let guide_html =
            fs::read_to_string(outdir.join("start/create-react-app/index.html")).unwrap();
        assert!(guide_html.contains("<title>创建 React 应用 · Wake &amp; Docs</title>"));
        assert!(guide_html.contains("content=\"使用 &quot;Wake&quot; 构建\""));
        let not_found_html = fs::read_to_string(outdir.join("404.html")).unwrap();
        assert!(not_found_html.contains("<title>404 · Wake &amp; Docs</title>"));
        assert!(not_found_html.contains("找不到请求的文档页面。"));
    }

    #[test]
    fn public_logo_follows_the_configured_base_path() {
        let root = fixture();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();
        let options = DocsOptions {
            base_path: "/crab/".to_string(),
            logo: Some("/logo.svg".to_string()),
            ..DocsOptions::default()
        };
        let generated = generate_fixture(&root, &options, BuildMode::Production).unwrap();
        let config = fs::read_to_string(generated.generated_dir.join("config.tsx")).unwrap();
        assert!(config.contains(r#""basePath":"/crab/""#));
        assert!(config.contains(r#""logo":"/crab/logo.svg""#));
        assert!(config.contains(r#""locale":"zh-CN""#));
        assert_eq!(
            public_asset_url("/crab/", "https://cdn.example/logo.svg"),
            "https://cdn.example/logo.svg"
        );
    }

    #[test]
    fn docs_options_default_to_chinese_and_allow_explicit_english() {
        assert_eq!(DocsOptions::default().locale, "zh-CN");

        let options = DocsOptions {
            locale: "en-US".to_string(),
            ..DocsOptions::default()
        };
        let config = render_config(Path::new("."), &options, DocsMode::Site).unwrap();
        assert!(config.contains(r#""locale":"en-US""#));
    }

    #[test]
    fn accent_color_is_optional_and_serializes_explicit_overrides() {
        let default_config =
            render_config(Path::new("."), &DocsOptions::default(), DocsMode::Site).unwrap();
        assert!(default_config.contains(r#""accentColor":null"#));

        let options = DocsOptions {
            accent_color: Some("#7c3aed".to_string()),
            ..DocsOptions::default()
        };
        let explicit_config = render_config(Path::new("."), &options, DocsMode::Site).unwrap();
        assert!(explicit_config.contains(r##""accentColor":"#7c3aed""##));

        let invalid = DocsOptions {
            accent_color: Some("purple".to_string()),
            ..DocsOptions::default()
        };
        assert!(matches!(
            validate_options(&invalid),
            Err(DocsError::InvalidConfig(message))
                if message == "accent_color must be a six-digit hex color"
        ));
    }

    #[test]
    fn component_mode_builds_a_typed_demo_catalog_without_mdx() {
        let root = fixture();
        fs::write(
            root.join("docs/demos/basic.demo.tsx"),
            r#"
                import type { ButtonProps } from "../../src/button";
                export const meta = {
                    title: "Editable",
                    group: "Actions",
                    component: "Button",
                    order: 7,
                    args: { label: "Create" },
                };
                export default function Demo(props: ButtonProps) {
                    return <button>{props.label}</button>;
                }
            "#,
        )
        .unwrap();

        let generated = generate_with_mode(
            &root,
            &DocsOptions::default(),
            BuildMode::Development,
            DocsMode::Components,
        )
        .unwrap();

        assert_eq!(generated.mode, DocsMode::Components);
        assert!(generated.routes.is_empty());
        assert!(generated.warnings.is_empty());
        assert_eq!(generated.demos.len(), 1);
        let demo = &generated.demos[0];
        assert_eq!(demo.title, "Editable");
        assert_eq!(demo.group, "Actions");
        assert_eq!(demo.component, "Button");
        assert_eq!(demo.order, 7);
        assert_eq!(demo.control_count, 1);

        let registry = fs::read_to_string(generated.generated_dir.join("registry.ts")).unwrap();
        assert!(registry.contains(r#""kind":"string""#));
        assert!(registry.contains(r#""name":"label""#));
        let config = fs::read_to_string(generated.generated_dir.join("config.tsx")).unwrap();
        assert!(config.contains(r#""mode":"components""#));
        assert!(
            generated
                .generated_dir
                .join("runtime/components.tsx")
                .is_file()
        );
        let components_runtime =
            fs::read_to_string(generated.generated_dir.join("runtime/components.tsx")).unwrap();
        assert!(components_runtime.contains("@crab-dev/wake/internal/components-runtime"));
        assert!(!components_runtime.contains("from \"@crab-dev/rc-"));
        assert!(!components_runtime.contains("from \"lucide-react\""));
        assert!(
            generated
                .generated_dir
                .join("runtime/components-state.mjs")
                .is_file()
        );
        assert!(generated.entry.ends_with("runtime/components-entry.tsx"));
        assert!(
            !generated
                .generated_dir
                .join("runtime/site-entry.tsx")
                .exists()
        );
    }

    #[test]
    fn site_mode_emits_only_the_site_runtime_surface() {
        let root = fixture();
        fs::write(root.join("docs/index.mdx"), "# Home\n").unwrap();

        let generated =
            generate_fixture(&root, &DocsOptions::default(), BuildMode::Production).unwrap();

        assert!(generated.entry.ends_with("runtime/site-entry.tsx"));
        let entry = fs::read_to_string(&generated.entry).unwrap();
        assert!(entry.contains("runtime/app.tsx"));
        assert!(!entry.contains("ComponentsApp"));
        assert!(!entry.contains("components.css"));
        assert!(
            !generated
                .generated_dir
                .join("runtime/components.tsx")
                .exists()
        );
        assert!(
            !generated
                .generated_dir
                .join("runtime/components.css")
                .exists()
        );
        assert!(
            !generated
                .generated_dir
                .join("runtime/components-state.mjs")
                .exists()
        );
    }

    #[test]
    fn component_metadata_only_reads_top_level_overrides() {
        let source = r#"
            export const meta = {
                args: { group: "Nested", component: "Nested", order: 99 },
                // Only these top-level values describe the catalog entry.
                group: "Actions",
                component: "Button",
                order: 3,
            };
        "#;

        let meta = extract_static_demo_meta(source);
        assert_eq!(meta.group.as_deref(), Some("Actions"));
        assert_eq!(meta.component.as_deref(), Some("Button"));
        assert_eq!(meta.order, 3);
        assert_eq!(
            static_demo_args(source),
            Some(r#"{ group: "Nested", component: "Nested", order: 99 }"#)
        );
    }

    #[test]
    fn component_mode_warns_and_ignores_dynamic_meta_args() {
        let root = fixture();
        fs::write(
            root.join("docs/demos/basic.demo.tsx"),
            r#"
                import type { ButtonProps } from "../../src/button";
                const dynamicLabel = () => "Create";
                export const meta = { args: { label: dynamicLabel() } };
                export default function Demo(props: ButtonProps) {
                    return <button>{props.label}</button>;
                }
            "#,
        )
        .unwrap();

        let generated = generate_with_mode(
            &root,
            &DocsOptions::default(),
            BuildMode::Production,
            DocsMode::Components,
        )
        .unwrap();
        assert_eq!(generated.demos.len(), 1);
        assert!(
            generated
                .warnings
                .iter()
                .any(|warning| warning.contains("meta.args"))
        );
        assert_eq!(generated.demos[0].warnings, generated.warnings);
    }
}
