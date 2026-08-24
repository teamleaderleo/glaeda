use std::collections::BTreeSet;
use std::path::{Component, Path};

use serde_json::Value;
use smolrunner::artifact::Sha256Digest;
use smolrunner::project_catalog::ProjectIdentity;
use smolrunner::project_disk_lease::{
    ProjectDiskAttachmentGeneration, ProjectDiskGeneration, ProjectDiskId, ProjectDiskRevision,
    ResidentSandboxGeneration, ResidentSandboxId,
};

use super::*;

pub(super) fn validate_document(
    document: ProjectDiskPhysicalReceiptDocument,
) -> Result<ProjectDiskPhysicalReceipt, ProjectDiskPhysicalReceiptError> {
    if document.schema_version != PROJECT_DISK_PHYSICAL_RECEIPT_SCHEMA_VERSION {
        return Err(error(
            ProjectDiskPhysicalReceiptErrorKind::UnsupportedSchema,
            "schema_version",
            "project_disk_receipt_schema_unsupported",
            "project disk physical receipt schema version is unsupported",
        ));
    }
    if document.captured_at_unix_millis == 0 || !valid_git_commit(&document.repo_commit) {
        return Err(invalid_field("capture"));
    }
    validate_absolute_path(&document.lima.lima_home, "lima.lima_home")?;
    validate_absolute_path(&document.disk_directory.path, "disk_directory.path")?;
    validate_absolute_path(&document.guest.project_mount, "guest.project_mount")?;
    let lima_home = Path::new(&document.lima.lima_home);
    let disk_directory = Path::new(&document.disk_directory.path);
    if disk_directory == lima_home || disk_directory.strip_prefix(lima_home).is_err() {
        return Err(invalid_field("disk_directory.path"));
    }

    ProjectIdentity::parse(&document.declared_binding.project_identity)
        .map_err(|_| invalid_field("declared_binding.project_identity"))?;
    ProjectDiskId::parse(&document.declared_binding.project_disk_id)
        .map_err(|_| invalid_field("declared_binding.project_disk_id"))?;
    ProjectDiskGeneration::new(document.declared_binding.project_disk_generation)
        .map_err(|_| invalid_field("declared_binding.project_disk_generation"))?;
    ProjectDiskRevision::new(document.declared_binding.project_disk_revision)
        .map_err(|_| invalid_field("declared_binding.project_disk_revision"))?;
    ProjectDiskAttachmentGeneration::new(document.declared_binding.attachment_generation)
        .map_err(|_| invalid_field("declared_binding.attachment_generation"))?;
    ResidentSandboxId::parse(&document.declared_binding.resident_sandbox_id)
        .map_err(|_| invalid_field("declared_binding.resident_sandbox_id"))?;
    ResidentSandboxGeneration::new(document.declared_binding.resident_sandbox_generation)
        .map_err(|_| invalid_field("declared_binding.resident_sandbox_generation"))?;
    validate_locator(&document.lima.disk_name, "lima.disk_name")?;
    validate_locator(
        &document.lima.resident_sandbox_instance,
        "lima.resident_sandbox_instance",
    )?;
    Sha256Digest::parse(&document.lima.host_identity_digest)
        .map_err(|_| invalid_field("lima.host_identity_digest"))?;

    validate_command(&document.lima.limactl_version, "lima.limactl_version")?;
    validate_command(&document.lima.disk_list_json, "lima.disk_list_json")?;
    validate_command(&document.lima.instance_list_json, "lima.instance_list_json")?;
    validate_command(&document.guest.mountinfo, "guest.mountinfo")?;
    validate_command(
        &document.guest.block_devices_json,
        "guest.block_devices_json",
    )?;
    validate_command_contract(&document)?;
    validate_json_stdout(&document.lima.disk_list_json, "lima.disk_list_json")?;
    validate_json_stdout(&document.lima.instance_list_json, "lima.instance_list_json")?;
    validate_json_stdout(
        &document.guest.block_devices_json,
        "guest.block_devices_json",
    )?;
    validate_directory(&document.disk_directory)?;
    let guest_filesystem_device = parse_guest_mount(
        &document.guest.mountinfo.stdout,
        &document.guest.project_mount,
    )?;

    Ok(ProjectDiskPhysicalReceipt {
        document,
        guest_filesystem_device,
    })
}

fn validate_directory(
    directory: &ProjectDiskDirectoryEvidenceDocument,
) -> Result<(), ProjectDiskPhysicalReceiptError> {
    if directory.before != directory.after {
        return Err(changed("disk_directory"));
    }
    if directory.before.mode & 0o170000 != 0o040000 {
        return Err(invalid_field("disk_directory"));
    }
    validate_snapshot(&directory.before, "disk_directory")?;
    validate_snapshot(&directory.after, "disk_directory")?;
    if directory.entries.is_empty()
        || directory.entries.len() > MAX_PROJECT_DISK_RECEIPT_ENTRY_COUNT
        || directory.after_entry_names_hex.len() != directory.entries.len()
    {
        return Err(invalid_field("disk_directory.entries"));
    }

    let mut names = BTreeSet::new();
    for entry in &directory.entries {
        let name = decode_entry_name(
            &entry.name_hex,
            "disk_directory.entries.name_hex",
        )?;
        if !names.insert(name.clone()) {
            return Err(duplicate_entry("disk_directory.entries.name_hex"));
        }
        match (std::str::from_utf8(&name), entry.name_utf8.as_deref()) {
            (Ok(expected), Some(observed)) if expected == observed => {}
            (Err(_), None) => {}
            _ => return Err(invalid_field("disk_directory.entries.name_utf8")),
        }
        validate_snapshot(&entry.before, "disk_directory.entries")?;
        validate_snapshot(&entry.after, "disk_directory.entries")?;
        validate_entry_kind(entry)?;
    }

    let mut after_names = BTreeSet::new();
    for name_hex in &directory.after_entry_names_hex {
        let name = decode_entry_name(
            name_hex,
            "disk_directory.after_entry_names_hex",
        )?;
        if !after_names.insert(name) {
            return Err(duplicate_entry("disk_directory.after_entry_names_hex"));
        }
    }
    if names != after_names {
        return Err(changed("disk_directory.entries"));
    }
    Ok(())
}

fn decode_entry_name(
    value: &str,
    field: &'static str,
) -> Result<Vec<u8>, ProjectDiskPhysicalReceiptError> {
    let name = decode_hex(value, MAX_ENTRY_NAME_BYTES).ok_or_else(|| invalid_field(field))?;
    if name.is_empty()
        || name == b"."
        || name == b".."
        || name.contains(&b'/')
        || name.contains(&0)
    {
        return Err(invalid_field(field));
    }
    Ok(name)
}

const fn duplicate_entry(field: &'static str) -> ProjectDiskPhysicalReceiptError {
    error(
        ProjectDiskPhysicalReceiptErrorKind::DuplicateEntry,
        field,
        "project_disk_receipt_duplicate_entry",
        "project disk physical receipt contains a duplicate directory entry",
    )
}

fn validate_entry_kind(
    entry: &ReceiptDirectoryEntryEvidence,
) -> Result<(), ProjectDiskPhysicalReceiptError> {
    let mode_type = entry.before.mode & 0o170000;
    match entry.kind {
        ReceiptDirectoryEntryKind::Regular => {
            if !same_entry_binding(&entry.before, &entry.after) {
                return Err(changed("disk_directory.entries"));
            }
            if mode_type != 0o100000 || entry.symlink_target_hex.is_some() {
                return Err(invalid_field("disk_directory.entries.kind"));
            }
            if entry.before.logical_bytes <= MAX_PROJECT_DISK_RECEIPT_SMALL_FILE_BYTES as u64 {
                let bytes = entry
                    .small_regular_file_hex
                    .as_deref()
                    .and_then(|value| decode_hex(value, MAX_PROJECT_DISK_RECEIPT_SMALL_FILE_BYTES))
                    .ok_or_else(|| {
                        invalid_field("disk_directory.entries.small_regular_file_hex")
                    })?;
                if bytes.len() as u64 != entry.before.logical_bytes {
                    return Err(invalid_field(
                        "disk_directory.entries.small_regular_file_hex",
                    ));
                }
            } else if entry.small_regular_file_hex.is_some() {
                return Err(invalid_field(
                    "disk_directory.entries.small_regular_file_hex",
                ));
            }
        }
        ReceiptDirectoryEntryKind::Directory => {
            if entry.before != entry.after {
                return Err(changed("disk_directory.entries"));
            }
            if mode_type != 0o040000
                || entry.symlink_target_hex.is_some()
                || entry.small_regular_file_hex.is_some()
            {
                return Err(invalid_field("disk_directory.entries.kind"));
            }
        }
        ReceiptDirectoryEntryKind::Symlink => {
            if entry.before != entry.after {
                return Err(changed("disk_directory.entries"));
            }
            if mode_type != 0o120000 || entry.small_regular_file_hex.is_some() {
                return Err(invalid_field("disk_directory.entries.kind"));
            }
            let target = entry
                .symlink_target_hex
                .as_deref()
                .and_then(|value| decode_hex(value, MAX_PATH_BYTES))
                .ok_or_else(|| invalid_field("disk_directory.entries.symlink_target_hex"))?;
            if target.is_empty() || target.len() as u64 != entry.before.logical_bytes {
                return Err(invalid_field("disk_directory.entries.symlink_target_hex"));
            }
        }
    }
    Ok(())
}

pub(crate) fn same_entry_binding(
    before: &ReceiptFilesystemSnapshot,
    after: &ReceiptFilesystemSnapshot,
) -> bool {
    before.device == after.device
        && before.inode == after.inode
        && before.uid == after.uid
        && before.gid == after.gid
        && before.mode == after.mode
        && before.links == after.links
        && before.logical_bytes == after.logical_bytes
}

fn validate_snapshot(
    snapshot: &ReceiptFilesystemSnapshot,
    field: &'static str,
) -> Result<(), ProjectDiskPhysicalReceiptError> {
    if snapshot.inode == 0
        || snapshot.links == 0
        || !snapshot
            .allocated_bytes
            .is_multiple_of(BLOCK_ALLOCATION_UNIT_BYTES)
        || !(0..1_000_000_000).contains(&snapshot.mtime_nanoseconds)
        || !(0..1_000_000_000).contains(&snapshot.ctime_nanoseconds)
    {
        return Err(invalid_field(field));
    }
    Ok(())
}

fn validate_command(
    command: &ReceiptCommandEvidence,
    field: &'static str,
) -> Result<(), ProjectDiskPhysicalReceiptError> {
    if command.argv.is_empty()
        || command.argv.iter().any(|value| value.is_empty())
        || command.status != Some(0)
        || !command.success
    {
        return Err(error(
            ProjectDiskPhysicalReceiptErrorKind::CommandFailed,
            field,
            "project_disk_receipt_command_failed",
            "project disk physical receipt contains failed command evidence",
        ));
    }
    let expected_environment = ["HOME", "LANG", "LC_ALL", "LIMA_HOME", "PATH"];
    if !command
        .environment_keys
        .iter()
        .map(String::as_str)
        .eq(expected_environment)
    {
        return Err(invalid_field(field));
    }
    Ok(())
}

fn validate_command_contract(
    document: &ProjectDiskPhysicalReceiptDocument,
) -> Result<(), ProjectDiskPhysicalReceiptError> {
    let program = document
        .lima
        .limactl_version
        .argv
        .first()
        .ok_or_else(|| invalid_field("lima.limactl_version"))?;
    validate_absolute_path(program, "lima.limactl_version")?;
    let instance = document.lima.resident_sandbox_instance.as_str();

    validate_expected_argv(
        &document.lima.limactl_version,
        &[program, "--version"],
        "lima.limactl_version",
    )?;
    validate_expected_argv(
        &document.lima.disk_list_json,
        &[program, "disk", "list", "--json"],
        "lima.disk_list_json",
    )?;
    validate_expected_argv(
        &document.lima.instance_list_json,
        &[
            program,
            "--tty=false",
            "list",
            "--format=json",
            "--all-fields",
            instance,
        ],
        "lima.instance_list_json",
    )?;
    validate_expected_argv(
        &document.guest.mountinfo,
        &[
            program,
            "shell",
            instance,
            "--",
            "/usr/bin/cat",
            "/proc/self/mountinfo",
        ],
        "guest.mountinfo",
    )?;
    validate_expected_argv(
        &document.guest.block_devices_json,
        &[
            program,
            "shell",
            instance,
            "--",
            "/usr/bin/lsblk",
            "--json",
            "--bytes",
            "--output",
            "NAME,KNAME,MAJ:MIN,TYPE,SIZE,MOUNTPOINTS",
        ],
        "guest.block_devices_json",
    )
}

fn validate_expected_argv(
    command: &ReceiptCommandEvidence,
    expected: &[&str],
    field: &'static str,
) -> Result<(), ProjectDiskPhysicalReceiptError> {
    if !command
        .argv
        .iter()
        .map(String::as_str)
        .eq(expected.iter().copied())
    {
        return Err(invalid_field(field));
    }
    Ok(())
}

fn validate_json_stdout(
    command: &ReceiptCommandEvidence,
    field: &'static str,
) -> Result<(), ProjectDiskPhysicalReceiptError> {
    let mut values = serde_json::Deserializer::from_str(&command.stdout).into_iter::<Value>();
    let mut count = 0_usize;
    for value in &mut values {
        value.map_err(|_| invalid_json_evidence(field))?;
        count = count
            .checked_add(1)
            .ok_or_else(|| invalid_json_evidence(field))?;
    }
    if count == 0 {
        return Err(invalid_json_evidence(field));
    }
    Ok(())
}

const fn invalid_json_evidence(field: &'static str) -> ProjectDiskPhysicalReceiptError {
    error(
        ProjectDiskPhysicalReceiptErrorKind::InvalidJsonEvidence,
        field,
        "project_disk_receipt_json_evidence_invalid",
        "project disk physical receipt contains malformed JSON observation evidence",
    )
}

fn parse_guest_mount(
    mountinfo: &str,
    project_mount: &str,
) -> Result<GuestFilesystemDeviceObservation, ProjectDiskPhysicalReceiptError> {
    let expected = project_mount.as_bytes();
    let mut matches = Vec::new();
    for line in mountinfo.as_bytes().split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        let fields = line.split(|byte| *byte == b' ').collect::<Vec<_>>();
        if fields.len() < 10 {
            return Err(mountinfo_invalid());
        }
        if decode_mountinfo_field(fields[4])? != expected {
            continue;
        }
        let separator = fields
            .iter()
            .position(|field| *field == b"-")
            .ok_or_else(mountinfo_invalid)?;
        if separator + 3 >= fields.len() || separator < 6 {
            return Err(mountinfo_invalid());
        }
        let (major, minor) = parse_device_number(fields[2]).ok_or_else(mountinfo_invalid)?;
        let filesystem_type = std::str::from_utf8(fields[separator + 1])
            .map_err(|_| mountinfo_invalid())?
            .to_owned();
        let source = String::from_utf8(decode_mountinfo_field(fields[separator + 2])?)
            .map_err(|_| mountinfo_invalid())?;
        matches.push(GuestFilesystemDeviceObservation {
            major,
            minor,
            filesystem_type,
            source,
        });
    }
    if matches.len() != 1 {
        return Err(error(
            ProjectDiskPhysicalReceiptErrorKind::AmbiguousGuestMount,
            "guest.mountinfo",
            "project_disk_receipt_guest_mount_ambiguous",
            "project disk physical receipt does not identify one exact guest filesystem mount",
        ));
    }
    Ok(matches.pop().expect("guest mount match count checked"))
}

fn parse_device_number(field: &[u8]) -> Option<(u32, u32)> {
    let value = std::str::from_utf8(field).ok()?;
    let (major, minor) = value.split_once(':')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

fn decode_mountinfo_field(field: &[u8]) -> Result<Vec<u8>, ProjectDiskPhysicalReceiptError> {
    let mut result = Vec::with_capacity(field.len());
    let mut index = 0;
    while index < field.len() {
        if field[index] != b'\\' {
            result.push(field[index]);
            index += 1;
            continue;
        }
        let Some(octal) = field.get(index + 1..index + 4) else {
            return Err(mountinfo_invalid());
        };
        if octal.iter().any(|byte| !(b'0'..=b'7').contains(byte)) {
            return Err(mountinfo_invalid());
        }
        let value = u16::from(octal[0] - b'0') * 64
            + u16::from(octal[1] - b'0') * 8
            + u16::from(octal[2] - b'0');
        let decoded = u8::try_from(value).map_err(|_| mountinfo_invalid())?;
        if !matches!(decoded, b'\t' | b'\n' | b' ' | b'\\') {
            return Err(mountinfo_invalid());
        }
        result.push(decoded);
        index += 4;
    }
    Ok(result)
}

const fn mountinfo_invalid() -> ProjectDiskPhysicalReceiptError {
    malformed("guest.mountinfo", "project_disk_receipt_mountinfo_invalid")
}

pub(crate) fn validate_absolute_path(
    value: &str,
    field: &'static str,
) -> Result<(), ProjectDiskPhysicalReceiptError> {
    if value.is_empty()
        || value.len() > MAX_PATH_BYTES
        || value
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_control())
    {
        return Err(invalid_field(field));
    }
    let path = Path::new(value);
    let mut components = path.components();
    if components.next() != Some(Component::RootDir)
        || components.any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(invalid_field(field));
    }
    Ok(())
}

pub(crate) fn validate_locator(
    value: &str,
    field: &'static str,
) -> Result<(), ProjectDiskPhysicalReceiptError> {
    if value.is_empty()
        || value.len() > MAX_ENTRY_NAME_BYTES
        || value == "."
        || value == ".."
        || value.bytes().any(|byte| {
            byte == b'/' || byte == 0 || byte.is_ascii_control() || byte.is_ascii_whitespace()
        })
    {
        return Err(invalid_field(field));
    }
    Ok(())
}

pub(crate) fn valid_git_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn decode_hex(value: &str, max_bytes: usize) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) || value.len() / 2 > max_bytes {
        return None;
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    for pair in value.as_bytes().chunks_exact(2) {
        let high = hex_nibble(pair[0])?;
        let low = hex_nibble(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Some(bytes)
}

const fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
