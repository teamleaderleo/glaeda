from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    file = Path(path)
    text = file.read_text()
    count = text.count(old)
    if count != 1:
        raise RuntimeError(
            f"expected exactly one match in {path}, found {count}: {old[:100]!r}"
        )
    file.write_text(text.replace(old, new, 1))


replace_once(
    "src/lib.rs",
    "pub mod doctor;\n",
    "pub mod doctor;\npub mod durable_journal;\n",
)

replace_once(
    "src/journal.rs",
    """pub enum ActionOutcome {
    Pending,
    Completed,
    Failed,
    Skipped,
    RolledBack,
    Compensated,
    RollbackFailed,
}""",
    """pub enum ActionOutcome {
    Pending,
    Executing,
    Completed,
    Failed,
    Skipped,
    RollbackInProgress,
    RolledBack,
    Compensated,
    RollbackFailed,
}""",
)

replace_once(
    "src/journal_document.rs",
    "    validate_stop_position(journal, &mut problems);\n",
    "    validate_stop_position(journal, &mut problems);\n    validate_recovery_position(journal, &mut problems);\n",
)

replace_once(
    "src/journal_document.rs",
    """enum WireActionOutcome {
    Pending,
    Completed,
    Failed,
    Skipped,
    RolledBack,
    Compensated,
    RollbackFailed,
}""",
    """enum WireActionOutcome {
    Pending,
    Executing,
    Completed,
    Failed,
    Skipped,
    RollbackInProgress,
    RolledBack,
    Compensated,
    RollbackFailed,
}""",
)

replace_once(
    "src/journal_document.rs",
    """        match wire {
            WireActionOutcome::Pending => Self::Pending,
            WireActionOutcome::Completed => Self::Completed,
            WireActionOutcome::Failed => Self::Failed,
            WireActionOutcome::Skipped => Self::Skipped,
            WireActionOutcome::RolledBack => Self::RolledBack,
            WireActionOutcome::Compensated => Self::Compensated,
            WireActionOutcome::RollbackFailed => Self::RollbackFailed,
        }""",
    """        match wire {
            WireActionOutcome::Pending => Self::Pending,
            WireActionOutcome::Executing => Self::Executing,
            WireActionOutcome::Completed => Self::Completed,
            WireActionOutcome::Failed => Self::Failed,
            WireActionOutcome::Skipped => Self::Skipped,
            WireActionOutcome::RollbackInProgress => Self::RollbackInProgress,
            WireActionOutcome::RolledBack => Self::RolledBack,
            WireActionOutcome::Compensated => Self::Compensated,
            WireActionOutcome::RollbackFailed => Self::RollbackFailed,
        }""",
)

replace_once(
    "src/journal_document.rs",
    """        ActionOutcome::Pending => {
            if record.message.is_some() {
                problems.push(format!("{prefix}.message must be absent while pending"));
            }
        }
        ActionOutcome::Completed
        | ActionOutcome::Failed
        | ActionOutcome::Skipped
        | ActionOutcome::RolledBack
        | ActionOutcome::Compensated
        | ActionOutcome::RollbackFailed => match record.message.as_deref() {""",
    """        ActionOutcome::Pending => {
            if record.message.is_some() {
                problems.push(format!("{prefix}.message must be absent while pending"));
            }
        }
        ActionOutcome::Executing => {
            if record.message.is_some() {
                problems.push(format!("{prefix}.message must be absent while executing"));
            }
        }
        ActionOutcome::Completed
        | ActionOutcome::Failed
        | ActionOutcome::Skipped
        | ActionOutcome::RollbackInProgress
        | ActionOutcome::RolledBack
        | ActionOutcome::Compensated
        | ActionOutcome::RollbackFailed => match record.message.as_deref() {""",
)

replace_once(
    "src/journal_document.rs",
    """        ActionOutcome::RollbackFailed if record.action.rollback == RollbackClass::Irreversible => {
            problems.push(format!(
                "{prefix} cannot report rollback_failed for an irreversible action"
            ));
        }""",
    """        ActionOutcome::RollbackInProgress | ActionOutcome::RollbackFailed
            if record.action.rollback == RollbackClass::Irreversible =>
        {
            problems.push(format!(
                "{prefix} cannot report rollback state for an irreversible action"
            ));
        }""",
)

replace_once(
    "src/journal_document.rs",
    """            if journal.records[..stop_index]
                .iter()
                .any(|record| record.outcome == ActionOutcome::Pending)
            {
                problems.push("records before stopped_after cannot remain pending".to_owned());
            }
            if journal.records[stop_index + 1..]
                .iter()
                .any(|record| record.outcome != ActionOutcome::Pending)
            {
                problems.push("records after stopped_after must remain pending".to_owned());
            }""",
    """            match journal.records[stop_index].outcome {
                ActionOutcome::Failed => {
                    if journal.records[..stop_index]
                        .iter()
                        .any(|record| record.outcome == ActionOutcome::Pending)
                    {
                        problems.push(
                            "records before a failed stop cannot remain pending".to_owned(),
                        );
                    }
                    if journal.records[stop_index + 1..]
                        .iter()
                        .any(|record| record.outcome != ActionOutcome::Pending)
                    {
                        problems.push(
                            "records after a failed stop must remain pending".to_owned(),
                        );
                    }
                }
                ActionOutcome::Skipped => {
                    if journal.records.iter().enumerate().any(|(index, record)| {
                        index != stop_index && record.outcome != ActionOutcome::Pending
                    }) {
                        problems.push(
                            "all records other than a skipped preflight stop must remain pending"
                                .to_owned(),
                        );
                    }
                }
                _ => {}
            }""",
)

replace_once(
    "src/journal_document.rs",
    "fn validate_public_text(field: &str, value: &str, problems: &mut Vec<String>) {",
    """fn validate_recovery_position(journal: &ExecutionJournal, problems: &mut Vec<String>) {
    let active = journal
        .records
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            matches!(
                record.outcome,
                ActionOutcome::Executing | ActionOutcome::RollbackInProgress
            )
        })
        .collect::<Vec<_>>();
    if active.len() > 1 {
        problems.push("execution journal contains more than one in-progress record".to_owned());
        return;
    }

    let Some((active_index, active_record)) = active.first().copied() else {
        return;
    };
    match active_record.outcome {
        ActionOutcome::Executing => {
            if journal.stopped_after.is_some() {
                problems.push("an executing journal cannot also contain a stop position".to_owned());
            }
            if journal.records[..active_index]
                .iter()
                .any(|record| record.outcome != ActionOutcome::Completed)
            {
                problems.push("records before an executing action must be completed".to_owned());
            }
            if journal.records[active_index + 1..]
                .iter()
                .any(|record| record.outcome != ActionOutcome::Pending)
            {
                problems.push("records after an executing action must remain pending".to_owned());
            }
        }
        ActionOutcome::RollbackInProgress => {
            if journal.stopped_after.is_none() {
                problems.push("rollback_in_progress requires a failed stop position".to_owned());
            }
        }
        _ => unreachable!(),
    }
}

fn validate_public_text(field: &str, value: &str, problems: &mut Vec<String>) {""",
)

replace_once(
    "src/journal_document.rs",
    "    #[test]\n    fn pending_and_rollback_outcomes_must_match_their_contracts() {",
    """    #[test]
    fn skipped_irreversible_preflight_allows_earlier_records_to_remain_pending() {
        let journal = ExecutionJournal {
            schema_version: JOURNAL_SCHEMA_VERSION,
            records: vec![
                JournalRecord {
                    action: action("one", RollbackClass::Reversible),
                    outcome: ActionOutcome::Pending,
                    message: None,
                },
                JournalRecord {
                    action: action("two", RollbackClass::Irreversible),
                    outcome: ActionOutcome::Skipped,
                    message: Some(
                        "irreversible action requires explicit confirmation".to_owned(),
                    ),
                },
                JournalRecord {
                    action: action("three", RollbackClass::Reversible),
                    outcome: ActionOutcome::Pending,
                    message: None,
                },
            ],
            stopped_after: Some("two".to_owned()),
        };

        document(journal);
    }

    #[test]
    fn executing_state_round_trips_and_rejects_a_second_active_record() {
        let mut journal = ExecutionJournal {
            schema_version: JOURNAL_SCHEMA_VERSION,
            records: vec![
                JournalRecord {
                    action: action("one", RollbackClass::Reversible),
                    outcome: ActionOutcome::Executing,
                    message: None,
                },
                JournalRecord {
                    action: action("two", RollbackClass::Reversible),
                    outcome: ActionOutcome::Pending,
                    message: None,
                },
            ],
            stopped_after: None,
        };
        let document = document(journal.clone());
        let encoded = encode_journal_document(&document).expect("encode journal");
        assert_eq!(
            decode_journal_document(&encoded).expect("decode journal"),
            document
        );

        journal.records[1].outcome = ActionOutcome::Executing;
        JournalStateDocument::new(
            InstallationId::parse("0123456789abcdef").expect("installation ID"),
            JournalId::parse("apply-00000001").expect("journal ID"),
            journal,
        )
        .expect_err("two active records must fail");
    }

    #[test]
    fn pending_and_rollback_outcomes_must_match_their_contracts() {""",
)

replace_once(
    "docs/ROADMAP.md",
    """- [ ] Atomic ownership and journal persistence with crash recovery.
- [ ] Root and runner-user lane implementations.""",
    """- [ ] Atomic ownership persistence with crash recovery.
- [x] Atomic execution-journal checkpoints with explicit interrupted states.
- [ ] Root and runner-user lane implementations.""",
)

replace_once(
    "src/durable_journal.rs",
    "    Checkpoint(DurableCheckpointError),",
    "    Checkpoint(Box<DurableCheckpointError>),",
)

replace_once(
    "src/durable_journal.rs",
    """        Err(failure) => Err(DurableExecutionError::Checkpoint(DurableCheckpointError {
            phase,
            action_id: action_index.map(|index| journal.records[index].action.id.clone()),
            last_durable: last_durable.clone(),
            attempted: journal.clone(),
            failure,
        })),""",
    """        Err(failure) => Err(DurableExecutionError::Checkpoint(Box::new(
            DurableCheckpointError {
                phase,
                action_id: action_index.map(|index| journal.records[index].action.id.clone()),
                last_durable: last_durable.clone(),
                attempted: journal.clone(),
                failure,
            },
        ))),""",
)
