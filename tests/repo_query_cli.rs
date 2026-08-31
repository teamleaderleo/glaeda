#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

const BINARY: &str = env!("CARGO_BIN_EXE_glaeda-repo-query");
const GIT: &str = "/usr/bin/git";

struct Fixture {
    root: PathBuf,
    checkout: PathBuf,
    base: String,
    head: String,
}

impl Fixture {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "glaeda-repo-query-cli-{}-{nonce}",
            std::process::id()
        ));
        let checkout = root.join("checkout");
        fs::create_dir_all(&checkout).expect("create checkout");
        git(&checkout, &["init", "-b", "main"]);
        git(
            &checkout,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/teamleaderleo/glaeda.git",
            ],
        );
        fs::write(checkout.join("one.txt"), "one\n").expect("write base");
        git(&checkout, &["add", "one.txt"]);
        commit(&checkout, "base");
        let base = git_stdout(&checkout, &["rev-parse", "HEAD"]);
        fs::write(checkout.join("one.txt"), "one\ntwo\n").expect("change file");
        fs::write(checkout.join("two.txt"), "three\n").expect("add file");
        git(&checkout, &["add", "one.txt", "two.txt"]);
        commit(&checkout, "head");
        let head = git_stdout(&checkout, &["rev-parse", "HEAD"]);
        Self {
            root,
            checkout,
            base,
            head,
        }
    }

    fn query(&self, extra: &[&str]) -> Output {
        Command::new(BINARY)
            .args([
                "--checkout",
                self.checkout.to_str().expect("UTF-8 checkout"),
                "--project",
                "github.com/teamleaderleo/glaeda",
                "--base",
                &self.base,
                "--head",
                &self.head,
            ])
            .args(extra)
            .output()
            .expect("run repo query")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self.root.starts_with(std::env::temp_dir()) {
            fs::remove_dir_all(&self.root).expect("remove fixture")
        }
    }
}

fn git(checkout: &Path, arguments: &[&str]) {
    let output = Command::new(GIT)
        .arg("-C")
        .arg(checkout)
        .args(arguments)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run fixture Git");
    assert!(
        output.status.success(),
        "fixture Git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(checkout: &Path, arguments: &[&str]) -> String {
    let output = Command::new(GIT)
        .arg("-C")
        .arg(checkout)
        .args(arguments)
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("read fixture Git");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("UTF-8 Git output")
        .trim()
        .to_owned()
}

fn commit(checkout: &Path, message: &str) {
    git(
        checkout,
        &[
            "-c",
            "user.name=Glaeda Test",
            "-c",
            "user.email=glaeda-test@example.invalid",
            "commit",
            "-m",
            message,
        ],
    );
}

#[test]
fn exact_bundle_is_compact_path_private_and_complete() {
    let fixture = Fixture::new();
    let output = fixture.query(&[]);
    assert!(
        output.status.success(),
        "query failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["document_type"], "glaeda-resident-repo-query");
    assert_eq!(report["profile_id"], "repo-query/v1");
    assert_eq!(report["authority"], "observation_only");
    assert_eq!(report["requested_base"], fixture.base);
    assert_eq!(report["head"], fixture.head);
    assert_eq!(report["base_is_ancestor"], true);
    assert_eq!(report["commits_since_merge_base"], 1);
    assert_eq!(report["diff_summary"]["files_changed"], 2);
    assert_eq!(report["diff_summary"]["insertions"], 2);
    assert_eq!(report["diff_summary"]["deletions"], 0);
    assert_eq!(report["patch"]["included"], true);
    assert_eq!(report["metrics"]["git_processes"], 12);
    let encoded = String::from_utf8(output.stdout).expect("UTF-8 report");
    assert!(!encoded.contains(fixture.root.to_str().expect("UTF-8 root")));
    assert!(!encoded.contains("/usr/bin/git"));
}

#[test]
fn patch_limit_omits_content_without_losing_digest_or_counts() {
    let fixture = Fixture::new();
    let output = fixture.query(&["--max-patch-bytes", "0"]);
    assert!(output.status.success());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(report["patch"]["included"], false);
    assert!(report["patch"].get("text").is_none());
    assert!(report["patch"]["bytes"].as_u64().expect("bytes") > 0);
    assert!(
        report["patch"]["sha256"]
            .as_str()
            .expect("digest")
            .starts_with("sha256:")
    );
}

#[test]
fn project_mismatch_fails_without_echoing_checkout() {
    let fixture = Fixture::new();
    let output = Command::new(BINARY)
        .args([
            "--checkout",
            fixture.checkout.to_str().expect("UTF-8 checkout"),
            "--project",
            "github.com/teamleaderleo/cultist",
            "--base",
            &fixture.base,
            "--head",
            &fixture.head,
        ])
        .output()
        .expect("run mismatch");
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
    let error = String::from_utf8(output.stderr).expect("UTF-8 error");
    assert!(error.contains("repository_mismatch"));
    assert!(!error.contains(fixture.root.to_str().expect("UTF-8 root")));
}

#[test]
fn cli_has_no_arbitrary_git_program_or_query_argv_surface() {
    let output = Command::new(BINARY)
        .arg("--help")
        .output()
        .expect("read help");
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("UTF-8 help");
    assert!(!help.contains("git-program"));
    assert!(!help.contains("command"));
    assert!(!help.contains("argument"));
}
