use wake_common::Span;
use wake_css_in_js::value::Scope;

use super::{CompletionKind, HostLanguage, LanguageDocument, LanguageSeverity, SemanticKind};

fn analyze(source: &str) -> LanguageDocument {
    LanguageDocument::analyze("src/component.tsx", source, HostLanguage::TypeScriptReact)
}

#[test]
fn discovers_aliases_and_ignores_shadowing() {
    let source = "import { css as c } from '@crab-dev/css';\n\
        function ignored(c: unknown) { return c`color: red;`; }\n\
        const box = c`display: grid;`;";
    let document = analyze(source);
    assert_eq!(document.virtual_documents().len(), 1);
    assert!(
        document.virtual_documents()[0]
            .text
            .contains("display: grid;")
    );
    assert!(!document.virtual_documents()[0].text.contains("color: red;"));
}

#[test]
fn virtual_document_maps_unicode_crlf_and_interpolation_without_exposing_holes() {
    let source = "// 𝒳 中文\r\nimport { css } from '@crab-dev/css';\r\n\
        const box = css`color: red; width: ${token.size}px;`;";
    let document = analyze(source);
    let virtual_css = &document.virtual_documents()[0];
    let color = source.find("color").unwrap() as u32;
    let virtual_color = virtual_css.host_to_virtual_offset(color).unwrap();
    assert_eq!(
        virtual_css.virtual_to_host_offset(virtual_color),
        Some(color)
    );
    let interpolation = source.find("token.size").unwrap() as u32;
    assert_eq!(virtual_css.host_to_virtual_offset(interpolation), None);
    assert!(virtual_css.text.contains("width:              px;"));
    let (line, column) = document.source().location0_utf16(color);
    assert_eq!(line, 2);
    assert!(column > 0);
}

#[test]
fn incomplete_templates_are_tolerated() {
    let source = "import { css } from '@crab-dev/css'; const box = css`display: ${token.";
    let document = analyze(source);
    assert_eq!(document.virtual_documents().len(), 1);
    assert!(
        document
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.severity == LanguageSeverity::Error)
    );
}

#[test]
fn completes_properties_values_at_rules_and_pseudos() {
    let properties = "import { css } from '@crab-dev/css'; const box = css`disp`;";
    let document = analyze(properties);
    let items = document.completions(properties.find("disp").unwrap() as u32 + 4);
    assert!(
        items
            .iter()
            .any(|item| { item.label == "display" && item.kind == CompletionKind::Property })
    );

    let values = "import { css } from '@crab-dev/css'; const box = css`display: `;";
    let document = analyze(values);
    let items = document.completions(values.find("display: ").unwrap() as u32 + 9);
    assert!(items.iter().any(|item| item.label == "grid"));

    let global = "import { globalStyle } from '@crab-dev/css'; globalStyle`@med`;";
    let document = analyze(global);
    let items = document.completions(global.find("@med").unwrap() as u32 + 4);
    assert!(items.iter().any(|item| item.label == "@media"));

    let pseudo = "import { css } from '@crab-dev/css'; const box = css`&:hov`;";
    let document = analyze(pseudo);
    let items = document.completions(pseudo.find(":hov").unwrap() as u32 + 4);
    assert!(items.iter().any(|item| item.label == ":hover"));
}

#[test]
fn reports_unknown_properties_and_exposes_hover_and_tokens() {
    let source = "import { css } from '@crab-dev/css'; const box = css`\n  colour: red;\n  display: grid;\n`;";
    let document = analyze(source);
    assert!(
        document
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "CSS_UNKNOWN_PROPERTY")
    );
    let display = source.find("display").unwrap() as u32;
    let hover = document.hover(display + 2).unwrap();
    assert!(hover.markdown.contains("inner and outer display"));
    assert!(document.semantic_tokens().iter().any(|token| {
        token.kind == SemanticKind::Property && token.span.slice(source) == "display"
    }));
}

#[test]
fn finds_hex_colors() {
    let source = "import { css } from '@crab-dev/css'; const box = css`color: #336699cc;`;";
    let colors = analyze(source).colors();
    assert_eq!(colors.len(), 1);
    assert!((colors[0].red - 0.2).abs() < 0.001);
    assert!((colors[0].alpha - 0.8).abs() < 0.001);
}

#[test]
fn formatting_preserves_interpolations_and_emits_literal_only_edits() {
    let source = "import { css } from '@crab-dev/css'; const box = css`\n color: ${token.color};  \n &:hover {\n opacity: 1;\n }\n`;";
    let document = analyze(source);
    let edits = document.format(None);
    assert!(!edits.is_empty());
    let interpolation = Span::new(
        source.find("token.color").unwrap() as u32,
        (source.find("token.color").unwrap() + "token.color".len()) as u32,
    );
    assert!(
        edits
            .iter()
            .all(|edit| edit.span.hi <= interpolation.lo || interpolation.hi <= edit.span.lo)
    );
}

#[test]
fn compiler_diagnostics_share_the_build_time_source_of_truth() {
    let source = "import { css } from '@crab-dev/css'; function value() { return 1; }\n\
        const box = css`width: ${value()}px;`;";
    let diagnostics = analyze(source).compiler_diagnostics("src/component.tsx", &Scope::default());
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CRAB_CSS_STATIC_VALUE")
    );
}

#[test]
fn symbols_folds_and_selection_use_host_ranges() {
    let source =
        "import { css } from '@crab-dev/css'; const box = css`\n&:hover {\n color: red;\n}\n`;";
    let document = analyze(source);
    assert_eq!(document.symbols().len(), 1);
    assert_eq!(document.folding_ranges().len(), 1);
    let color = source.find("color").unwrap() as u32;
    assert!(
        document
            .selection_span(color)
            .unwrap()
            .contains_offset(color)
    );
}
