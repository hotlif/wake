//! Static JSX accessibility checks over original grammar facts, without evaluating expressions.

use crate::aria::ascii_validation_view;
use crate::{LintDiagnostic, RuleLevel};
use std::collections::{BTreeMap, BTreeSet};
use wake_common::Span;
use wake_ecma_ast::{SourceJsxValue, SourcePrimitiveValue as Value};
use wake_ecma_parser::{SourceNode, SourceNodeKind as Kind, SourceParseOutput};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Proof {
    No,
    Maybe,
    Yes,
}

#[derive(Clone, Copy)]
enum Attribute<'a> {
    Missing,
    Unknown,
    Known(&'a Value),
}

impl Attribute<'_> {
    fn text(self, empty_allowed: bool) -> Proof {
        match self {
            Self::Unknown | Self::Known(Value::Unknown) => Proof::Maybe,
            Self::Known(Value::String(text))
                if empty_allowed || !ascii_validation_view(text).trim().is_empty() =>
            {
                Proof::Yes
            }
            _ => Proof::No,
        }
    }
    fn content(self) -> Proof {
        match self {
            Self::Missing => Proof::No,
            Self::Unknown => Proof::Maybe,
            Self::Known(value) => content_value(value),
        }
    }
    fn is_string(self, expected: &str) -> bool {
        matches!(self, Self::Known(Value::String(text)) if text.as_str().is_some_and(|text| text.eq_ignore_ascii_case(expected)))
    }
}

struct Tree<'a> {
    source: &'a str,
    nodes: &'a [SourceNode],
    children: Vec<Vec<usize>>,
    values: Vec<Option<&'a SourceJsxValue>>,
    openings: Vec<Option<usize>>,
    content: Vec<Proof>,
    controls: Vec<Proof>,
    visibility: Vec<Proof>,
}

impl<'a> Tree<'a> {
    fn new(source: &'a str, parsed: &'a SourceParseOutput) -> Self {
        let nodes = &parsed.syntax;
        let mut tree = Self {
            source,
            nodes,
            children: vec![Vec::new(); nodes.len()],
            values: vec![None; nodes.len()],
            openings: vec![None; nodes.len()],
            content: vec![Proof::No; nodes.len()],
            controls: vec![Proof::No; nodes.len()],
            visibility: vec![Proof::No; nodes.len()],
        };
        for (index, node) in nodes.iter().enumerate() {
            if let Some(parent) = node.parent {
                tree.children[parent].push(index);
                if node.kind == Kind::JsxOpeningElement {
                    tree.openings[parent] = Some(index);
                }
            }
        }
        for value in &parsed.jsx_values {
            tree.values[value.node] = Some(value);
        }
        for (index, node) in nodes.iter().enumerate() {
            let inherited = node
                .parent
                .map_or(Proof::No, |parent| tree.visibility[parent]);
            let own = tree.openings[index].map_or(Proof::No, |opening| {
                if tree.tag(opening).is_some_and(intrinsic) {
                    tree.hidden(opening)
                } else {
                    Proof::No
                }
            });
            tree.visibility[index] = inherited.max(own);
        }
        for index in (0..nodes.len()).rev() {
            tree.summarize(index);
        }
        tree
    }

    fn tag(&self, opening: usize) -> Option<&str> {
        let name = self.children[opening]
            .iter()
            .find(|&&child| self.nodes[child].kind == Kind::JsxName)?;
        Some(self.text(self.nodes[*name].span))
    }
    fn text(&self, span: Span) -> &str {
        &self.source[span.lo as usize..span.hi as usize]
    }
    fn attribute(&self, opening: usize, name: &str) -> Attribute<'a> {
        let mut value = Attribute::Missing;
        for &child in &self.children[opening] {
            if self.nodes[child].kind == Kind::JsxSpreadAttribute {
                value = Attribute::Unknown;
            } else if self.nodes[child].kind == Kind::JsxAttribute
                && self.children[child].iter().any(|&node| {
                    self.nodes[node].kind == Kind::JsxName
                        && self.text(self.nodes[node].span) == name
                })
            {
                value = self.values[child]
                    .map_or(Attribute::Unknown, |value| Attribute::Known(&value.value));
            }
        }
        value
    }
    fn named(&self, opening: usize) -> Proof {
        self.attribute(opening, "aria-label")
            .text(false)
            .max(self.attribute(opening, "aria-labelledby").text(false))
    }
    fn hidden(&self, opening: usize) -> Proof {
        let hidden = match self.attribute(opening, "hidden") {
            Attribute::Unknown | Attribute::Known(Value::Unknown) => Proof::Maybe,
            Attribute::Known(Value::Boolean(true) | Value::String(_)) => Proof::Yes,
            Attribute::Known(Value::Number(value)) if *value != 0.0 => Proof::Yes,
            _ => Proof::No,
        };
        let aria = match self.attribute(opening, "aria-hidden") {
            Attribute::Unknown | Attribute::Known(Value::Unknown) => Proof::Maybe,
            Attribute::Known(Value::Boolean(true)) => Proof::Yes,
            Attribute::Known(Value::String(value))
                if value
                    .as_str()
                    .is_some_and(|value| value.eq_ignore_ascii_case("true")) =>
            {
                Proof::Yes
            }
            _ => Proof::No,
        };
        let input = if self.tag(opening) == Some("input") {
            match self.attribute(opening, "type") {
                Attribute::Unknown | Attribute::Known(Value::Unknown) => Proof::Maybe,
                attribute if attribute.is_string("hidden") => Proof::Yes,
                _ => Proof::No,
            }
        } else {
            Proof::No
        };
        hidden.max(aria).max(input)
    }

    fn event(&self, opening: usize, name: &str) -> Proof {
        match self.attribute(opening, name) {
            Attribute::Missing
            | Attribute::Known(
                Value::Null | Value::Undefined | Value::Boolean(false) | Value::Empty,
            ) => Proof::No,
            Attribute::Unknown => Proof::Maybe,
            Attribute::Known(_) => Proof::Yes,
        }
    }

    fn boolean_attribute(&self, opening: usize, name: &str) -> Proof {
        match self.attribute(opening, name) {
            Attribute::Unknown | Attribute::Known(Value::Unknown) => Proof::Maybe,
            Attribute::Known(Value::Boolean(true) | Value::String(_)) => Proof::Yes,
            Attribute::Known(Value::Number(value)) if *value != 0.0 => Proof::Yes,
            _ => Proof::No,
        }
    }

    fn disabled(&self, opening: usize, tag: &str) -> Proof {
        let aria = match self.attribute(opening, "aria-disabled") {
            Attribute::Unknown | Attribute::Known(Value::Unknown) => Proof::Maybe,
            Attribute::Known(Value::Boolean(true)) => Proof::Yes,
            attribute if attribute.is_string("true") => Proof::Yes,
            _ => Proof::No,
        };
        if matches!(
            tag,
            "button" | "fieldset" | "input" | "optgroup" | "option" | "select" | "textarea"
        ) {
            aria.max(self.boolean_attribute(opening, "disabled"))
        } else {
            aria
        }
    }

    fn native_interactive(&self, index: usize, opening: usize, tag: &str) -> Proof {
        let native = match tag {
            "button" | "select" | "textarea" => Proof::Yes,
            "input" => match self.attribute(opening, "type") {
                Attribute::Unknown | Attribute::Known(Value::Unknown) => Proof::Maybe,
                attribute if attribute.is_string("hidden") => Proof::No,
                _ => Proof::Yes,
            },
            "a" | "area" => match self.attribute(opening, "href") {
                Attribute::Unknown | Attribute::Known(Value::Unknown) => Proof::Maybe,
                Attribute::Known(Value::String(_)) => Proof::Yes,
                _ => Proof::No,
            },
            "audio" | "video" => self.boolean_attribute(opening, "controls"),
            "summary" => self.summary_interactive(index),
            _ => Proof::No,
        };
        let editable = match self.attribute(opening, "contentEditable") {
            Attribute::Unknown | Attribute::Known(Value::Unknown) => Proof::Maybe,
            Attribute::Known(Value::Boolean(true)) => Proof::Yes,
            attribute
                if attribute.is_string("")
                    || attribute.is_string("true")
                    || attribute.is_string("plaintext-only") =>
            {
                Proof::Yes
            }
            _ => Proof::No,
        };
        native.max(editable)
    }

    fn summary_interactive(&self, index: usize) -> Proof {
        let Some(parent) = self.nodes[index].parent else {
            return Proof::No;
        };
        if matches!(
            self.nodes[parent].kind,
            Kind::JsxFragment | Kind::JsxExpressionContainer
        ) {
            return Proof::Maybe;
        }
        if self.openings[parent].and_then(|opening| self.tag(opening)) != Some("details") {
            return Proof::No;
        }
        for &sibling in &self.children[parent] {
            if sibling == index {
                return Proof::Yes;
            }
            if self.openings[sibling].and_then(|opening| self.tag(opening)) == Some("summary") {
                return Proof::No;
            }
            if matches!(
                self.nodes[sibling].kind,
                Kind::JsxFragment | Kind::JsxExpressionContainer
            ) || self.openings[sibling]
                .and_then(|opening| self.tag(opening))
                .is_some_and(|tag| !intrinsic(tag))
            {
                return Proof::Maybe;
            }
        }
        Proof::No
    }

    fn focusable(&self, index: usize, opening: usize, tag: &str) -> Proof {
        let tabindex = match self.attribute(opening, "tabIndex") {
            Attribute::Unknown | Attribute::Known(Value::Unknown) => Proof::Maybe,
            Attribute::Known(Value::String(_) | Value::Number(_)) => {
                let Attribute::Known(value) = self.attribute(opening, "tabIndex") else {
                    unreachable!()
                };
                if crate::aria::Property::Integer.valid(value) {
                    Proof::Yes
                } else {
                    Proof::No
                }
            }
            _ => Proof::No,
        };
        tabindex.max(self.native_interactive(index, opening, tag))
    }
    fn image_input(&self, opening: usize) -> bool {
        self.attribute(opening, "type").is_string("image")
    }
    fn children_proof(&self, index: usize, facts: &[Proof]) -> Proof {
        self.children[index]
            .iter()
            .map(|&child| facts[child])
            .max()
            .unwrap_or(Proof::No)
    }
    fn expression_proof(&self, index: usize, facts: &[Proof], content: bool) -> Proof {
        let Some(value) = self.values[index] else {
            return Proof::Maybe;
        };
        if let Some(expression) = value.expression
            && let Some(child) = self.children[index].iter().find(|&&child| {
                matches!(self.nodes[child].kind, Kind::JsxElement | Kind::JsxFragment)
                    && self.nodes[child].span == expression
            })
        {
            return facts[*child];
        }
        if content {
            content_value(&value.value)
        } else if value.value == Value::Unknown {
            Proof::Maybe
        } else {
            Proof::No
        }
    }
    fn has_child_argument(&self, index: usize) -> bool {
        self.children[index]
            .iter()
            .any(|&child| match self.nodes[child].kind {
                Kind::JsxElement | Kind::JsxFragment => true,
                Kind::JsxExpressionContainer | Kind::JsxText => {
                    self.values[child].is_some_and(|value| value.value != Value::Empty)
                }
                _ => false,
            })
    }
    fn summarize(&mut self, index: usize) {
        match self.nodes[index].kind {
            Kind::JsxText => {
                self.content[index] =
                    self.values[index].map_or(Proof::No, |value| content_value(&value.value))
            }
            Kind::JsxExpressionContainer => {
                self.content[index] = self.expression_proof(index, &self.content, true);
                self.controls[index] = self.expression_proof(index, &self.controls, false);
            }
            Kind::JsxFragment => {
                self.content[index] = self.children_proof(index, &self.content);
                self.controls[index] = self.children_proof(index, &self.controls);
            }
            Kind::JsxElement => {
                let Some(opening) = self.openings[index] else {
                    return;
                };
                let Some(tag) = self.tag(opening) else {
                    return;
                };
                if !intrinsic(tag) {
                    self.content[index] = Proof::Maybe;
                    self.controls[index] = Proof::Maybe;
                    return;
                }
                let mut content = self
                    .children_proof(index, &self.content)
                    .max(self.named(opening));
                if !self.has_child_argument(index) {
                    content = content.max(self.attribute(opening, "children").content());
                }
                if matches!(tag, "img" | "area") || tag == "input" && self.image_input(opening) {
                    content = content.max(self.attribute(opening, "alt").text(false));
                }
                if tag == "object" {
                    content = content.max(self.attribute(opening, "title").text(false));
                }
                if matches!(
                    self.attribute(opening, "dangerouslySetInnerHTML"),
                    Attribute::Unknown | Attribute::Known(Value::Unknown)
                ) {
                    content = content.max(Proof::Maybe);
                }
                let controls = match tag {
                    "label" => Proof::No,
                    "button" | "meter" | "output" | "progress" | "select" | "textarea" => {
                        Proof::Yes
                    }
                    "input" if self.attribute(opening, "type").is_string("hidden") => Proof::No,
                    "input" => match self.attribute(opening, "type") {
                        Attribute::Unknown | Attribute::Known(Value::Unknown) => Proof::Maybe,
                        _ => Proof::Yes,
                    },
                    _ => self.children_proof(index, &self.controls),
                };
                self.content[index] = match self.hidden(opening) {
                    Proof::Yes => Proof::No,
                    Proof::Maybe if content != Proof::No => Proof::Maybe,
                    _ => content,
                };
                self.controls[index] = controls;
            }
            _ => {}
        }
    }
}

fn intrinsic(tag: &str) -> bool {
    tag.as_bytes().first().is_some_and(u8::is_ascii_lowercase) && !tag.contains('.')
}
fn content_value(value: &Value) -> Proof {
    match value {
        Value::Unknown => Proof::Maybe,
        Value::String(text) if !ascii_validation_view(text).trim().is_empty() => Proof::Yes,
        Value::Number(_) => Proof::Yes,
        _ => Proof::No,
    }
}

pub(crate) fn check(
    source: &str,
    parsed: &SourceParseOutput,
    levels: &BTreeMap<String, RuleLevel>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    if !levels
        .iter()
        .any(|(id, level)| id.starts_with("a11y/") && *level != RuleLevel::Off)
    {
        return;
    }
    let tree = Tree::new(source, parsed);
    let mut report = |id: &str, message_id: &str, opening: usize, message: &str| {
        if levels[id] != RuleLevel::Off {
            diagnostics.push(LintDiagnostic {
                rule_id: id.into(),
                level: levels[id],
                message_id: message_id.into(),
                message: message.into(),
                start: tree.nodes[opening].span.lo,
                end: tree.nodes[opening].span.hi,
                fix: None,
            });
        }
    };
    for (index, node) in tree.nodes.iter().enumerate() {
        if node.kind != Kind::JsxElement {
            continue;
        }
        let Some(opening) = tree.openings[index] else {
            continue;
        };
        let Some(tag) = tree.tag(opening) else {
            continue;
        };
        if !intrinsic(tag) {
            continue;
        }
        // Unknown property spellings are authoring mistakes even before a spread.
        for &attribute in &tree.children[opening] {
            if tree.nodes[attribute].kind != Kind::JsxAttribute {
                continue;
            }
            let Some(&name) = tree.children[attribute]
                .iter()
                .find(|&&child| tree.nodes[child].kind == Kind::JsxName)
            else {
                continue;
            };
            let name = tree.text(tree.nodes[name].span);
            if name
                .get(..5)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("aria-"))
                && crate::aria::property(name).is_none()
            {
                report(
                    "a11y/aria-props",
                    "property",
                    attribute,
                    "Use a recognized lowercase ARIA property name.",
                );
            }
        }
        let mut seen = BTreeSet::new();
        for &attribute in tree.children[opening].iter().rev() {
            if tree.nodes[attribute].kind == Kind::JsxSpreadAttribute {
                break;
            }
            if tree.nodes[attribute].kind != Kind::JsxAttribute {
                continue;
            }
            let Some(&name) = tree.children[attribute]
                .iter()
                .find(|&&child| tree.nodes[child].kind == Kind::JsxName)
            else {
                continue;
            };
            let name = tree.text(tree.nodes[name].span);
            if !seen.insert(name) {
                continue;
            }
            if let Some(property) = crate::aria::property(name)
                && let Some(value) = tree.values[attribute]
                && !property.valid(&value.value)
            {
                report(
                    "a11y/aria-proptypes",
                    "value",
                    attribute,
                    "Use a value of the declared ARIA property type.",
                );
            }
        }
        let invalid_role = match tree.attribute(opening, "role") {
            Attribute::Missing
            | Attribute::Unknown
            | Attribute::Known(Value::Unknown | Value::Null | Value::Undefined) => false,
            Attribute::Known(Value::String(role)) => !ascii_validation_view(role)
                .split_ascii_whitespace()
                .any(crate::aria::concrete_role),
            _ => true,
        };
        if invalid_role {
            report(
                "a11y/aria-role",
                "role",
                opening,
                "Use a concrete ARIA role or a fallback list containing one.",
            );
        }
        if matches!(tag, "img" | "area") || tag == "input" && tree.image_input(opening) {
            if tree
                .attribute(opening, "alt")
                .text(tag == "img")
                .max(tree.named(opening))
                == Proof::No
            {
                report(
                    "a11y/alt-text",
                    "alternative",
                    opening,
                    "Provide a text alternative for this image.",
                );
            }
        } else if tag == "object" && tree.content[index] == Proof::No {
            report(
                "a11y/alt-text",
                "alternative",
                opening,
                "Provide a name or fallback content for this object.",
            );
        }
        if tree.visibility[index] == Proof::No && tree.disabled(opening, tag) == Proof::No {
            let role_text = match tree.attribute(opening, "role") {
                Attribute::Known(Value::String(role)) => Some(ascii_validation_view(role)),
                _ => None,
            };
            let role = role_text.as_deref().and_then(|role| {
                role.split_ascii_whitespace()
                    .find(|role| crate::aria::concrete_role(role))
            });
            let click = tree.event(opening, "onClick");
            let keyboard = ["onKeyDown", "onKeyUp", "onKeyPress"]
                .into_iter()
                .map(|name| tree.event(opening, name))
                .max()
                .unwrap_or(Proof::No);
            if click == Proof::Yes
                && keyboard == Proof::No
                && tree.native_interactive(index, opening, tag) == Proof::No
                && !matches!(role, Some("none" | "presentation"))
                && !matches!(
                    tree.attribute(opening, "role"),
                    Attribute::Unknown | Attribute::Known(Value::Unknown)
                )
            {
                report(
                    "a11y/click-events-have-key-events",
                    "keyboard",
                    opening,
                    "Provide a keyboard handler for this click interaction.",
                );
            }
            if click.max(keyboard) == Proof::Yes
                && role.is_some_and(crate::aria::interactive_role)
                && tree.focusable(index, opening, tag) == Proof::No
            {
                report(
                    "a11y/interactive-supports-focus",
                    "focus",
                    opening,
                    "Make this interactive role focusable with a native control or an integer tabIndex.",
                );
            }
        }
        if tag == "a" {
            if tree.visibility[index] == Proof::No && tree.content[index] == Proof::No {
                report(
                    "a11y/anchor-has-content",
                    "content",
                    opening,
                    "Anchor has no accessible content or name.",
                );
            }
            let invalid = match tree.attribute(opening, "href") {
                Attribute::Unknown | Attribute::Known(Value::Unknown) => false,
                Attribute::Known(Value::String(value)) => {
                    let normalized = ascii_validation_view(value)
                        .chars()
                        .filter(|character| *character > '\u{20}' && *character != '\u{7f}')
                        .collect::<String>()
                        .to_ascii_lowercase();
                    normalized.is_empty()
                        || normalized == "#"
                        || normalized.starts_with("javascript:")
                }
                _ => true,
            };
            if invalid {
                report(
                    "a11y/anchor-is-valid",
                    "href",
                    opening,
                    "Use a valid href, or a button for an action without a destination.",
                );
            }
        }
        if tag == "label" && tree.visibility[index] == Proof::No {
            if tree.content[index] == Proof::No {
                report(
                    "a11y/label-has-associated-control",
                    "content",
                    opening,
                    "Label has no accessible text.",
                );
            }
            let association = match tree.attribute(opening, "htmlFor") {
                Attribute::Missing | Attribute::Known(Value::Null | Value::Undefined) => {
                    tree.children_proof(index, &tree.controls)
                }
                attribute => attribute.text(false),
            };
            if association == Proof::No {
                report(
                    "a11y/label-has-associated-control",
                    "control",
                    opening,
                    "Associate this label using htmlFor or a nested labelable control.",
                );
            }
        }
    }
}
