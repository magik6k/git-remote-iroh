//! End-to-end tests: real `git` talking to a real `serve` over loopback iroh
//! connections. These bind iroh endpoints; they work without internet access
//! (tickets carry direct addresses), though a relay warning may be printed.

use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_git-remote-iroh");

/// PATH with the helper binary's directory prepended, so git finds
/// git-remote-iroh for iroh:// URLs.
fn helper_path() -> String {
    let bin_dir = Path::new(BIN).parent().unwrap();
    format!(
        "{}:{}",
        bin_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=test", "-c", "user.email=test@example.com"])
        .args(args)
        .env("PATH", helper_path())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("failed to run git");
    assert!(
        out.status.success(),
        "git {args:?} failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn commit_file(repo: &Path, name: &str, contents: &str, message: &str) {
    std::fs::write(repo.join(name), contents).unwrap();
    git(repo, &["add", "."]);
    git(repo, &["commit", "-qm", message]);
}

/// Kills the child process on drop so a failing test doesn't leak servers.
struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

/// Spawn `git-remote-iroh serve` in `repo` and return (guard, url).
fn spawn_serve(repo: &Path, extra_args: &[&str]) -> (KillOnDrop, String) {
    let child = Command::new(BIN)
        .arg("serve")
        .args(extra_args)
        .current_dir(repo)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn serve");
    let mut guard = KillOnDrop(child);
    let stdout = guard.0.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let url = lines
        .next()
        .expect("serve exited without printing a url")
        .expect("failed to read serve stdout");
    assert!(url.starts_with("iroh://"), "unexpected serve output: {url}");
    (guard, url)
}

fn wait_exit(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait failed") {
            return status;
        }
        assert!(
            start.elapsed() < timeout,
            "process did not exit within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn clone_fetch_push() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main"]);
    commit_file(&src, "file.txt", "hello\n", "one");
    commit_file(&src, "more.txt", "world\n", "two");

    let (_serve, url) = spawn_serve(&src, &["--writable"]);

    // Clone through the iroh:// URL.
    let clone = tmp.path().join("clone");
    git(tmp.path(), &["clone", "-q", &url, clone.to_str().unwrap()]);
    assert_eq!(
        git(&clone, &["rev-parse", "HEAD"]),
        git(&src, &["rev-parse", "HEAD"])
    );

    // Incremental fetch picks up new commits.
    commit_file(&src, "third.txt", "third\n", "three");
    git(&clone, &["fetch", "-q", "origin"]);
    assert_eq!(
        git(&clone, &["rev-parse", "origin/main"]),
        git(&src, &["rev-parse", "main"])
    );

    // Push a new branch back into the served repo.
    git(&clone, &["checkout", "-qb", "feature"]);
    commit_file(&clone, "feature.txt", "feature\n", "from clone");
    git(&clone, &["push", "-q", "origin", "feature"]);
    assert_eq!(
        git(&src, &["rev-parse", "feature"]),
        git(&clone, &["rev-parse", "feature"])
    );
}

#[test]
fn read_only_serve_rejects_push() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main"]);
    commit_file(&src, "file.txt", "hello\n", "one");

    let (_serve, url) = spawn_serve(&src, &[]);

    let clone = tmp.path().join("clone");
    git(tmp.path(), &["clone", "-q", &url, clone.to_str().unwrap()]);
    commit_file(&clone, "nope.txt", "nope\n", "rejected");
    let out = Command::new("git")
        .arg("-C")
        .arg(&clone)
        .args(["push", "origin", "main:other"])
        .env("PATH", helper_path())
        .output()
        .unwrap();
    assert!(!out.status.success(), "push into read-only serve succeeded");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--writable"),
        "unhelpful push rejection: {stderr}"
    );
}

#[test]
fn offer_mode_push() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    std::fs::create_dir(&src).unwrap();
    git(&src, &["init", "-q", "-b", "main"]);
    commit_file(&src, "file.txt", "hello\n", "one");

    // `git push iroh:// main` blocks until a peer fetches; grab the URL from
    // its stderr.
    let child = Command::new("git")
        .arg("-C")
        .arg(&src)
        .args(["push", "iroh://", "main"])
        .env("PATH", helper_path())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut guard = KillOnDrop(child);
    let mut stderr = guard.0.stderr.take().unwrap();
    let url = {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        let url = loop {
            assert!(stderr.read(&mut byte).unwrap() > 0, "push exited early");
            buf.push(byte[0]);
            let text = String::from_utf8_lossy(&buf);
            if let Some(start) = text.find("iroh://endpoint") {
                if let Some(rest) = text[start..].split_whitespace().next() {
                    if text[start..].contains(char::is_whitespace) {
                        break rest.to_string();
                    }
                }
            }
        };
        // Keep draining stderr in the background so the push cannot block on
        // a full pipe.
        std::thread::spawn(move || std::io::copy(&mut stderr, &mut std::io::sink()));
        url
    };

    let dst = tmp.path().join("dst");
    git(tmp.path(), &["clone", "-q", &url, dst.to_str().unwrap()]);
    assert_eq!(
        git(&dst, &["rev-parse", "HEAD"]),
        git(&src, &["rev-parse", "HEAD"])
    );

    let status = wait_exit(&mut guard.0, Duration::from_secs(60));
    assert!(status.success(), "offer-mode push exited with {status}");
}
