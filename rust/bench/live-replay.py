#!/usr/bin/env python3
"""Replay synthetic accounting histories through reports, streams, and Commits.

Python 3.9+, standard library only. Never reads the user's agent histories.
Expected counts come from fixture specifications, not a production parser or
another counter. A failed invariant is a result, not an automatically blessed
baseline. See live-replay.md for the measured boundaries and limitations.
"""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import queue
import shutil
import subprocess
import sys
import tempfile
import threading
import time


KEYS = ("inputTokens", "outputTokens", "cacheCreationTokens", "cacheReadTokens")
ZERO = (0, 0, 0, 0)
SOURCES = ("AccountingGrowth.swift", "UsageLogParser.swift", "UsageCostEstimator.swift",
           "FileChangeWatcher.swift", "StructuredUsageReader.swift", "CodexAccount.swift",
           "LiveUsageOracle.swift")


def counts(value):
    result = tuple(value[key] for key in KEYS)
    if any(type(n) is not int or n < 0 for n in result):
        raise ValueError("invalid token components: %r" % (result,))
    if type(value.get("totalTokens")) is not int or value["totalTokens"] != sum(result):
        raise ValueError("totalTokens does not equal its four components")
    return result


def add(a, b):
    return tuple(x + y for x, y in zip(a, b))


def subtract(a, b):
    return tuple(x - y for x, y in zip(a, b))


def encode(value):
    return (json.dumps(value, separators=(",", ":")) + "\n").encode()


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def run(command, **kwargs):
    return subprocess.run(command, check=True, capture_output=True, text=True,
                          timeout=kwargs.pop("timeout", 30), **kwargs).stdout


def percentile(values, p):
    return sorted(values)[max(0, math.ceil(len(values) * p) - 1)] if values else None


class JsonProcess:
    """A bounded-wait NDJSON reader; timestamps belong to received whole lines."""

    def __init__(self, command, env, cwd, stderr_path):
        self.errors = open(stderr_path, "wb")
        self.child = subprocess.Popen(command, env=env, cwd=cwd, stdin=subprocess.PIPE,
                                      stdout=subprocess.PIPE, stderr=self.errors)
        self.lines = queue.Queue()
        self.thread = threading.Thread(target=self._read, daemon=True)
        self.thread.start()

    def _read(self):
        try:
            for line in self.child.stdout:
                at = time.perf_counter()
                try:
                    self.lines.put((at, json.loads(line)))
                except (ValueError, UnicodeDecodeError) as error:
                    self.lines.put((at, {"readerError": str(error)}))
        finally:
            self.lines.put((time.perf_counter(), {"readerEOF": True}))

    def receive(self, timeout):
        at, item = self.lines.get(timeout=timeout)
        if "readerError" in item or "readerEOF" in item:
            raise RuntimeError("subprocess output failed: %r" % item)
        return at, item

    def drain(self, duration):
        deadline = time.perf_counter() + duration
        items = []
        while time.perf_counter() < deadline:
            try:
                items.append(self.receive(deadline - time.perf_counter()))
            except queue.Empty:
                break
        return items

    def request(self, op):
        self.child.stdin.write(encode({"op": op}))
        self.child.stdin.flush()
        return self.receive(10)[1]

    def close(self):
        if self.child.poll() is None:
            self.child.terminate()
            try:
                self.child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.child.kill()
                self.child.wait(timeout=5)
        self.thread.join(timeout=2)
        self.child.stdin.close()
        self.child.stdout.close()
        self.errors.close()


def compile_commits(repo, root):
    """Compile the actual oracle, watcher, parser and ledger, without the UI.

    API.ModelRow is copied verbatim from API.swift: only that data declaration
    is needed, not the API client and its unrelated application dependencies.
    """
    source = repo / "mac/Sources/CommitsSH"
    target = root / "commits-reader"
    target.mkdir()
    hashes = {}
    for name in SOURCES:
        text = (source / name).read_text()
        if name == "CodexAccount.swift":
            # The path-discovery declarations precede unrelated quota/account
            # UI models. Keep those declarations verbatim, including URL helpers.
            text, _ = text.split("\nstruct CodexAccountIdentity:", 1)
        (target / name).write_text(text)
        hashes[name] = {"original": digest(source / name), "compiled": digest(target / name)}
    api = (source / "API.swift").read_text()
    start = api.index("  struct ModelRow:")
    end = api.index("\n  }", start) + len("\n  }")
    model = "import Foundation\nenum API {\n" + api[start:end] + "\n}\n"
    (target / "ModelRow.swift").write_text(model)
    hashes["ModelRow.swift"] = digest(target / "ModelRow.swift")
    driver = Path(__file__).with_name("commits-replay.swift")
    shutil.copy2(driver, target / driver.name)
    hashes[driver.name] = digest(target / driver.name)
    binary = target / "commits-replay"
    command = ["swiftc", "-O", "-parse-as-library", "-o", str(binary)]
    command.extend(str(p) for p in sorted(target.glob("*.swift")))
    run(command, timeout=180)
    return binary, {"revision": run(["git", "-C", str(repo), "rev-parse", "HEAD"]).strip(),
                    "source_sha256": hashes,
                    "compiler": run(["swiftc", "--version"]).strip()}


def claude(stamp, identity, vector):
    i, o, w, r = vector
    return {"type": "assistant", "timestamp": stamp, "sessionId": "replay-claude",
            "requestId": "req-" + identity, "message": {
                "id": identity, "role": "assistant", "model": "claude-sonnet-4-5",
                "usage": {"input_tokens": i, "output_tokens": o,
                          "cache_creation_input_tokens": w, "cache_read_input_tokens": r}}}


def codex(stamp, vector, last=None):
    i, o, w, r = vector
    assert w == 0
    record = {"type": "event_msg", "timestamp": stamp,
            "payload": {"type": "token_count", "info": {"total_token_usage": {
                "input_tokens": i + r, "cached_input_tokens": r,
                "output_tokens": o, "reasoning_output_tokens": 0,
                "total_tokens": i + o + r}}}}
    if last is not None:
        li, lo, lw, lr = last
        assert lw == 0
        record["payload"]["info"]["last_token_usage"] = {
            "input_tokens": li + lr, "cached_input_tokens": lr,
            "output_tokens": lo, "reasoning_output_tokens": 0, "total_tokens": li + lo + lr}
    return record


def scenarios(stamp, samples):
    """Explicit fixture totals describe logical responses, independent of readers."""
    a, b, c = (10, 20, 30, 40), (3, 7, 11, 13), (2, 5, 7, 9)
    record = encode(claude(stamp, "response-a", a))
    second = encode(claude(stamp, "response-b", b))
    third = encode(claude(stamp, "response-c", c))
    # step: name, operation, relative path, bytes, expected current-history total
    path = ".claude/projects/replay/session.jsonl"
    child = ".claude/projects/replay/session/subagents/agent-child.jsonl"
    yield "claude-append", "claude", {}, [
        ("first-response", "append", path, record, a),
        ("duplicate-content-block", "append", path, record, a),
        ("partial-line", "append", path, second[:-8], a),
        ("complete-partial-line", "append", path, second[-8:], add(a, b)),
        ("malformed-line", "append", path, b'{"broken":\n', add(a, b)),
        ("copied-parent-plus-subagent", "append", child, record + third, add(add(a, b), c)),
        ("restart", "restart", path, b"", add(add(a, b), c)),
        ("post-restart-response", "append", path,
         encode(claude(stamp, "response-d", b)), add(add(a, b), add(c, b))),
    ]
    grown = (10, 120, 30, 40)
    yield "claude-response-growth", "claude", {}, [
        ("first-content-block", "append", path, record, a),
        ("final-content-block", "append", path, encode(claude(stamp, "response-a", grown)), grown),
        ("duplicate-final-block", "append", path, encode(claude(stamp, "response-a", grown)), grown),
    ]
    yield "claude-startup-growth", "claude", {path: record}, [
        ("response-straddles-startup", "append", path,
         encode(claude(stamp, "response-a", grown)), grown),
    ]
    cp = ".codex/sessions/2026/10/19/replay.jsonl"
    meta = encode({"type": "session_meta", "timestamp": stamp,
                   "payload": {"id": "replay-codex", "timestamp": stamp, "cwd": "/synthetic/replay"}})
    meta += encode({"type": "turn_context", "timestamp": stamp,
                    "payload": {"model": "gpt-5", "cwd": "/synthetic/replay"}})
    first, growing, reset, final = (60, 20, 0, 40), (140, 35, 0, 60), (20, 5, 0, 10), (28, 8, 0, 12)
    last = encode(codex(stamp, final))
    yield "codex-cumulative", "codex", {cp: meta}, [
        ("first-counter", "append", cp, encode(codex(stamp, first)), first),
        ("repeated-counter", "append", cp, encode(codex(stamp, first)), first),
        ("growing-counter", "append", cp, encode(codex(stamp, growing)), growing),
        # last_token_usage makes this an explicit new inference after reset.
        # A decreasing cumulative counter alone cannot distinguish new spend
        # from replay/correction and is deliberately not used as ground truth.
        ("reset-with-last-usage", "append", cp, encode(codex(stamp, reset, last=reset)), add(growing, reset)),
        ("partial-counter", "append", cp, last[:-8], add(growing, reset)),
        ("complete-counter", "append", cp, last[-8:], add(growing, final)),
        ("restart", "restart", cp, b"", add(growing, final)),
        ("post-restart-counter", "append", cp, encode(codex(stamp, (30, 10, 0, 13))),
         add(growing, (30, 10, 0, 13))),
    ]
    yield "codex-rotation", "codex", {cp: meta}, [
        ("first-counter", "append", cp, encode(codex(stamp, first)), first),
        # A replacement inode has retained history plus a new counter. This
        # tests exactly-once consumption, not deletion/correction semantics.
        ("replace-and-replay", "replace", cp,
         meta + encode(codex(stamp, first)) + encode(codex(stamp, growing)), growing),
    ]
    yield "codex-truncation", "codex", {cp: meta}, [
        ("first-counter", "append", cp, encode(codex(stamp, first)), first),
        ("growing-counter", "append", cp, encode(codex(stamp, growing)), growing),
        ("truncate-retaining-cumulative-total", "truncate", cp,
         meta + encode(codex(stamp, growing)), growing),
    ]
    steps = []
    total = ZERO
    for n in range(samples):
        vector = (n + 1, 3, 2, 5)
        total = add(total, vector)
        steps.append(("sample-%03d" % n, "append", path,
                      encode(claude(stamp, "latency-%03d" % n, vector)), total))
    yield "claude-latency", "claude", {}, steps


class Replay:
    def __init__(self, args, root, oracle_binary):
        self.args, self.root, self.oracle_binary = args, root, oracle_binary
        self.binary = root / "turbotokens"
        shutil.copy2(args.turbotokens, self.binary)
        self.results = []

    def start_stream(self, agent, env, folder, suffix):
        proc = JsonProcess([str(self.binary), "stream", "--agent", agent, "--offline",
                            "--timezone", "UTC", "--interval", str(self.args.interval_ms)],
                           env, folder, folder / ("stream-" + suffix + ".stderr"))
        try:
            at, initial = proc.receive(self.args.timeout)
            if initial.get("type") != "snapshot":
                raise ValueError("first stream message must be a snapshot")
            # Startup replays the history after its snapshot. These events are
            # NOT live increments. Keep them as evidence, but never add both.
            replay = proc.drain(self.args.settle_ms / 1000)
            return proc, initial, [{"receivedAt": t, "value": v} for t, v in replay]
        except BaseException:
            proc.close()
            raise

    def report(self, agent, env, folder):
        start = time.perf_counter()
        raw = run([str(self.binary), agent, "daily", "--json", "--offline", "--timezone", "UTC"],
                  env=env, cwd=folder, timeout=self.args.timeout)
        return json.loads(raw), (time.perf_counter() - start) * 1000

    def case(self, name, agent, initial, steps):
        folder = self.root / name
        home = folder / "home"
        # Each scenario gets a fresh timestamp after compilation and previous
        # scenarios finish. Preserve it across duplicate/rotation writes so a
        # replay is byte-identical, not a fabricated new timestamp/identity.
        stamp = datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
        def materialize(data):
            return data.replace(b"@TIMESTAMP@", stamp.encode())
        for sub in (".claude/projects", ".codex/sessions", ".codex/archived_sessions", ".cache", ".config"):
            (home / sub).mkdir(parents=True, exist_ok=True)
        for relative, data in initial.items():
            path = home / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(materialize(data))
        # These are the application's documented home selectors, scoped ONLY
        # to child processes. No shell or parent environment is changed.
        env = {"PATH": os.environ.get("PATH", ""), "HOME": str(home),
               "CODEX_HOME": str(home / ".codex"), "CLAUDE_CONFIG_DIR": str(home / ".claude"),
               "XDG_CACHE_HOME": str(home / ".cache"), "XDG_CONFIG_HOME": str(home / ".config"),
               "TZ": "UTC", "NO_COLOR": "1", "LOG_LEVEL": "0"}
        stream = oracle = None
        result = {"scenario": name, "agent": agent, "steps": [], "errors": [],
                  "expected_checkpoints": len(steps), "commits_required": bool(self.oracle_binary),
                  "fixture_timestamp": stamp}
        self.results.append(result)
        try:
            # Independently specified initial history: the one startup-growth
            # fixture has a preexisting response; all other baselines are zero.
            baseline = (10, 20, 30, 40) if name == "claude-startup-growth" else ZERO
            stream, snapshot, startup = self.start_stream(agent, env, folder, "initial")
            result["startup"] = {"snapshot": snapshot, "replayed": startup,
                                 "matches_expected": counts(snapshot) == baseline}
            if self.oracle_binary:
                oracle = JsonProcess([str(self.oracle_binary), str(home)], env, folder, folder / "commits.stderr")
                if oracle.receive(self.args.timeout)[1] != {"type": "ready"}:
                    raise ValueError("Commits reader did not initialize")
            emitted = ZERO
            previous = baseline
            for index, (label, op, relative, data, expected) in enumerate(steps):
                data = materialize(data)
                row = {"name": label, "operation": op, "expected_history": expected}
                result["steps"].append(row)
                if op == "restart":
                    stream.close()
                    stream = None
                    stream, snapshot, startup = self.start_stream(agent, env, folder, str(index))
                    row["restart_snapshot"] = snapshot
                    row["restart_snapshot_matches"] = counts(snapshot) == expected
                    row["startup_replayed_events"] = len(startup)
                    baseline, emitted = expected, ZERO
                    if oracle:
                        row["commits_restart"] = oracle.request("restart")
                        row["commits_restart_matches"] = counts(row["commits_restart"]) == ZERO
                else:
                    path = home / relative
                    path.parent.mkdir(parents=True, exist_ok=True)
                    started = time.perf_counter()
                    if op == "replace":
                        replacement = path.with_suffix(".replacement")
                        replacement.write_bytes(data)
                        replacement.replace(path)
                    elif op == "truncate":
                        path.write_bytes(data)
                    else:
                        with path.open("ab") as output:
                            output.write(data)
                    # Start at append/replace initiation: includes the actual
                    # write, close and reader delivery, not just parser time.
                    received = stream.drain(self.args.settle_ms / 1000)
                    row["events"] = [value for _at, value in received]
                    usage = [(at, value) for at, value in received if value.get("type") == "usage"]
                    for _at, value in usage:
                        emitted = add(emitted, counts(value))
                    if name == "claude-latency":
                        delta = subtract(expected, previous)
                        matching = [at for at, value in usage if counts(value) == delta
                                    and value.get("sessionId") == "session"
                                    and value.get("agent") == "claude"
                                    and value.get("project") == "replay"]
                        row["latency_ms"] = (matching[0] - started) * 1000 if len(matching) == 1 and len(usage) == 1 else None
                    row["expected_since_start"] = subtract(expected, baseline)
                    row["stream_since_start"] = emitted
                    row["stream_matches"] = emitted == subtract(expected, baseline)
                    if oracle:
                        observed = oracle.request("snapshot")
                        row["commits"] = observed
                        row["commits_matches"] = counts(observed) == subtract(expected, baseline)
                report, elapsed = self.report(agent, env, folder)
                row["report"] = report
                row["report_ms"] = elapsed
                row["report_matches"] = counts(report["totals"]) == expected
                previous = expected
        except Exception as error:
            result["errors"].append(type(error).__name__ + ": " + str(error))
        finally:
            if stream:
                stream.close()
            if oracle:
                oracle.close()
        return result


def summarize(results, latency_budget, samples_expected):
    failures = []
    latencies = []
    checks = 0
    for case in results:
        failures.extend({"scenario": case["scenario"], "error": error} for error in case["errors"])
        if len(case["steps"]) != case["expected_checkpoints"]:
            failures.append({"scenario": case["scenario"], "check": "incomplete_scenario"})
        if "startup" in case:
            checks += 1
            if not case["startup"]["matches_expected"]:
                failures.append({"scenario": case["scenario"], "check": "startup_snapshot"})
        for step in case["steps"]:
            required = ["report_matches"]
            if step["operation"] == "restart":
                required.append("restart_snapshot_matches")
                if case["commits_required"]:
                    required.append("commits_restart_matches")
            else:
                required.append("stream_matches")
                if case["commits_required"]:
                    required.append("commits_matches")
            for key in required:
                if key not in step:
                    failures.append({"scenario": case["scenario"], "step": step["name"],
                                     "check": "missing_" + key})
            for key, value in step.items():
                if key.endswith("_matches"):
                    checks += 1
                    if not value:
                        failures.append({"scenario": case["scenario"], "step": step["name"], "check": key})
            if "latency_ms" in step:
                if step["latency_ms"] is None:
                    failures.append({"scenario": case["scenario"], "step": step["name"], "check": "missing_or_ambiguous_latency_event"})
                else:
                    latencies.append(step["latency_ms"])
    p95 = percentile(latencies, .95)
    if len(latencies) != samples_expected:
        failures.append({"check": "latency_sample_count", "expected": samples_expected,
                         "actual": len(latencies)})
    if p95 is not None and p95 > latency_budget:
        failures.append({"check": "latency_budget", "p95_ms": p95, "budget_ms": latency_budget})
    return {"checks": checks, "failures": failures, "passed": not failures,
            "latency": {"samples": len(latencies), "p50_ms": percentile(latencies, .5),
                        "p95_ms": p95, "budget_ms": latency_budget}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--turbotokens", required=True, type=Path)
    commits = parser.add_mutually_exclusive_group()
    commits.add_argument("--commits-repo", type=Path, help="Compile its real local oracle (macOS and Swift required)")
    commits.add_argument("--commits-reader", type=Path, help="Reuse a driver binary from retained failure evidence")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--samples", type=int, default=20)
    parser.add_argument("--interval-ms", type=int, default=100)
    parser.add_argument("--settle-ms", type=int, default=500, help="Observation window per step, including negative checks")
    parser.add_argument("--timeout", type=float, default=30)
    parser.add_argument("--latency-budget-ms", type=float, default=250)
    args = parser.parse_args()
    if args.samples < 1 or args.interval_ms < 1 or args.settle_ms < 3 * args.interval_ms:
        parser.error("samples and interval must be positive; settle window must cover at least three polls")
    args.turbotokens = args.turbotokens.resolve(strict=True)
    args.output = args.output.resolve()
    if args.output.exists():
        parser.error("output already exists; choose a new result path")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    stamp = datetime.now(timezone.utc).isoformat(timespec="milliseconds").replace("+00:00", "Z")
    result = {"schema_version": 1, "started_at": stamp,
              "platform": platform.platform(), "machine": platform.machine(),
              "harness_sha256": digest(__file__), "interval_ms": args.interval_ms,
              "settle_ms": args.settle_ms, "samples_requested": args.samples,
              "commits": None}
    # Evidence is retained on failure, including synthetic logs and process
    # stderr. Temporary files and all owned children are removed on success.
    with tempfile.TemporaryDirectory(prefix="turbotokens-live-replay-") as temporary:
        root = Path(temporary)
        shutil.copy2(__file__, root / "live-replay.py")
        shutil.copy2(Path(__file__).with_name("commits-replay.swift"), root / "commits-replay.swift")
        try:
            oracle_binary = None
            if args.commits_repo:
                oracle_binary, result["commits"] = compile_commits(args.commits_repo.resolve(), root)
            elif args.commits_reader:
                oracle_binary = root / "commits-replay"
                shutil.copy2(args.commits_reader.resolve(strict=True), oracle_binary)
                result["commits"] = {"precompiled": True}
            if oracle_binary:
                result["commits"]["binary_sha256"] = digest(oracle_binary)
            replay = Replay(args, root, oracle_binary)
            # Fingerprint the immutable run copy, even if another task replaces
            # the original executable while the Swift driver is compiling.
            result["binary_sha256"] = digest(replay.binary)
            result["binary_version"] = run([str(replay.binary), "--version"]).strip()
            for case in scenarios("@TIMESTAMP@", args.samples):
                outcome = replay.case(*case)
                print("%s: %d checkpoints%s" % (case[0], len(outcome["steps"]),
                                              " (execution error)" if outcome["errors"] else ""), flush=True)
            result["scenarios"] = replay.results
            result["summary"] = summarize(replay.results, args.latency_budget_ms, args.samples)
        except Exception as error:
            detail = error.stderr if isinstance(error, subprocess.CalledProcessError) else str(error)
            result["summary"] = {"passed": False, "execution_error": detail}
        if not result["summary"]["passed"]:
            evidence = args.output.with_suffix(".evidence")
            if evidence.exists():
                raise FileExistsError("evidence path already exists: " + str(evidence))
            shutil.copytree(root, evidence)
            result["evidence"] = str(evidence)
        args.output.write_text(json.dumps(result, indent=2) + "\n")
    print(json.dumps(result["summary"], indent=2))
    return 0 if result["summary"]["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
