from pathlib import Path

path = Path("src/bin/glaeda-local-patch-check.rs")
text = path.read_text()
text = text.replace(
    "    CommandSpec, MAX_CAPTURED_STDIN_BYTES, ProcessExecutor, TimedCommandExecutor,\n    TimedInputCommandExecutor,",
    "    CommandSpec, MAX_CAPTURED_STDIN_BYTES, ProcessExecutor, TimedInputCommandExecutor,",
    1,
)
old = "        let fixture = fixture();\n        let replacement = fixture();"
new = "        let replacement = fixture();\n        let fixture = fixture();"
if text.count(old) != 1:
    raise SystemExit(f"expected one race fixture pair, found {text.count(old)}")
path.write_text(text.replace(old, new, 1))
