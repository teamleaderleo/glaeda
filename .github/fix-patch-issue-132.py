from pathlib import Path

path = Path('.github/patch-issue-132.py')
text = path.read_text()
for old, new in [
    ("render_anchor = '''", "render_anchor = r'''") ,
    ("render_block = '''", "render_block = r'''") ,
]:
    if text.count(old) != 1:
        raise SystemExit(f'patch literal anchor missing or duplicated: {old}')
    text = text.replace(old, new, 1)
path.write_text(text)
