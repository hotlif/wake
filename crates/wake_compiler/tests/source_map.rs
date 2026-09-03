use wake_compiler::{
    AutomaticJsxOptions, Language, ModuleOutput, SourceMapMode, SourceText, TranspileOptions,
    transpile_module,
};

#[test]
fn detached_map_uses_utf16_columns_for_crlf_and_non_bmp_source() {
    let source = "const emoji = \"😀\", value = emoji;\r\nexport { value };\r\n";
    let output = transpile_module(
        SourceText::new("unicode.js", source),
        &TranspileOptions::new(Language::JavaScript).with_source_map(SourceMapMode::Detached),
    )
    .expect("Unicode source should transpile");
    let map: serde_json::Value =
        serde_json::from_str(output.source_map().expect("detached map").json())
            .expect("valid source-map JSON");

    assert_eq!(map["sourcesContent"][0], source);
    let mappings = decode_original_positions(map["mappings"].as_str().expect("mappings string"));
    let first_value_byte = source.find("value").unwrap();
    let first_value_utf16 = source[..first_value_byte].encode_utf16().count() as i64;
    assert!(
        mappings.contains(&(0, first_value_utf16)),
        "first-line mapping must count the emoji as two UTF-16 units: {mappings:?}"
    );
    let exported_value_byte = source.rfind("value").unwrap();
    let second_line_start = source.find("\r\n").unwrap() + 2;
    let exported_value_utf16 = source[second_line_start..exported_value_byte]
        .encode_utf16()
        .count() as i64;
    assert!(
        mappings.contains(&(1, exported_value_utf16)),
        "CRLF must advance exactly one source line: {mappings:?}"
    );
}

#[test]
fn typescript_erasure_and_synthetic_jsx_runtime_keep_precise_anchors() {
    let source = concat!(
        "type Label = string;\n",
        "export const View = (label: Label) => <button title={label}>{label}</button>;\n",
    );
    let output = transpile_module(
        SourceText::new("src/view.tsx", source),
        &TranspileOptions::new(Language::TypeScript)
            .with_jsx(AutomaticJsxOptions::production())
            .with_source_map(SourceMapMode::Detached),
    )
    .expect("TSX should transpile with a detached map");
    let map: serde_json::Value =
        serde_json::from_str(output.source_map().expect("detached TSX map").json())
            .expect("valid source-map JSON");
    let mappings = decode_mappings(map["mappings"].as_str().expect("mappings string"));

    assert!(!output.code().contains("type Label"), "{}", output.code());
    assert!(!output.code().contains(": Label"), "{}", output.code());
    assert_exact_unmapped(
        output.code(),
        &mappings,
        "\"react/jsx-runtime\"",
        "the parser-injected runtime request has no source token",
    );
    assert_exact_mapping(
        output.code(),
        source,
        &mappings,
        "_jsx(\"button\"",
        "<button",
        "the lowered JSX call is derived from the opening `<`",
    );
    assert_exact_mapping(
        output.code(),
        source,
        &mappings,
        "\"button\"",
        "button title",
        "the generated intrinsic string is anchored to the JSX tag name",
    );
    assert_exact_mapping(
        output.code(),
        source,
        &mappings,
        "label) =>",
        "label: Label",
        "the retained parameter skips its erased TypeScript annotation",
    );
    assert_exact_mapping(
        output.code(),
        source,
        &mappings,
        "title",
        "title",
        "the retained JSX attribute keeps its exact source token",
    );
}

#[test]
fn commonjs_synthetic_require_and_getter_are_unmapped_without_changing_bytes() {
    let source = concat!(
        "import { value } from \"./dep.js\";\n",
        "export const answer = value + 1;\n",
    );
    let base =
        TranspileOptions::new(Language::JavaScript).with_module_output(ModuleOutput::CommonJs);
    let plain = transpile_module(SourceText::new("src/answer.js", source), &base)
        .expect("plain CommonJS transpile");
    let mapped = transpile_module(
        SourceText::new("src/answer.js", source),
        &base.with_source_map(SourceMapMode::Detached),
    )
    .expect("mapped CommonJS transpile");
    let map: serde_json::Value =
        serde_json::from_str(mapped.source_map().expect("detached CommonJS map").json())
            .expect("valid source-map JSON");
    let mappings = decode_mappings(map["mappings"].as_str().expect("mappings string"));

    assert_eq!(plain.code(), mapped.code());
    assert!(plain.source_map().is_none());
    assert_exact_mapping(
        mapped.code(),
        source,
        &mappings,
        "const __wake_namespace_0",
        "import",
        "the derived namespace declaration is anchored to its import declaration",
    );
    assert_exact_unmapped(
        mapped.code(),
        &mappings,
        "require",
        "the synthetic CommonJS loader must not impersonate the source import token",
    );
    assert_exact_unmapped(
        mapped.code(),
        &mappings,
        "\"./dep.js\"",
        "the synthetic require argument must not impersonate the source specifier",
    );
    assert_exact_unmapped(
        mapped.code(),
        &mappings,
        "Object.defineProperty(exports, \"answer\"",
        "the synthetic live-export getter has no source token of its own",
    );
    assert_exact_unmapped(
        mapped.code(),
        &mappings,
        "function",
        "the synthetic getter closure remains generated-only",
    );
    assert_exact_mapping(
        mapped.code(),
        source,
        &mappings,
        "const answer",
        "const answer",
        "the retained source declaration remains precisely mapped",
    );
    assert_exact_mapping(
        mapped.code(),
        source,
        &mappings,
        "__wake_namespace_0[\"value\"]",
        "value + 1",
        "the lowered imported read remains anchored to the retained source reference",
    );
    assert_exact_mapping(
        mapped.code(),
        source,
        &mappings,
        "1",
        "1",
        "the retained initializer literal remains precisely mapped",
    );
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DecodedMapping {
    generated_line: i64,
    generated_column: i64,
    original: Option<(i64, i64)>,
}

fn assert_exact_mapping(
    generated: &str,
    source: &str,
    mappings: &[DecodedMapping],
    generated_token: &str,
    source_token: &str,
    context: &str,
) {
    let generated_position = utf16_position(generated, unique_offset(generated, generated_token));
    let source_position = utf16_position(source, unique_offset(source, source_token));
    let mapping = exact_mapping(mappings, generated_position, generated_token, context);
    assert_eq!(
        mapping.original,
        Some(source_position),
        "{context}: generated token `{generated_token}` at {generated_position:?} must map to source token `{source_token}` at {source_position:?}; mapping={mapping:?}"
    );
}

fn assert_exact_unmapped(
    generated: &str,
    mappings: &[DecodedMapping],
    generated_token: &str,
    context: &str,
) {
    let generated_position = utf16_position(generated, unique_offset(generated, generated_token));
    let mapping = mappings
        .iter()
        .filter(|mapping| {
            mapping.generated_line == generated_position.0
                && mapping.generated_column <= generated_position.1
        })
        .max_by_key(|mapping| mapping.generated_column)
        .unwrap_or_else(|| {
            panic!(
                "{context}: no mapping state covers generated token `{generated_token}` at {generated_position:?}; mappings={mappings:?}"
            )
        });
    assert_eq!(
        mapping.original, None,
        "{context}: generated token `{generated_token}` at {generated_position:?} must be covered by an unmapped segment; mapping={mapping:?}"
    );
}

fn exact_mapping<'a>(
    mappings: &'a [DecodedMapping],
    generated_position: (i64, i64),
    generated_token: &str,
    context: &str,
) -> &'a DecodedMapping {
    mappings
        .iter()
        .find(|mapping| {
            (mapping.generated_line, mapping.generated_column) == generated_position
        })
        .unwrap_or_else(|| {
            panic!(
                "{context}: no exact mapping segment starts at generated token `{generated_token}` at {generated_position:?}; mappings={mappings:?}"
            )
        })
}

fn unique_offset(text: &str, needle: &str) -> usize {
    let mut matches = text.match_indices(needle);
    let (offset, _) = matches
        .next()
        .unwrap_or_else(|| panic!("missing `{needle}` in:\n{text}"));
    assert!(
        matches.next().is_none(),
        "expected unique `{needle}` in:\n{text}"
    );
    offset
}

fn utf16_position(text: &str, byte_offset: usize) -> (i64, i64) {
    let prefix = &text[..byte_offset];
    let line = prefix.bytes().filter(|byte| *byte == b'\n').count() as i64;
    let line_start = prefix.rfind('\n').map_or(0, |newline| newline + 1);
    let column = text[line_start..byte_offset].encode_utf16().count() as i64;
    (line, column)
}

fn decode_original_positions(encoded: &str) -> Vec<(i64, i64)> {
    decode_mappings(encoded)
        .into_iter()
        .filter_map(|mapping| mapping.original)
        .collect()
}

fn decode_mappings(encoded: &str) -> Vec<DecodedMapping> {
    let mut source_index = 0_i64;
    let mut original_line = 0_i64;
    let mut original_column = 0_i64;
    let mut name_index = 0_i64;
    let mut mappings = Vec::new();

    for (generated_line, line) in encoded.split(';').enumerate() {
        let mut generated_column = 0_i64;
        for segment in line.split(',').filter(|segment| !segment.is_empty()) {
            let fields = decode_segment(segment);
            generated_column += fields[0];
            if fields.len() < 4 {
                assert_eq!(fields.len(), 1, "invalid unmapped segment `{segment}`");
                mappings.push(DecodedMapping {
                    generated_line: generated_line as i64,
                    generated_column,
                    original: None,
                });
                continue;
            }
            source_index += fields[1];
            original_line += fields[2];
            original_column += fields[3];
            if let Some(delta) = fields.get(4) {
                name_index += delta;
                assert!(name_index >= 0, "negative source-map name index");
            }
            assert_eq!(
                source_index, 0,
                "single-source map must keep source index zero"
            );
            mappings.push(DecodedMapping {
                generated_line: generated_line as i64,
                generated_column,
                original: Some((original_line, original_column)),
            });
        }
    }
    mappings
}

fn decode_segment(segment: &str) -> Vec<i64> {
    let bytes = segment.as_bytes();
    let mut cursor = 0;
    let mut values = Vec::new();
    while cursor < bytes.len() {
        let mut value = 0_u64;
        let mut shift = 0;
        loop {
            let digit = base64_value(bytes[cursor]);
            cursor += 1;
            value |= u64::from(digit & 31) << shift;
            if digit & 32 == 0 {
                break;
            }
            shift += 5;
        }
        let negative = value & 1 == 1;
        let magnitude = (value >> 1) as i64;
        values.push(if negative { -magnitude } else { magnitude });
    }
    values
}

fn base64_value(byte: u8) -> u8 {
    match byte {
        b'A'..=b'Z' => byte - b'A',
        b'a'..=b'z' => byte - b'a' + 26,
        b'0'..=b'9' => byte - b'0' + 52,
        b'+' => 62,
        b'/' => 63,
        _ => panic!("invalid base64 VLQ byte {byte}"),
    }
}
