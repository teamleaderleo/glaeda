use std::collections::BTreeMap;

use super::{
    MAX_ROOTLESS_PODMAN_CONFIG_BYTES, MAX_ROOTLESS_PODMAN_CONFIG_LINE_BYTES,
    MAX_ROOTLESS_PODMAN_CONFIG_LINES, MAX_ROOTLESS_PODMAN_CONFIG_VALUE_BYTES,
    RootlessPodmanConfigError, RootlessPodmanConfigErrorKind, RootlessPodmanConfigKind,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ConfigField {
    StorageDriver,
    StorageRunroot,
    StorageGraphroot,
    RootlessStoragePath,
    OverlayMountProgram,
    CgroupManager,
    NetworkBackend,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Root,
    Storage,
    StorageOptions,
    StorageOptionsOverlay,
    Engine,
    Network,
    Other,
}

impl Section {
    const fn prefix(self) -> Option<&'static str> {
        match self {
            Self::Root => Some(""),
            Self::Storage => Some("storage"),
            Self::StorageOptions => Some("storage.options"),
            Self::StorageOptionsOverlay => Some("storage.options.overlay"),
            Self::Engine => Some("engine"),
            Self::Network => Some("network"),
            Self::Other => None,
        }
    }
}

pub(super) fn parse_relevant_fields(
    input: &str,
    kind: RootlessPodmanConfigKind,
) -> Result<BTreeMap<ConfigField, String>, RootlessPodmanConfigError> {
    validate_document(input)?;

    let mut section = Section::Root;
    let mut fields = BTreeMap::new();
    for (index, raw_line) in input.lines().enumerate() {
        let line_number = index + 1;
        if raw_line.len() > MAX_ROOTLESS_PODMAN_CONFIG_LINE_BYTES {
            return Err(RootlessPodmanConfigError::new(
                RootlessPodmanConfigErrorKind::LineTooLong,
                Some(line_number),
                format!("configuration line exceeds {MAX_ROOTLESS_PODMAN_CONFIG_LINE_BYTES} bytes"),
            ));
        }
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            section = parse_section(line, line_number)?;
            continue;
        }

        let Some((raw_key, raw_value)) = split_assignment(line) else {
            if looks_like_relevant_key(kind, section, line) {
                return Err(malformed_assignment(
                    line_number,
                    "relevant configuration key must use a key = \"value\" assignment",
                ));
            }
            continue;
        };
        let key = raw_key.trim();
        let Some(field) = relevant_field(kind, section, key) else {
            if looks_like_relevant_key(kind, section, key) {
                return Err(malformed_assignment(
                    line_number,
                    "relevant configuration key contains unsupported assignment syntax",
                ));
            }
            continue;
        };
        let value = parse_quoted_value(raw_value, line_number)?;
        if fields.insert(field, value).is_some() {
            return Err(RootlessPodmanConfigError::new(
                RootlessPodmanConfigErrorKind::DuplicateRelevantKey,
                Some(line_number),
                format!("relevant configuration key {key:?} is duplicated"),
            ));
        }
    }
    Ok(fields)
}

fn validate_document(input: &str) -> Result<(), RootlessPodmanConfigError> {
    if input.len() > MAX_ROOTLESS_PODMAN_CONFIG_BYTES {
        return Err(RootlessPodmanConfigError::new(
            RootlessPodmanConfigErrorKind::Oversized,
            None,
            format!("configuration input exceeds {MAX_ROOTLESS_PODMAN_CONFIG_BYTES} bytes"),
        ));
    }
    if input.lines().count() > MAX_ROOTLESS_PODMAN_CONFIG_LINES {
        return Err(RootlessPodmanConfigError::new(
            RootlessPodmanConfigErrorKind::TooManyLines,
            None,
            format!("configuration input exceeds {MAX_ROOTLESS_PODMAN_CONFIG_LINES} lines"),
        ));
    }
    if input
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
    {
        return Err(RootlessPodmanConfigError::new(
            RootlessPodmanConfigErrorKind::InvalidControlCharacter,
            None,
            "configuration input contains a disallowed control character",
        ));
    }
    Ok(())
}

fn parse_section(line: &str, line_number: usize) -> Result<Section, RootlessPodmanConfigError> {
    let (name, array_table) = if let Some(name) = line
        .strip_prefix("[[")
        .and_then(|value| value.strip_suffix("]]"))
    {
        (name, true)
    } else if let Some(name) = line
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        (name, false)
    } else {
        return Err(malformed_table(line_number));
    };

    let name = name.trim();
    if name.is_empty()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return Err(malformed_table(line_number));
    }
    if array_table {
        return Ok(Section::Other);
    }

    Ok(match name {
        "storage" => Section::Storage,
        "storage.options" => Section::StorageOptions,
        "storage.options.overlay" => Section::StorageOptionsOverlay,
        "engine" => Section::Engine,
        "network" => Section::Network,
        _ => Section::Other,
    })
}

fn malformed_table(line_number: usize) -> RootlessPodmanConfigError {
    RootlessPodmanConfigError::new(
        RootlessPodmanConfigErrorKind::MalformedTable,
        Some(line_number),
        "configuration table header is not a supported bare TOML path",
    )
}

fn split_assignment(line: &str) -> Option<(&str, &str)> {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if let Some(active_quote) = quote {
            if active_quote == '"' && escaped {
                escaped = false;
                continue;
            }
            if active_quote == '"' && character == '\\' {
                escaped = true;
            } else if character == active_quote {
                quote = None;
            }
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character == '=' {
            return Some((&line[..index], &line[index + 1..]));
        }
    }
    None
}

fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (index, character) in line.char_indices() {
        if let Some(active_quote) = quote {
            if active_quote == '"' && escaped {
                escaped = false;
                continue;
            }
            if active_quote == '"' && character == '\\' {
                escaped = true;
            } else if character == active_quote {
                quote = None;
            }
        } else if matches!(character, '\'' | '"') {
            quote = Some(character);
        } else if character == '#' {
            return &line[..index];
        }
    }
    line
}

fn relevant_field(
    kind: RootlessPodmanConfigKind,
    section: Section,
    key: &str,
) -> Option<ConfigField> {
    let relative = normalize_complete_key_path(key, false)?;
    let full = full_key_path(section, &relative)?;
    relevant_field_for_full_path(kind, &full)
}

fn looks_like_relevant_key(kind: RootlessPodmanConfigKind, section: Section, source: &str) -> bool {
    let Some(relative) = normalize_leading_key_path(source) else {
        return false;
    };
    let Some(full) = full_key_path(section, &relative) else {
        return false;
    };
    relevant_field_for_full_path(kind, &full).is_some()
}

fn full_key_path(section: Section, relative: &str) -> Option<String> {
    let prefix = section.prefix()?;
    if prefix.is_empty() {
        Some(relative.to_owned())
    } else {
        Some(format!("{prefix}.{relative}"))
    }
}

fn relevant_field_for_full_path(kind: RootlessPodmanConfigKind, full: &str) -> Option<ConfigField> {
    match (kind, full) {
        (RootlessPodmanConfigKind::Storage, "storage.driver") => Some(ConfigField::StorageDriver),
        (RootlessPodmanConfigKind::Storage, "storage.runroot") => Some(ConfigField::StorageRunroot),
        (RootlessPodmanConfigKind::Storage, "storage.graphroot") => {
            Some(ConfigField::StorageGraphroot)
        }
        (RootlessPodmanConfigKind::Storage, "storage.rootless_storage_path") => {
            Some(ConfigField::RootlessStoragePath)
        }
        (RootlessPodmanConfigKind::Storage, "storage.options.overlay.mount_program") => {
            Some(ConfigField::OverlayMountProgram)
        }
        (RootlessPodmanConfigKind::Containers, "engine.cgroup_manager") => {
            Some(ConfigField::CgroupManager)
        }
        (RootlessPodmanConfigKind::Containers, "network.network_backend") => {
            Some(ConfigField::NetworkBackend)
        }
        _ => None,
    }
}

fn normalize_complete_key_path(input: &str, allow_quoted: bool) -> Option<String> {
    let (path, consumed) = parse_key_path_prefix(input, allow_quoted)?;
    input[consumed..].trim().is_empty().then_some(path)
}

fn normalize_leading_key_path(input: &str) -> Option<String> {
    parse_key_path_prefix(input, true).map(|(path, _)| path)
}

fn parse_key_path_prefix(input: &str, allow_quoted: bool) -> Option<(String, usize)> {
    let mut offset = 0;
    let mut segments = Vec::new();

    loop {
        offset += input[offset..]
            .char_indices()
            .take_while(|(_, character)| matches!(character, ' ' | '\t'))
            .map(|(_, character)| character.len_utf8())
            .sum::<usize>();
        if offset >= input.len() {
            return None;
        }

        let remaining = &input[offset..];
        let first = remaining.chars().next()?;
        let (segment, consumed) = if matches!(first, '\'' | '"') {
            if !allow_quoted {
                return None;
            }
            parse_simple_quoted_key_segment(remaining, first)?
        } else {
            let consumed = remaining
                .char_indices()
                .take_while(|(_, character)| is_bare_key_character(*character))
                .map(|(_, character)| character.len_utf8())
                .sum::<usize>();
            if consumed == 0 {
                return None;
            }
            (&remaining[..consumed], consumed)
        };
        if segment.is_empty() || !segment.chars().all(is_bare_key_character) {
            return None;
        }
        segments.push(segment.to_owned());
        offset += consumed;

        let whitespace = input[offset..]
            .char_indices()
            .take_while(|(_, character)| matches!(character, ' ' | '\t'))
            .map(|(_, character)| character.len_utf8())
            .sum::<usize>();
        offset += whitespace;
        if input[offset..].starts_with('.') {
            offset += 1;
            continue;
        }
        break;
    }

    Some((segments.join("."), offset))
}

fn parse_simple_quoted_key_segment(input: &str, quote: char) -> Option<(&str, usize)> {
    let start = quote.len_utf8();
    for (offset, character) in input[start..].char_indices() {
        if character == quote {
            let end = start + offset;
            return Some((&input[start..end], end + character.len_utf8()));
        }
        if character.is_control() || (quote == '"' && character == '\\') {
            return None;
        }
    }
    None
}

fn is_bare_key_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
}

fn parse_quoted_value(
    raw_value: &str,
    line_number: usize,
) -> Result<String, RootlessPodmanConfigError> {
    let value = raw_value.trim();
    if value.len() > MAX_ROOTLESS_PODMAN_CONFIG_VALUE_BYTES {
        return Err(malformed_assignment(
            line_number,
            format!(
                "relevant configuration value exceeds {MAX_ROOTLESS_PODMAN_CONFIG_VALUE_BYTES} bytes"
            ),
        ));
    }
    let Some(quote) = value
        .chars()
        .next()
        .filter(|value| matches!(value, '\'' | '"'))
    else {
        return Err(malformed_assignment(
            line_number,
            "relevant configuration value must be a one-line quoted TOML string",
        ));
    };
    if value.starts_with("\"\"\"") || value.starts_with("'''") {
        return Err(malformed_assignment(
            line_number,
            "multiline strings are not supported for relevant configuration values",
        ));
    }

    let mut output = String::new();
    let mut escaped = false;
    let mut closing = None;
    for (offset, character) in value[quote.len_utf8()..].char_indices() {
        if quote == '"' && escaped {
            match character {
                '"' | '\\' => output.push(character),
                _ => {
                    return Err(malformed_assignment(
                        line_number,
                        "relevant configuration value uses an unsupported escape",
                    ));
                }
            }
            escaped = false;
            continue;
        }
        if quote == '"' && character == '\\' {
            escaped = true;
            continue;
        }
        if character == quote {
            closing = Some(quote.len_utf8() + offset + character.len_utf8());
            break;
        }
        if character.is_control() {
            return Err(malformed_assignment(
                line_number,
                "relevant configuration value contains a control character",
            ));
        }
        output.push(character);
    }
    if escaped {
        return Err(malformed_assignment(
            line_number,
            "relevant configuration value ends with an incomplete escape",
        ));
    }
    let Some(closing) = closing else {
        return Err(malformed_assignment(
            line_number,
            "relevant configuration value is missing its closing quote",
        ));
    };
    if !value[closing..].trim().is_empty() {
        return Err(malformed_assignment(
            line_number,
            "relevant configuration value has trailing non-comment content",
        ));
    }
    Ok(output)
}

fn malformed_assignment(
    line_number: usize,
    message: impl Into<String>,
) -> RootlessPodmanConfigError {
    RootlessPodmanConfigError::new(
        RootlessPodmanConfigErrorKind::MalformedRelevantAssignment,
        Some(line_number),
        message,
    )
}
