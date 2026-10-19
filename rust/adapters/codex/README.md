# turbotokens-adapter-codex

The Codex adapter: it turns Codex session JSONL, including forked and replayed sessions
into the usage entries the reports render.

## Owns

- `aggregate.rs` — grouping events into report periods, including cross-session dedupe.
- `loader.rs` — reading the source, dedupe, and date filtering.
- `parser.rs` — raw record parsing, token mapping, and model naming.
- `paths.rs` — environment variables, default directories, and file discovery.
- `replay.rs` — detecting history that a forked or subagent session replays.
- `report.rs` — the JSON and table shapes where they differ from the shared ones.
- `speed.rs` — resolving the service tier that priced each request.
- `types.rs` — types that stay inside this adapter.

Anything that is not specific to this source belongs in `turbotokens-core` or
`turbotokens-adapter-common` instead.

## Data source

- `~/.codex/sessions/**/*.jsonl`

Record shapes, token mapping, and cost rules are documented in [`src/README.md`](src/README.md).

Reads plain files through `turbotokens-adapter-common`, which handles walking, size-balanced
chunking, and ordered parallel reads.

## Live file replay

`turbotokens codex live --json --offline` retains accounting checkpoints when
an atomic replacement or an observed file shrink rewinds the byte reader.
Retained token-count records do not spend tokens again or move the cumulative
baseline backwards. Model metadata is replayed so subsequent events retain
the correct price. A different `session_meta.id` starts a new logical counter.

Both cumulative and timestamped `last_token_usage` records are covered.
Headless completions without request IDs use occurrence counts during a
re-read: two identical retained completions remain two, and another identical
append still counts. Missing history with indistinguishable headless events
cannot establish which occurrences are new.

This does not detect every concurrent rewrite: an in-place truncate followed
by regrowth past the old offset between polls can still look like an append.
The startup snapshot/historical-event contract and daily-report format are
unchanged. See the [live replay harness](../../bench/live-replay.md) for
fixtures, measured behavior, and the separate Claude stream limitation.

## Public surface

- `aggregate::aggregate_events`
- `aggregate::filter_events_by_date`
- `aggregate::load_groups`
- `loader::load_codex_events`
- `loader::load_codex_events_from_directory`
- `report::calculate_codex_model_cost`
- `report::calculate_group_cost`
- `report::codex_model_missing_pricing`
- `report::non_cached_input_tokens`
- `speed::CodexSpeedPolicy`
- `speed::resolve_codex_speed`
- `types::CodexGroup`
- `types::CodexModelUsage`
- `types::CodexRawUsage`
- `types::CodexServiceTier`
- `types::CodexTokenUsageEvent`
- `types::CodexUsageBucket`
- `types::merge_codex_service_tiers`
- `run`
- `report_json`

## Depends on

- `turbotokens-adapter-common`
- `turbotokens-core`
- `compact_str`
- `jiff`
- `memchr`
- `rustc-hash`
- `serde`
- `serde_json`

## Build layer

Built in the `adapters` Crane artifact layer; the layer compiles all adapters in one Cargo invocation, so they build concurrently.
