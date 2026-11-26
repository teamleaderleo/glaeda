#!/usr/bin/env python3
"""What one `glaeda-gh read` (the upstream read a downstream host's tick sends, cmd_read) costs in-process: files opened,
json decodes and thread CPU per call, for N keys (a third each pr, run and head) whose watch and cache files are older
than STABLE_RACY, as a downstream that polls unchanged keys finds them.

A temporary GLAEDA_GH_DIR with the daemon lock held by this process and N cached answers. Two warm calls, a wait past
STABLE_RACY, then REPS measured calls. --mode touched sends each line's downstream times (touched, asked), as an
upstream tick does; --mode plain sends none, so a head's consumer is stamped now on every call (its watch is written).
--script compares another copy of glaeda-gh (a checkout of the base, say) with the same harness.

    python3 scripts/benchmark-upstream-read.py [--keys 60] [--mode touched|plain] [--reps 50] [--script PATH] [--json]
"""

from __future__ import annotations

import argparse
import contextlib
import fcntl
import importlib.machinery
import importlib.util
import io
import json
import os
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--keys", type=int, default=60)
    ap.add_argument("--mode", choices=("touched", "plain"), default="touched")
    ap.add_argument("--reps", type=int, default=50)
    ap.add_argument("--age", type=float, default=2.5, help="seconds to wait so the files are past STABLE_RACY")
    ap.add_argument("--script", default=os.fspath(HERE / "glaeda-gh"))
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args()
    tmp = Path(tempfile.mkdtemp(prefix="glaeda-read-bench."))
    os.environ["GLAEDA_GH_DIR"] = os.fspath(tmp)
    os.environ["GLAEDA_GH_READER"] = "tool:benchmark"
    loader = importlib.machinery.SourceFileLoader("glaeda_gh", a.script)
    gg = importlib.util.module_from_spec(importlib.util.spec_from_loader("glaeda_gh", loader))
    sys.modules[loader.name] = gg
    loader.exec_module(gg)
    gg.BASE = tmp
    lock = open(tmp / "daemon.lock", "w")
    fcntl.flock(lock, fcntl.LOCK_EX)
    gg.write_json(tmp / "daemon.json", {"at": time.time()})
    now = time.time()
    lines = []
    for i in range(a.keys):
        key = (f"pr:o/r#{i + 1}", f"run:o/r/{1000 + i}", f"head:o/r@{i:040x}")[i % 3]
        checks = [{"name": f"c{j}", "status": "COMPLETED"} for j in range(20)]
        gg.write_json(gg.cache_dir() / gg.file_name(key), {"key": key, "data": {"n": i, "checks": checks},
                                                           "fetchedAt": now - 100, "dataAt": now - 100})
        line = {"key": key, "jobs": False, "wait": False, "reader": "h/session:bench"}
        if key.startswith("head:"):
            line["consumer"] = "lane"
        if a.mode == "touched":
            line.update(touched=now - 3, asked=now - 3)
        lines.append(line)
    text = json.dumps({"keys": lines})
    counts = {"open": 0, "decode": 0}
    sys.addaudithook(lambda ev, _args: counts.__setitem__("open", counts["open"] + 1) if ev == "open" else None)
    real_loads = json.loads

    class CountingJson:
        def __getattr__(self, name):
            return getattr(json, name)

        @staticmethod
        def loads(*args, **kw):
            counts["decode"] += 1
            return real_loads(*args, **kw)
    gg.json = CountingJson()

    def once() -> str:
        with contextlib.redirect_stdout(io.StringIO()) as out:
            gg.cmd_read(text)
        return out.getvalue()
    once()
    once()
    time.sleep(a.age)
    once()
    opens = decodes = 0
    cpu = []
    for _ in range(a.reps):
        counts.update(open=0, decode=0)
        start = time.thread_time()
        once()
        cpu.append((time.thread_time() - start) * 1000)
        opens += counts["open"]
        decodes += counts["decode"]
    cpu.sort()
    row = {"keys": a.keys, "mode": a.mode, "opens_per_read": opens / a.reps, "json_decodes_per_read": decodes / a.reps,
           "cpu_ms_median": round(cpu[len(cpu) // 2], 2), "cpu_ms_p90": round(cpu[int(len(cpu) * 0.9)], 2)}
    print(json.dumps(row) if a.json else "  ".join(f"{k} {v}" for k, v in row.items()))
    return 0


if __name__ == "__main__":
    sys.exit(main())
