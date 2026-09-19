use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::{Duration, Instant};
use wake_bundler::{BuildOptions, BuildRequest, BuildSession};
use wake_common::{FileSystem, MemoryFileSystem};

#[test]
fn progress_parallel_child() {
    if std::env::var_os("WAKE_PROGRESS_PARALLEL_CHILD").is_none() {
        return;
    }
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let workers = ["a", "b"].map(|name| {
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            let fs = Arc::new(BlockedRead {
                inner: MemoryFileSystem::from_files([
                    (format!("{name}/index.js"), "import './slow.js';"),
                    (format!("{name}/slow.js"), "console.log('slow');"),
                ]),
                blocked: AtomicBool::new(false),
            });
            let mut session = BuildSession::new(fs, BuildOptions::default());
            barrier.wait();
            let first = session.build(BuildRequest::new(format!("{name}/index.js")));
            assert!(!first.has_errors());
            let second = session.build(BuildRequest::new(format!("{name}/index.js")));
            assert_eq!(first.bundle, second.bundle);
        })
    });
    for worker in workers {
        worker.join().unwrap();
    }
    wake_turbo::executor::global_executor().parallel(vec![|| {
        let _progress = wake_common::progress::task("read", || "unscoped.js".into());
    }]);
}

#[test]
fn progress_isolates_overlapping_builds_on_shared_workers() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "progress_parallel_child", "--nocapture"])
        .env("WAKE_PROGRESS", "1")
        .env("WAKE_PROGRESS_PARALLEL_CHILD", "1")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in io::BufReader::new(stderr).lines() {
            let _ = tx.send(line.unwrap());
        }
    });
    let mut ids = [None, None];
    let mut log = String::new();
    let deadline = Instant::now() + Duration::from_secs(15);
    while ids.iter().any(Option::is_none) {
        let Ok(line) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) else {
            break;
        };
        let normalized = line.replace('\\', "/");
        for (index, name) in ["a/slow.js", "b/slow.js"].iter().enumerate() {
            if line.contains("active") && line.contains("read") && normalized.contains(name) {
                ids[index] = Some(line.split(']').next().unwrap().to_owned());
            }
        }
        log.push_str(&line);
        log.push('\n');
    }
    let _ = child.stdin.take().unwrap().write_all(b"xx");
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    reader.join().unwrap();
    log.extend(rx.try_iter().map(|line| format!("{line}\n")));
    assert!(status.is_some_and(|status| status.success()), "{log}");
    assert!(ids.iter().all(Option::is_some), "{log}");
    assert_ne!(
        ids[0], ids[1],
        "overlapping builds shared one diagnostic owner: {log}"
    );
    let normalized = log.replace('\\', "/");
    for (index, name) in ["a", "b"].iter().enumerate() {
        let id = ids[index].as_ref().unwrap();
        let other = if *name == "a" { "b/" } else { "a/" };
        let owned = normalized
            .lines()
            .filter(|row| row.starts_with(&format!("{id}]")))
            .collect::<Vec<_>>();
        assert!(owned.iter().all(|row| !row.contains(other)), "{log}");
        assert!(
            owned
                .iter()
                .any(|row| row.contains("slowest") && row.contains(&format!("{name}/slow.js"))),
            "{log}"
        );
        assert!(
            owned
                .iter()
                .any(|row| row.contains("active #") && row.contains("parent=Some(")),
            "{log}"
        );
        let summaries = normalized
            .lines()
            .filter(|row| {
                row.contains("summary rebuild") && row.contains(&format!("target={name}/index.js"))
            })
            .collect::<Vec<_>>();
        assert_eq!(summaries.len(), 2, "{log}");
        assert_ne!(
            summaries[0].split(']').next(),
            summaries[1].split(']').next(),
            "{log}"
        );
        assert_eq!(
            owned.iter().filter(|row| row.contains("summary ")).count(),
            1,
            "{log}"
        );
    }
    let orphan = log
        .lines()
        .find(|row| row.contains("summary read target=unscoped.js"))
        .expect(&log);
    assert!(
        ids.iter()
            .flatten()
            .all(|id| !orphan.starts_with(&format!("{id}]"))),
        "{log}"
    );
}

struct BlockedRead {
    inner: MemoryFileSystem,
    blocked: AtomicBool,
}

#[test]
fn progress_cache_child() {
    if std::env::var_os("WAKE_PROGRESS_CACHE_CHILD").is_none() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    for (path, text) in [
        (
            "src/plain.js",
            "export function answer(value) { return value + 2; }",
        ),
        (
            "src/index.js",
            "import './plain.css'; import { css } from '@crab-dev/css'; const box = css`color: red;`; console.log(box, 1+2);",
        ),
        ("src/plain.css", "body { margin: 0px; }"),
        (
            "node_modules/@crab-dev/css/package.json",
            r#"{"name":"@crab-dev/css","main":"index.js"}"#,
        ),
        (
            "node_modules/@crab-dev/css/index.js",
            "export const css = () => {};",
        ),
    ] {
        let path = root.path().join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    fn run(root: &Path, cache: &Path, css: bool) -> Vec<String> {
        let fs = Arc::new(wake_common::OsFileSystem);
        let options = BuildOptions {
            minify: true,
            css_in_js: css,
            extract_css: true,
            source_map: true,
            persistent_cache: Some(cache.join(format!("cache-{css}.bin"))),
            project_root: Some(root.to_owned()),
            ..BuildOptions::default()
        };
        let entry = root.join(if css { "src/index.js" } else { "src/plain.js" });
        let mut session = BuildSession::new(fs.clone(), options.clone());
        let cold = session.build(BuildRequest::new(&entry));
        let warm = session.build(BuildRequest::new(&entry));
        drop(session);
        let disk = BuildSession::new(fs, options).build(BuildRequest::new(&entry));
        assert!(warm.cached_module_count > 0);
        if !css {
            assert!(disk.cached_module_count > 0);
        }
        assert!(cache.join(format!("cache-{css}.bin")).is_file());
        [cold, warm, disk]
            .into_iter()
            .map(|output| {
                assert!(!output.has_errors(), "{:?}", output.diagnostics);
                let chunks = output
                    .chunks
                    .iter()
                    .map(|chunk| {
                        (
                            &chunk.file_name,
                            &chunk.code,
                            &chunk.source_map,
                            &chunk.styles,
                            &chunk.imports,
                            &chunk.dynamic_imports,
                            &chunk.module_ids,
                        )
                    })
                    .collect::<Vec<_>>();
                let assets = output
                    .assets
                    .iter()
                    .map(|asset| {
                        (
                            &asset.file_name,
                            &asset.bytes,
                            asset.is_css,
                            &asset.owner_module_ids,
                            &asset.unscoped_css_owner_module_ids,
                        )
                    })
                    .collect::<Vec<_>>();
                format!(
                    "{} {} {} {:?} {:?} {}",
                    output.module_count,
                    output.updated_module_count,
                    output.cached_module_count,
                    chunks,
                    assets,
                    output.bundle
                )
            })
            .collect()
    }
    let plain = tempfile::tempdir().unwrap();
    let observed = tempfile::tempdir().unwrap();
    let baseline = [false, true].map(|css| run(root.path(), plain.path(), css));
    wake_common::progress::enable();
    assert_eq!(
        baseline,
        [false, true].map(|css| run(root.path(), observed.path(), css))
    );
}

#[test]
fn progress_preserves_cold_memory_and_disk_cache_results() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "progress_cache_child", "--nocapture"])
        .env_remove("WAKE_PROGRESS")
        .env("WAKE_PROGRESS_CACHE_CHILD", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    for step in [
        "css-scopes",
        "css-module-scope",
        "css-transform",
        "css-in-js-transform",
        "css-artifacts",
        "sourcemap-extract",
        "sourcemap-facts",
        "sourcemap-merge",
        "sourcemap-serialize",
    ] {
        assert!(
            stderr.contains(&format!("{step} count=")),
            "missing {step}: {stderr}"
        );
    }
}

impl FileSystem for BlockedRead {
    fn canonicalize(&self, p: &Path) -> io::Result<PathBuf> {
        self.inner.canonicalize(p)
    }
    fn read_to_string(&self, p: &Path) -> io::Result<String> {
        if p.ends_with("slow.js") && !self.blocked.swap(true, Ordering::Relaxed) {
            // Only the parent can release this read: a completed-build timing cannot pass.
            io::stdin().read_exact(&mut [0])?;
        }
        self.inner.read_to_string(p)
    }
    fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        self.inner.read(p)
    }
    fn exists(&self, p: &Path) -> bool {
        self.inner.exists(p)
    }
    fn is_file(&self, p: &Path) -> bool {
        self.inner.is_file(p)
    }
    fn is_dir(&self, p: &Path) -> bool {
        self.inner.is_dir(p)
    }
    fn read_dir(&self, p: &Path) -> io::Result<Vec<PathBuf>> {
        self.inner.read_dir(p)
    }
}

#[test]
fn progress_child() {
    if std::env::var_os("WAKE_PROGRESS_CHILD").is_none() {
        return;
    }
    let fs = Arc::new(BlockedRead {
        inner: MemoryFileSystem::from_files([
            (
                "src/index.js",
                b"import './slow.js'; console.log('entry');".as_slice(),
            ),
            ("src/slow.js", b"console.log('slow');".as_slice()),
        ]),
        blocked: AtomicBool::new(false),
    });
    let mut session = BuildSession::new(fs, BuildOptions::default());
    let first = session.build(BuildRequest::new("src/index.js"));
    assert!(!first.has_errors());
    let second = session.build(BuildRequest::new("src/index.js"));
    assert!(!second.has_errors());
    assert_eq!(first.bundle, second.bundle);
}

#[test]
fn progress_reports_blocked_read_before_it_returns() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "progress_child", "--nocapture"])
        .env("WAKE_PROGRESS", "1")
        .env("WAKE_PROGRESS_CHILD", "1")
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in io::BufReader::new(stderr).lines() {
            if tx.send(line.unwrap()).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut log = String::new();
    let observed = loop {
        let Ok(line) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) else {
            break false;
        };
        log.push_str(&line);
        log.push('\n');
        if line.contains("active") && line.contains("read") && line.contains("slow.js") {
            break true;
        }
    };
    if !observed {
        let _ = child.kill();
        let _ = child.wait();
        reader.join().unwrap();
        panic!("blocked read was not observable before returning: {log}");
    }
    child.stdin.take().unwrap().write_all(b"x").unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("build did not return after releasing read: {log}");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    reader.join().unwrap();
    log.extend(rx.try_iter().map(|line| format!("{line}\n")));
    assert!(status.success(), "{log}");
    assert!(log.contains("slowest") && log.contains("slow.js"), "{log}");
}
