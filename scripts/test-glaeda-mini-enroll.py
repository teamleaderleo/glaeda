#!/usr/bin/env python3
"""Contract tests for scripts/glaeda-mini-enroll. Runs on Linux CI: no Mac, no cargo, no builds."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import os
import sys
import unittest
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader("glaeda_mini_enroll", os.fspath(ROOT / "scripts" / "glaeda-mini-enroll"))
spec = importlib.util.spec_from_loader("glaeda_mini_enroll", loader)
me = importlib.util.module_from_spec(spec)
sys.modules["glaeda_mini_enroll"] = me
loader.exec_module(me)


def enrollment(state: str, node: str = "cmux-mac-001", reason: str | None = None) -> dict:
    return {"nodeId": node, "state": state, "quarantineReason": reason}


class Plan(unittest.TestCase):
    def test_fresh_mini_runs_every_step(self) -> None:
        steps = me.plan(me.State(False, None, False), "cmux-mac-001", False, False)
        self.assertEqual(steps, ["install-glaeda", "bootstrap", "enroll", "accept-local", "eligible", "status"])

    def test_fresh_mini_needs_a_node_id(self) -> None:
        with self.assertRaisesRegex(me.Stop, "--node-id"):
            me.plan(me.State(True, None, False), None, False, False)

    def test_eligible_node_with_current_receipt_only_reports(self) -> None:
        self.assertEqual(me.plan(me.State(True, enrollment("eligible"), True), None, False, False), ["status"])

    def test_resume_after_a_rejected_acceptance(self) -> None:
        steps = me.plan(me.State(True, enrollment("enrolling"), False), None, False, False)
        self.assertEqual(steps, ["accept-local", "eligible", "status"])

    def test_accepted_but_not_transitioned(self) -> None:
        self.assertEqual(me.plan(me.State(True, enrollment("enrolling"), True), None, False, False), ["eligible", "status"])

    def test_draining_node_comes_back(self) -> None:
        self.assertEqual(me.plan(me.State(True, enrollment("draining"), True), None, False, False), ["eligible", "status"])

    def test_reaccept_on_an_enrolling_node_runs_acceptance(self) -> None:
        steps = me.plan(me.State(True, enrollment("enrolling"), True), None, True, False)
        self.assertEqual(steps, ["accept-local", "eligible", "status"])

    def test_reacceptance_of_an_eligible_or_draining_node_stops_with_the_transition(self) -> None:
        # cmux_fleet.py refuses accept-local unless the node is enrolling.
        for state, reaccept, current in (("eligible", True, True), ("draining", False, False)):
            with self.subTest(state=state), self.assertRaisesRegex(me.Stop, "--to enrolling"):
                me.plan(me.State(True, enrollment(state), current), None, reaccept, False)

    def test_rebuild_does_not_force_acceptance(self) -> None:
        steps = me.plan(me.State(True, enrollment("eligible"), True), None, False, True)
        self.assertEqual(steps, ["install-glaeda", "status"])

    def test_a_different_node_id_is_refused(self) -> None:
        with self.assertRaisesRegex(me.Stop, "already enrolled as cmux-mac-001"):
            me.plan(me.State(True, enrollment("eligible"), True), "cmux-mac-002", False, False)

    def test_quarantined_and_retired_stop(self) -> None:
        with self.assertRaisesRegex(me.Stop, "disk_pressure"):
            me.plan(me.State(True, enrollment("quarantined", reason="disk_pressure"), False), None, False, False)
        with self.assertRaisesRegex(me.Stop, "retired"):
            me.plan(me.State(True, enrollment("retired"), False), None, False, False)


class CandidatePlan(unittest.TestCase):
    """A reviewed candidate replaces the source build; nothing compiles Rust on the node."""

    def test_fresh_mini_stages_instead_of_building(self) -> None:
        steps = me.plan(me.State(False, None, False), "cmux-mac-001", False, False, candidate=True)
        self.assertEqual(steps, ["stage", "bootstrap", "enroll", "accept-local", "eligible", "status"])

    def test_a_staged_generation_is_not_restaged(self) -> None:
        steps = me.plan(me.State(True, enrollment("eligible"), True), None, False, False, candidate=True)
        self.assertEqual(steps, ["status"])

    def test_a_new_candidate_on_an_eligible_node_stops_before_staging_work(self) -> None:
        with self.assertRaisesRegex(me.Stop, "fresh acceptance"):
            me.plan(me.State(False, enrollment("eligible"), False), None, False, False, candidate=True)

    def test_staged_code_is_inspected_before_it_runs(self) -> None:
        order = []
        with mock.patch.object(me, "DARWIN_REQUIRED", False), \
             mock.patch.object(me, "pick_python", return_value="/py"), \
             mock.patch.object(me, "read_json", return_value=None), \
             mock.patch.object(me.Path, "is_file", return_value=True), \
             mock.patch.object(me.Path, "exists", return_value=False), \
             mock.patch.object(me.Runner, "inspect", side_effect=lambda *a: order.append("inspect")), \
             mock.patch.object(me.Runner, "acceptance_current", side_effect=lambda *a: order.append("run") or False), \
             mock.patch("builtins.print"):
            me.main(["--cmux-root", "/c", "--node-id", "n", "--candidate", "/a", "--sha256", "s",
                     "--source", "36e07e36ea7b9bc9e04c366547a5312dd348024d"])
        self.assertEqual(order, ["inspect", "run"])

    def test_an_unreadable_enrollment_is_not_replaced(self) -> None:
        with mock.patch.object(me, "DARWIN_REQUIRED", False), \
             mock.patch.object(me, "pick_python", return_value="/py"), \
             mock.patch.object(me, "read_json", return_value=None), \
             mock.patch.object(me.Path, "exists", return_value=True), \
             mock.patch("builtins.print"):
            self.assertEqual(me.main(["--cmux-root", "/c", "--node-id", "n"]), 2)

    def test_candidate_steps_run_the_generation_tools_and_binary(self) -> None:
        generation = Path("/Users/op/Projects/glaeda-generations/36e07e36ea7b")
        runner = me.Runner("/py", me.Paths(Path("/Users/op")), Path("/cmux"), generation)
        self.assertEqual(runner.fleet_py, "/Users/op/Projects/glaeda-generations/36e07e36ea7b/scripts/cmux_fleet.py")
        self.assertEqual(runner.glaeda_bin, generation / "bin/glaeda")
        self.assertEqual(me.Paths(Path("/Users/op")).generation("36e07e36ea7b9bc9e04c366547a5312dd348024d"),
                         generation)

    def test_fleet_tools_run_without_writing_bytecode(self) -> None:
        runner = me.Runner("/py", me.Paths(Path("/Users/op")), Path("/cmux"), Path("/g"))
        with mock.patch.object(me.subprocess, "run") as run:
            runner.fleet("status", "e")
        self.assertEqual(run.call_args.args[0][:3], ["/py", "-B", "/g/scripts/cmux_fleet.py"])

    def test_candidate_flags_go_together(self) -> None:
        with mock.patch.object(me, "DARWIN_REQUIRED", False), \
             mock.patch.object(me, "pick_python", return_value="/py"), \
             mock.patch("builtins.print"):
            self.assertEqual(me.main(["--cmux-root", "/c", "--candidate", "/a.tar.gz"]), 2)


class Python(unittest.TestCase):
    def test_picks_the_first_313_or_newer(self) -> None:
        versions = {"/a": (3, 12), "/b": (3, 13), "/c": (3, 14)}
        with mock.patch.object(me, "PYTHON_CANDIDATES", ("/a", "/b", "/c")), \
             mock.patch.object(me.shutil, "which", return_value=None), \
             mock.patch.object(me.os, "access", return_value=True), \
             mock.patch.object(me, "python_version", side_effect=versions.get):
            self.assertEqual(me.pick_python(None), "/b")

    def test_none_when_only_old_pythons(self) -> None:
        with mock.patch.object(me, "PYTHON_CANDIDATES", ("/a",)), \
             mock.patch.object(me.shutil, "which", return_value=None), \
             mock.patch.object(me.os, "access", return_value=True), \
             mock.patch.object(me, "python_version", return_value=(3, 9)):
            self.assertIsNone(me.pick_python(None))

    def test_an_explicit_old_python_is_refused(self) -> None:
        with mock.patch.object(me.os, "access", return_value=True), \
             mock.patch.object(me, "python_version", return_value=(3, 12)):
            self.assertIsNone(me.pick_python("/usr/bin/python3"))


class Paths(unittest.TestCase):
    def test_paths_match_the_enrollment_runbook_and_mini_setup(self) -> None:
        with mock.patch.dict(os.environ, {}, clear=True):
            paths = me.Paths(Path("/Users/op"))
            self.assertEqual(paths.enrollment, Path("/Users/op/.config/glaeda/cmux-fleet/enrollment.json"))
            self.assertEqual(paths.acceptance,
                             Path("/Users/op/.config/glaeda/cmux-fleet/acceptance/cmux_macos_native_build.json"))
            self.assertEqual(paths.glaeda_bin, Path("/Users/op/.local/share/glaeda/cmux-fleet/glaeda"))
        setup = (ROOT / "scripts" / "glaeda-mini-setup").read_text()
        self.assertIn('".cache/glaeda/cmux-native-cache"', setup)
        self.assertIn('.local/share/glaeda/cmux-fleet/glaeda"', setup)


class PlanOnlyHasNoSideEffects(unittest.TestCase):
    def test_plan_mode_runs_no_step(self) -> None:
        with mock.patch.object(me, "DARWIN_REQUIRED", False), \
             mock.patch.object(me, "pick_python", return_value="/py"), \
             mock.patch.object(me.Runner, "acceptance_current", return_value=False), \
             mock.patch.object(me, "read_json", return_value=None), \
             mock.patch.object(me.Runner, "install_glaeda") as install, \
             mock.patch.object(me.Runner, "bootstrap") as bootstrap, \
             mock.patch.object(me.Runner, "accept_local") as accept, \
             mock.patch("builtins.print"):
            code = me.main(["--cmux-root", "/tmp/cmux", "--node-id", "cmux-mac-001"])
        self.assertEqual(code, 0)
        install.assert_not_called()
        bootstrap.assert_not_called()
        accept.assert_not_called()


if __name__ == "__main__":
    unittest.main()
