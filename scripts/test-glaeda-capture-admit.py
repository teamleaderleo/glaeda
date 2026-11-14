#!/usr/bin/env python3
"""Behavioral tests for whole-host protected capture admission; no real host or account."""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys
import time
import unittest

import importlib.machinery
import importlib.util

ROOT = Path(__file__).resolve().parents[1]
loader = importlib.machinery.SourceFileLoader('promotion_harness', str(ROOT / 'scripts/test-glaeda-promotion-admit.py'))
spec = importlib.util.spec_from_loader(loader.name, loader)
harness = importlib.util.module_from_spec(spec)
sys.modules[loader.name] = harness
loader.exec_module(harness)
HELPER = ROOT / 'scripts/glaeda-capture-admit'


class CaptureTest(unittest.TestCase):
    # The same fake listener and real flock ledger exercise both admission policies.
    setUp = harness.PromotionTest.setUp
    tearDown = harness.PromotionTest.tearDown
    set_tailnet = harness.PromotionTest.set_tailnet
    lease_dir = harness.PromotionTest.lease_dir
    state = harness.PromotionTest.state
    units_held = harness.PromotionTest.units_held
    wait_for = harness.PromotionTest.wait_for
    ready = harness.PromotionTest.ready
    started = harness.PromotionTest.started
    listener_alive = harness.PromotionTest.listener_alive

    def run_admit(self, request, timeout=30):
        self.state_root = self.home / '.local/state/glaeda/capture'
        return subprocess.run([sys.executable, str(HELPER), '--request-stdin'], input=json.dumps(request),
                              env=self.env, capture_output=True, text=True, timeout=timeout)

    def request(self, **overrides):
        request = harness.PromotionTest.request(self)
        request.pop('controller_url')
        request.pop('status_path')
        request.update(repository='manaflow-ai/cmuxterm-hq', workflow='pr-capture-cloud.yml', job='capture',
                       ref='refs/heads/main', runner_name='cmux-capture-' + self.lease_id,
                       runner_label='cmux-capture-' + self.lease_id + '-capture')
        request.update(overrides)
        return request

    def assign(self, **overrides):
        event = self.dir / 'dispatch.json'
        event.write_text(json.dumps({'repository': {'full_name': 'manaflow-ai/cmuxterm-hq', 'fork': False},
                                    'ref': 'refs/heads/main'}))
        env = dict(GITHUB_REPOSITORY='manaflow-ai/cmuxterm-hq', GITHUB_RUN_ID='4242', GITHUB_RUN_ATTEMPT='1',
                   GITHUB_SHA=harness.SHA, GITHUB_WORKFLOW_SHA=harness.SHA, GITHUB_JOB='capture',
                   RUNNER_NAME='cmux-capture-' + self.lease_id, GITHUB_EVENT_NAME='workflow_dispatch',
                   GITHUB_REF='refs/heads/main', GITHUB_ENV=str(self.dir / 'github-env'),
                   GITHUB_WORKFLOW_REF='manaflow-ai/cmuxterm-hq/.github/workflows/pr-capture-cloud.yml@refs/heads/main',
                   GITHUB_EVENT_PATH=str(event))
        env.update(overrides)
        (self.control / 'assign.json').write_text(json.dumps({'env': env}))

    def test_refuses_wrong_scope_before_listener_or_claim(self):
        for change in ({'repository': 'manaflow-ai/cmux'}, {'workflow': 'nightly.yml'}, {'job': 'proof'},
                       {'ref': 'refs/heads/branch'}, {'source_sha': 'x'}, {'run_attempt': True},
                       {'runner_label': 'glaeda-ios-sim'}, {'CLOUD_CAPTURE_PASSWORD': 'no'},
                       {'expires_at': harness.rfc(time.time() + 901)},
                       {'expires_at': harness.rfc(time.time() - 5)}):
            with self.subTest(change=change):
                result = self.run_admit(self.request(**change))
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertEqual(result.stdout, '')
                self.assertFalse((self.control / 'listener-start.json').exists())
                self.assertFalse(any(self.units_held()))

    def test_ready_holds_every_unit_before_listener_then_adopts_exact_job(self):
        receipt = self.ready()
        self.assertEqual(receipt['exclusive_claim'], self.lease_id)
        self.assertEqual(receipt['workflow'], 'pr-capture-cloud.yml')
        self.assertRegex(receipt['hook_sha256'], r'^[a-f0-9]{64}$')
        started = json.loads((self.control / 'listener-start.json').read_text())
        self.assertEqual(started['units_held'], [True] * harness.UNITS)
        self.assertNotIn(harness.JIT, json.dumps(started))
        self.assign()
        self.assertEqual(self.started()['rc'], 0, self.started())
        exported = (self.dir / 'github-env').read_text()
        self.assertIn('GLAEDA_JOB_CLAIM=exclusive', exported)
        self.assertIn('GLAEDA_CAPTURE_LEASE_ID=' + self.lease_id, exported)
        self.assertIn('GLAEDA_HOOK_SHA256=' + receipt['hook_sha256'], exported)
        (self.control / 'finish').touch()
        (self.control / 'worker-exit').touch()
        self.wait_for(lambda: self.state().get('phase') == 'released', why='owned worker exit releases lease')
        self.assertFalse(any(self.units_held()))
        self.assertEqual(self.controller.hits, [])

    def test_wrong_job_never_receives_claim(self):
        self.ready()
        self.assign(GITHUB_JOB='other')
        self.assertNotEqual(self.started()['rc'], 0)
        self.assertFalse((self.dir / 'github-env').exists())

    def test_unassigned_listener_expires_without_controller(self):
        self.ready(expires_at=harness.rfc(time.time() + 4))
        self.wait_for(lambda: self.state().get('phase') == 'expired', timeout=15, why='bounded unassigned lease')
        self.assertFalse(any(self.units_held()))
        self.assertEqual(self.controller.hits, [])

    def test_assigned_job_keeps_claim_past_admission_expiry(self):
        self.ready(expires_at=harness.rfc(time.time() + 4))
        self.assign()
        self.assertEqual(self.started()['rc'], 0)
        time.sleep(4)
        self.assertTrue(all(self.units_held()))
        self.assertTrue(self.listener_alive())


if __name__ == '__main__':
    unittest.main()
