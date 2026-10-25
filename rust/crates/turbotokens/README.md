# turbotokens

The distributed binary. It stays thin on purpose: argument parsing lives in
`turbotokens-cli-parser`, shared runtime behavior in `turbotokens-core`, and every data
source in its own `turbotokens-adapter-*` crate.

## Owns

- `main.rs` — startup, command dispatch, and the version string the release
  embeds through `TURBOTOKENS_VERSION`.
- `cli.rs` — the parse entry point and the config context it passes to the parser.
- `commands/` — the command implementations that are not an agent report, such as
  `blocks` and `statusline`.
- `adapter/` — the thin aliases that map each `turbotokens <agent>` subcommand to its
  adapter crate.
- `bin/generate_config_schema.rs` — writes `apps/turbotokens/config-schema.json`; the
  `config-schema` flake check fails when the committed file drifts.

## Depends on

- `turbotokens-adapter-all`
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
- `turbotokens-cli`
- `turbotokens-cli-parser`
- `turbotokens-config`
- `turbotokens-core`
- `serde`
- `serde_json`

## Build layer

Outside the Crane artifact layers: it is compiled with the final binary, so editing it leaves the cached layers untouched. Its link step is the dominant cost of a warm build, because the release
profile uses fat LTO with a single codegen unit.
