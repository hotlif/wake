use super::*;

fn fixture(source: &str, config: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("package.json"), "{}").unwrap();
    fs::write(directory.path().join("math.test.ts"), source).unwrap();
    fs::write(directory.path().join("wake.config.toml"), config).unwrap();
    directory
}

fn options(root: &Path) -> TestOptions {
    TestOptions {
        root: Some(root.to_path_buf()),
        serial: true,
        ..TestOptions::default()
    }
}

#[test]
fn retries_rerun_case_hooks_and_report_actual_attempts() {
    let directory = fixture(
        r#"
        import {test, expect, beforeAll, beforeEach, afterEach, afterAll} from '@crab-dev/wake/test'
        let attempts = 0, before = 0, after = 0, all = 0
        beforeAll(() => { all++ })
        beforeEach(() => { before++ })
        afterEach(() => { after++ })
        test('eventually succeeds', () => { expect(++attempts).toBe(3) })
        afterAll(() => { expect([before, after, all]).toEqual([3, 3, 1]) })
    "#,
        "[test]\nretries = 2\n",
    );
    let result = run_tests(options(directory.path())).unwrap();
    assert!(result.success, "{result:#?}");
    assert_eq!(result.suites[0].tests[0].attempts, 3);
}

#[test]
fn expected_failures_do_not_invert_hooks_timeouts_or_framework_errors() {
    let directory = fixture(
        r#"
        import {test, expect, describe, beforeEach} from '@crab-dev/wake/test'
        test.failing('throws', () => { expect(1).toBe(2) })
        test.failing.each([1, 2])('row %s', n => { throw new Error(String(n)) })
        test.failing('unexpected pass', () => {})
        test.failing('timeout', () => new Promise(() => {}), {timeout: 30})
        test.failing('framework', () => { throw Object.assign(new Error('host failed'), {code: 'WAKE_TEST_HOST'}) })
        test.skip.failing('ignored', () => {})
        describe('hook', () => {
            beforeEach(() => { throw new Error('setup broke') })
            test.failing('still fails', () => { throw new Error('never reached') })
        })
    "#,
        "",
    );
    let result = run_tests(options(directory.path())).unwrap();
    assert_eq!(result.counts.tests.passed, 3, "{result:#?}");
    assert_eq!(result.counts.tests.failed, 4, "{result:#?}");
}

#[test]
fn retry_budget_does_not_hide_hook_or_snapshot_failures() {
    let directory = fixture(
        r#"
        import {test, expect, describe, beforeEach} from '@crab-dev/wake/test'
        describe('hook', () => {
            beforeEach(() => { throw new Error('hook') })
            test('fails once', () => {})
        })
        test.failing('snapshot', () => expect('missing').toMatchSnapshot())
        test('override zero', () => { throw new Error('no retry') }, {retries: 0})
    "#,
        "[test]\nretries = 2\n",
    );
    let mut request = options(directory.path());
    request.update_snapshots = Some("none".into());
    let result = run_tests(request).unwrap();
    assert_eq!(result.counts.tests.failed, 3, "{result:#?}");
    assert!(result.suites[0].tests.iter().all(|case| case.attempts == 1));
}

#[test]
fn array_matchers_and_error_snapshots_use_the_external_format() {
    let directory = fixture(
        r#"
        import {test, expect} from '@crab-dev/wake/test'
        test('arrays', () => {
            expect([1, 2]).toEqual(expect.arrayOf(expect.any(Number)))
            expect([]).toEqual(expect.arrayOf(expect.any(Number)))
            expect([1, 'two']).toEqual(expect.not.arrayOf(expect.any(Number)))
            expect(() => { throw new Error('Wake failure') }).toThrowErrorMatchingSnapshot('error')
        })
        test('async error snapshot', async () => {
            await expect(Promise.reject(new Error('async failure'))).rejects.toThrowErrorMatchingSnapshot('async')
        })
    "#,
        "",
    );
    let result = run_tests(options(directory.path())).unwrap();
    assert!(result.success, "{result:#?}");
    let snapshot =
        fs::read_to_string(directory.path().join("__snapshots__/math.test.ts.snap")).unwrap();
    assert!(snapshot.contains("Wake failure"), "{snapshot}");
}

#[test]
fn global_lifecycle_runs_once_and_teardown_shares_state_after_case_failure() {
    let directory = fixture(
        r#"
        import {test} from '@crab-dev/wake/test'
        test('external resource', () => {
            throw new Error('case failure still tears down')
        })
    "#,
        "[test]\nglobal_setup = ['setup.ts']\nglobal_teardown = ['teardown.ts']\n",
    );
    let marker =
        serde_json::to_string(&normalize_path(&directory.path().join("lifecycle.txt"))).unwrap();
    fs::write(directory.path().join("setup.ts"), format!("import {{writeFileSync}} from 'node:fs'; export default async () => {{ globalThis.owner = 'ready'; writeFileSync({marker}, 'setup') }}")).unwrap();
    fs::write(directory.path().join("teardown.ts"), format!("import {{writeFileSync}} from 'node:fs'; export default () => {{ if (globalThis.owner !== 'ready') throw new Error('realm lost'); writeFileSync({marker}, 'closed') }}")).unwrap();
    let result = run_tests(options(directory.path())).unwrap();
    assert!(!result.success);
    assert_eq!(
        fs::read_to_string(directory.path().join("lifecycle.txt")).unwrap(),
        "closed"
    );
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "WAKE_TEST_LIFECYCLE"),
        "{result:#?}"
    );
}

#[test]
fn global_setup_failure_still_runs_every_teardown_in_reverse_order() {
    let directory = fixture(
        "import {test} from '@crab-dev/wake/test'; test('must not run', () => {})",
        "[test]\nglobal_setup = ['setup.ts']\nglobal_teardown = ['last.ts', 'first.ts']\n",
    );
    let marker =
        serde_json::to_string(&normalize_path(&directory.path().join("closed.txt"))).unwrap();
    fs::write(
        directory.path().join("setup.ts"),
        "export default () => { throw new Error('setup failed') }",
    )
    .unwrap();
    fs::write(
        directory.path().join("first.ts"),
        "export default () => { throw new Error('first teardown failed') }",
    )
    .unwrap();
    fs::write(directory.path().join("last.ts"), format!("import {{writeFileSync}} from 'node:fs'; export default () => writeFileSync({marker}, 'closed')")).unwrap();
    let result = run_tests(options(directory.path())).unwrap();
    assert!(!result.success, "{result:#?}");
    assert_eq!(result.counts.tests.total, 0);
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|item| item.message == "No tests found"),
        "setup failure must not claim discovery found no suites: {result:#?}"
    );
    assert_eq!(
        fs::read_to_string(directory.path().join("closed.txt")).unwrap(),
        "closed"
    );
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|item| item.code == "WAKE_TEST_LIFECYCLE")
            .count(),
        2
    );
}

#[test]
fn coverage_collect_counts_unimported_source_without_running_it() {
    let directory = fixture(
        "import {test, expect} from '@crab-dev/wake/test'; test('loaded', () => expect(1).toBe(1))",
        "[test]\nexclude = ['<rootDir>/src/excluded.ts']\n[test.coverage]\nenabled = true\ncollect = ['src/**/*.ts', '!src/ignored.ts']\nreporters = ['json', 'lcov', 'html']\n[test.coverage.threshold]\nlines = 100\n",
    );
    fs::create_dir(directory.path().join("src")).unwrap();
    fs::write(
        directory.path().join("src/excluded.ts"),
        "excluded invalid syntax!",
    )
    .unwrap();
    fs::write(directory.path().join("src/untouched.ts"), "throw new Error('must never execute');\nexport function untouched(value: number) { return value > 0 ? 1 : 0 }\n").unwrap();
    fs::write(
        directory.path().join("src/ignored.ts"),
        "this is not valid JS!",
    )
    .unwrap();
    let result = run_tests(options(directory.path())).unwrap();
    assert!(!result.success, "{result:#?}");
    assert_eq!(result.counts.tests.passed, 1);
    let coverage = result.coverage.unwrap();
    assert_eq!(coverage.files.len(), 1, "{coverage:#?}");
    assert!(coverage.summary.lines.total > 0);
    assert!(coverage.summary.functions.total > 0);
    assert_eq!(coverage.summary.lines.covered, 0);
    assert_eq!(coverage.summary.functions.covered, 0);
}

#[test]
fn coverage_collect_cannot_pass_thresholds_by_allowing_an_empty_test_set() {
    let directory = fixture(
        "",
        "[test]\ninclude = ['missing/**/*.test.ts']\n[test.coverage]\nenabled = true\ncollect = ['src/*.ts']\n[test.coverage.threshold]\nlines = 100\n",
    );
    fs::create_dir(directory.path().join("src")).unwrap();
    fs::write(
        directory.path().join("src/untouched.ts"),
        "throw new Error('must not run'); export const unused = 1",
    )
    .unwrap();
    let mut request = options(directory.path());
    request.allow_no_tests = true;
    let result = run_tests(request).unwrap();
    assert!(
        !result.success,
        "empty test discovery must not bypass requested coverage thresholds: {result:#?}"
    );
    assert_eq!(result.counts.tests.total, 0);
    assert_eq!(result.coverage.as_ref().unwrap().files.len(), 1);
    let mut selected = options(directory.path());
    selected.allow_no_tests = true;
    selected.related = vec![directory.path().join("src/untouched.ts")];
    let skipped = run_tests(selected).unwrap();
    assert!(
        skipped.success,
        "an empty explicit selection skips coverage: {skipped:#?}"
    );
    assert!(skipped.coverage.is_none());
}

#[test]
fn project_setup_timeout_and_coverage_override_root_configuration() {
    let directory = fixture(
        "",
        "[test]\nsetup = ['missing.ts']\ntimeout_ms = 1\n[test.coverage]\nenabled = false\n[[test.projects]]\nname = 'client'\nroot = 'client'\nsetup = ['setup.ts']\ntimeout_ms = 1000\ninclude = ['**/*.test.ts']\n[test.projects.coverage]\nenabled = true\ncollect = ['src/*.ts']\nreporters = ['json']\n[test.projects.coverage.threshold]\nlines = 100\n",
    );
    fs::create_dir_all(directory.path().join("client/src")).unwrap();
    fs::write(
        directory.path().join("client/setup.ts"),
        "globalThis.prepared = true",
    )
    .unwrap();
    fs::write(directory.path().join("client/app.test.ts"), "import {test, expect} from '@crab-dev/wake/test'; test('configured', async () => { await new Promise(resolve => setTimeout(resolve, 15)); expect(globalThis.prepared).toBe(true) })").unwrap();
    fs::write(
        directory.path().join("client/src/missing.ts"),
        "export const unused = 42",
    )
    .unwrap();
    let result = run_tests(options(directory.path())).unwrap();
    assert_eq!(result.counts.tests.passed, 1, "{result:#?}");
    assert!(!result.success, "project threshold must fail");
    assert!(result.coverage.is_some());
    assert!(
        result
            .artifacts
            .iter()
            .any(|artifact| artifact.kind == "coverage-json"
                && artifact.path.contains("/client/coverage/")
                && Path::new(&artifact.path).is_file())
    );
}

#[test]
fn lifecycle_watch_reruns_have_fresh_realms_and_observe_external_module_inputs() {
    let directory = fixture(
        "import {test} from '@crab-dev/wake/test'; test('runs', () => {})",
        "[test]\nglobal_setup = ['setup.ts']\nglobal_teardown = ['teardown.ts']\n",
    );
    let dependency = tempfile::tempdir().unwrap();
    let source_path = dependency.path().join("value.ts");
    fs::write(&source_path, "export const value = 1").unwrap();
    let source = serde_json::to_string(&normalize_path(&source_path)).unwrap();
    let marker =
        serde_json::to_string(&normalize_path(&directory.path().join("marker.txt"))).unwrap();
    fs::write(directory.path().join("setup.ts"), format!("import {{value}} from {source}; import {{writeFileSync}} from 'node:fs'; export default () => {{ if(globalThis.seen) throw new Error('stale realm'); globalThis.seen = true; writeFileSync({marker}, String(value)); }}")).unwrap();
    fs::write(
        directory.path().join("teardown.ts"),
        "export default () => { if(!globalThis.seen) throw new Error('setup realm lost') }",
    )
    .unwrap();
    let request = options(directory.path());
    let mut session = TestWorkspaceSession::new(request.clone());
    let first = session
        .run(request.clone(), TestCancellationToken::default())
        .unwrap();
    assert!(first.success, "{first:#?}");
    assert!(
        session
            .watch_paths()
            .iter()
            .any(|input| input.path == source_path.canonicalize().unwrap())
    );
    fs::write(&source_path, "export const value = 2").unwrap();
    let second = session
        .run_watch(
            request,
            TestWatchRequest {
                invalidated_paths: vec![source_path],
                selection: TestWatchSelection::Affected,
                rediscover: false,
            },
            TestCancellationToken::default(),
        )
        .unwrap();
    assert!(second.success, "{second:#?}");
    assert_eq!(
        fs::read_to_string(directory.path().join("marker.txt")).unwrap(),
        "2"
    );
}

#[test]
fn never_settling_lifecycle_times_out_and_teardown_still_runs() {
    let directory = fixture(
        "import {test} from '@crab-dev/wake/test'; test('not reached', () => {})",
        "[test]\ntimeout_ms = 100\nglobal_setup = ['setup.ts']\nglobal_teardown = ['teardown.ts']\n",
    );
    fs::write(
        directory.path().join("setup.ts"),
        "export default () => new Promise(() => {})",
    )
    .unwrap();
    let marker = serde_json::to_string(&normalize_path(&directory.path().join("closed"))).unwrap();
    fs::write(directory.path().join("teardown.ts"), format!("import {{writeFileSync}} from 'node:fs'; export default () => writeFileSync({marker}, 'yes')")).unwrap();
    let result = run_tests(options(directory.path())).unwrap();
    assert!(!result.success);
    assert_eq!(result.counts.tests.total, 0);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|item| item.code == "WAKE_TEST_LIFECYCLE"
                && (item.message.contains("timeout") || item.message.contains("deadline"))),
        "{result:#?}"
    );
    assert!(directory.path().join("closed").is_file());
}

#[test]
fn overlapping_projects_keep_independent_coverage_artifacts_and_thresholds() {
    let directory = fixture(
        "import {test, expect} from '@crab-dev/wake/test'; import {value} from './src/value'; test('value', () => expect(value).toBe(1))",
        "[[test.projects]]\nname = 'alpha'\nroot = '.'\n[test.projects.coverage]\nenabled = true\nreporters = ['json']\n[[test.projects]]\nname = 'beta'\nroot = '.'\n[test.projects.coverage]\nenabled = true\nreporters = ['json']\ncollect = ['src/unused.ts']\n[test.projects.coverage.threshold]\nlines = 100\n",
    );
    fs::create_dir(directory.path().join("src")).unwrap();
    fs::write(
        directory.path().join("src/value.ts"),
        "export const value = 1",
    )
    .unwrap();
    fs::write(
        directory.path().join("src/unused.ts"),
        "export const unused = 2",
    )
    .unwrap();
    let result = run_tests(options(directory.path())).unwrap();
    assert_eq!(result.counts.tests.passed, 2, "{result:#?}");
    assert!(!result.success, "beta threshold must fail");
    let paths = result
        .artifacts
        .iter()
        .filter(|artifact| artifact.kind == "coverage-json")
        .map(|artifact| &artifact.path)
        .collect::<BTreeSet<_>>();
    assert_eq!(paths.len(), 2, "{result:#?}");
    assert_eq!(
        result.coverage.unwrap().files.len(),
        2,
        "aggregate deduplicates shared sources"
    );
}

#[test]
fn retry_attempts_reset_snapshots_and_do_not_mask_a_later_timeout() {
    let directory = fixture(
        r#"
        import {test, expect} from '@crab-dev/wake/test'
        let attempts = 0, timeoutAttempts = 0
        test('snapshot', () => {
            expect({attempt: ++attempts}).toMatchSnapshot()
            expect(attempts).toBe(2)
        }, {retries: 1})
        test('later timeout', () => {
            if (++timeoutAttempts === 1) throw new Error('ordinary failure')
            for (;;) {}
        }, {retries: 2, timeout: 50})
    "#,
        "",
    );
    let result = run_tests(options(directory.path())).unwrap();
    assert_eq!(result.suites[0].tests[0].attempts, 2, "{result:#?}");
    assert_eq!(result.suites[0].tests[0].status, TestStatus::Passed);
    assert_eq!(result.suites[0].tests[1].attempts, 2);
    assert_eq!(
        result.suites[0].tests[1].failures[0].code.as_deref(),
        Some("WAKE_TEST_TIMEOUT")
    );
    assert_eq!(result.snapshot.added, 1);
}

#[test]
fn cancellation_still_runs_global_teardown_with_a_fresh_deadline() {
    let directory = fixture("", "[test]\nglobal_teardown = ['teardown.ts']\n");
    let started = directory.path().join("started");
    let marker = serde_json::to_string(&normalize_path(&started)).unwrap();
    fs::write(directory.path().join("math.test.ts"), format!("import {{test}} from '@crab-dev/wake/test'; import {{writeFileSync}} from 'node:fs'; test('cancel', () => {{ writeFileSync({marker}, 'started'); return new Promise(() => {{}}) }})")).unwrap();
    let closed = serde_json::to_string(&normalize_path(&directory.path().join("closed"))).unwrap();
    fs::write(directory.path().join("teardown.ts"), format!("import {{writeFileSync}} from 'node:fs'; export default () => writeFileSync({closed}, 'closed')")).unwrap();
    let cancellation = TestCancellationToken::default();
    let token = cancellation.clone();
    let waiter = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !started.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        token.cancel();
    });
    let result = run_tests_with_cancellation(options(directory.path()), cancellation).unwrap();
    waiter.join().unwrap();
    assert_eq!(
        result.termination_reason,
        TestTerminationReason::Cancelled,
        "{result:#?}"
    );
    assert!(directory.path().join("closed").is_file(), "{result:#?}");
}

#[test]
fn lifecycle_setup_can_keep_owned_background_timers_until_teardown() {
    let directory = fixture(
        "import {test} from '@crab-dev/wake/test'; test('runs', () => {})",
        "[test]\ntimeout_ms = 1000\nglobal_setup = ['setup.ts']\nglobal_teardown = ['teardown.ts']\n",
    );
    fs::write(
        directory.path().join("setup.ts"),
        "export default () => { globalThis.background = setInterval(() => {}, 1) }",
    )
    .unwrap();
    fs::write(
        directory.path().join("teardown.ts"),
        "export default () => clearInterval(globalThis.background)",
    )
    .unwrap();
    let result = run_tests(options(directory.path())).unwrap();
    assert!(result.success, "{result:#?}");
    assert_eq!(result.counts.tests.passed, 1);
}

#[test]
#[ignore = "requires an installed system Chromium-family browser"]
fn chromium_uses_the_same_retry_and_expected_failure_scheduler() {
    let directory = fixture(
        r#"
        import {test, expect, beforeEach, afterEach} from '@crab-dev/wake/test'
        import {createHash} from 'node:crypto'
        let attempt = 0, before = 0, after = 0
        beforeEach(() => { before++ })
        afterEach(() => { after++ })
        test('retries', () => { expect(++attempt).toBe(2) }, {retries: 1})
        test.failing('expected', () => { throw new Error('expected failure') })
        test('shared matchers and boundaries', () => {
            expect([before, after]).toEqual([4, 3])
            expect([1, 2]).toEqual(expect.arrayOf(expect.any(Number)))
            expect(() => createHash('sha256')).toThrow()
        })
    "#,
        "",
    );
    let mut request = options(directory.path());
    request.environment = Some("browser".into());
    request.browser_path = std::env::var_os("WAKE_SYSTEM_BROWSER_PATH").map(PathBuf::from);
    let result = run_tests(request).unwrap();
    assert!(result.success, "{result:#?}");
    assert_eq!(result.suites[0].tests[0].attempts, 2);
}
