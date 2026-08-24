use std::fmt;
use std::os::fd::{AsFd as _, OwnedFd};

use rustix::fs::{self as rustix_fs, AtFlags, Mode, OFlags, StatxFlags, statx};
use rustix::io::Errno;
use rustix::mount::{
    FsMountFlags, FsOpenFlags, MountAttrFlags, MoveMountFlags, fsconfig_create, fsconfig_set_fd,
    fsmount, fsopen, move_mount,
};
use serde::Serialize;
use sha2::{Digest as _, Sha256};

use crate::artifact::Sha256Digest;
use crate::trusted_overlay_mount_plan::{
    TrustedOverlayMountDescriptorLease, TrustedOverlayMountExecutionDescriptors,
    TrustedOverlayMountOptionPolicy, TrustedOverlayMountPlan,
};
use crate::trusted_overlay_task_view::{
    OverlaySourceAnchorGeneration, OverlaySourceAnchorId, OverlaySourceAnchorRecord,
    OverlayTaskViewLease, OverlayTaskViewRecord,
};
use crate::trusted_project_filesystem_correlation::{
    TrustedProjectFilesystemCorrelationGeneration, TrustedProjectFilesystemCorrelationProof,
};

pub const TRUSTED_OVERLAY_MOUNT_EXECUTION_SCHEMA_VERSION: u8 = 1;
const MOUNT_ID_DOMAIN: &[u8] = b"smolrunner-trusted-overlay-mount-observation-v1\0";
const SHA256_PREFIX: &str = "sha256:";
const HEX: &[u8; 16] = b"0123456789abcdef";
const OVERLAY_FILESYSTEM: &str = "overlay";
const LOWER_KEY: &str = "lowerdir+";
const UPPER_KEY: &str = "upperdir";
const WORK_KEY: &str = "workdir";
const VISIBLE_DIRECTORY_FLAGS: OFlags = OFlags::PATH
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrustedOverlayMountExecutionReceipt {
    schema_version: u8,
    task_lease: OverlayTaskViewLease,
    source_anchor_id: OverlaySourceAnchorId,
    source_anchor_generation: OverlaySourceAnchorGeneration,
    correlation_generation: TrustedProjectFilesystemCorrelationGeneration,
    option_policy: TrustedOverlayMountOptionPolicy,
    mount_identity: Sha256Digest,
}

impl TrustedOverlayMountExecutionReceipt {
    #[must_use]
    pub const fn schema_version(&self) -> u8 {
        self.schema_version
    }

    #[must_use]
    pub const fn task_lease(&self) -> &OverlayTaskViewLease {
        &self.task_lease
    }

    #[must_use]
    pub const fn source_anchor_id(&self) -> &OverlaySourceAnchorId {
        &self.source_anchor_id
    }

    #[must_use]
    pub const fn source_anchor_generation(&self) -> OverlaySourceAnchorGeneration {
        self.source_anchor_generation
    }

    #[must_use]
    pub const fn correlation_generation(&self) -> TrustedProjectFilesystemCorrelationGeneration {
        self.correlation_generation
    }

    #[must_use]
    pub const fn option_policy(&self) -> TrustedOverlayMountOptionPolicy {
        self.option_policy
    }

    #[must_use]
    pub const fn mount_identity(&self) -> &Sha256Digest {
        &self.mount_identity
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustedOverlayMountExecutionErrorKind {
    Authority,
    Correlation,
    KernelUnavailable,
    PermissionDenied,
    MountConfiguration,
    Observation,
    RecoveryRequired,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct TrustedOverlayMountExecutionError {
    kind: TrustedOverlayMountExecutionErrorKind,
    code: &'static str,
    message: &'static str,
    recovery_required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    mount_identity: Option<Sha256Digest>,
}

impl TrustedOverlayMountExecutionError {
    #[must_use]
    pub const fn kind(&self) -> TrustedOverlayMountExecutionErrorKind {
        self.kind
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub const fn recovery_required(&self) -> bool {
        self.recovery_required
    }

    #[must_use]
    pub const fn mount_identity(&self) -> Option<&Sha256Digest> {
        self.mount_identity.as_ref()
    }
}

impl fmt::Debug for TrustedOverlayMountExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrustedOverlayMountExecutionError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .field("recovery_required", &self.recovery_required)
            .field("mount_identity", &self.mount_identity)
            .finish()
    }
}

impl fmt::Display for TrustedOverlayMountExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for TrustedOverlayMountExecutionError {}

/// Attach one sealed trusted OverlayFS view using only retained directory descriptors.
///
/// This function is physically privileged on Linux, but normal product composition cannot call it
/// successfully until #565 can mint a `TrustedProjectFilesystemCorrelationProof`. P1 intentionally
/// provides no production proof constructor.
///
/// The second plan/descriptor/correlation confirmation occurs after the filesystem context has been
/// configured with retained FDs and immediately before `FSCONFIG_CMD_CREATE`, the first operation in
/// this transaction that may cause OverlayFS to initialize its work state.
///
/// # Errors
///
/// Returns bounded path-private errors for authority/correlation drift, unavailable/denied kernel
/// mount operations, configuration failure, or post-attach observation ambiguity. A post-attach
/// failure carries `recovery_required = true` and an opaque mount identity when one was available.
pub fn execute_trusted_overlay_mount(
    plan: &TrustedOverlayMountPlan,
    descriptor_lease: &TrustedOverlayMountDescriptorLease,
    source_anchor: &OverlaySourceAnchorRecord,
    task_view: &OverlayTaskViewRecord,
    correlation: &TrustedProjectFilesystemCorrelationProof,
) -> Result<TrustedOverlayMountExecutionReceipt, TrustedOverlayMountExecutionError> {
    execute_with_backend(
        plan,
        descriptor_lease,
        source_anchor,
        task_view,
        correlation,
        &mut RustixOverlayMountBackend,
    )
}

trait OverlayMountBackend {
    type Prepared;
    type UnattachedMount;

    fn prepare(
        &mut self,
        descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
    ) -> Result<Self::Prepared, BackendError>;

    fn create_unattached(
        &mut self,
        prepared: Self::Prepared,
    ) -> Result<(Self::UnattachedMount, u64), BackendError>;

    fn attach(
        &mut self,
        mount: &Self::UnattachedMount,
        descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
    ) -> Result<(), BackendError>;

    fn visible_mount_id(
        &mut self,
        descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
    ) -> Result<u64, BackendError>;
}

fn execute_with_backend<B: OverlayMountBackend>(
    plan: &TrustedOverlayMountPlan,
    descriptor_lease: &TrustedOverlayMountDescriptorLease,
    source_anchor: &OverlaySourceAnchorRecord,
    task_view: &OverlayTaskViewRecord,
    correlation: &TrustedProjectFilesystemCorrelationProof,
    backend: &mut B,
) -> Result<TrustedOverlayMountExecutionReceipt, TrustedOverlayMountExecutionError> {
    confirm_inputs(plan, descriptor_lease, source_anchor, task_view, correlation)?;
    let descriptors = descriptor_lease.execution_descriptors();
    let prepared = backend
        .prepare(&descriptors)
        .map_err(|error| map_backend(error, false, None))?;

    // SET_FD configuration retains kernel references but does not publish a mount. Reconfirm every
    // accepted authority immediately before CREATE/FSMOUNT may initialize OverlayFS work state.
    confirm_inputs(plan, descriptor_lease, source_anchor, task_view, correlation)?;

    let (mount, mount_id) = backend
        .create_unattached(prepared)
        .map_err(|error| map_backend(error, true, None))?;
    if mount_id == 0 {
        return Err(observation_error(false, None));
    }
    let mount_identity = mount_identity(plan, correlation, mount_id)?;

    backend
        .attach(&mount, &descriptors)
        .map_err(|error| map_backend(error, true, Some(mount_identity.clone())))?;
    let visible_mount_id = backend
        .visible_mount_id(&descriptors)
        .map_err(|error| map_backend(error, true, Some(mount_identity.clone())))?;
    if visible_mount_id != mount_id {
        return Err(observation_error(true, Some(mount_identity)));
    }

    Ok(TrustedOverlayMountExecutionReceipt {
        schema_version: TRUSTED_OVERLAY_MOUNT_EXECUTION_SCHEMA_VERSION,
        task_lease: task_view.lease().clone(),
        source_anchor_id: source_anchor.binding().anchor_id().clone(),
        source_anchor_generation: source_anchor.binding().anchor_generation(),
        correlation_generation: correlation.summary().correlation_generation(),
        option_policy: plan.summary().option_policy(),
        mount_identity,
    })
}

fn confirm_inputs(
    plan: &TrustedOverlayMountPlan,
    descriptor_lease: &TrustedOverlayMountDescriptorLease,
    source_anchor: &OverlaySourceAnchorRecord,
    task_view: &OverlayTaskViewRecord,
    correlation: &TrustedProjectFilesystemCorrelationProof,
) -> Result<(), TrustedOverlayMountExecutionError> {
    plan.confirm(source_anchor, task_view)
        .map_err(|_| authority_error())?;
    descriptor_lease
        .confirm(plan, source_anchor, task_view)
        .map_err(|_| authority_error())?;
    let descriptors = descriptor_lease.execution_descriptors();
    let stat = rustix_fs::fstat(descriptors.lower).map_err(|_| observation_error(false, None))?;
    correlation
        .confirm_overlay_anchor(source_anchor.binding(), stat.st_dev)
        .map_err(|_| correlation_error())?;
    Ok(())
}

struct RustixOverlayMountBackend;

impl OverlayMountBackend for RustixOverlayMountBackend {
    type Prepared = OwnedFd;
    type UnattachedMount = OwnedFd;

    fn prepare(
        &mut self,
        descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
    ) -> Result<Self::Prepared, BackendError> {
        let context = fsopen(OVERLAY_FILESYSTEM, FsOpenFlags::FSOPEN_CLOEXEC)
            .map_err(|error| backend_error(error, BackendStage::Prepare))?;
        fsconfig_set_fd(&context, LOWER_KEY, descriptors.lower)
            .map_err(|error| backend_error(error, BackendStage::Prepare))?;
        fsconfig_set_fd(&context, UPPER_KEY, descriptors.upper)
            .map_err(|error| backend_error(error, BackendStage::Prepare))?;
        fsconfig_set_fd(&context, WORK_KEY, descriptors.work)
            .map_err(|error| backend_error(error, BackendStage::Prepare))?;
        Ok(context)
    }

    fn create_unattached(
        &mut self,
        prepared: Self::Prepared,
    ) -> Result<(Self::UnattachedMount, u64), BackendError> {
        fsconfig_create(&prepared).map_err(|error| backend_error(error, BackendStage::Create))?;
        let mount = fsmount(
            &prepared,
            FsMountFlags::FSMOUNT_CLOEXEC,
            MountAttrFlags::MOUNT_ATTR_NODEV | MountAttrFlags::MOUNT_ATTR_NOSUID,
        )
        .map_err(|error| backend_error(error, BackendStage::Create))?;
        let mount_id = mount_id(&mount).map_err(|error| backend_error(error, BackendStage::Observe))?;
        Ok((mount, mount_id))
    }

    fn attach(
        &mut self,
        mount: &Self::UnattachedMount,
        descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
    ) -> Result<(), BackendError> {
        move_mount(
            mount,
            "",
            descriptors.merged,
            "",
            MoveMountFlags::MOVE_MOUNT_F_EMPTY_PATH | MoveMountFlags::MOVE_MOUNT_T_EMPTY_PATH,
        )
        .map_err(|error| backend_error(error, BackendStage::Attach))
    }

    fn visible_mount_id(
        &mut self,
        descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
    ) -> Result<u64, BackendError> {
        let visible = rustix_fs::openat(
            descriptors.merged_parent,
            descriptors.merged_name,
            VISIBLE_DIRECTORY_FLAGS,
            Mode::empty(),
        )
        .map_err(|error| backend_error(error, BackendStage::Observe))?;
        mount_id(&visible).map_err(|error| backend_error(error, BackendStage::Observe))
    }
}

fn mount_id(fd: &impl std::os::fd::AsFd) -> rustix::io::Result<u64> {
    Ok(statx(fd, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID)?.stx_mnt_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendStage {
    Prepare,
    Create,
    Attach,
    Observe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BackendError {
    stage: BackendStage,
    class: BackendErrorClass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackendErrorClass {
    Unavailable,
    Permission,
    Other,
}

fn backend_error(error: Errno, stage: BackendStage) -> BackendError {
    let class = match error {
        Errno::NOSYS => BackendErrorClass::Unavailable,
        Errno::ACCESS | Errno::PERM => BackendErrorClass::Permission,
        _ => BackendErrorClass::Other,
    };
    BackendError { stage, class }
}

fn map_backend(
    error: BackendError,
    recovery_required: bool,
    mount_identity: Option<Sha256Digest>,
) -> TrustedOverlayMountExecutionError {
    let (kind, code, message) = match error.class {
        BackendErrorClass::Unavailable => (
            TrustedOverlayMountExecutionErrorKind::KernelUnavailable,
            "overlay_new_mount_api_unavailable",
            "the reviewed Linux OverlayFS mount API is unavailable",
        ),
        BackendErrorClass::Permission => (
            TrustedOverlayMountExecutionErrorKind::PermissionDenied,
            "overlay_mount_permission_denied",
            "current guest authority cannot create the reviewed OverlayFS mount",
        ),
        BackendErrorClass::Other => match error.stage {
            BackendStage::Observe => (
                TrustedOverlayMountExecutionErrorKind::Observation,
                "overlay_mount_observation_failed",
                "the exact OverlayFS mount identity could not be observed",
            ),
            BackendStage::Attach if recovery_required => (
                TrustedOverlayMountExecutionErrorKind::RecoveryRequired,
                "overlay_mount_attach_ambiguous",
                "OverlayFS attachment requires exact recovery observation",
            ),
            BackendStage::Prepare | BackendStage::Create | BackendStage::Attach => (
                TrustedOverlayMountExecutionErrorKind::MountConfiguration,
                "overlay_mount_operation_failed",
                "the reviewed OverlayFS mount transaction failed",
            ),
        },
    };
    TrustedOverlayMountExecutionError {
        kind,
        code,
        message,
        recovery_required,
        mount_identity,
    }
}

fn authority_error() -> TrustedOverlayMountExecutionError {
    error(
        TrustedOverlayMountExecutionErrorKind::Authority,
        "overlay_mount_authority_changed",
        "trusted OverlayFS mount authority changed before execution",
        false,
        None,
    )
}

fn correlation_error() -> TrustedOverlayMountExecutionError {
    error(
        TrustedOverlayMountExecutionErrorKind::Correlation,
        "overlay_mount_project_filesystem_unproven",
        "the exact project-disk/filesystem correlation is not proven",
        false,
        None,
    )
}

fn observation_error(
    recovery_required: bool,
    mount_identity: Option<Sha256Digest>,
) -> TrustedOverlayMountExecutionError {
    error(
        if recovery_required {
            TrustedOverlayMountExecutionErrorKind::RecoveryRequired
        } else {
            TrustedOverlayMountExecutionErrorKind::Observation
        },
        if recovery_required {
            "overlay_mount_post_attach_unproven"
        } else {
            "overlay_mount_identity_unavailable"
        },
        if recovery_required {
            "attached OverlayFS state requires exact recovery observation"
        } else {
            "the exact OverlayFS mount identity could not be observed"
        },
        recovery_required,
        mount_identity,
    )
}

const fn error(
    kind: TrustedOverlayMountExecutionErrorKind,
    code: &'static str,
    message: &'static str,
    recovery_required: bool,
    mount_identity: Option<Sha256Digest>,
) -> TrustedOverlayMountExecutionError {
    TrustedOverlayMountExecutionError {
        kind,
        code,
        message,
        recovery_required,
        mount_identity,
    }
}

fn mount_identity(
    plan: &TrustedOverlayMountPlan,
    correlation: &TrustedProjectFilesystemCorrelationProof,
    mount_id: u64,
) -> Result<Sha256Digest, TrustedOverlayMountExecutionError> {
    let mut hasher = Sha256::new();
    hasher.update(MOUNT_ID_DOMAIN);
    hasher.update(plan.kernel().mount_namespace_device().to_be_bytes());
    hasher.update(plan.kernel().mount_namespace_inode().to_be_bytes());
    hasher.update(mount_id.to_be_bytes());
    push_token(&mut hasher, plan.task_lease().task_id().as_str())?;
    hasher.update(plan.task_lease().generation().get().to_be_bytes());
    push_token(&mut hasher, plan.source_anchor().anchor_id().as_str())?;
    hasher.update(plan.source_anchor().anchor_generation().get().to_be_bytes());
    hasher.update(
        correlation
            .summary()
            .correlation_generation()
            .get()
            .to_be_bytes(),
    );
    digest_to_sha256(hasher.finalize().as_slice())
}

fn push_token(
    hasher: &mut Sha256,
    value: &str,
) -> Result<(), TrustedOverlayMountExecutionError> {
    let length = u32::try_from(value.len()).map_err(|_| observation_error(false, None))?;
    hasher.update(length.to_be_bytes());
    hasher.update(value.as_bytes());
    Ok(())
}

fn digest_to_sha256(bytes: &[u8]) -> Result<Sha256Digest, TrustedOverlayMountExecutionError> {
    let mut value = String::with_capacity(SHA256_PREFIX.len() + bytes.len() * 2);
    value.push_str(SHA256_PREFIX);
    for byte in bytes {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Sha256Digest::parse(&value).map_err(|_| observation_error(false, None))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::MetadataExt as _;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{
        BackendError, BackendErrorClass, BackendStage, OverlayMountBackend,
        TrustedOverlayMountExecutionDescriptors, TrustedOverlayMountExecutionErrorKind,
        execute_with_backend,
    };
    use crate::artifact::{CommitId, GitTreeId, Sha256Digest};
    use crate::project_catalog::ProjectIdentity;
    use crate::project_disk_lease::{
        ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId,
        ResidentSandboxGeneration, ResidentSandboxId,
    };
    use crate::trusted_overlay_mount_plan::{
        TrustedOverlayMountPaths, observe_trusted_overlay_mount_plan,
    };
    use crate::trusted_overlay_task_view::{
        OverlayGitProofObservation, OverlayIndexObservation, OverlayLinkedWorktreeObservation,
        OverlayMountObservation, OverlaySourceAnchorBinding, OverlaySourceAnchorGeneration,
        OverlaySourceAnchorId, OverlaySourceAnchorRecord, OverlayTaskProcessObservation,
        OverlayTaskViewGeneration, OverlayTaskViewId, OverlayTaskViewLease,
        OverlayTaskViewObservation, OverlayTaskViewRecord,
    };
    use crate::trusted_project_filesystem_correlation::{
        TrustedProjectFilesystemCorrelationGeneration, TrustedProjectFilesystemCorrelationProof,
    };

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

    struct Fixture {
        root: PathBuf,
        work: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "smolrunner-overlay-execution-{}-{id}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&root);
            for name in ["lower", "upper", "work", "merged"] {
                fs::create_dir_all(root.join(name)).unwrap();
            }
            Self {
                work: root.join("work"),
                root,
            }
        }

        fn paths(&self) -> TrustedOverlayMountPaths {
            TrustedOverlayMountPaths::new(
                self.root.join("lower"),
                self.root.join("upper"),
                self.root.join("work"),
                self.root.join("merged"),
            )
            .unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn anchor_binding() -> OverlaySourceAnchorBinding {
        OverlaySourceAnchorBinding::new(
            ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap(),
            ProjectDiskId::parse("disk-a").unwrap(),
            ProjectDiskGeneration::new(3).unwrap(),
            ResidentSandboxId::parse("sandbox-a").unwrap(),
            ResidentSandboxGeneration::new(11).unwrap(),
            OverlaySourceAnchorId::parse("anchor-a").unwrap(),
            OverlaySourceAnchorGeneration::new(5).unwrap(),
            CommitId::parse("0123456789abcdef0123456789abcdef01234567").unwrap(),
            GitTreeId::parse("89abcdef0123456789abcdef0123456789abcdef").unwrap(),
            Sha256Digest::parse(
                "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .unwrap(),
        )
    }

    fn authority() -> (OverlaySourceAnchorRecord, OverlayTaskViewRecord) {
        let binding = anchor_binding();
        let lease = OverlayTaskViewLease::new(
            OverlayTaskViewId::parse("task-a").unwrap(),
            OverlayTaskViewGeneration::new(7).unwrap(),
        );
        let anchor = OverlaySourceAnchorRecord::new_ready(binding.clone())
            .acquire_task(lease.clone())
            .unwrap();
        let task = OverlayTaskViewRecord::new_planned(lease, binding)
            .record_worktree_registered(OverlayTaskViewObservation::new(
                OverlayLinkedWorktreeObservation::Exact,
                OverlayMountObservation::Absent,
                OverlayIndexObservation::Absent,
                OverlayGitProofObservation::NotRun,
                OverlayTaskProcessObservation::Absent,
            ))
            .unwrap();
        (anchor, task)
    }

    fn correlation(
        anchor: &OverlaySourceAnchorRecord,
        device: u64,
    ) -> TrustedProjectFilesystemCorrelationProof {
        TrustedProjectFilesystemCorrelationProof::for_test(
            anchor.binding(),
            ProjectDiskAttachmentGeneration::new(9).unwrap(),
            TrustedProjectFilesystemCorrelationGeneration::new(13).unwrap(),
            device,
        )
    }

    struct FakeBackend {
        mount_id: u64,
        visible_id: u64,
        mutate_during_prepare: Option<PathBuf>,
        prepared: bool,
        created: bool,
        attached: bool,
        fail_attach: bool,
    }

    impl FakeBackend {
        fn successful() -> Self {
            Self {
                mount_id: 41,
                visible_id: 41,
                mutate_during_prepare: None,
                prepared: false,
                created: false,
                attached: false,
                fail_attach: false,
            }
        }
    }

    impl OverlayMountBackend for FakeBackend {
        type Prepared = ();
        type UnattachedMount = ();

        fn prepare(
            &mut self,
            _descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
        ) -> Result<Self::Prepared, BackendError> {
            self.prepared = true;
            if let Some(path) = self.mutate_during_prepare.as_ref() {
                fs::write(path.join("late"), b"x").unwrap();
            }
            Ok(())
        }

        fn create_unattached(
            &mut self,
            _prepared: Self::Prepared,
        ) -> Result<(Self::UnattachedMount, u64), BackendError> {
            self.created = true;
            Ok(((), self.mount_id))
        }

        fn attach(
            &mut self,
            _mount: &Self::UnattachedMount,
            _descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
        ) -> Result<(), BackendError> {
            if self.fail_attach {
                return Err(BackendError {
                    stage: BackendStage::Attach,
                    class: BackendErrorClass::Other,
                });
            }
            self.attached = true;
            Ok(())
        }

        fn visible_mount_id(
            &mut self,
            _descriptors: &TrustedOverlayMountExecutionDescriptors<'_>,
        ) -> Result<u64, BackendError> {
            Ok(self.visible_id)
        }
    }

    fn inputs(
        fixture: &Fixture,
    ) -> (
        OverlaySourceAnchorRecord,
        OverlayTaskViewRecord,
        crate::trusted_overlay_mount_plan::TrustedOverlayMountPlan,
        crate::trusted_overlay_mount_plan::TrustedOverlayMountDescriptorLease,
        TrustedProjectFilesystemCorrelationProof,
    ) {
        let (anchor, task) = authority();
        let plan = observe_trusted_overlay_mount_plan(&anchor, &task, fixture.paths()).unwrap();
        let lease = plan.open_descriptor_lease(&anchor, &task).unwrap();
        let device = fs::metadata(fixture.root.join("lower")).unwrap().dev();
        let proof = correlation(&anchor, device);
        (anchor, task, plan, lease, proof)
    }

    #[test]
    fn fake_backend_accepts_exact_descriptor_bound_transaction() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, proof) = inputs(&fixture);
        let mut backend = FakeBackend::successful();
        let receipt = execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend)
            .unwrap();
        assert!(backend.prepared && backend.created && backend.attached);
        assert_eq!(receipt.task_lease(), task.lease());
        assert_eq!(receipt.correlation_generation().get(), 13);
        assert!(receipt.mount_identity().as_str().starts_with("sha256:"));
    }

    #[test]
    fn second_confirmation_blocks_drift_before_first_overlay_mutation() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, proof) = inputs(&fixture);
        let mut backend = FakeBackend::successful();
        backend.mutate_during_prepare = Some(fixture.work.clone());
        let error = execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend)
            .unwrap_err();
        assert_eq!(error.kind(), TrustedOverlayMountExecutionErrorKind::Authority);
        assert!(backend.prepared);
        assert!(!backend.created);
        assert!(!backend.attached);
    }

    #[test]
    fn correlation_mismatch_blocks_backend_before_prepare() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, _) = inputs(&fixture);
        let proof = correlation(&anchor, u64::MAX);
        let mut backend = FakeBackend::successful();
        let error = execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend)
            .unwrap_err();
        assert_eq!(error.kind(), TrustedOverlayMountExecutionErrorKind::Correlation);
        assert!(!backend.prepared);
    }

    #[test]
    fn visible_mount_id_mismatch_returns_recovery_debt() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, proof) = inputs(&fixture);
        let mut backend = FakeBackend::successful();
        backend.visible_id = 42;
        let error = execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend)
            .unwrap_err();
        assert_eq!(
            error.kind(),
            TrustedOverlayMountExecutionErrorKind::RecoveryRequired
        );
        assert!(error.recovery_required());
        assert!(error.mount_identity().is_some());
        assert!(backend.attached);
    }

    #[test]
    fn attach_failure_is_recovery_debt_with_mount_identity() {
        let fixture = Fixture::new();
        let (anchor, task, plan, lease, proof) = inputs(&fixture);
        let mut backend = FakeBackend::successful();
        backend.fail_attach = true;
        let error = execute_with_backend(&plan, &lease, &anchor, &task, &proof, &mut backend)
            .unwrap_err();
        assert!(error.recovery_required());
        assert!(error.mount_identity().is_some());
    }
}
