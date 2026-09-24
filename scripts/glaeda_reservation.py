"""glaeda_reservation: read a host reservation marker the same way everywhere.

/Users/Shared/cmux-build-fleet/reservation.json, written by `glaeda-mini-fleet reserve`:

    {"schema": "glaeda-reservation/v1", "owner": str, "purpose": str, "since": int, "until": int}

Times are Unix seconds (UTC). A marker is active while now < until, whoever owns it; every scheduler
skips an active host. Anything else in the file (bad JSON, another schema, a string or float time, a
missing field) is invalid, and invalid is treated like active: refuse, never guess. The runner's
job-started hook, glaeda-mini-fleet and its pools count all use this module. Pure: no I/O.
"""

from __future__ import annotations

import datetime
import json
from typing import Any

SCHEMA = "glaeda-reservation/v1"


def _int(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def parse(text: str) -> tuple[dict[str, Any] | None, str | None]:
    """(marker, None) for a valid v1 marker, or (None, why it is invalid)."""
    try:
        doc = json.loads(text)
    except ValueError:
        return None, "not JSON"
    if not isinstance(doc, dict):
        return None, "not a JSON object"
    if doc.get("schema") != SCHEMA:
        return None, f"schema is not {SCHEMA}"
    for field in ("owner", "purpose"):
        if not isinstance(doc.get(field), str):
            return None, f"{field} is not a string"
    for field in ("since", "until"):
        if not _int(doc.get(field)):
            return None, f"{field} is not integer Unix seconds"
    return {"owner": doc["owner"], "purpose": doc["purpose"], "since": doc["since"], "until": doc["until"]}, None


def active(marker: dict[str, Any], now: float) -> bool:
    return now < marker["until"]


def describe(marker: dict[str, Any]) -> str:
    until = datetime.datetime.fromtimestamp(marker["until"], datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    return f"{marker['owner'] or '?'} for {marker['purpose'] or '?'} until {until}"
