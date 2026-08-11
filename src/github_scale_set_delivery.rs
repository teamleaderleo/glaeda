use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::github_scale_set_bridge::{
    ScaleSetBridgeEvent, ScaleSetBridgeJobEvidence, ScaleSetBridgePoll, ScaleSetStatistics,
};
use crate::github_scale_set_protocol::{
    ScaleSetJobId, ScaleSetJobResult, ScaleSetRunnerId, ScaleSetRunnerName, ScaleSetRunnerReference,
    ScaleSetRunnerRequestId,
};

pub(crate) const SCALE_SET_DELIVERY_SCHEMA_VERSION: u8 = 1;
pub(crate) const MAX_SCALE_SET_DELIVERY_BYTES: usize = 128 * 1024;
const MAX_SCALE_SET_DELIVERY_EVENTS: usize = 50;
const MAX_SCALE_SET_DELIVERY_LABELS: usize = 32;
const MAX_REPOSITORY_TOKEN_BYTES: usize = 100;
const MAX_LABEL_BYTES: usize = 100;

/// Canonical durable representation of one validated Runner Scale Set message.
///
/// The delivery retains every lifecycle event plus the exact available runner-request identities
/// that must survive before the bridge may acknowledge the containing message. An idle poll has no
/// delivery document because there is no service message to acknowledge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetDelivery {
    schema_version: u8,
    message_id: u32,
    statistics: ScaleSetDeliveryStatistics,
    available_request_ids: Vec<ScaleSetRunnerRequestId>,
    events: Vec<ScaleSetDeliveryEvent>,
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
        if *message_id == 0 || events.len() > MAX_SCALE_SET_DELIVERY_EVENTS {
            return Err(delivery_error(
                ScaleSetDeliveryErrorKind::CorruptEvidence,
                "validated bridge delivery is outside the reviewed bounds",
            ));
        }

        let mut available_request_ids = Vec::new();
        let mut normalized_events = Vec::with_capacity(events.len());
        for event in events {
            let normalized = ScaleSetDeliveryEvent::from_bridge(event)?;
            if let ScaleSetDeliveryEvent::Available { job } = &normalized {
                available_request_ids.push(job.runner_request_id);
            }
            normalized_events.push(normalized);
        }
        let delivery = Self {
            schema_version: SCALE_SET_DELIVERY_SCHEMA_VERSION,
            message_id: *message_id,
            statistics: ScaleSetDeliveryStatistics::from_bridge(*statistics),
            available_request_ids,
            events: normalized_events,
        };
        delivery.validate()?;
        Ok(Some(delivery))
    }

    #[must_use]
    pub(crate) const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub(crate) const fn message_id(&self) -> u32 {
        self.message_id
    }

    #[must_use]
    pub(crate) fn available_request_ids(&self) -> &[ScaleSetRunnerRequestId] {
        &self.available_request_ids
    }

    #[must_use]
    pub(crate) fn events(&self) -> &[ScaleSetDeliveryEvent] {
        &self.events
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
                if !seen_available.insert(job.runner_request_id) {
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
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ScaleSetDeliveryStatistics {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScaleSetDeliveryJob {
    runner_request_id: ScaleSetRunnerRequestId,
    repository: String,
    owner: String,
    job_id: ScaleSetJobId,
    workflow_run_id: u64,
    request_labels: Vec<String>,
}

impl ScaleSetDeliveryJob {
    fn from_bridge(job: &ScaleSetBridgeJobEvidence) -> Result<Self, ScaleSetDeliveryError> {
        let job = Self {
            runner_request_id: ScaleSetRunnerRequestId::new(job.runner_request_id).map_err(|_| {
                delivery_error(
                    ScaleSetDeliveryErrorKind::CorruptEvidence,
                    "Scale Set runner request identity is invalid",
                )
            })?,
            repository: job.repository.clone(),
            owner: job.owner.clone(),
            job_id: job.job_id.clone(),
            workflow_run_id: job.workflow_run_id,
            request_labels: job.request_labels.clone(),
        };
        job.validate()?;
        Ok(job)
    }

    fn validate(&self) -> Result<(), ScaleSetDeliveryError> {
        if self.workflow_run_id == 0
            || !bounded_token(&self.repository, MAX_REPOSITORY_TOKEN_BYTES)
            || !bounded_token(&self.owner, MAX_REPOSITORY_TOKEN_BYTES)
            || self.request_labels.len() > MAX_SCALE_SET_DELIVERY_LABELS
            || self
                .request_labels
                .iter()
                .any(|label| !bounded_token(label, MAX_LABEL_BYTES))
        {
            return Err(delivery_error(
                ScaleSetDeliveryErrorKind::CorruptEvidence,
                "Scale Set job evidence is invalid",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ScaleSetDeliveryEvent {
    Available {
        job: ScaleSetDeliveryJob,
    },
    Assigned {
        job: ScaleSetDeliveryJob,
    },
    Started {
        job: ScaleSetDeliveryJob,
        runner: ScaleSetRunnerReference,
    },
    Completed {
        job: ScaleSetDeliveryJob,
        runner: Option<ScaleSetRunnerReference>,
        result: ScaleSetJobResult,
    },
}

impl ScaleSetDeliveryEvent {
    fn from_bridge(event: &ScaleSetBridgeEvent) -> Result<Self, ScaleSetDeliveryError> {
        let event = match event {
            ScaleSetBridgeEvent::Available(job) => Self::Available {
                job: ScaleSetDeliveryJob::from_bridge(job)?,
            },
            ScaleSetBridgeEvent::Assigned(job) => Self::Assigned {
                job: ScaleSetDeliveryJob::from_bridge(job)?,
            },
            ScaleSetBridgeEvent::Started { job, runner } => Self::Started {
                job: ScaleSetDeliveryJob::from_bridge(job)?,
                runner: runner.clone(),
            },
            ScaleSetBridgeEvent::Completed {
                job,
                runner,
                result,
            } => Self::Completed {
                job: ScaleSetDeliveryJob::from_bridge(job)?,
                runner: runner.clone(),
                result: result.clone(),
            },
        };
        event.validate()?;
        Ok(event)
    }

    fn validate(&self) -> Result<(), ScaleSetDeliveryError> {
        match self {
            Self::Available { job } | Self::Assigned { job } => job.validate(),
            Self::Started { job, runner } => {
                job.validate()?;
                validate_runner(runner)
            }
            Self::Completed {
                job,
                runner,
                result,
            } => {
                job.validate()?;
                if let Some(runner) = runner {
                    validate_runner(runner)?;
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

fn validate_runner(runner: &ScaleSetRunnerReference) -> Result<(), ScaleSetDeliveryError> {
    if runner.id.get() == 0 || runner.name.as_str().is_empty() {
        return Err(delivery_error(
            ScaleSetDeliveryErrorKind::CorruptEvidence,
            "Scale Set runner evidence is invalid",
        ));
    }
    Ok(())
}

/// Encode one validated message delivery into bounded canonical JSON bytes.
pub(crate) fn encode_scale_set_delivery(
    delivery: &ScaleSetDelivery,
) -> Result<Vec<u8>, ScaleSetDeliveryError> {
    delivery.validate()?;
    let wire = DeliveryWire::from_delivery(delivery);
    let bytes = serde_json::to_vec(&wire).map_err(|_| {
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
    let version: DeliveryVersionWire = serde_json::from_slice(bytes).map_err(|_| {
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
    let wire: DeliveryWire = serde_json::from_slice(bytes).map_err(|_| {
        delivery_error(
            ScaleSetDeliveryErrorKind::InvalidDocument,
            "Scale Set delivery JSON is invalid",
        )
    })?;
    let delivery = wire.into_delivery()?;
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
struct DeliveryVersionWire {
    schema_version: u8,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryWire {
    schema_version: u8,
    message_id: u32,
    statistics: StatisticsWire,
    available_request_ids: Vec<u64>,
    events: Vec<EventWire>,
}

impl DeliveryWire {
    fn from_delivery(delivery: &ScaleSetDelivery) -> Self {
        Self {
            schema_version: delivery.schema_version,
            message_id: delivery.message_id,
            statistics: StatisticsWire::from_statistics(delivery.statistics),
            available_request_ids: delivery
                .available_request_ids
                .iter()
                .map(|id| id.get())
                .collect(),
            events: delivery
                .events
                .iter()
                .map(EventWire::from_event)
                .collect(),
        }
    }

    fn into_delivery(self) -> Result<ScaleSetDelivery, ScaleSetDeliveryError> {
        if self.events.len() > MAX_SCALE_SET_DELIVERY_EVENTS
            || self.available_request_ids.len() > MAX_SCALE_SET_DELIVERY_EVENTS
        {
            return Err(delivery_error(
                ScaleSetDeliveryErrorKind::CorruptEvidence,
                "Scale Set delivery exceeds a reviewed entry bound",
            ));
        }
        let delivery = ScaleSetDelivery {
            schema_version: self.schema_version,
            message_id: self.message_id,
            statistics: self.statistics.into_statistics(),
            available_request_ids: self
                .available_request_ids
                .into_iter()
                .map(|id| {
                    ScaleSetRunnerRequestId::new(id).map_err(|_| {
                        delivery_error(
                            ScaleSetDeliveryErrorKind::CorruptEvidence,
                            "Scale Set runner request identity is invalid",
                        )
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            events: self
                .events
                .into_iter()
                .map(EventWire::into_event)
                .collect::<Result<Vec<_>, _>>()?,
        };
        delivery.validate()?;
        Ok(delivery)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatisticsWire {
    available_jobs: u32,
    acquired_jobs: u32,
    assigned_jobs: u32,
    running_jobs: u32,
    registered_runners: u32,
    busy_runners: u32,
    idle_runners: u32,
}

impl StatisticsWire {
    fn from_statistics(statistics: ScaleSetDeliveryStatistics) -> Self {
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

    fn into_statistics(self) -> ScaleSetDeliveryStatistics {
        ScaleSetDeliveryStatistics {
            available_jobs: self.available_jobs,
            acquired_jobs: self.acquired_jobs,
            assigned_jobs: self.assigned_jobs,
            running_jobs: self.running_jobs,
            registered_runners: self.registered_runners,
            busy_runners: self.busy_runners,
            idle_runners: self.idle_runners,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum EventWire {
    Available { job: JobWire },
    Assigned { job: JobWire },
    Started { job: JobWire, runner: RunnerWire },
    Completed {
        job: JobWire,
        #[serde(skip_serializing_if = "Option::is_none")]
        runner: Option<RunnerWire>,
        result: String,
    },
}

impl EventWire {
    fn from_event(event: &ScaleSetDeliveryEvent) -> Self {
        match event {
            ScaleSetDeliveryEvent::Available { job } => Self::Available {
                job: JobWire::from_job(job),
            },
            ScaleSetDeliveryEvent::Assigned { job } => Self::Assigned {
                job: JobWire::from_job(job),
            },
            ScaleSetDeliveryEvent::Started { job, runner } => Self::Started {
                job: JobWire::from_job(job),
                runner: RunnerWire::from_runner(runner),
            },
            ScaleSetDeliveryEvent::Completed {
                job,
                runner,
                result,
            } => Self::Completed {
                job: JobWire::from_job(job),
                runner: runner.as_ref().map(RunnerWire::from_runner),
                result: result.as_str().to_owned(),
            },
        }
    }

    fn into_event(self) -> Result<ScaleSetDeliveryEvent, ScaleSetDeliveryError> {
        let event = match self {
            Self::Available { job } => ScaleSetDeliveryEvent::Available {
                job: job.into_job()?,
            },
            Self::Assigned { job } => ScaleSetDeliveryEvent::Assigned {
                job: job.into_job()?,
            },
            Self::Started { job, runner } => ScaleSetDeliveryEvent::Started {
                job: job.into_job()?,
                runner: runner.into_runner()?,
            },
            Self::Completed {
                job,
                runner,
                result,
            } => ScaleSetDeliveryEvent::Completed {
                job: job.into_job()?,
                runner: runner.map(RunnerWire::into_runner).transpose()?,
                result: ScaleSetJobResult::parse(&result).map_err(|_| {
                    delivery_error(
                        ScaleSetDeliveryErrorKind::CorruptEvidence,
                        "Scale Set completion result is invalid",
                    )
                })?,
            },
        };
        event.validate()?;
        Ok(event)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JobWire {
    runner_request_id: u64,
    repository: String,
    owner: String,
    job_id: String,
    workflow_run_id: u64,
    request_labels: Vec<String>,
}

impl JobWire {
    fn from_job(job: &ScaleSetDeliveryJob) -> Self {
        Self {
            runner_request_id: job.runner_request_id.get(),
            repository: job.repository.clone(),
            owner: job.owner.clone(),
            job_id: job.job_id.as_str().to_owned(),
            workflow_run_id: job.workflow_run_id,
            request_labels: job.request_labels.clone(),
        }
    }

    fn into_job(self) -> Result<ScaleSetDeliveryJob, ScaleSetDeliveryError> {
        let job = ScaleSetDeliveryJob {
            runner_request_id: ScaleSetRunnerRequestId::new(self.runner_request_id).map_err(|_| {
                delivery_error(
                    ScaleSetDeliveryErrorKind::CorruptEvidence,
                    "Scale Set runner request identity is invalid",
                )
            })?,
            repository: self.repository,
            owner: self.owner,
            job_id: ScaleSetJobId::parse(&self.job_id).map_err(|_| {
                delivery_error(
                    ScaleSetDeliveryErrorKind::CorruptEvidence,
                    "Scale Set job identity is invalid",
                )
            })?,
            workflow_run_id: self.workflow_run_id,
            request_labels: self.request_labels,
        };
        job.validate()?;
        Ok(job)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunnerWire {
    id: u64,
    name: String,
}

impl RunnerWire {
    fn from_runner(runner: &ScaleSetRunnerReference) -> Self {
        Self {
            id: runner.id.get(),
            name: runner.name.as_str().to_owned(),
        }
    }

    fn into_runner(self) -> Result<ScaleSetRunnerReference, ScaleSetDeliveryError> {
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
        assert_eq!(delivery.schema_version(), 1);
        assert_eq!(delivery.message_id(), 7);
        assert_eq!(
            delivery
                .available_request_ids()
                .iter()
                .map(|id| id.get())
                .collect::<Vec<_>>(),
            vec![41]
        );
        assert_eq!(delivery.events().len(), 2);

        let encoded = encode_scale_set_delivery(&delivery).unwrap();
        assert_eq!(decode_scale_set_delivery(&encoded).unwrap(), delivery);
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
            ScaleSetDelivery::from_bridge_poll(&poll).unwrap_err().kind(),
            ScaleSetDeliveryErrorKind::CorruptEvidence
        );
    }

    #[test]
    fn durable_codec_rejects_future_noncanonical_and_changed_available_ids() {
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
            .replacen("\"available_request_ids\":[41]", "\"available_request_ids\":[42]", 1)
            .into_bytes();
        assert_eq!(
            decode_scale_set_delivery(&changed).unwrap_err().kind(),
            ScaleSetDeliveryErrorKind::CorruptEvidence
        );
    }
}
