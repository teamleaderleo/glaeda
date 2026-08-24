use std::fs::{self, File};
use std::os::unix::fs::{PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::lima_observation::{LimaArchitecture, LimaVmType};

static NEXT_ROOT: AtomicU64 = AtomicU64::new(1);
const DISK_BYTES: u64 = 1024 * 1024;

struct Fixture {
    root: PathBuf,
    lima_home: PathBuf,
    collection: PathBuf,
    disk: PathBuf,
    backing: PathBuf,
    lock: PathBuf,
    instance: PathBuf,
    disk_name: LimaStandaloneDiskName,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "smolrunner-project-disk-observation-{}-{}",
            std::process::id(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        set_mode(&root, 0o700);
        let lima_home = root.join("lima");
        let collection = lima_home.join("opaque-collection");
        let disk_name = LimaStandaloneDiskName::parse("fixture-disk").unwrap();
        let disk = collection.join(disk_name.as_str());
        fs::create_dir(&lima_home).unwrap();
        fs::create_dir(&collection).unwrap();
        fs::create_dir(&disk).unwrap();
        for directory in [&lima_home, &collection, &disk] {
            set_mode(directory, 0o700);
        }
        let backing = disk.join("opaque-regular-entry");
        let file = File::create(&backing).unwrap();
        file.set_len(DISK_BYTES).unwrap();
        drop(file);
        set_mode(&backing, 0o600);
        let lock = disk.join("opaque-symlink-entry");
        let instance = lima_home.join("fixture-instance");
        Self {
            root,
            lima_home,
            collection,
            disk,
            backing,
            lock,
            instance,
            disk_name,
        }
    }

    fn request(&self) -> LimaStandaloneDiskObservationRequest {
        LimaStandaloneDiskObservationRequest::new(
            self.disk_name.clone(),
            self.lima_home.clone(),
            self.disk.clone(),
        )
        .unwrap()
    }

    fn detached_inventory(&self) -> Vec<u8> {
        inventory(self.disk_name.as_str(), &self.disk, "", "", DISK_BYTES)
    }

    fn attach(&self) {
        fs::create_dir(&self.instance).unwrap();
        set_mode(&self.instance, 0o700);
        symlink(&self.instance, &self.lock).unwrap();
    }

    fn attached_inventory(&self) -> Vec<u8> {
        inventory(
            self.disk_name.as_str(),
            &self.disk,
            "fixture-instance",
            self.instance.to_str().unwrap(),
            DISK_BYTES,
        )
    }

    fn lima_request(&self) -> LimaObservationRequest {
        LimaObservationRequest::new(
            LimaInstanceName::parse("fixture-instance").unwrap(),
            canonical_for_test(&self.lima_home),
            LimaVmType::Vz,
            LimaArchitecture::Aarch64,
            PathBuf::from("/var/lib/smolrunner-cache"),
            30,
        )
        .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

fn canonical_for_test(path: &Path) -> PathBuf {
    AcceptedPath::new(path.to_owned()).unwrap().physical
}

fn inventory(
    name: &str,
    directory: &Path,
    instance: &str,
    instance_directory: &str,
    size: u64,
) -> Vec<u8> {
    format!(
        "{{\"name\":\"{name}\",\"size\":{size},\"format\":\"raw\",\"dir\":\"{}\",\"instance\":\"{instance}\",\"instanceDir\":\"{instance_directory}\",\"mountPoint\":\"/mnt/fixture\"}}\n",
        directory.display()
    )
    .into_bytes()
}

fn project() -> ProjectIdentity {
    ProjectIdentity::parse("github.com/teamleaderleo/smolrunner").unwrap()
}

fn detached_record() -> ProjectDiskLeaseRecord {
    ProjectDiskLeaseRecord::new_detached(
        project(),
        ProjectDiskId::parse("disk-a").unwrap(),
        ProjectDiskGeneration::new(3).unwrap(),
    )
}

fn exact_detached_observation() -> ProjectDiskObservation {
    ProjectDiskObservation::new(
        ProjectDiskPhysicalObservation::Exact,
        ProjectDiskUseObservation::Unused,
        ProjectDiskLockObservation::Unlocked,
        ProjectDiskRecoverability::Unknown,
    )
}

fn attached_record() -> ProjectDiskLeaseRecord {
    let detached = detached_record();
    let plan = detached
        .plan_attach(
            ResidentSandboxId::parse("sandbox-a").unwrap(),
            ResidentSandboxGeneration::new(5).unwrap(),
            exact_detached_observation(),
        )
        .unwrap();
    detached
        .record_attach_success(
            &plan,
            ProjectDiskObservation::new(
                ProjectDiskPhysicalObservation::Exact,
                ProjectDiskUseObservation::CurrentAttachment,
                ProjectDiskLockObservation::CurrentAttachment,
                ProjectDiskRecoverability::Unknown,
            ),
        )
        .unwrap()
}

#[test]
fn detached_schema_binds_exact_backing_identity_and_distinct_byte_counts() {
    let fixture = Fixture::new();
    let inventory = fixture.detached_inventory();
    let mut observed = observe_lima_standalone_disk(fixture.request(), &inventory).unwrap();
    assert_eq!(
        observed.summary().disposition(),
        LimaStandaloneDiskDisposition::Detached
    );
    assert_eq!(observed.summary().backing_logical_bytes(), DISK_BYTES);
    assert!(observed.summary().backing_allocated_bytes() <= DISK_BYTES);
    assert_eq!(observed.summary().inventory_logical_bytes(), DISK_BYTES);
    assert!(observed.summary().inventory_format_raw());
    observed.confirm(&inventory).unwrap();

    let record = detached_record();
    let binding =
        ProjectDiskPhysicalBinding::new(&record, observed.summary().physical_identity().clone());
    let report = observed
        .bind_to_project_disk(&record, &binding, None, &inventory)
        .unwrap();
    assert_eq!(
        report.observation(),
        ProjectDiskObservation::new(
            ProjectDiskPhysicalObservation::Exact,
            ProjectDiskUseObservation::Unused,
            ProjectDiskLockObservation::Unlocked,
            ProjectDiskRecoverability::Unknown,
        )
    );
    assert!(!report.resident_host_identity_bound());
}

#[test]
fn attached_lock_becomes_current_only_with_exact_p1_and_resident_binding() {
    let fixture = Fixture::new();
    fixture.attach();
    let inventory = fixture.attached_inventory();
    let mut observed = observe_lima_standalone_disk(fixture.request(), &inventory).unwrap();
    assert_eq!(
        observed.summary().disposition(),
        LimaStandaloneDiskDisposition::Attached
    );

    let record = attached_record();
    let binding =
        ProjectDiskPhysicalBinding::new(&record, observed.summary().physical_identity().clone());
    let without_resident = observed
        .bind_to_project_disk(&record, &binding, None, &inventory)
        .unwrap();
    assert_eq!(
        without_resident.observation().use_state(),
        ProjectDiskUseObservation::Other
    );
    assert_eq!(
        without_resident.observation().lock_state(),
        ProjectDiskLockObservation::Other
    );

    let resident =
        ProjectDiskResidentSandboxBinding::for_test(&record, fixture.lima_request()).unwrap();
    let current = observed
        .bind_to_project_disk(&record, &binding, Some(&resident), &inventory)
        .unwrap();
    assert_eq!(
        current.observation().use_state(),
        ProjectDiskUseObservation::CurrentAttachment
    );
    assert_eq!(
        current.observation().lock_state(),
        ProjectDiskLockObservation::CurrentAttachment
    );
    assert!(current.resident_host_identity_bound());
}

#[test]
fn physical_identity_mismatch_is_conflicting_not_adopted() {
    let first = Fixture::new();
    let second = Fixture::new();
    let first_observed =
        observe_lima_standalone_disk(first.request(), &first.detached_inventory()).unwrap();
    let mut second_observed =
        observe_lima_standalone_disk(second.request(), &second.detached_inventory()).unwrap();
    let record = detached_record();
    let wrong = ProjectDiskPhysicalBinding::new(
        &record,
        first_observed.summary().physical_identity().clone(),
    );
    let report = second_observed
        .bind_to_project_disk(&record, &wrong, None, &second.detached_inventory())
        .unwrap();
    assert_eq!(
        report.observation().physical(),
        ProjectDiskPhysicalObservation::Conflicting
    );
    assert_eq!(
        report.observation().use_state(),
        ProjectDiskUseObservation::Unknown
    );
}

#[test]
fn persisted_identity_round_trips_but_stale_p1_revision_is_refused() {
    let fixture = Fixture::new();
    let inventory = fixture.detached_inventory();
    let mut observed = observe_lima_standalone_disk(fixture.request(), &inventory).unwrap();
    let identity = ProjectDiskPhysicalIdentity::parse(
        observed.summary().physical_identity().digest().as_str(),
    )
    .unwrap();
    assert_eq!(&identity, observed.summary().physical_identity());

    let record = detached_record();
    let binding = ProjectDiskPhysicalBinding::new(&record, identity);
    let successor = record.request_retire().unwrap();
    let error = observed
        .bind_to_project_disk(&successor, &binding, None, &inventory)
        .unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::BindingMismatch
    );
}

#[test]
fn planned_name_absence_is_descriptor_bound_and_detects_appearance() {
    let fixture = Fixture::new();
    let planned_name = LimaStandaloneDiskName::parse("planned-disk").unwrap();
    let planned_directory = fixture.collection.join(planned_name.as_str());
    let request = LimaStandaloneDiskObservationRequest::new(
        planned_name.clone(),
        fixture.lima_home.clone(),
        planned_directory.clone(),
    )
    .unwrap();
    let inventory = fixture.detached_inventory();
    let mut absent = observe_lima_standalone_disk_absence(request, &inventory).unwrap();
    assert!(absent.summary().disk_directory_absent());
    assert!(absent.summary().inventory_record_absent());
    absent.confirm(&inventory).unwrap();

    let request = LimaStandaloneDiskObservationRequest::new(
        planned_name.clone(),
        fixture.lima_home.clone(),
        planned_directory.clone(),
    )
    .unwrap();
    let error = observe_absence_with_hook(request, &inventory, || {
        fs::create_dir(&planned_directory).unwrap();
        set_mode(&planned_directory, 0o700);
    })
    .unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::ChangedDuringObservation
    );

    let request = LimaStandaloneDiskObservationRequest::new(
        planned_name,
        fixture.lima_home.clone(),
        planned_directory,
    )
    .unwrap();
    let error = observe_lima_standalone_disk_absence(request, &inventory).unwrap_err();
    assert_eq!(error.kind(), ProjectDiskHostObservationErrorKind::Present);
}

#[test]
fn same_name_backing_replacement_is_detected_before_return() {
    let fixture = Fixture::new();
    let inventory = fixture.detached_inventory();
    let old = fixture.disk.join("old-backing");
    let error = observe_with_hook(fixture.request(), &inventory, || {
        fs::rename(&fixture.backing, &old).unwrap();
        let replacement = File::create(&fixture.backing).unwrap();
        replacement.set_len(DISK_BYTES).unwrap();
        drop(replacement);
        set_mode(&fixture.backing, 0o600);
    })
    .unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::ChangedDuringObservation
    );
}

#[test]
fn same_name_disk_directory_rebind_is_detected_before_return() {
    let fixture = Fixture::new();
    let inventory = fixture.detached_inventory();
    let old = fixture.collection.join("old-disk");
    let error = observe_with_hook(fixture.request(), &inventory, || {
        fs::rename(&fixture.disk, &old).unwrap();
        fs::create_dir(&fixture.disk).unwrap();
        set_mode(&fixture.disk, 0o700);
        let replacement = File::create(fixture.disk.join("opaque-regular-entry")).unwrap();
        replacement.set_len(DISK_BYTES).unwrap();
        drop(replacement);
        set_mode(&fixture.disk.join("opaque-regular-entry"), 0o600);
    })
    .unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::ChangedDuringObservation
    );
}

#[test]
fn same_name_attachment_symlink_replacement_is_detected_before_return() {
    let fixture = Fixture::new();
    fixture.attach();
    let inventory = fixture.attached_inventory();
    let old = fixture.disk.join("old-attachment");
    let error = observe_with_hook(fixture.request(), &inventory, || {
        fs::rename(&fixture.lock, &old).unwrap();
        symlink(&fixture.instance, &fixture.lock).unwrap();
    })
    .unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::ChangedDuringObservation
    );
}

#[test]
fn arbitrary_intermediate_symlink_and_extra_entry_are_refused() {
    let fixture = Fixture::new();
    let alias = fixture.lima_home.join("aliased-collection");
    symlink(&fixture.collection, &alias).unwrap();
    let aliased_disk = alias.join(fixture.disk_name.as_str());
    let request = LimaStandaloneDiskObservationRequest::new(
        fixture.disk_name.clone(),
        fixture.lima_home.clone(),
        aliased_disk,
    )
    .unwrap();
    let error = observe_lima_standalone_disk(request, &fixture.detached_inventory()).unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::UnsafeFilesystem
    );

    fs::create_dir(fixture.disk.join("unexpected")).unwrap();
    let error =
        observe_lima_standalone_disk(fixture.request(), &fixture.detached_inventory()).unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::UnsupportedSchema
    );
}

#[test]
fn inventory_is_strict_external_correlation_and_never_overrides_physical_shape() {
    let fixture = Fixture::new();
    let mut unknown = String::from_utf8(fixture.detached_inventory()).unwrap();
    unknown = unknown.replace("\"mountPoint\"", "\"unknown\":1,\"mountPoint\"");
    let error = observe_lima_standalone_disk(fixture.request(), unknown.as_bytes()).unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::MalformedInventory
    );

    let conflicting = inventory(
        fixture.disk_name.as_str(),
        &fixture.disk,
        "fixture-instance",
        "",
        DISK_BYTES,
    );
    let observed = observe_lima_standalone_disk(fixture.request(), &conflicting).unwrap();
    assert_eq!(
        observed.summary().disposition(),
        LimaStandaloneDiskDisposition::Conflicting
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_var_and_private_var_aliases_bind_the_same_disk() {
    let fixture = Fixture::new();
    let physical_home = canonical_for_test(&fixture.lima_home);
    let physical_disk = canonical_for_test(&fixture.disk);
    let alias_home = PathBuf::from(physical_home.to_str().unwrap().replacen(
        "/private/var/",
        "/var/",
        1,
    ));
    let alias_disk = PathBuf::from(physical_disk.to_str().unwrap().replacen(
        "/private/var/",
        "/var/",
        1,
    ));
    let direct = observe_lima_standalone_disk(
        LimaStandaloneDiskObservationRequest::new(
            fixture.disk_name.clone(),
            physical_home,
            physical_disk,
        )
        .unwrap(),
        &fixture.detached_inventory(),
    )
    .unwrap();
    let alias = observe_lima_standalone_disk(
        LimaStandaloneDiskObservationRequest::new(
            fixture.disk_name.clone(),
            alias_home,
            alias_disk,
        )
        .unwrap(),
        &fixture.detached_inventory(),
    )
    .unwrap();
    assert_eq!(
        direct.summary().physical_identity(),
        alias.summary().physical_identity()
    );
}

#[test]
fn debug_and_public_json_do_not_expose_private_paths_or_entry_names() {
    let fixture = Fixture::new();
    let observed =
        observe_lima_standalone_disk(fixture.request(), &fixture.detached_inventory()).unwrap();
    let debug = format!("{observed:?}");
    let json = serde_json::to_string(observed.summary()).unwrap();
    for private in [
        fixture.root.to_str().unwrap(),
        fixture.backing.file_name().unwrap().to_str().unwrap(),
        fixture.collection.file_name().unwrap().to_str().unwrap(),
    ] {
        assert!(!debug.contains(private));
        assert!(!json.contains(private));
    }
}
