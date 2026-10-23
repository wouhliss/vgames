import contextlib
import io
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

from collect import Collector, EvidenceError, main, requirements, result, rust_test, state


class CollectorTests(unittest.TestCase):
    def setUp(self):
        self.collector = Collector("a" * 40)
        source = patch.object(self.collector, "source", return_value="name: CI\n")
        source.start()
        self.addCleanup(source.stop)
        self.run = {"id": 42, "head_sha": "a" * 40, "run_number": 2,
                    "run_attempt": 1, "status": "completed", "conclusion": "success"}
        self.job = {"id": 13, "name": "tests", "status": "completed", "conclusion": "success",
                    "steps": [{"name": "scenario", "conclusion": "success"}]}

    def proof(self, log="test required_case ... ok\n"):
        with patch.object(self.collector, "runs", return_value=[self.run]), \
                patch.object(self.collector, "jobs", return_value=[self.job]), \
                patch.object(self.collector, "step_log", return_value=log):
            return self.collector.proof("test", "ci.yml", "tests", "scenario", [rust_test("required_case")])

    def test_zero_tests_ignored_and_missing_receipts_fail(self):
        self.assertEqual(self.proof()["state"], "passed")
        for log in ("test result: ok. 0 passed", "test required_case ... ignored\n",
                    "test other_case ... ok\n", "test required_case ... FAILED\n"):
            self.assertEqual(self.proof(log)["state"], "failed")

    def test_existing_failed_cancelled_skipped_jobs_and_steps_fail(self):
        for conclusion in ("failure", "cancelled", "skipped", "timed_out"):
            self.job["conclusion"] = conclusion
            self.assertEqual(self.proof()["state"], "failed")
        self.job["conclusion"] = "success"
        self.job["steps"][0]["conclusion"] = "skipped"
        self.assertEqual(self.proof()["state"], "failed")

    def test_missing_and_duplicate_jobs_fail_but_running_job_is_pending(self):
        with patch.object(self.collector, "runs", return_value=[self.run]):
            for jobs in ([], [self.job, self.job]):
                with patch.object(self.collector, "jobs", return_value=jobs):
                    self.assertEqual(self.collector.proof("x", "ci.yml", "tests", "scenario", ["ok"])["state"], "failed")
        self.job["status"] = "in_progress"
        self.assertEqual(self.proof()["state"], "pending")

    def test_latest_failed_attempt_never_falls_back_to_green(self):
        older = dict(self.run, run_number=1)
        latest = dict(self.run, run_attempt=2, conclusion="failure")
        other_sha = dict(self.run, head_sha="b" * 40, run_number=100)
        with patch("collect.api", return_value={"workflow_runs": [older, other_sha, self.run, latest]}):
            self.assertEqual(self.collector.runs("ci.yml"), [latest, self.run, older])
            self.assertEqual(self.collector.workflow_health("CI", "ci.yml")["state"], "failed")

    def test_pagination_keeps_the_current_attempt(self):
        with patch("collect.api", side_effect=[{"jobs": [self.job] * 100}, {"jobs": [self.job]}]) as api:
            self.assertEqual(len(self.collector.jobs(42, 2)), 101)
            self.assertIn("attempts/2/jobs", api.call_args.args[0])
            self.assertIn("page=2", api.call_args.args[0])

    def test_unavailable_evidence_does_not_turn_green(self):
        with patch.object(self.collector, "runs", side_effect=EvidenceError("unavailable")):
            with self.assertRaises(EvidenceError):
                self.collector.workflow_health("CI", "ci.yml")
        self.assertEqual(state([result("missing", "pending", "pending")]), "not reached")
        self.assertEqual(state([result("missing", "pending", "pending"), result("existing", "failed", "failed")]), "failed")

    def test_empty_job_logs_use_the_same_job_fallback_and_still_fail_if_empty(self):
        with patch("collect.gh", side_effect=["", "test required_case ... ok\n"]) as gh:
            self.assertIn("required_case", self.collector.log(13))
            self.assertIn("13", gh.call_args.args)
        with patch("collect.gh", return_value=""):
            with self.assertRaisesRegex(EvidenceError, "empty job log"):
                self.collector.log(14)

    def test_history_requires_a_successful_named_main_gate_and_never_changes_current_failure(self):
        older = dict(self.run, id=41, run_number=1, head_branch="main")
        latest = dict(self.run, head_branch="main", conclusion="failure")
        foreign = dict(self.run, id=43, run_number=3, head_branch="feature")
        passed = dict(self.job, name="Milestone M1")
        skipped = dict(passed, conclusion="skipped")
        with patch("collect.api", return_value={"workflow_runs": [older, latest, foreign]}), \
             patch.object(self.collector, "jobs", side_effect=[[skipped], [passed]]):
            self.assertTrue(self.collector.last_green("m1").endswith("/41"))
        self.assertEqual(state([result("current test", "failed", "failed")]), "failed")
        with patch("collect.api", return_value={"workflow_runs": [latest]}), \
             patch.object(self.collector, "jobs", return_value=[skipped]):
            self.assertEqual(self.collector.last_green("m1"), "")

    def test_report_outputs_distinguish_pending_failed_and_reached_despite_green_history(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'outputs'
            for status, expected, code in (('pending', 'not reached', 0),
                                           ('failed', 'failed', 1), ('passed', 'reached', 0)):
                with patch('sys.argv', ['collect.py', 'm1', '--sha', 'a' * 40, '--report']), \
                     patch('collect.requirements', return_value=[result('proof', status, 'fixture')]), \
                     patch.object(Collector, 'last_green', return_value='https://github.com/wouhliss/vgames/actions/runs/41'), \
                     patch.dict(os.environ, {'GITHUB_OUTPUT': str(output), 'GITHUB_STEP_SUMMARY': ''}), \
                     contextlib.redirect_stdout(io.StringIO()):
                    output.write_text('')
                    self.assertEqual(main(), code)
                    self.assertEqual(output.read_text(), f'm1={expected}\n')

    def test_selected_commit_source_ignores_worktree_changes_and_distinguishes_missing_objects(self):
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.check_output(['git', '-C', directory, *args], stderr=subprocess.DEVNULL)
            git('init', '-q')
            git('config', 'user.name', 'Evidence fixture')
            git('config', 'user.email', 'fixture@example.invalid')
            git('config', 'commit.gpgsign', 'false')
            path = Path(directory) / 'workflow.yml'
            path.write_text('committed evidence\n')
            git('add', 'workflow.yml')
            git('commit', '-qm', 'fixture')
            sha = git('rev-parse', 'HEAD').decode().strip()
            path.write_text('uncommitted changes\n')
            with patch('collect.ROOT', Path(directory)):
                collector = Collector(sha)
                self.assertEqual(collector.source('workflow.yml'), 'committed evidence\n')
                self.assertIsNone(collector.source('missing.yml'))
                with self.assertRaisesRegex(EvidenceError, 'Fetch the selected'):
                    Collector('a' * 40).source('workflow.yml')

    def test_a_later_backend_receipt_cannot_satisfy_the_requested_step(self):
        log = ('job\tscenario\t2026-10-23T10:00:00Z test result: ok. 0 passed\n'
               'job\tother backend\t2026-10-23T10:00:01Z test required_case ... ok\n')
        with patch.object(self.collector, 'runs', return_value=[self.run]), \
             patch.object(self.collector, 'jobs', return_value=[self.job]), \
             patch.object(self.collector, 'named_job_log', return_value=log):
            self.assertEqual(self.collector.proof('x', 'ci.yml', 'tests', 'scenario',
                                                  [rust_test('required_case')])['state'], 'failed')
            self.assertNotIn('required_case', self.collector.step_log(13, 'scenario'))
        with patch.object(self.collector, 'named_job_log', return_value='test required_case ... ok\n'):
            with self.assertRaisesRegex(EvidenceError, 'step-scoped'):
                self.collector.step_log(13, 'scenario')

    def test_m3b_requires_each_wine_backend_despite_a_green_workflow(self):
        with patch.object(self.collector, 'proof', return_value=result('fixture', 'passed', 'fixture')) as proof:
            requirements('m3b', self.collector)
        cases = [call.args for call in proof.call_args_list if call.args[0].startswith('GAME-06 Wine ')]
        self.assertEqual(len(cases), 3)
        receipt = 'test compat::plan::wine_e2e::the_d3d11_test_game_runs_through_wine ... ok\n'
        for args, backend in zip(cases, ('Dxmt', 'Dxvk', 'Wined3d'), strict=True):
            job = dict(self.job, name=args[2], steps=[{'name': args[3], 'conclusion': 'success'}])
            with self.subTest(backend=backend), \
                 patch.object(self.collector, 'runs', return_value=[self.run]), \
                 patch.object(self.collector, 'jobs', return_value=[job]):
                with patch.object(self.collector, 'step_log', return_value=f'Wine plan: fixture with {backend}\n{receipt}'):
                    self.assertEqual(self.collector.proof(*args)['state'], 'passed')
                    job['steps'][0]['conclusion'] = 'failure'
                    self.assertEqual(self.collector.proof(*args)['state'], 'failed')
                job['steps'][0]['conclusion'] = 'success'
                for log in (receipt, f'Wine plan: fixture with OtherBackend\n{receipt}'):
                    with patch.object(self.collector, 'step_log', return_value=log):
                        self.assertEqual(self.collector.proof(*args)['state'], 'failed')

    def test_m4_collects_release_measurements_without_claiming_the_throughput_target(self):
        with patch.object(self.collector, 'proof', return_value=result('fixture', 'passed', 'fixture')) as proof, \
             patch.object(self.collector, 'workflow_health', return_value=result('fixture', 'passed', 'fixture')):
            rows = requirements('m4', self.collector)
        self.assertEqual(len(proof.call_args_list), 6)
        self.assertEqual(state(rows), 'not reached')
        footprint = ('test memory_stays_bounded_at_32_connections ... ok\n'
                     'test disk_usage_never_exceeds_the_final_size_plus_journal ... ok\n'
                     'budget memory: 32 connections, 512 MiB: peak RSS 100 MiB (release build, fixture)\n'
                     'budget disk: peak 42 bytes\n')
        throughput = ('test loopback_throughput ... ok\n'
                      'budget throughput: 2 GiB loopback, download phase 1 s\n'
                      'budget throughput: the same folder takes plain sequential writes at 1 GB/s\n')
        for call in proof.call_args_list:
            args = call.args
            log = footprint if args[3] == 'Memory and disk footprint' else throughput
            job = dict(self.job, name=args[2], steps=[{'name': args[3], 'conclusion': 'success'}])
            with self.subTest(label=args[0]), \
                 patch.object(self.collector, 'runs', return_value=[self.run]), \
                 patch.object(self.collector, 'jobs', return_value=[job]):
                with patch.object(self.collector, 'step_log', return_value=log):
                    self.assertEqual(self.collector.proof(*args)['state'], 'passed')
                for missing in (log.replace('budget ', 'missing '),
                                log.replace('release build', 'debug build').replace('2 GiB', '0.1 GiB')):
                    with patch.object(self.collector, 'step_log', return_value=missing):
                        self.assertEqual(self.collector.proof(*args)['state'], 'failed')


if __name__ == "__main__":
    unittest.main()
