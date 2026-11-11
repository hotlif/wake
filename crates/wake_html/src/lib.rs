//! # wake_html — 构建期 HTML 生成（WAKE-COMPATIBILITY §M2 / 决策②）
//!
//! 旧实现使用 `react-dom/server` 的 `renderToString` 对 `bootstrap.tsx` 做 SSR 生成外壳；wake 无
//! JS 运行时，改为 **静态外壳 + 资源注入**：取显式模板或根目录 `index.html`（或内置默认外壳），
//! 在 `</head>` 前注入 `<script defer>`（JS chunk）与 `<link rel="stylesheet">`（CSS 产物），
//! 资源 URL 前缀 `public_path`。dev 下 CSS 经运行时 `<style>` 注入并随整页 Live Reload 重新求值，
//! `styles` 为空；
//! prod（`enable_css_extraction`）抽取为 `.css` 产物后 `styles` 非空，在此注入 `<link>`。
//!
//! 模板选择、读取和资源注入由开发与生产共用，无外部依赖。

use std::path::{Path, PathBuf};

/// Resolve exactly one template, relative to the project root rather than the config directory.
pub fn template_path(root: &Path, explicit: Option<&Path>) -> PathBuf {
    root.join(explicit.unwrap_or_else(|| Path::new("index.html")))
}

/// Only an absent implicit index.html uses the default shell. Explicit paths and all other read
/// errors fail closed, including invalid UTF-8. The error identifies the selected path.
pub fn load_template(root: &Path, explicit: Option<&Path>) -> std::io::Result<String> {
    let path = template_path(root, explicit);
    match std::fs::read_to_string(&path) {
        Ok(template) => Ok(template),
        Err(error) if explicit.is_none() && error.kind() == std::io::ErrorKind::NotFound => {
            Ok(default_shell())
        }
        Err(error) => Err(std::io::Error::new(
            error.kind(),
            format!("HTML template {}: {error}", path.display()),
        )),
    }
}

/// Insert product-specific tags using the same insertion point as generated resources.
pub fn inject_head(html: &mut String, tags: &str) {
    let position = html.find("</head>").unwrap_or(html.len());
    html.insert_str(position, tags);
}

/// HTML 生成输入。
pub struct HtmlInputs<'a> {
    /// 需注入的 JS 文件名（相对 `public_path`，按加载顺序）。
    pub scripts: &'a [String],
    /// 需注入的 CSS 文件名（相对 `public_path`）。
    pub styles: &'a [String],
    /// 静态资源公共路径（如 `/` 或 `/app/`）。
    pub public_path: &'a str,
}

/// 生成 HTML：`template` 为 `None` 时用内置默认外壳。注入点为 `</head>` 之前（无则追加）。
pub fn generate(template: Option<&str>, inputs: &HtmlInputs) -> String {
    let mut base = template.map(str::to_string).unwrap_or_else(default_shell);

    let mut inject = String::new();
    for css in inputs.styles {
        inject.push_str("<link rel=\"stylesheet\" href=\"");
        inject.push_str(&join_url(inputs.public_path, css));
        inject.push_str("\">\n");
    }
    for js in inputs.scripts {
        inject.push_str("<script defer src=\"");
        inject.push_str(&join_url(inputs.public_path, js));
        inject.push_str("\"></script>\n");
    }

    inject_head(&mut base, &inject);
    base
}

/// 拼接 `public_path` 与文件名，规范化斜杠（避免 `//` 或缺失 `/`）。
fn join_url(public_path: &str, file: &str) -> String {
    let file = file.trim_start_matches('/');
    if public_path.is_empty() {
        file.to_string()
    } else if public_path.ends_with('/') {
        format!("{public_path}{file}")
    } else {
        format!("{public_path}/{file}")
    }
}

/// 内置默认 HTML 外壳（含 `#root` 挂载点）。
pub fn default_shell() -> String {
    "<!doctype html>\n\
     <html lang=\"zh-CN\">\n\
     <head>\n\
     <meta charset=\"utf-8\">\n\
     <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
     <title>wake app</title>\n\
     </head>\n\
     <body>\n\
     <div id=\"root\"></div>\n\
     </body>\n\
     </html>\n"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs<'a>(
        scripts: &'a [String],
        styles: &'a [String],
        public_path: &'a str,
    ) -> HtmlInputs<'a> {
        HtmlInputs {
            scripts,
            styles,
            public_path,
        }
    }

    #[test]
    fn template_selection_reads_exactly_one_path() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("public")).unwrap();
        std::fs::write(root.path().join("public/index.html"), "PUBLIC").unwrap();
        assert_eq!(load_template(root.path(), None).unwrap(), default_shell());
        std::fs::write(root.path().join("index.html"), "ROOT").unwrap();
        assert_eq!(load_template(root.path(), None).unwrap(), "ROOT");
        assert!(load_template(root.path(), Some(Path::new("absent.html"))).is_err());
        assert_eq!(
            load_template(root.path(), Some(Path::new("public/index.html"))).unwrap(),
            "PUBLIC"
        );
        let external = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(external.path(), "EXPLICIT_ABSOLUTE").unwrap();
        assert_eq!(
            load_template(root.path(), Some(external.path())).unwrap(),
            "EXPLICIT_ABSOLUTE"
        );
    }

    #[test]
    fn unreadable_templates_never_fall_back() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("index.html");
        std::fs::write(&path, [0xff]).unwrap();
        for explicit in [None, Some(Path::new("index.html"))] {
            let error = load_template(root.path(), explicit).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
            assert!(error.to_string().contains("index.html"));
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(load_template(root.path(), None).is_err());
    }

    #[test]
    fn injects_before_head_close() {
        let scripts = vec!["index.abcd.js".to_string()];
        let styles = vec!["index.efgh.css".to_string()];
        let html = generate(None, &inputs(&scripts, &styles, "/"));
        assert!(html.contains("<script defer src=\"/index.abcd.js\"></script>"));
        assert!(html.contains("<link rel=\"stylesheet\" href=\"/index.efgh.css\">"));
        // 注入在 </head> 之前。
        let inj = html.find("index.abcd.js").unwrap();
        let head = html.find("</head>").unwrap();
        assert!(inj < head);
        // 有挂载点。
        assert!(html.contains("id=\"root\""));
    }

    #[test]
    fn public_path_prefix() {
        let scripts = vec!["main.js".to_string()];
        let html = generate(None, &inputs(&scripts, &[], "/app/"));
        assert!(html.contains("src=\"/app/main.js\""));
    }

    #[test]
    fn public_path_without_trailing_slash() {
        let scripts = vec!["main.js".to_string()];
        let html = generate(None, &inputs(&scripts, &[], "/app"));
        assert!(html.contains("src=\"/app/main.js\""));
    }

    #[test]
    fn relative_public_path_is_preserved_for_file_protocol_builds() {
        let scripts = vec!["main.js".to_string()];
        let styles = vec!["main.css".to_string()];
        let html = generate(None, &inputs(&scripts, &styles, "./"));
        assert!(html.contains("src=\"./main.js\""));
        assert!(html.contains("href=\"./main.css\""));
    }

    #[test]
    fn custom_template_preserved() {
        let tpl = "<html><head><title>Mine</title></head><body><main></main></body></html>";
        let scripts = vec!["b.js".to_string()];
        let html = generate(Some(tpl), &inputs(&scripts, &[], "/"));
        assert!(html.contains("<title>Mine</title>"));
        assert!(html.contains("<main></main>"));
        assert!(html.contains("<script defer src=\"/b.js\"></script>"));
    }

    #[test]
    fn no_head_close_appends() {
        let tpl = "<div>no head</div>";
        let scripts = vec!["b.js".to_string()];
        let html = generate(Some(tpl), &inputs(&scripts, &[], "/"));
        assert!(html.starts_with("<div>no head</div>"));
        assert!(html.contains("<script defer src=\"/b.js\"></script>"));
    }
}
