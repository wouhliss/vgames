import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

from collect import Collector, EvidenceError, LogUnavailable, rust_test
from receipts import FORMAT, MAX_BYTES, filename, sanitize

ROOT = Path(__file__).resolve().parents[2]


class ReceiptTests(unittest.TestCase):
    def test_only_public_test_receipts_and_numeric_metrics_survive(self):
        for line in ('<input value="fixture-private-value">', 'token=fixture-private-value',
                     'ok 1 - part 1: add the server, sign in, choose a library (M1) # SKIP',
                     '- 1 foundation.spec.ts admins see the navigation',
                     'budget disk: peak fixture-private-value'):
            self.assertIsNone(sanitize(line))
        self.assertEqual(sanitize('Wine plan: /fixture-private-value/wine with Dxmt'),
                         'Wine plan: [redacted] with Dxmt')
        self.assertEqual(sanitize('✓ 1 [chromium] › foundation.spec.ts:10:1 › admins see the navigation; every page is reachable by keyboard (1s)'),
                         'foundation.spec.ts admins see the navigation')
        for line in ('test required_case ... ok', 'test required_case ... ignored',
                     'test result: ok. 0 passed; 0 failed; 1 ignored; finished in 0s',
                     'budget memory: 32 connections, 512 MiB: peak RSS 200 MiB (budget 256), 900 MiB/s (release build, including process start)',
                     'budget disk: peak 1024 bytes, final 512 bytes, 512 bytes beyond (allowance 12288, 7 samples)',
                     'budget throughput: 2 GiB loopback, download phase 1.00 s = 2.00 GB/s; whole install 2.00 s = 1.00 GB/s; peak connections 32, 32 buffers, 8 ranges served',
                     "budget throughput: the same folder takes plain sequential writes at 3.00 GB/s (the disk's ceiling for this run)"):
            self.assertIsNotNone(sanitize(line))
        metric = 'budget disk: peak 1024 bytes, final 512 bytes, 512 bytes beyond (allowance 12288, 7 samples)'
        self.assertEqual(sanitize('test disk_usage ... ' + metric), metric)

    def environment(self, temporary):
        return dict(os.environ, VGAMES_CAPTURE_RECEIPTS='1', RUNNER_TEMP=temporary,
                    VGAMES_EVIDENCE_JOB='tests', GITHUB_SHA='a' * 40,
                    GITHUB_RUN_ID='42', GITHUB_RUN_ATTEMPT='1', GITHUB_EVENT_NAME='workflow_dispatch')

    def test_real_wrapper_preserves_exit_status_and_artifacts_exclude_private_output(self):
        for status in (0, 7):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as temporary:
                command = "printf 'test required_case ... ok\\n'; printf 'fixture-private-value\\n' >&2; exit " + str(status)
                result = subprocess.run(['bash', 'scripts/ci/evidence.sh', 'scenario', 'bash', '-c', command],
                                        cwd=ROOT, env=self.environment(temporary), capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, status)
                payload = (Path(temporary) / 'milestone-receipts' / filename('scenario')).read_text()
                self.assertNotIn('fixture-private-value', payload)
                data = json.loads(payload)
                self.assertEqual(data['exit_code'], status)
                self.assertEqual(data['lines'], ['test required_case ... ok'])
                self.assertEqual(data['sha'], 'a' * 40)

    def test_capture_stdin_preserves_errexit_and_pipefail(self):
        with tempfile.TemporaryDirectory() as temporary:
            result = subprocess.run(['bash', 'scripts/ci/evidence.sh', 'scenario'],
                                    input="false | true\nprintf 'test unexpected ... ok\\n'\n",
                                    cwd=ROOT, env=self.environment(temporary), capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            data = json.loads((Path(temporary) / 'milestone-receipts' / filename('scenario')).read_text())
            self.assertNotEqual(data['exit_code'], 0)
            self.assertEqual(data['lines'], [])

    def test_pull_request_identity_uses_the_head_without_retaining_the_event_body(self):
        with tempfile.TemporaryDirectory() as temporary:
            event = Path(temporary) / 'event.json'
            event.write_text(json.dumps({'pull_request': {'head': {'sha': 'b' * 40}, 'body': 'fixture-private-value'}}))
            env = dict(self.environment(temporary), GITHUB_EVENT_NAME='pull_request', GITHUB_EVENT_PATH=str(event))
            result = subprocess.run(['bash', 'scripts/ci/evidence.sh', 'scenario', 'true'],
                                    cwd=ROOT, env=env, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0)
            payload = (Path(temporary) / 'milestone-receipts' / filename('scenario')).read_text()
            self.assertNotIn('fixture-private-value', payload)
            self.assertEqual(json.loads(payload)['sha'], 'b' * 40)


class ArtifactTests(unittest.TestCase):
    def setUp(self):
        self.collector = Collector('a' * 40)
        self.run = {'id': 42, 'run_attempt': 1}
        self.job = {'id': 13, 'name': 'tests', 'status': 'completed', 'conclusion': 'success',
                    'steps': [{'name': 'scenario', 'conclusion': 'success'}]}
        self.value = {'format': FORMAT, 'sha': 'a' * 40, 'run_id': 42, 'attempt': 1,
                      'job': 'tests', 'step': 'scenario', 'exit_code': 0,
                      'lines': ['test required_case ... ok']}
        self.artifact = {'id': 44, 'name': 'milestone-receipts-tests-1', 'expired': False, 'size_in_bytes': 1000}

    def archive(self, value, name=None):
        output = io.BytesIO()
        with zipfile.ZipFile(output, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
            archive.writestr(name or filename('scenario'), json.dumps(value))
        return output.getvalue()

    def proof(self, value=None, artifacts=None, archive_name=None):
        self.collector.receipt_log.cache_clear()
        response = subprocess.CompletedProcess([], 0, self.archive(value or self.value, archive_name), b'')
        with patch.object(self.collector, 'runs', return_value=[self.run]), \
             patch.object(self.collector, 'jobs', return_value=[self.job]), \
             patch.object(self.collector, 'step_log', side_effect=LogUnavailable('unavailable')), \
             patch('collect.api', return_value={'artifacts': artifacts if artifacts is not None else [self.artifact]}), \
             patch('collect.subprocess.run', return_value=response):
            return self.collector.proof('test', 'ci.yml', 'tests', 'scenario', [rust_test('required_case')])

    def test_in_progress_workflow_uses_the_scoped_producer_artifact(self):
        self.assertEqual(self.proof()['state'], 'passed')

    def test_wrong_commit_attempt_run_job_step_or_format_never_substitutes(self):
        for key, wrong in (('sha', 'b' * 40), ('attempt', 2), ('run_id', 99),
                           ('job', 'other'), ('step', 'other'), ('format', 'unknown')):
            value = dict(self.value, **{key: wrong})
            with self.subTest(field=key):
                self.assertEqual(self.proof(value)['state'], 'failed')

    def test_missing_expired_duplicate_and_wrong_step_artifacts_fail(self):
        for artifacts in ([], [dict(self.artifact, expired=True)], [self.artifact, self.artifact],
                          [dict(self.artifact, name='milestone-receipts-tests-0')]):
            self.assertEqual(self.proof(artifacts=artifacts)['state'], 'failed')
        self.assertEqual(self.proof(archive_name='../outside.json')['state'], 'failed')

    def test_incomplete_failed_or_private_payloads_fail_without_echoing_values(self):
        for change in ({'exit_code': None}, {'exit_code': 7}, {'exit_code': False},
                       {'lines': []}, {'lines': ['fixture-private-value']},
                       {'lines': ['test required_case ... ok'] * 2},
                       {'attempt': True}, {'private': 'fixture-private-value'},
                       {'lines': ['test required_case ... ignored']}):
            result = self.proof(dict(self.value, **change))
            self.assertEqual(result['state'], 'failed')
            self.assertNotIn('fixture-private-value', json.dumps(result))

    def test_oversized_uncompressed_receipts_are_rejected_before_reading(self):
        value = dict(self.value, lines=['x' * (MAX_BYTES + 1)])
        self.assertEqual(self.proof(value)['state'], 'failed')

    def test_artifacts_cannot_override_a_failed_recorded_command(self):
        with patch.object(self.collector, 'runs', return_value=[self.run]), \
             patch.object(self.collector, 'jobs', return_value=[self.job]), \
             patch.object(self.collector, 'step_log', side_effect=EvidenceError('Recorded evidence command failed')), \
             patch.object(self.collector, 'receipt_log') as receipt:
            result = self.collector.proof('test', 'ci.yml', 'tests', 'scenario', [rust_test('required_case')])
            self.assertEqual(result['state'], 'failed')
            receipt.assert_not_called()

    def test_failed_or_skipped_job_metadata_precedes_any_artifact(self):
        for conclusion in ('failure', 'skipped', 'cancelled'):
            self.job['conclusion'] = conclusion
            self.assertEqual(self.proof()['state'], 'failed')
