"""Fault-injection checks for the replay harness itself; no agent installs needed."""

import importlib.util
import os
from pathlib import Path
import queue
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("live_replay", Path(__file__).with_name("live-replay.py"))
replay = importlib.util.module_from_spec(spec)
spec.loader.exec_module(replay)


def case(**step):
    value = {"name": "one", "operation": "append", "report_matches": True,
             "stream_matches": True, "commits_matches": True, "latency_ms": 12}
    value.update(step)
    return [{"scenario": "fixture", "errors": [], "expected_checkpoints": 1,
             "commits_required": True, "startup": {"matches_expected": True},
             "steps": [value]}]


class AccountingContractTests(unittest.TestCase):
    def test_components_cannot_hide_a_wrong_total(self):
        row = dict(zip(replay.KEYS, (1, 2, 3, 4)), totalTokens=11)
        with self.assertRaises(ValueError):
            replay.counts(row)

    def test_booleans_are_not_tokens(self):
        row = dict(zip(replay.KEYS, (True, 2, 3, 4)), totalTokens=10)
        with self.assertRaises(ValueError):
            replay.counts(row)

    def test_missing_correction_is_a_failure_even_when_report_passes(self):
        result = replay.summarize(case(stream_matches=False), 250, 1)
        self.assertFalse(result["passed"])
        self.assertEqual(result["failures"][0]["check"], "stream_matches")

    def test_missing_latency_event_cannot_become_a_fast_sample(self):
        result = replay.summarize(case(latency_ms=None), 250, 1)
        self.assertFalse(result["passed"])
        self.assertEqual(result["latency"]["samples"], 0)
        self.assertIsNone(result["latency"]["p95_ms"])

    def test_missing_reader_check_fails_closed(self):
        data = case()
        del data[0]["steps"][0]["commits_matches"]
        self.assertFalse(replay.summarize(data, 250, 1)["passed"])

    def test_incomplete_suite_and_missing_samples_cannot_pass(self):
        data = case()
        data[0]["expected_checkpoints"] = 2
        result = replay.summarize(data, 250, 2)
        self.assertFalse(result["passed"])
        self.assertEqual(len(result["failures"]), 2)

    def test_latency_budget_is_enforced(self):
        self.assertFalse(replay.summarize(case(latency_ms=251), 250, 1)["passed"])
        self.assertTrue(replay.summarize(case(latency_ms=249), 250, 1)["passed"])


class SubprocessTests(unittest.TestCase):
    def process(self, code, folder):
        return replay.JsonProcess([sys.executable, "-u", "-c", code], os.environ.copy(),
                                  folder, Path(folder) / "stderr")

    def test_invalid_json_is_an_error_not_an_empty_event_list(self):
        with tempfile.TemporaryDirectory() as folder:
            process = self.process("print('broken')", folder)
            try:
                with self.assertRaises(RuntimeError):
                    process.receive(2)
            finally:
                process.close()

    def test_partial_output_is_not_an_event_until_the_line_completes(self):
        with tempfile.TemporaryDirectory() as folder:
            code = "import sys; sys.stdout.write('{\"n\":'); sys.stdout.flush(); input(); print('1}')"
            process = self.process(code, folder)
            try:
                with self.assertRaises(queue.Empty):
                    process.receive(.1)
                process.child.stdin.write(b"continue\n")
                process.child.stdin.flush()
                self.assertEqual(process.receive(2)[1], {"n": 1})
            finally:
                process.close()

    def test_early_exit_is_an_error(self):
        with tempfile.TemporaryDirectory() as folder:
            process = self.process("pass", folder)
            try:
                with self.assertRaises(RuntimeError):
                    process.receive(2)
            finally:
                process.close()


if __name__ == "__main__":
    unittest.main()
