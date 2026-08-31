from pathlib import Path

path = Path("src/bin/glaeda-local-patch-check.rs")
text = path.read_text()
old = "        let fixture = fixture();\n        let replacement = fixture();"
new = "        let replacement = fixture();\n        let fixture = fixture();"
count = text.count(old)
if count != 2:
    raise SystemExit(f"expected two generated fixture pairs, found {count}")
path.write_text(text.replace(old, new))
