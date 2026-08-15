#![allow(dead_code)]

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::github_scale_set_bridge::{
    ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence, ScaleSetBridgePoll, ScaleSetStatistics,
};
use crate::github_scale_set_protocol::{
    ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName,
    ScaleSetRunnerReference, ScaleSetRunnerRequestId,
};

pub(crate) const SCALE_SET_DELIVERY_SCHEMA_VERSION: u8 = 1;
pub(crate) const MAX_SCALE_SET_DELIVERY_BYTES: usize = 128 * 1024;
const MAX_SCALE_SET_DELIVERY_EVENTS: usize = 50;
const MAX_SCALE_SET_DELIVERY_LABELS: usize = 32;
const MAX_TOKEN_BYTES: usize = 100;

/// Canonical durable representation of one validated Runner Scale Set message.
///
/// The document retains every normalized lifecycle event plus the exact available runner-request
/// identities. A later consumer can durably publish these bytes before it permits `ack` to delete
/// the corresponding service message and acquire its available jobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ScaleSetDelivery {
    schema_version: u8,
    message_id: u32,
    statistics: ScaleSetDeliveryStatistics,
    available_request_ids: Vec<u64>,
    events: Vec<ScaleSetDeliveryEvent>,
}

/// Validated job evidence projected from one canonical durable Scale Set delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetDeliveryJobEvidence {
    pub(crate) runner_request_id: ScaleSetRunnerRequestId,
    pub(crate) repository: String,
    pub(crate) owner: String,
    pub(crate) job_id: ScaleSetJobId,
    pub(crate) workflow_run_id: u64,
    pub(crate) request_labels: Vec<String>,
}

/// Typed lifecycle evidence retained by one canonical durable Scale Set delivery.
///
/// This projection is intentionally separate from the bridge adapter's response type. Consumers
/// reconstruct it from validated durable bytes after restart instead of retaining bridge-process
/// objects across the durability boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScaleSetDeliveryLifecycleEvent {
    Available {
        job: ScaleSetDeliveryJobEvidence,
    },
    Assigned {
        job: ScaleSetDeliveryJobEvidence,
    },
    Started {
        job: ScaleSetDeliveryJobEvidence,
        runner: ScaleSetRunnerReference,
    },
    Completed {
        job: ScaleSetDeliveryJobEvidence,
        runner: Option<ScaleSetRunnerReference>,
        result: ScaleSetJobResult,
    },
}

impl ScaleSetDelivery {
    pub(crate) fn from_bridge_poll(
        poll: &ScaleSetBridgePoll,
    ) -> Result<Option<Self>, ScaleSetDeliveryError> {
        let ScaleSetBridgePoll::Message {
            message_id,
            statistics,
            events,
        } = poll
        else {
            return Ok(None);
        };
        let mut available_request_ids = Vec::new();
        let mut retained_events = Vec::with_capacity(events.len());
        for event in events {
            let retained = ScaleSetDeliveryEvent::from_bridge(event);
            if let ScaleSetDeliveryEvent::Available { job } = &retained {
                available_request_ids.push(job.runner_request_id);
            }
            retained_events.push(retained);
        }
        let delivery = Self {
            schema_version: SCALE_SET_DELIVERY_SCHEMA_VERSION,
            message_id: *message_id,
            statistics: ScaleSetDeliveryStatistics::from_bridge(*statistics),
            available_request_ids,
            events: retained_events,
        };
        delivery.validate()?;
        Ok(Some(delivery))
    }

    #[must_use]
    pub(crate) const fn message_id(&self) -> u32 {
        self.message_id
    }

    pub(crate) fn available_request_ids(
        &self,
    ) -> Result<Vec<ScaleSetRunnerRequestId>, ScaleSetDeliveryError> {
        self.available_request_ids
            .iter()
            .copied()
            .map(parse_runner_request_id)
            .collect()
    }

    /// Reconstruct every retained lifecycle event using validated protocol identities.
    pub(crate) fn retained_events(
        &self,
    ) -> Result<Vec<ScaleSetDeliveryLifecycleEvent>, ScaleSetDeliveryError> {
        self.validate()?;
        self.events
            .iter()
            .map(ScaleSetDeliveryEvent::to_lifecycle_event)
            .collect()
    }

    fn validate(&self) -> Result<(), ScaleSetDeliveryError> {
        if self.schema_version != SCALE_SET_DELIVERY_SCHEMA_VERSION
            || self.message_id == 0
            || self.events.len() > MAX_SCALE_SET_DELIVERY_EVENTS
            || self.available_request_ids.len() > MAX_SCALE_SET_DELIVERY_EVENTS
        {
            return Err(delivery_error(
                ScaleSetDeliveryErrorKind::CorruptEvidence,
                "Scale Set delivery metadata is invalid",
            ));
        }
        self.statistics.validate()?;

        let mut expected_available = Vec::new();
        let mut seen_available = BTreeSet::new();
        for event in &self.events {
            event.validate()?;
            if let ScaleSetDeliveryEvent::Available { job } = event {
                let request_id = parse_runner_request_id(job.runner_request_id)?;
                if !seen_available.insert(request_id) {
                    return Err(delivery_error(
                        ScaleSetDeliveryErrorKind::CorruptEvidence,
                        "Scale Set delivery repeats an available runner request",
                    ));
                }
                expected_available.push(job.runner_request_id);
            }
        }
        if expected_available != self.available_request_ids {
            return Err(delivery_error(
                ScaleSetDeliveryErrorKind::CorruptEvidence,
                "available runner requests differ from the retained message events",
            ));
        }
        self.available_request_ids()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScaleSetDeliveryStatistics {
    available_jobs: u32,
    acquired_jobs: u32,
    assigned_jobs: u32,
    running_jobs: u32,
    registered_runners: u32,
    busy_runners: u32,
    idle_runners: u32,
}

impl ScaleSetDeliveryStatistics {
    fn from_bridge(statistics: ScaleSetStatistics) -> Self {
        Self {
            available_jobs: statistics.available_jobs,
            acquired_jobs: statistics.acquired_jobs,
            assigned_jobs: statistics.assigned_jobs,
            running_jobs: statistics.running_jobs,
            registered_runners: statistics.registered_runners,
            busy_runners: statistics.busy_runners,
            idle_runners: statistics.idle_runners,
        }
    }

    fn validate(self) -> Result<(), ScaleSetDeliveryError> {
        let classified = self
            .busy_runners
            .checked_add(self.idle_runners)
            .ok_or_else(|| {
                delivery_error(
                    ScaleSetDeliveryErrorKind::CorruptEvidence,
                    "Scale Set runner statistics overflow",
                )
            })?;
        if self.running_jobs > self.assigned_jobs
            || self.busy_runners > self.registered_runners
            || self.idle_runners > self.registered_runners
            || classified > self.registered_runners
        {
            return Err(delivery_error(
                ScaleSetDeliveryErrorKind::CorruptEvidence,
                "Scale Set delivery statistics are inconsistent",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ScaleSetDeliveryEvent {
    Available {
        job: ScaleSetDeliveryJob,
    },
    Assigned {
        job: ScaleSetDeliveryJob,
    },
    Started {
        job: ScaleSetDeliveryJob,
        runner: ScaleSetDeliveryRunner,
    },
    Completed {
        job: ScaleSetDeliveryJob,
        #[serde(skip_serializing_if = "Option::is_none")]
        runner: Option<ScaleSetDeliveryRunner>,
        result: String,
    },
}

impl ScaleSetDeliveryEvent {
    fn from_bridge(event: &ScaleSetBridgeEvent) -> Self {
        match event {
            ScaleSetBridgeEvent::Available(job) => Self::Available {
                job: ScaleSetDeliveryJob::from_bridge(job),
            },
            ScaleSetBridgeEvent::Assigned(job) => Self::Assigned {
                job: ScaleSetDeliveryJob::from_bridge(job),
            },
            ScaleSetBridgeEvent::Started { job, runner } => Self::Started {
                job: ScaleSetDeliveryJob::from_bridge(job),
                runner: ScaleSetDeliveryRunner::from_bridge(runner),
            },
            ScaleSetBridgeEvent::Completed {
                job,
                runner,
                result,
            } => Self::Completed {
                job: ScaleSetDeliveryJob::from_bridge(job),
                runner: runner.as_ref().map(ScaleSetDeliveryRunner::from_bridge),
                result: result.as_str().to_owned(),
            },
        }
    }

    fn to_lifecycle_event(&self) -> Result<ScaleSetDeliveryLifecycleEvent, ScaleSetDeliveryError> {
        self.validate()?;
        Ok(match self {
            Self::Available { job } => ScaleSetDeliveryLifecycleEvent::Available {
                job: job.to_job_evidence()?,
            },
            Self::Assigned { job } => ScaleSetDeliveryLifecycleEvent::Assigned {
                job: job.to_job_evidence()?,
            },
            Self::Started { job, runner } => ScaleSetDeliveryLifecycleEvent::Started {
                job: job.to_job_evidence()?,
                runner: runner.to_runner_reference()?,
            },
            Self::Completed {
                job,
                runner,
                result,
            } => ScaleSetDeliveryLifecycleEvent::Completed {
                job: job.to_job_evidence()?,
                runner: runner
                    .as_ref()
                    .map(ScaleSetDeliveryRunner::to_runner_reference)
                    .transpose()?,
                result: ScaleSetJobResult::parse(result).map_err(|_| {
                    delivery_error(
                        ScaleSetDeliveryErrorKind::CorruptEvidence,
                        "Scale Set completion result is invalid",
                    )
                })?,
            },
        })
    }

    fn validate(&self) -> Result<(), ScaleSetDeliveryError> {
        match self {
            Self::Available { job } | Self::Assigned { job } => job.validate(),
            Self::Started { job, runner } => {
                job.validate()?;
                runner.validate()
            }
            Self::Completed {
                job,
                runner,
                result,
            } => {
                job.validate()?;
                let result = ScaleSetJobResult::parse(result).map_err(|_| {
                    delivery_error(
                        ScaleSetDeliveryErrorKind::CorruptEvidence,
                        "Scale Set completion result is invalid",
                    )
                })?;
                if let Some(runner) = runner {
                    runner.validate()?;
                } else if result.as_str() != "canceled" {
                    return Err(delivery_error(
                        ScaleSetDeliveryErrorKind::CorruptEvidence,
                        "runnerless completion must be an exact cancellation",
                    ));
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScaleSetDeliveryJob {
    runner_request_id: u64,
    repository: String,
    owner: String,
    job_id: String,
    workflow_run_id: u64,
    request_labels: Vec<String>,
}

impl ScaleSetDeliveryJob {
    fn from_bridge(job: &ScaleSetBridgeJobEvidence) -> Self {
        Self {
            runner_request_id: job.runner_request_id,
            repository: job.repository.clone(),
            owner: job.owner.clone(),
            job_id: job.job_id.as_str().to_owned(),
            workflow_run_id: job.workflow_run_id,
            request_labels: job.request_labels.clone(),
        }
    }

    fn to_job_evidence(&self) -> Result<ScaleSetDeliveryJobEvidence, ScaleSetDeliveryError> {
        self.validate()?;
        Ok(ScaleSetDeliveryJobEvidence {
            runner_request_id: parse_runner_request_id(self.runner_request_id)?,
            repository: self.repository.clone(),
            owner: self.owner.clone(),
            job_id: ScaleSetJobId::parse(&self.job_id).map_err(|_| {
                delivery_error(
                    ScaleSetDeliveryErrorKind::CorruptEvidence,
                    "Scale Set job identity is invalid",
                )
            })?,
            workflow_run_id: self.workflow_run_id,
            request_labels: self.request_labels.clone(),
        })
    }

    fn validate(&self) -> Result<(), ScaleSetDeliveryError> {
        parse_runner_request_id(self.runner_request_id)?;
        ScaleSetJobId::parse(&self.job_id).map_err(|_| {
            delivery_error(
                ScaleSetDeliveryErrorKind::CorruptEvidence,
                "Scale Set job identity is invalid",
            )
        })?;
        if self.workflow_run_id == 0
            || !bounded_token(&self.repository, MAX_TOKEN_BYTES)
            || !bounded_token(&self.owner, MAX_TOKEN_BYTES)
            || self.request_labels.len() > MAX_SCALE_SET_DELIVERY_LABELS
            || self
                .request_labels
                .iter()
                .any(|label| !bounded_token(label, MAX_TOKEN_BYTES))
        {
            return Err(delivery_error(
                ScaleSetDeliveryErrorKind::CorruptEvidence,
                "Scale Set job evidence is invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScaleSetDeliveryRunner {
    id: u64,
    name: String,
}

impl ScaleSetDeliveryRunner {
    fn from_bridge(runner: &ScaleSetRunnerReference) -> Self {
        Self {
            id: runner.id.get(),
            name: runner.name.as_str().to_owned(),
        }
    }

    fn to_runner_reference(&self) -> Result<ScaleSetRunnerReference, ScaleSetDeliveryError> {
        self.validate()?;
        Ok(ScaleSetRunnerReference::new(
            ScaleSetRunnerId::new(self.id).map_err(|_| {
                delivery_error(
                    ScaleSetDeliveryErrorKind::CorruptEvidence,
                    "Scale Set runner ID is invalid",
                )
            })?,
            ScaleSetRunnerName::parse(&self.name).map_err(|_| {
                delivery_error(
                    ScaleSetDeliveryErrorKind::CorruptEvidence,
                    "Scale Set runner name is invalid",
                )
            })?,
        ))
    }

    fn validate(&self) -> Result<(), ScaleSetDeliveryError> {
        ScaleSetRunnerId::new(self.id).map_err(|_| {
            delivery_error(
                ScaleSetDeliveryErrorKind::CorruptEvidence,
                "Scale Set runner ID is invalid",
            )
        })?;
        ScaleSetRunnerName::parse(&self.name).map_err(|_| {
            delivery_error(
                ScaleSetDeliveryErrorKind::CorruptEvidence,
                "Scale Set runner name is invalid",
            )
        })?;
        Ok(())
    }
}

/// Encode one validated message delivery into bounded canonical JSON bytes.
pub(crate) fn encode_scale_set_delivery(
    delivery: &ScaleSetDelivery,
) -> Result<Vec<u8>, ScaleSetDeliveryError> {
    delivery.validate()?;
    let bytes = serde_json::to_vec(delivery).map_err(|_| {
        delivery_error(
            ScaleSetDeliveryErrorKind::InvalidDocument,
            "Scale Set delivery cannot encode",
        )
    })?;
    if bytes.len() > MAX_SCALE_SET_DELIVERY_BYTES {
        return Err(delivery_error(
            ScaleSetDeliveryErrorKind::DocumentTooLarge,
            "Scale Set delivery exceeds the reviewed byte limit",
        ));
    }
    Ok(bytes)
}

/// Decode, revalidate, and require the canonical byte representation of one message delivery.
pub(crate) fn decode_scale_set_delivery(
    bytes: &[u8],
) -> Result<ScaleSetDelivery, ScaleSetDeliveryError> {
    if bytes.len() > MAX_SCALE_SET_DELIVERY_BYTES {
        return Err(delivery_error(
            ScaleSetDeliveryErrorKind::DocumentTooLarge,
            "Scale Set delivery exceeds the reviewed byte limit",
        ));
    }
    let version: ScaleSetDeliveryVersion = serde_json::from_slice(bytes).map_err(|_| {
        delivery_error(
            ScaleSetDeliveryErrorKind::InvalidDocument,
            "Scale Set delivery JSON is invalid",
        )
    })?;
    if version.schema_version != SCALE_SET_DELIVERY_SCHEMA_VERSION {
        return Err(delivery_error(
            ScaleSetDeliveryErrorKind::VersionIncompatible,
            "Scale Set delivery schema version is unsupported",
        ));
    }
    let delivery: ScaleSetDelivery = serde_json::from_slice(bytes).map_err(|_| {
        delivery_error(
            ScaleSetDeliveryErrorKind::InvalidDocument,
            "Scale Set delivery JSON is invalid",
        )
    })?;
    delivery.validate()?;
    if encode_scale_set_delivery(&delivery)? != bytes {
        return Err(delivery_error(
            ScaleSetDeliveryErrorKind::NonCanonical,
            "Scale Set delivery is not in canonical JSON form",
        ));
    }
    Ok(delivery)
}

#[derive(Deserialize)]
struct ScaleSetDeliveryVersion {
    schema_version: u8,
}

fn parse_runner_request_id(value: u64) -> Result<ScaleSetRunnerRequestId, ScaleSetDeliveryError> {
    ScaleSetRunnerRequestId::new(value).map_err(|_| {
        delivery_error(
            ScaleSetDeliveryErrorKind::CorruptEvidence,
            "Scale Set runner request identity is invalid",
        )
    })
}

fn bounded_token(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ScaleSetDeliveryErrorKind {
    InvalidDocument,
    VersionIncompatible,
    DocumentTooLarge,
    NonCanonical,
    CorruptEvidence,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct ScaleSetDeliveryError {
    kind: ScaleSetDeliveryErrorKind,
    message: &'static str,
}

impl ScaleSetDeliveryError {
    #[must_use]
    pub(crate) const fn kind(self) -> ScaleSetDeliveryErrorKind {
        self.kind
    }
}

impl fmt::Display for ScaleSetDeliveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ScaleSetDeliveryError {}

const fn delivery_error(
    kind: ScaleSetDeliveryErrorKind,
    message: &'static str,
) -> ScaleSetDeliveryError {
    ScaleSetDeliveryError { kind, message }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn statistics() -> ScaleSetStatistics {
        ScaleSetStatistics {
            available_jobs: 1,
            acquired_jobs: 0,
            assigned_jobs: 1,
            running_jobs: 0,
            registered_runners: 0,
            busy_runners: 0,
            idle_runners: 0,
        }
    }

    fn lifecycle_statistics() -> ScaleSetStatistics {
        ScaleSetStatistics {
            available_jobs: 1,
            acquired_jobs: 0,
            assigned_jobs: 3,
            running_jobs: 1,
            registered_runners: 1,
            busy_runners: 1,
            idle_runners: 0,
        }
    }

    fn job(request_id: u64, job_id: &str) -> ScaleSetBridgeJobEvidence {
        ScaleSetBridgeJobEvidence {
            runner_request_id: request_id,
            repository: "project".to_owned(),
            owner: "example".to_owned(),
            job_id: ScaleSetJobId::parse(job_id).unwrap(),
            workflow_run_id: 99,
            request_labels: vec!["smolrunner".to_owned()],
        }
    }

    fn retained_job(request_id: u64, job_id: &str) -> ScaleSetDeliveryJobEvidence {
        ScaleSetDeliveryJobEvidence {
            runner_request_id: ScaleSetRunnerRequestId::new(request_id).unwrap(),
            repository: "project".to_owned(),
            owner: "example".to_owned(),
            job_id: ScaleSetJobId::parse(job_id).unwrap(),
            workflow_run_id: 99,
            request_labels: vec!["smolrunner".to_owned()],
        }
    }

    fn runner(id: u64, name: &str) -> ScaleSetRunnerReference {
        ScaleSetRunnerReference::new(
            ScaleSetRunnerId::new(id).unwrap(),
            ScaleSetRunnerName::parse(name).unwrap(),
        )
    }

    #[test]
    fn bridge_message_becomes_canonical_delivery_before_ack() {
        let poll = ScaleSetBridgePoll::Message {
            message_id: 7,
            statistics: statistics(),
            events: vec![
                ScaleSetBridgeEvent::Available(job(41, "job-1")),
                ScaleSetBridgeEvent::Assigned(job(42, "job-2")),
            ],
        };
        let delivery = ScaleSetDelivery::from_bridge_poll(&poll)
            .unwrap()
            .expect("message must produce a delivery");
        assert_eq!(delivery.message_id(), 7);
        assert_eq!(
            delivery
                .available_request_ids()
                .unwrap()
                .iter()
                .map(|id| id.get())
                .collect::<Vec<_>>(),
            vec![41]
        );

        let encoded = encode_scale_set_delivery(&delivery).unwrap();
        assert_eq!(decode_scale_set_delivery(&encoded).unwrap(), delivery);
    }

    #[test]
    fn decoded_delivery_reconstructs_every_retained_lifecycle_event() {
        let runner = runner(500, "smol-attempt-1");
        let poll = ScaleSetBridgePoll::Message {
            message_id: 8,
            statistics: lifecycle_statistics(),
            events: vec![
                ScaleSetBridgeEvent::Available(job(41, "job-1")),
                ScaleSetBridgeEvent::Assigned(job(42, "job-2")),
                ScaleSetBridgeEvent::Started {
                    job: job(43, "job-3"),
                    runner: runner.clone(),
                },
                ScaleSetBridgeEvent::Completed {
                    job: job(44, "job-4"),
                    runner: Some(runner.clone()),
                    result: ScaleSetJobResult::parse("succeeded").unwrap(),
                },
                ScaleSetBridgeEvent::Completed {
                    job: job(45, "job-5"),
                    runner: None,
                    result: ScaleSetJobResult::parse("canceled").unwrap(),
                },
            ],
        };
        let delivery = ScaleSetDelivery::from_bridge_poll(&poll)
            .unwrap()
            .expect("message must produce a delivery");
        let encoded = encode_scale_set_delivery(&delivery).unwrap();
        let decoded = decode_scale_set_delivery(&encoded).unwrap();

        assert_eq!(
            decoded.retained_events().unwrap(),
            vec![
                ScaleSetDeliveryLifecycleEvent::Available {
                    job: retained_job(41, "job-1"),
                },
                ScaleSetDeliveryLifecycleEvent::Assigned {
                    job: retained_job(42, "job-2"),
                },
                ScaleSetDeliveryLifecycleEvent::Started {
                    job: retained_job(43, "job-3"),
                    runner: runner.clone(),
                },
                ScaleSetDeliveryLifecycleEvent::Completed {
                    job: retained_job(44, "job-4"),
                    runner: Some(runner),
                    result: ScaleSetJobResult::parse("succeeded").unwrap(),
                },
                ScaleSetDeliveryLifecycleEvent::Completed {
                    job: retained_job(45, "job-5"),
                    runner: None,
                    result: ScaleSetJobResult::parse("canceled").unwrap(),
                },
            ]
        );
    }

    #[test]
    fn idle_poll_has_no_acknowledgeable_delivery() {
        let poll = ScaleSetBridgePoll::Idle {
            statistics: statistics(),
        };
        assert!(ScaleSetDelivery::from_bridge_poll(&poll).unwrap().is_none());
    }

    #[test]
    fn duplicate_available_runner_request_fails_closed() {
        let poll = ScaleSetBridgePoll::Message {
            message_id: 7,
            statistics: statistics(),
            events: vec![
                ScaleSetBridgeEvent::Available(job(41, "job-1")),
                ScaleSetBridgeEvent::Available(job(41, "job-1")),
            ],
        };
        assert_eq!(
            ScaleSetDelivery::from_bridge_poll(&poll)
                .unwrap_err()
                .kind(),
            ScaleSetDeliveryErrorKind::CorruptEvidence
        );
    }

    #[test]
    fn codec_rejects_future_noncanonical_and_changed_available_ids() {
        let poll = ScaleSetBridgePoll::Message {
            message_id: 7,
            statistics: statistics(),
            events: vec![ScaleSetBridgeEvent::Available(job(41, "job-1"))],
        };
        let delivery = ScaleSetDelivery::from_bridge_poll(&poll)
            .unwrap()
            .expect("message must produce a delivery");
        let canonical = encode_scale_set_delivery(&delivery).unwrap();

        let future = String::from_utf8(canonical.clone())
            .unwrap()
            .replacen("\"schema_version\":1", "\"schema_version\":2", 1)
            .into_bytes();
        assert_eq!(
            decode_scale_set_delivery(&future).unwrap_err().kind(),
            ScaleSetDeliveryErrorKind::VersionIncompatible
        );

        let mut noncanonical = b" ".to_vec();
        noncanonical.extend_from_slice(&canonical);
        assert_eq!(
            decode_scale_set_delivery(&noncanonical).unwrap_err().kind(),
            ScaleSetDeliveryErrorKind::NonCanonical
        );

        let changed = String::from_utf8(canonical)
            .unwrap()
            .replacen(
                "\"available_request_ids\":[41]",
                "\"available_request_ids\":[42]",
                1,
            )
            .into_bytes();
        assert_eq!(
            decode_scale_set_delivery(&changed).unwrap_err().kind(),
            ScaleSetDeliveryErrorKind::CorruptEvidence
        );
    }
}
