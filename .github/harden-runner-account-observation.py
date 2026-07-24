from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if text.count(old) != 1:
        raise SystemExit(f"expected {label} once, found {text.count(old)}")
    return text.replace(old, new, 1)


path = Path("src/runner_account_observation.rs")
text = path.read_text()

text = replace_once(
    text,
    "        parsed_group.as_ref(),\n        desired,\n",
    "        parsed_group.as_ref(),\n        group.state(),\n        desired,\n",
    "group state call argument",
)

old_signature = '''fn classify_user(
    lookup: &Lookup,
    parsed: Option<&PasswdRecord>,
    group: Option<&GroupRecord>,
    desired: &DesiredRunnerAccount,
) -> Result<PreparationObservation, RunnerAccountObservationError> {
'''
new_signature = '''fn classify_user(
    lookup: &Lookup,
    parsed: Option<&PasswdRecord>,
    group: Option<&GroupRecord>,
    group_state: PreparationObservationState,
    desired: &DesiredRunnerAccount,
) -> Result<PreparationObservation, RunnerAccountObservationError> {
'''
text = replace_once(text, old_signature, new_signature, "classify_user signature")

old_branch = '''            Some(record)
                if record.username() == desired.username()
                    && record.uid() > 0
                    && record.primary_gid() > 0
                    && record.home() == desired.home()
                    && record.shell() == EXPECTED_SHELL
                    && group.is_some_and(|group| group.gid == record.primary_gid()) =>
            {
                (
                    PreparationObservationState::Matching,
                    format!(
                        "getent passwd {} matched UID {}, primary GID {}, home, and nologin shell",
                        record.username().as_str(),
                        record.uid(),
                        record.primary_gid()
                    ),
                )
            }
            Some(_) => (
                PreparationObservationState::Conflicting,
                format!(
                    "getent passwd {} conflicts with the desired identity or primary group",
                    desired.username().as_str()
                ),
            ),
'''
new_branch = '''            Some(record) if user_record_matches_desired(record, desired) => match group_state {
                PreparationObservationState::Matching
                    if group.is_some_and(|group| group.gid == record.primary_gid()) =>
                {
                    (
                        PreparationObservationState::Matching,
                        format!(
                            "getent passwd {} matched UID {}, primary GID {}, home, and nologin shell",
                            record.username().as_str(),
                            record.uid(),
                            record.primary_gid()
                        ),
                    )
                }
                PreparationObservationState::Unknown => (
                    PreparationObservationState::Unknown,
                    format!(
                        "getent passwd {} matched its local fields but the primary group is unknown",
                        desired.username().as_str()
                    ),
                ),
                PreparationObservationState::Matching
                | PreparationObservationState::Absent
                | PreparationObservationState::Conflicting => (
                    PreparationObservationState::Conflicting,
                    format!(
                        "getent passwd {} conflicts with the desired primary group",
                        desired.username().as_str()
                    ),
                ),
            },
            Some(_) => (
                PreparationObservationState::Conflicting,
                format!(
                    "getent passwd {} conflicts with the desired account fields",
                    desired.username().as_str()
                ),
            ),
'''
text = replace_once(text, old_branch, new_branch, "classify_user matching branch")

marker = '''fn classify_home(
'''
helper = '''fn user_record_matches_desired(record: &PasswdRecord, desired: &DesiredRunnerAccount) -> bool {
    record.username() == desired.username()
        && record.uid() > 0
        && record.primary_gid() > 0
        && record.home() == desired.home()
        && record.shell() == EXPECTED_SHELL
}

fn classify_home(
'''
text = replace_once(text, marker, helper, "classify_home marker")

text = replace_once(
    text,
    "    size: u64,\n",
    "    size: u64,\n    nlink: u64,\n",
    "observed path nlink field",
)
text = replace_once(
    text,
    "            size: metadata.size(),\n",
    "            size: metadata.size(),\n            nlink: metadata.nlink(),\n",
    "linux path nlink observation",
)
text = replace_once(
    text,
    "                && metadata.size == 0 =>\n",
    "                && metadata.size == 0\n                && metadata.nlink == 1 =>\n",
    "linger nlink validation",
)
text = text.replace("                        size: 0,\n                    }),", "                        size: 0,\n                        nlink: 1,\n                    }),")
text = text.replace("                        size: 1,\n                    }),", "                        size: 1,\n                        nlink: 2,\n                    }),")

marker = '''    #[test]
    fn incompatible_user_home_ranges_and_linger_are_conflicting() {
'''
test = '''    #[test]
    fn matching_user_fields_remain_unknown_when_group_lookup_is_unknown() {
        let group_command =
            getent_command("group", &account("project-runner")).expect("group command");
        let mut group = absent(group_command);
        group.status = Some(1);
        let passwd = success(
            getent_command("passwd", &account("project-runner")).expect("passwd command"),
            "project-runner:x:1001:1001::/var/lib/project-runner:/usr/sbin/nologin\n",
        );
        let executor = FakeExecutor::new(vec![group, passwd]);
        let report = observe_with(&desired(), &executor, &paths(), &matching_filesystem())
            .expect("unknown group observation");
        assert_eq!(
            report.observations.user.state(),
            PreparationObservationState::Unknown
        );
        assert!(report.identity().is_none());
    }

    #[test]
    fn incompatible_user_home_ranges_and_linger_are_conflicting() {
'''
text = replace_once(text, marker, test, "conflicting observation test marker")

path.write_text(text)
