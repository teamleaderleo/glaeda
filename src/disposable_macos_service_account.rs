//! Exact dedicated macOS service-account lifecycle delegated to Directory Services.
//!
//! SmolRunner does not edit account databases. It invokes the installed `/usr/bin/dscl`, observes
//! the resulting local records through the same mature service, and uses `/usr/bin/id` plus
//! `/usr/bin/pgrep` for the two kernel-facing checks. A plan-derived UUID is the ownership marker;
//! the mutable record name or numeric ID alone never authorizes adoption or deletion.

#![allow(
    dead_code,
    reason = "consumed by the in-progress production root action driver"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::Duration;

use serde::Serialize;

use crate::disposable_macos_service_installation::{
    DISPOSABLE_MACOS_SERVICE_ACCOUNT, DISPOSABLE_MACOS_SERVICE_GROUP,
    DisposableMacosServiceActionKind, DisposableMacosServicePlan,
};
use crate::disposable_macos_service_lifecycle::{
    DisposableMacosServiceActionConfirmation, DisposableMacosServiceActionDriver,
    DisposableMacosServiceActionRecovery,
};
use crate::process::{CommandSpec, ExecutionRecord, TimedCommandExecutor};

const DSCL: &str = "/usr/bin/dscl";
const ID: &str = "/usr/bin/id";
const PGREP: &str = "/usr/bin/pgrep";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_DIRECTORY_RECORDS: usize = 8_192;
const REAL_NAME: &str = "SmolRunner";
const HOME_DIRECTORY: &str = "/var/empty";
const LOGIN_SHELL: &str = "/usr/bin/false";
const DISABLED_PASSWORD: &str = "*";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DisposableMacosServiceAccountState {
    Absent,
    OwnedIncomplete,
    Exact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum DisposableMacosServiceAccountErrorKind {
    UnsafeState,
    Busy,
    CommandFailed,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct DisposableMacosServiceAccountError {
    kind: DisposableMacosServiceAccountErrorKind,
    code: &'static str,
}

impl DisposableMacosServiceAccountError {
    pub(crate) const fn kind(self) -> DisposableMacosServiceAccountErrorKind {
        self.kind
    }

    pub(crate) const fn code(self) -> &'static str {
        self.code
    }
}

impl fmt::Debug for DisposableMacosServiceAccountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DisposableMacosServiceAccountError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .finish()
    }
}

impl fmt::Display for DisposableMacosServiceAccountError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the dedicated macOS service account is unavailable")
    }
}

impl std::error::Error for DisposableMacosServiceAccountError {}

#[derive(Debug)]
struct RecordObservation {
    attributes: BTreeMap<&'static str, Option<String>>,
}

impl RecordObservation {
    fn exact(&self, expected: &[(&'static str, String)]) -> bool {
        expected.iter().all(|(key, value)| {
            self.attributes.get(key).and_then(Option::as_deref) == Some(value.as_str())
        })
    }
}

pub(crate) struct DisposableMacosServiceAccountActionDriver<'a, E> {
    executor: &'a E,
}

impl<'a, E> DisposableMacosServiceAccountActionDriver<'a, E> {
    pub(crate) const fn new(executor: &'a E) -> Self {
        Self { executor }
    }
}

impl<E: TimedCommandExecutor> DisposableMacosServiceActionDriver
    for DisposableMacosServiceAccountActionDriver<'_, E>
{
    type Prepared = DisposableMacosServiceActionKind;
    type Error = DisposableMacosServiceAccountError;

    fn recover(
        &mut self,
        plan: &DisposableMacosServicePlan,
        action: DisposableMacosServiceActionKind,
    ) -> Result<DisposableMacosServiceActionRecovery, Self::Error> {
        let state = observe_disposable_macos_service_account(plan, self.executor)?;
        match (action, state) {
            (
                DisposableMacosServiceActionKind::EnsureServiceAccount,
                DisposableMacosServiceAccountState::Exact,
            )
            | (
                DisposableMacosServiceActionKind::RemoveServiceAccount,
                DisposableMacosServiceAccountState::Absent,
            ) => Ok(DisposableMacosServiceActionRecovery::Completed),
            (
                DisposableMacosServiceActionKind::EnsureServiceAccount
                | DisposableMacosServiceActionKind::RemoveServiceAccount,
                DisposableMacosServiceAccountState::Absent
                | DisposableMacosServiceAccountState::OwnedIncomplete
                | DisposableMacosServiceAccountState::Exact,
            ) => Ok(DisposableMacosServiceActionRecovery::RetryAuthorized),
            _ => Err(wrong_action()),
        }
    }

    fn prepare(
        &mut self,
        plan: &DisposableMacosServicePlan,
        action: DisposableMacosServiceActionKind,
    ) -> Result<Self::Prepared, Self::Error> {
        if !matches!(
            action,
            DisposableMacosServiceActionKind::EnsureServiceAccount
                | DisposableMacosServiceActionKind::RemoveServiceAccount
        ) {
            return Err(wrong_action());
        }
        let _ = observe_disposable_macos_service_account(plan, self.executor)?;
        Ok(action)
    }

    fn execute(
        &mut self,
        plan: &DisposableMacosServicePlan,
        action: DisposableMacosServiceActionKind,
        prepared: Self::Prepared,
    ) -> Result<(), Self::Error> {
        if prepared != action {
            return Err(wrong_action());
        }
        match action {
            DisposableMacosServiceActionKind::EnsureServiceAccount => {
                ensure_disposable_macos_service_account(plan, self.executor)
            }
            DisposableMacosServiceActionKind::RemoveServiceAccount => {
                remove_disposable_macos_service_account(plan, self.executor)
            }
            _ => Err(wrong_action()),
        }
    }

    fn confirm_completed(
        &mut self,
        plan: &DisposableMacosServicePlan,
        action: DisposableMacosServiceActionKind,
    ) -> Result<DisposableMacosServiceActionConfirmation, Self::Error> {
        let state = observe_disposable_macos_service_account(plan, self.executor)?;
        let completed = matches!(
            (action, state),
            (
                DisposableMacosServiceActionKind::EnsureServiceAccount,
                DisposableMacosServiceAccountState::Exact
            ) | (
                DisposableMacosServiceActionKind::RemoveServiceAccount,
                DisposableMacosServiceAccountState::Absent
            )
        );
        Ok(if completed {
            DisposableMacosServiceActionConfirmation::Completed
        } else {
            DisposableMacosServiceActionConfirmation::Unknown
        })
    }
}

/// Observe the exact account/group ownership and isolation contract without mutation.
pub(crate) fn observe_disposable_macos_service_account(
    plan: &DisposableMacosServicePlan,
    executor: &impl TimedCommandExecutor,
) -> Result<DisposableMacosServiceAccountState, DisposableMacosServiceAccountError> {
    let parts = plan.apply_parts();
    let users = list_numeric_records(executor, "/Users", "UniqueID")?;
    let groups = list_numeric_records(executor, "/Groups", "PrimaryGroupID")?;
    require_unique_numeric_identity(&users, DISPOSABLE_MACOS_SERVICE_ACCOUNT, parts.service_uid)?;
    require_unique_numeric_identity(
        &groups,
        DISPOSABLE_MACOS_SERVICE_GROUP,
        parts.primary_group_id,
    )?;
    require_no_supplementary_membership(plan, executor)?;

    let user = if users.contains_key(DISPOSABLE_MACOS_SERVICE_ACCOUNT) {
        Some(observe_record(
            executor,
            &format!("/Users/{DISPOSABLE_MACOS_SERVICE_ACCOUNT}"),
            &user_attributes(plan),
        )?)
    } else {
        None
    };
    let group = if groups.contains_key(DISPOSABLE_MACOS_SERVICE_GROUP) {
        Some(observe_record(
            executor,
            &format!("/Groups/{DISPOSABLE_MACOS_SERVICE_GROUP}"),
            &group_attributes(plan),
        )?)
    } else {
        None
    };

    match (&user, &group) {
        (None, None) => Ok(DisposableMacosServiceAccountState::Absent),
        _ => {
            require_owned_record(user.as_ref(), &user_attributes(plan), "GeneratedUID")?;
            require_owned_record(group.as_ref(), &group_attributes(plan), "GeneratedUID")?;
            let exact = user
                .as_ref()
                .is_some_and(|record| record.exact(&user_attributes(plan)))
                && group
                    .as_ref()
                    .is_some_and(|record| record.exact(&group_attributes(plan)));
            if exact {
                require_only_primary_group(executor, parts.primary_group_id)?;
                Ok(DisposableMacosServiceAccountState::Exact)
            } else {
                Ok(DisposableMacosServiceAccountState::OwnedIncomplete)
            }
        }
    }
}

/// Create or finish the exact dedicated account using idempotent Directory Services attributes.
///
/// Each record is first created with its plan-derived UUID in the same `dscl -create` operation.
/// Recovery may resume only when every present record retains that UUID and every present policy
/// attribute is still exact.
pub(crate) fn ensure_disposable_macos_service_account(
    plan: &DisposableMacosServicePlan,
    executor: &impl TimedCommandExecutor,
) -> Result<(), DisposableMacosServiceAccountError> {
    if observe_disposable_macos_service_account(plan, executor)?
        == DisposableMacosServiceAccountState::Exact
    {
        return Ok(());
    }
    for (record, attributes) in [
        (
            format!("/Groups/{DISPOSABLE_MACOS_SERVICE_GROUP}"),
            group_attributes(plan),
        ),
        (
            format!("/Users/{DISPOSABLE_MACOS_SERVICE_ACCOUNT}"),
            user_attributes(plan),
        ),
    ] {
        for (key, value) in attributes {
            require_owned_or_absent(plan, executor)?;
            run_mutation(
                executor,
                CommandSpec::new(DSCL)
                    .argument(".")
                    .argument("-create")
                    .argument(&record)
                    .argument(key)
                    .argument(value),
            )?;
            require_owned_or_absent(plan, executor)?;
        }
    }
    if observe_disposable_macos_service_account(plan, executor)?
        != DisposableMacosServiceAccountState::Exact
    {
        return Err(unsafe_state());
    }
    Ok(())
}

/// Remove only exact UUID-owned records after all service-UID processes are gone.
pub(crate) fn remove_disposable_macos_service_account(
    plan: &DisposableMacosServicePlan,
    executor: &impl TimedCommandExecutor,
) -> Result<(), DisposableMacosServiceAccountError> {
    let state = observe_disposable_macos_service_account(plan, executor)?;
    if state == DisposableMacosServiceAccountState::Absent {
        return Ok(());
    }
    require_no_service_processes(plan, executor)?;
    for record in [
        format!("/Users/{DISPOSABLE_MACOS_SERVICE_ACCOUNT}"),
        format!("/Groups/{DISPOSABLE_MACOS_SERVICE_GROUP}"),
    ] {
        let before = observe_disposable_macos_service_account(plan, executor)?;
        if before == DisposableMacosServiceAccountState::Absent {
            break;
        }
        let named_present = if record.starts_with("/Users/") {
            list_numeric_records(executor, "/Users", "UniqueID")?
                .contains_key(DISPOSABLE_MACOS_SERVICE_ACCOUNT)
        } else {
            list_numeric_records(executor, "/Groups", "PrimaryGroupID")?
                .contains_key(DISPOSABLE_MACOS_SERVICE_GROUP)
        };
        if !named_present {
            continue;
        }
        run_mutation(
            executor,
            CommandSpec::new(DSCL)
                .argument(".")
                .argument("-delete")
                .argument(record),
        )?;
    }
    if observe_disposable_macos_service_account(plan, executor)?
        != DisposableMacosServiceAccountState::Absent
    {
        return Err(unsafe_state());
    }
    Ok(())
}

fn require_owned_or_absent(
    plan: &DisposableMacosServicePlan,
    executor: &impl TimedCommandExecutor,
) -> Result<(), DisposableMacosServiceAccountError> {
    match observe_disposable_macos_service_account(plan, executor)? {
        DisposableMacosServiceAccountState::Absent
        | DisposableMacosServiceAccountState::OwnedIncomplete
        | DisposableMacosServiceAccountState::Exact => Ok(()),
    }
}

fn user_attributes(plan: &DisposableMacosServicePlan) -> Vec<(&'static str, String)> {
    let parts = plan.apply_parts();
    vec![
        ("GeneratedUID", parts.service_user_uuid.to_owned()),
        ("RecordName", DISPOSABLE_MACOS_SERVICE_ACCOUNT.to_owned()),
        ("UniqueID", parts.service_uid.to_string()),
        ("PrimaryGroupID", parts.primary_group_id.to_string()),
        ("NFSHomeDirectory", HOME_DIRECTORY.to_owned()),
        ("UserShell", LOGIN_SHELL.to_owned()),
        ("RealName", REAL_NAME.to_owned()),
        ("IsHidden", "1".to_owned()),
        ("Password", DISABLED_PASSWORD.to_owned()),
    ]
}

fn group_attributes(plan: &DisposableMacosServicePlan) -> Vec<(&'static str, String)> {
    let parts = plan.apply_parts();
    vec![
        ("GeneratedUID", parts.service_group_uuid.to_owned()),
        ("RecordName", DISPOSABLE_MACOS_SERVICE_GROUP.to_owned()),
        ("PrimaryGroupID", parts.primary_group_id.to_string()),
        ("RealName", REAL_NAME.to_owned()),
        ("Password", DISABLED_PASSWORD.to_owned()),
    ]
}

fn list_numeric_records(
    executor: &impl TimedCommandExecutor,
    path: &str,
    attribute: &str,
) -> Result<BTreeMap<String, Option<u32>>, DisposableMacosServiceAccountError> {
    let record = run_success(
        executor,
        CommandSpec::new(DSCL)
            .argument("-url")
            .argument(".")
            .argument("-list")
            .argument(path)
            .argument(attribute),
    )?;
    let mut parsed = BTreeMap::new();
    for line in record.stdout.lines() {
        let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
        let (name, value) = match fields.as_slice() {
            [name] => ((*name).to_owned(), None),
            [name, value] => (
                (*name).to_owned(),
                Some(value.parse::<u32>().map_err(|_| unsafe_state())?),
            ),
            _ => return Err(unsafe_state()),
        };
        if name.is_empty()
            || name.chars().any(char::is_control)
            || parsed.insert(name, value).is_some()
            || parsed.len() > MAX_DIRECTORY_RECORDS
        {
            return Err(unsafe_state());
        }
    }
    Ok(parsed)
}

fn require_unique_numeric_identity(
    records: &BTreeMap<String, Option<u32>>,
    expected_name: &str,
    expected_id: u32,
) -> Result<(), DisposableMacosServiceAccountError> {
    if records
        .get(expected_name)
        .is_some_and(|value| value.is_some_and(|value| value != expected_id))
        || records
            .iter()
            .any(|(name, value)| name != expected_name && *value == Some(expected_id))
    {
        return Err(unsafe_state());
    }
    Ok(())
}

fn observe_record(
    executor: &impl TimedCommandExecutor,
    path: &str,
    expected: &[(&'static str, String)],
) -> Result<RecordObservation, DisposableMacosServiceAccountError> {
    let mut attributes = BTreeMap::new();
    for (key, value) in expected {
        let observed = read_attribute(executor, path, key)?;
        if observed
            .as_deref()
            .is_some_and(|observed| observed != value)
        {
            return Err(unsafe_state());
        }
        attributes.insert(*key, observed);
    }
    Ok(RecordObservation { attributes })
}

fn read_attribute(
    executor: &impl TimedCommandExecutor,
    path: &str,
    key: &'static str,
) -> Result<Option<String>, DisposableMacosServiceAccountError> {
    let record = run_success(
        executor,
        CommandSpec::new(DSCL)
            .argument("-url")
            .argument(".")
            .argument("-read")
            .argument(path)
            .argument(key),
    )?;
    if record.stdout == format!("No such key: {key}\n") {
        return Ok(None);
    }
    let prefix = format!("{key}: ");
    let value = record
        .stdout
        .strip_prefix(&prefix)
        .and_then(|value| value.strip_suffix('\n'))
        .ok_or_else(unsafe_state)?;
    if value.is_empty() || value.chars().any(char::is_control) || value.contains('\n') {
        return Err(unsafe_state());
    }
    Ok(Some(value.to_owned()))
}

fn require_owned_record(
    record: Option<&RecordObservation>,
    expected: &[(&'static str, String)],
    ownership_key: &'static str,
) -> Result<(), DisposableMacosServiceAccountError> {
    let Some(record) = record else {
        return Ok(());
    };
    let expected = expected
        .iter()
        .find_map(|(key, value)| (*key == ownership_key).then_some(value.as_str()))
        .ok_or_else(unsafe_state)?;
    if record
        .attributes
        .get(ownership_key)
        .and_then(Option::as_deref)
        != Some(expected)
    {
        return Err(unsafe_state());
    }
    Ok(())
}

fn require_no_supplementary_membership(
    plan: &DisposableMacosServicePlan,
    executor: &impl TimedCommandExecutor,
) -> Result<(), DisposableMacosServiceAccountError> {
    for (attribute, forbidden) in [
        ("GroupMembership", DISPOSABLE_MACOS_SERVICE_ACCOUNT),
        ("GroupMembers", plan.apply_parts().service_user_uuid),
        ("NestedGroups", plan.apply_parts().service_group_uuid),
    ] {
        let record = run_success(
            executor,
            CommandSpec::new(DSCL)
                .argument("-url")
                .argument(".")
                .argument("-list")
                .argument("/Groups")
                .argument(attribute),
        )?;
        let mut groups = BTreeSet::new();
        for line in record.stdout.lines() {
            let mut fields = line.split_ascii_whitespace();
            let group = fields.next().ok_or_else(unsafe_state)?;
            if !groups.insert(group) || groups.len() > MAX_DIRECTORY_RECORDS {
                return Err(unsafe_state());
            }
            if fields.any(|member| member == forbidden) {
                return Err(unsafe_state());
            }
        }
    }
    Ok(())
}

fn require_only_primary_group(
    executor: &impl TimedCommandExecutor,
    primary_group_id: u32,
) -> Result<(), DisposableMacosServiceAccountError> {
    let record = run_success(
        executor,
        CommandSpec::new(ID)
            .argument("-G")
            .argument(DISPOSABLE_MACOS_SERVICE_ACCOUNT),
    )?;
    if record.stdout != format!("{primary_group_id}\n") {
        return Err(unsafe_state());
    }
    Ok(())
}

fn require_no_service_processes(
    plan: &DisposableMacosServicePlan,
    executor: &impl TimedCommandExecutor,
) -> Result<(), DisposableMacosServiceAccountError> {
    let record = executor
        .execute_with_timeout(
            &CommandSpec::new(PGREP)
                .argument("-U")
                .argument(plan.apply_parts().service_uid.to_string()),
            COMMAND_TIMEOUT,
        )
        .map_err(|_| command_failed())?;
    if record.status == Some(1) && record.stdout.is_empty() && record.stderr.is_empty() {
        return Ok(());
    }
    if record.success {
        return Err(lifecycle_error(
            DisposableMacosServiceAccountErrorKind::Busy,
            "disposable_macos_service_account_processes_active",
        ));
    }
    Err(command_failed())
}

fn run_success(
    executor: &impl TimedCommandExecutor,
    spec: CommandSpec,
) -> Result<ExecutionRecord, DisposableMacosServiceAccountError> {
    let record = executor
        .execute_with_timeout(&spec, COMMAND_TIMEOUT)
        .map_err(|_| command_failed())?;
    if !record.success || record.status != Some(0) || !record.stderr.is_empty() {
        return Err(command_failed());
    }
    Ok(record)
}

fn run_mutation(
    executor: &impl TimedCommandExecutor,
    spec: CommandSpec,
) -> Result<(), DisposableMacosServiceAccountError> {
    let record = run_success(executor, spec)?;
    if !record.stdout.is_empty() {
        return Err(command_failed());
    }
    Ok(())
}

const fn lifecycle_error(
    kind: DisposableMacosServiceAccountErrorKind,
    code: &'static str,
) -> DisposableMacosServiceAccountError {
    DisposableMacosServiceAccountError { kind, code }
}

const fn unsafe_state() -> DisposableMacosServiceAccountError {
    lifecycle_error(
        DisposableMacosServiceAccountErrorKind::UnsafeState,
        "disposable_macos_service_account_unsafe_state",
    )
}

const fn command_failed() -> DisposableMacosServiceAccountError {
    lifecycle_error(
        DisposableMacosServiceAccountErrorKind::CommandFailed,
        "disposable_macos_service_account_command_failed",
    )
}

const fn wrong_action() -> DisposableMacosServiceAccountError {
    lifecycle_error(
        DisposableMacosServiceAccountErrorKind::UnsafeState,
        "disposable_macos_service_account_action_invalid",
    )
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Mutex;

    use crate::artifact::Sha256Digest;
    use crate::disposable_macos_service_installation::{
        DisposableMacosServiceDesiredState, plan_disposable_macos_service,
    };
    use crate::process::{CommandExecutor, CommandValue};
    use crate::state::InstallationId;

    use super::*;

    const DIGEST_A: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str =
        "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const NETWORK_DIGEST: &str =
        "sha256:65ceec8974086e378f216acc555724cb40b08ccc047391dedd0b6f17df72587e";

    #[derive(Default)]
    struct FakeState {
        users: BTreeMap<String, BTreeMap<String, String>>,
        groups: BTreeMap<String, BTreeMap<String, String>>,
        supplementary: BTreeMap<String, Vec<String>>,
        active_processes: bool,
        mutation_calls: usize,
    }

    #[derive(Default)]
    struct FakeDirectoryService(Mutex<FakeState>);

    impl FakeDirectoryService {
        fn record(
            status: i32,
            stdout: impl Into<String>,
            stderr: impl Into<String>,
        ) -> ExecutionRecord {
            ExecutionRecord {
                argv: Vec::new(),
                environment_keys: Vec::new(),
                status: Some(status),
                success: status == 0,
                stdout: stdout.into(),
                stderr: stderr.into(),
            }
        }

        fn execute_inner(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            assert!(spec.environment.is_empty());
            let argv = spec
                .arguments
                .iter()
                .map(|value| match value {
                    CommandValue::Plain(value) => value.as_str(),
                    CommandValue::Secret(_) => panic!("account command must not contain secrets"),
                })
                .collect::<Vec<_>>();
            let mut state = self.0.lock().unwrap();
            match (spec.program.to_str().unwrap(), argv.as_slice()) {
                (DSCL, ["-url", ".", "-list", path, attribute]) => {
                    if *path == "/Groups" && *attribute == "GroupMembership" {
                        let mut output = String::new();
                        for name in state.groups.keys() {
                            output.push_str(name);
                            if let Some(members) = state.supplementary.get(name) {
                                for member in members {
                                    output.push(' ');
                                    output.push_str(member);
                                }
                            }
                            output.push('\n');
                        }
                        return Ok(Self::record(0, output, ""));
                    }
                    let records = if *path == "/Users" {
                        &state.users
                    } else {
                        &state.groups
                    };
                    let mut output = String::new();
                    for (name, attributes) in records {
                        output.push_str(name);
                        if let Some(value) = attributes.get(*attribute) {
                            output.push(' ');
                            output.push_str(value);
                        }
                        output.push('\n');
                    }
                    Ok(Self::record(0, output, ""))
                }
                (DSCL, ["-url", ".", "-read", path, key]) => {
                    let (records, name) = if let Some(name) = path.strip_prefix("/Users/") {
                        (&state.users, name)
                    } else {
                        (&state.groups, path.strip_prefix("/Groups/").unwrap())
                    };
                    let attributes = records.get(name).unwrap();
                    let output = attributes.get(*key).map_or_else(
                        || format!("No such key: {key}\n"),
                        |value| format!("{key}: {value}\n"),
                    );
                    Ok(Self::record(0, output, ""))
                }
                (DSCL, [".", "-create", path, key, value]) => {
                    state.mutation_calls += 1;
                    let records = if path.starts_with("/Users/") {
                        &mut state.users
                    } else {
                        &mut state.groups
                    };
                    let name = path.rsplit('/').next().unwrap();
                    records
                        .entry(name.to_owned())
                        .or_default()
                        .insert((*key).to_owned(), (*value).to_owned());
                    Ok(Self::record(0, "", ""))
                }
                (DSCL, [".", "-delete", path]) => {
                    state.mutation_calls += 1;
                    let name = path.rsplit('/').next().unwrap();
                    if path.starts_with("/Users/") {
                        state.users.remove(name);
                    } else {
                        state.groups.remove(name);
                    }
                    Ok(Self::record(0, "", ""))
                }
                (ID, ["-G", account]) => {
                    assert_eq!(*account, DISPOSABLE_MACOS_SERVICE_ACCOUNT);
                    let gid = state
                        .users
                        .get(*account)
                        .and_then(|attributes| attributes.get("PrimaryGroupID"))
                        .cloned()
                        .unwrap_or_default();
                    Ok(Self::record(0, format!("{gid}\n"), ""))
                }
                (PGREP, ["-U", _]) if state.active_processes => Ok(Self::record(0, "123\n", "")),
                (PGREP, ["-U", _]) => Ok(Self::record(1, "", "")),
                _ => panic!("unexpected command: {:?}", spec.displayed_argv()),
            }
        }
    }

    impl CommandExecutor for FakeDirectoryService {
        fn execute(&self, spec: &CommandSpec) -> io::Result<ExecutionRecord> {
            self.execute_inner(spec)
        }
    }

    impl TimedCommandExecutor for FakeDirectoryService {
        fn execute_with_timeout(
            &self,
            spec: &CommandSpec,
            timeout: Duration,
        ) -> io::Result<ExecutionRecord> {
            assert_eq!(timeout, COMMAND_TIMEOUT);
            self.execute_inner(spec)
        }
    }

    fn plan() -> DisposableMacosServicePlan {
        let enrollment = format!(
            concat!(
                "{{\n",
                "  \"schema_version\": 2,\n",
                "  \"state_root\": \"/private/var/lib/smolrunner\",\n",
                "  \"network\": {{\n",
                "    \"backend\": \"macos_pf_dedicated_uid\",\n",
                "    \"service_uid\": 502,\n",
                "    \"policy_identity\": \"{NETWORK_DIGEST}\"\n",
                "  }},\n",
                "  \"lima\": {{\n",
                "    \"program\": \"/opt/homebrew/bin/limactl\",\n",
                "    \"home\": \"/private/var/lib/smolrunner/lima\",\n",
                "    \"source_instance\": \"smolrunner-prepared-template\"\n",
                "  }},\n",
                "  \"bridge\": {{\n",
                "    \"program_digest\": \"{DIGEST_B}\"\n",
                "  }},\n",
                "  \"github\": {{\n",
                "    \"config_url\": \"https://github.com/acme\",\n",
                "    \"client_id\": \"Iv1.0123456789abcdef\",\n",
                "    \"installation_id\": 42,\n",
                "    \"keychain_service\": \"smolrunner.github-app\",\n",
                "    \"keychain_account\": \"acme-ci\"\n",
                "  }},\n",
                "  \"scale_set\": {{\n",
                "    \"id\": 17,\n",
                "    \"name\": \"smolrunner-disposable\",\n",
                "    \"runner_group_id\": 3,\n",
                "    \"owner\": \"acme\",\n",
                "    \"repository\": \"widgets\",\n",
                "    \"labels\": [\n",
                "      \"self-hosted\",\n",
                "      \"smolrunner\"\n",
                "    ]\n",
                "  }},\n",
                "  \"resources\": {{\n",
                "    \"cpu_millis\": 2000,\n",
                "    \"memory_bytes\": 2147483648,\n",
                "    \"disk_bytes\": 21474836480\n",
                "  }}\n",
                "}}\n"
            ),
            NETWORK_DIGEST = NETWORK_DIGEST,
            DIGEST_B = DIGEST_B,
        )
        .into_bytes();
        plan_disposable_macos_service(
            DisposableMacosServiceDesiredState::Installed,
            &InstallationId::parse("smolrunner-install-0001").unwrap(),
            std::path::Path::new("/opt/operator/smolrunner"),
            &Sha256Digest::parse(DIGEST_A).unwrap(),
            std::path::Path::new("/opt/operator/scaleset-bridge"),
            &Sha256Digest::parse(DIGEST_B).unwrap(),
            std::path::Path::new("/opt/operator/enrollment.json"),
            &enrollment,
        )
        .unwrap()
    }

    #[test]
    fn exact_account_is_created_observed_and_removed_through_fixed_tools() {
        let plan = plan();
        let service = FakeDirectoryService::default();
        assert_eq!(
            observe_disposable_macos_service_account(&plan, &service).unwrap(),
            DisposableMacosServiceAccountState::Absent
        );
        ensure_disposable_macos_service_account(&plan, &service).unwrap();
        assert_eq!(
            observe_disposable_macos_service_account(&plan, &service).unwrap(),
            DisposableMacosServiceAccountState::Exact
        );
        remove_disposable_macos_service_account(&plan, &service).unwrap();
        assert_eq!(
            observe_disposable_macos_service_account(&plan, &service).unwrap(),
            DisposableMacosServiceAccountState::Absent
        );
    }

    #[test]
    fn matching_name_or_numeric_id_without_owned_uuid_is_refused_without_mutation() {
        let plan = plan();
        let service = FakeDirectoryService::default();
        service.0.lock().unwrap().users.insert(
            DISPOSABLE_MACOS_SERVICE_ACCOUNT.to_owned(),
            BTreeMap::from([
                ("GeneratedUID".to_owned(), "FOREIGN".to_owned()),
                ("UniqueID".to_owned(), "502".to_owned()),
            ]),
        );
        assert_eq!(
            ensure_disposable_macos_service_account(&plan, &service)
                .unwrap_err()
                .kind(),
            DisposableMacosServiceAccountErrorKind::UnsafeState
        );
        assert_eq!(service.0.lock().unwrap().mutation_calls, 0);
    }

    #[test]
    fn supplementary_membership_and_active_processes_block_destructive_cleanup() {
        let plan = plan();
        let service = FakeDirectoryService::default();
        ensure_disposable_macos_service_account(&plan, &service).unwrap();
        service.0.lock().unwrap().active_processes = true;
        assert_eq!(
            remove_disposable_macos_service_account(&plan, &service)
                .unwrap_err()
                .kind(),
            DisposableMacosServiceAccountErrorKind::Busy
        );
        service.0.lock().unwrap().active_processes = false;
        service.0.lock().unwrap().groups.insert(
            "admin".to_owned(),
            BTreeMap::from([("PrimaryGroupID".to_owned(), "80".to_owned())]),
        );
        service.0.lock().unwrap().supplementary.insert(
            "admin".to_owned(),
            vec![DISPOSABLE_MACOS_SERVICE_ACCOUNT.to_owned()],
        );
        assert_eq!(
            remove_disposable_macos_service_account(&plan, &service)
                .unwrap_err()
                .kind(),
            DisposableMacosServiceAccountErrorKind::UnsafeState
        );
    }

    #[test]
    fn lifecycle_driver_retries_only_owned_account_states() {
        let plan = plan();
        let service = FakeDirectoryService::default();
        let mut driver = DisposableMacosServiceAccountActionDriver::new(&service);
        assert_eq!(
            driver
                .recover(
                    &plan,
                    DisposableMacosServiceActionKind::EnsureServiceAccount
                )
                .unwrap(),
            DisposableMacosServiceActionRecovery::RetryAuthorized
        );
        let prepared = driver
            .prepare(
                &plan,
                DisposableMacosServiceActionKind::EnsureServiceAccount,
            )
            .unwrap();
        driver
            .execute(
                &plan,
                DisposableMacosServiceActionKind::EnsureServiceAccount,
                prepared,
            )
            .unwrap();
        assert_eq!(
            driver
                .confirm_completed(
                    &plan,
                    DisposableMacosServiceActionKind::EnsureServiceAccount
                )
                .unwrap(),
            DisposableMacosServiceActionConfirmation::Completed
        );
        assert_eq!(
            driver
                .recover(
                    &plan,
                    DisposableMacosServiceActionKind::EnsureServiceAccount
                )
                .unwrap(),
            DisposableMacosServiceActionRecovery::Completed
        );
        assert_eq!(
            driver
                .prepare(&plan, DisposableMacosServiceActionKind::PublishExecutables)
                .unwrap_err()
                .code(),
            "disposable_macos_service_account_action_invalid"
        );
    }
}
