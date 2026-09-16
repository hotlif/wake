use wake_common::Interner;
use wake_ecma_parser::{ParseOptions, SourceType, parse, parse_source};

#[test]
fn original_terminators_are_grammar_owned_and_leave_compilation_unchanged() {
    let source = "import type {T} from 'm'\ntype Alias=T;\nconst 名=1 // keep\nfunction f(){return 名}\nclass C {field:T}\ndo{}while(false) next();\nfor(;;);";
    let interner = Interner::new();
    let parsed = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    let facts = &parsed.terminators;
    assert_eq!(
        facts.len(),
        7,
        "for separators and empty statements are excluded"
    );
    assert_eq!(facts.iter().filter(|f| f.explicit).count(), 2);
    for fact in facts {
        assert!(source.is_char_boundary(fact.span.lo as usize));
        assert_eq!(
            &source[fact.span.lo as usize..fact.span.hi as usize],
            if fact.explicit { ";" } else { "" }
        );
    }
    assert!(
        facts
            .iter()
            .any(|f| !f.explicit && &source[..f.span.lo as usize] == "import type {T} from 'm'")
    );
    let ordinary = parse(source, &interner, SourceType::TypeScript);
    assert_eq!(
        ordinary.module.structure_hash(),
        parsed.parsed.module.structure_hash()
    );
    assert_eq!(
        format!("{:?}", ordinary.dependencies),
        format!("{:?}", parsed.parsed.dependencies)
    );
}

#[test]
fn committed_terminators_are_not_duplicated_by_arrow_speculation() {
    let source = "const f = (x = function(){ return 1 }) => x;";
    let interner = Interner::new();
    let parsed = parse_source(
        source,
        &interner,
        SourceType::TypeScript,
        ParseOptions::default(),
    );
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    assert_eq!(parsed.terminators.len(), 2);
    assert!(!parsed.terminators[0].explicit);
    assert!(parsed.terminators[1].explicit);
}

#[test]
fn omission_facts_preserve_asi_hazards_and_do_while_special_grammar() {
    let source = "a; b;\n(c)();\nd;\n/regex/.test(s);\ndo{}while(false); next();";
    let interner = Interner::new();
    let parsed = parse_source(
        source,
        &interner,
        SourceType::Module,
        ParseOptions::default(),
    );
    assert!(
        !parsed.parsed.has_errors(),
        "{:?}",
        parsed.parsed.diagnostics
    );
    assert_eq!(
        parsed
            .terminators
            .iter()
            .map(|f| f.can_omit)
            .collect::<Vec<_>>(),
        [false, false, true, false, true, true, true]
    );
}
