//! Git-backed changed-file discovery for Wake Test.
//!
//! This module deliberately owns the Git process boundary. Callers receive normalized absolute
//! paths and never need to interpret Git output or repository state themselves.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

/// Stable categories that the test runner maps to `WAKE_TEST_DISCOVERY` diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GitChangedErrorKind {
    InvalidRoot,
    GitUnavailable,
    NotRepository,
    CommandFailed,
    InvalidOutput,
    PathOutsideRoot,
}

/// A failure at the Git changed-file discovery boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GitChangedError {
    kind: GitChangedErrorKind,
    message: String,
}

impl GitChangedError {
    pub(crate) const fn kind(&self) -> GitChangedErrorKind {
        self.kind
    }

    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    fn new(kind: GitChangedErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for GitChangedError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for GitChangedError {}

/// Returns staged, unstaged, and non-ignored untracked paths below `root`.
///
/// A repository with no `HEAD` compares its index with an empty history. Rename detection is
/// disabled so both the deleted and added names are returned. The result is deduplicated and
/// sorted by `PathBuf`'s platform-native stable ordering.
pub(crate) fn changed_paths(root: &Path) -> Result<Vec<PathBuf>, GitChangedError> {
    let root = canonical_root(root)?;
    ensure_work_tree(&root)?;

    let has_head = repository_has_head(&root)?;
    let tracked = if has_head {
        checked_git(
            &root,
            &[
                "diff",
                "--relative",
                "--no-renames",
                "--name-only",
                "-z",
                "HEAD",
                "--",
                ".",
            ],
            "read staged and unstaged paths relative to HEAD",
        )?
    } else {
        checked_git(
            &root,
            &["ls-files", "--cached", "-z", "--", "."],
            "read the index of an unborn repository",
        )?
    };
    let untracked = checked_git(
        &root,
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "-z",
            "--",
            ".",
        ],
        "read non-ignored untracked paths",
    )?;

    let mut paths = parse_nul_paths(&root, &tracked, "tracked changed paths")?;
    paths.extend(parse_nul_paths(
        &root,
        &untracked,
        "untracked changed paths",
    )?);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn canonical_root(root: &Path) -> Result<PathBuf, GitChangedError> {
    let canonical = root.canonicalize().map_err(|error| {
        GitChangedError::new(
            GitChangedErrorKind::InvalidRoot,
            format!(
                "cannot resolve Wake Test changed-file root `{}`: {error}",
                root.display()
            ),
        )
    })?;
    let metadata = fs::metadata(&canonical).map_err(|error| {
        GitChangedError::new(
            GitChangedErrorKind::InvalidRoot,
            format!(
                "cannot inspect Wake Test changed-file root `{}`: {error}",
                canonical.display()
            ),
        )
    })?;
    if !metadata.is_dir() {
        return Err(GitChangedError::new(
            GitChangedErrorKind::InvalidRoot,
            format!(
                "Wake Test changed-file root `{}` is not a directory",
                canonical.display()
            ),
        ));
    }
    Ok(canonical)
}

fn ensure_work_tree(root: &Path) -> Result<(), GitChangedError> {
    let output = run_git(root, &["rev-parse", "--is-inside-work-tree"])?;
    if !output.status.success() {
        let stderr = stderr_message(&output);
        let kind = if stderr.contains("not a git repository") {
            GitChangedErrorKind::NotRepository
        } else {
            GitChangedErrorKind::CommandFailed
        };
        return Err(GitChangedError::new(
            kind,
            format!(
                "cannot inspect Git work tree for `{}`: {}",
                root.display(),
                status_message(&output)
            ),
        ));
    }
    if output.stdout != b"true\n" && output.stdout != b"true\r\n" {
        return Err(GitChangedError::new(
            GitChangedErrorKind::NotRepository,
            format!(
                "Wake Test changed-file root `{}` is not inside a Git work tree",
                root.display()
            ),
        ));
    }
    Ok(())
}

fn repository_has_head(root: &Path) -> Result<bool, GitChangedError> {
    let head = run_git(root, &["rev-parse", "--verify", "--quiet", "HEAD"])?;
    if head.status.success() {
        return Ok(true);
    }

    // A symbolic HEAD whose target does not exist is the portable representation of an unborn
    // branch. A malformed/detached HEAD that cannot be verified is a genuine command failure.
    let symbolic = run_git(root, &["symbolic-ref", "--quiet", "HEAD"])?;
    if symbolic.status.success() {
        return Ok(false);
    }
    Err(GitChangedError::new(
        GitChangedErrorKind::CommandFailed,
        format!(
            "cannot resolve Git HEAD for `{}`: {}; symbolic HEAD check: {}",
            root.display(),
            status_message(&head),
            status_message(&symbolic)
        ),
    ))
}

fn checked_git(root: &Path, args: &[&str], operation: &str) -> Result<Vec<u8>, GitChangedError> {
    let output = run_git(root, args)?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    Err(GitChangedError::new(
        GitChangedErrorKind::CommandFailed,
        format!(
            "failed to {operation} in `{}`: {}",
            root.display(),
            status_message(&output)
        ),
    ))
}

fn run_git(root: &Path, args: &[&str]) -> Result<Output, GitChangedError> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        // Machine-readable output is parsed independently of locale; forcing the diagnostic
        // locale lets us classify the standard non-repository failure consistently.
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .output()
        .map_err(|error| {
            let kind = if error.kind() == io::ErrorKind::NotFound {
                GitChangedErrorKind::GitUnavailable
            } else {
                GitChangedErrorKind::CommandFailed
            };
            GitChangedError::new(
                kind,
                format!(
                    "cannot launch Git for Wake Test changed-file discovery in `{}`: {error}",
                    root.display()
                ),
            )
        })
}

fn parse_nul_paths(
    root: &Path,
    bytes: &[u8],
    description: &str,
) -> Result<Vec<PathBuf>, GitChangedError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if bytes.last() != Some(&0) {
        return Err(GitChangedError::new(
            GitChangedErrorKind::InvalidOutput,
            format!("Git returned unterminated -z output for {description}"),
        ));
    }

    bytes[..bytes.len() - 1]
        .split(|byte| *byte == 0)
        .map(|entry| absolute_git_path(root, entry, description))
        .collect()
}

fn absolute_git_path(
    root: &Path,
    bytes: &[u8],
    description: &str,
) -> Result<PathBuf, GitChangedError> {
    if bytes.is_empty() {
        return Err(GitChangedError::new(
            GitChangedErrorKind::InvalidOutput,
            format!("Git returned an empty path in {description}"),
        ));
    }
    let relative = path_buf_from_git(bytes, description)?;
    if relative.is_absolute() {
        return Err(outside_root_error(&relative, root));
    }

    let mut normalized = PathBuf::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => normalized.push(value),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(outside_root_error(&relative, root));
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(GitChangedError::new(
            GitChangedErrorKind::InvalidOutput,
            format!("Git returned a non-file path in {description}"),
        ));
    }
    Ok(root.join(normalized))
}

#[cfg(unix)]
fn path_buf_from_git(bytes: &[u8], _description: &str) -> Result<PathBuf, GitChangedError> {
    use std::os::unix::ffi::OsStringExt;

    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

#[cfg(windows)]
fn path_buf_from_git(bytes: &[u8], description: &str) -> Result<PathBuf, GitChangedError> {
    let path = String::from_utf8(bytes.to_vec()).map_err(|error| {
        GitChangedError::new(
            GitChangedErrorKind::InvalidOutput,
            format!("Git returned a non-UTF-8 Windows path in {description}: {error}"),
        )
    })?;
    Ok(PathBuf::from(OsString::from(path)))
}

fn outside_root_error(path: &Path, root: &Path) -> GitChangedError {
    GitChangedError::new(
        GitChangedErrorKind::PathOutsideRoot,
        format!(
            "Git returned path `{}` outside Wake Test changed-file root `{}`",
            path.display(),
            root.display()
        ),
    )
}

fn stderr_message(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).trim().to_string()
}

fn status_message(output: &Output) -> String {
    let stderr = stderr_message(output);
    if stderr.is_empty() {
        format!("Git exited with {}", output.status)
    } else {
        format!("Git exited with {}: {stderr}", output.status)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{GitChangedErrorKind, absolute_git_path, changed_paths, parse_nul_paths};

    static NEXT_TEMP_DIRECTORY: AtomicU64 = AtomicU64::new(0);

    struct TestDirectory {
        path: PathBuf,
    }

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let sequence = NEXT_TEMP_DIRECTORY.fetch_add(1, Ordering::Relaxed);
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "wake-test-git-changed-{label}-{}-{timestamp}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create isolated Git test directory");
            Self { path }
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    }

    fn git(root: &Path, args: &[&str]) -> Output {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .env("LC_ALL", "C")
            .env("LANG", "C")
            .output()
            .expect("launch Git test command");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn init_repository(root: &Path) {
        git(root, &["init", "--quiet"]);
        git(root, &["config", "user.name", "Wake Test"]);
        git(root, &["config", "user.email", "wake-test@example.invalid"]);
        git(root, &["config", "core.autocrlf", "false"]);
    }

    fn commit_all(root: &Path) {
        git(root, &["add", "--all"]);
        git(root, &["commit", "--quiet", "-m", "fixture"]);
    }

    fn absolute_paths(root: &Path, relative: &[&str]) -> Vec<PathBuf> {
        let root = root.canonicalize().expect("canonical fixture root");
        let mut paths = relative
            .iter()
            .map(|path| root.join(path))
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        paths
    }

    #[test]
    fn returns_staged_unstaged_untracked_deleted_and_both_rename_paths() {
        // Git is an optional developer tool for this Rust unit test. Production reports its
        // absence as GitUnavailable; CI environments without Git skip only this integration case.
        if !git_available() {
            eprintln!("skipping Git changed integration test because Git is unavailable");
            return;
        }

        let directory = TestDirectory::new("work-tree");
        let root = &directory.path;
        init_repository(root);
        fs::write(root.join(".gitignore"), "ignored.txt\n").expect("write ignore file");
        for path in ["modified.txt", "deleted.txt", "renamed.txt", "stable.txt"] {
            fs::write(root.join(path), "initial\n").expect("write tracked fixture");
        }
        commit_all(root);
        assert!(
            changed_paths(root)
                .expect("collect clean work-tree paths")
                .is_empty(),
            "a clean HEAD must not select tests"
        );

        fs::write(root.join("modified.txt"), "unstaged\n").expect("modify tracked fixture");
        fs::remove_file(root.join("deleted.txt")).expect("delete tracked fixture");
        git(root, &["mv", "renamed.txt", "renamed-new.txt"]);
        fs::write(root.join("staged.txt"), "staged\n").expect("write staged fixture");
        git(root, &["add", "staged.txt"]);
        fs::write(root.join("untracked file.txt"), "untracked\n").expect("write untracked fixture");
        fs::write(root.join("ignored.txt"), "ignored\n").expect("write ignored fixture");

        let paths = changed_paths(root).expect("collect changed paths");
        assert_eq!(
            paths,
            absolute_paths(
                root,
                &[
                    "deleted.txt",
                    "modified.txt",
                    "renamed-new.txt",
                    "renamed.txt",
                    "staged.txt",
                    "untracked file.txt",
                ],
            )
        );
    }

    #[test]
    fn unborn_repository_uses_index_and_non_ignored_untracked_paths() {
        if !git_available() {
            eprintln!("skipping unborn Git integration test because Git is unavailable");
            return;
        }

        let directory = TestDirectory::new("unborn");
        let root = &directory.path;
        init_repository(root);
        fs::write(root.join("indexed.txt"), "indexed\n").expect("write indexed fixture");
        git(root, &["add", "indexed.txt"]);
        fs::write(root.join("untracked.txt"), "untracked\n").expect("write untracked fixture");
        fs::write(root.join(".git/info/exclude"), "ignored.txt\n").expect("write exclude file");
        fs::write(root.join("ignored.txt"), "ignored\n").expect("write ignored fixture");

        let paths = changed_paths(root).expect("collect unborn changed paths");
        assert_eq!(
            paths,
            absolute_paths(root, &["indexed.txt", "untracked.txt"])
        );
    }

    #[test]
    fn scopes_results_to_a_root_below_the_repository_top_level() {
        if !git_available() {
            eprintln!("skipping nested-root Git test because Git is unavailable");
            return;
        }

        let directory = TestDirectory::new("nested-root");
        let repository = &directory.path;
        let root = repository.join("packages/app");
        fs::create_dir_all(&root).expect("create nested project root");
        init_repository(repository);
        fs::write(repository.join("outside.txt"), "initial\n").expect("write outer fixture");
        fs::write(root.join("inside.txt"), "initial\n").expect("write inner fixture");
        commit_all(repository);

        fs::write(repository.join("outside.txt"), "changed\n").expect("change outer fixture");
        fs::write(root.join("inside.txt"), "changed\n").expect("change inner fixture");
        fs::write(root.join("new.txt"), "untracked\n").expect("write inner untracked fixture");

        assert_eq!(
            changed_paths(&root).expect("collect nested-root changed paths"),
            absolute_paths(&root, &["inside.txt", "new.txt"])
        );
    }

    #[test]
    fn reports_non_repository_with_a_stable_category_and_message() {
        if !git_available() {
            eprintln!("skipping non-repository test because Git is unavailable");
            return;
        }

        let directory = TestDirectory::new("not-repository");
        let error = changed_paths(&directory.path).expect_err("reject non-repository root");
        assert_eq!(error.kind(), GitChangedErrorKind::NotRepository);
        assert!(error.message().contains("Git work tree"));
        assert!(
            error
                .message()
                .contains(&directory.path.display().to_string())
        );
    }

    #[test]
    fn parses_nul_delimited_paths_and_rejects_unsafe_or_truncated_output() {
        let directory = TestDirectory::new("parser");
        let root = directory
            .path
            .canonicalize()
            .expect("canonical parser root");

        assert_eq!(
            parse_nul_paths(&root, b"z.txt\0space name.txt\0", "fixture")
                .expect("parse -z fixture"),
            vec![root.join("z.txt"), root.join("space name.txt")]
        );

        let truncated = parse_nul_paths(&root, b"missing-nul", "fixture")
            .expect_err("reject truncated -z output");
        assert_eq!(truncated.kind(), GitChangedErrorKind::InvalidOutput);

        let outside = absolute_git_path(&root, b"../outside.txt", "fixture")
            .expect_err("reject path traversal");
        assert_eq!(outside.kind(), GitChangedErrorKind::PathOutsideRoot);
    }
}
