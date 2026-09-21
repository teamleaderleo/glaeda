#!/usr/bin/env python3
from __future__ import annotations

import copy
import importlib.util
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = Path(__file__).with_name('cmux_execution_roles.py')
SPEC = importlib.util.spec_from_file_location('cmux_execution_roles', MODULE_PATH)
assert SPEC and SPEC.loader
m = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(m)

GEN = 'sha256:' + 'a' * 64
PREF = 'sha256:' + 'b' * 64
TOOLCHAIN = 'sha256:' + 'c' * 64
GLAEDA = 'sha256:' + 'd' * 64
ALT_WORKLOAD = 'sha256:' + 'e' * 64
ALT_TOOLCHAIN = 'sha256:' + 'f' * 64
ALT_GLAEDA = 'sha256:' + '1' * 64


def mac_node(state='eligible', generation=7, xcode='apple-xcode-26-sdk-26', memory='large', pressure=None):
    return {
        'schema': m.NODE_SCHEMA,
        'nodeId': 'cmux-mac-001',
        'state': state,
        'platform': 'macos',
        'architecture': 'arm64',
        'osVersionClass': 'macos-26',
        'enrollmentGeneration': 3,
        'glaedaGeneration': GLAEDA,
        'cpuClass': 'large',
        'memoryClass': memory,
        'diskClass': 'large',
        'capabilityGeneration': generation,
        'toolchainProfiles': {xcode: TOOLCHAIN},
        'roleWorkloadGenerations': {
            'benchmark': GEN,
            'cmux_macos_native_build': GEN,
            'cmux_macos_test': GEN,
            'diagnostic': GEN,
        },
        'capabilities': sorted({
            'apple_silicon',
            'app_host_test',
            'benchmark',
            'diagnostic',
            'native_glaeda_build',
            'native_release_build',
            'native_xcode_build',
        }),
        'pressure': pressure or {
            'cpu': 'normal',
            'memory': 'normal',
            'swap': 'none',
            'thermal': 'stable',
        },
    }


def linux_node(state='eligible', generation=4, memory='large'):
    return {
        'schema': m.NODE_SCHEMA,
        'nodeId': 'cmux-linux-001',
        'state': state,
        'platform': 'linux',
        'architecture': 'x86_64',
        'osVersionClass': 'ubuntu-24.04',
        'enrollmentGeneration': 2,
        'glaedaGeneration': GLAEDA,
        'cpuClass': 'large',
        'memoryClass': memory,
        'diskClass': 'large',
        'capabilityGeneration': generation,
        'toolchainProfiles': {'linux-rust-ci-2026-09': TOOLCHAIN},
        'roleWorkloadGenerations': {
            'artifact_cache': GEN,
            'background_replay': GEN,
            'benchmark': GEN,
            'cmux_linux_agent': GEN,
            'cmux_linux_ci': GEN,
            'diagnostic': GEN,
        },
        'capabilities': sorted({
            'artifact_service',
            'background_replay',
            'background_verification',
            'benchmark',
            'build_helper',
            'cgroup_v2',
            'diagnostic',
            'linux_agent',
            'linux_ci',
            'linux_ci_toolchain',
            'systemd_execution',
            'task_isolation',
            'web_ci',
        }),
        'pressure': {
            'cpu': 'normal',
            'memory': 'normal',
            'swap': 'none',
            'thermal': 'stable',
        },
    }


def canary(node, role, *, result='accepted', toolchain=None, capabilities=None, profiles=None):
    if capabilities is None:
        capabilities = set(m.ROLE_REQUIREMENTS[role]['requiredCapabilities'])
    if role == 'cmux_macos_native_build':
        capabilities |= {'native_release_build'}
    if role == 'cmux_macos_test':
        capabilities |= {'app_host_test'}
    if role == 'cmux_linux_ci':
        capabilities |= {'linux_ci', 'web_ci', 'background_verification'}
    if role == 'cmux_linux_agent':
        capabilities |= {'linux_agent', 'build_helper'}
    requires_toolchain = m.ROLE_REQUIREMENTS[role]['requiresToolchainProfile']
    default_toolchain = next(iter(node['toolchainProfiles'])) if requires_toolchain else None
    profile = toolchain if toolchain is not None else default_toolchain
    toolchain_generation = (
        node['toolchainProfiles'].get(profile) if profile is not None else None
    )
    return {
        'schema': m.ROLE_CANARY_SCHEMA,
        'nodeId': node['nodeId'],
        'enrollmentGeneration': node['enrollmentGeneration'],
        'glaedaGeneration': node['glaedaGeneration'],
        'capabilityGeneration': node['capabilityGeneration'],
        'role': role,
        'toolchainProfile': profile,
        'toolchainGeneration': toolchain_generation,
        'acceptedCapabilities': sorted(capabilities),
        'acceptedResourceProfiles': profiles or ['large', 'medium', 'small'],
        'workloadGeneration': node['roleWorkloadGenerations'][role],
        'result': result,
    }


def capacity(node, role, profile, slot, concurrent, *, result='accepted', pressure='normal', unfinished=0):
    requires_toolchain = m.ROLE_REQUIREMENTS[role]['requiresToolchainProfile']
    toolchain_profile = next(iter(node['toolchainProfiles'])) if requires_toolchain else None
    toolchain_generation = (
        node['toolchainProfiles'].get(toolchain_profile)
        if toolchain_profile is not None
        else None
    )
    return {
        'schema': m.CAPACITY_SCHEMA,
        'nodeId': node['nodeId'],
        'enrollmentGeneration': node['enrollmentGeneration'],
        'glaedaGeneration': node['glaedaGeneration'],
        'capabilityGeneration': node['capabilityGeneration'],
        'role': role,
        'workloadGeneration': node['roleWorkloadGenerations'][role],
        'toolchainProfile': toolchain_profile,
        'toolchainGeneration': toolchain_generation,
        'resourceProfile': profile,
        'slotClass': slot,
        'maxConcurrent': concurrent,
        'contentionEvidenceGeneration': GEN,
        'measurement': {
            'validatedCompletions': 8,
            'p50Millis': 1000,
            'p90Millis': 1400,
            'cpuPressure': pressure,
            'memoryPressure': pressure,
            'swapClass': 'none',
            'thermalBehavior': 'stable',
            'unfinishedWork': unfinished,
        },
        'result': result,
    }


def workload(operation, role, platform, architecture, capabilities, *, toolchain=None, memory='medium', profile='medium'):
    return {
        'schema': m.WORKLOAD_SCHEMA,
        'operation': operation,
        'role': role,
        'platform': platform,
        'architecture': architecture,
        'toolchainProfile': toolchain,
        'minimumCpuClass': 'medium',
        'minimumMemoryClass': memory,
        'requiredCapabilities': sorted(capabilities),
        'resourceProfile': profile,
    }


class RoleModelTests(unittest.TestCase):
    def test_closed_role_vocabulary_keeps_workload_specialization_as_capabilities(self):
        self.assertEqual(
            set(m.ROLES),            {
                'artifact_cache',
                'background_replay',
                'benchmark',
                'cmux_linux_agent',
                'cmux_linux_ci',
                'cmux_macos_native_build',
                'cmux_macos_test',
                'diagnostic',
            },
        )
        self.assertEqual(m.OPERATION_CONTRACTS['cmux_macos_release_admission']['role'], 'cmux_macos_native_build')
        self.assertIn('native_release_build', m.OPERATION_CONTRACTS['cmux_macos_release_admission']['requiredCapabilities'])
        self.assertEqual(m.OPERATION_CONTRACTS['cmux_web_ci_admission']['role'], 'cmux_linux_ci')

    def test_node_meets_exact_mac_role_and_workload_requirements(self):
        node = mac_node()
        canaries = [canary(node, 'cmux_macos_native_build')]
        caps = [
            capacity(node, 'cmux_macos_native_build', 'medium', 'mac_native_build_lane', 1),
        ]
        req = workload(
            'cmux_macos_compile_admission',
            'cmux_macos_native_build',
            'macos',
            'arm64',
            {'native_xcode_build'},
            toolchain='apple-xcode-26-sdk-26',
        )
        self.assertEqual(m.select_eligible(req, node, canaries, caps), (True, 'eligible'))
        decision = m.local_admission(req, node, canaries, caps, {})
        self.assertTrue(decision['accepted'])
        self.assertEqual(decision['authority'], 'admission_only')
        self.assertEqual(decision['slotClaims'], ['mac_native_build_lane'])

    def test_wrong_xcode_profile_refuses_cleanly(self):
        node = mac_node()
        canaries = [canary(node, 'cmux_macos_native_build')]
        caps = [capacity(node, 'cmux_macos_native_build', 'medium', 'mac_native_build_lane', 1)]
        req = workload(
            'cmux_macos_compile_admission',
            'cmux_macos_native_build',
            'macos',
            'arm64',
            {'native_xcode_build'},
            toolchain='apple-xcode-27-sdk-27',
        )
        self.assertEqual(m.select_eligible(req, node, canaries, caps), (False, 'toolchain_profile_missing'))

    def test_insufficient_memory_class_refuses(self):
        node = mac_node(memory='small')
        canaries = [canary(node, 'cmux_macos_native_build')]
        eligibility = m.role_eligibility(node, canaries)
        self.assertEqual(eligibility['cmux_macos_native_build']['reason'], 'insufficient_memory_class')

    def test_insufficient_cpu_class_refuses_linux_role(self):
        node = linux_node()
        node['cpuClass'] = 'small'
        canaries = [canary(node, 'cmux_linux_ci')]
        eligibility = m.role_eligibility(node, canaries)
        self.assertEqual(eligibility['cmux_linux_ci']['reason'], 'insufficient_cpu_class')

    def test_reenrollment_invalidates_old_role_canary(self):
        node = mac_node()
        receipt = canary(node, 'cmux_macos_native_build')
        node['enrollmentGeneration'] += 1
        eligibility = m.role_eligibility(node, [receipt])
        self.assertEqual(
            eligibility['cmux_macos_native_build']['reason'],
            'role_canary_enrollment_stale',
        )

    def test_role_workload_generation_change_invalidates_canary(self):
        node = mac_node()
        receipt = canary(node, 'cmux_macos_native_build')
        node['roleWorkloadGenerations']['cmux_macos_native_build'] = ALT_WORKLOAD
        eligibility = m.role_eligibility(node, [receipt])
        self.assertEqual(
            eligibility['cmux_macos_native_build']['reason'],
            'role_canary_workload_stale',
        )

    def test_glaeda_generation_change_invalidates_canary(self):
        node = mac_node()
        receipt = canary(node, 'cmux_macos_native_build')
        node['glaedaGeneration'] = ALT_GLAEDA
        eligibility = m.role_eligibility(node, [receipt])
        self.assertEqual(
            eligibility['cmux_macos_native_build']['reason'],
            'role_canary_glaeda_stale',
        )

    def test_exact_toolchain_generation_change_invalidates_canary(self):
        node = mac_node()
        receipt = canary(node, 'cmux_macos_native_build')
        node['toolchainProfiles']['apple-xcode-26-sdk-26'] = ALT_TOOLCHAIN
        eligibility = m.role_eligibility(node, [receipt])
        self.assertEqual(
            eligibility['cmux_macos_native_build']['reason'],
            'toolchain_canary_pending',
        )

    def test_failed_role_canary_refuses(self):
        node = mac_node()
        eligibility = m.role_eligibility(node, [canary(node, 'cmux_macos_native_build', result='rejected')])
        self.assertEqual(eligibility['cmux_macos_native_build']['reason'], 'role_canary_failed')

    def test_draining_node_refuses_every_role(self):
        node = mac_node(state='draining')
        eligibility = m.role_eligibility(node, [canary(node, 'cmux_macos_native_build')])
        self.assertEqual(eligibility['cmux_macos_native_build']['reason'], 'node_draining')

    def test_role_is_removed_after_upgrade_until_recanary(self):
        old = mac_node(generation=7)
        receipt = canary(old, 'cmux_macos_native_build')
        upgraded = mac_node(generation=8, xcode='apple-xcode-27-sdk-27')
        eligibility = m.role_eligibility(upgraded, [receipt])
        self.assertEqual(eligibility['cmux_macos_native_build']['reason'], 'role_canary_pending')

    def test_multiple_roles_share_one_scarce_slot(self):
        node = mac_node()
        canaries = [
            canary(node, 'cmux_macos_native_build'),
            canary(node, 'cmux_macos_test'),
        ]
        caps = [
            capacity(node, 'cmux_macos_native_build', 'large', 'mac_native_build_lane', 1),
            capacity(node, 'cmux_macos_test', 'large', 'mac_app_host_test_slot', 2),
            capacity(node, 'cmux_macos_test', 'large', 'mac_native_build_lane', 2),
        ]
        compile_req = workload(
            'cmux_macos_compile_admission', 'cmux_macos_native_build', 'macos', 'arm64',
            {'native_xcode_build'}, toolchain='apple-xcode-26-sdk-26', profile='large'
        )
        test_req = workload(
            'cmux_macos_app_host_test', 'cmux_macos_test', 'macos', 'arm64',
            {'app_host_test'}, toolchain='apple-xcode-26-sdk-26', profile='large'
        )
        admitted = m.local_admission(compile_req, node, canaries, caps, {})
        self.assertTrue(admitted['accepted'])
        self.assertEqual(admitted['authority'], 'admission_only')
        self.assertEqual(admitted['leaseBoundary'], 'physical_execution_lease')
        compile_refused = m.local_admission(
            compile_req, node, canaries, caps, {'mac_native_build_lane': 1}
        )
        self.assertFalse(compile_refused['accepted'])
        refused = m.local_admission(test_req, node, canaries, caps, {'mac_native_build_lane': 2})
        self.assertFalse(refused['accepted'])
        self.assertEqual(refused['reason'], 'physical_slot_unavailable')
        self.assertIn('mac_native_build_lane', refused['slotClaims'])

    def test_external_scheduler_cannot_request_ineligible_role(self):
        node = linux_node()
        canaries = [canary(node, 'cmux_linux_ci')]
        caps = [capacity(node, 'cmux_linux_ci', 'medium', 'linux_medium_slot', 4)]
        req = workload(
            'cmux_linux_agent_admission', 'cmux_linux_agent', 'linux', 'x86_64',
            {'linux_agent'}, toolchain='linux-rust-ci-2026-09'
        )
        decision = m.local_admission(req, node, canaries, caps, {})
        self.assertFalse(decision['accepted'])
        self.assertEqual(decision['reason'], 'role_canary_pending')

    def test_node_pressure_between_selection_and_local_admission_vetoes(self):
        selected = mac_node()
        canaries = [canary(selected, 'cmux_macos_native_build')]
        caps = [capacity(selected, 'cmux_macos_native_build', 'medium', 'mac_native_build_lane', 1)]
        req = workload(
            'cmux_macos_compile_admission', 'cmux_macos_native_build', 'macos', 'arm64',
            {'native_xcode_build'}, toolchain='apple-xcode-26-sdk-26'
        )
        self.assertEqual(m.select_eligible(req, selected, canaries, caps), (True, 'eligible'))
        pressured = copy.deepcopy(selected)
        pressured['pressure']['memory'] = 'critical'
        decision = m.local_admission(req, pressured, canaries, caps, {})
        self.assertFalse(decision['accepted'])
        self.assertEqual(decision['reason'], 'node_pressured')

    def test_resource_profile_requires_reviewed_measurement(self):
        node = linux_node()
        canaries = [canary(node, 'cmux_linux_ci')]
        req = workload(
            'cmux_linux_ci_admission', 'cmux_linux_ci', 'linux', 'x86_64',
            {'linux_ci'}, toolchain='linux-rust-ci-2026-09', profile='large'
        )
        caps = [capacity(node, 'cmux_linux_ci', 'medium', 'linux_medium_slot', 4)]
        self.assertEqual(m.select_eligible(req, node, canaries, caps), (False, 'resource_profile_unmeasured'))

    def test_stale_capacity_evidence_refuses_after_workload_generation_change(self):
        node = linux_node()
        canaries = [canary(node, 'cmux_linux_ci')]
        old_capacity = capacity(
            node, 'cmux_linux_ci', 'medium', 'linux_medium_slot', 4
        )
        node['roleWorkloadGenerations']['cmux_linux_ci'] = ALT_WORKLOAD
        canaries = [canary(node, 'cmux_linux_ci')]
        req = workload(
            'cmux_linux_ci_admission', 'cmux_linux_ci', 'linux', 'x86_64',
            {'linux_ci'}, toolchain='linux-rust-ci-2026-09'
        )
        self.assertEqual(
            m.select_eligible(req, node, canaries, [old_capacity]),
            (False, 'resource_profile_unmeasured'),
        )

    def test_background_profile_still_requires_measurement(self):
        node = linux_node()        canaries = [canary(node, 'background_replay', profiles=['small'])]
        req = workload(
            'background_replay', 'background_replay', 'linux', 'x86_64',
            {'background_replay'}, profile='small', memory='small'
        )
        req['minimumCpuClass'] = 'small'
        self.assertEqual(
            m.select_eligible(req, node, canaries, []),
            (False, 'resource_profile_unmeasured'),
        )
        evidence = [capacity(node, 'background_replay', 'small', 'background_capacity', 4)]
        self.assertEqual(m.select_eligible(req, node, canaries, evidence), (True, 'eligible'))

    def test_capacity_receipt_records_contention_window_fields_without_remote_raw_cpu_ram(self):
        node = linux_node()
        receipt = capacity(node, 'cmux_linux_ci', 'medium', 'linux_medium_slot', 4)
        validated = m.validate_capacity(receipt)
        measurement = validated['measurement']
        self.assertEqual(validated['contentionEvidenceGeneration'], GEN)
        self.assertEqual(
            set(measurement),
            {
                'validatedCompletions', 'p50Millis', 'p90Millis', 'cpuPressure',
                'memoryPressure', 'swapClass', 'thermalBehavior', 'unfinishedWork'
            },
        )
        self.assertNotIn('cpu', validated)
        self.assertNotIn('ramBytes', validated)
        self.assertNotIn('cgroup', validated)

    def test_one_mac_and_one_linux_can_each_hold_two_roles(self):
        mac = mac_node()
        mac_roles = m.role_eligibility(mac, [
            canary(mac, 'cmux_macos_native_build'),
            canary(mac, 'cmux_macos_test'),
        ])
        self.assertTrue(mac_roles['cmux_macos_native_build']['eligible'])
        self.assertTrue(mac_roles['cmux_macos_test']['eligible'])

        linux = linux_node()
        linux_roles = m.role_eligibility(linux, [
            canary(linux, 'cmux_linux_ci'),
            canary(linux, 'cmux_linux_agent'),
        ])
        self.assertTrue(linux_roles['cmux_linux_ci']['eligible'])
        self.assertTrue(linux_roles['cmux_linux_agent']['eligible'])

    def test_preferred_is_advisory_and_separate_from_eligibility(self):
        node = mac_node()
        canaries = [canary(node, 'cmux_macos_test')]
        preference = {
            'schema': m.PREFERENCE_SCHEMA,
            'nodeId': node['nodeId'],
            'role': 'cmux_macos_native_build',
            'evidenceGeneration': PREF,
            'preference': 'preferred',
            'authority': 'observation_only',
        }
        status = m.operator_status(node, canaries, [], [preference])
        self.assertIn('cmux_macos_native_build', status['preferred'])
        self.assertNotIn('cmux_macos_native_build', status['eligible'])

    def test_compact_operator_status_hides_internal_objects(self):
        node = mac_node(state='active')
        canaries = [
            canary(node, 'cmux_macos_native_build'),
            canary(node, 'cmux_macos_test'),
        ]
        caps = [
            capacity(node, 'cmux_macos_native_build', 'medium', 'mac_native_build_lane', 1),
            capacity(node, 'cmux_macos_test', 'medium', 'mac_app_host_test_slot', 2),
        ]
        status = m.operator_status(node, canaries, caps)
        human = m.render_status(status)
        self.assertIn('node cmux-mac-001', human)
        self.assertIn('cmux_macos_native_build', human)
        self.assertIn('mac_native_build_lane: 1', human)
        self.assertIn('state: active', human)
        self.assertNotIn('capabilityGeneration', human)
        self.assertNotIn('workloadGeneration', human)

    def test_workload_has_no_machine_selector_or_raw_resource_controls(self):
        req = workload(
            'cmux_macos_compile_admission', 'cmux_macos_native_build', 'macos', 'arm64',
            {'native_xcode_build'}, toolchain='apple-xcode-26-sdk-26'
        )
        m.validate_workload(req)
        self.assertNotIn('nodeId', req)
        self.assertNotIn('cpu', req)
        self.assertNotIn('ramBytes', req)
        self.assertNotIn('cgroup', req)


if __name__ == '__main__':
    unittest.main()