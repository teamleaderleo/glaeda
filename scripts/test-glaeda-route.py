#!/usr/bin/env python3
"""Tests for scripts/glaeda-route. No network, no SSH, no GitHub: every source is a fixture."""
from __future__ import annotations

import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
import urllib.error
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "glaeda-route"
loader = importlib.machinery.SourceFileLoader("glaeda_route", str(SCRIPT))
spec = importlib.util.spec_from_loader("glaeda_route", loader)
gr = importlib.util.module_from_spec(spec)
sys.modules["glaeda_route"] = gr
loader.exec_module(gr)

NOW = 1_800_000_000.0
STD = "glaeda-std-xcode-26.6"
LIGHT = "glaeda-light-xcode-26.6"
REPO = "manaflow-ai/cmux"
SECRET = "ghs_supersecretvalue123"


def pools_doc(observed_at=NOW - 10, std=None, light=None):
    std = std or {"declared": ["m1", "m2", "m3", "m4"], "conforming": ["m1", "m2", "m3"],
                  "reserved": ["m4"], "busy": []}
    light = light or {"declared": ["l1", "l2"], "conforming": ["l1", "l2"]}
    return {"schema": "glaeda-mini-fleet/v1/pools", "observed_at": observed_at,
            "pools": {STD: std, LIGHT: light, "glaeda-mini": {"declared": ["x"]}}}


def runner(name, label, status="online", busy=False):
    return {"name": f"{name}-glaeda", "status": status, "busy": busy,
            "labels": [{"name": n} for n in ("self-hosted", "macOS", "glaeda-mini", label)]}


def runners():
    return {"total_count": 6, "runners": [
        runner("m1", STD), runner("m2", STD, busy=True), runner("m3", STD, status="offline"),
        runner("m4", STD), runner("l1", LIGHT), runner("l2", LIGHT),
        {"name": "tart-x", "status": "online", "busy": False, "labels": [{"name": "tart-canary"}]},
    ]}


def state(**kw):
    return gr.build_state(kw.pop("pools", pools_doc()), kw.pop("runners", runners()), repo=REPO, now=NOW, **kw)


class FakeResponse:
    def __init__(self, status, body):
        self.status, self._body = status, body

    def read(self):
        return b"" if self._body is None else json.dumps(self._body).encode()

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class FakeGitHub:
    """A urlopen stand-in: routes (method, path) to canned answers and records every request."""

    def __init__(self, routes):
        self.routes, self.calls = routes, []

    def __call__(self, request, timeout=None):
        path = request.full_url.removeprefix(gr.API)
        body = json.loads(request.data) if request.data else None
        self.calls.append((request.get_method(), path, body, dict(request.header_items())))
        for (method, prefix), answer in self.routes.items():
            if method == request.get_method() and path.startswith(prefix):
                status, data = answer(body) if callable(answer) else answer
                if status >= 400:
                    raise urllib.error.HTTPError(request.full_url, status, "err", {}, io.BytesIO(json.dumps(data).encode()))
                return FakeResponse(status, data)
        raise AssertionError(f"unexpected {request.get_method()} {path}")


def run_main(argv, env=None, opener=None):
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        code = gr.main(argv, env=env or {}, now=lambda: NOW, opener=opener)
    return code, out.getvalue(), err.getvalue()


class StateTests(unittest.TestCase):
    def test_counts_join_fleet_and_github(self):
        doc = state()
        self.assertEqual(doc["schema"], gr.STATE_SCHEMA)
        self.assertEqual(doc["order"], [STD, LIGHT])
        std = doc["pools"][STD]
        # m1 idle; m2 busy on GitHub; m3 offline; m4 reserved by the fleet.
        self.assertEqual((std["declared"], std["conforming"], std["reserved"], std["runners"],
                          std["online"], std["busy"], std["idle"]), (4, 3, 1, 4, 3, 1, 1))
        self.assertEqual(doc["pools"][LIGHT]["idle"], 2)
        self.assertEqual((std["class"], std["xcode"], std["rank"]), ("std", "26.6", 0))

    def test_no_host_names_leave_the_document(self):
        text = json.dumps(state())
        for name in ("m1", "m2", "l1", "tart"):
            self.assertNotIn(f'"{name}', text)

    def test_locked_or_reserved_member_is_never_idle(self):
        doc = state(pools=pools_doc(std={"declared": ["m1"], "conforming": ["m1"], "busy": ["m1"]}))
        self.assertEqual(doc["pools"][STD]["idle"], 0)
        self.assertEqual(doc["pools"][STD]["locked"], 1)

    def test_runner_without_pool_label_or_unknown_member_is_not_idle(self):
        rows = {"runners": [runner("m1", "glaeda-mini"), runner("stranger", STD)]}
        doc = state(runners=rows)
        self.assertEqual(doc["pools"][STD]["idle"], 0)
        self.assertEqual(doc["pools"][STD]["runners"], 1)

    def test_observed_at_is_the_oldest_input(self):
        doc = state(runners_observed_at=NOW - 5)
        self.assertEqual(doc["observed_at"], gr.utc(NOW - 10))
        doc = state(pools=pools_doc(observed_at=None))
        self.assertEqual(doc["observed_at"], gr.utc(NOW))

    def test_light_before_std_is_reordered_and_xl_is_not_routed(self):
        doc = state(pools={"observed_at": NOW, "pools": {LIGHT: {}, STD: {}, "glaeda-xl-xcode-26.6": {}}})
        self.assertEqual(doc["order"], [STD, LIGHT])

    def test_newer_xcode_sorts_first_within_a_class(self):
        doc = state(pools={"observed_at": NOW, "pools": {"glaeda-std-xcode-26.4": {}, STD: {}}})
        self.assertEqual(doc["order"], [STD, "glaeda-std-xcode-26.4"])

    def test_runner_in_two_pools_is_idle_in_one_only(self):
        both = {"name": "m1-glaeda", "status": "online", "busy": False,
                "labels": [{"name": STD}, {"name": "glaeda-std-xcode-26.4"}]}
        pools = {"observed_at": NOW, "pools": {STD: {"conforming": ["m1"]},
                                               "glaeda-std-xcode-26.4": {"conforming": ["m1"]}}}
        doc = gr.build_state(pools, {"runners": [both]}, repo=REPO, now=NOW)
        self.assertEqual((doc["pools"][STD]["idle"], doc["pools"]["glaeda-std-xcode-26.4"]["idle"]), (1, 0))

    def test_malformed_inputs_fail(self):
        with self.assertRaises(gr.Failure):
            state(pools={"pools": []})
        with self.assertRaises(gr.Failure):
            state(runners="nope")


class ValidateTests(unittest.TestCase):
    def check(self, doc, **kw):
        return gr.validate_state(doc, now=kw.pop("now", NOW), repo=kw.pop("repo", REPO), **kw)

    def test_fresh_document_is_usable(self):
        usable, reason = self.check(state())
        self.assertTrue(usable, reason)

    def test_stale_missing_foreign_future_and_malformed_are_not(self):
        doc = state()
        self.assertFalse(self.check(doc, now=NOW + 61)[0])
        self.assertTrue(self.check(doc, now=NOW + 49)[0])
        self.assertFalse(self.check(None)[0])
        self.assertFalse(self.check("")[0])
        self.assertFalse(self.check(doc, repo="someone/else")[0])
        self.assertFalse(self.check(doc, now=NOW - 120)[0])
        for mutate in (
            lambda d: d.update(schema="glaeda-pool-state/v0"),
            lambda d: d.update(generated_at="2027-01-15"),
            lambda d: d.update(observed_at=None),
            lambda d: d.update(order=[STD]),
            lambda d: d.update(order=[STD, 3]),
            lambda d: d["pools"][STD].update(idle=-1),
            lambda d: d["pools"][STD].update(idle=True),
            lambda d: d["pools"][STD].update(idle=9),
            lambda d: d["pools"].update({"blacksmith-6vcpu-macos-26": d["pools"].pop(LIGHT)}),
        ):
            bad = json.loads(json.dumps(doc))
            mutate(bad)
            if "order" in bad and isinstance(bad.get("pools"), dict) and "blacksmith-6vcpu-macos-26" in bad["pools"]:
                bad["order"] = sorted(bad["pools"])
            self.assertFalse(self.check(bad)[0], bad)

    def test_check_command_exit_codes(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "state.json"
            path.write_text(json.dumps(state()))
            code, out, _ = run_main(["check", "--state", str(path), "--repo", REPO])
            self.assertEqual(code, 0, out)
            path.write_text(json.dumps({**state(), "observed_at": gr.utc(NOW - 3600)}))
            code, out, _ = run_main(["check", "--state", str(path)])
            self.assertEqual(code, 1)
            self.assertIn("stale", out)


class PublishTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        (self.dir / "pools.json").write_text(json.dumps(pools_doc()))
        (self.dir / "runners.json").write_text(json.dumps(runners()))

    def tearDown(self):
        self.tmp.cleanup()

    def args(self, *extra):
        return ["publish", "--pools", str(self.dir / "pools.json"), *extra]

    def test_dry_run_writes_nothing(self):
        api = FakeGitHub({("GET", f"/repos/{REPO}/actions/runners"): (200, runners())})
        code, out, _ = run_main(self.args(), env={"GLAEDA_ROUTE_TOKEN": SECRET}, opener=api)
        self.assertEqual(code, 0)
        self.assertIn("would write GLAEDA_POOL_STATE", out)
        self.assertEqual([c[0] for c in api.calls], ["GET"])

    def test_publish_updates_variable_with_compact_document(self):
        api = FakeGitHub({("GET", f"/repos/{REPO}/actions/runners"): (200, runners()),
                          ("PATCH", f"/repos/{REPO}/actions/variables/GLAEDA_POOL_STATE"): (204, None)})
        code, out, err = run_main(self.args("--yes"), env={"GLAEDA_ROUTE_TOKEN": SECRET}, opener=api)
        self.assertEqual(code, 0, err)
        method, _, body, headers = api.calls[-1]
        self.assertEqual(method, "PATCH")
        doc = json.loads(body["value"])
        self.assertTrue(gr.validate_state(doc, now=NOW, repo=REPO)[0])
        self.assertEqual(headers["Authorization"], f"Bearer {SECRET}")
        self.assertNotIn(SECRET, out + err)
        self.assertIn("updated GLAEDA_POOL_STATE", out)

    def test_publish_creates_missing_variable(self):
        api = FakeGitHub({("PATCH", f"/repos/{REPO}/actions/variables/"): (404, {"message": "Not Found"}),
                          ("POST", f"/repos/{REPO}/actions/variables"): (201, None)})
        code, out, err = run_main(self.args("--runners", str(self.dir / "runners.json"), "--yes"),
                                  env={"GLAEDA_ROUTE_TOKEN": SECRET}, opener=api)
        self.assertEqual(code, 0, err)
        self.assertIn("created", out)

    def test_api_error_exits_2_without_leaking_the_token(self):
        api = FakeGitHub({("GET", f"/repos/{REPO}/actions/runners"): (403, {"message": "Resource not accessible"})})
        code, out, err = run_main(self.args("--yes"), env={"GLAEDA_ROUTE_TOKEN": SECRET}, opener=api)
        self.assertEqual(code, 2)
        self.assertIn("403", err)
        self.assertNotIn(SECRET, out + err)

    def test_missing_credentials_fail_closed(self):
        code, _, err = run_main(self.args("--yes"))
        self.assertEqual(code, 2)
        self.assertIn("no credentials", err)

    def test_token_file_must_be_private(self):
        path = self.dir / "token"
        path.write_text(SECRET + "\n")
        os.chmod(path, 0o644)
        code, _, err = run_main(self.args("--token-file", str(path), "--yes"))
        self.assertEqual(code, 2)
        self.assertIn("chmod 600", err)
        os.chmod(path, 0o600)
        api = FakeGitHub({("GET", f"/repos/{REPO}/actions/runners"): (200, runners()),
                          ("PATCH", f"/repos/{REPO}/actions/variables/"): (204, None)})
        code, _, err = run_main(self.args("--token-file", str(path), "--yes"), opener=api)
        self.assertEqual(code, 0, err)
        self.assertEqual(api.calls[0][3]["Authorization"], f"Bearer {SECRET}")

    def test_runner_listing_pages(self):
        page = {"runners": [runner(f"m{i}", STD) for i in range(100)]}
        api = FakeGitHub({("GET", f"/repos/{REPO}/actions/runners?per_page=100&page=1"): (200, page),
                          ("GET", f"/repos/{REPO}/actions/runners?per_page=100&page=2"): (200, {"runners": []})})
        self.assertEqual(len(gr.GitHub(SECRET, api).runners(REPO)), 100)


class AppTokenTests(unittest.TestCase):
    def test_jwt_is_signed_by_openssl_with_the_key_path_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            key = Path(tmp) / "app.pem"
            key.write_text("-----BEGIN PRIVATE KEY-----\nfake\n-----END PRIVATE KEY-----\n")
            os.chmod(key, 0o600)
            seen = {}

            def fake_run(argv, **kw):
                seen["argv"], seen["input"], seen["env"] = argv, kw["input"], kw["env"]
                return subprocess.CompletedProcess(argv, 0, b"signature", b"")

            jwt = gr.app_jwt("123", key, NOW, run=fake_run)
            self.assertEqual(seen["argv"][:4], [gr.OPENSSL, "dgst", "-sha256", "-sign"])
            self.assertNotIn("fake", " ".join(seen["argv"]))
            header, payload, sig = jwt.split(".")
            claims = json.loads(gr.base64.urlsafe_b64decode(payload + "=="))
            self.assertEqual((claims["iss"], claims["exp"] - claims["iat"]), ("123", 600))
            self.assertEqual(seen["input"], f"{header}.{payload}".encode())

            api = FakeGitHub({("GET", f"/repos/{REPO}/installation"): (200, {"id": 42}),
                              ("POST", "/app/installations/42/access_tokens"): (201, {"token": SECRET})})
            token = gr.installation_token("123", key, REPO, NOW, opener=api, run=fake_run)
            self.assertEqual(token, SECRET)
            self.assertEqual(api.calls[1][2], {"repositories": ["cmux"]})
            self.assertTrue(api.calls[0][3]["Authorization"].startswith("Bearer "))

    def test_failed_signature_is_a_failure(self):
        with tempfile.TemporaryDirectory() as tmp:
            key = Path(tmp) / "app.pem"
            key.write_text("x")
            os.chmod(key, 0o600)
            with self.assertRaises(gr.Failure):
                gr.app_jwt("1", key, NOW, run=lambda argv, **kw: subprocess.CompletedProcess(argv, 1, b"", b"bad"))


if __name__ == "__main__":
    unittest.main()
