//! # wake_html — 构建期 HTML 生成（WAKE-COMPATIBILITY §M2 / 决策②）
//!
//! 旧实现使用 `react-dom/server` 的 `renderToString` 对 `bootstrap.tsx` 做 SSR 生成外壳；wake 无
//! JS 运行时，改为 **Vite 式静态外壳 + 资源注入**：取用户 `public/index.html`（或内置默认外壳），
//! 在 `</head>` 前注入 `<script defer>`（JS chunk）与 `<link rel="stylesheet">`（CSS 产物），
//! 资源 URL 前缀 `public_path`。dev 下 CSS 经运行时 `<style>` 注入（利于 HMR），`styles` 为空；
//! prod（`enable_css_extraction`）抽取为 `.css` 产物后 `styles` 非空，在此注入 `<link>`。
//!
//! 纯字符串处理，无依赖。

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
    let base = template.map(str::to_string).unwrap_or_else(default_shell);

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

    match base.find("</head>") {
        Some(pos) => {
            let mut out = String::with_capacity(base.len() + inject.len());
            out.push_str(&base[..pos]);
            out.push_str(&inject);
            out.push_str(&base[pos..]);
            out
        }
        None => {
            let mut out = base;
            out.push_str(&inject);
            out
        }
    }
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
