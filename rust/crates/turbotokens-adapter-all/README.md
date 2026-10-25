# turbotokens-adapter-all

The unified `turbotokens` report: it loads every agent adapter and merges their rows
into one table or JSON document.

## Owns

- `loader.rs` — per-agent loading, the agent selection the CLI asks for, and the
  merge into shared rows.
- `report.rs` — the unified row and total shapes.
- `types.rs` — the accumulators the merge needs.

This is the only crate that depends on every adapter, which keeps the adapters
themselves independent of each other.

## Public surface

- `run`

## Depends on

- `turbotokens-adapter-amp`
- `turbotokens-adapter-antigravity`
- `turbotokens-adapter-claude`
- `turbotokens-adapter-codebuff`
- `turbotokens-adapter-codex`
- `turbotokens-adapter-common`
- `turbotokens-adapter-copilot`
- `turbotokens-adapter-droid`
- `turbotokens-adapter-gemini`
- `turbotokens-adapter-goose`
- `turbotokens-adapter-grok`
- `turbotokens-adapter-hermes`
- `turbotokens-adapter-kilo`
- `turbotokens-adapter-kimi`
- `turbotokens-adapter-muse`
- `turbotokens-adapter-openclaw`
- `turbotokens-adapter-opencode`
- `turbotokens-adapter-pi`
- `turbotokens-adapter-qwen`
- `turbotokens-adapter-zcode`
- `turbotokens-cli`
- `turbotokens-core`
- `serde`
- `serde_json`

## Build layer

Built in the `adapters` Crane artifact layer; the layer compiles all adapters in one Cargo invocation, so they build concurrently. Because it depends on every adapter, a change to any adapter also
recompiles this crate.
