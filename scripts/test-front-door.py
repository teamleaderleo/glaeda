#!/usr/bin/env python3
"""Focused fixtures for the one-command Glaeda readiness journey."""

from __future__ import annotations

import sys
from pathlib import Path

sys.dont_write_bytecode = True
SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))

from workspace_bootstrap.journey import (
    _valid_bootstrap_receipt,
    _valid_doctor_receipt,
    build_journey_report,
    render_human,
)


def bootstrap(state: str = "ready", *, commit: str = "1" * 40) -> dict[str, object]:
    blockers = [] if state != "blocked" else [{"code": "required_tool_git_unavailable"}]
    return {
        "schema_version": 1,
        "receipt_type": "smolrunner-workspace-capability-receipt",
        "state": state,
        "source": {
            "commit": commit,
            "tree": "2" * 40,
            "clean_after": True,
        },
        "capability_fingerprint": "sha256:" + "3" * 64,
        "deviations": [{"code": "optional_tool_just_unavailable"}],
        "blocking_reasons": blockers,
    }


def doctor(overall: str = "pass") -> dict[str, object]:
    return {
        "schema_version": 1,
        "overall": overall,
        "checks": [{"id": "platform", "status": overall, "summary": "bounded"}],
    }


def main() -> int:
    ready = build_journey_report(bootstrap(), doctor(), None, bootstrap())
    assert ready["verdict"] == "ready"
    assert ready["next_action"] == "none"
    assert ready["doctor"] == {"evaluated": True, "overall": "pass"}

    warning = build_journey_report(bootstrap("ready_with_declared_deviations"), doctor("warn"), None, bootstrap("ready_with_declared_deviations"))
    assert warning["verdict"] == "ready"

    blocked = build_journey_report(bootstrap("blocked"), None, None, None)
    assert blocked["verdict"] == "blocked"
    assert blocked["blocking_codes"] == ["required_tool_git_unavailable"]

    failed = build_journey_report(bootstrap(), doctor("fail"), None, bootstrap())
    assert failed["verdict"] == "blocked"
    assert "doctor_reported_failure" in failed["blocking_codes"]

    unavailable = build_journey_report(bootstrap(), None, "doctor_timeout", bootstrap())
    assert unavailable["verdict"] == "blocked"
    assert unavailable["blocking_codes"] == ["doctor_timeout"]

    changed = build_journey_report(bootstrap(), doctor(), None, bootstrap(commit="4" * 40))
    assert changed["verdict"] == "blocked"
    assert "checkout_changed_during_journey" in changed["blocking_codes"]

    assert _valid_bootstrap_receipt(bootstrap())
    future_bootstrap = bootstrap()
    future_bootstrap["schema_version"] = 2
    assert not _valid_bootstrap_receipt(future_bootstrap)
    assert _valid_doctor_receipt(doctor())
    future_doctor = doctor()
    future_doctor["schema_version"] = 2
    assert not _valid_doctor_receipt(future_doctor)

    public = str(ready) + render_human(ready)
    for marker in ["/Users/", "/home/runner/", "secret.txt", "remote.origin.url", "GIT_ASKPASS", "HOME="]:
        assert marker not in public

    print("Glaeda front-door readiness tests passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
