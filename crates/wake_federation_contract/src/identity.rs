use std::fmt;

use serde::{Deserialize, Serialize};

macro_rules! string_identity {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(
            Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[must_use]
            pub fn into_string(self) -> String {
                self.0
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                Self(value.to_owned())
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                Self(value)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

string_identity!(
    ContainerName,
    "Stable page-level identity of a host or remote container."
);
string_identity!(
    ExposeKey,
    "Canonical exposed-module key, written as `./path` in a manifest."
);
string_identity!(
    BuildId,
    "Content-derived build identity supplied by the producer."
);

impl BuildId {
    /// Whether this value is safe to use as one opaque build URL segment.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        is_valid_identity_token(self.as_str(), 256)
    }
}

/// Stable identity of one loaded container generation.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContainerIdentity {
    pub container: ContainerName,
    pub build_id: BuildId,
    pub generation: u64,
}

/// Global module identity. Container-local numeric module IDs never cross this boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModuleIdentity {
    pub container: ContainerName,
    pub build_id: BuildId,
    pub expose: ExposeKey,
    pub generation: u64,
}

#[must_use]
pub(crate) fn is_valid_container_name(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    value.len() <= 64
        && first.is_ascii_alphabetic()
        && chars
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

#[must_use]
pub(crate) fn is_valid_expose_key(value: &str) -> bool {
    let Some(path) = value.strip_prefix("./") else {
        return false;
    };
    !path.is_empty()
        && value.len() <= 256
        && !path.contains(['\\', '?', '#'])
        && path.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '/' | '@' | '_' | '-' | '.')
        })
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && !matches!(segment, "." | ".."))
        && !value.chars().any(char::is_control)
}

#[must_use]
pub(crate) fn is_valid_config_expose_key(value: &str) -> bool {
    let canonical = if value.starts_with("./") {
        value.to_owned()
    } else {
        format!("./{value}")
    };
    is_valid_expose_key(&canonical)
}

#[must_use]
pub(crate) fn is_non_empty_token(value: &str, maximum_len: usize) -> bool {
    !value.is_empty() && value.len() <= maximum_len && !value.chars().any(char::is_control)
}

#[must_use]
pub(crate) fn is_valid_identity_token(value: &str, maximum_len: usize) -> bool {
    is_non_empty_token(value, maximum_len)
        && value.chars().all(|character| {
            character.is_ascii() && !character.is_whitespace() && character != '\\'
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_validators_are_ascii_and_path_stable() {
        assert!(is_valid_container_name("catalog_v2"));
        assert!(!is_valid_container_name("catalog/button"));
        assert!(is_valid_expose_key("./components/Button"));
        assert!(!is_valid_expose_key("components/Button"));
        assert!(!is_valid_expose_key("./components/../secret"));
    }
}
