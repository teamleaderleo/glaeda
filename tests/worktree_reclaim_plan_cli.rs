#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const BINARY: &str = env!("CARGO_BIN_EXE_glaeda-worktree-reclaim-plan");
const GIT: &str = "/usr/bin/git";
const TOUCH: &str = "/usr/bin/touch";
/// Older than any idle window the tests use.
const LONG_AGO: &str = "202001010000";
static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
    main: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("current time")
            .as_nanos();
        let temporary_root =
            fs::canonicalize(std::env::temp_dir()).expect("canonicalize test temporary directory");
        let root = temporary_root.join(format!(
            "glaeda-worktree-reclaim-cli-{}-{nonce}-{sequence}",
            std::process::id()
        ));
        let main = root.join("main");
        fs::create_dir_all(&main).expect("create main checkout");
        git(&main, &["init", "-b", "main"]);
        fs::write(main.join("tracked.txt"), "initial\n").expect("write tracked fixture");
        git(&main, &["add", "tracked.txt"]);
        commit(&main, "fixture");
        Self { root, main }
    }

    /// Add a linked worktree on a new branch and return its path.
    fn add(&self, name: &str) -> PathBuf {
        let path = self.root.join(name);
        git(
            &self.main,
            &[
                "worktree",
                "add",
                "-b",
                name,
                path.to_str().expect("UTF-8 fixture path"),
            ],
        );
        path
    }

    /// Make every activity signal of one linked worktree old.
    fn age(&self, name: &str) {
        let git_dir = self.main.join(".git/worktrees").join(name);
        for path in [
            self.root.join(name),
            git_dir.join("HEAD"),
            git_dir.join("logs/HEAD"),
        ] {
            let output = Command::new(TOUCH)
                .args(["-h", "-t", LONG_AGO])
                .arg(&path)
                .output()
                .expect("run touch");
            assert_child_success("backdate fixture", &output);
        }
    }

    fn plan(&self, repository: &Path) -> Output {
        Command::new(BINARY)
            .args([
                "--repository",
                repository.to_str().expect("UTF-8 fixture path"),
                "--output",
                "json",
            ])
            .output()
            .expect("run worktree reclaim planner")
    }

    /// Linked worktree basenames in `git worktree list` order, which the report's ordinals follow.
    fn linked_order(&self) -> Vec<String> {
        let output = git_output(&self.main, &["worktree", "list", "--porcelain"]);
        String::from_utf8(output.stdout)
            .expect("UTF-8 worktree list")
            .lines()
            .filter_map(|line| line.strip_prefix("worktree "))
            .skip(1)
            .map(|path| {
                Path::new(path)
                    .file_name()
                    .expect("worktree basename")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let temporary_root =
            fs::canonicalize(std::env::temp_dir()).expect("canonicalize test temporary directory");
        if self.root.parent() == Some(temporary_root.as_path()) {
            fs::remove_dir_all(&self.root).expect("remove exact fixture root");
        }
    }
}

fn git_output(checkout: &Path, arguments: &[&str]) -> Output {
    let output = Command::new(GIT)
        .arg("-C")
        .arg(checkout)
        .args(arguments)
        .env_clear()
        .env("HOME", checkout)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .output()
        .expect("run fixture Git");
    assert_child_success("fixture Git", &output);
    output
}

fn git(checkout: &Path, arguments: &[&str]) {
    git_output(checkout, arguments);
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
            "--allow-empty",
            "-m",
            message,
        ],
    );
}

fn assert_child_success(operation: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{operation} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn report(output: &Output) -> serde_json::Value {
    assert_child_success("worktree reclaim plan", output);
    serde_json::from_slice(&output.stdout).expect("JSON report")
}

/// Every worktree state the planner distinguishes, observed through the real binary and real Git.
#[test]
fn plan_classifies_real_worktrees_and_changes_nothing() {
    let fixture = Fixture::new();

    fixture.add("clean");
    fixture.age("clean");

    // A detached commit no branch reaches: removing the worktree would orphan it.
    let detached = fixture.add("detached");
    git(&detached, &["checkout", "--detach"]);
    commit(&detached, "only reachable from this worktree");
    git(&fixture.main, &["branch", "-D", "detached"]);
    fixture.age("detached");

    let dirty = fixture.add("dirty");
    fs::write(dirty.join("tracked.txt"), "edited\n").expect("edit tracked file");
    fs::write(dirty.join("scratch.txt"), "local\n").expect("write untracked file");
    fixture.age("dirty");

    let locked = fixture.add("locked");
    git(
        &fixture.main,
        &["worktree", "lock", locked.to_str().expect("UTF-8")],
    );
    fixture.age("locked");

    fixture.add("recent");

    let missing = fixture.add("missing");
    fs::remove_dir_all(&missing).expect("remove worktree directory behind Git's back");

    let listing_before = git_output(&fixture.main, &["worktree", "list", "--porcelain"]).stdout;
    let refs_before = git_output(&fixture.main, &["for-each-ref"]).stdout;

    let report = report(&fixture.plan(&fixture.main));
    assert_eq!(report["document_type"], "glaeda-worktree-reclaim-plan");
    assert_eq!(report["mutation_performed"], false);
    assert_eq!(report["summary"]["linked"], 6);
    assert_eq!(report["summary"]["eligible"], 2);
    assert_eq!(report["summary"]["eligible_requiring_head_pin"], 1);
    assert_eq!(report["summary"]["refused"], 3);
    assert_eq!(report["summary"]["prunable"], 1);
    assert_eq!(report["summary"]["unobservable"], 0);

    let order = fixture.linked_order();
    let by_name = |name: &str| {
        let ordinal = order
            .iter()
            .position(|entry| entry == name)
            .expect("listed")
            + 1;
        report["worktrees"]
            .as_array()
            .expect("worktree array")
            .iter()
            .find(|worktree| worktree["ordinal"] == ordinal)
            .expect("reported ordinal")
            .clone()
    };

    let clean = by_name("clean");
    assert_eq!(clean["decision"]["decision"], "eligible");
    assert_eq!(clean["decision"]["compensation"], "none_required");

    let detached = by_name("detached");
    assert_eq!(detached["decision"]["decision"], "eligible");
    assert_eq!(detached["decision"]["compensation"], "pin_head_commit");
    assert_eq!(
        detached["facts"]["head_reachability"],
        "only_from_this_worktree"
    );

    assert_eq!(
        by_name("dirty")["decision"]["vetoes"],
        serde_json::json!(["tracked_changes_present", "untracked_entries_present"])
    );
    assert_eq!(
        by_name("locked")["decision"]["vetoes"],
        serde_json::json!(["locked"])
    );
    assert_eq!(
        by_name("recent")["decision"]["vetoes"],
        serde_json::json!(["recently_active"])
    );
    assert_eq!(by_name("missing")["result"], "prunable");

    let text = String::from_utf8(serde_json::to_vec(&report).expect("serialize")).expect("UTF-8");
    assert!(
        !text.contains(fixture.root.to_str().expect("UTF-8")),
        "report must not publish checkout paths"
    );

    assert_eq!(
        git_output(&fixture.main, &["worktree", "list", "--porcelain"]).stdout,
        listing_before
    );
    assert_eq!(
        git_output(&fixture.main, &["for-each-ref"]).stdout,
        refs_before
    );
    assert!(dirty.join("scratch.txt").exists());
}

#[test]
fn a_linked_worktree_is_not_accepted_as_the_repository() {
    let fixture = Fixture::new();
    let linked = fixture.add("linked");
    let output = fixture.plan(&linked);
    assert_eq!(output.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON error");
    assert_eq!(error["code"], "not_main_worktree");
}
