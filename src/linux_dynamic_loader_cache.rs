//! Pure, bounded parsing of the admitted glibc 2.39 `ld.so.cache` format.
//!
//! This module does not open `/etc/ld.so.cache`, resolve or open a library, inspect a search
//! directory, execute `ldconfig`/`ldd`, or construct runtime evidence. It only validates
//! already-bounded bytes from the current little-endian glibc 1.1 cache format for a later
//! descriptor-bound R01 observer.

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::fmt;

use serde::Serialize;

use crate::personal_worker_runtime_contract::PersonalWorkerRuntimeArchitecture;

pub const LINUX_DYNAMIC_LOADER_CACHE_MAX_BYTES: usize = 16_777_216;

const HEADER_BYTES: usize = 48;
const ENTRY_BYTES: usize = 24;
const MAX_ENTRIES: usize = 65_536;
const MAX_STRING_TABLE_BYTES: usize = 8_388_608;
const MAX_LIBRARY_NAME_BYTES: usize = 255;
const MAX_LIBRARY_PATH_BYTES: usize = 4_096;
const MAX_HWCAP_NAMES: usize = 256;
const MAX_HWCAP_NAME_BYTES: usize = 255;
const MAX_GENERATOR_BYTES: usize = 1_024;
const CACHE_MAGIC: &[u8; 17] = b"glibc-ld.so.cache";
const CACHE_VERSION: &[u8; 3] = b"1.1";
const LITTLE_ENDIAN_FLAG: u8 = 2;
const X86_64_CACHE_ID: i32 = 0x0303;
const AARCH64_CACHE_ID: i32 = 0x0a03;
const HWCAP_EXTENSION_BIT: u64 = 1_u64 << 62;
const HWCAP_ISA_MASK: u64 = 0x03ff;
const EXTENSION_HEADER_BYTES: usize = 8;
const EXTENSION_SECTION_BYTES: usize = 16;
const EXTENSION_MAGIC: u32 = (-358_342_284_i32) as u32;
const EXTENSION_TAG_GENERATOR: u32 = 0;
const EXTENSION_TAG_GLIBC_HWCAPS: u32 = 1;

#[derive(Clone, PartialEq, Eq)]
pub struct LinuxDynamicLoaderCacheEntry {
    library_name: String,
    library_path: String,
    hwcap_name: Option<String>,
    isa_level: u16,
}

impl LinuxDynamicLoaderCacheEntry {
    #[must_use]
    pub fn library_name(&self) -> &str {
        &self.library_name
    }

    #[must_use]
    pub fn library_path(&self) -> &str {
        &self.library_path
    }

    #[must_use]
    pub fn hwcap_name(&self) -> Option<&str> {
        self.hwcap_name.as_deref()
    }

    #[must_use]
    pub const fn isa_level(&self) -> u16 {
        self.isa_level
    }
}

impl fmt::Debug for LinuxDynamicLoaderCacheEntry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxDynamicLoaderCacheEntry")
            .field("has_glibc_hwcap", &self.hwcap_name.is_some())
            .field("isa_level", &self.isa_level)
            .finish_non_exhaustive()
    }
}

/// Canonical semantics parsed from one complete current-format loader cache.
///
/// This is not observation evidence: it carries no cache path identity, owner, mode, content
/// digest, library file identity, search-directory proof, or revalidation receipt.
#[derive(Clone, PartialEq, Eq)]
pub struct LinuxDynamicLoaderCache {
    architecture: PersonalWorkerRuntimeArchitecture,
    entries: Vec<LinuxDynamicLoaderCacheEntry>,
    glibc_hwcap_names: Vec<String>,
}

impl LinuxDynamicLoaderCache {
    #[must_use]
    pub const fn architecture(&self) -> PersonalWorkerRuntimeArchitecture {
        self.architecture
    }

    #[must_use]
    pub fn entries(&self) -> &[LinuxDynamicLoaderCacheEntry] {
        &self.entries
    }

    #[must_use]
    pub fn glibc_hwcap_names(&self) -> &[String] {
        &self.glibc_hwcap_names
    }
}

impl fmt::Debug for LinuxDynamicLoaderCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxDynamicLoaderCache")
            .field("architecture", &self.architecture)
            .field("entry_count", &self.entries.len())
            .field("glibc_hwcap_name_count", &self.glibc_hwcap_names.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LinuxDynamicLoaderCacheErrorKind {
    Size,
    VersionIncompatible,
    Format,
    Architecture,
    UnsafePath,
    UnsupportedCapability,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct LinuxDynamicLoaderCacheError {
    pub kind: LinuxDynamicLoaderCacheErrorKind,
    pub code: &'static str,
    pub message: &'static str,
}

impl fmt::Debug for LinuxDynamicLoaderCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LinuxDynamicLoaderCacheError")
            .field("kind", &self.kind)
            .field("code", &self.code)
            .field("message", &self.message)
            .finish()
    }
}

impl fmt::Display for LinuxDynamicLoaderCacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for LinuxDynamicLoaderCacheError {}

/// Parse one complete, already-bounded glibc 2.39 cache without resolving any library.
///
/// # Errors
///
/// Rejects old, mixed, wrong-endian, malformed, wrong-architecture, unbounded, unsorted,
/// path-unsafe, or unsupported capability/extension input.
pub fn parse_linux_dynamic_loader_cache(
    bytes: &[u8],
    architecture: PersonalWorkerRuntimeArchitecture,
) -> Result<LinuxDynamicLoaderCache, LinuxDynamicLoaderCacheError> {
    if bytes.len() < HEADER_BYTES || bytes.len() > LINUX_DYNAMIC_LOADER_CACHE_MAX_BYTES {
        return Err(size_error());
    }
    if bytes.get(..CACHE_MAGIC.len()) != Some(CACHE_MAGIC) {
        return Err(version_error());
    }
    if bytes.get(CACHE_MAGIC.len()..CACHE_MAGIC.len() + CACHE_VERSION.len()) != Some(CACHE_VERSION)
    {
        return Err(version_error());
    }

    let entry_count = usize::try_from(read_u32(bytes, 20)?).map_err(|_| size_error())?;
    let string_table_bytes = usize::try_from(read_u32(bytes, 24)?).map_err(|_| size_error())?;
    if entry_count > MAX_ENTRIES
        || string_table_bytes == 0
        || string_table_bytes > MAX_STRING_TABLE_BYTES
    {
        return Err(size_error());
    }
    if bytes[28] != LITTLE_ENDIAN_FLAG
        || bytes[29..32].iter().any(|byte| *byte != 0)
        || bytes[36..48].iter().any(|byte| *byte != 0)
    {
        return Err(format_error());
    }

    let entries_bytes = entry_count
        .checked_mul(ENTRY_BYTES)
        .ok_or_else(size_error)?;
    let entries_end = HEADER_BYTES
        .checked_add(entries_bytes)
        .ok_or_else(size_error)?;
    let string_end = entries_end
        .checked_add(string_table_bytes)
        .ok_or_else(size_error)?;
    if string_end > bytes.len() {
        return Err(size_error());
    }
    let expected_extension_offset = align_up(string_end, 4)?;
    let extension_offset = usize::try_from(read_u32(bytes, 32)?).map_err(|_| size_error())?;
    if extension_offset != expected_extension_offset
        || extension_offset > bytes.len()
        || bytes[string_end..extension_offset]
            .iter()
            .any(|byte| *byte != 0)
    {
        return Err(format_error());
    }

    let extensions = parse_extensions(bytes, extension_offset, entries_end, string_end)?;
    let expected_cache_id = match architecture {
        PersonalWorkerRuntimeArchitecture::Aarch64 => AARCH64_CACHE_ID,
        PersonalWorkerRuntimeArchitecture::X86_64 => X86_64_CACHE_ID,
    };
    let mut entries = Vec::with_capacity(entry_count);
    let mut seen_entries = BTreeSet::new();
    let mut referenced_hwcaps = BTreeSet::new();
    for index in 0..entry_count {
        let offset = HEADER_BYTES + index * ENTRY_BYTES;
        let flags = read_i32(bytes, offset)?;
        if flags != expected_cache_id {
            return Err(architecture_error());
        }
        if read_u32(bytes, offset + 12)? != 0 {
            return Err(format_error());
        }
        let library_name = read_cache_string(
            bytes,
            usize::try_from(read_u32(bytes, offset + 4)?).map_err(|_| size_error())?,
            entries_end,
            string_end,
            MAX_LIBRARY_NAME_BYTES,
        )?;
        validate_component(library_name, MAX_LIBRARY_NAME_BYTES)?;
        let library_path = read_cache_string(
            bytes,
            usize::try_from(read_u32(bytes, offset + 8)?).map_err(|_| size_error())?,
            entries_end,
            string_end,
            MAX_LIBRARY_PATH_BYTES,
        )?;
        validate_absolute_path(library_path)?;

        let raw_hwcap = read_u64(bytes, offset + 16)?;
        let (hwcap_name, isa_level) = if raw_hwcap == 0 {
            (None, 0)
        } else {
            let upper_without_isa = (raw_hwcap >> 32) & !HWCAP_ISA_MASK;
            if upper_without_isa != (HWCAP_EXTENSION_BIT >> 32) {
                return Err(unsupported_capability_error());
            }
            let hwcap_index =
                usize::try_from(raw_hwcap & u64::from(u32::MAX)).map_err(|_| size_error())?;
            let name = extensions
                .hwcap_names
                .get(hwcap_index)
                .ok_or_else(format_error)?
                .clone();
            referenced_hwcaps.insert(hwcap_index);
            let isa_level =
                u16::try_from((raw_hwcap >> 32) & HWCAP_ISA_MASK).map_err(|_| format_error())?;
            (Some(name), isa_level)
        };

        let duplicate_key = (
            library_name.to_owned(),
            library_path.to_owned(),
            hwcap_name.clone(),
            isa_level,
        );
        if !seen_entries.insert(duplicate_key.clone()) {
            return Err(format_error());
        }
        entries.push(LinuxDynamicLoaderCacheEntry {
            library_name: duplicate_key.0,
            library_path: duplicate_key.1,
            hwcap_name: duplicate_key.2,
            isa_level,
        });
    }
    if referenced_hwcaps.len() != extensions.hwcap_names.len() {
        return Err(format_error());
    }
    validate_entry_order(&entries)?;

    Ok(LinuxDynamicLoaderCache {
        architecture,
        entries,
        glibc_hwcap_names: extensions.hwcap_names,
    })
}

struct ParsedExtensions {
    hwcap_names: Vec<String>,
}

fn parse_extensions(
    bytes: &[u8],
    extension_offset: usize,
    string_start: usize,
    string_end: usize,
) -> Result<ParsedExtensions, LinuxDynamicLoaderCacheError> {
    if read_u32(bytes, extension_offset)? != EXTENSION_MAGIC {
        return Err(format_error());
    }
    let section_count =
        usize::try_from(read_u32(bytes, extension_offset + 4)?).map_err(|_| size_error())?;
    if !matches!(section_count, 1 | 2) {
        return Err(unsupported_capability_error());
    }
    let directory_bytes = section_count
        .checked_mul(EXTENSION_SECTION_BYTES)
        .and_then(|value| value.checked_add(EXTENSION_HEADER_BYTES))
        .ok_or_else(size_error)?;
    let directory_end = extension_offset
        .checked_add(directory_bytes)
        .ok_or_else(size_error)?;
    if directory_end > bytes.len() {
        return Err(size_error());
    }

    let generator = read_extension_section(bytes, extension_offset, 0)?;
    if generator.tag != EXTENSION_TAG_GENERATOR || generator.flags != 0 {
        return Err(format_error());
    }
    let hwcap = if section_count == 2 {
        let section = read_extension_section(bytes, extension_offset, 1)?;
        if section.tag != EXTENSION_TAG_GLIBC_HWCAPS
            || section.flags != 0
            || section.offset != directory_end
            || section.size == 0
            || section.size % 4 != 0
        {
            return Err(format_error());
        }
        Some(section)
    } else {
        None
    };

    let hwcap_end = match hwcap {
        Some(section) => section
            .offset
            .checked_add(section.size)
            .ok_or_else(size_error)?,
        None => directory_end,
    };
    if generator.offset != hwcap_end
        || generator.size == 0
        || generator.size > MAX_GENERATOR_BYTES
        || generator
            .offset
            .checked_add(generator.size)
            .ok_or_else(size_error)?
            != bytes.len()
        || bytes[generator.offset..]
            .iter()
            .any(|byte| !matches!(byte, b' '..=b'~'))
    {
        return Err(format_error());
    }

    let mut hwcap_names: Vec<String> = Vec::new();
    if let Some(section) = hwcap {
        let count = section.size / 4;
        if count > MAX_HWCAP_NAMES {
            return Err(size_error());
        }
        for index in 0..count {
            let string_offset = usize::try_from(read_u32(bytes, section.offset + index * 4)?)
                .map_err(|_| size_error())?;
            let name = read_cache_string(
                bytes,
                string_offset,
                string_start,
                string_end,
                MAX_HWCAP_NAME_BYTES,
            )?;
            validate_component(name, MAX_HWCAP_NAME_BYTES)?;
            if hwcap_names
                .last()
                .is_some_and(|previous| previous.as_str() >= name)
            {
                return Err(format_error());
            }
            hwcap_names.push(name.to_owned());
        }
    }
    Ok(ParsedExtensions { hwcap_names })
}

#[derive(Clone, Copy)]
struct ExtensionSection {
    tag: u32,
    flags: u32,
    offset: usize,
    size: usize,
}

fn read_extension_section(
    bytes: &[u8],
    extension_offset: usize,
    index: usize,
) -> Result<ExtensionSection, LinuxDynamicLoaderCacheError> {
    let offset = extension_offset + EXTENSION_HEADER_BYTES + index * EXTENSION_SECTION_BYTES;
    Ok(ExtensionSection {
        tag: read_u32(bytes, offset)?,
        flags: read_u32(bytes, offset + 4)?,
        offset: usize::try_from(read_u32(bytes, offset + 8)?).map_err(|_| size_error())?,
        size: usize::try_from(read_u32(bytes, offset + 12)?).map_err(|_| size_error())?,
    })
}

fn validate_entry_order(
    entries: &[LinuxDynamicLoaderCacheEntry],
) -> Result<(), LinuxDynamicLoaderCacheError> {
    for pair in entries.windows(2) {
        let previous = &pair[0];
        let current = &pair[1];
        match cache_library_cmp(&previous.library_name, &current.library_name)? {
            Ordering::Less => return Err(format_error()),
            Ordering::Greater => continue,
            Ordering::Equal => {}
        }
        match (&previous.hwcap_name, &current.hwcap_name) {
            (None, Some(_)) => return Err(format_error()),
            (Some(previous), Some(current)) if previous > current => return Err(format_error()),
            _ => {}
        }
    }
    Ok(())
}

fn cache_library_cmp(left: &str, right: &str) -> Result<Ordering, LinuxDynamicLoaderCacheError> {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let mut left_index = 0;
    let mut right_index = 0;
    while left_index < left.len() {
        let left_byte = left[left_index];
        let right_byte = right.get(right_index).copied().unwrap_or(0);
        if left_byte.is_ascii_digit() {
            if !right_byte.is_ascii_digit() {
                return Ok(Ordering::Greater);
            }
            let (left_value, next_left) = decimal_run(left, left_index)?;
            let (right_value, next_right) = decimal_run(right, right_index)?;
            if left_value != right_value {
                return Ok(left_value.cmp(&right_value));
            }
            left_index = next_left;
            right_index = next_right;
        } else if right_byte.is_ascii_digit() {
            return Ok(Ordering::Less);
        } else if left_byte != right_byte {
            return Ok(left_byte.cmp(&right_byte));
        } else {
            left_index += 1;
            right_index += 1;
        }
    }
    Ok(0_u8.cmp(&right.get(right_index).copied().unwrap_or(0)))
}

fn decimal_run(bytes: &[u8], start: usize) -> Result<(u32, usize), LinuxDynamicLoaderCacheError> {
    let mut value = 0_u32;
    let mut index = start;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        value = value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(bytes[index] - b'0')))
            .filter(|value| *value <= i32::MAX as u32)
            .ok_or_else(format_error)?;
        index += 1;
    }
    Ok((value, index))
}

fn read_cache_string(
    bytes: &[u8],
    offset: usize,
    string_start: usize,
    string_end: usize,
    max_bytes: usize,
) -> Result<&str, LinuxDynamicLoaderCacheError> {
    if offset < string_start || offset >= string_end {
        return Err(format_error());
    }
    let available = bytes.get(offset..string_end).ok_or_else(size_error)?;
    let terminator = available
        .iter()
        .position(|byte| *byte == 0)
        .ok_or_else(format_error)?;
    if terminator == 0 || terminator > max_bytes {
        return Err(format_error());
    }
    std::str::from_utf8(&available[..terminator]).map_err(|_| format_error())
}

fn validate_component(value: &str, max_bytes: usize) -> Result<(), LinuxDynamicLoaderCacheError> {
    let bytes = value.as_bytes();
    if bytes.is_empty()
        || bytes.len() > max_bytes
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
        || bytes.iter().any(|byte| {
            !(byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
        })
        || value == "."
        || value == ".."
    {
        return Err(unsafe_path_error());
    }
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_digit() {
            let (_, next) = decimal_run(bytes, index)?;
            index = next;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn validate_absolute_path(value: &str) -> Result<(), LinuxDynamicLoaderCacheError> {
    if value.len() > MAX_LIBRARY_PATH_BYTES || !value.starts_with('/') || value.ends_with('/') {
        return Err(unsafe_path_error());
    }
    for component in value[1..].split('/') {
        validate_component(component, MAX_LIBRARY_NAME_BYTES)?;
    }
    Ok(())
}

fn align_up(value: usize, alignment: usize) -> Result<usize, LinuxDynamicLoaderCacheError> {
    value
        .checked_add(alignment - 1)
        .map(|value| value & !(alignment - 1))
        .ok_or_else(size_error)
}

fn read_i32(bytes: &[u8], offset: usize) -> Result<i32, LinuxDynamicLoaderCacheError> {
    Ok(i32::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, LinuxDynamicLoaderCacheError> {
    Ok(u32::from_le_bytes(read_array(bytes, offset)?))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, LinuxDynamicLoaderCacheError> {
    Ok(u64::from_le_bytes(read_array(bytes, offset)?))
}

fn read_array<const N: usize>(
    bytes: &[u8],
    offset: usize,
) -> Result<[u8; N], LinuxDynamicLoaderCacheError> {
    bytes
        .get(offset..offset.checked_add(N).ok_or_else(size_error)?)
        .ok_or_else(size_error)?
        .try_into()
        .map_err(|_| size_error())
}

const fn error(
    kind: LinuxDynamicLoaderCacheErrorKind,
    code: &'static str,
    message: &'static str,
) -> LinuxDynamicLoaderCacheError {
    LinuxDynamicLoaderCacheError {
        kind,
        code,
        message,
    }
}

const fn size_error() -> LinuxDynamicLoaderCacheError {
    error(
        LinuxDynamicLoaderCacheErrorKind::Size,
        "dynamic_loader_cache_size",
        "dynamic-loader cache exceeds its canonical bounds",
    )
}

const fn version_error() -> LinuxDynamicLoaderCacheError {
    error(
        LinuxDynamicLoaderCacheErrorKind::VersionIncompatible,
        "dynamic_loader_cache_version_incompatible",
        "dynamic-loader cache format requires explicit migration",
    )
}

const fn format_error() -> LinuxDynamicLoaderCacheError {
    error(
        LinuxDynamicLoaderCacheErrorKind::Format,
        "dynamic_loader_cache_format",
        "dynamic-loader cache is malformed or noncanonical",
    )
}

const fn architecture_error() -> LinuxDynamicLoaderCacheError {
    error(
        LinuxDynamicLoaderCacheErrorKind::Architecture,
        "dynamic_loader_cache_architecture",
        "dynamic-loader cache contains a library for another architecture",
    )
}

const fn unsafe_path_error() -> LinuxDynamicLoaderCacheError {
    error(
        LinuxDynamicLoaderCacheErrorKind::UnsafePath,
        "dynamic_loader_cache_unsafe_path",
        "dynamic-loader cache contains an unsafe library identity or path",
    )
}

const fn unsupported_capability_error() -> LinuxDynamicLoaderCacheError {
    error(
        LinuxDynamicLoaderCacheErrorKind::UnsupportedCapability,
        "dynamic_loader_cache_unsupported_capability",
        "dynamic-loader cache uses unsupported capability or extension semantics",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_current_cache_preserves_entries_and_hwcaps() {
        let bytes = fixture(
            PersonalWorkerRuntimeArchitecture::X86_64,
            &[
                FixtureEntry::plain("libz.so.1", "/lib/x86_64-linux-gnu/libz.so.1"),
                FixtureEntry::hwcap(
                    "libc.so.6",
                    "/lib/x86_64-linux-gnu/glibc-hwcaps/x86-64-v3/libc.so.6",
                    0,
                    3,
                ),
                FixtureEntry::plain("libc.so.6", "/lib/x86_64-linux-gnu/libc.so.6"),
            ],
            &["x86-64-v3"],
        );
        let parsed =
            parse_linux_dynamic_loader_cache(&bytes, PersonalWorkerRuntimeArchitecture::X86_64)
                .expect("current loader cache");
        assert_eq!(parsed.entries().len(), 3);
        assert_eq!(parsed.glibc_hwcap_names(), &["x86-64-v3"]);
        assert_eq!(parsed.entries()[1].hwcap_name(), Some("x86-64-v3"));
        assert_eq!(parsed.entries()[1].isa_level(), 3);
        let debug = format!("{parsed:?} {:?}", parsed.entries()[0]);
        assert!(!debug.contains("/lib/"));
        assert!(!debug.contains("libz"));
    }

    #[test]
    fn version_architecture_and_reserved_drift_fail_closed() {
        let original = fixture(
            PersonalWorkerRuntimeArchitecture::Aarch64,
            &[FixtureEntry::plain(
                "libc.so.6",
                "/lib/aarch64-linux-gnu/libc.so.6",
            )],
            &[],
        );
        for (bytes, expected) in [
            (
                {
                    let mut bytes = original.clone();
                    bytes[17] = b'2';
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::VersionIncompatible,
            ),
            (
                {
                    let mut bytes = original.clone();
                    bytes[28] = 3;
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::Format,
            ),
            (
                {
                    let mut bytes = original.clone();
                    bytes[29] = 1;
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::Format,
            ),
            (
                {
                    let mut bytes = original.clone();
                    bytes[48..52].copy_from_slice(&X86_64_CACHE_ID.to_le_bytes());
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::Architecture,
            ),
        ] {
            assert_eq!(
                parse_linux_dynamic_loader_cache(
                    &bytes,
                    PersonalWorkerRuntimeArchitecture::Aarch64,
                )
                .expect_err("loader cache drift")
                .kind,
                expected
            );
        }
    }

    #[test]
    fn malformed_ranges_paths_order_and_capabilities_fail_closed() {
        let original = fixture(
            PersonalWorkerRuntimeArchitecture::X86_64,
            &[
                FixtureEntry::plain("libz.so.1", "/lib/x86_64-linux-gnu/libz.so.1"),
                FixtureEntry::plain("libc.so.6", "/lib/x86_64-linux-gnu/libc.so.6"),
            ],
            &[],
        );
        let entries_end = HEADER_BYTES + 2 * ENTRY_BYTES;
        for (bytes, expected) in [
            (
                {
                    let mut bytes = original.clone();
                    bytes[52..56].copy_from_slice(&1_u32.to_le_bytes());
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::Format,
            ),
            (
                {
                    let mut bytes = original.clone();
                    let first_key = read_u32(&bytes, 52).expect("first key");
                    let second_key = read_u32(&bytes, 52 + ENTRY_BYTES).expect("second key");
                    bytes[52..56].copy_from_slice(&second_key.to_le_bytes());
                    bytes[52 + ENTRY_BYTES..56 + ENTRY_BYTES]
                        .copy_from_slice(&first_key.to_le_bytes());
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::Format,
            ),
            (
                {
                    let mut bytes = original.clone();
                    let value = usize::try_from(read_u32(&bytes, 56).expect("path offset"))
                        .expect("path usize");
                    bytes[value] = b'.';
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::UnsafePath,
            ),
            (
                {
                    let mut bytes = original.clone();
                    bytes[64..72].copy_from_slice(&1_u64.to_le_bytes());
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::UnsupportedCapability,
            ),
            (
                {
                    let mut bytes = original.clone();
                    bytes[entries_end] = 0xff;
                    bytes
                },
                LinuxDynamicLoaderCacheErrorKind::Format,
            ),
        ] {
            assert_eq!(
                parse_linux_dynamic_loader_cache(
                    &bytes,
                    PersonalWorkerRuntimeArchitecture::X86_64,
                )
                .expect_err("malformed loader cache")
                .kind,
                expected
            );
        }
    }

    #[test]
    fn disposable_noble_loader_cache_matches_the_closed_model() {
        if std::env::var("SMOLRUNNER_ELF_PACKAGE_PROBE").as_deref() != Ok("github-hosted-ubuntu") {
            return;
        }
        let architecture = match std::env::consts::ARCH {
            "aarch64" => PersonalWorkerRuntimeArchitecture::Aarch64,
            "x86_64" => PersonalWorkerRuntimeArchitecture::X86_64,
            other => panic!("unsupported package-probe architecture: {other}"),
        };
        let bytes = std::fs::read("/etc/ld.so.cache").expect("read Noble loader cache");
        let parsed =
            parse_linux_dynamic_loader_cache(&bytes, architecture).unwrap_or_else(|error| {
                if error.kind == LinuxDynamicLoaderCacheErrorKind::Architecture {
                    let entry_count =
                        usize::try_from(read_u32(&bytes, 20).expect("cache entry count"))
                            .expect("cache entry count usize");
                    assert!(entry_count <= MAX_ENTRIES, "bounded cache entry count");
                    let mut cache_id_counts = std::collections::BTreeMap::new();
                    for index in 0..entry_count {
                        let cache_id = read_i32(&bytes, HEADER_BYTES + index * ENTRY_BYTES)
                            .expect("cache entry id");
                        *cache_id_counts.entry(cache_id).or_insert(0_usize) += 1;
                    }
                    panic!("parse Noble loader cache; numeric cache IDs: {cache_id_counts:x?}");
                }
                panic!("parse Noble loader cache: {error:?}");
            });
        assert!(!parsed.entries().is_empty());
    }

    #[derive(Clone, Copy)]
    struct FixtureEntry<'a> {
        name: &'a str,
        path: &'a str,
        hwcap_index: Option<u32>,
        isa_level: u16,
    }

    impl<'a> FixtureEntry<'a> {
        const fn plain(name: &'a str, path: &'a str) -> Self {
            Self {
                name,
                path,
                hwcap_index: None,
                isa_level: 0,
            }
        }

        const fn hwcap(name: &'a str, path: &'a str, index: u32, isa_level: u16) -> Self {
            Self {
                name,
                path,
                hwcap_index: Some(index),
                isa_level,
            }
        }
    }

    fn fixture(
        architecture: PersonalWorkerRuntimeArchitecture,
        entries: &[FixtureEntry<'_>],
        hwcap_names: &[&str],
    ) -> Vec<u8> {
        let entries_end = HEADER_BYTES + entries.len() * ENTRY_BYTES;
        let mut strings = Vec::new();
        let mut entry_offsets = Vec::new();
        for entry in entries {
            let name = entries_end + strings.len();
            strings.extend_from_slice(entry.name.as_bytes());
            strings.push(0);
            let path = entries_end + strings.len();
            strings.extend_from_slice(entry.path.as_bytes());
            strings.push(0);
            entry_offsets.push((name, path));
        }
        let mut hwcap_offsets = Vec::new();
        for name in hwcap_names {
            hwcap_offsets.push(entries_end + strings.len());
            strings.extend_from_slice(name.as_bytes());
            strings.push(0);
        }
        let extension_offset = (entries_end + strings.len() + 3) & !3;
        let section_count = if hwcap_names.is_empty() { 1 } else { 2 };
        let directory_end =
            extension_offset + EXTENSION_HEADER_BYTES + section_count * EXTENSION_SECTION_BYTES;
        let hwcap_bytes = hwcap_names.len() * 4;
        let generator = b"ldconfig fixture release version 2.39";
        let generator_offset = directory_end + hwcap_bytes;
        let mut bytes = vec![0_u8; generator_offset + generator.len()];
        bytes[..17].copy_from_slice(CACHE_MAGIC);
        bytes[17..20].copy_from_slice(CACHE_VERSION);
        write_u32(
            &mut bytes,
            20,
            u32::try_from(entries.len()).expect("entry count"),
        );
        write_u32(
            &mut bytes,
            24,
            u32::try_from(strings.len()).expect("strings"),
        );
        bytes[28] = LITTLE_ENDIAN_FLAG;
        write_u32(
            &mut bytes,
            32,
            u32::try_from(extension_offset).expect("extension offset"),
        );
        let cache_id = match architecture {
            PersonalWorkerRuntimeArchitecture::Aarch64 => AARCH64_CACHE_ID,
            PersonalWorkerRuntimeArchitecture::X86_64 => X86_64_CACHE_ID,
        };
        for (index, entry) in entries.iter().enumerate() {
            let offset = HEADER_BYTES + index * ENTRY_BYTES;
            bytes[offset..offset + 4].copy_from_slice(&cache_id.to_le_bytes());
            write_u32(
                &mut bytes,
                offset + 4,
                u32::try_from(entry_offsets[index].0).expect("name offset"),
            );
            write_u32(
                &mut bytes,
                offset + 8,
                u32::try_from(entry_offsets[index].1).expect("path offset"),
            );
            let hwcap = entry.hwcap_index.map_or(0, |hwcap_index| {
                HWCAP_EXTENSION_BIT | (u64::from(entry.isa_level) << 32) | u64::from(hwcap_index)
            });
            bytes[offset + 16..offset + 24].copy_from_slice(&hwcap.to_le_bytes());
        }
        bytes[entries_end..entries_end + strings.len()].copy_from_slice(&strings);
        write_u32(&mut bytes, extension_offset, EXTENSION_MAGIC);
        write_u32(
            &mut bytes,
            extension_offset + 4,
            u32::try_from(section_count).expect("sections"),
        );
        write_section(
            &mut bytes,
            extension_offset + 8,
            EXTENSION_TAG_GENERATOR,
            generator_offset,
            generator.len(),
        );
        if !hwcap_names.is_empty() {
            write_section(
                &mut bytes,
                extension_offset + 24,
                EXTENSION_TAG_GLIBC_HWCAPS,
                directory_end,
                hwcap_bytes,
            );
            for (index, offset) in hwcap_offsets.into_iter().enumerate() {
                write_u32(
                    &mut bytes,
                    directory_end + index * 4,
                    u32::try_from(offset).expect("hwcap string offset"),
                );
            }
        }
        bytes[generator_offset..].copy_from_slice(generator);
        bytes
    }

    fn write_section(
        bytes: &mut [u8],
        offset: usize,
        tag: u32,
        section_offset: usize,
        size: usize,
    ) {
        write_u32(bytes, offset, tag);
        write_u32(
            bytes,
            offset + 8,
            u32::try_from(section_offset).expect("section offset"),
        );
        write_u32(
            bytes,
            offset + 12,
            u32::try_from(size).expect("section size"),
        );
    }

    fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
}
