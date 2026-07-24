from pathlib import Path

path = Path("src/runner_account_observation.rs")
text = path.read_text()
text = text.replace(
    "    let home = classify_home(filesystem.inspect(desired.home()), identity, desired.home())?;",
    "    let home = classify_home(\n        filesystem.inspect(Path::new(desired.home())),\n        identity,\n        desired.home(),\n    )?;",
    1,
)
text = text.replace(
    "        RunnerAccountObservationPaths, TrustedFile, getent_command, observe_with,\n",
    "        GETENT, RunnerAccountObservationPaths, TrustedFile, getent_command, observe_with,\n",
    1,
)
path.write_text(text)
