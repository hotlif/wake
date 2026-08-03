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
const RUNTIME_ENTRY: &str = include_str!("../runtime/entry.tsx");
const RUNTIME_STYLE: &str = include_str!("../runtime/styles.css");
const MINIMUM_REACT_MAJOR: u64 = 19;
static ATOMIC_WRITE_LOCK: Mutex<()> = Mutex::new(());
static NEXT_ATOMIC_WRITE_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildMode {
    Development,
    Production,
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
    pub accent_color: String,
}

impl Default for DocsOptions {
    fn default() -> Self {
        Self {
            source_dir: PathBuf::from("docs"),
            title: "Wake Docs".to_string(),
            description: String::new(),
            locale: "en-US".to_string(),
            logo: None,
            repository_url: None,
            base_path: "/".to_string(),
            preview: None,
            theme_css: None,
            default_theme: "system".to_string(),
            accent_color: "#8b5cf6".to_string(),
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
    pub changed_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouteInfo {
    pub id: String,
    pub file: String,
    pub title: String,
    pub description: String,
    pub group: String,
    pub group_order: i32,
    pub order: i32,
    pub slug: String,
    pub status: String,
    pub draft: bool,
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
#[serde(default)]
struct Frontmatter {
    title: Option<String>,
    description: String,
    group: Option<String>,
    group_order: i32,
    order: i32,
    slug: Option<String>,
    status: Option<String>,
    draft: bool,
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

#[derive(Debug, Serialize)]
struct DemoInfo {
    id: String,
    title: String,
    source: String,
    source_module: String,
    import_path: String,
}

/// Scan, compile, and atomically materialize the generated docs module tree.
pub fn generate(
    project_root: impl AsRef<Path>,
    options: &DocsOptions,
    mode: BuildMode,
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

    let mut pages = Vec::new();
    for path in &mdx_files {
        let page = compile_page(&root, &source_dir, path)?;
        if mode != BuildMode::Production || !page.route.draft {
            pages.push((path.clone(), page));
        }
    }
    pages.sort_by(|(_, left), (_, right)| {
        left.route
            .group_order
            .cmp(&right.route.group_order)
            .then_with(|| left.route.group.cmp(&right.route.group))
            .then_with(|| left.route.order.cmp(&right.route.order))
            .then_with(|| left.route.title.cmp(&right.route.title))
    });
    ensure_unique_routes(&pages)?;

    let demos = compile_demos(&root, &demo_files)?;
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
    let config = render_config(&root, options)?;
    let fixed = [
        ("registry.ts", registry.as_str()),
        ("config.tsx", config.as_str()),
        ("runtime/app.tsx", RUNTIME_APP),
        ("runtime/entry.tsx", RUNTIME_ENTRY),
        ("runtime/styles.css", RUNTIME_STYLE),
    ];
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

    Ok(GeneratedProject {
        root: root.clone(),
        generated_dir: generated_dir.clone(),
        entry: generated_dir.join("runtime/entry.tsx"),
        aliases: vec![
            ("@wake/docs".to_string(), generated_dir),
            ("@wake/docs-project".to_string(), root),
        ],
        watch_roots,
        routes,
        changed_files,
    })
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
    let group = frontmatter
        .group
        .clone()
        .unwrap_or_else(|| derive_group(relative));
    let derived_slug = derive_slug(relative);
    let slug = normalize_slug(frontmatter.slug.as_deref().unwrap_or(&derived_slug));
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
        group,
        group_order: frontmatter.group_order,
        order: frontmatter.order,
        slug,
        status,
        draft: frontmatter.draft,
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

fn compile_demos(root: &Path, files: &[PathBuf]) -> Result<Vec<DemoInfo>, DocsError> {
    files
        .iter()
        .map(|path| {
            let source = fs::read_to_string(path)
                .map_err(|error| DocsError::Io(path.clone(), error.to_string()))?;
            let relative = path.strip_prefix(root).map_err(|_| {
                DocsError::InvalidConfig(format!(
                    "demo `{}` is outside project root",
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
            Ok(DemoInfo {
                id: id.clone(),
                title: title_case(stem),
                source,
                source_module,
                import_path: format!("@wake/docs-project/{id}"),
            })
        })
        .collect()
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
        output.push_str(&format!(
            "  {{ id: {}, title: {}, load: () => import({}), loadSource: () => import(\"@wake/docs/demo-source/{}\") }},\n",
            js_string(&demo.id),
            js_string(&demo.title),
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

fn render_config(root: &Path, options: &DocsOptions) -> Result<String, DocsError> {
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
            Node::Paragraph(value) => format!("<p>{}</p>", self.render_children(&value.children)),
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
    let color = Regex::new(r"^#[0-9a-fA-F]{6}$").expect("valid color regex");
    if !color.is_match(&options.accent_color) {
        return Err(DocsError::InvalidConfig(
            "accent_color must be a six-digit hex color".to_string(),
        ));
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

fn derive_group(relative: &Path) -> String {
    let many = relative.components().count() > 1;
    relative
        .components()
        .next()
        .and_then(|component| match component {
            Component::Normal(value) if many => value.to_str(),
            _ => None,
        })
        .map(title_case)
        .unwrap_or_else(|| "Guide".to_string())
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

    #[test]
    fn leaves_imports_inside_fenced_code_as_documentation_text() {
        let root = fixture();
        fs::write(
            root.join("docs/index.mdx"),
            "# Home\n\n```tsx\nimport \"./styles.css\";\nexport default function Example() {}\n```\n",
        )
        .unwrap();
        let generated = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
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
        let error = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
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
group = "General"
order = 10
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
        let generated = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
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
        let generated = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
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
        let generated = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
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
        let error = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap_err();
        assert!(matches!(error, DocsError::InvalidMacro { line: 3, .. }));
    }

    #[test]
    fn detects_duplicate_routes_and_excludes_production_drafts() {
        let root = fixture();
        fs::write(root.join("docs/a.mdx"), "+++\nslug = \"/same\"\n+++\n# A").unwrap();
        fs::write(root.join("docs/b.mdx"), "+++\nslug = \"/same\"\n+++\n# B").unwrap();
        assert!(matches!(
            generate(&root, &DocsOptions::default(), BuildMode::Development),
            Err(DocsError::DuplicateRoute { .. })
        ));
        fs::write(
            root.join("docs/b.mdx"),
            "+++\ndraft = true\nslug = \"/same\"\n+++\n# B",
        )
        .unwrap();
        let generated = generate(&root, &DocsOptions::default(), BuildMode::Production).unwrap();
        assert_eq!(generated.routes.len(), 1);
    }
    #[test]
    fn group_order_controls_sidebar_route_order() {
        let root = fixture();
        fs::write(
            root.join("docs/later.mdx"),
            "+++\ntitle = \"稍后\"\ngroup = \"A\"\ngroup_order = 20\n+++\n# Later\n",
        )
        .unwrap();
        fs::write(
            root.join("docs/first.mdx"),
            "+++\ntitle = \"优先\"\ngroup = \"Z\"\ngroup_order = 10\n+++\n# First\n",
        )
        .unwrap();
        let generated = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        assert_eq!(
            generated
                .routes
                .iter()
                .map(|route| route.title.as_str())
                .collect::<Vec<_>>(),
            vec!["优先", "稍后"]
        );
    }

    #[test]
    fn page_edit_only_rewrites_the_page_and_its_source_map() {
        let root = fixture();
        let page = root.join("docs/index.mdx");
        fs::write(&page, "# Home\n\nFirst paragraph.\n").unwrap();
        let first = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        let unchanged = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        assert!(unchanged.changed_files.is_empty());

        fs::write(&page, "# Home\n\nSecond paragraph.\n").unwrap();
        let changed = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
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
        let first = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
        fs::write(
            root.join("docs/demos/basic.demo.tsx"),
            "export default function Demo() { return <button>Changed</button>; }",
        )
        .unwrap();
        let changed = generate(&root, &DocsOptions::default(), BuildMode::Development).unwrap();
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
            group: "指南".to_string(),
            group_order: 0,
            order: 0,
            slug: slug.to_string(),
            status: "stable".to_string(),
            draft: false,
            headings: Vec::new(),
        };
        let routes = vec![
            route("Wake & Docs", "中文 <首页>", "/"),
            route("快速开始", "使用 \"Wake\" 构建", "/guide/start"),
        ];

        write_route_shells(&outdir, &routes, shell, "Wake & Docs", "默认描述", "zh-CN").unwrap();

        let root_html = fs::read_to_string(outdir.join("index.html")).unwrap();
        assert!(root_html.contains("<html lang=\"zh-CN\">"));
        assert!(root_html.contains("<title>Wake &amp; Docs</title>"));
        assert!(root_html.contains("content=\"中文 &lt;首页&gt;\""));
        let guide_html = fs::read_to_string(outdir.join("guide/start/index.html")).unwrap();
        assert!(guide_html.contains("<title>快速开始 · Wake &amp; Docs</title>"));
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
        let generated = generate(&root, &options, BuildMode::Production).unwrap();
        let config = fs::read_to_string(generated.generated_dir.join("config.tsx")).unwrap();
        assert!(config.contains(r#""basePath":"/crab/""#));
        assert!(config.contains(r#""logo":"/crab/logo.svg""#));
        assert!(config.contains(r#""locale":"en-US""#));
        assert_eq!(
            public_asset_url("/crab/", "https://cdn.example/logo.svg"),
            "https://cdn.example/logo.svg"
        );
    }
}
