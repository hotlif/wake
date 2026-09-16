//! Owned ECMAScript string values, distinct from UTF-8 source text and identifiers.
//!
//! Every UTF-16 code-unit sequence is representable. Scalar-only values use shared UTF-8;
//! sequences containing isolated surrogates use shared UTF-16. Constructors canonicalize
//! these representations, so equality and hashing never depend on how a value was spelled.

use std::cmp::Ordering;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Repr {
    Utf8(Arc<str>),
    // Only constructors in this module can create this variant. It always contains at
    // least one isolated surrogate; paired-only values must be canonicalized to Utf8.
    Utf16(Arc<[u16]>),
}

/// A lossless JavaScript string value. Length and ordering use UTF-16 code units.
///
/// This is a value, not source spelling: a backslash followed by `ud800`, a replacement
/// character, and the code unit D800 are three different values. [`Self::as_str`] fails
/// for isolated surrogates; no lossy conversion or implicit display conversion is provided.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct JsString(Repr);

impl JsString {
    /// Preserve every code unit, canonicalizing well-formed sequences to shared UTF-8.
    pub fn from_utf16(units: &[u16]) -> Self {
        match String::from_utf16(units) {
            Ok(text) => Self::from(text),
            Err(_) => Self(Repr::Utf16(Arc::from(units))),
        }
    }

    /// Borrow a Unicode scalar string only when the value contains no isolated surrogates.
    pub fn as_str(&self) -> Option<&str> {
        match &self.0 {
            Repr::Utf8(text) => Some(text),
            Repr::Utf16(_) => None,
        }
    }

    /// Iterate the exact runtime value, including isolated surrogates, without allocation.
    pub fn code_units(&self) -> impl Iterator<Item = u16> + Clone + '_ {
        match &self.0 {
            Repr::Utf8(text) => CodeUnits::Utf8(text.encode_utf16()),
            Repr::Utf16(units) => CodeUnits::Utf16(units.iter().copied()),
        }
    }

    /// ECMAScript string length; this is not a source byte offset or a character count.
    pub fn len_utf16(&self) -> usize {
        match &self.0 {
            Repr::Utf8(text) => text.encode_utf16().count(),
            Repr::Utf16(units) => units.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        match &self.0 {
            Repr::Utf8(text) => text.is_empty(),
            Repr::Utf16(units) => units.is_empty(),
        }
    }

    /// Concatenate code units. A trailing high surrogate and leading low surrogate may
    /// form a scalar across this boundary, which is canonicalized just like any other pair.
    pub fn concat(&self, other: &Self) -> Self {
        if self.is_empty() {
            return other.clone();
        }
        if other.is_empty() {
            return self.clone();
        }
        if let (Some(left), Some(right)) = (self.as_str(), other.as_str()) {
            let mut text = String::with_capacity(left.len() + right.len());
            text.push_str(left);
            text.push_str(right);
            return Self::from(text);
        }
        Self::from_utf16(
            &self
                .code_units()
                .chain(other.code_units())
                .collect::<Vec<_>>(),
        )
    }
}

impl From<&str> for JsString {
    fn from(text: &str) -> Self {
        Self(Repr::Utf8(Arc::from(text)))
    }
}

impl From<String> for JsString {
    fn from(text: String) -> Self {
        Self(Repr::Utf8(Arc::from(text)))
    }
}

impl From<&String> for JsString {
    fn from(text: &String) -> Self {
        Self::from(text.as_str())
    }
}

impl From<&JsString> for JsString {
    fn from(value: &JsString) -> Self {
        value.clone()
    }
}

impl PartialEq<str> for JsString {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == Some(other)
    }
}

impl PartialEq<&str> for JsString {
    fn eq(&self, other: &&str) -> bool {
        self == *other
    }
}

impl PartialEq<String> for JsString {
    fn eq(&self, other: &String) -> bool {
        self == other.as_str()
    }
}

impl Default for JsString {
    fn default() -> Self {
        Self::from("")
    }
}

impl Ord for JsString {
    fn cmp(&self, other: &Self) -> Ordering {
        self.code_units().cmp(other.code_units())
    }
}

impl PartialOrd for JsString {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone)]
enum CodeUnits<'a> {
    Utf8(std::str::EncodeUtf16<'a>),
    Utf16(std::iter::Copied<std::slice::Iter<'a, u16>>),
}

impl Iterator for CodeUnits<'_> {
    type Item = u16;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Utf8(units) => units.next(),
            Self::Utf16(units) => units.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Utf8(units) => units.size_hint(),
            Self::Utf16(units) => units.size_hint(),
        }
    }
}
