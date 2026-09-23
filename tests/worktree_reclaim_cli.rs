#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const BINARY: &str = env!("CARGO_BIN_EXE_glaeda-worktree-reclaim");
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
        self.run(repository, &[])
    }

    fn run(&self, repository: &Path, extra: &[&str]) -> Output {
        Command::new(BINARY)
            .args([
                "--repository",
                repository.to_str().expect("UTF-8 fixture path"),
                "--output",
                "json",
            ])
            .args(extra)
            .output()
            .expect("run worktree reclaim")
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

fn document(output: &Output) -> serde_json::Value {
    assert_child_success("worktree reclaim", output);
    serde_json::from_slice(&output.stdout).expect("JSON report")
}

/// The single repository section of a one-repository report.
fn report(output: &Output) -> serde_json::Value {
    let document = document(output);
    assert_eq!(document["repositories"][0]["result"], "listed");
    document["repositories"][0].clone()
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

    let document = document(&fixture.plan(&fixture.main));
    assert_eq!(document["document_type"], "glaeda-worktree-reclaim-plan");
    assert_eq!(document["mutation_performed"], false);
    let report = document["repositories"][0].clone();
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

    let text = String::from_utf8(serde_json::to_vec(&document).expect("serialize")).expect("UTF-8");
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
    assert_eq!(
        output.status.code(),
        Some(2),
        "an unlisted repository must not pass silently"
    );
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    assert_eq!(document["repositories"][0]["result"], "unlisted");
    assert_eq!(document["repositories"][0]["code"], "not_main_worktree");
}

fn git_as_user(checkout: &Path, arguments: &[&str]) {
    let mut full = vec![
        "-c",
        "user.name=Glaeda Test",
        "-c",
        "user.email=glaeda-test@example.invalid",
        "-c",
        "protocol.file.allow=always",
    ];
    full.extend_from_slice(arguments);
    git(checkout, &full);
}

fn entry_by_name(fixture: &Fixture, report: &serde_json::Value, name: &str) -> serde_json::Value {
    let ordinal = fixture
        .linked_order()
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
}

fn vetoes_of(entry: &serde_json::Value) -> Vec<String> {
    entry["decision"]["vetoes"]
        .as_array()
        .unwrap_or_else(|| panic!("refused entry expected, got {entry}"))
        .iter()
        .map(|veto| veto.as_str().expect("veto code").to_owned())
        .collect()
}

/// Regression fixtures for state `git status` does not show but removal would destroy.
#[test]
fn hidden_unique_state_is_never_eligible() {
    let fixture = Fixture::new();

    // Edits hidden from status by index flags. skip-worktree also forces an index v3 layout.
    let assumed = fixture.add("assumed");
    git(
        &assumed,
        &["update-index", "--assume-unchanged", "tracked.txt"],
    );
    fs::write(assumed.join("tracked.txt"), "hidden edit\n").expect("edit assumed file");
    fixture.age("assumed");

    let skipped = fixture.add("skipped");
    git(
        &skipped,
        &["update-index", "--skip-worktree", "tracked.txt"],
    );
    fs::write(skipped.join("tracked.txt"), "hidden edit\n").expect("edit skipped file");
    fixture.age("skipped");

    // A submodule whose repository lives inside the worktree as a `.git` directory.
    let embedded = fixture.add("embedded");
    let nested = embedded.join("nested");
    fs::create_dir(&nested).expect("create nested repository");
    git(&nested, &["init", "-b", "main"]);
    commit(&nested, "unique nested commit");
    git_as_user(&embedded, &["submodule", "add", "./nested", "nested"]);
    git_as_user(&embedded, &["commit", "-m", "add embedded submodule"]);
    fixture.age("embedded");

    // A submodule cloned normally, which Git absorbs into the worktree's own modules directory.
    let source = fixture.root.join("submodule-source");
    fs::create_dir(&source).expect("create submodule source");
    git(&source, &["init", "-b", "main"]);
    commit(&source, "source commit");
    let absorbed = fixture.add("absorbed");
    git_as_user(
        &absorbed,
        &["submodule", "add", source.to_str().expect("UTF-8"), "sub"],
    );
    git_as_user(&absorbed, &["commit", "-m", "add absorbed submodule"]);
    fixture.age("absorbed");

    // A per-worktree ref pointing at a commit nothing else reaches.
    let per_worktree = fixture.add("per-worktree");
    commit(&per_worktree, "only a per-worktree ref reaches this");
    git(&per_worktree, &["update-ref", "refs/worktree/keep", "HEAD"]);
    git(&per_worktree, &["reset", "--hard", "HEAD~1"]);
    fixture.age("per-worktree");

    // An interrupted cherry-pick.
    git(&fixture.main, &["branch", "conflict-source"]);
    let conflict_source = fixture.root.join("conflict-source-wt");
    git(
        &fixture.main,
        &[
            "worktree",
            "add",
            conflict_source.to_str().expect("UTF-8"),
            "conflict-source",
        ],
    );
    fs::write(conflict_source.join("tracked.txt"), "theirs\n").expect("write theirs");
    git_as_user(&conflict_source, &["commit", "-am", "theirs"]);
    let operation = fixture.add("operation");
    fs::write(operation.join("tracked.txt"), "ours\n").expect("write ours");
    git_as_user(&operation, &["commit", "-am", "ours"]);
    let status = Command::new(GIT)
        .arg("-C")
        .arg(&operation)
        .args([
            "-c",
            "user.name=Glaeda Test",
            "-c",
            "user.email=glaeda-test@example.invalid",
            "cherry-pick",
            "conflict-source",
        ])
        .env_clear()
        .env("HOME", &operation)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("run conflicting cherry-pick");
    assert!(
        !status.status.success(),
        "cherry-pick must stop on conflict"
    );
    fixture.age("operation");

    let report = report(&fixture.plan(&fixture.main));
    assert_eq!(
        vetoes_of(&entry_by_name(&fixture, &report, "assumed")),
        ["hidden_index_entries_present"]
    );
    assert_eq!(
        vetoes_of(&entry_by_name(&fixture, &report, "skipped")),
        ["hidden_index_entries_present"]
    );
    assert_eq!(
        vetoes_of(&entry_by_name(&fixture, &report, "embedded")),
        ["populated_submodules_present"]
    );
    assert_eq!(
        vetoes_of(&entry_by_name(&fixture, &report, "absorbed")),
        ["populated_submodules_present"]
    );
    assert_eq!(
        vetoes_of(&entry_by_name(&fixture, &report, "per-worktree")),
        ["per_worktree_refs_present"]
    );
    assert!(
        vetoes_of(&entry_by_name(&fixture, &report, "operation"))
            .contains(&"operation_in_progress".to_owned())
    );
}

/// Only branches, tags, and remote-tracking refs preserve a detached HEAD.
#[test]
fn detached_head_preservation_ignores_short_lived_refs() {
    let fixture = Fixture::new();

    let reachable = fixture.add("reachable");
    git(&reachable, &["checkout", "--detach"]);
    fixture.age("reachable");

    // The stash contains this commit, but a stash drop would orphan it.
    let stashed = fixture.add("stashed");
    git(&stashed, &["checkout", "--detach"]);
    commit(&stashed, "reached only by the stash");
    fs::write(stashed.join("tracked.txt"), "to stash\n").expect("edit for stash");
    git_as_user(&stashed, &["stash"]);
    git(&fixture.main, &["branch", "-D", "stashed"]);
    fixture.age("stashed");

    let report = report(&fixture.plan(&fixture.main));
    let reachable = entry_by_name(&fixture, &report, "reachable");
    assert_eq!(reachable["decision"]["compensation"], "none_required");
    let stashed = entry_by_name(&fixture, &report, "stashed");
    assert_eq!(stashed["decision"]["compensation"], "pin_head_commit");
}

/// A registration whose path became a symlink to another worktree must not borrow its facts.
#[test]
fn aliased_registration_is_unobservable() {
    let fixture = Fixture::new();
    let target = fixture.add("target");
    fixture.age("target");
    let alias = fixture.add("alias");
    fs::rename(&alias, fixture.root.join("alias-moved")).expect("move alias checkout away");
    std::os::unix::fs::symlink(&target, &alias).expect("replace alias with symlink");

    let report = report(&fixture.plan(&fixture.main));
    let alias = entry_by_name(&fixture, &report, "alias");
    assert_eq!(alias["result"], "unobservable");
    assert_eq!(alias["code"], "registration_aliased");
    assert_eq!(
        entry_by_name(&fixture, &report, "target")["decision"]["decision"],
        "eligible"
    );
}

/// Layouts the index scanner must either read correctly or refuse, never misread.
#[test]
fn index_layouts_are_read_or_refused_never_misread() {
    let fixture = Fixture::new();

    // Index v4 prefix-compresses paths; skip-worktree must still be found through it.
    let compressed = fixture.add("compressed");
    git(&compressed, &["update-index", "--index-version", "4"]);
    git(
        &compressed,
        &["update-index", "--skip-worktree", "tracked.txt"],
    );
    fs::write(compressed.join("tracked.txt"), "hidden edit\n").expect("edit v4 file");
    fixture.age("compressed");

    // A split index keeps entries and flags in a shared file this observer does not read.
    let split = fixture.add("split");
    git(
        &split,
        &["update-index", "--assume-unchanged", "tracked.txt"],
    );
    git(&split, &["update-index", "--split-index"]);
    fs::write(split.join("tracked.txt"), "hidden edit\n").expect("edit split file");
    git(
        &split,
        &[
            "-c",
            "splitIndex.maxPercentChange=0",
            "update-index",
            "--split-index",
        ],
    );
    fixture.age("split");

    // A submodule path whose repository was deleted but whose files remain.
    let source = fixture.root.join("submodule-source");
    fs::create_dir(&source).expect("create submodule source");
    git(&source, &["init", "-b", "main"]);
    commit(&source, "source commit");
    let orphaned = fixture.add("orphaned");
    git_as_user(
        &orphaned,
        &["submodule", "add", source.to_str().expect("UTF-8"), "sub"],
    );
    git_as_user(&orphaned, &["commit", "-m", "add submodule"]);
    fs::remove_file(orphaned.join("sub/.git")).expect("remove submodule gitfile");
    fs::remove_dir_all(fixture.main.join(".git/worktrees/orphaned/modules"))
        .expect("remove absorbed submodule repository");
    fs::write(orphaned.join("sub/work.txt"), "local only\n").expect("write orphaned file");
    fixture.age("orphaned");

    let report = report(&fixture.plan(&fixture.main));
    assert_eq!(
        vetoes_of(&entry_by_name(&fixture, &report, "compressed")),
        ["hidden_index_entries_present"]
    );
    let split = entry_by_name(&fixture, &report, "split");
    assert_eq!(split["result"], "unobservable");
    assert_eq!(split["code"], "split_index_unsupported");
    assert_eq!(
        vetoes_of(&entry_by_name(&fixture, &report, "orphaned")),
        ["populated_submodules_present"]
    );
}

/// `worktree add --relative-paths` (Git 2.48+) records a relative backlink that must still bind.
#[test]
fn relative_path_worktrees_are_observable() {
    let fixture = Fixture::new();
    let path = fixture.root.join("relative");
    let added = Command::new(GIT)
        .arg("-C")
        .arg(&fixture.main)
        .args([
            "worktree",
            "add",
            "--relative-paths",
            "-b",
            "relative",
            path.to_str().expect("UTF-8"),
        ])
        .env_clear()
        .env("HOME", &fixture.main)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .expect("run git worktree add");
    if !added.status.success() {
        // Older Git has no relative worktree links, so there is nothing to bind.
        assert!(
            String::from_utf8_lossy(&added.stderr).contains("relative-paths"),
            "unexpected worktree add failure: {}",
            String::from_utf8_lossy(&added.stderr)
        );
        return;
    }
    fixture.age("relative");
    let report = report(&fixture.plan(&fixture.main));
    assert_eq!(
        entry_by_name(&fixture, &report, "relative")["decision"]["decision"],
        "eligible"
    );
}

fn ref_exists(repository: &Path, reference: &str) -> bool {
    Command::new(GIT)
        .arg("-C")
        .arg(repository)
        .args(["rev-parse", "--quiet", "--verify", reference])
        .output()
        .expect("run rev-parse")
        .status
        .success()
}

/// `--apply` removes exactly the eligible worktrees, pins an unreachable HEAD first, and keeps
/// everything a fresh check refuses.
#[test]
fn apply_reclaims_only_eligible_worktrees_and_pins_orphan_heads() {
    let fixture = Fixture::new();

    let clean = fixture.add("clean");
    fixture.age("clean");

    let detached = fixture.add("detached");
    git(&detached, &["checkout", "--detach"]);
    commit(&detached, "only this worktree reaches it");
    let orphan = String::from_utf8(git_output(&detached, &["rev-parse", "HEAD"]).stdout)
        .expect("UTF-8")
        .trim()
        .to_owned();
    git(&fixture.main, &["branch", "-D", "detached"]);
    fixture.age("detached");

    let dirty = fixture.add("dirty");
    fs::write(dirty.join("scratch.txt"), "local\n").expect("write untracked file");
    fixture.age("dirty");

    let recent = fixture.add("recent");

    let receipt = document(&fixture.run(&fixture.main, &["--apply"]));
    assert_eq!(receipt["document_type"], "glaeda-worktree-reclaim-receipt");
    assert_eq!(receipt["mutation_performed"], true);
    assert_eq!(receipt["circuit_breaker_tripped"], false);
    assert_eq!(receipt["repositories"][0]["summary"]["reclaimed"], 2);
    let outcomes = receipt["repositories"][0]["worktrees"]
        .as_array()
        .expect("worktrees")
        .iter()
        .filter_map(|worktree| worktree.get("reclaim"))
        .collect::<Vec<_>>();
    assert!(outcomes.iter().any(|outcome| {
        outcome["target"]["branch"] == "clean"
            && outcome["target"]["commit"]
                .as_str()
                .is_some_and(|commit| commit.len() == 40)
    }));
    assert!(
        outcomes
            .iter()
            .any(|outcome| outcome["target"]["commit"] == orphan.as_str()
                && outcome["target"]["pinned"] == true)
    );

    assert!(!clean.exists(), "clean worktree removed");
    assert!(!detached.exists(), "detached worktree removed");
    assert!(
        ref_exists(&fixture.main, "refs/heads/clean"),
        "branch survives removal"
    );
    assert!(
        ref_exists(
            &fixture.main,
            &format!("refs/glaeda/worktree-pins/{orphan}")
        ),
        "orphan HEAD pinned before removal"
    );
    assert!(dirty.join("scratch.txt").exists(), "dirty worktree kept");
    assert!(recent.exists(), "recent worktree kept");

    let listed = fixture.linked_order();
    assert_eq!(listed, ["dirty", "recent"]);

    // A second run finds nothing left to do and changes nothing.
    let again = document(&fixture.run(&fixture.main, &["--apply"]));
    assert_eq!(again["mutation_performed"], false);
    assert_eq!(again["repositories"][0]["summary"]["eligible"], 0);
}

#[test]
fn apply_stops_at_the_reclaim_budget() {
    let fixture = Fixture::new();
    let first = fixture.add("first");
    fixture.age("first");
    let second = fixture.add("second");
    fixture.age("second");

    let document = document(&fixture.run(&fixture.main, &["--apply", "--max-reclaims", "1"]));
    assert_eq!(document["budget_exhausted"], true);
    assert_eq!(document["repositories"][0]["summary"]["reclaimed"], 1);
    assert_eq!(
        usize::from(first.exists()) + usize::from(second.exists()),
        1,
        "exactly one worktree removed"
    );
}

/// A removal Git starts but cannot finish trips the circuit breaker and stops the batch.
#[test]
fn a_partial_removal_trips_the_circuit_breaker() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = Fixture::new();
    fs::write(fixture.main.join(".git/info/exclude"), "build/\n").expect("ignore build output");
    let first = fixture.add("first");
    fixture.age("first");
    let second = fixture.add("second");
    fixture.age("second");
    let order = fixture.linked_order();
    let (stuck, untouched) = if order[0] == "first" {
        (first, second)
    } else {
        (second, first)
    };
    let build = stuck.join("build");
    fs::create_dir(&build).expect("create ignored build directory");
    fs::write(build.join("output.o"), "binary\n").expect("write ignored output");
    fs::set_permissions(&build, fs::Permissions::from_mode(0o555)).expect("make build read-only");
    // Re-age after creating the directory changed the checkout root mtime.
    fixture.age(
        stuck
            .file_name()
            .and_then(|name| name.to_str())
            .expect("name"),
    );

    let output = fixture.run(&fixture.main, &["--apply"]);
    fs::set_permissions(&build, fs::Permissions::from_mode(0o755)).expect("restore permissions");
    assert_eq!(output.status.code(), Some(3), "circuit breaker exit status");
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON receipt");
    assert_eq!(receipt["circuit_breaker_tripped"], true);
    assert_eq!(receipt["mutation_performed"], true);
    assert_eq!(receipt["repositories"][0]["summary"]["reclaimed"], 0);
    assert!(
        untouched.exists(),
        "the batch stopped before the next worktree"
    );
}
