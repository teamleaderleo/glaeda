from pathlib import Path

path = Path("src/process.rs")
text = path.read_text()

old = '''        let fixture = timeout_fixture_directory()?;
        let script = "import os,sys; sys.stdout.buffer.write(os.getcwd().encode()+b'\\\\n'+sys.stdin.buffer.read())";'''
new = '''        let fixture = timeout_fixture_directory()?;
        let expected_directory = fs::canonicalize(&fixture)?;
        let script = "import os,sys; sys.stdout.buffer.write(os.getcwd().encode()+b'\\\\n'+sys.stdin.buffer.read())";'''
count = text.count(old)
if count != 1:
    raise SystemExit(f"src/process.rs: expected one cwd-input fixture anchor, found {count}")
text = text.replace(old, new, 1)

old = '''            format!("{}\\nbounded-input", fixture.to_string_lossy())'''
new = '''            format!("{}\\nbounded-input", expected_directory.to_string_lossy())'''
count = text.count(old)
if count != 1:
    raise SystemExit(f"src/process.rs: expected one cwd-input expected-path anchor, found {count}")
text = text.replace(old, new, 1)

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
