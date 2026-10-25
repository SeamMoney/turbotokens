# turbotokens-adapter-muse

The Muse adapter: it turns the per-session `session.jsonl` logs written by
Meta's `muse` coding-agent CLI into the usage entries the focused and unified
reports render.

```bash
turbotokens muse daily --json
turbotokens muse weekly
turbotokens muse monthly
turbotokens muse session
turbotokens daily --json --by-agent   # Muse rows carry agent "muse"
```

## Owns

- `paths.rs` — `MUSE_DATA_DIR` / `XDG_DATA_HOME` root resolution and session-log discovery.
- `parser.rs` — record filtering, token mapping, timestamps, and cost-mode behavior.
- `loader.rs` — parallel reads and record-id dedupe across files.
- `report.rs` — daily, weekly, monthly, and session summary shapes.

Anything that is not specific to this source belongs in `turbotokens-core` or
`turbotokens-adapter-common` instead.

## Data source

```text
${MUSE_DATA_DIR:-${XDG_DATA_HOME:-~/.local/share}/muse}/
└── sessions/
    ├── YYYY/MM/DD/<session-id>/
    │   ├── session.jsonl                          # read
    │   ├── subagent/<child-id>/session.jsonl      # read (its own session)
    │   ├── tool-outputs/, approval-review/, input-assets/   # skipped
    │   └── cli-*.log, cron.db, *.sqlite3, .session.lock     # ignored
    └── .msp-view-*/                               # skipped (binary journals)
```

Only files named exactly `session.jsonl` are read. Hidden directories (the
`.msp-view-*` journal/index/snapshot stores and `.spool` scratch files) and the
tool-output, approval-review, and input-asset directories are never walked.

A non-empty `MUSE_DATA_DIR` replaces the default and may hold comma-separated
data directories (each the directory that contains `sessions/`). Configured
roots that do not exist are ignored and do not fall back to the default.
`XDG_DATA_HOME` is honored only when it is an absolute path, matching where
Muse itself writes sessions.

## Records

Each line is one JSON record. Usage comes mainly from records shaped like:

```json
{"schema_version":1,"id":"<record-id>","stream":{"kind":"session","id":"<session-id>"},
 "sequence":67,"recorded_at":1792892878389308,"record_type":"event",
 "payload_type":"runtime.session",
 "payload":{"kind":"run","run_id":"…","event":{"kind":"model_completed","model":"muse-wonder-1.0",
   "usage":{"input_tokens":45596,"output_tokens":512,"cached_tokens":38132,
            "cache_read_tokens":38132,"cache_write_tokens":0,"reasoning_tokens":210}}}}
```

A `model_completed` record counts when `payload_type` is `runtime.session` and
`payload.event.kind` is `model_completed`. A byte prefilter on the quoted
event kind keeps parsing cheap; the structural check is what decides, so tool
results that quote log lines are never counted.

A `goal_usage_attribution` record counts only as an unlogged reminder
side-call (below). Not counted:

- every other `goal_usage_attribution`: main-root and native-subagent
  attributions repeat a `model_completed` record one sequence later, and
  `tool`-family or unreported attributions carry no provider usage;
- lines with a top-level `retained_frame` (permission/bootstrap frames that
  wrap child records as JSON strings) and `retained_marker` placeholders for
  omitted live-only records;
- every other payload type and event kind;
- usage records whose counters are all zero.

Records are deduplicated by their top-level `id` across all files (and
reminder side-calls also by `usage_id`), so a forked or copied session is
counted once. The session of a record is its
`stream.id` (falling back to the session directory name); a subagent log is a
separate session. `recorded_at` is integer microseconds since the Unix epoch
(UTC) and is truncated to milliseconds; days follow `--timezone`.

The first `runtime.session.metadata` record in a log supplies the session's
project path (`workspace_root`, shown as `projectPath` in session reports) and
Muse build version. A log without one reports the project path `Muse`.

## Reminder side-calls

Muse's skill and memory reminder agents run short child sessions. A logged
child appears as `<parent>/subagent/<child-id>/session.jsonl` and counts
through its own `model_completed` records. An unlogged child exists only as a
`goal_usage_attribution` record in the parent's log. Such a record counts when
all of these hold:

- `record.owner.owner_type` is `reminder_child`;
- `record.usage_family` is `provider`;
- `record.quantity.reported` is `true`;
- no `session.jsonl` read by the adapter (under any configured root) sits in
  a directory named `record.owner.owner_id`.

The side-call's day is the record's `recorded_at` and its session is the log's
`stream.id` (the parent session). Tokens follow the same rule as
`model_completed`: input = `input_tokens − cached_tokens`, cache read =
`cached_tokens` (bounded by `input_tokens`), cache creation 0, and output =
`output_tokens`, which already includes `reasoning_tokens`.

Attribution records name no model, so these side-calls are reported as
`muse-spark-1.3`: Muse's model catalog sets `background_agent_model` to
`muse-spark-1.3` for every `muse-wonder` variant (contributor and full
included), and every reminder child that has a log ran `muse-spark-1.3`. If a
child's log appears later, the attribution stops counting and the log's own
records (with their recorded model) replace it.

## Token mapping

| Muse field | turbotokens bucket |
| --- | --- |
| `input_tokens − cache_read_tokens` | input |
| `cache_read_tokens` (or `cached_tokens` when only it is present) | cache read |
| `cache_write_tokens` | cache creation |
| `output_tokens` | output |

`input_tokens` is the whole prompt including its cached part, so the cache read
is subtracted from it. `cached_tokens` mirrors `cache_read_tokens` and is never
added a second time. `reasoning_tokens` is already inside `output_tokens` and
is not added. The cache read is bounded by `input_tokens`, so each record's
total is exactly `input_tokens + output_tokens + cache_write_tokens`. Muse has
not yet recorded a non-zero `cache_write_tokens`; it is treated as separate
from `input_tokens`, which is an assumption to revisit if Muse starts writing
it.

## Cost

Muse records no per-request cost, so `--mode display` reports zero. `auto`
and `calculate` estimate cost from an exact pricing entry for the model id:
`meta/<model>` as LiteLLM publishes Meta's API rates, or the bare model id (for
example a pricing override). Fuzzy matching is not used, because it would
price `muse-spark-1.3` at the `meta/muse-spark-1.3-contributor` entry. The
embedded pricing snapshot keeps LiteLLM's `meta/muse-*` entries, so `--offline`
reports (the Mac app and collector) price Muse too. Models without a published
price, such as `muse-wonder-full-1.0`, stay at zero with the usual
missing-pricing warning. Muse's own model catalog lists no cost.

## Testing

```text
cargo test -p turbotokens-adapter-muse
```

Fixture tests cover path discovery and skipped directories, the cache split,
reasoning, attribution and retained-frame records, logged and unlogged
reminder side-calls, quoted tool output, cross-file dedupe by record id and
usage id, subagent logs, microsecond timestamps across time zones, pricing,
and focused report shapes. The shared fixture lives in
`turbotokens-test-support` (`muse.rs`). `local_muse_data_matches_an_independent_parse`
checks the real store against an independent JSON walk when one exists and
returns early otherwise.

## Public surface

- `loader::load_entries`
- `report::summarize_entries`
- `run`

## Depends on

- `turbotokens-adapter-common`
- `turbotokens-core`
- `jiff`
- `serde`
- `serde_json`

## Build layer

Built in the `adapters` Crane artifact layer; the layer compiles all adapters in one Cargo invocation, so they build concurrently.
