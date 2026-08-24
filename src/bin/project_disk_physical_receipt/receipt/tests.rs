use serde_json::{Value, json};

use super::*;

fn snapshot(mode: u32, inode: u64, logical: u64, allocated: u64) -> Value {
    json!({
        "device": 7,
        "inode": inode,
        "uid": 501,
        "gid": 20,
        "mode": mode,
        "links": 1,
        "logical_bytes": logical,
        "allocated_bytes": allocated,
        "mtime_seconds": 1_700_000_000,
        "mtime_nanoseconds": 1,
        "ctime_seconds": 1_700_000_000,
        "ctime_nanoseconds": 2
    })
}

fn command(argv: &[&str], stdout: &str) -> Value {
    json!({
        "argv": argv,
        "environment_keys": ["HOME", "LANG", "LC_ALL", "LIMA_HOME", "PATH"],
        "status": 0,
        "success": true,
        "stdout": stdout,
        "stderr": ""
    })
}

fn fixture() -> Value {
    let directory = snapshot(0o040700, 10, 128, 512);
    json!({
        "schema_version": 1,
        "repo_commit": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "captured_at_unix_millis": 1_700_000_000_000_u64,
        "declared_binding": {
            "project_identity": "github.com/example/project",
            "project_disk_id": "disk-a",
            "project_disk_generation": 3,
            "project_disk_revision": 5,
            "attachment_generation": 7,
            "resident_sandbox_id": "sandbox-a",
            "resident_sandbox_generation": 11
        },
        "lima": {
            "lima_home": "/Users/operator/.lima",
            "disk_name": "candidate",
            "resident_sandbox_instance": "smolrunner",
            "host_identity_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "limactl_version": command(&["/opt/homebrew/bin/limactl", "--version"], "limactl version 1\n"),
            "disk_list_json": command(&["/opt/homebrew/bin/limactl", "disk", "list", "--json"], "[{\"name\":\"candidate\"}]\n"),
            "instance_list_json": command(&["/opt/homebrew/bin/limactl", "--tty=false", "list", "--format=json", "--all-fields", "smolrunner"], "[{\"name\":\"smolrunner\"}]\n")
        },
        "disk_directory": {
            "path": "/Users/operator/.lima/private-observed-disk-dir",
            "before": directory.clone(),
            "entries": [
                {
                    "name_hex": "6f70617175652d61",
                    "name_utf8": "opaque-a",
                    "kind": "regular",
                    "before": snapshot(0o100600, 20, 1_073_741_824, 4_194_304),
                    "after": snapshot(0o100600, 20, 1_073_741_824, 4_194_304)
                },
                {
                    "name_hex": "6f70617175652d62",
                    "name_utf8": "opaque-b",
                    "kind": "symlink",
                    "before": snapshot(0o120777, 21, 9, 0),
                    "symlink_target_hex": "73616e64626f782d61",
                    "after": snapshot(0o120777, 21, 9, 0)
                }
            ],
            "after_entry_names_hex": ["6f70617175652d61", "6f70617175652d62"],
            "after": directory
        },
        "guest": {
            "project_mount": "/srv/project",
            "mountinfo": command(&["/opt/homebrew/bin/limactl", "shell", "smolrunner", "--", "/usr/bin/cat", "/proc/self/mountinfo"], "36 25 8:1 / /srv/project rw,relatime - xfs /dev/vdb1 rw\n"),
            "block_devices_json": command(&["/opt/homebrew/bin/limactl", "shell", "smolrunner", "--", "/usr/bin/lsblk", "--json", "--bytes", "--output", "NAME,KNAME,MAJ:MIN,TYPE,SIZE,MOUNTPOINTS"], "{\"blockdevices\":[{\"maj:min\":\"8:1\"}]}\n")
        }
    })
}

fn decode(value: &Value) -> Result<ProjectDiskPhysicalReceipt, ProjectDiskPhysicalReceiptError> {
    ProjectDiskPhysicalReceipt::decode_private_json(
        &serde_json::to_vec(value).expect("encode fixture"),
    )
}

#[test]
fn parses_opaque_entries_and_exact_guest_device_without_assigning_roles() {
    let receipt = decode(&fixture()).expect("valid physical receipt");
    assert_eq!(
        receipt.document.declared_binding.project_identity,
        "github.com/example/project"
    );
    assert_eq!(receipt.document.declared_binding.project_disk_revision, 5);
    assert_eq!(receipt.document.disk_directory.entries.len(), 2);
    assert_eq!(receipt.guest_filesystem_device.major, 8);
    assert_eq!(receipt.guest_filesystem_device.minor, 1);
    assert_eq!(receipt.guest_filesystem_device.filesystem_type, "xfs");
    assert_eq!(receipt.guest_filesystem_device.source, "/dev/vdb1");
}

#[test]
fn refuses_same_name_replacement_visible_in_descriptor_snapshots() {
    let mut value = fixture();
    value["disk_directory"]["entries"][0]["after"]["inode"] = json!(999);
    let failure = decode(&value).expect_err("entry drift must fail");
    assert_eq!(
        failure.kind(),
        ProjectDiskPhysicalReceiptErrorKind::ChangedDuringObservation
    );
}

#[test]
fn refuses_symlink_replacement_even_when_device_inode_and_target_match() {
    let mut value = fixture();
    value["disk_directory"]["entries"][1]["after"]["ctime_nanoseconds"] = json!(3);
    let failure = decode(&value).expect_err("symlink metadata drift must fail");
    assert_eq!(
        failure.kind(),
        ProjectDiskPhysicalReceiptErrorKind::ChangedDuringObservation
    );
}

#[test]
fn refuses_changed_second_directory_entry_set() {
    let mut value = fixture();
    value["disk_directory"]["after_entry_names_hex"][1] = json!("6f70617175652d63");
    let failure = decode(&value).expect_err("entry set drift must fail");
    assert_eq!(
        failure.kind(),
        ProjectDiskPhysicalReceiptErrorKind::ChangedDuringObservation
    );
}

#[test]
fn refuses_duplicate_opaque_entry_names() {
    let mut value = fixture();
    value["disk_directory"]["entries"][1]["name_hex"] = json!("6f70617175652d61");
    value["disk_directory"]["entries"][1]["name_utf8"] = json!("opaque-a");
    let failure = decode(&value).expect_err("duplicate names must fail");
    assert_eq!(
        failure.kind(),
        ProjectDiskPhysicalReceiptErrorKind::DuplicateEntry
    );
}

#[test]
fn refuses_malformed_lima_json_without_interpreting_its_schema() {
    let mut value = fixture();
    value["lima"]["disk_list_json"]["stdout"] = json!("not-json");
    let failure = decode(&value).expect_err("malformed Lima JSON must fail");
    assert_eq!(
        failure.kind(),
        ProjectDiskPhysicalReceiptErrorKind::InvalidJsonEvidence
    );
}

#[test]
fn refuses_mutation_command_substitution() {
    let mut value = fixture();
    value["lima"]["disk_list_json"]["argv"] =
        json!(["/opt/homebrew/bin/limactl", "disk", "unlock", "candidate"]);
    let failure = decode(&value).expect_err("mutation argv must fail");
    assert_eq!(
        failure.kind(),
        ProjectDiskPhysicalReceiptErrorKind::InvalidField
    );
}

#[test]
fn refuses_ambiguous_guest_mount_device() {
    let mut value = fixture();
    value["guest"]["mountinfo"]["stdout"] = json!(concat!(
        "36 25 8:1 / /srv/project rw - xfs /dev/vdb1 rw\n",
        "37 25 8:2 / /srv/project rw - xfs /dev/vdc1 rw\n"
    ));
    let failure = decode(&value).expect_err("ambiguous device must fail");
    assert_eq!(
        failure.kind(),
        ProjectDiskPhysicalReceiptErrorKind::AmbiguousGuestMount
    );
}

#[test]
fn accepts_allocated_byte_growth_for_the_same_held_large_file_identity() {
    let mut value = fixture();
    value["disk_directory"]["entries"][0]["after"]["allocated_bytes"] = json!(8_388_608);
    decode(&value).expect("allocated growth alone does not replace the held file");
}

#[test]
fn refuses_unknown_fields_and_small_file_length_mismatch() {
    let mut unknown = fixture();
    unknown["unexpected"] = json!(true);
    assert_eq!(
        decode(&unknown)
            .expect_err("unknown field must fail")
            .kind(),
        ProjectDiskPhysicalReceiptErrorKind::Malformed
    );

    let mut small = fixture();
    let entry = &mut small["disk_directory"]["entries"][0];
    entry["before"]["logical_bytes"] = json!(2);
    entry["before"]["allocated_bytes"] = json!(512);
    entry["after"] = entry["before"].clone();
    entry["small_regular_file_hex"] = json!("00");
    assert_eq!(
        decode(&small)
            .expect_err("short small-file evidence must fail")
            .kind(),
        ProjectDiskPhysicalReceiptErrorKind::InvalidField
    );
}
