//! Bounded translation of native semantic relations into source-bound Wake facts.
use super::project::{ProjectId, TypeProject};
use crate::WakeError;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use wake_lint_core::{
    CallArgumentType, CallType, MemberType, SourceAssertionKind, SourceAssignmentKind,
    SourceCallKind, SourceCallbackKind, SourceMemberKind, TypeId, TypeKind, TypeLiteral, TypeNode,
    TypeSource, TypedSource,
};

fn failure(message: &str) -> WakeError {
    WakeError::new(
        "WAKE_LINT_ANALYSIS",
        format!("Type facts adapter: {message}"),
    )
}
fn handle(value: &Value) -> Result<u64, WakeError> {
    value
        .as_u64()
        .filter(|id| *id != 0)
        .ok_or_else(|| failure("missing or invalid semantic handle"))
}
fn flags(value: &Value) -> Result<u32, WakeError> {
    value
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| failure("invalid numeric flags"))
}
fn category(value: &Value) -> Result<TypeKind, WakeError> {
    handle(&value["id"])?;
    let flags = flags(&value["flags"])?;
    if flags == 0 || flags >= 1 << 29 {
        return Err(failure("unknown compiler type flags"));
    }
    Ok(if flags & 1 != 0 {
        match value["intrinsicName"].as_str() {
            Some("error") => TypeKind::Error,
            Some(_) => TypeKind::Any,
            None => return Err(failure("any type omitted intrinsic identity")),
        }
    } else if flags & 2 != 0 {
        TypeKind::Unknown
    } else if flags & 262_144 != 0 {
        TypeKind::Never
    } else if flags & 16 != 0 {
        TypeKind::Void
    } else if flags & 1_048_576 != 0 {
        TypeKind::Object
    } else if flags & 524_288 != 0 {
        TypeKind::Parameter
    } else if flags & 134_217_728 != 0 {
        TypeKind::Union
    } else if flags & 268_435_456 != 0 {
        TypeKind::Intersection
    } else if flags & (32 | 1024 | 4_194_304 | 8_388_608) != 0 {
        TypeKind::String
    } else if flags & (64 | 2048) != 0 {
        TypeKind::Number
    } else if flags & (128 | 4096) != 0 {
        TypeKind::BigInt
    } else if flags & (256 | 8192) != 0 {
        TypeKind::Boolean
    } else if flags & (512 | 16384) != 0 {
        TypeKind::Symbol
    } else if flags & 4 != 0 {
        TypeKind::Undefined
    } else if flags & 8 != 0 {
        TypeKind::Null
    } else {
        TypeKind::Other
    })
}

fn literal(value: &Value, kind: TypeKind) -> Result<Option<TypeLiteral>, WakeError> {
    Ok(match kind {
        TypeKind::String => value["value"]
            .as_str()
            .map(|value| TypeLiteral::String(value.into())),
        TypeKind::Number => value["value"].as_f64().map(TypeLiteral::Number),
        TypeKind::Boolean => value["value"].as_bool().map(TypeLiteral::Boolean),
        TypeKind::BigInt => value["value"]
            .as_str()
            .map(normalize_bigint)
            .transpose()
            .map_err(|error| failure(&format!("invalid BigInt literal: {error}")))?
            .map(TypeLiteral::BigInt),
        TypeKind::Null if value["intrinsicName"] == "null" => Some(TypeLiteral::Null),
        TypeKind::Undefined if value["intrinsicName"] == "undefined" => {
            Some(TypeLiteral::Undefined)
        }
        _ => None,
    })
}

fn normalize_bigint(raw: &str) -> Result<String, &'static str> {
    let raw = raw.replace('_', "");
    let (negative, unsigned) = raw
        .strip_prefix('-')
        .map_or((false, raw.as_str()), |value| (true, value));
    let (base, digits) = if let Some(value) = unsigned
        .strip_prefix("0x")
        .or_else(|| unsigned.strip_prefix("0X"))
    {
        (16u32, value)
    } else if let Some(value) = unsigned
        .strip_prefix("0b")
        .or_else(|| unsigned.strip_prefix("0B"))
    {
        (2u32, value)
    } else if let Some(value) = unsigned
        .strip_prefix("0o")
        .or_else(|| unsigned.strip_prefix("0O"))
    {
        (8u32, value)
    } else {
        (10u32, unsigned)
    };
    if digits.is_empty() {
        return Err("missing digits");
    }
    let mut limbs = vec![0u32];
    for digit in digits.chars() {
        let value = digit
            .to_digit(base)
            .ok_or("digit is not valid for its base")?;
        let mut carry = value;
        for limb in &mut limbs {
            let product = u64::from(*limb) * u64::from(base) + u64::from(carry);
            *limb = (product % 1_000_000_000) as u32;
            carry = (product / 1_000_000_000) as u32;
        }
        if carry != 0 {
            limbs.push(carry);
        }
    }
    while limbs.len() > 1 && limbs.last() == Some(&0) {
        limbs.pop();
    }
    let mut result = limbs.pop().unwrap_or(0).to_string();
    for limb in limbs.into_iter().rev() {
        result.push_str(&format!("{limb:09}"));
    }
    if negative && result != "0" {
        result.insert(0, '-');
    }
    Ok(result)
}

fn promise_identity_needed(
    calls: usize,
    expression_statements: usize,
    awaits: usize,
    conditions: usize,
    assignments: usize,
    returns: usize,
    callbacks: usize,
) -> bool {
    calls != 0
        || expression_statements != 0
        || awaits != 0
        || conditions != 0
        || assignments != 0
        || returns != 0
        || callbacks != 0
}

struct Collector<'a> {
    project: &'a mut TypeProject,
    owner: ProjectId,
    ids: BTreeMap<u64, TypeId>,
    enum_identities: BTreeMap<u64, u32>,
    next_enum_identity: u32,
    unique_symbol_identities: BTreeMap<u64, u32>,
    next_unique_symbol_identity: u32,
    class_identities: BTreeMap<u64, u32>,
    next_class_identity: u32,
    property_permissions: BTreeMap<PathBuf, BTreeMap<u32, (u32, bool)>>,
    raw: Vec<Value>,
    nodes: Vec<TypeNode>,
    relations: usize,
}
impl Collector<'_> {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, WakeError> {
        self.project.request(self.owner, method, params)
    }
    fn count(&mut self, amount: usize) -> Result<(), WakeError> {
        self.relations = self
            .relations
            .checked_add(amount)
            .filter(|value| *value <= TypedSource::MAX_RELATIONS)
            .ok_or_else(|| failure("relation/signature budget exceeded"))?;
        Ok(())
    }
    fn literal(&mut self, raw: &Value, kind: TypeKind) -> Result<Option<TypeLiteral>, WakeError> {
        if kind == TypeKind::String
            && let Some(value) = raw["value"].as_str()
            && value.contains('\u{fffd}')
        {
            // Go's JSON encoding replaces invalid UTF-8 (including each byte of a WTF-8
            // surrogate). Never compare those replacement strings as semantic identities.
            // The pinned printer escapes surrogate code units and NoTruncation preserves length.
            let printed = self.request(
                "typeToString",
                json!({"type":handle(&raw["id"])?,"flags":1}),
            )?;
            let printed = printed
                .as_str()
                .ok_or_else(|| failure("missing string type text"))?;
            return TypeLiteral::from_string_source(printed)
                .map(Some)
                .map_err(|_| {
                    failure("cannot recover a lossless string literal from the native type service")
                });
        }
        literal(raw, kind)
    }
    fn insert(&mut self, value: Value) -> Result<TypeId, WakeError> {
        let id = handle(&value["id"])?;
        category(&value)?;
        if let Some(index) = self.ids.get(&id) {
            return Ok(*index);
        }
        if self.raw.len() >= TypedSource::MAX_TYPES {
            return Err(failure("type count budget exceeded"));
        }
        let index = TypeId(self.raw.len());
        self.ids.insert(id, index);
        self.raw.push(value);
        Ok(index)
    }
    fn enum_identity(&mut self, value: &Value) -> Result<Option<u32>, WakeError> {
        // TypeScript's EnumLiteral flag is adapter-owned. The backend symbol/id is converted to
        // a session-local token before the type graph reaches the core; no compiler handle or
        // member spelling becomes part of the public fact model.
        let flags = flags(&value["flags"])?;
        if flags & 32_768 == 0 {
            return Ok(None);
        }
        if !matches!(category(value)?, TypeKind::String | TypeKind::Number) {
            return Ok(None);
        }
        let key = value["symbol"]
            .as_u64()
            .filter(|id| *id != 0)
            .or_else(|| value["id"].as_u64().filter(|id| *id != 0))
            .ok_or_else(|| failure("enum literal omitted identity"))?;
        if let Some(identity) = self.enum_identities.get(&key) {
            return Ok(Some(*identity));
        }
        let identity = self.next_enum_identity;
        self.next_enum_identity = self
            .next_enum_identity
            .checked_add(1)
            .ok_or_else(|| failure("enum identity budget exceeded"))?;
        self.enum_identities.insert(key, identity);
        Ok(Some(identity))
    }
    fn unique_symbol_identity(
        &mut self,
        value: &Value,
        kind: TypeKind,
    ) -> Result<Option<u32>, WakeError> {
        let flags = flags(&value["flags"])?;
        if kind != TypeKind::Symbol || flags & 16_384 == 0 {
            return Ok(None);
        }
        let key = value["symbol"]
            .as_u64()
            .filter(|id| *id != 0)
            .or_else(|| value["id"].as_u64().filter(|id| *id != 0))
            .ok_or_else(|| failure("unique symbol omitted identity"))?;
        if let Some(identity) = self.unique_symbol_identities.get(&key) {
            return Ok(Some(*identity));
        }
        let identity = self.next_unique_symbol_identity;
        self.next_unique_symbol_identity = self
            .next_unique_symbol_identity
            .checked_add(1)
            .ok_or_else(|| failure("unique symbol identity budget exceeded"))?;
        self.unique_symbol_identities.insert(key, identity);
        Ok(Some(identity))
    }
    fn class_identity(&mut self, value: &Value) -> Result<Option<u32>, WakeError> {
        let object_flags = flags(&value["objectFlags"])?;
        let is_this_type = value["isThisType"].as_bool().unwrap_or(false);
        if object_flags & 1 == 0 && !is_this_type {
            return Ok(None);
        }
        let key = value["symbol"]
            .as_u64()
            .filter(|id| *id != 0)
            .or_else(|| value["id"].as_u64().filter(|id| *id != 0))
            .ok_or_else(|| failure("class omitted identity"))?;
        if let Some(identity) = self.class_identities.get(&key) {
            return Ok(Some(*identity));
        }
        let identity = self.next_class_identity;
        self.next_class_identity = self
            .next_class_identity
            .checked_add(1)
            .ok_or_else(|| failure("class identity budget exceeded"))?;
        self.class_identities.insert(key, identity);
        Ok(Some(identity))
    }
    fn related(&mut self, method: &str, params: Value) -> Result<Vec<TypeId>, WakeError> {
        let response = self.request(method, params)?;
        let values = response
            .as_array()
            .ok_or_else(|| failure("missing type relationship list"))?;
        self.count(values.len())?;
        values
            .iter()
            .cloned()
            .map(|value| self.insert(value))
            .collect()
    }

    fn property_readonly(&mut self, property: &Value) -> Result<Option<bool>, WakeError> {
        let symbol_flags = flags(&property["flags"])?;
        let check_flags = flags(&property["checkFlags"])?;
        // Pinned TypeScript 7.0.2 CheckFlags.Readonly and Mapped describe effective permissions,
        // including +/-readonly and key remapping. Their declarations may retain the old modifier
        // or be absent, so do not fall back to source modifiers for those symbols.
        if check_flags & 8 != 0 {
            return Ok(Some(true));
        }
        if check_flags & (1 << 18) != 0 {
            return Ok(Some(false));
        }
        // Plain synthetic properties include fresh object literals and spread copies. A spread
        // deliberately drops readonly while keeping its source declarations for navigation.
        if symbol_flags & (1 << 25) != 0 && symbol_flags & 4 != 0 && check_flags == 0 {
            return Ok(Some(false));
        }
        // Unmodeled instantiation/late-binding relationships remain incomplete.
        if check_flags != 0 || symbol_flags & (1 << 25) != 0 {
            return Ok(None);
        }
        // A getter without a setter is readonly; an accessor with a setter is writable.
        if symbol_flags & ((1 << 15) | (1 << 16)) != 0 {
            return Ok(Some(symbol_flags & (1 << 16) == 0));
        }
        let Some(declarations) = property["declarations"]
            .as_array()
            .filter(|values| !values.is_empty())
        else {
            return Ok(None);
        };
        self.count(declarations.len())?;
        let mut permission = None;
        for declaration in declarations {
            let declaration = declaration
                .as_str()
                .ok_or_else(|| failure("invalid property declaration"))?;
            let mut parts = declaration.splitn(3, '.');
            let index = parts
                .next()
                .and_then(|part| part.parse::<u32>().ok())
                .filter(|index| *index != 0)
                .ok_or_else(|| failure("invalid declaration index"))?;
            let kind = parts
                .next()
                .and_then(|part| part.parse::<u32>().ok())
                .ok_or_else(|| failure("invalid declaration kind"))?;
            let path = parts
                .next()
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .ok_or_else(|| failure("invalid declaration path"))?;
            // JavaScript can acquire permissions through JSDoc; source modifiers alone are not
            // a complete proof there. TypeScript parameter/method declarations also stay opaque.
            if !matches!(kind, 172 | 173 | 303 | 304)
                || !path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| matches!(ext, "ts" | "tsx" | "mts" | "cts"))
            {
                return Ok(None);
            }
            if !self.property_permissions.contains_key(&path) {
                let permissions = self.project.property_permissions(self.owner, &path)?;
                self.count(permissions.len())?;
                self.property_permissions.insert(path.clone(), permissions);
            }
            let Some(&(actual_kind, readonly)) = self.property_permissions[&path].get(&index)
            else {
                return Err(failure("property declaration address missing from source"));
            };
            if actual_kind != kind {
                return Err(failure("property declaration kind differs from source"));
            }
            if permission.is_some_and(|previous| previous != readonly) {
                return Ok(None);
            }
            permission = Some(readonly);
        }
        Ok(permission)
    }
    fn standard_symbol(&mut self, name: &str) -> Result<Option<u64>, WakeError> {
        // A global lookup has no source location, so file-local declarations cannot shadow it.
        let symbol = self.request("resolveName", json!({"name":name,"meaning":788968}))?;
        if symbol.is_null() {
            return Ok(None);
        }
        let id = handle(&symbol["id"])?;
        if symbol["name"] != name {
            return Err(failure("global symbol lookup returned another name"));
        }
        let declarations = symbol["declarations"]
            .as_array()
            .ok_or_else(|| failure("global symbol omitted declarations"))?;
        self.count(declarations.len())?;
        let mut libraries = BTreeMap::<PathBuf, bool>::new();
        for declaration in declarations {
            let declaration = declaration
                .as_str()
                .ok_or_else(|| failure("invalid declaration handle"))?;
            let mut fields = declaration.splitn(3, '.');
            for _ in 0..2 {
                fields
                    .next()
                    .and_then(|field| field.parse::<u32>().ok())
                    .filter(|field| *field != 0)
                    .ok_or_else(|| failure("invalid declaration node address"))?;
            }
            let path = Path::new(
                fields
                    .next()
                    .ok_or_else(|| failure("missing declaration source"))?,
            );
            if !path.is_absolute() || path.to_string_lossy().contains('\0') {
                return Err(failure("invalid declaration source"));
            }
            let library = if let Some(library) = libraries.get(path) {
                *library
            } else {
                let metadata = self.request("getSourceFileMetadata", json!({"file":path}))?;
                let library = metadata["isDefaultLibrary"]
                    .as_bool()
                    .ok_or_else(|| failure("missing library provenance"))?;
                libraries.insert(path.into(), library);
                library
            };
            if library {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }
    fn populate(
        &mut self,
        standard: Option<u64>,
        promise: Option<u64>,
        thenable: Option<u64>,
        regexp: Option<u64>,
    ) -> Result<(), WakeError> {
        while self.nodes.len() < self.raw.len() {
            let raw = self.raw[self.nodes.len()].clone();
            let id = handle(&raw["id"])?;
            let kind = category(&raw)?;
            let mut node = TypeNode {
                kind,
                literal: self.literal(&raw, kind)?,
                enum_identity: self.enum_identity(&raw)?,
                unique_symbol_identity: self.unique_symbol_identity(&raw, kind)?,
                class_identity: if kind == TypeKind::Object {
                    self.class_identity(&raw)?
                } else {
                    None
                },
                ..Default::default()
            };
            match kind {
                TypeKind::Object => {
                    node.standard_function =
                        standard.is_some() && raw["symbol"].as_u64() == standard;
                    node.standard_promise = promise.is_some() && raw["symbol"].as_u64() == promise;
                    node.standard_thenable =
                        thenable.is_some() && raw["symbol"].as_u64() == thenable;
                    node.standard_regexp = regexp.is_some() && raw["symbol"].as_u64() == regexp;
                    let object = flags(&raw["objectFlags"])?;
                    if object & 3 != 0 {
                        node.bases = self.related("getBaseTypes", json!({"type":id}))?;
                    } else if object & 4 != 0 && handle(&raw["target"])? != id {
                        let target = self.request("getTargetOfType", json!({"objectId":id}))?;
                        if target["id"] != raw["target"] {
                            return Err(failure("generic target identity mismatch"));
                        }
                        self.count(1)?;
                        node.reference_target = Some(self.insert(target)?);
                    }
                    if object & 4 != 0 {
                        node.type_arguments =
                            self.related("getTypeArguments", json!({"type":id}))?;
                    }
                    // Structural object members carry value types that are not represented by
                    // generic arguments or base types. Preserve those types so
                    // no-unsafe-assignment/return can recurse into `{ value: any }` and source
                    // interface/class shapes. Standard-library identity objects remain outside
                    // this finite relation to avoid expanding their ambient member surface.
                    if !node.standard_function
                        && !node.standard_promise
                        && !node.standard_thenable
                        && !node.standard_regexp
                        && (object & 4 == 0 || object & 1 != 0)
                    {
                        node.structural_complete = node.class_identity.is_none();
                        let properties = self.request("getPropertiesOfType", json!({"type":id}))?;
                        let properties = properties
                            .as_array()
                            .ok_or_else(|| failure("missing property symbol list"))?;
                        self.count(properties.len())?;
                        for property in properties {
                            let symbol = handle(&property["id"])?;
                            let name = property["name"]
                                .as_str()
                                .ok_or_else(|| failure("property omitted name"))?
                                .to_owned();
                            if name.contains('\u{fffd}') {
                                return Err(failure(
                                    "native property name may contain replaced UTF-16 code units",
                                ));
                            }
                            let property_flags = flags(&property["flags"])?;
                            // SymbolFlags.Optional is 1 << 24 in the pinned TypeScript 7 wire.
                            // Convert that backend bit to a source-bound boolean before the fact
                            // reaches the core; no compiler flags are exposed publicly.
                            let optional = property_flags & (1 << 24) != 0;
                            let property_type =
                                self.request("getTypeOfSymbol", json!({"symbol":symbol}))?;
                            self.count(2)?;
                            let property_type = self.insert(property_type)?;
                            node.properties.push(property_type);
                            // Computed symbol names may contain backend-local symbol handles.
                            // Their values still participate in unsafe proofs, but their opaque
                            // spellings cannot be an exported structural identity.
                            if name.starts_with("__@") {
                                node.structural_complete = false;
                            } else {
                                if node.structural_complete {
                                    match self.property_readonly(property)? {
                                        Some(true) => {
                                            node.readonly_properties.insert(name.clone());
                                        }
                                        Some(false) => {}
                                        None => node.structural_complete = false,
                                    }
                                }
                                node.structural_properties
                                    .push((name, optional, property_type));
                            }
                        }
                        node.structural_properties
                            .sort_by(|left, right| left.0.cmp(&right.0));
                        let index_infos =
                            self.request("getIndexInfosOfType", json!({"type":id}))?;
                        let index_infos = index_infos
                            .as_array()
                            .ok_or_else(|| failure("missing index-info list"))?;
                        self.count(index_infos.len())?;
                        for index_info in index_infos {
                            let key_type = index_info
                                .get("keyType")
                                .cloned()
                                .ok_or_else(|| failure("missing index-info key type"))?;
                            let value_type = index_info
                                .get("valueType")
                                .cloned()
                                .ok_or_else(|| failure("missing index-info value type"))?;
                            let readonly = match index_info.get("isReadonly") {
                                None | Some(Value::Null) => false,
                                Some(Value::Bool(value)) => *value,
                                _ => return Err(failure("invalid index-info readonly flag")),
                            };
                            self.count(3)?;
                            let key_type = self.insert(key_type)?;
                            let value_type = self.insert(value_type)?;
                            node.properties.push(value_type);
                            node.index_signatures.push((key_type, value_type, readonly));
                        }
                        for kind in [0, 1] {
                            let signatures = self
                                .request("getSignaturesOfType", json!({"type":id,"kind":kind}))?;
                            let signatures = signatures
                                .as_array()
                                .ok_or_else(|| failure("missing object signature list"))?;
                            self.count(signatures.len())?;
                            if !signatures.is_empty() {
                                node.structural_complete = false;
                            }
                            for signature in signatures {
                                let signature_id = handle(&signature["id"])?;
                                flags(&signature["flags"])?;
                                let return_type = self.request(
                                    "getReturnTypeOfSignature",
                                    json!({"signature":signature_id}),
                                )?;
                                self.count(1)?;
                                node.signature_returns.push(self.insert(return_type)?);
                            }
                        }
                    }
                }
                TypeKind::Parameter => {
                    let constraint = self.request("getBaseConstraintOfType", json!({"type":id}))?;
                    if !constraint.is_null() {
                        self.count(1)?;
                        node.constraint = Some(self.insert(constraint)?);
                    }
                }
                TypeKind::Union | TypeKind::Intersection => {
                    node.parts = self.related("getTypesOfType", json!({"objectId":id}))?;
                    if node.parts.is_empty() {
                        return Err(failure("empty compound type"));
                    }
                }
                _ => {}
            }
            self.nodes.push(node);
        }
        Ok(())
    }
    fn call_type(&mut self, type_id: TypeId) -> Result<CallType, WakeError> {
        let id = handle(&self.raw[type_id.0]["id"])?;
        let mut call = CallType::new(type_id);
        for kind in [0, 1] {
            let signatures = self.request("getSignaturesOfType", json!({"type":id,"kind":kind}))?;
            let signatures = signatures
                .as_array()
                .ok_or_else(|| failure("missing signature list"))?;
            self.count(signatures.len())?;
            for signature in signatures {
                let id = handle(&signature["id"])?;
                flags(&signature["flags"])?;
                if kind == 0 {
                    let returns =
                        self.request("getReturnTypeOfSignature", json!({"signature":id}))?;
                    call.returns.push(category(&returns)?);
                    call.return_types.push(self.insert(returns)?);
                }
            }
            if kind == 1 {
                call.construct_signatures = u32::try_from(signatures.len())
                    .map_err(|_| failure("signature budget exceeded"))?;
            }
        }
        Ok(call)
    }
}

impl TypeProject {
    pub(super) fn type_source(
        &mut self,
        file: &Path,
        input: TypeSource,
    ) -> Result<TypedSource, WakeError> {
        let owner = self.project_for(file)?;
        let wire = self.source(owner, file, input.source())?;
        let mut locations = Vec::new();
        for call in input.calls() {
            let kind = match call.kind {
                SourceCallKind::Call => 214,
                SourceCallKind::Construct => 215,
                SourceCallKind::TaggedTemplate => 216,
                // TypeScript represents `import()` as a CallExpression whose expression is the
                // import keyword. Query the complete expression because there is no callee head;
                // its type is the Promise return value used by no-floating-promises.
                SourceCallKind::DynamicImport => {
                    locations.push(wire.address(214, call.span).map_err(|error| {
                        failure(&format!("dynamic call range {:?}: {error}", call.span))
                    })?);
                    continue;
                }
            };
            locations.push(
                wire.head_address(kind, call.span, call.head)
                    .map_err(|error| {
                        failure(&format!(
                            "call head range {:?} in call {:?}: {error}",
                            call.head, call.span
                        ))
                    })?,
            );
        }
        let call_count = locations.len();
        let mut call_argument_addresses = Vec::with_capacity(input.calls().len());
        for call in input.calls() {
            let kind = match call.kind {
                SourceCallKind::Call | SourceCallKind::DynamicImport => 214,
                SourceCallKind::Construct => 215,
                SourceCallKind::TaggedTemplate => {
                    call_argument_addresses.push(vec![None; call.arguments.len()]);
                    continue;
                }
            };
            call_argument_addresses.push(
                call.arguments
                    .iter()
                    .map(|argument| {
                        wire.child_address(kind, call.span, *argument)
                            .map(Some)
                            .map_err(|error| {
                                failure(&format!(
                                    "call argument range {:?} in call {:?}: {error}",
                                    argument, call.span
                                ))
                            })
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            );
        }
        let expression_start = locations.len();
        for statement in input.expression_statements() {
            locations
                .push(wire.expression_statement_address(statement.span, statement.expression)?);
        }
        let expression_end = locations.len();
        for site in input.awaits() {
            locations.push(wire.child_address(224, site.span, site.argument)?);
        }
        for assertion in input.assertions() {
            let kind = match assertion.kind {
                SourceAssertionKind::As => 235,
                SourceAssertionKind::Angle => 217,
                SourceAssertionKind::NonNull => 236,
                SourceAssertionKind::Satisfies => 239,
            };
            locations.push(wire.address(kind, assertion.span)?);
            locations.push(wire.child_address(kind, assertion.span, assertion.operand)?);
        }
        let assignment_start = locations.len();
        let mut assignment_addresses = Vec::with_capacity(input.assignments().len());
        for assignment in input.assignments() {
            let kind = match assignment.kind {
                SourceAssignmentKind::Expression => 227,
                SourceAssignmentKind::Variable => 261,
            };
            let address = wire
                .child_address(kind, assignment.span, assignment.value)
                .map_err(|error| {
                    failure(&format!(
                        "assignment kind {kind} span {:?} value {:?}: {error}",
                        assignment.span, assignment.value
                    ))
                })?;
            assignment_addresses.push(address.clone());
            locations.push(address);
        }
        let assignment_end = locations.len();
        let return_start = locations.len();
        let mut return_addresses = Vec::with_capacity(input.returns().len());
        for returned in input.returns() {
            let address = wire.child_address(254, returned.span, returned.argument)?;
            return_addresses.push(address.clone());
            locations.push(address);
        }
        let return_end = locations.len();
        let callback_start = locations.len();
        let mut callback_addresses = Vec::with_capacity(input.callbacks().len());
        let mut callback_contextual_addresses = Vec::with_capacity(input.callbacks().len());
        for callback in input.callbacks() {
            let address = wire
                .descendant_address(callback.span, callback.value)
                .map_err(|error| {
                    failure(&format!(
                        "callback kind {:?} span {:?} value {:?}: {error}",
                        callback.kind, callback.span, callback.value
                    ))
                })?;
            callback_addresses.push(address.clone());
            let contextual_address = match callback.kind {
                SourceCallbackKind::ObjectProperty => address.clone(),
                SourceCallbackKind::JsxAttribute => {
                    wire.descendant_kind_address(callback.span, 295)?
                }
            };
            callback_contextual_addresses.push(contextual_address);
            locations.push(address);
        }
        let callback_end = locations.len();
        let condition_start = locations.len();
        for condition in input.conditions() {
            let kind = match condition.kind {
                wake_lint_core::SourceConditionKind::If => 246,
                wake_lint_core::SourceConditionKind::DoWhile => 247,
                wake_lint_core::SourceConditionKind::While => 248,
                wake_lint_core::SourceConditionKind::For => 249,
                wake_lint_core::SourceConditionKind::Conditional => 228,
                wake_lint_core::SourceConditionKind::Logical => 227,
            };
            locations.push(wire.child_address(kind, condition.span, condition.test)?);
        }
        let condition_end = locations.len();
        let switch_start = locations.len();
        let mut switch_case_counts = Vec::with_capacity(input.switches().len());
        for switch in input.switches() {
            locations.push(wire.child_address(256, switch.span, switch.discriminant)?);
            switch_case_counts.push(switch.case_spans.len());
            for (clause, case) in switch.case_clause_spans.iter().zip(&switch.case_spans) {
                locations.push(wire.child_address(297, *clause, *case)?);
            }
        }
        let switch_end = locations.len();
        let template_start = locations.len();
        for template in input.templates().iter().filter(|template| !template.tagged) {
            for expression in &template.expressions {
                locations.push(wire.template_address(template.span, *expression)?);
            }
        }
        let template_end = locations.len();
        for member in input.members() {
            let kind = if member.kind == SourceMemberKind::Computed {
                213
            } else {
                212
            };
            locations.push(wire.child_address(kind, member.span, member.object)?);
            if member.kind == SourceMemberKind::Computed {
                locations.push(wire.child_address(kind, member.span, member.property)?);
            }
        }
        let mut collector = Collector {
            project: self,
            owner,
            ids: BTreeMap::new(),
            enum_identities: BTreeMap::new(),
            next_enum_identity: 0,
            unique_symbol_identities: BTreeMap::new(),
            next_unique_symbol_identity: 0,
            class_identities: BTreeMap::new(),
            next_class_identity: 0,
            property_permissions: BTreeMap::new(),
            raw: Vec::new(),
            nodes: Vec::new(),
            relations: 0,
        };
        let mut heads = Vec::new();
        for batch in locations.chunks(512) {
            let response = collector.request("getTypeAtLocations", json!({"locations":batch}))?;
            let response = response
                .as_array()
                .filter(|values| values.len() == batch.len())
                .ok_or_else(|| failure("incomplete callee type response"))?;
            for value in response {
                heads.push(collector.insert(value.clone())?);
            }
        }
        let standard = if call_count == 0
            && input.assignments().is_empty()
            && input.returns().is_empty()
            && input.callbacks().is_empty()
        {
            None
        } else {
            collector.standard_symbol("Function")?
        };
        let promise = if promise_identity_needed(
            call_count,
            input.expression_statements().len(),
            input.awaits().len(),
            input.conditions().len(),
            input.assignments().len(),
            input.returns().len(),
            input.callbacks().len(),
        ) {
            collector.standard_symbol("Promise")?
        } else {
            None
        };
        let thenable = promise_identity_needed(
            call_count,
            input.expression_statements().len(),
            input.awaits().len(),
            input.conditions().len(),
            input.assignments().len(),
            input.returns().len(),
            input.callbacks().len(),
        )
        .then(|| collector.standard_symbol("PromiseLike"))
        .transpose()?
        .flatten();
        let regexp = if template_end == template_start {
            None
        } else {
            collector.standard_symbol("RegExp")?
        };
        collector.populate(standard, promise, thenable, regexp)?;
        let mut cached = BTreeMap::<usize, CallType>::new();
        let mut calls = Vec::with_capacity(input.calls().len());
        let mut heads = heads.into_iter();
        for call in input.calls() {
            if call.kind == SourceCallKind::DynamicImport {
                let id = heads.next().expect("every dynamic import was queried");
                let kind = category(&collector.raw[id.0])?;
                calls.push(Some(CallType {
                    type_id: id,
                    returns: vec![kind],
                    return_types: vec![id],
                    construct_signatures: 0,
                }));
                continue;
            }
            let id = heads.next().expect("every original call was queried");
            let fact = if let Some(cached) = cached.get(&id.0) {
                cached.clone()
            } else {
                let fact = collector.call_type(id)?;
                cached.insert(id.0, fact.clone());
                fact
            };
            // Retained per-site signatures are bounded even when their backend query was cached.
            collector.count(
                1 + fact.returns.len()
                    + fact.return_types.len()
                    + fact.construct_signatures as usize,
            )?;
            calls.push(Some(fact));
        }
        collector.populate(standard, promise, thenable, regexp)?;
        let mut argument_queries = Vec::new();
        for (call_index, addresses) in call_argument_addresses.iter().enumerate() {
            for (argument_index, address) in addresses.iter().enumerate() {
                if let Some(address) = address {
                    argument_queries.push((call_index, argument_index, address.clone()));
                }
            }
        }
        let mut argument_actuals = BTreeMap::new();
        let query_addresses = argument_queries
            .iter()
            .map(|(_, _, address)| address.clone())
            .collect::<Vec<_>>();
        for (batch_index, batch) in query_addresses.chunks(512).enumerate() {
            let response = collector.request("getTypeAtLocations", json!({"locations":batch}))?;
            let response = response
                .as_array()
                .filter(|values| values.len() == batch.len())
                .ok_or_else(|| failure("incomplete call argument type response"))?;
            for (offset, value) in response.iter().enumerate() {
                let query_index = batch_index * 512 + offset;
                let (call_index, argument_index, _) = &argument_queries[query_index];
                argument_actuals.insert(
                    (*call_index, *argument_index),
                    collector.insert(value.clone())?,
                );
            }
        }
        let mut argument_contextuals = BTreeMap::new();
        for (call_index, argument_index, address) in &argument_queries {
            let response = collector.request("getContextualType", json!({"location":address}))?;
            if !response.is_null() {
                argument_contextuals
                    .insert((*call_index, *argument_index), collector.insert(response)?);
            }
        }
        collector.populate(standard, promise, thenable, regexp)?;
        let mut call_arguments = Vec::with_capacity(input.calls().len());
        for (call_index, addresses) in call_argument_addresses.iter().enumerate() {
            let mut arguments = Vec::with_capacity(addresses.len());
            for argument_index in 0..addresses.len() {
                let actual = argument_actuals
                    .get(&(call_index, argument_index))
                    .copied()
                    .map(|id| {
                        if let Some(fact) = cached.get(&id.0) {
                            Ok(fact.clone())
                        } else {
                            let fact = collector.call_type(id)?;
                            cached.insert(id.0, fact.clone());
                            Ok(fact)
                        }
                    })
                    .transpose()?;
                let contextual = argument_contextuals
                    .get(&(call_index, argument_index))
                    .copied()
                    .map(|id| {
                        if let Some(fact) = cached.get(&id.0) {
                            Ok(fact.clone())
                        } else {
                            let fact = collector.call_type(id)?;
                            cached.insert(id.0, fact.clone());
                            Ok(fact)
                        }
                    })
                    .transpose()?;
                for fact in [&actual, &contextual].into_iter().flatten() {
                    collector.count(
                        1 + fact.returns.len()
                            + fact.return_types.len()
                            + fact.construct_signatures as usize,
                    )?;
                }
                arguments.push(CallArgumentType { actual, contextual });
            }
            call_arguments.push(arguments);
        }
        collector.populate(standard, promise, thenable, regexp)?;
        let mut expression_statements = Vec::with_capacity(input.expression_statements().len());
        for _ in expression_start..expression_end {
            expression_statements.push(
                heads
                    .next()
                    .expect("every expression statement was queried"),
            );
        }
        let mut awaits = Vec::with_capacity(input.awaits().len());
        for _ in input.awaits() {
            awaits.push(heads.next().expect("every await operand was queried"));
        }
        let mut assertions = Vec::with_capacity(input.assertions().len());
        for _ in input.assertions() {
            let asserted = heads.next().expect("every assertion was queried");
            let operand = heads.next().expect("every assertion operand was queried");
            assertions.push((operand, asserted));
        }
        let mut assignments = Vec::with_capacity(input.assignments().len());
        for _ in assignment_start..assignment_end {
            assignments.push(heads.next().expect("every assignment value was queried"));
        }
        let mut returns = Vec::with_capacity(input.returns().len());
        for _ in return_start..return_end {
            returns.push(heads.next().expect("every return value was queried"));
        }
        let mut callback_actuals = Vec::with_capacity(input.callbacks().len());
        for _ in callback_start..callback_end {
            callback_actuals.push(heads.next().expect("every callback value was queried"));
        }
        let mut conditions = Vec::with_capacity(input.conditions().len());
        for _ in condition_start..condition_end {
            conditions.push(heads.next().expect("every condition was queried"));
        }
        let mut switches = Vec::with_capacity(input.switches().len());
        let mut switch_cases = Vec::with_capacity(input.switches().len());
        let mut switch_location = switch_start;
        for case_count in switch_case_counts {
            switches.push(heads.next().expect("every switch discriminant was queried"));
            switch_location += 1;
            let mut cases = Vec::with_capacity(case_count);
            for _ in 0..case_count {
                cases.push(heads.next().expect("every switch case was queried"));
                switch_location += 1;
            }
            switch_cases.push(cases);
        }
        debug_assert_eq!(switch_location, switch_end);
        let mut templates = Vec::with_capacity(input.templates().len());
        for template in input.templates() {
            if template.tagged {
                templates.push(None);
                continue;
            }
            collector.count(template.expressions.len())?;
            templates.push(Some(
                template
                    .expressions
                    .iter()
                    .map(|_| {
                        heads
                            .next()
                            .expect("every original interpolation was queried")
                    })
                    .collect(),
            ));
        }
        let mut members = Vec::with_capacity(input.members().len());
        for member in input.members() {
            let object = heads.next().expect("every original receiver was queried");
            let property = (member.kind == SourceMemberKind::Computed)
                .then(|| heads.next().expect("every computed member key was queried"));
            collector.count(1 + usize::from(property.is_some()))?;
            members.push(MemberType { object, property });
        }
        let mut assignment_contextuals = BTreeMap::new();
        for (index, address) in assignment_addresses.iter().enumerate() {
            let response = collector.request("getContextualType", json!({"location":address}))?;
            if !response.is_null() {
                assignment_contextuals.insert(index, collector.insert(response)?);
            }
        }
        collector.populate(standard, promise, thenable, regexp)?;
        let mut assignment_call_types = Vec::with_capacity(assignments.len());
        for (index, id) in assignments.iter().copied().enumerate() {
            let actual = if let Some(fact) = cached.get(&id.0) {
                fact.clone()
            } else {
                let fact = collector.call_type(id)?;
                cached.insert(id.0, fact.clone());
                fact
            };
            collector.count(
                1 + actual.returns.len()
                    + actual.return_types.len()
                    + actual.construct_signatures as usize,
            )?;
            let contextual = assignment_contextuals
                .get(&index)
                .copied()
                .map(|id| {
                    if let Some(fact) = cached.get(&id.0) {
                        Ok(fact.clone())
                    } else {
                        let fact = collector.call_type(id)?;
                        cached.insert(id.0, fact.clone());
                        Ok(fact)
                    }
                })
                .transpose()?;
            if let Some(fact) = contextual.as_ref() {
                collector.count(
                    1 + fact.returns.len()
                        + fact.return_types.len()
                        + fact.construct_signatures as usize,
                )?;
            }
            assignment_call_types.push(CallArgumentType {
                actual: Some(actual),
                contextual,
            });
        }
        let mut return_contextuals = BTreeMap::new();
        for (index, address) in return_addresses.iter().enumerate() {
            let response = collector.request("getContextualType", json!({"location":address}))?;
            if !response.is_null() {
                return_contextuals.insert(index, collector.insert(response)?);
            }
        }
        collector.populate(standard, promise, thenable, regexp)?;
        let mut return_call_types = Vec::with_capacity(returns.len());
        for (index, id) in returns.iter().copied().enumerate() {
            let actual = if let Some(fact) = cached.get(&id.0) {
                fact.clone()
            } else {
                let fact = collector.call_type(id)?;
                cached.insert(id.0, fact.clone());
                fact
            };
            collector.count(
                1 + actual.returns.len()
                    + actual.return_types.len()
                    + actual.construct_signatures as usize,
            )?;
            let contextual = return_contextuals
                .get(&index)
                .copied()
                .map(|id| {
                    if let Some(fact) = cached.get(&id.0) {
                        Ok(fact.clone())
                    } else {
                        let fact = collector.call_type(id)?;
                        cached.insert(id.0, fact.clone());
                        Ok(fact)
                    }
                })
                .transpose()?;
            if let Some(fact) = contextual.as_ref() {
                collector.count(
                    1 + fact.returns.len()
                        + fact.return_types.len()
                        + fact.construct_signatures as usize,
                )?;
            }
            return_call_types.push(CallArgumentType {
                actual: Some(actual),
                contextual,
            });
        }
        let mut callback_contextuals = BTreeMap::new();
        for (index, address) in callback_contextual_addresses.iter().enumerate() {
            let response = collector.request("getContextualType", json!({"location":address}))?;
            if !response.is_null() {
                callback_contextuals.insert(index, collector.insert(response)?);
            }
        }
        collector.populate(standard, promise, thenable, regexp)?;
        let mut callback_call_types = Vec::with_capacity(callback_actuals.len());
        for (index, id) in callback_actuals.iter().copied().enumerate() {
            let actual = if let Some(fact) = cached.get(&id.0) {
                fact.clone()
            } else {
                let fact = collector.call_type(id)?;
                cached.insert(id.0, fact.clone());
                fact
            };
            collector.count(
                1 + actual.returns.len()
                    + actual.return_types.len()
                    + actual.construct_signatures as usize,
            )?;
            let contextual = callback_contextuals
                .get(&index)
                .copied()
                .map(|id| {
                    if let Some(fact) = cached.get(&id.0) {
                        Ok(fact.clone())
                    } else {
                        let fact = collector.call_type(id)?;
                        cached.insert(id.0, fact.clone());
                        Ok(fact)
                    }
                })
                .transpose()?;
            if let Some(fact) = contextual.as_ref() {
                collector.count(
                    1 + fact.returns.len()
                        + fact.return_types.len()
                        + fact.construct_signatures as usize,
                )?;
            }
            callback_call_types.push(CallArgumentType {
                actual: Some(actual),
                contextual,
            });
        }
        collector.populate(standard, promise, thenable, regexp)?;
        TypedSource::new(input, collector.nodes, calls)
            .and_then(|typed| typed.with_call_argument_types(call_arguments))
            .and_then(|typed| typed.with_assignment_call_types(assignment_call_types))
            .and_then(|typed| typed.with_return_call_types(return_call_types))
            .and_then(|typed| typed.with_callback_types(callback_call_types))
            .and_then(|typed| typed.with_expression_statement_types(expression_statements))
            .and_then(|typed| typed.with_await_types(awaits))
            .and_then(|typed| typed.with_assertion_types(assertions))
            .and_then(|typed| typed.with_assignment_types(assignments))
            .and_then(|typed| typed.with_return_types(returns))
            .and_then(|typed| typed.with_condition_types(conditions))
            .and_then(|typed| typed.with_switch_types(switches))
            .and_then(|typed| typed.with_switch_case_types(switch_cases))
            .and_then(|typed| typed.with_template_types(templates))
            .and_then(|typed| typed.with_member_types(members))
            .map_err(|error| failure(&error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::{normalize_bigint, promise_identity_needed};

    #[test]
    fn bigint_normalization_rejects_empty_and_invalid_digits() {
        assert_eq!(normalize_bigint("0x10").unwrap(), "16");
        assert_eq!(normalize_bigint("-0b1_01").unwrap(), "-5");
        assert!(normalize_bigint("").is_err());
        assert!(normalize_bigint("0xzz").is_err());
        assert!(normalize_bigint("0b102").is_err());
    }

    #[test]
    fn promise_identity_is_requested_for_float_only_calls() {
        assert!(promise_identity_needed(1, 0, 0, 0, 0, 0, 0));
        assert!(promise_identity_needed(0, 1, 0, 0, 0, 0, 0));
        assert!(promise_identity_needed(0, 0, 1, 0, 0, 0, 0));
        assert!(promise_identity_needed(0, 0, 0, 1, 0, 0, 0));
        assert!(promise_identity_needed(0, 0, 0, 0, 1, 0, 0));
        assert!(promise_identity_needed(0, 0, 0, 0, 0, 1, 0));
        assert!(promise_identity_needed(0, 0, 0, 0, 0, 0, 1));
        assert!(!promise_identity_needed(0, 0, 0, 0, 0, 0, 0));
    }
}
