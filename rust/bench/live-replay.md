# Live accounting replay

`live-replay.py` checks whether Turbo Tokens can supply the live accounting
contract Commits needs. It compares independently specified token totals with:

1. Turbo Tokens' offline daily JSON report.
2. The sum of **new** usage events from its NDJSON stream.
3. Commits' actual `LiveUsageOracle`, when a Commits checkout is supplied.

The desired stream contract is intentionally stronger than the published v1
contract. v1 documents silent Claude replacements and historical startup
events. A failed stream check means **the feed cannot supply these accounting
increments**, not that v1 promised an interface it does not implement.

## Run

Python 3.9+ is required. The Swift reader additionally requires macOS and Swift.
Use the binary actually shipped to users; its version and SHA-256 are recorded.
No runtime packages are installed by the harness.

```sh
python3 rust/bench/live-replay.py \
  --turbotokens /absolute/path/to/turbotokens \
  --commits-repo /absolute/path/to/commit-markets \
  --samples 20 \
  --output /tmp/live-accounting.json
```

Omit `--commits-repo` to test Turbo Tokens alone on another platform. That run
does **not** claim Commits coverage. The harness exits 1 on any mismatch,
missing/ambiguous latency sample, exceeded latency budget, or execution error.
There is no flag to bless a failing baseline. Existing result files are never
overwritten.

Use `--agent codex` for the Codex release gate. It executes all four Codex
scenarios, records `agent_scope: "codex"`, and makes no Claude or latency claim.
The default still runs both agents and fails on the documented Claude correction
gaps. The release workflow runs the Codex gate against each of its six native
binaries before packaging and retains each result as a workflow artifact.

## Fixtures and expected counts

All input is synthetic and generated in a unique temporary directory. Expected
counts are specified as logical response vectors, independently of the readers:
noncached input, output, cache creation, cache reads. Every reported total must
also equal the sum of those four components. Prices are outside this test's
scope.

| Scenario | Behavior exercised |
| --- | --- |
| Claude append | Duplicate blocks, partial lines, invalid JSON, copied parent history in a subagent, restart and post-restart append |
| Claude response growth | A response's final block increases its output count; a duplicate final block adds nothing |
| Claude startup growth | Only the response growth after startup is new usage |
| Codex cumulative | Repeated and growing cumulative counters, explicit reset with `last_token_usage`, partial lines, restart |
| Codex rotation | A replacement inode contains retained history plus new usage |
| Codex truncation | An in-place truncation retains the latest cumulative counter; no new inference occurred |
| Codex startup rotation | Replacement before the first append preserves the preexisting counter as a baseline |
| Claude latency | Individually identifiable new responses, delivered one at a time |

A declining Codex cumulative count **without** `last_token_usage` is ambiguous:
it may be a reset, replay, or correction. This harness does not declare such a
record new billable usage. The reset fixture includes the explicit new request
usage. File removal with genuinely unavailable history is also outside scope;
the rotation/truncation fixtures retain enough information to reconstruct the
same billed work.

Historical startup usage events are recorded separately and never added to the
startup snapshot. A restart checks the snapshot against the full fixture total,
checks that the Commits session counter returns to zero, then checks subsequent
growth against the new baseline. This tests reconciliation boundaries, not a
durable consumer cursor or an exactly-once network transport.

## Real Commits source, isolated execution

The Swift driver compiles copied, unmodified implementations of the oracle,
JSONL parser, identity ledger, filesystem watcher, structured reader, and cost
estimator from the supplied checkout. It includes only the path-discovery
section of `CodexAccount.swift` and the verbatim `API.ModelRow` declaration,
excluding unrelated account/UI/network code. Original and compiled source
hashes, checkout revision, compiler version, and driver binary hash are recorded.
For app revisions that discover saved Claude homes, it also copies the pure
directory-discovery declarations from `ClaudeAccountStore.swift` verbatim.
No production checkout is edited and no app or uploader is launched.

Each subprocess receives an isolated synthetic home and agent path selectors.
The parent environment, real agent histories, credentials, and caches remain
untouched. Child processes have bounded waits and are terminated on completion
or failure. Errors retain the synthetic history, binary, Swift source copies,
Python harness, and stderr beside the result as `*.evidence/`.

To repeat against the exact saved binaries without recompilation:

```sh
python3 /tmp/live-accounting.evidence/live-replay.py \
  --turbotokens /tmp/live-accounting.evidence/turbotokens \
  --commits-reader /tmp/live-accounting.evidence/commits-reader/commits-replay \
  --output /tmp/live-accounting-repeat.json
```

## Measurement boundaries

- Latency is measured with a monotonic clock from starting a fixture append to
  receiving its complete, matching NDJSON usage line. An unrelated output line
  cannot satisfy a sample. All requested samples must arrive.
- The default poll interval is 100 ms, observation window is 500 ms, and p95
  budget is 250 ms. The observation window also checks that negative cases do
  not emit extra usage. Delayed events beyond that window fail the checkpoint.
- Commits is sampled explicitly with `snapshot(forceScan: true)`. Its recorded
  duration is **reader snapshot execution time**, not native event notification,
  the app's one-second sampling cadence, network delivery, or visible UI latency.
- Report duration includes process startup and JSON output. Reports are checked
  after each step; their durations are diagnostic, not a controlled cold/warm
  performance comparison.
- These are small histories. This run does not establish large-history scaling,
  CPU/RSS limits, pricing accuracy, other agents, compressed/SQLite Codex inputs,
  or current ccusage parity.

Harness fault-injection checks run in CI independently of the diagnostic replay:

```sh
python3 -m unittest discover -s rust/bench -p test_live_replay.py -v
```

They verify that missing reader checks, invalid tokens, incorrect totals,
silent corrections, incomplete runs, missing latency samples, corrupt NDJSON,
partial output and prematurely exiting processes cannot produce a green result.
Passing those tests does not mean the product replay passes.

## Recorded baseline: October 19, 2026

See [the recorded JSON](results/live-replay-20261019.json) for all checkpoints,
raw events, counts, latencies and source/binary hashes. The tested executable is
the bundled Turbo Tokens 1.1.4 release. This is not a measurement of an
unreleased source build; a source-build attempt was blocked by unavailable
crates.io downloads.

The seven scenarios contain 45 checkpoints and 142 accounting assertions.
All 45 daily reports match the independent fixture totals. Six live-contract
assertions fail, representing these three gaps:

| Transition | Expected tokens | Turbo Tokens event sum | Commits local reader |
| --- | ---: | ---: | ---: |
| Claude response grows from its first to final block | 200 | 100 | 200 |
| Codex file replaced with retained history plus new usage | 235 | 235 | 355 |
| Codex log truncated while retaining its latest cumulative counter | 235 | 470 | 470 |

All 20 new-response latency samples arrived, with p50 **81.8 ms** and p95
**100.6 ms** at a 100 ms poll interval. This is the small-history fixture above,
not an application UI or large-history performance claim.

The Claude failure also appears when a response straddles startup: 100 new
tokens are expected, Commits counts 100, and the stream emits none. Repeating
the final Claude block leaves the original discrepancy in place; it is not a
second independent bug.

The next production changes should address the missing Claude correction
contract and Codex deduplication across physical file replacement/truncation.
Preserve the valid reset-with-last-usage and post-restart tests while doing so.
These results alone do not justify switching Commits to the current event feed.

## Codex fix: October 19, 2026

The [follow-up replay](results/live-replay-codex-fix-20261019.json) uses a
source-built release executable and the fixed Commits oracle. All **67 Codex
accounting assertions pass**. The expanded suite has eight scenarios, 53
checkpoints and 167 accounting assertions. All 53 daily reports match the
independently specified totals.

| Transition | Expected | Fixed Turbo Tokens | Fixed Commits |
| --- | ---: | ---: | ---: |
| Replacement retaining history and growth | 235 | 235 | 235 |
| Observed truncation retaining the latest counter | 235 | 235 | 235 |
| Replacement before first append: new usage beyond startup baseline | 115 | 115 | 115 |

Reset-with-last-usage, replay across a reset, later growth, and restart checks
also pass. The fixture now specifies its initial history explicitly instead
of hardcoding the baseline by scenario name.

The full harness still exits **1**, with exactly three Claude stream failures:
final-block growth, its repeated final block, and a response straddling startup.
They are the previously recorded missing-correction contract, not Codex
failures. This change does not enable switching Commits to the event feed.

All 20 small-history latency samples arrived: p50 **25.6 ms**, p95 **89.4 ms**
at the existing 100 ms poll interval. These samples exercise new Claude
responses, not Codex or application UI latency. Scheduling variation makes
this unsuitable as a speedup claim against the earlier run.

The [10,000-event Codex report comparison](results/codex-report-parity-20261019.json)
is byte-identical against the shipped 1.1.4 executable. Five warm runs had
medians of 18.1 ms shipped and 17.8 ms fixed on the same 2.31 MB fixture;
these are comparable timings, not evidence of a material speedup. The existing
warm/uncached Claude parity check also passes on synthetic history.

### Validation and provenance

- All 95 Codex adapter tests pass, including replay metadata/pricing,
  timestamped last-usage records, identical headless completions, partial
  lines, logical session replacement and reset epochs.
- All 10 harness fault-injection tests pass. Release build, release Clippy,
  formatting and whitespace checks pass.
- Commits completed 983 tests with zero failures and two existing opt-in
  capture tests skipped. Its nine new replay tests and six bootstrap tests ran.
- A separate, read-only reviewer passed four Rust and six Swift adversarial
  runtime cases against the exact artifacts. See the
  [independent grade](results/codex-fix-independent-review-20261019.json).
- The precompiled oracle's source/compiler provenance is retained
  [separately](results/codex-fix-source-provenance-20261019.json). All production
  source hashes match Commits commit `649e52c4`, proposed in
  [Commits PR #380](https://github.com/maxmoneycash/commit-markets/pull/380).

The executable hashes in the replay and independent review match. Validation
uses isolated synthetic homes, without launching an uploader or changing an
installed app. The release version remains 1.1.4 in this unreleased source
build; its SHA-256 identifies the tested artifact.

The fix covers atomic replacement and an observed shrink. The prior limits
remain: truncate-and-regrow past the old offset between polls can look like
an append, and an already-complete JSON record awaiting a newline at startup
is treated as baseline. Headless records without stable IDs cannot identify
new occurrences after arbitrary loss of indistinguishable history. Large
history scaling, CPU/RSS bounds, compressed logs and SQLite remain outside
this validation.
