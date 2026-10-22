#![cfg(any(windows, target_os = "linux"))]

use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use wake_app::{CancellationToken, LintProjectOptions, lint_project};

fn package_file(root: &tempfile::TempDir, name: &str) -> PathBuf {
    root.path()
        .join("node_modules/@demo/wake-lint-plugin")
        .join(name)
}

fn extension_project() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    let package = root.path().join("node_modules/@demo/wake-lint-plugin");
    fs::create_dir_all(&package).unwrap();
    fs::write(
        root.path().join("package.json"),
        r#"{"name":"extension-project","private":true,"dependencies":{"@demo/wake-lint-plugin":"1.0.0"}}"#,
    )
    .unwrap();
    fs::write(
        root.path().join("wake.config.toml"),
        "[lint]\nrecommended=false\nextensions=['@demo/wake-lint-plugin']\n[lint.rules]\n'demo/no-debugger'='error'\n",
    )
    .unwrap();
    fs::write(root.path().join("a.js"), "debugger;").unwrap();
    fs::write(
        package.join("package.json"),
        r#"{"name":"@demo/wake-lint-plugin","version":"1.0.0","type":"module","main":"./index.mjs","wakeLint":{"manifest":"./wake-lint.manifest.json","module":"./index.mjs"}}"#,
    )
    .unwrap();
    fs::write(
        package.join("wake-lint.manifest.json"),
        r#"{"schema":"wake.lint.package.v1","name":"@demo/wake-lint-plugin","version":"1.0.0","protocol":"wake.lint.extension.v1","sdkMajor":1,"rules":[{"id":"demo/no-debugger","meta":{"description":"find debugger","category":"problem","fixable":false},"messageIds":["found"],"optionsSchema":{"type":"object","additionalProperties":false,"properties":{}}}]}"#,
    )
    .unwrap();
    fs::write(
        package.join("index.mjs"),
        r#"import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
export default {
  protocol: 'wake.lint.extension.v1', sdkMajor: 1,
  name: '@demo/wake-lint-plugin', version: '1.0.0',
  rules: [{
    protocol: 'wake.lint.extension.v1', id: 'demo/no-debugger',
    meta: { description: 'find debugger', category: 'problem', fixable: false },
    create() {
      const packagePath = fileURLToPath(new URL('.', import.meta.url))
      const child = spawn(process.execPath, ['-e', `
        const fs = require('node:fs')
        const path = require('node:path')
        const root = process.argv[1]
        fs.writeFileSync(path.join(root, 'started'), 'yes')
        const timer = setInterval(() => {
          if (fs.existsSync(path.join(root, 'stop'))) {
            clearInterval(timer)
            process.exit(0)
          }
        }, 25)
        setTimeout(() => process.exit(0), 7000)
      `, packagePath], { stdio: ['ignore', 1, 'ignore'] })
      child.unref()
      while (true) {}
    },
  }],
}
"#,
    )
    .unwrap();
    root
}

#[cfg(windows)]
fn thread_count() -> usize {
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &format!("(Get-Process -Id {}).Threads.Count", std::process::id()),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[cfg(target_os = "linux")]
fn thread_count() -> usize {
    fs::read_dir("/proc/self/task").unwrap().count()
}

#[test]
fn cancelling_workers_with_inherited_stdout_does_not_leave_reader_threads() {
    let before = thread_count();
    let mut roots = Vec::new();
    for _ in 0..5 {
        let root = extension_project();
        let cancellation = CancellationToken::default();
        let running = cancellation.clone();
        let options = LintProjectOptions {
            root: root.path().into(),
            allow_extensions: true,
            paths: vec!["a.js".into()],
            ..Default::default()
        };
        let handle = thread::spawn(move || lint_project(options, &running));
        let started = Instant::now();
        while !package_file(&root, "started").exists() && started.elapsed() < Duration::from_secs(5)
        {
            thread::sleep(Duration::from_millis(10));
        }
        cancellation.cancel();
        assert!(
            package_file(&root, "started").exists(),
            "descendant did not start"
        );
        let error = handle.join().unwrap().unwrap_err();
        assert_eq!(error.code, "WAKE_CANCELLED");
        roots.push(root);
    }
    thread::sleep(Duration::from_millis(100));
    let after = thread_count();
    for root in &roots {
        fs::write(package_file(root, "stop"), "yes").unwrap();
    }
    assert!(
        after <= before + 2,
        "cancelled workers left {} blocked stdout reader threads",
        after.saturating_sub(before)
    );
}

#[test]
fn extension_cannot_read_its_stdout_descriptor() {
    let root = extension_project();
    fs::write(
        package_file(&root, "index.mjs"),
        r#"import { readSync, writeFileSync } from 'node:fs'
export default {
  protocol: 'wake.lint.extension.v1', sdkMajor: 1,
  name: '@demo/wake-lint-plugin', version: '1.0.0',
  rules: [{
    protocol: 'wake.lint.extension.v1', id: 'demo/no-debugger',
    meta: { description: 'find debugger', category: 'problem', fixable: false },
    create() {
      let readable = true
      try { readSync(1, Buffer.alloc(1), 0, 1, 0) } catch { readable = false }
      writeFileSync(new URL('./stdout-readable', import.meta.url), String(readable))
    },
  }],
}
"#,
    )
    .unwrap();
    let result = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            allow_extensions: true,
            paths: vec!["a.js".into()],
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap();
    assert_eq!(result.error_count, 0);
    assert_eq!(
        fs::read_to_string(package_file(&root, "stdout-readable")).unwrap(),
        "false"
    );
}

#[test]
fn inherited_stdout_cannot_exceed_worker_output_budget() {
    let root = extension_project();
    fs::write(
        package_file(&root, "index.mjs"),
        r#"import { spawnSync } from 'node:child_process'
export default {
  protocol: 'wake.lint.extension.v1', sdkMajor: 1,
  name: '@demo/wake-lint-plugin', version: '1.0.0',
  rules: [{
    protocol: 'wake.lint.extension.v1', id: 'demo/no-debugger',
    meta: { description: 'find debugger', category: 'problem', fixable: false },
    create() {
      spawnSync(process.execPath, ['-e', "require('node:fs').writeSync(1, Buffer.alloc(2 * 1024 * 1024 + 1, 65))"],
        { stdio: ['ignore', 1, 'ignore'] })
    },
  }],
}
"#,
    )
    .unwrap();
    let error = lint_project(
        LintProjectOptions {
            root: root.path().into(),
            allow_extensions: true,
            paths: vec!["a.js".into()],
            ..Default::default()
        },
        &CancellationToken::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "WAKE_LINT_EXTENSION");
    assert!(error.message.contains("output budget"), "{error}");
}

#[cfg(windows)]
#[test]
fn captured_stdout_file_is_unlinked_while_descendant_holds_its_writer() {
    use std::fs::OpenOptions;
    use std::process::{Command, Stdio};

    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("descendant-started");
    let output = tempfile::NamedTempFile::new().unwrap();
    let output_path = output.path().to_owned();
    let writer = OpenOptions::new().append(true).open(&output_path).unwrap();
    let mut descendant = Command::new("node")
        .arg("-e")
        .arg("require('node:fs').writeFileSync(process.argv[1], 'yes'); setTimeout(() => {}, 5000)")
        .arg(&marker)
        .stdin(Stdio::null())
        .stdout(Stdio::from(writer))
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let started = Instant::now();
    while !marker.exists() && started.elapsed() < Duration::from_secs(5) {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(marker.exists());
    drop(output);
    let unlinked = !output_path.exists();
    descendant.kill().unwrap();
    descendant.wait().unwrap();
    if !unlinked {
        fs::remove_file(&output_path).unwrap();
    }
    assert!(
        unlinked,
        "captured stdout file remained visible after parent drop"
    );
}
