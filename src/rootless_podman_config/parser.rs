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
    Storage,
    StorageOptionsOverlay,
    Engine,
    Network,
    Other,
}

pub(super) fn parse_relevant_fields(
    input: &str,
    kind: RootlessPodmanConfigKind,
) -> Result<BTreeMap<ConfigField, String>, RootlessPodmanConfigError> {
    validate_document(input)?;

    let mut section = Section::Other;
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
            if starts_with_relevant_key(kind, section, line) {
                return Err(malformed_assignment(
                    line_number,
                    "relevant configuration key must use a key = \"value\" assignment",
                ));
            }
            continue;
        };
        let key = raw_key.trim();
        let Some(field) = relevant_field(kind, section, key) else {
            if starts_with_relevant_key(kind, section, key) {
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

fn starts_with_relevant_key(kind: RootlessPodmanConfigKind, section: Section, line: &str) -> bool {
    relevant_field(kind, section, leading_bare_key(line)).is_some()
}

fn leading_bare_key(line: &str) -> &str {
    let end = line
        .char_indices()
        .find(|(_, character)| {
            !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
        })
        .map_or(line.len(), |(index, _)| index);
    &line[..end]
}

fn relevant_field(
    kind: RootlessPodmanConfigKind,
    section: Section,
    key: &str,
) -> Option<ConfigField> {
    match (kind, section, key) {
        (RootlessPodmanConfigKind::Storage, Section::Storage, "driver") => {
            Some(ConfigField::StorageDriver)
        }
        (RootlessPodmanConfigKind::Storage, Section::Storage, "runroot") => {
            Some(ConfigField::StorageRunroot)
        }
        (RootlessPodmanConfigKind::Storage, Section::Storage, "graphroot") => {
            Some(ConfigField::StorageGraphroot)
        }
        (RootlessPodmanConfigKind::Storage, Section::Storage, "rootless_storage_path") => {
            Some(ConfigField::RootlessStoragePath)
        }
        (RootlessPodmanConfigKind::Storage, Section::StorageOptionsOverlay, "mount_program") => {
            Some(ConfigField::OverlayMountProgram)
        }
        (RootlessPodmanConfigKind::Containers, Section::Engine, "cgroup_manager") => {
            Some(ConfigField::CgroupManager)
        }
        (RootlessPodmanConfigKind::Containers, Section::Network, "network_backend") => {
            Some(ConfigField::NetworkBackend)
        }
        _ => None,
    }
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
