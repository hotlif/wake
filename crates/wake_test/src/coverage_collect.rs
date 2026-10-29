//! Explicit zero-hit source collection never executes unimported modules.
use super::*;

pub(super) fn append_missing(
    root: &Path,
    config: &wake_config::Test,
    files: &mut Vec<NormalizedCoverageFile>,
) -> Result<(), TestError> {
    if config.coverage.collect.is_empty() {
        return Ok(());
    }
    let mut include = Vec::new();
    let mut exclude = Vec::new();
    for pattern in &config.coverage.collect {
        let (negative, pattern) = pattern
            .strip_prefix('!')
            .map_or((false, pattern.as_str()), |pattern| (true, pattern));
        if pattern.is_empty() {
            return Err(TestError::new(
                "WAKE_TEST_CONFIG",
                "coverage.collect patterns must not be empty",
            ));
        }
        let regex = Regex::new(&wake_glob_regex(pattern)?)
            .map_err(|error| TestError::new("WAKE_TEST_CONFIG", error.to_string()))?;
        if negative {
            exclude.push(regex);
        } else {
            include.push(regex);
        }
    }
    let matcher = TestFileMatcher::compile(root, config)?;
    let loaded = files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    let setup = config
        .setup
        .iter()
        .chain(&config.global_setup)
        .chain(&config.global_teardown)
        .map(|path| {
            normalize_watch_input(
                root,
                &root.join(path.replace("<rootDir>", &normalize_path(root))),
            )
        })
        .collect::<BTreeSet<_>>();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|error| {
            TestError::new("WAKE_TEST_COVERAGE", error.to_string()).at(&directory)
        })?;
        for entry in entries {
            let entry =
                entry.map_err(|error| TestError::new("WAKE_TEST_COVERAGE", error.to_string()))?;
            let path = entry.path();
            if matcher.is_excluded(&path, root) {
                continue;
            }
            let kind = entry.file_type().map_err(|error| {
                TestError::new("WAKE_TEST_COVERAGE", error.to_string()).at(&path)
            })?;
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if !matches!(
                    entry.file_name().to_str(),
                    Some("node_modules" | ".git" | ".tmp" | "target" | "coverage")
                ) {
                    pending.push(path);
                }
                continue;
            }
            if !kind.is_file() || !is_supported_test_module(&path) {
                continue;
            }
            let relative = normalize_path(path.strip_prefix(root).unwrap_or(&path));
            if loaded.contains(&relative)
                || setup.contains(&normalize_watch_input(root, &path))
                || !include.iter().any(|regex| regex.is_match(&relative))
                || exclude.iter().any(|regex| regex.is_match(&relative))
                || matcher.is_match(&path, root)
            {
                continue;
            }
            let source = fs::read_to_string(&path).map_err(|error| {
                TestError::new("WAKE_TEST_COVERAGE", error.to_string()).at(&path)
            })?;
            let (offsets, syntax) = wake_js_runtime::coverage_source_metadata(&path, &source)
                .map_err(|error| {
                    TestError::new("WAKE_TEST_COVERAGE", error.to_string()).at(&path)
                })?;
            let mut lines = BTreeMap::new();
            for offset in offsets {
                if let Some(prefix) = source.get(..offset as usize) {
                    lines.insert(
                        1 + prefix.bytes().filter(|byte| *byte == b'\n').count() as u32,
                        false,
                    );
                }
            }
            let ranges = |spans: Vec<wake_common::Span>| {
                spans
                    .into_iter()
                    .filter(|span| !span.is_empty())
                    .map(|span| {
                        (
                            OriginalCoverageRange {
                                start_byte: span.lo,
                                end_byte: span.hi,
                            },
                            false,
                        )
                    })
                    .collect()
            };
            files.push(NormalizedCoverageFile {
                path: relative,
                source,
                lines,
                functions: ranges(syntax.functions),
                blocks: ranges(syntax.blocks),
                range_anchor_hits: BTreeMap::new(),
            });
        }
    }
    Ok(())
}
