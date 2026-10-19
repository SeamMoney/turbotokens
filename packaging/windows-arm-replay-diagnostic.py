#!/usr/bin/env python3
"""Read-only implementation review: exact imported fixture, extra diagnostic drain.
Never upgrades the original 500 ms result to PASS. All histories are synthetic.
"""
import argparse, hashlib, importlib.util, json, os, platform, shutil, time
from pathlib import Path
from types import SimpleNamespace

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--harness', type=Path, required=True)
p.add_argument('--binary', '--turbotokens', dest='turbotokens', type=Path, required=True)
p.add_argument('--output', type=Path, required=True)
p.add_argument('--iterations', type=int, default=20)
a = p.parse_args()
a.output = a.output.resolve()
a.output.mkdir(parents=True, exist_ok=False)
spec = importlib.util.spec_from_file_location('exact_replay', a.harness.resolve())
h = importlib.util.module_from_spec(spec)
spec.loader.exec_module(h)
original = h.JsonProcess
observed = []
active_replay = None

def stat_files(folder):
    values = []
    for path in sorted((folder / 'home' / '.codex').rglob('*.jsonl')):
        stat = path.stat()
        values.append({'path': str(path.relative_to(folder)),
                       'sha256': h.digest(path),
                       **{name: getattr(stat, name, None) for name in
                          ('st_size', 'st_ino', 'st_dev', 'st_ctime_ns', 'st_birthtime_ns', 'st_mtime_ns')}})
    return values

class ObservedProcess(original):
    def __init__(self, command, env, cwd, stderr_path):
        super().__init__(command, env, cwd, stderr_path)
        self.folder = Path(cwd)
        self.record = {'command': command, 'pid': self.child.pid, 'windows': []}
        observed.append(self.record)

    def drain(self, duration):
        started = time.perf_counter()
        rows = super().drain(duration)
        self.record['windows'].append({'start': started, 'end': time.perf_counter(),
                                      'requested_seconds': duration, 'files': stat_files(self.folder),
                                      'received': [{'at': at, 'value': value} for at, value in rows]})
        return rows

    def close(self):
        result = active_replay.results[-1] if active_replay and active_replay.results else {}
        last = result.get('steps', [{}])[-1]
        if 'stream_since_start' in last:
            started = time.perf_counter()
            rows = super().drain(5)
            late = h.ZERO
            for _, value in rows:
                if value.get('type') == 'usage':
                    late = h.add(late, h.counts(value))
            after = h.add(last['stream_since_start'], late)
            self.record['late_diagnostic'] = {
                'start': started, 'end': time.perf_counter(),
                'received': [{'at': at, 'value': value} for at, value in rows],
                'after_late_since_start': after,
                'eventually_exact': tuple(after) == tuple(last['expected_since_start']),
                'original_check_unchanged': last['stream_matches'],
                'files': stat_files(self.folder)}
        super().close()

h.JsonProcess = ObservedProcess
case = next(case for case in h.scenarios('@TIMESTAMP@', 1) if case[0] == 'codex-rotation')
results = []
args = SimpleNamespace(turbotokens=a.turbotokens.resolve(strict=True), interval_ms=100,
                       settle_ms=500, timeout=60)
for index in range(a.iterations):
    root = a.output / ('run-%03d' % index)
    root.mkdir()
    active_replay = h.Replay(args, root, None)
    result = active_replay.case(*case)
    summary = h.summarize([result], 250, 0)
    results.append({'iteration': index, 'result': result, 'summary': summary,
                    'observed': observed[-1]})
    (root / 'result.json').write_text(json.dumps(results[-1], indent=2) + '\n')
    print(json.dumps({'iteration': index, 'summary': summary,
                      'late': observed[-1].get('late_diagnostic')}), flush=True)
    # Avoid copying the executable once per synthetic run into the archive.
    active_replay.binary.unlink()
short_window_misses = []
persistent_failures = []
for run in results:
    result = run['result']
    for row in result['steps']:
        if row.get('stream_matches') is False:
            short_window_misses.append({'iteration': run['iteration'], 'step': row['name'],
                                        'expected': row.get('expected_since_start'),
                                        'observed': row.get('stream_since_start')})
        expected = row.get('expected_since_start', ())
        actual = row.get('stream_since_start', ())
        if any(x > y for x, y in zip(actual, expected)):
            persistent_failures.append({'iteration': run['iteration'], 'step': row['name'],
                                        'reason': 'extra stream counts'})
        if row.get('report_matches') is False:
            persistent_failures.append({'iteration': run['iteration'], 'step': row['name'],
                                        'reason': 'report mismatch'})
    late = run['observed'].get('late_diagnostic', {})
    if not late.get('eventually_exact') or result['errors'] or len(result['steps']) != result['expected_checkpoints']:
        persistent_failures.append({'iteration': run['iteration'], 'reason': 'persistent mismatch or execution error',
                                    'errors': result['errors']})
    if not result.get('startup', {}).get('matches_expected'):
        persistent_failures.append({'iteration': run['iteration'], 'reason': 'startup mismatch'})
classification = {'original_short_window_misses': short_window_misses,
                  'persistent_or_extra_count_failures': persistent_failures,
                  'diagnostic_passed': not persistent_failures,
                  'original_strict_suite_passed': all(run['summary']['passed'] for run in results)}
report = {'classification': classification, 'platform': platform.platform(), 'machine': platform.machine(),
          'harness_sha256': h.digest(a.harness), 'binary_sha256': h.digest(a.turbotokens),
          'diagnostic_sha256': h.digest(__file__), 'results': results}
(a.output / 'diagnostic.json').write_text(json.dumps(report, indent=2) + '\n')
shutil.make_archive(str(a.output), 'zip', a.output)
print('Retained complete synthetic evidence:', str(a.output) + '.zip')

print(json.dumps(classification, indent=2))
raise SystemExit(1 if persistent_failures else 0)
