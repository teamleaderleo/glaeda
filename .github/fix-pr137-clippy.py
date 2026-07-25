from pathlib import Path

path = Path("src/durable_lane_execution.rs")
text = path.read_text()
old = '''        let root_command = root_command(&root);
        let extra_command = root_command(&extra);
'''
new = '''        let bound_root_command = root_command(&root);
        let extra_command = root_command(&extra);
'''
if text.count(old) != 1:
    raise SystemExit("root command binding anchor missing or duplicated")
text = text.replace(old, new, 1)
text = text.replace(
    "vec![root_command.clone(), extra_command]",
    "vec![bound_root_command.clone(), extra_command]",
    1,
)
text = text.replace(
    "vec![root_command.clone(), root_command.clone()]",
    "vec![bound_root_command.clone(), bound_root_command.clone()]",
    1,
)
text = text.replace(
    "vec![runner_lane], vec![root_command.clone()]",
    "vec![runner_lane], vec![bound_root_command.clone()]",
    1,
)
path.write_text(text)
