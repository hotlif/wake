use std::sync::OnceLock;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CssFacts {
    pub source: String,
    pub source_revision: String,
    pub properties: Vec<PropertyFact>,
    pub at_rules: Vec<String>,
    pub pseudos: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PropertyFact {
    pub name: String,
    pub description: String,
    pub values: Vec<String>,
}

pub fn css_facts() -> &'static CssFacts {
    static FACTS: OnceLock<CssFacts> = OnceLock::new();
    FACTS.get_or_init(|| {
        serde_json::from_str(include_str!("../data/css-facts.json"))
            .expect("embedded CSS fact snapshot must be valid")
    })
}

pub fn property(name: &str) -> Option<&'static PropertyFact> {
    css_facts()
        .properties
        .iter()
        .find(|property| property.name.eq_ignore_ascii_case(name))
}
