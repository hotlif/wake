use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PathZone {
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub except: Vec<String>,
    pub message: Option<String>,
}

pub(crate) fn regex(pattern: &str) -> bool {
    pattern.chars().count() <= 4096 && regex::Regex::new(pattern).is_ok()
}

pub(crate) fn regex_list(value: &Value) -> bool {
    value.as_array().is_some_and(|list| {
        list.len() <= 64 && list.iter().all(|entry| entry.as_str().is_some_and(regex))
    })
}

pub(crate) fn zones(value: &Value) -> bool {
    let Some(list) = value.as_array().filter(|list| list.len() <= 64) else {
        return false;
    };
    list.iter().all(|entry| {
        if entry
            .get("message")
            .is_some_and(|message| !message.is_string())
        {
            return false;
        }
        serde_json::from_value::<PathZone>(entry.clone()).is_ok_and(|zone| {
            regex(&zone.from)
                && regex(&zone.to)
                && zone.except.len() <= 64
                && zone.except.iter().all(|pattern| regex(pattern))
                && zone
                    .message
                    .as_ref()
                    .is_none_or(|message| message.chars().count() <= 4096)
        })
    })
}

pub(crate) fn zones_schema() -> Value {
    let pattern = json!({"type":"string","format":"rust-regex","maxLength":4096});
    json!({"type":"array","default":[],"maxItems":64,"items":{
        "type":"object","additionalProperties":false,"required":["from","to"],"properties":{
            "from":pattern,"to":pattern,
            "except":{"type":"array","maxItems":64,"items":pattern},
            "message":{"type":"string","maxLength":4096}
        }
    }})
}
