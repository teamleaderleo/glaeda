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

    fn new_clean_home() -> Self {
        let mut fixture = Self::new();
        fs::remove_file(&fixture.backing).unwrap();
        fs::remove_dir(&fixture.disk).unwrap();
        fs::remove_dir(&fixture.collection).unwrap();
        let collection = fixture.lima_home.join("_disks");
        fixture.collection = collection.clone();
        fixture.disk = collection.join(fixture.disk_name.as_str());
        fixture.backing = fixture.disk.join("opaque-regular-entry");
        fixture.lock = fixture.disk.join("opaque-symlink-entry");
        fixture
    }

    fn external_first_create(&self) {
        fs::create_dir(&self.collection).unwrap();
        set_mode(&self.collection, 0o700);
        self.create_disk_child();
    }

    fn create_disk_child(&self) {
        fs::create_dir(&self.disk).unwrap();
        set_mode(&self.disk, 0o700);
        self.recreate_backing();
    }

    fn recreate_backing(&self) {
        let file = File::create(&self.backing).unwrap();
        file.set_len(DISK_BYTES).unwrap();
        drop(file);
        set_mode(&self.backing, 0o600);
    }

    fn request(&self) -> LimaStandaloneDiskObservationRequest {
        LimaStandaloneDiskObservationRequest::new(
            self.disk_name.clone(),
            self.lima_home.clone(),
            self.disk.clone(),
        )
        .unwrap()
    }

    fn planned_request(&self) -> LimaStandaloneDiskObservationRequest {
        self.request()
            .with_planned_source_identity(test_source_identity())
    }

    fn detached_inventory(&self) -> Vec<u8> {
        self.detached_inventory_with_size(DISK_BYTES)
    }

    fn detached_inventory_with_size(&self, size: u64) -> Vec<u8> {
        inventory(self.disk_name.as_str(), &self.disk, "", "", size)
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

fn test_source_identity() -> ProjectDiskLimaSourceIdentity {
    ProjectDiskLimaSourceIdentity::parse(&format!("sha256:{}", "a".repeat(64))).unwrap()
}

fn other_planned_name() -> LimaStandaloneDiskName {
    LimaStandaloneDiskName::parse("planned-absent-disk").unwrap()
}

fn absence_for(
    fixture: &Fixture,
    planned_name: &LimaStandaloneDiskName,
    inventory: &[u8],
) -> LimaStandaloneDiskAbsenceObservation {
    observe_lima_standalone_disk_absence(
        LimaStandaloneDiskObservationRequest::new(
            planned_name.clone(),
            fixture.lima_home.clone(),
            fixture.collection.join(planned_name.as_str()),
        )
        .unwrap(),
        inventory,
    )
    .unwrap()
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

#[test]
fn first_disk_bootstrap_proves_collection_absence_then_observes_created() {
    let fixture = Fixture::new_clean_home();
    let inventory = fixture.detached_inventory();
    let absent = absence_for(&fixture, &fixture.disk_name, &[]);
    assert!(absent.summary().proven_collection_absent());
    assert!(!absent.summary().retained_collection_descriptor());
    assert!(absent.summary().retained_lima_home_descriptor());
    assert!(absent.summary().disk_directory_absent());
    assert!(absent.summary().inventory_record_absent());

    fixture.external_first_create();
    let mut created = absent.observe_created(&inventory).unwrap();
    assert_eq!(
        created.summary().disposition(),
        LimaStandaloneDiskDisposition::Detached
    );
    assert_eq!(created.summary().backing_logical_bytes(), DISK_BYTES);
    assert!(created.summary().backing_allocated_bytes() <= DISK_BYTES);
    created.confirm(&inventory).unwrap();

    let retained = Fixture::new();
    let bound = absence_for(&retained, &other_planned_name(), &[]);
    assert!(!bound.summary().proven_collection_absent());
    assert!(bound.summary().retained_collection_descriptor());
}

#[test]
fn foreign_collection_appearance_during_bootstrap_confirm_refuses() {
    let fixture = Fixture::new_clean_home();
    let error = observe_absence_with_hook(
        LimaStandaloneDiskObservationRequest::new(
            fixture.disk_name.clone(),
            fixture.lima_home.clone(),
            fixture.collection.join(fixture.disk_name.as_str()),
        )
        .unwrap(),
        &[],
        || {
            fixture.external_first_create();
        },
    )
    .unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::ChangedDuringObservation
    );
}

#[test]
fn created_observation_fails_while_collection_still_absent() {
    let fixture = Fixture::new_clean_home();
    let absent = absence_for(&fixture, &fixture.disk_name, &[]);
    let error = absent.observe_created(b"").unwrap_err();
    assert_eq!(error.kind(), ProjectDiskHostObservationErrorKind::Missing);

    let fixture = Fixture::new_clean_home();
    let inventory = fixture.detached_inventory();
    let absent = absence_for(&fixture, &fixture.disk_name, &[]);
    let error = absent.observe_created(&inventory).unwrap_err();
    assert_eq!(error.kind(), ProjectDiskHostObservationErrorKind::Missing);
}

#[test]
fn created_observation_refuses_symlinked_or_unsafe_new_collection() {
    let fixture = Fixture::new_clean_home();
    let inventory = fixture.detached_inventory();
    let absent = absence_for(&fixture, &fixture.disk_name, &[]);
    let outside = fixture.root.join("outside-disks");
    fs::create_dir(&outside).unwrap();
    set_mode(&outside, 0o700);
    symlink(&outside, &fixture.collection).unwrap();
    let error = absent.observe_created(&inventory).unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::UnsafeFilesystem
    );

    let fixture = Fixture::new_clean_home();
    let inventory = fixture.detached_inventory();
    let absent = absence_for(&fixture, &fixture.disk_name, &[]);
    fixture.external_first_create();
    set_mode(&fixture.collection, 0o777);
    let error = absent.observe_created(&inventory).unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::UnsafeFilesystem
    );
}

#[test]
fn home_rebind_between_absence_and_created_fails_for_both_lineages() {
    for clean in [true, false] {
        let fixture = if clean {
            Fixture::new_clean_home()
        } else {
            Fixture::new()
        };
        let inventory = fixture.detached_inventory();
        let absent = absence_for(&fixture, &other_planned_name(), &[]);
        if clean {
            fixture.external_first_create();
        }
        let moved = fixture.root.join("moved-home");
        fs::rename(&fixture.lima_home, &moved).unwrap();
        let error = absent.observe_created(&inventory).unwrap_err();
        assert_eq!(
            error.kind(),
            ProjectDiskHostObservationErrorKind::ChangedDuringObservation,
            "clean={clean}"
        );
    }
}

#[test]
fn child_and_backing_replacement_during_created_observation_fails() {
    let fixture = Fixture::new_clean_home();
    let inventory = fixture.detached_inventory();
    let absent = absence_for(&fixture, &fixture.disk_name, &[]);
    fixture.external_first_create();
    let old_disk = fixture.collection.join("old-created");
    let error = absent
        .observe_created_with_hook(&inventory, || {
            fs::rename(&fixture.disk, &old_disk).unwrap();
            fixture.create_disk_child();
        })
        .unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::ChangedDuringObservation
    );

    let fixture = Fixture::new_clean_home();
    let inventory = fixture.detached_inventory();
    let absent = absence_for(&fixture, &fixture.disk_name, &[]);
    fixture.external_first_create();
    let error = absent
        .observe_created_with_hook(&inventory, || {
            fs::remove_file(&fixture.backing).unwrap();
            fixture.recreate_backing();
        })
        .unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::ChangedDuringObservation
    );
}

#[test]
fn confirm_source_path_binding_detects_rebind_and_holds_stable() {
    let fixture = Fixture::new_clean_home();
    let absent = absence_for(&fixture, &fixture.disk_name, &[]);
    absent.confirm_source_path_binding().unwrap();
    fixture.external_first_create();
    absent.confirm_source_path_binding().unwrap();

    let moved = fixture.root.join("moved-home");
    fs::rename(&fixture.lima_home, &moved).unwrap();
    let error = absent.confirm_source_path_binding().unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::ChangedDuringObservation
    );
}

#[test]
fn source_path_binding_refuses_same_inode_replacement_by_mode_or_owner_drift() {
    let fixture = Fixture::new_clean_home();
    let absent = absence_for(&fixture, &other_planned_name(), &[]);
    set_mode(&fixture.lima_home, 0o755);
    let error = absent.confirm_source_path_binding().unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::ChangedDuringObservation
    );
}

#[test]
fn created_observation_requires_fresh_strict_matching_inventory() {
    let malformed_fixture = Fixture::new_clean_home();
    let absent = absence_for(&malformed_fixture, &malformed_fixture.disk_name, &[]);
    malformed_fixture.external_first_create();
    let error = absent.observe_created(b"{not-json}\n").unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::MalformedInventory
    );

    let duplicate_fixture = Fixture::new_clean_home();
    let duplicated = [
        duplicate_fixture.detached_inventory(),
        duplicate_fixture.detached_inventory(),
    ]
    .concat();
    let absent = absence_for(&duplicate_fixture, &duplicate_fixture.disk_name, &[]);
    duplicate_fixture.external_first_create();
    let error = absent.observe_created(&duplicated).unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::DuplicateInventory
    );

    let empty_fixture = Fixture::new_clean_home();
    let absent = absence_for(&empty_fixture, &empty_fixture.disk_name, &[]);
    empty_fixture.external_first_create();
    let error = absent.observe_created(b"").unwrap_err();
    assert_eq!(error.kind(), ProjectDiskHostObservationErrorKind::Missing);

    let unsupported_fixture = Fixture::new_clean_home();
    let unsupported = format!(
        "{{\"name\":\"{}\",\"size\":{DISK_BYTES},\"format\":\"qcow2\",\"dir\":\"{}\",\"instance\":\"\",\"instanceDir\":\"\",\"mountPoint\":\"/mnt/fixture\"}}\n",
        unsupported_fixture.disk_name.as_str(),
        unsupported_fixture.disk.display()
    )
    .into_bytes();
    let absent = absence_for(&unsupported_fixture, &unsupported_fixture.disk_name, &[]);
    unsupported_fixture.external_first_create();
    let error = absent.observe_created(&unsupported).unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::UnsupportedSchema
    );
}

#[test]
fn created_attached_and_conflicting_results_remain_unbound_evidence() {
    let fixture = Fixture::new_clean_home();
    let attached = fixture.attached_inventory();
    let absent = absence_for(&fixture, &fixture.disk_name, &[]);
    fixture.external_first_create();
    fixture.attach();
    let mut created = absent.observe_created(&attached).unwrap();
    assert_eq!(
        created.summary().disposition(),
        LimaStandaloneDiskDisposition::Attached
    );
    created.confirm(&attached).unwrap();

    let fixture = Fixture::new_clean_home();
    let doubled = fixture.detached_inventory_with_size(DISK_BYTES * 2);
    let absent = absence_for(&fixture, &fixture.disk_name, &[]);
    fixture.external_first_create();
    let mut created = absent.observe_created(&doubled).unwrap();
    assert_eq!(
        created.summary().disposition(),
        LimaStandaloneDiskDisposition::Conflicting
    );
    created.confirm(&doubled).unwrap();
}

#[test]
fn consuming_transition_is_single_use_by_ownership() {
    fn consume(
        lease: LimaStandaloneDiskAbsenceObservation,
        inventory: &[u8],
    ) -> Result<LimaStandaloneDiskObservation, ProjectDiskHostObservationError> {
        lease.observe_created(inventory)
    }

    let fixture = Fixture::new_clean_home();
    let inventory = fixture.detached_inventory();
    let absent = absence_for(&fixture, &fixture.disk_name, &[]);
    fixture.external_first_create();
    let created = consume(absent, &inventory).unwrap();
    assert_eq!(
        created.summary().disposition(),
        LimaStandaloneDiskDisposition::Detached
    );

    let fixture = Fixture::new_clean_home();
    let absent = absence_for(&fixture, &fixture.disk_name, &[]);
    let error = consume(absent, b"").unwrap_err();
    assert_eq!(error.kind(), ProjectDiskHostObservationErrorKind::Missing);
}

#[cfg(target_os = "macos")]
#[test]
fn darwin_alias_revalidation_refuses_drifted_alias_evidence() {
    let mut accepted = AcceptedPath::new(PathBuf::from("/var/tmp")).unwrap();
    assert!(accepted.darwin_var_alias.is_some());
    accepted.revalidate_alias().unwrap();
    if let Some(expected) = accepted.darwin_var_alias.as_mut() {
        expected.inode = expected.inode.wrapping_add(1);
    }
    let error = accepted.revalidate_alias().unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::ChangedDuringObservation
    );
}

#[test]
fn durability_target_requires_planned_origin_and_current_evidence() {
    let fixture = Fixture::new();
    let inventory = fixture.detached_inventory();
    let mut observed = observe_lima_standalone_disk(fixture.request(), &inventory).unwrap();
    let error = observed
        .project_disk_create_durability_target(&inventory)
        .unwrap_err();
    assert_eq!(
        error.code(),
        "project_disk_create_durability_target_unavailable"
    );

    let mut planned = observe_lima_standalone_disk(fixture.planned_request(), &inventory).unwrap();
    let target = planned
        .project_disk_create_durability_target(&inventory)
        .unwrap();
    assert_eq!(target.source_identity(), &test_source_identity());

    fs::remove_file(&fixture.backing).unwrap();
    fixture.recreate_backing();
    let error = planned
        .project_disk_create_durability_target(&inventory)
        .unwrap_err();
    assert_eq!(
        error.kind(),
        ProjectDiskHostObservationErrorKind::ChangedDuringObservation
    );
}

#[test]
fn durability_targets_bind_exact_distinct_disks_and_attempts() {
    use std::os::unix::fs::MetadataExt as _;

    let first = Fixture::new();
    let second = Fixture::new();
    let mut first_observed =
        observe_lima_standalone_disk(first.planned_request(), &first.detached_inventory()).unwrap();
    let mut second_observed =
        observe_lima_standalone_disk(second.planned_request(), &second.detached_inventory())
            .unwrap();
    let first_target = first_observed
        .project_disk_create_durability_target(&first.detached_inventory())
        .unwrap();
    let second_target = second_observed
        .project_disk_create_durability_target(&second.detached_inventory())
        .unwrap();
    assert_ne!(
        first_target.physical_identity(),
        second_target.physical_identity()
    );
    assert_ne!(
        first_target.backing_identity(),
        second_target.backing_identity()
    );
    assert_eq!(
        rustix_fs::fstat(first_target.held_backing_descriptor())
            .unwrap()
            .st_ino,
        fs::metadata(&first.backing).unwrap().ino()
    );
    assert_eq!(
        rustix_fs::fstat(second_target.held_backing_descriptor())
            .unwrap()
            .st_ino,
        fs::metadata(&second.backing).unwrap().ino()
    );
}

#[test]
fn persisted_source_digest_cannot_mint_live_capability_or_expose_paths() {
    let identity = test_source_identity();
    let parsed = ProjectDiskLimaSourceIdentity::parse(identity.digest().as_str()).unwrap();
    assert_eq!(parsed, identity);

    let fixture = Fixture::new();
    let inventory = fixture.detached_inventory();
    let mut observed = observe_lima_standalone_disk(fixture.request(), &inventory).unwrap();
    let error = observed
        .project_disk_create_durability_target(&inventory)
        .unwrap_err();
    assert_eq!(
        error.code(),
        "project_disk_create_durability_target_unavailable"
    );

    let debug = format!("{identity:?}");
    assert!(!debug.contains(fixture.root.to_str().unwrap()));
}

#[test]
fn absence_created_and_error_output_redact_private_paths() {
    let fixture = Fixture::new_clean_home();
    let inventory = fixture.detached_inventory();
    let absent = absence_for(&fixture, &fixture.disk_name, &[]);
    let absence_debug = format!("{absent:?}");
    let absence_json = serde_json::to_string(absent.summary()).unwrap();
    fixture.external_first_create();
    let created = absent.observe_created(&inventory).unwrap();
    let created_debug = format!("{created:?}");
    let created_json = serde_json::to_string(created.summary()).unwrap();

    let rebound_request = LimaStandaloneDiskObservationRequest::new(
        fixture.disk_name.clone(),
        fixture.lima_home.clone(),
        fixture.collection.join(fixture.disk_name.as_str()),
    )
    .unwrap();
    let moved = fixture.root.join("moved-for-redaction");
    fs::rename(&fixture.lima_home, &moved).unwrap();
    let error = observe_created_rebind_probe(rebound_request, &inventory).unwrap_err();
    let error_text = format!("{error:?} {error}");

    for private in [
        fixture.root.to_str().unwrap(),
        "opaque-regular-entry",
        "opaque-symlink-entry",
    ] {
        assert!(!absence_debug.contains(private));
        assert!(!absence_json.contains(private));
        assert!(!created_debug.contains(private));
        assert!(!created_json.contains(private));
        assert!(!error_text.contains(private));
    }
}

fn observe_created_rebind_probe(
    request: LimaStandaloneDiskObservationRequest,
    inventory: &[u8],
) -> Result<LimaStandaloneDiskObservation, ProjectDiskHostObservationError> {
    let lease = observe_lima_standalone_disk_absence(request, inventory)?;
    lease.observe_created(inventory)
}
