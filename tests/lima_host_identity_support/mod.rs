#![allow(dead_code)]

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const LOGICAL_BLOCK_BYTES: u64 = 512;
const DISK_BYTES: u64 = 80 * 1_024 * 1_024 * 1_024;
const GPT_ENTRY_COUNT: u32 = 128;
const GPT_ENTRY_BYTES: u32 = 128;
const GPT_TABLE_BYTES: usize = GPT_ENTRY_COUNT as usize * GPT_ENTRY_BYTES as usize;
const GPT_TABLE_BLOCKS: u64 = GPT_TABLE_BYTES as u64 / LOGICAL_BLOCK_BYTES;
const GPT_HEADER_BYTES: u32 = 92;

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(1);

pub struct LimaHostIdentityFixture {
    root: PathBuf,
    lima_home: PathBuf,
}

impl LimaHostIdentityFixture {
    pub fn new(label: &str, instance: &str) -> Self {
        let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let root = std::env::current_dir()
            .expect("current directory")
            .join("target/b02-host-identity-fixtures")
            .join(format!("{label}-{}-{}", std::process::id(), sequence));
        let lima_home = root.join("lima-home");
        let instance_directory = lima_home.join(instance);
        fs::create_dir_all(&instance_directory).expect("create Lima identity fixture");
        fs::set_permissions(&lima_home, Permissions::from_mode(0o700)).expect("private Lima home");
        fs::set_permissions(&instance_directory, Permissions::from_mode(0o700))
            .expect("private instance directory");
        let identity_byte = u8::try_from(sequence % 254 + 1).expect("bounded identity byte");
        write_identifier(&instance_directory.join("vz-identifier"), identity_byte);
        write_disk(&instance_directory.join("disk"), identity_byte, true);
        Self { root, lima_home }
    }

    pub fn lima_home(&self) -> &Path {
        &self.lima_home
    }

    pub fn lima_home_string(&self) -> String {
        self.lima_home
            .to_str()
            .expect("UTF-8 test Lima home")
            .to_owned()
    }

    pub fn rewrite_disk_identity(&self, instance: &str, identity_byte: u8) {
        rewrite_disk_identity(&self.lima_home.join(instance).join("disk"), identity_byte);
    }
}

pub fn rewrite_disk_identity(path: &Path, identity_byte: u8) {
    write_disk(path, identity_byte, false);
}

impl Drop for LimaHostIdentityFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
        if let Some(parent) = self.root.parent() {
            let _ = fs::remove_dir(parent);
        }
    }
}

fn write_identifier(path: &Path, identity_byte: u8) {
    let mut bytes = [0_u8; 70];
    bytes[..8].copy_from_slice(b"bplist00");
    bytes[8..19].copy_from_slice(b"\xd1\x01\x02\x54UUIDO\x10\x10");
    bytes[19..35].fill(identity_byte);
    bytes[35..38].copy_from_slice(b"\x08\x0b\x10");
    bytes[44..46].copy_from_slice(&[1, 1]);
    bytes[46..54].copy_from_slice(&3_u64.to_be_bytes());
    bytes[62..70].copy_from_slice(&35_u64.to_be_bytes());
    let mut options = OpenOptions::new();
    options.create_new(true).write(true).mode(0o600);
    options
        .open(path)
        .expect("create VZ identifier")
        .write_all(&bytes)
        .expect("write VZ identifier");
}

fn write_disk(path: &Path, identity_byte: u8, create_new: bool) {
    let mut options = OpenOptions::new();
    options
        .create(create_new)
        .create_new(create_new)
        .truncate(!create_new)
        .read(true)
        .write(true)
        .mode(0o600);
    let mut disk = options.open(path).expect("create sparse root disk");
    disk.set_len(DISK_BYTES).expect("size sparse root disk");

    let mut table = vec![0_u8; GPT_TABLE_BYTES];
    table[..16].copy_from_slice(&[
        0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47, 0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d,
        0xe4,
    ]);
    table[16..32].copy_from_slice(&[0x33; 16]);
    let last_lba = DISK_BYTES / LOGICAL_BLOCK_BYTES - 1;
    table[32..40].copy_from_slice(&34_u64.to_le_bytes());
    table[40..48].copy_from_slice(&(last_lba - GPT_TABLE_BLOCKS - 1).to_le_bytes());

    let table_crc = crc32(&table);
    let primary = gpt_header(1, last_lba, 2, last_lba, table_crc, identity_byte);
    let backup = gpt_header(
        last_lba,
        1,
        last_lba - GPT_TABLE_BLOCKS,
        last_lba,
        table_crc,
        identity_byte,
    );
    write_at(&mut disk, LOGICAL_BLOCK_BYTES, &primary);
    write_at(&mut disk, 2 * LOGICAL_BLOCK_BYTES, &table);
    write_at(
        &mut disk,
        (last_lba - GPT_TABLE_BLOCKS) * LOGICAL_BLOCK_BYTES,
        &table,
    );
    write_at(&mut disk, last_lba * LOGICAL_BLOCK_BYTES, &backup);
}

fn gpt_header(
    current_lba: u64,
    backup_lba: u64,
    entries_lba: u64,
    last_lba: u64,
    table_crc: u32,
    identity_byte: u8,
) -> [u8; 512] {
    let mut header = [0_u8; 512];
    header[..8].copy_from_slice(b"EFI PART");
    header[8..12].copy_from_slice(&0x0001_0000_u32.to_le_bytes());
    header[12..16].copy_from_slice(&GPT_HEADER_BYTES.to_le_bytes());
    header[24..32].copy_from_slice(&current_lba.to_le_bytes());
    header[32..40].copy_from_slice(&backup_lba.to_le_bytes());
    header[40..48].copy_from_slice(&34_u64.to_le_bytes());
    header[48..56].copy_from_slice(&(last_lba - GPT_TABLE_BLOCKS - 1).to_le_bytes());
    header[56..72].copy_from_slice(&[identity_byte; 16]);
    header[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    header[80..84].copy_from_slice(&GPT_ENTRY_COUNT.to_le_bytes());
    header[84..88].copy_from_slice(&GPT_ENTRY_BYTES.to_le_bytes());
    header[88..92].copy_from_slice(&table_crc.to_le_bytes());
    let header_crc = crc32(&header[..GPT_HEADER_BYTES as usize]);
    header[16..20].copy_from_slice(&header_crc.to_le_bytes());
    header
}

fn write_at(file: &mut File, offset: u64, bytes: &[u8]) {
    file.seek(SeekFrom::Start(offset))
        .expect("seek sparse disk");
    file.write_all(bytes).expect("write GPT evidence");
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}
