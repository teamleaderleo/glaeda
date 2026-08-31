from pathlib import Path

path = Path("src/process.rs")
text = path.read_text()
old = '''            b"bounded-input",
            Duration::from_secs(1),
        )?;'''
new = '''            b"bounded-input",
            Duration::from_secs(5),
        )?;'''
count = text.count(old)
if count != 1:
    raise SystemExit(f"src/process.rs: expected one combined-cwd-input timeout anchor, found {count}")
path.write_text(text.replace(old, new, 1))
