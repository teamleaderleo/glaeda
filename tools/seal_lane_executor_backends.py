from pathlib import Path

path = Path("src/lane_executor.rs")
text = path.read_text(encoding="utf-8")
replacements = {
    "pub struct ProcStatusPrivilegeProbe;": "struct ProcStatusPrivilegeProbe;",
    "pub struct SystemExecutableVerifier;": "struct SystemExecutableVerifier;",
}
for old, new in replacements.items():
    if text.count(old) != 1:
        raise SystemExit(f"unexpected helper visibility: {old}")
    text = text.replace(old, new, 1)
if "pub trait PrivilegeProbe" in text or "pub trait ReviewedExecutableVerifier" in text:
    raise SystemExit("injectable evidence traits remain public")
if "pub const fn new(process:" in text:
    raise SystemExit("injectable executor constructor remains public")
path.write_text(text, encoding="utf-8")
