from pathlib import Path


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if text.count(old) != 1:
        raise SystemExit(f"expected {label} once, found {text.count(old)}")
    return text.replace(old, new, 1)


path = Path("src/runner_account_observation.rs")
text = path.read_text()
text = replace_once(
    text,
    "use crate::runner_user::{\n    PasswdRecord, SubordinateRange, parse_passwd_record, parse_subordinate_ranges,\n};\n",
    "use crate::runner_user::{PasswdRecord, parse_passwd_record};\n",
    "runner_user imports",
)

text = replace_once(
    text,
    "    if gid == 0 || &name != desired || !valid_group_members(fields[3]) {\n",
    "    if gid == 0 || &name != desired || !fields[3].is_empty() {\n",
    "dedicated group member validation",
)
text = replace_once(
    text,
    '''fn valid_group_members(value: &str) -> bool {
    value.is_empty()
        || value
            .split(',')
            .all(|member| LinuxAccountName::parse(member).is_ok())
}

''',
    "",
    "group members helper",
)

old_present = '''        Lookup::Present(_) => match parsed {
            Some(record) => (
                PreparationObservationState::Matching,
                format!(
                    "getent group {} returned canonical GID {}",
                    record.name.as_str(),
                    record.gid
                ),
            ),
            None => (
                PreparationObservationState::Unknown,
                format!(
                    "getent group {} returned malformed or unsafe data",
                    desired.as_str()
                ),
            ),
        },
'''
new_present = '''        Lookup::Present(input) => match parsed {
            Some(record) => (
                PreparationObservationState::Matching,
                format!(
                    "getent group {} returned canonical dedicated GID {}",
                    record.name.as_str(),
                    record.gid
                ),
            ),
            None if group_record_is_well_formed(input) => (
                PreparationObservationState::Conflicting,
                format!(
                    "getent group {} returned an incompatible name, GID, or member list",
                    desired.as_str()
                ),
            ),
            None => (
                PreparationObservationState::Unknown,
                format!(
                    "getent group {} returned malformed or unsafe data",
                    desired.as_str()
                ),
            ),
        },
'''
text = replace_once(text, old_present, new_present, "group present classification")

marker = '''fn classify_group(
'''
helper = '''fn group_record_is_well_formed(input: &str) -> bool {
    let lines = input.lines().filter(|line| !line.is_empty()).collect::<Vec<_>>();
    if lines.len() != 1 {
        return false;
    }
    let fields = lines[0].split(':').collect::<Vec<_>>();
    fields.len() == 4
        && LinuxAccountName::parse(fields[0]).is_ok()
        && canonical_u32(fields[2]).is_some()
        && fields[3]
            .split(',')
            .filter(|member| !member.is_empty())
            .all(|member| LinuxAccountName::parse(member).is_ok())
}

fn classify_group(
'''
text = replace_once(text, marker, helper, "classify_group marker")

start = text.index("fn classify_subordinate(\n")
end = text.index("\nfn classify_linger(\n", start)
replacement = r'''fn classify_subordinate(
    file: TrustedFile,
    username: &LinuxAccountName,
    desired: PlannedSubordinateRange,
    user_matching: bool,
    label: &str,
) -> Result<PreparationObservation, RunnerAccountObservationError> {
    let (state, evidence) = match file {
        TrustedFile::Missing | TrustedFile::Unknown => (
            PreparationObservationState::Unknown,
            format!("subordinate {label} authority could not be read safely"),
        ),
        TrustedFile::Present(input) => match inspect_subordinate_authority(&input, username, desired)
        {
            AuthorityResult::Malformed => (
                PreparationObservationState::Unknown,
                format!("subordinate {label} authority contains malformed data"),
            ),
            AuthorityResult::Absent => (
                PreparationObservationState::Absent,
                format!(
                    "no subordinate {label} range is assigned to {} and the desired allocation does not overlap another owner",
                    username.as_str()
                ),
            ),
            AuthorityResult::Exact if user_matching => (
                PreparationObservationState::Matching,
                format!(
                    "subordinate {label} range {}-{} exactly matches the desired allocation without cross-owner overlap",
                    desired.start(),
                    desired.end_inclusive()
                ),
            ),
            AuthorityResult::Exact | AuthorityResult::Conflicting => (
                PreparationObservationState::Conflicting,
                format!(
                    "subordinate {label} authority conflicts with the desired single allocation for {}",
                    username.as_str()
                ),
            ),
        },
    };
    Ok(PreparationObservation::new(state, [evidence])?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthorityResult {
    Absent,
    Exact,
    Conflicting,
    Malformed,
}

fn inspect_subordinate_authority(
    input: &str,
    username: &LinuxAccountName,
    desired: PlannedSubordinateRange,
) -> AuthorityResult {
    let desired_start = u64::from(desired.start());
    let desired_end = desired_start + u64::from(desired.count());
    let mut owned_ranges = Vec::new();
    let mut foreign_overlap = false;

    for line in input.lines().filter(|line| !line.is_empty()) {
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() != 3 || !valid_subordinate_owner(fields[0]) {
            return AuthorityResult::Malformed;
        }
        let Some(start) = canonical_u32(fields[1]) else {
            return AuthorityResult::Malformed;
        };
        let Some(count) = canonical_u32(fields[2]) else {
            return AuthorityResult::Malformed;
        };
        if count == 0 {
            return AuthorityResult::Malformed;
        }
        let end = u64::from(start) + u64::from(count);
        if end > u64::from(u32::MAX) + 1 {
            return AuthorityResult::Malformed;
        }

        if fields[0] == username.as_str() {
            owned_ranges.push((start, count));
        } else if u64::from(start) < desired_end && desired_start < end {
            foreign_overlap = true;
        }
    }

    if foreign_overlap {
        AuthorityResult::Conflicting
    } else if owned_ranges.is_empty() {
        AuthorityResult::Absent
    } else if owned_ranges == [(desired.start(), desired.count())] {
        AuthorityResult::Exact
    } else {
        AuthorityResult::Conflicting
    }
}

fn valid_subordinate_owner(value: &str) -> bool {
    LinuxAccountName::parse(value).is_ok()
        || canonical_u32(value).is_some_and(|identifier| identifier > 0)
}
'''
text = text[:start] + replacement + text[end:]

marker = '''    #[test]
    fn matching_user_fields_remain_unknown_when_group_lookup_is_unknown() {
'''
tests = r'''    #[test]
    fn supplementary_group_members_are_conflicting() {
        let group = success(
            getent_command("group", &account("project-runner")).expect("group command"),
            "project-runner:x:1001:other-user\n",
        );
        let passwd = success(
            getent_command("passwd", &account("project-runner")).expect("passwd command"),
            "project-runner:x:1001:1001::/var/lib/project-runner:/usr/sbin/nologin\n",
        );
        let report = observe_with(
            &desired(),
            &FakeExecutor::new(vec![group, passwd]),
            &paths(),
            &matching_filesystem(),
        )
        .expect("conflicting group observation");
        assert_eq!(
            report.observations.group.state(),
            PreparationObservationState::Conflicting
        );
        assert_eq!(
            report.observations.user.state(),
            PreparationObservationState::Conflicting
        );
    }

    #[test]
    fn matching_user_fields_remain_unknown_when_group_lookup_is_unknown() {
'''
text = replace_once(text, marker, tests, "unknown group test marker")

marker = '''    #[test]
    fn incompatible_user_home_ranges_and_linger_are_conflicting() {
'''
tests = r'''    #[test]
    fn foreign_overlapping_subordinate_range_is_conflicting() {
        let mut filesystem = matching_filesystem();
        filesystem.files.insert(
            "/test/subuid".into(),
            TrustedFile::Present("other-user:90000:65536\n".to_owned()),
        );
        let report = observe_with(&desired(), &matching_executor(), &paths(), &filesystem)
            .expect("overlap observation");
        assert_eq!(
            report.observations.subordinate_uids.state(),
            PreparationObservationState::Conflicting
        );
    }

    #[test]
    fn malformed_unrelated_subordinate_entry_keeps_authority_unknown() {
        let mut filesystem = matching_filesystem();
        filesystem.files.insert(
            "/test/subuid".into(),
            TrustedFile::Present("other-user:not-a-number:65536\n".to_owned()),
        );
        let report = observe_with(&desired(), &matching_executor(), &paths(), &filesystem)
            .expect("malformed authority observation");
        assert_eq!(
            report.observations.subordinate_uids.state(),
            PreparationObservationState::Unknown
        );
    }

    #[test]
    fn incompatible_user_home_ranges_and_linger_are_conflicting() {
'''
text = replace_once(text, marker, tests, "conflicting state test marker")

path.write_text(text)
