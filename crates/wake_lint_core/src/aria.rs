//! Vocabulary facts from the pinned W3C specifications in engineering/LINT.md.
//! These checks validate author values, not browser error-recovery defaults.

use wake_ecma_ast::SourcePrimitiveValue as Value;

/// View used only by ASCII token/number and whitespace predicates. Escaping an isolated
/// surrogate keeps it non-whitespace and outside every valid ASCII token; following fallback
/// tokens remain visible. Never use this view for string identity, emitted code or source facts.
pub(crate) fn ascii_validation_view(value: &wake_common::JsString) -> std::borrow::Cow<'_, str> {
    if let Some(text) = value.as_str() {
        return std::borrow::Cow::Borrowed(text);
    }
    use std::fmt::Write as _;
    let mut text = String::new();
    for character in char::decode_utf16(value.code_units()) {
        match character {
            Ok(character) => text.push(character),
            Err(surrogate) => {
                let _ = write!(text, "\\u{:04x}", surrogate.unpaired_surrogate());
            }
        }
    }
    std::borrow::Cow::Owned(text)
}

#[derive(Clone, Copy)]
pub(crate) enum Property {
    String,
    Id,
    IdList,
    Integer,
    Number,
    Tokens(&'static [&'static str], bool),
}

pub(crate) fn property(name: &str) -> Option<Property> {
    use Property::*;
    Some(match name {
        "aria-atomic"
        | "aria-busy"
        | "aria-disabled"
        | "aria-modal"
        | "aria-multiline"
        | "aria-multiselectable"
        | "aria-readonly"
        | "aria-required" => Tokens(&["true", "false"], false),
        "aria-expanded" | "aria-grabbed" | "aria-hidden" | "aria-selected" => {
            Tokens(&["true", "false", "undefined"], false)
        }
        "aria-checked" | "aria-pressed" => Tokens(&["true", "false", "mixed", "undefined"], false),
        "aria-autocomplete" => Tokens(&["inline", "list", "both", "none"], false),
        "aria-current" => Tokens(
            &["page", "step", "location", "date", "time", "true", "false"],
            false,
        ),
        "aria-dropeffect" => Tokens(&["copy", "execute", "link", "move", "none", "popup"], true),
        "aria-haspopup" => Tokens(
            &["false", "true", "menu", "listbox", "tree", "grid", "dialog"],
            false,
        ),
        "aria-invalid" => Tokens(&["grammar", "false", "spelling", "true"], false),
        "aria-live" => Tokens(&["assertive", "off", "polite"], false),
        "aria-orientation" => Tokens(&["horizontal", "undefined", "vertical"], false),
        "aria-relevant" => Tokens(&["additions", "all", "removals", "text"], true),
        "aria-sort" => Tokens(&["ascending", "descending", "none", "other"], false),
        "aria-activedescendant" => Id,
        "aria-controls" | "aria-describedby" | "aria-details" | "aria-errormessage"
        | "aria-flowto" | "aria-labelledby" | "aria-owns" => IdList,
        "aria-colcount" | "aria-colindex" | "aria-colspan" | "aria-level" | "aria-posinset"
        | "aria-rowcount" | "aria-rowindex" | "aria-rowspan" | "aria-setsize" => Integer,
        "aria-valuemax" | "aria-valuemin" | "aria-valuenow" => Number,
        "aria-braillelabel"
        | "aria-brailleroledescription"
        | "aria-colindextext"
        | "aria-description"
        | "aria-keyshortcuts"
        | "aria-label"
        | "aria-placeholder"
        | "aria-roledescription"
        | "aria-rowindextext"
        | "aria-valuetext" => String,
        _ => return None,
    })
}

impl Property {
    pub(crate) fn valid(self, value: &Value) -> bool {
        if matches!(value, Value::Unknown | Value::Null | Value::Undefined) {
            return true;
        }
        let text = match value {
            Value::String(text) => ascii_validation_view(text),
            Value::Boolean(value) => {
                std::borrow::Cow::Borrowed(if *value { "true" } else { "false" })
            }
            Value::Number(number) => std::borrow::Cow::Owned(number.to_string()),
            _ => return false,
        };
        match self {
            Self::String => true,
            Self::Id => text.split_ascii_whitespace().count() == 1,
            Self::IdList => text.split_ascii_whitespace().next().is_some(),
            Self::Number => {
                !matches!(value, Value::Boolean(_))
                    && text.trim().parse::<f64>().is_ok_and(f64::is_finite)
            }
            Self::Integer => match value {
                Value::Number(number) => number.is_finite() && number.fract() == 0.0,
                Value::String(text) => {
                    let text = ascii_validation_view(text);
                    let text = text.trim().strip_prefix(['+', '-']).unwrap_or(text.trim());
                    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
                }
                _ => false,
            },
            Self::Tokens(allowed, list) => {
                let mut tokens = text.split_ascii_whitespace();
                let Some(first) = tokens.next() else {
                    return false;
                };
                allowed
                    .iter()
                    .any(|token| token.eq_ignore_ascii_case(first))
                    && tokens.all(|token| {
                        list && allowed.iter().any(|item| item.eq_ignore_ascii_case(token))
                    })
            }
        }
    }
}

pub(crate) fn concrete_role(role: &str) -> bool {
    // ARIA 1.3, DPUB 1.1 (including retained deprecated roles), Graphics 1.0.
    const ROLES: &str = "alert alertdialog application article banner blockquote button caption cell checkbox code columnheader combobox comment complementary contentinfo definition deletion dialog directory document emphasis feed figure form generic grid gridcell group heading image img insertion link list listbox listitem log main mark marquee math menu menubar menuitem menuitemcheckbox menuitemradio meter navigation none note option paragraph presentation progressbar radio radiogroup region row rowgroup rowheader scrollbar search searchbox sectionfooter sectionheader separator slider spinbutton status strong subscript suggestion superscript switch tab table tablist tabpanel term textbox time timer toolbar tooltip tree treegrid treeitem doc-abstract doc-acknowledgments doc-afterword doc-appendix doc-backlink doc-biblioentry doc-bibliography doc-biblioref doc-chapter doc-colophon doc-conclusion doc-cover doc-credit doc-credits doc-dedication doc-endnote doc-endnotes doc-epigraph doc-epilogue doc-errata doc-example doc-footnote doc-foreword doc-glossary doc-glossref doc-index doc-introduction doc-noteref doc-notice doc-pagebreak doc-pagefooter doc-pageheader doc-pagelist doc-part doc-preface doc-prologue doc-pullquote doc-qna doc-subtitle doc-tip doc-toc graphics-document graphics-object graphics-symbol";
    ROLES.split_ascii_whitespace().any(|known| known == role)
}

pub(crate) fn interactive_role(role: &str) -> bool {
    matches!(
        role,
        "button"
            | "checkbox"
            | "columnheader"
            | "combobox"
            | "grid"
            | "gridcell"
            | "link"
            | "listbox"
            | "menu"
            | "menubar"
            | "menuitem"
            | "menuitemcheckbox"
            | "menuitemradio"
            | "option"
            | "radio"
            | "radiogroup"
            | "row"
            | "rowheader"
            | "scrollbar"
            | "searchbox"
            | "slider"
            | "spinbutton"
            | "switch"
            | "tab"
            | "tablist"
            | "textbox"
            | "tree"
            | "treegrid"
            | "treeitem"
            | "doc-backlink"
            | "doc-biblioref"
            | "doc-glossref"
            | "doc-noteref"
    )
}
