use std::fs::{self as std_fs, OpenOptions};
use std::io::{Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{FileExt as _, OpenOptionsExt as _, PermissionsExt as _, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::lima_observation::{
    LimaArchitecture, LimaInstanceName, LimaObservationRequest, LimaVmType,
};

const INSTANCE: &str = "smolrunner";
const CACHE_PATH: &str = "/home/runner/.cache/cargo";
const TEST_DISK_BLOCKS: u64 = 128;
const TEST_DISK_BYTES: u64 = TEST_DISK_BLOCKS * LOGICAL_BLOCK_BYTES;
const GPT_ENTRY_COUNT: u32 = 128;
const GPT_ENTRY_BYTES: u32 = 128;
const GPT_TABLE_BYTES: usize = GPT_ENTRY_COUNT as usize * GPT_ENTRY_BYTES as usize;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
    lima_home: PathBuf,
    instance: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let fixture_parent = std::env::current_dir()
            .expect("current directory")
            .join("target/lima-host-identity-fixtures");
        std_fs::create_dir_all(&fixture_parent).expect("create fixture parent");
        let root = fixture_parent.join(format!(
            "fixture-{}-{}",
            std::process::id(),
            NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed)
        ));
        let lima_home = root.join("lima-home");
        let instance = lima_home.join(INSTANCE);
        std_fs::create_dir(&root).expect("create fixture root");
        std_fs::create_dir(&lima_home).expect("create Lima home");
        std_fs::create_dir(&instance).expect("create instance directory");
        set_mode(&lima_home, 0o700);
        set_mode(&instance, 0o700);
        write_identifier(&instance.join(VZ_IDENTIFIER_FILE), 0x41);
        write_test_disk(&instance.join(ROOT_DISK_FILE), [0x11; 16]);
        Self {
            root,
            lima_home,
            instance,
        }
    }

    fn request(&self) -> LimaObservationRequest {
        LimaObservationRequest::new(
            LimaInstanceName::parse(INSTANCE).expect("instance"),
            &self.lima_home,
            LimaVmType::Vz,
            LimaArchitecture::Aarch64,
            CACHE_PATH,
            30,
        )
        .expect("request")
    }

    fn observe(&self) -> Result<LimaHostIdentityObservation, LimaHostIdentityError> {
        LimaHostIdentityAdapter.observe(&self.request())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std_fs::remove_dir_all(&self.root);
    }
}

#[test]
fn stable_identity_ignores_ordinary_guest_partition_writes() {
    let fixture = Fixture::new();
    let before = fixture.observe().expect("initial identity");
    let mut disk = OpenOptions::new()
        .write(true)
        .open(fixture.instance.join(ROOT_DISK_FILE))
        .expect("open disk body");
    disk.seek(SeekFrom::Start(40 * LOGICAL_BLOCK_BYTES))
        .expect("seek inside usable partition space");
    disk.write_all(b"ordinary guest filesystem write")
        .expect("write guest partition bytes");
    drop(disk);
    std_fs::write(
        fixture.instance.join("lima.yaml"),
        b"cpus: 8\nmemory: 8GiB\n",
    )
    .expect("write mutable profile");

    let after = fixture.observe().expect("identity after guest write");
    assert_eq!(before.identity(), after.identity());
}

#[test]
fn platform_identifier_and_gpt_changes_change_identity() {
    let fixture = Fixture::new();
    let original = fixture.observe().expect("original identity");

    write_identifier(&fixture.instance.join(VZ_IDENTIFIER_FILE), 0x42);
    let changed_platform = fixture.observe().expect("changed platform identity");
    assert_ne!(original.identity(), changed_platform.identity());

    write_test_disk(&fixture.instance.join(ROOT_DISK_FILE), [0x22; 16]);
    let changed_disk = fixture.observe().expect("changed disk identity");
    assert_ne!(changed_platform.identity(), changed_disk.identity());
}

#[test]
fn held_observation_confirmation_refuses_later_gpt_drift() {
    let fixture = Fixture::new();
    let request = fixture.request();
    let observation = fixture.observe().expect("initial identity");

    write_test_disk(&fixture.instance.join(ROOT_DISK_FILE), [0x22; 16]);

    let error = observation
        .confirm(&request)
        .expect_err("held observation must refuse changed GPT evidence");
    assert_eq!(error.kind, LimaHostIdentityErrorKind::IdentityDrift);
}

#[test]
fn missing_symlinked_hardlinked_and_writable_evidence_is_refused() {
    let missing = Fixture::new();
    std_fs::remove_file(missing.instance.join(VZ_IDENTIFIER_FILE)).expect("remove identifier");
    assert_eq!(
        missing.observe().expect_err("missing identifier").kind,
        LimaHostIdentityErrorKind::MissingEvidence
    );

    let aliased = Fixture::new();
    let identifier = aliased.instance.join(VZ_IDENTIFIER_FILE);
    let target = aliased.instance.join("identifier-target");
    std_fs::rename(&identifier, &target).expect("move identifier");
    symlink(&target, &identifier).expect("alias identifier");
    assert_eq!(
        aliased.observe().expect_err("symlinked identifier").kind,
        LimaHostIdentityErrorKind::UnsafeFilesystem
    );

    let linked = Fixture::new();
    std_fs::hard_link(
        linked.instance.join(ROOT_DISK_FILE),
        linked.instance.join("disk-copy"),
    )
    .expect("hardlink disk");
    assert_eq!(
        linked.observe().expect_err("hardlinked disk").kind,
        LimaHostIdentityErrorKind::UnsafeFilesystem
    );

    let writable = Fixture::new();
    set_mode(&writable.instance.join(ROOT_DISK_FILE), 0o666);
    assert_eq!(
        writable.observe().expect_err("writable disk").kind,
        LimaHostIdentityErrorKind::UnsafeFilesystem
    );
}

#[test]
fn malformed_identifier_and_gpt_are_refused() {
    let identifier = Fixture::new();
    let mut malformed = vz_identifier_bytes(0x44);
    malformed[8] = 0xd2;
    std_fs::write(identifier.instance.join(VZ_IDENTIFIER_FILE), malformed)
        .expect("replace identifier structure");
    assert_eq!(
        identifier.observe().expect_err("malformed identifier").kind,
        LimaHostIdentityErrorKind::MalformedIdentifier
    );

    let disk = Fixture::new();
    let mut file = OpenOptions::new()
        .write(true)
        .open(disk.instance.join(ROOT_DISK_FILE))
        .expect("open backup GPT");
    file.seek(SeekFrom::Start(
        (TEST_DISK_BLOCKS - 1) * LOGICAL_BLOCK_BYTES,
    ))
    .expect("seek backup header");
    file.write_all(b"NOT GPT!").expect("corrupt backup header");
    drop(file);
    assert_eq!(
        disk.observe().expect_err("malformed backup GPT").kind,
        LimaHostIdentityErrorKind::MalformedDisk
    );

    let header_crc = Fixture::new();
    flip_disk_byte(
        &header_crc.instance.join(ROOT_DISK_FILE),
        LOGICAL_BLOCK_BYTES + 16,
    );
    assert_eq!(
        header_crc.observe().expect_err("invalid header CRC").kind,
        LimaHostIdentityErrorKind::MalformedDisk
    );

    let table_crc = Fixture::new();
    write_disk_byte(
        &table_crc.instance.join(ROOT_DISK_FILE),
        2 * LOGICAL_BLOCK_BYTES + 32,
        0x7f,
    );
    write_disk_byte(
        &table_crc.instance.join(ROOT_DISK_FILE),
        (TEST_DISK_BLOCKS - 33) * LOGICAL_BLOCK_BYTES + 32,
        0x7f,
    );
    assert_eq!(
        table_crc.observe().expect_err("invalid table CRC").kind,
        LimaHostIdentityErrorKind::MalformedDisk
    );

    let header_padding = Fixture::new();
    write_disk_byte(
        &header_padding.instance.join(ROOT_DISK_FILE),
        LOGICAL_BLOCK_BYTES + u64::from(MIN_GPT_HEADER_BYTES),
        1,
    );
    assert_eq!(
        header_padding
            .observe()
            .expect_err("nonzero GPT header padding")
            .kind,
        LimaHostIdentityErrorKind::MalformedDisk
    );
}

#[test]
fn final_binding_check_refuses_replacement_after_confirming_reads() {
    let fixture = Fixture::new();
    let request = fixture.request();
    let lima_home = open_absolute_directory(&fixture.lima_home).expect("open Lima home");
    let lima_home_stat =
        validate_private_directory(&lima_home, "lima_home").expect("inspect Lima home");
    let instance =
        fs::openat(&lima_home, INSTANCE, directory_flags(), Mode::empty()).expect("open instance");
    let instance_stat = validate_private_directory(&instance, "instance_directory")
        .expect("inspect instance directory");
    let identifier = open_identity_file(&instance, VZ_IDENTIFIER_FILE, "platform_identifier")
        .expect("open held identifier");
    let identifier_stat = validate_identity_file(&identifier, "platform_identifier")
        .expect("inspect held identifier");
    let disk = open_identity_file(&instance, ROOT_DISK_FILE, "root_disk").expect("open held disk");
    let disk_stat = validate_identity_file(&disk, "root_disk").expect("inspect held disk");
    let disk_bytes = u64::try_from(disk_stat.st_size).expect("disk size");

    let held_evidence = HeldIdentityEvidence {
        lima_home: &lima_home,
        lima_home_stat: &lima_home_stat,
        instance_directory: &instance,
        instance_stat: &instance_stat,
        identifier: &identifier,
        identifier_stat: &identifier_stat,
        root_disk: &disk,
        root_disk_stat: &disk_stat,
    };
    held_evidence
        .verify_bindings(&request)
        .expect("initial bindings");
    read_identifier(&identifier, &identifier_stat).expect("confirm identifier");
    read_disk_evidence(&disk, disk_bytes).expect("confirm GPT evidence");

    std_fs::rename(
        fixture.instance.join(ROOT_DISK_FILE),
        fixture.instance.join("old-disk"),
    )
    .expect("move old disk");
    write_test_disk(&fixture.instance.join(ROOT_DISK_FILE), [0x22; 16]);

    let error = held_evidence
        .verify_bindings(&request)
        .expect_err("replacement after confirming reads must fail");
    assert_eq!(error.kind, LimaHostIdentityErrorKind::IdentityDrift);
}

#[test]
fn private_paths_and_raw_evidence_are_absent_from_public_surfaces() {
    let fixture = Fixture::new();
    let observation = fixture.observe().expect("identity");
    let debug = format!("{observation:?}");
    assert!(!debug.contains(fixture.root.to_str().expect("UTF-8 fixture")));
    assert!(!debug.contains("AAAAAAAA"));
    assert!(debug.contains("private_filesystem_evidence"));

    let error = serde_json::to_string(
        &LimaHostIdentityAdapter
            .observe(
                &LimaObservationRequest::new(
                    LimaInstanceName::parse(INSTANCE).expect("instance"),
                    fixture.root.join("private-missing-home"),
                    LimaVmType::Vz,
                    LimaArchitecture::Aarch64,
                    CACHE_PATH,
                    30,
                )
                .expect("missing request"),
            )
            .expect_err("missing evidence"),
    )
    .expect("serialize error");
    assert!(!error.contains(fixture.root.to_str().expect("UTF-8 fixture")));
}

#[test]
fn non_vz_backend_is_refused_before_filesystem_access() {
    let fixture = Fixture::new();
    let request = LimaObservationRequest::new(
        LimaInstanceName::parse(INSTANCE).expect("instance"),
        fixture.root.join("missing-private-home"),
        LimaVmType::Qemu,
        LimaArchitecture::Aarch64,
        CACHE_PATH,
        30,
    )
    .expect("request");
    assert_eq!(
        LimaHostIdentityAdapter
            .observe(&request)
            .expect_err("unsupported backend")
            .kind,
        LimaHostIdentityErrorKind::UnsupportedBackend
    );

    let wrong_architecture = LimaObservationRequest::new(
        LimaInstanceName::parse(INSTANCE).expect("instance"),
        fixture.root.join("missing-private-home"),
        LimaVmType::Vz,
        LimaArchitecture::X86_64,
        CACHE_PATH,
        30,
    )
    .expect("request");
    assert_eq!(
        LimaHostIdentityAdapter
            .observe(&wrong_architecture)
            .expect_err("unsupported architecture")
            .kind,
        LimaHostIdentityErrorKind::UnsupportedBackend
    );
}

#[test]
#[ignore = "requires an explicitly selected stopped local Lima instance"]
fn observes_explicit_physical_stopped_instance() {
    let lima_home = std::env::var("SMOLRUNNER_TEST_LIMA_HOME")
        .expect("set SMOLRUNNER_TEST_LIMA_HOME for the ignored physical test");
    let instance =
        std::env::var("SMOLRUNNER_TEST_LIMA_INSTANCE").unwrap_or_else(|_| INSTANCE.to_owned());
    let request = LimaObservationRequest::new(
        LimaInstanceName::parse(&instance).expect("physical instance"),
        lima_home,
        LimaVmType::Vz,
        LimaArchitecture::Aarch64,
        CACHE_PATH,
        30,
    )
    .expect("physical request");
    let observation = LimaHostIdentityAdapter
        .observe(&request)
        .expect("physical stopped host identity");
    assert!(
        observation
            .identity()
            .digest()
            .as_str()
            .starts_with("sha256:")
    );
}

fn set_mode(path: &Path, mode: u32) {
    std_fs::set_permissions(path, std_fs::Permissions::from_mode(mode)).expect("set mode");
}

fn write_identifier(path: &Path, fill: u8) {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true).mode(0o600);
    options
        .open(path)
        .expect("create identifier")
        .write_all(&vz_identifier_bytes(fill))
        .expect("write identifier");
}

fn vz_identifier_bytes(fill: u8) -> [u8; EXPECTED_VZ_IDENTIFIER_BYTES] {
    let mut bytes = [0_u8; EXPECTED_VZ_IDENTIFIER_BYTES];
    bytes[..8].copy_from_slice(VZ_IDENTIFIER_MAGIC);
    bytes[8..19].copy_from_slice(VZ_IDENTIFIER_PREFIX);
    bytes[19..35].fill(fill);
    bytes[35..38].copy_from_slice(VZ_IDENTIFIER_OFFSET_TABLE);
    bytes[44..46].copy_from_slice(&[1, 1]);
    bytes[46..54].copy_from_slice(&3_u64.to_be_bytes());
    bytes[62..70].copy_from_slice(&35_u64.to_be_bytes());
    bytes
}

fn write_test_disk(path: &Path, disk_guid: [u8; 16]) {
    let mut options = OpenOptions::new();
    options
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .mode(0o600);
    let mut disk = options.open(path).expect("create disk");
    disk.set_len(TEST_DISK_BYTES).expect("size disk");

    let mut table = vec![0_u8; GPT_TABLE_BYTES];
    table[..16].copy_from_slice(&[
        0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47, 0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d,
        0xe4,
    ]);
    table[16..32].copy_from_slice(&[0x33; 16]);
    table[32..40].copy_from_slice(&34_u64.to_le_bytes());
    table[40..48].copy_from_slice(&94_u64.to_le_bytes());

    let table_crc32 = crc32(&table);
    let primary = gpt_header(1, TEST_DISK_BLOCKS - 1, 2, disk_guid, table_crc32);
    let backup = gpt_header(
        TEST_DISK_BLOCKS - 1,
        1,
        TEST_DISK_BLOCKS - 33,
        disk_guid,
        table_crc32,
    );
    write_at(&mut disk, LOGICAL_BLOCK_BYTES, &primary);
    write_at(&mut disk, 2 * LOGICAL_BLOCK_BYTES, &table);
    write_at(
        &mut disk,
        (TEST_DISK_BLOCKS - 33) * LOGICAL_BLOCK_BYTES,
        &table,
    );
    write_at(
        &mut disk,
        (TEST_DISK_BLOCKS - 1) * LOGICAL_BLOCK_BYTES,
        &backup,
    );
}

fn gpt_header(
    current: u64,
    backup: u64,
    entries_lba: u64,
    disk_guid: [u8; 16],
    table_crc32: u32,
) -> [u8; 512] {
    let mut header = [0_u8; 512];
    header[..8].copy_from_slice(GPT_SIGNATURE);
    header[8..12].copy_from_slice(&0x0001_0000_u32.to_le_bytes());
    header[12..16].copy_from_slice(&MIN_GPT_HEADER_BYTES.to_le_bytes());
    header[24..32].copy_from_slice(&current.to_le_bytes());
    header[32..40].copy_from_slice(&backup.to_le_bytes());
    header[40..48].copy_from_slice(&34_u64.to_le_bytes());
    header[48..56].copy_from_slice(&94_u64.to_le_bytes());
    header[56..72].copy_from_slice(&disk_guid);
    header[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    header[80..84].copy_from_slice(&GPT_ENTRY_COUNT.to_le_bytes());
    header[84..88].copy_from_slice(&GPT_ENTRY_BYTES.to_le_bytes());
    header[88..92].copy_from_slice(&table_crc32.to_le_bytes());
    let header_crc32 = crc32(&header[..MIN_GPT_HEADER_BYTES as usize]);
    header[16..20].copy_from_slice(&header_crc32.to_le_bytes());
    header
}

fn write_at(file: &mut File, offset: u64, bytes: &[u8]) {
    file.seek(SeekFrom::Start(offset)).expect("seek disk");
    file.write_all(bytes).expect("write disk evidence");
}

fn write_disk_byte(path: &Path, offset: u64, byte: u8) {
    let mut file = OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open disk for corruption");
    write_at(&mut file, offset, &[byte]);
}

fn flip_disk_byte(path: &Path, offset: u64) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .expect("open disk for corruption");
    let mut byte = [0_u8; 1];
    file.read_exact_at(&mut byte, offset)
        .expect("read disk byte");
    byte[0] ^= 1;
    write_at(&mut file, offset, &byte);
}
