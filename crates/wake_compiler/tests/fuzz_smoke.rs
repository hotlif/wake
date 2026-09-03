use std::panic::{AssertUnwindSafe, catch_unwind};

use wake_compiler::{
    AutomaticJsxOptions, Language, ModuleOutput, SourceMapMode, SourceText, TranspileOptions,
    transpile_module,
};

#[test]
fn valid_utf8_fuzz_smoke_never_panics() {
    const ALPHABET: &[char] = &[
        'a', 'Z', '0', ' ', '\t', '\r', '\n', '(', ')', '{', '}', '[', ']', '<', '>', '/', '*',
        '\'', '"', '`', '$', ':', ';', ',', '.', '?', '=', '+', '-', '\0', 'é', '中', '😀', '𐐷',
        '\u{2028}', '\u{2029}',
    ];
    let mut state = 0x6a09_e667_f3bc_c909_u64;

    for case in 0..256_u32 {
        let mut source = String::new();
        for _ in 0..(case as usize % 97) {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            source.push(ALPHABET[state as usize % ALPHABET.len()]);
        }
        let mut options = TranspileOptions::new(if case % 2 == 0 {
            Language::JavaScript
        } else {
            Language::TypeScript
        });
        if case % 3 == 0 {
            options = options.with_jsx(AutomaticJsxOptions::production());
        }
        if case % 5 == 0 {
            options = options.with_module_output(ModuleOutput::CommonJs);
        }
        if case % 7 == 0 {
            options = options.with_source_map(SourceMapMode::Detached);
        }

        let result = catch_unwind(AssertUnwindSafe(|| {
            transpile_module(SourceText::new("fuzz-input.tsx", &source), &options)
        }));
        assert!(
            result.is_ok(),
            "valid UTF-8 case {case} panicked; bytes={:?}",
            source.as_bytes()
        );
    }
}
