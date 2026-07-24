from pathlib import Path

path = Path("src/runner_account_observation.rs")
text = path.read_text()
text = text.replace("use std::collections::VecDeque;\n", "", 1)
text = text.replace("    if gid == 0 || name != *desired || !valid_group_members(fields[3]) {", "    if gid == 0 || &name != desired || !valid_group_members(fields[3]) {")
text = text.replace(
    "    use std::collections::BTreeMap;\n",
    "    use std::collections::{BTreeMap, VecDeque};\n    use std::io;\n    use std::path::{Path, PathBuf};\n",
    1,
)
path.write_text(text)
