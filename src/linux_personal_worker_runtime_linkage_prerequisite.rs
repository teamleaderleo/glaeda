//! Same-lock snapshot of the current personal-worker executable and loader prerequisites.
//!
//! This module composes the three descriptor-retaining linkage inputs beneath the shared runtime
//! manifest lock. It does not resolve a cache path or `DT_NEEDED` library, construct a runtime
//! evidence class, seal readiness, execute a command, or mutate host state.

use std::fmt;

use serde::Serialize;

use crate::linux_personal_worker_runtime_executable_prerequisite::{
    PersonalWorkerRuntimeExecutablePrerequisite, PersonalWorkerRuntimeExecutablePrerequisiteError,
    PersonalWorkerRuntimeExecutablePrerequisiteErrorKind,
    observe_personal_worker_runtime_executable_prerequisite,
};
use crate::linux_personal_worker_runtime_loader_object_prerequisite::{
    PersonalWorkerRuntimeLoaderObjectPrerequisite,
    PersonalWorkerRuntimeLoaderObjectPrerequisiteError,
    PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind,
    observe_personal_worker_runtime_loader_object_prerequisite,
};
use crate::linux_personal_worker_runtime_loader_state_prerequisite::{
    PersonalWorkerRuntimeLoaderStatePrerequisite,
    PersonalWorkerRuntimeLoaderStatePrerequisiteError,
    PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind,
    observe_personal_worker_runtime_loader_state_prerequisite,
};
use crate::linux_personal_worker_runtime_manifest::{
    LockedPersonalWorkerRuntimeManifestObservation,
    LockedPersonalWorkerRuntimeManifestObservationError,
    PersonalWorkerRuntimeManifestDiscoveryError, PersonalWorkerRuntimeManifestDiscoveryErrorKind,
    with_reconfirmed_locked_personal_worker_runtime_manifest,
};
use crate::ownership::ProjectIdentity;
use crate::personal_worker_runtime_contract::{
    PersonalWorkerRuntimeArchitecture, PersonalWorkerRuntimePlatform,
};
use crate::personal_worker_runtime_manifest::PersonalWorkerRuntimeManifest;

pub const PERSONAL_WORKER_RUNTIME_LINKAGE_PREREQUISITE_SCHEMA_VERSION: u8 = 1;

const REDACTED: &str = "<private-runtime-linkage-prerequisite>";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeLinkagePrerequisiteDisposition {
    ObservedPrerequisite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeLinkagePrerequisiteSummary {
    schema_version: u8,
    disposition: PersonalWorkerRuntimeLinkagePrerequisiteDisposition,
    platform: PersonalWorkerRuntimePlatform,
    architecture: PersonalWorkerRuntimeArchitecture,
}

impl PersonalWorkerRuntimeLinkagePrerequisiteSummary {
    #[must_use]
    pub const fn schema_version(self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn disposition(self) -> PersonalWorkerRuntimeLinkagePrerequisiteDisposition {
        self.disposition
    }

    #[must_use]
    pub const fn platform(self) -> PersonalWorkerRuntimePlatform {
        self.platform
    }

    #[must_use]
    pub const fn architecture(self) -> PersonalWorkerRuntimeArchitecture {
        self.architecture
    }
}

/// Opaque partial linkage snapshot retained with its exact durable declaration.
///
/// This type has no constructor, cloning, serialization, digest, path, bytes, or readiness
/// conversion surface. The retained manifest and descriptors are available only to later
/// reviewed in-crate linkage composition.
pub struct PersonalWorkerRuntimeLinkagePrerequisite {
    summary: PersonalWorkerRuntimeLinkagePrerequisiteSummary,
    _manifest: PersonalWorkerRuntimeManifest,
    _sources: LinkageSources,
}

impl PersonalWorkerRuntimeLinkagePrerequisite {
    #[must_use]
    pub const fn summary(&self) -> PersonalWorkerRuntimeLinkagePrerequisiteSummary {
        self.summary
    }
}

impl fmt::Debug for PersonalWorkerRuntimeLinkagePrerequisite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeLinkagePrerequisite")
            .field("summary", &self.summary)
            .field("private_prerequisite", &REDACTED)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PersonalWorkerRuntimeLinkagePrerequisiteErrorKind {
    Missing,
    Busy,
    RecoveryRequired,
    VersionIncompatible,
    CorruptState,
    IdentityMismatch,
    UnsupportedArchitecture,
    UnsafeFilesystem,
    UnsafeConfiguration,
    InvalidPrerequisite,
    ChangedDuringRead,
    Io,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct PersonalWorkerRuntimeLinkagePrerequisiteError {
    pub kind: PersonalWorkerRuntimeLinkagePrerequisiteErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Debug for PersonalWorkerRuntimeLinkagePrerequisiteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PersonalWorkerRuntimeLinkagePrerequisiteError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for PersonalWorkerRuntimeLinkagePrerequisiteError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for PersonalWorkerRuntimeLinkagePrerequisiteError {}

struct LinkageSources {
    executables: PersonalWorkerRuntimeExecutablePrerequisite,
    loader_object: PersonalWorkerRuntimeLoaderObjectPrerequisite,
    loader_state: PersonalWorkerRuntimeLoaderStatePrerequisite,
}

impl LinkageSources {
    fn reconfirm(&mut self) -> Result<(), PersonalWorkerRuntimeLinkagePrerequisiteError> {
        self.executables.reconfirm().map_err(map_executable_error)?;
        self.loader_object
            .reconfirm()
            .map_err(map_loader_object_error)?;
        self.loader_state
            .reconfirm()
            .map_err(map_loader_state_error)
    }
}

/// Observe one exact partial linkage snapshot under the recorded runtime manifest lock.
///
/// # Errors
///
/// Returns bounded path-free errors for absent, busy, unsafe, incompatible, corrupt, stale, or
/// changed durable state; manifest identity mismatch; and any missing, unsafe, invalid, changed,
/// or unavailable executable/loader prerequisite. No host prerequisite is opened when the
/// recorded manifest is missing or its platform/architecture does not match the request.
pub fn observe_personal_worker_runtime_linkage_prerequisite(
    project: &ProjectIdentity,
    architecture: PersonalWorkerRuntimeArchitecture,
) -> Result<PersonalWorkerRuntimeLinkagePrerequisite, PersonalWorkerRuntimeLinkagePrerequisiteError>
{
    let result = with_reconfirmed_locked_personal_worker_runtime_manifest(
        project,
        |manifest| observe_sources(manifest, project, architecture),
        LinkageSources::reconfirm,
    );
    match result {
        Ok(LockedPersonalWorkerRuntimeManifestObservation::Missing) => Err(missing_error()),
        Ok(LockedPersonalWorkerRuntimeManifestObservation::Found {
            manifest,
            observation,
        }) => Ok(PersonalWorkerRuntimeLinkagePrerequisite {
            summary: PersonalWorkerRuntimeLinkagePrerequisiteSummary {
                schema_version: PERSONAL_WORKER_RUNTIME_LINKAGE_PREREQUISITE_SCHEMA_VERSION,
                disposition:
                    PersonalWorkerRuntimeLinkagePrerequisiteDisposition::ObservedPrerequisite,
                platform: PersonalWorkerRuntimePlatform::Ubuntu2404,
                architecture,
            },
            _manifest: manifest,
            _sources: observation,
        }),
        Err(LockedPersonalWorkerRuntimeManifestObservationError::State(error)) => {
            Err(map_manifest_error(error))
        }
        Err(LockedPersonalWorkerRuntimeManifestObservationError::Observer(error)) => Err(error),
    }
}

fn observe_sources(
    manifest: &PersonalWorkerRuntimeManifest,
    project: &ProjectIdentity,
    architecture: PersonalWorkerRuntimeArchitecture,
) -> Result<LinkageSources, PersonalWorkerRuntimeLinkagePrerequisiteError> {
    let summary = manifest.summary();
    if summary.platform() != PersonalWorkerRuntimePlatform::Ubuntu2404
        || summary.architecture() != architecture
    {
        return Err(identity_error());
    }
    let executables =
        observe_personal_worker_runtime_executable_prerequisite(project, architecture)
            .map_err(map_executable_error)?;
    let loader_object =
        observe_personal_worker_runtime_loader_object_prerequisite(project, architecture)
            .map_err(map_loader_object_error)?;
    let loader_state =
        observe_personal_worker_runtime_loader_state_prerequisite(project, architecture)
            .map_err(map_loader_state_error)?;
    Ok(LinkageSources {
        executables,
        loader_object,
        loader_state,
    })
}

fn map_manifest_error(
    error: PersonalWorkerRuntimeManifestDiscoveryError,
) -> PersonalWorkerRuntimeLinkagePrerequisiteError {
    match error.kind {
        PersonalWorkerRuntimeManifestDiscoveryErrorKind::Busy => busy_error(),
        PersonalWorkerRuntimeManifestDiscoveryErrorKind::RecoveryRequired => recovery_error(),
        PersonalWorkerRuntimeManifestDiscoveryErrorKind::VersionIncompatible => version_error(),
        PersonalWorkerRuntimeManifestDiscoveryErrorKind::CorruptState => corrupt_error(),
        PersonalWorkerRuntimeManifestDiscoveryErrorKind::UnsafeFilesystem => unsafe_error(),
        PersonalWorkerRuntimeManifestDiscoveryErrorKind::ChangedDuringRead => changed_error(),
        PersonalWorkerRuntimeManifestDiscoveryErrorKind::Io => io_error(),
    }
}

fn map_executable_error(
    error: PersonalWorkerRuntimeExecutablePrerequisiteError,
) -> PersonalWorkerRuntimeLinkagePrerequisiteError {
    match error.kind {
        PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::IdentityMismatch => identity_error(),
        PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::Missing => missing_error(),
        PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::UnsupportedArchitecture => {
            unsupported_error()
        }
        PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::UnsafeFilesystem => unsafe_error(),
        PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::InvalidExecutable => invalid_error(),
        PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::ChangedDuringRead => changed_error(),
        PersonalWorkerRuntimeExecutablePrerequisiteErrorKind::Io => io_error(),
    }
}

fn map_loader_object_error(
    error: PersonalWorkerRuntimeLoaderObjectPrerequisiteError,
) -> PersonalWorkerRuntimeLinkagePrerequisiteError {
    match error.kind {
        PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::IdentityMismatch => {
            identity_error()
        }
        PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::Missing => missing_error(),
        PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::UnsupportedArchitecture => {
            unsupported_error()
        }
        PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::UnsafeFilesystem => unsafe_error(),
        PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::InvalidLoader => invalid_error(),
        PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::ChangedDuringRead => {
            changed_error()
        }
        PersonalWorkerRuntimeLoaderObjectPrerequisiteErrorKind::Io => io_error(),
    }
}

fn map_loader_state_error(
    error: PersonalWorkerRuntimeLoaderStatePrerequisiteError,
) -> PersonalWorkerRuntimeLinkagePrerequisiteError {
    match error.kind {
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::IdentityMismatch => identity_error(),
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::Missing => missing_error(),
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::UnsupportedArchitecture => {
            unsupported_error()
        }
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::UnsafeFilesystem => unsafe_error(),
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::UnsafeConfiguration => {
            unsafe_configuration_error()
        }
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::VersionIncompatible => {
            version_error()
        }
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::InvalidConfiguration => {
            invalid_error()
        }
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::ChangedDuringRead => changed_error(),
        PersonalWorkerRuntimeLoaderStatePrerequisiteErrorKind::Io => io_error(),
    }
}

const fn error(
    kind: PersonalWorkerRuntimeLinkagePrerequisiteErrorKind,
    code: &'static str,
    message: &'static str,
) -> PersonalWorkerRuntimeLinkagePrerequisiteError {
    PersonalWorkerRuntimeLinkagePrerequisiteError {
        kind,
        code,
        message,
    }
}

macro_rules! fixed_error {
    ($name:ident, $kind:ident, $code:literal, $message:literal) => {
        const fn $name() -> PersonalWorkerRuntimeLinkagePrerequisiteError {
            error(
                PersonalWorkerRuntimeLinkagePrerequisiteErrorKind::$kind,
                $code,
                $message,
            )
        }
    };
}

fixed_error!(
    missing_error,
    Missing,
    "runtime_linkage_prerequisite_missing",
    "personal worker runtime linkage prerequisite is missing"
);
fixed_error!(
    busy_error,
    Busy,
    "runtime_linkage_prerequisite_busy",
    "personal worker runtime linkage prerequisite is busy"
);
fixed_error!(
    recovery_error,
    RecoveryRequired,
    "runtime_linkage_prerequisite_recovery_required",
    "personal worker runtime linkage prerequisite requires recovery"
);
fixed_error!(
    version_error,
    VersionIncompatible,
    "runtime_linkage_prerequisite_version_incompatible",
    "personal worker runtime linkage prerequisite requires an explicit version migration"
);
fixed_error!(
    corrupt_error,
    CorruptState,
    "runtime_linkage_prerequisite_corrupt",
    "personal worker runtime linkage state is corrupt or noncanonical"
);
fixed_error!(
    identity_error,
    IdentityMismatch,
    "runtime_linkage_prerequisite_identity_mismatch",
    "personal worker runtime linkage identity does not match the exact request"
);
fixed_error!(
    unsupported_error,
    UnsupportedArchitecture,
    "runtime_linkage_prerequisite_unsupported_architecture",
    "personal worker runtime linkage prerequisite is unsupported on this architecture"
);
fixed_error!(
    unsafe_error,
    UnsafeFilesystem,
    "runtime_linkage_prerequisite_unsafe_filesystem",
    "personal worker runtime linkage prerequisite has unsafe filesystem evidence"
);
fixed_error!(
    unsafe_configuration_error,
    UnsafeConfiguration,
    "runtime_linkage_prerequisite_unsafe_configuration",
    "personal worker runtime linkage prerequisite has unsafe loader configuration"
);
fixed_error!(
    invalid_error,
    InvalidPrerequisite,
    "runtime_linkage_prerequisite_invalid",
    "personal worker runtime linkage prerequisite is invalid or outside canonical bounds"
);
fixed_error!(
    changed_error,
    ChangedDuringRead,
    "runtime_linkage_prerequisite_changed",
    "personal worker runtime linkage prerequisite changed during observation"
);
fixed_error!(
    io_error,
    Io,
    "runtime_linkage_prerequisite_io",
    "personal worker runtime linkage prerequisite could not be read safely"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_failures_are_fixed_and_path_free() {
        for error in [
            missing_error(),
            busy_error(),
            recovery_error(),
            version_error(),
            corrupt_error(),
            identity_error(),
            unsupported_error(),
            unsafe_error(),
            unsafe_configuration_error(),
            invalid_error(),
            changed_error(),
            io_error(),
        ] {
            let debug = format!("{error:?}");
            assert!(!debug.contains('/'));
            assert!(!debug.contains("sha256:"));
            assert!(!error.code.is_empty());
            assert!(!error.message.is_empty());
        }
    }
}
