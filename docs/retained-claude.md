# Retained Claude observations

`retained-claude` is an explicit, local-only Unix command for preserving Claude
response usage after the original JSONL files disappear. Ordinary reports and
their disposable caches are unchanged. This command does not upload anything.

Choose an account/publisher scope and an absolute state directory outside the
source roots. Use durable application storage, not a cache or temporary folder.
Create its parent first. The state directory itself must not exist at enrollment:

```sh
mkdir -p "$HOME/Library/Application Support/turbotokens"
turbotokens retained-claude \
  --store "$HOME/Library/Application Support/turbotokens/claude-ledger" \
  --scope 'my-account/my-publisher' \
  --root "$HOME/.claude" \
  --initialize
```

Run the same command without `--initialize` on subsequent scans. Declare every
configured Claude config root with a separate `--root`, including roots that are
temporarily absent. Root order does not matter. Root, scope, or pricing changes
fail explicitly. This initial implementation does not migrate those changes.
Source symlinks are rejected; declare their real directories explicitly.

The ledger contains only parsed usage metadata: response/request IDs, token
components, model/speed, original and revision timestamps, session attribution,
quoted cost, and advisor usage. It never stores prompts or message content.
Directory and files require owner-only access (0700 and 0600). Keep this directory
private and include it in local backups. Deleting it removes the retained
evidence; callers should also persist an enrollment witness outside it and never
automatically re-enroll after deletion or failure.

Every successful scan locks the store, reads all complete source records, merges
stable response identities, writes and syncs a temporary ledger, replaces the
previous ledger atomically, and syncs the directory before returning a receipt.
Absent responses stay retained. Returning copies and renamed files do not add
usage. Distinct responses with identical token counts remain distinct. A later
revision can replace an earlier one only with a whole observed component vector
that dominates it; the code never synthesizes component-wise maxima. Original
report date/session attribution survives later revisions and sidechain copies.

Malformed complete records, missing IDs on nonzero usage, unknown model pricing,
ambiguous sidechain identities, incomparable component corrections, and model or
speed corrections reject the scan. The last valid ledger remains intact.
Conflicting identity observations produce one replaceable, owner-private
`last-rejection.json` diagnostic, bounded to 128 KiB. Unterminated JSONL tails wait
for a newline and are counted in `pendingTailFiles`. Explicit enrollment first
commits an empty generation0 with the chosen scope, roots, prices, and coverage
start. If source reading then fails, fix the source problem and retry an ordinary
scan without `--initialize`; that known enrollment can continue to generation1.
No successful receipt is emitted until a source scan commits. If interruption
occurs before even generation0 is committed, preserve the directory for explicit
recovery; a missing ledger never silently becomes a new account.

Prices use `calculate` mode with the embedded offline table. All pricing inputs
and the pricing implementation are pinned byte-for-byte at enrollment. A later
scanner with different prices or implementation refuses the store rather than
repricing historical usage silently. Source `costUSD` remains evidence but does
not override this accounting policy. Price migrations require a separate,
explicitly reviewed revision; this command does not implement one.

The JSON receipt is `schema: turbotokens-retained-claude-v1`, `provider: claude`:

- `scope`, `generation`, `coverageStart`, `committedAt`, `pricingPolicy`, and
  `scannerVersion` identify the committed transaction and policy.
- `coverage` is `observed-claude-responses` and
  `historyBeforeEnrollmentComplete` is always false. Enrollment cannot recover
  observations deleted before the first scan or prove an old anonymous total.
- `retainedIdentityCount` and `currentIdentityCount` count parent response
  identities; advisor usage is included in their token totals.
- `observedFiles`, `pendingTailFiles`, and `missingRoots` are numeric counts.
  No source roots or response IDs are exposed by these fields.
- `currentTotals` describes the deduplicated currently present source records;
  `allTimeTotals` describes all retained observations. Neither is filtered.
- `totals`, `daily`, `monthly`, and `sessions` describe the same retained response
  set filtered by optional inclusive UTC `--since`/`--until` calendar dates.
  Report filters never remove observations from the store.

Use `--summary` for periodic shadow capture. It omits `daily`, `monthly`, and
`sessions` entirely and returns only receipt metadata and totals, without source
paths, session IDs, response IDs, or a large report payload.

Do not add these totals to an existing aggregate baseline without proven overlap
coverage. They cover Claude only, not Codex or other agents. A live tail captured
after a scan is not proof of the scan's exact coverage. Initial consumers should
keep this as a shadow backup until an explicit identity-aware migration and
live-coverage contract have been independently verified.

State errors fail closed: ordinary scans never initialize missing state, and
`--initialize` refuses any existing state directory. File locking is released by
the OS when a process exits or is killed. An orphaned temporary write is not a
committed ledger. Receipts must be accepted only after exit status zero, validated
for the expected scope/schema, and ordered by generation by any consumer.
