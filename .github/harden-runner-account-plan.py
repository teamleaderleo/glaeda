from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if text.count(old) != 1:
        raise SystemExit(f"expected {label} once, found {text.count(old)}")
    return text.replace(old, new, 1)


lane = Path("src/lane_command.rs")
text = lane.read_text()
if "const MIN_SUBORDINATE_ID_COUNT" not in text:
    text = replace_once(
        text,
        'const INSTALL: &str = "/usr/bin/install";\n',
        'const INSTALL: &str = "/usr/bin/install";\nconst MIN_SUBORDINATE_ID_COUNT: u64 = 65_536;\n',
        "lane subordinate minimum constant",
    )
text = replace_once(
    text,
    '    if start == 0 || count == 0 {\n'
    '        return Err(LaneCommandError::single(\n'
    '            "subordinate-ID range must begin above zero and contain at least one ID",\n'
    '        ));\n'
    '    }\n',
    '    if start == 0 || u64::from(count) < MIN_SUBORDINATE_ID_COUNT {\n'
    '        return Err(LaneCommandError::single(\n'
    '            "subordinate-ID range must begin above zero and contain at least 65536 IDs",\n'
    '        ));\n'
    '    }\n',
    "lane subordinate minimum validation",
)
marker = '''        LaneCommand::ensure_subordinate_uids(
            &action(ExecutionLane::Root),
            &account("project-runner"),
            u32::MAX,
            2,
        )
        .expect_err("overflowing subordinate range must fail");
'''
extra = '''        LaneCommand::ensure_subordinate_uids(
            &action(ExecutionLane::Root),
            &account("project-runner"),
            100_000,
            1,
        )
        .expect_err("undersized subordinate range must fail");
'''
if "undersized subordinate range must fail" not in text:
    text = replace_once(text, marker, marker + extra, "lane subordinate error test")
lane.write_text(text)

executor = Path("src/lane_executor.rs")
text = executor.read_text()
text = replace_once(
    text,
    '    start > 0 && start <= end\n',
    '    start > 0\n'
    '        && start <= end\n'
    '        && u64::from(end) - u64::from(start) + 1 >= MIN_SUBORDINATE_ID_COUNT\n',
    "executor subordinate minimum validation",
)
executor.write_text(text)

plan = Path("src/runner_account_plan.rs")
text = plan.read_text()
marker = '''        PlannedSubordinateRange::new(100_000, 1).expect_err("undersized range");
'''
extra = '''        DesiredRunnerAccount::new(
            account("project-runner"),
            account("project-runner"),
            &format!("/{}", "a".repeat(4_000)),
            PlannedSubordinateRange::new(100_000, 65_536).expect("subuid range"),
            PlannedSubordinateRange::new(200_000, 65_536).expect("subgid range"),
        )
        .expect_err("oversized public home path");
'''
if "oversized public home path" not in text:
    text = replace_once(text, marker, marker + extra, "runner home length test")
plan.write_text(text)
