//! Opt-in, local-only Claude response accounting. Unlike the disposable parse
//! cache, absence from a later scan never removes an admitted observation.
//! Ordinary reports do not call this module.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use turbotokens_core::{
    BucketKind, LoadedEntry, PricingMap, Result, SessionAccumulator, TokenUsageRaw,
    cli::{CostMode, RetainedClaudeArgs, WeekDay},
    cli_error, format_rfc3339_millis, parse_ts_timestamp,
    pricing::retained_pricing_identity,
    session_summary_json, summarize_by_key, summarize_summaries_by_bucket, summary_json,
    totals_json, utc_now,
};

use super::{
    RawUsageEntry, UsageFileIdents, extract_project, finish_raw_usage_entry, scan_usage_line,
};

const SCHEMA: &str = "turbotokens-retained-claude-v1";
const POLICY: &str = "embedded-calculate-pinned-v1";
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Observation {
    raw: RawUsageEntry,
    attribution_timestamp: String,
    project: String,
    /// Source-provided session identity, or a per-response fallback. Never a filename.
    session: String,
    aliases: Vec<Option<String>>,
    prior: Vec<RawUsageEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Ledger {
    schema: String,
    scope: String,
    roots: Vec<PathBuf>,
    generation: u64,
    coverage_start: String,
    pricing_policy: String,
    /// Exact bytes, not a short checksum, determine compatibility.
    pricing_identity: Vec<String>,
    observations: Vec<Observation>,
}

#[derive(Default)]
struct Scan {
    observations: Vec<Observation>,
    files: usize,
    pending_tail_files: usize,
    missing_roots: usize,
}

pub fn run(args: RetainedClaudeArgs) -> Result<()> {
    let receipt = scan(&args)?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    serde_json::to_writer(&mut out, &receipt)?;
    out.write_all(b"\n")?;
    Ok(())
}

/// Returns a receipt only after the ledger and its parent directory are synced.
/// The OS releases the persistent lock-file lock even if the process is killed.
pub fn scan(args: &RetainedClaudeArgs) -> Result<Value> {
    let roots = validate_args(args)?;
    prepare_store(&args.store, args.initialize)?;
    let _lock = lock_store(&args.store)?;
    let ledger_path = args.store.join("ledger.json");
    let mut ledger = match fs::symlink_metadata(&ledger_path) {
        Ok(_) if args.initialize => {
            return Err(cli_error(
                "retained store already exists; --initialize never replaces it",
            ));
        }
        Ok(_) => {
            check_private(&ledger_path, false)?;
            let bytes = fs::read(&ledger_path)?;
            let ledger: Ledger = serde_json::from_slice(&bytes).map_err(|_| {
                cli_error("retained ledger is corrupt; preserve it and restore a verified backup")
            })?;
            validate_ledger(&ledger, args, &roots)?;
            ledger
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && args.initialize => Ledger {
            schema: SCHEMA.to_string(),
            scope: args.scope.clone(),
            roots: roots.clone(),
            generation: 0,
            coverage_start: format_rfc3339_millis(utc_now()),
            pricing_policy: POLICY.to_string(),
            pricing_identity: retained_pricing_identity().map(str::to_string).to_vec(),
            observations: Vec::new(),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(cli_error(
                "retained ledger is missing; ordinary scans never recreate it",
            ));
        }
        Err(error) => return Err(error.into()),
    };

    if args.initialize {
        // Persist the explicit enrollment before touching sources. A transient
        // initial read/parse failure can then retry normally without treating
        // a half-created directory as a new account. No generation0 receipt is
        // ever emitted; source capture must still commit successfully below.
        atomic_commit(&args.store, &ledger)?;
    }

    // Scan while holding the same lock as merge+commit. No scanner can overwrite
    // observations admitted by a concurrent process with a stale snapshot.
    let current = read_roots(&roots)?;
    let mut source = Vec::new();
    merge_observations(&mut source, current.observations, Some(&args.store))?;
    let current_count = source.len();
    let pricing = PricingMap::load_embedded();
    let current_entries = loaded_entries(&source, &pricing)?;
    let current_report = report(&current_entries, None, None, false)?;
    let prior_entries = loaded_entries(&ledger.observations, &pricing)?;
    let prior_total = total(&prior_entries)?;
    merge_observations(&mut ledger.observations, source, Some(&args.store))?;
    let retained_entries = loaded_entries(&ledger.observations, &pricing)?;
    let retained_total = total(&retained_entries)?;
    if retained_total.0 < prior_total.0 || retained_total.1 + 1e-9 < prior_total.1 {
        return Err(cli_error(
            "retained accounting correction would decrease totals; no transaction committed",
        ));
    }
    let all_time_report = report(&retained_entries, None, None, false)?;
    let filtered_report = report(
        &retained_entries,
        args.since.as_deref(),
        args.until.as_deref(),
        !args.summary,
    )?;
    ledger.generation = ledger
        .generation
        .checked_add(1)
        .filter(|n| *n <= MAX_SAFE_INTEGER)
        .ok_or_else(|| cli_error("retained generation overflow"))?;
    validate_ledger(&ledger, args, &roots)?;
    atomic_commit(&args.store, &ledger)?;

    let mut receipt = json!({
        "schema": SCHEMA,
        "provider": "claude",
        "scope": ledger.scope,
        "generation": ledger.generation,
        "coverageStart": ledger.coverage_start,
        "coverage": "observed-claude-responses",
        "historyBeforeEnrollmentComplete": false,
        "pricingPolicy": POLICY,
        "scannerVersion": env!("CARGO_PKG_VERSION"),
        "committedAt": format_rfc3339_millis(utc_now()),
        "retainedIdentityCount": ledger.observations.len(),
        "currentIdentityCount": current_count,
        "observedFiles": current.files,
        "pendingTailFiles": current.pending_tail_files,
        "missingRoots": current.missing_roots,
        "currentTotals": current_report["totals"],
        "allTimeTotals": all_time_report["totals"],
        "totals": filtered_report["totals"],
        "filter": {"since": args.since, "until": args.until},
    });
    if !args.summary {
        for key in ["daily", "monthly", "sessions"] {
            receipt[key] = filtered_report[key].clone();
        }
    }
    Ok(receipt)
}

fn validate_args(args: &RetainedClaudeArgs) -> Result<Vec<PathBuf>> {
    validate_absolute(&args.store)?;
    if args.scope.is_empty() || args.scope.len() > 200 || args.scope.chars().any(char::is_control) {
        return Err(cli_error(
            "--scope must be a nonempty identifier of at most 200 characters",
        ));
    }
    if args.roots.is_empty() {
        return Err(cli_error("at least one explicit --root is required"));
    }
    let mut roots = BTreeSet::new();
    for root in &args.roots {
        validate_absolute(root)?;
        if root == &args.store || args.store.starts_with(root) || root.starts_with(&args.store) {
            return Err(cli_error("retained store must be outside all source roots"));
        }
        roots.insert(root.clone());
    }
    for bound in [&args.since, &args.until].into_iter().flatten() {
        if bound.len() != 8 || !bound.bytes().all(|b| b.is_ascii_digit()) {
            return Err(cli_error("date bounds must use YYYYMMDD or YYYY-MM-DD"));
        }
        let formatted = format!("{}-{}-{}", &bound[..4], &bound[4..6], &bound[6..]);
        if formatted.parse::<jiff::civil::Date>().is_err() {
            return Err(cli_error("invalid calendar date bound"));
        }
    }
    if args
        .since
        .as_ref()
        .zip(args.until.as_ref())
        .is_some_and(|(a, b)| a > b)
    {
        return Err(cli_error("--since must not follow --until"));
    }
    Ok(roots.into_iter().collect())
}

fn validate_absolute(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(cli_error(
            "retained paths must be absolute without . or .. components",
        ));
    }
    Ok(())
}

fn validate_ledger(ledger: &Ledger, args: &RetainedClaudeArgs, roots: &[PathBuf]) -> Result<()> {
    if ledger.schema != SCHEMA || ledger.scope != args.scope || ledger.roots != roots {
        return Err(cli_error(
            "retained schema, account scope, or configured roots changed; no automatic migration",
        ));
    }
    if ledger.pricing_policy != POLICY || ledger.pricing_identity != retained_pricing_identity() {
        return Err(cli_error(
            "retained pricing implementation changed; explicit reviewed migration is required",
        ));
    }
    if ledger.generation > MAX_SAFE_INTEGER || parse_ts_timestamp(&ledger.coverage_start).is_none()
    {
        return Err(cli_error(
            "invalid retained generation or coverage timestamp",
        ));
    }
    if ledger.generation == 0 && !ledger.observations.is_empty() {
        return Err(cli_error(
            "unenrolled generation0 cannot contain observations",
        ));
    }
    for observation in &ledger.observations {
        validate_observation(observation)?;
    }
    // Valid JSON alone is not sufficient: duplicate persisted identities could
    // inflate output after manual edits or corruption.
    let mut canonical = Vec::new();
    merge_observations(&mut canonical, ledger.observations.clone(), None)?;
    if canonical.len() != ledger.observations.len() {
        return Err(cli_error("retained ledger contains duplicate identities"));
    }
    Ok(())
}

fn read_roots(roots: &[PathBuf]) -> Result<Scan> {
    let mut scan = Scan::default();
    let mut paths = Vec::new();
    for root in roots {
        match fs::symlink_metadata(root) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                scan.missing_roots += 1;
                continue;
            }
            Err(e) => return Err(e.into()),
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Err(cli_error(
                    "Claude root must be a real directory, not a symlink",
                ));
            }
            Ok(_) => {}
        }
        let projects = root.join("projects");
        match fs::symlink_metadata(&projects) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                scan.missing_roots += 1;
                continue;
            }
            Err(e) => return Err(e.into()),
            Ok(_) => collect_files(&projects, &mut paths)?,
        }
    }
    paths.sort();
    paths.dedup();
    let marker = memchr::memmem::Finder::new(br#""usage""#);
    for path in paths {
        scan.files += 1;
        let file = File::open(&path)?;
        let mut reader = BufReader::new(file);
        let mut line = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            if line.last() != Some(&b'\n') {
                scan.pending_tail_files += 1;
                break;
            }
            if line.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let value: Value = serde_json::from_slice(&line).map_err(|_| {
                cli_error("malformed complete Claude JSONL line; retained scan rejected")
            })?;
            if value.pointer("/message/usage").is_none() {
                continue;
            }
            let mut raws = Vec::new();
            scan_usage_line(&line, &marker, &mut None, &mut raws);
            if raws.len() != 1 {
                return Err(cli_error(
                    "unsupported Claude usage row; retained scan rejected",
                ));
            }
            let raw = raws.pop().expect("one parsed observation");
            let advisor_count = value
                .pointer("/message/usage/iterations")
                .and_then(Value::as_array)
                .map_or(0, |rows| {
                    rows.iter()
                        .filter(|row| {
                            row.get("type").and_then(Value::as_str) == Some("advisor_message")
                        })
                        .count()
                });
            if raw.advisors.len() != advisor_count {
                return Err(cli_error(
                    "unsupported advisor usage; retained scan rejected",
                ));
            }
            validate_usage(raw.usage)?;
            for (_, usage) in &raw.advisors {
                validate_usage(*usage)?;
            }
            if components(raw.usage).into_iter().all(|v| v == 0)
                && raw
                    .advisors
                    .iter()
                    .all(|(_, usage)| components(*usage).into_iter().all(|v| v == 0))
            {
                continue;
            }
            let session = raw.session_id.clone().unwrap_or_else(|| {
                format!("response:{}", raw.message_id.as_deref().unwrap_or_default())
            });
            let observation = Observation {
                aliases: vec![raw.request_id.clone()],
                attribution_timestamp: raw.timestamp.clone(),
                raw,
                project: extract_project(&path),
                session,
                prior: Vec::new(),
            };
            validate_observation(&observation)?;
            scan.observations.push(observation);
        }
    }
    Ok(scan)
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let meta = fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(cli_error("source tree contains a non-directory or symlink"));
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            return Err(cli_error(
                "source tree contains a symlink; declare real roots explicitly",
            ));
        }
        if kind.is_dir() {
            collect_files(&entry.path(), out)?;
        } else if kind.is_file() && entry.path().extension().is_some_and(|v| v == "jsonl") {
            out.push(entry.path());
        }
    }
    Ok(())
}

fn validate_observation(observation: &Observation) -> Result<()> {
    let raw = &observation.raw;
    if raw.message_id.as_deref().is_none_or(str::is_empty)
        || raw
            .model
            .as_deref()
            .is_none_or(|m| m.is_empty() || m == "<synthetic>")
    {
        return Err(cli_error(
            "retained Claude usage requires a stable message ID and known model",
        ));
    }
    if parse_ts_timestamp(&raw.timestamp).is_none_or(|t| t.as_millis() != raw.timestamp_ms)
        || parse_ts_timestamp(&observation.attribution_timestamp).is_none()
        || observation.session.is_empty()
        || !observation.aliases.contains(&raw.request_id)
    {
        return Err(cli_error("invalid retained observation metadata"));
    }
    validate_usage(raw.usage)?;
    for (_, usage) in &raw.advisors {
        validate_usage(*usage)?;
    }
    for prior in &observation.prior {
        if prior.message_id != raw.message_id
            || prior.model != raw.model
            || normalized_speed(prior.usage) != normalized_speed(raw.usage)
            || !observation.aliases.contains(&prior.request_id)
            || parse_ts_timestamp(&prior.timestamp)
                .is_none_or(|t| t.as_millis() != prior.timestamp_ms)
            || !dominates(raw, prior)
        {
            return Err(cli_error("retained prior identity mismatch"));
        }
        validate_usage(prior.usage)?;
        for (_, usage) in &prior.advisors {
            validate_usage(*usage)?;
        }
    }
    Ok(())
}

fn components(usage: TokenUsageRaw) -> [u64; 4] {
    [
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_read_input_tokens,
        usage.cache_creation_token_count(),
    ]
}

fn validate_usage(usage: TokenUsageRaw) -> Result<()> {
    if let Some(cache) = usage.cache_creation {
        cache
            .ephemeral_5m_input_tokens
            .checked_add(cache.ephemeral_1h_input_tokens)
            .filter(|v| *v <= MAX_SAFE_INTEGER)
            .ok_or_else(|| cli_error("cache usage overflow"))?;
    }
    components(usage)
        .into_iter()
        .try_fold(0u64, |sum, count| sum.checked_add(count))
        .filter(|v| *v <= MAX_SAFE_INTEGER)
        .ok_or_else(|| cli_error("usage is outside exact JSON integer range"))?;
    Ok(())
}

fn dominates(a: &RawUsageEntry, b: &RawUsageEntry) -> bool {
    coverage_components(a.usage)
        .into_iter()
        .zip(coverage_components(b.usage))
        .all(|(a, b)| a >= b)
        && a.advisors.len() >= b.advisors.len()
        && b.advisors
            .iter()
            .zip(&a.advisors)
            .all(|((bm, bu), (am, au))| {
                bm == am
                    && normalized_speed(*au) == normalized_speed(*bu)
                    && coverage_components(*au)
                        .into_iter()
                        .zip(coverage_components(*bu))
                        .all(|(a, b)| a >= b)
            })
}

fn coverage_components(usage: TokenUsageRaw) -> [u64; 5] {
    let (five_minute, one_hour) =
        usage
            .cache_creation
            .map_or((usage.cache_creation_input_tokens, 0), |cache| {
                (
                    cache.ephemeral_5m_input_tokens,
                    cache.ephemeral_1h_input_tokens,
                )
            });
    [
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_read_input_tokens,
        five_minute,
        one_hour,
    ]
}

fn normalized_speed(usage: TokenUsageRaw) -> turbotokens_core::Speed {
    usage.speed.unwrap_or(turbotokens_core::Speed::Standard)
}

fn only_sidechain(observation: &Observation) -> bool {
    observation.raw.is_sidechain == Some(true)
        && observation
            .prior
            .iter()
            .all(|raw| raw.is_sidechain == Some(true))
}

fn merge_observations(
    retained: &mut Vec<Observation>,
    incoming: Vec<Observation>,
    rejection_store: Option<&Path>,
) -> Result<()> {
    let mut by_message: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, observation) in retained.iter().enumerate() {
        by_message
            .entry(
                observation
                    .raw
                    .message_id
                    .clone()
                    .ok_or_else(|| cli_error("missing retained identity"))?,
            )
            .or_default()
            .push(index);
    }
    for candidate in incoming {
        validate_observation(&candidate)?;
        let message = candidate
            .raw
            .message_id
            .clone()
            .expect("validated identity");
        let indexes = by_message.entry(message).or_default();
        let exact: Vec<_> = indexes
            .iter()
            .copied()
            .filter(|i| {
                retained[*i]
                    .aliases
                    .iter()
                    .any(|alias| candidate.aliases.contains(alias))
            })
            .collect();
        let matches: Vec<_> = if exact.is_empty() {
            if candidate.raw.is_sidechain != Some(true)
                && indexes.iter().any(|i| {
                    !only_sidechain(&retained[*i])
                        && (candidate.aliases.contains(&None)
                            || retained[*i].aliases.contains(&None))
                })
            {
                return Err(reject_merge(
                    rejection_store,
                    "ambiguous missing request identity; no retained transaction committed",
                    &candidate,
                    indexes.iter().take(2).map(|i| &retained[*i]).collect(),
                ));
            }
            indexes
                .iter()
                .copied()
                .filter(|i| {
                    candidate.raw.is_sidechain == Some(true) || only_sidechain(&retained[*i])
                })
                .collect()
        } else {
            exact
        };
        if matches.len() > 1 {
            return Err(reject_merge(
                rejection_store,
                "ambiguous Claude sidechain identity; no retained transaction committed",
                &candidate,
                matches.iter().take(2).map(|i| &retained[*i]).collect(),
            ));
        }
        let Some(index) = matches.first().copied() else {
            indexes.push(retained.len());
            retained.push(candidate);
            continue;
        };
        let prior = &mut retained[index];
        if prior.raw.model != candidate.raw.model
            || normalized_speed(prior.raw.usage) != normalized_speed(candidate.raw.usage)
        {
            return Err(reject_merge(
                rejection_store,
                "Claude identity changed model or speed; explicit accounting correction required",
                &candidate,
                vec![prior],
            ));
        }
        if candidate.raw.is_sidechain == Some(true)
            && !only_sidechain(prior)
            && !dominates(&prior.raw, &candidate.raw)
        {
            return Err(reject_merge(
                rejection_store,
                "sidechain usage exceeds its retained parent; explicit accounting correction required",
                &candidate,
                vec![prior],
            ));
        }
        if prior.raw.is_sidechain == Some(true)
            && candidate.raw.is_sidechain != Some(true)
            && !dominates(&candidate.raw, &prior.raw)
        {
            return Err(reject_merge(
                rejection_store,
                "parent observation would lower retained sidechain usage; explicit accounting correction required",
                &candidate,
                vec![prior],
            ));
        }
        // Sidechain copies may have a different session and timestamp. Their
        // original attribution is retained; those fields are never identity.
        if !dominates(&candidate.raw, &prior.raw) && !dominates(&prior.raw, &candidate.raw) {
            return Err(reject_merge(
                rejection_store,
                "incomparable Claude usage correction; no retained transaction committed",
                &candidate,
                vec![prior],
            ));
        }
        for alias in candidate.aliases {
            if !prior.aliases.contains(&alias) {
                prior.aliases.push(alias);
            }
        }
        if dominates(&candidate.raw, &prior.raw)
            && candidate.raw != prior.raw
            && !(candidate.raw.is_sidechain == Some(true) && !only_sidechain(prior))
        {
            // Select a whole actual observation; never synthesize maxima from
            // separate input/output/cache vectors.
            if !prior.prior.contains(&prior.raw) {
                prior.prior.push(prior.raw.clone());
            }
            prior.raw = candidate.raw;
        } else if candidate.raw != prior.raw && !prior.prior.contains(&candidate.raw) {
            prior.prior.push(candidate.raw);
        }
        for raw in candidate.prior {
            if raw != prior.raw && !prior.prior.contains(&raw) {
                prior.prior.push(raw);
            }
        }
    }
    Ok(())
}

fn reject_merge(
    store: Option<&Path>,
    reason: &str,
    candidate: &Observation,
    previous: Vec<&Observation>,
) -> turbotokens_core::CliError {
    if let Some(store) = store {
        // One replaceable diagnostic, bounded to 128 KiB, containing only typed
        // usage metadata. Never copy JSONL text, prompts, or message content.
        let detail = json!({"schema": SCHEMA, "rejectedAt": format_rfc3339_millis(utc_now()), "reason": reason,
            "candidate": candidate.raw, "previous": previous.iter().map(|o| &o.raw).collect::<Vec<_>>()});
        if let Ok(bytes) = serde_json::to_vec(&detail) {
            let bytes = if bytes.len() <= 131_072 {
                bytes
            } else {
                serde_json::to_vec(&json!({"schema": SCHEMA, "reason": reason, "detailOmitted": "usage metadata exceeds diagnostic size limit"})).unwrap_or_default()
            };
            if atomic_write(store, "last-rejection.json", &bytes).is_err() {
                return cli_error(format!(
                    "{reason}; could not persist private rejection diagnostic"
                ));
            }
        }
    }
    cli_error(reason)
}

fn loaded_entries(observations: &[Observation], pricing: &PricingMap) -> Result<Vec<LoadedEntry>> {
    let mut entries = Vec::new();
    for observation in observations {
        let idents = UsageFileIdents {
            project: Arc::from(observation.project.as_str()),
            session_id: Arc::from(observation.session.as_str()),
            project_path: Arc::from(observation.project.as_str()),
        };
        let mut report_raw = observation.raw.clone();
        report_raw.timestamp = observation.attribution_timestamp.clone();
        report_raw.timestamp_ms = parse_ts_timestamp(&report_raw.timestamp)
            .expect("validated attribution timestamp")
            .as_millis();
        finish_raw_usage_entry(
            &report_raw,
            &idents,
            Some(&jiff::tz::TimeZone::UTC),
            CostMode::Calculate,
            Some(pricing),
            &mut entries,
        );
    }
    if entries.iter().any(|entry| {
        entry.missing_pricing_model.is_some() || !entry.cost.is_finite() || entry.cost < 0.0
    }) {
        return Err(cli_error(
            "retained usage has unavailable or invalid pinned pricing",
        ));
    }
    total(&entries)?;
    Ok(entries)
}

fn total(entries: &[LoadedEntry]) -> Result<(u64, f64)> {
    let count = entries
        .iter()
        .flat_map(|entry| components(entry.data.message.usage))
        .try_fold(0u64, |sum, v| sum.checked_add(v))
        .filter(|v| *v <= MAX_SAFE_INTEGER)
        .ok_or_else(|| cli_error("retained total exceeds exact JSON integer range"))?;
    let cost: f64 = entries.iter().map(|entry| entry.cost).sum();
    if !cost.is_finite() {
        return Err(cli_error("retained cost overflow"));
    }
    Ok((count, cost))
}

fn report(
    entries: &[LoadedEntry],
    since: Option<&str>,
    until: Option<&str>,
    sections: bool,
) -> Result<Value> {
    let filtered: Vec<_> = entries
        .iter()
        .filter(|entry| {
            let date = entry.date.replace('-', "");
            since.is_none_or(|since| date.as_str() >= since)
                && until.is_none_or(|until| date.as_str() <= until)
        })
        .cloned()
        .collect();
    let daily = summarize_by_key(
        &filtered,
        |entry| entry.date.clone(),
        |key| (key.to_string(), None),
    )?;
    if !sections {
        return Ok(json!({"totals": totals_json(&daily)}));
    }
    let monthly = summarize_summaries_by_bucket(&daily, BucketKind::Monthly, WeekDay::Monday);
    let mut sessions = BTreeMap::<String, SessionAccumulator>::new();
    for entry in &filtered {
        sessions
            .entry(entry.session_id.to_string())
            .or_default()
            .add_entry(entry);
    }
    let sessions = sessions
        .into_values()
        .map(SessionAccumulator::into_summary)
        .collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"totals": totals_json(&daily), "daily": daily.iter().map(summary_json).collect::<Vec<_>>(), "monthly": monthly.iter().map(summary_json).collect::<Vec<_>>(), "sessions": sessions.iter().map(session_summary_json).collect::<Vec<_>>() }),
    )
}

#[cfg(unix)]
fn check_private(path: &Path, directory: bool) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink()
        || meta.is_dir() != directory
        || (!directory && !meta.is_file())
        || meta.uid() != unsafe { geteuid() }
        || meta.mode() & 0o077 != 0
    {
        return Err(cli_error(
            "retained state must be owner-private, owned by this user, and not a symlink",
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_private(_: &Path, _: bool) -> Result<()> {
    Err(cli_error(
        "retained Claude accounting currently requires Unix owner permissions",
    ))
}

fn prepare_store(path: &Path, initialize: bool) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) if initialize => Err(cli_error(
            "--initialize requires an absent store directory; existing state is never replaced",
        )),
        Ok(_) => check_private(path, true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && initialize => {
            let mut builder = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path)?;
            check_private(path, true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(cli_error(
            "retained store is missing; explicit first enrollment requires --initialize",
        )),
        Err(error) => Err(error.into()),
    }
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

fn lock_store(store: &Path) -> Result<File> {
    let path = store.join("ledger.lock");
    let file = match private_options().create_new(true).open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            check_private(&path, false)?;
            private_options().open(&path)?
        }
        Err(e) => return Err(e.into()),
    };
    check_private(&path, false)?;
    file.lock()?;
    Ok(file)
}

fn atomic_commit(store: &Path, ledger: &Ledger) -> Result<()> {
    let bytes = serde_json::to_vec(ledger)?;
    let _: Ledger = serde_json::from_slice(&bytes)?;
    atomic_write(store, "ledger.json", &bytes)
}

fn atomic_write(store: &Path, destination: &str, bytes: &[u8]) -> Result<()> {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| cli_error("system clock precedes epoch"))?
        .as_nanos();
    let temporary = store.join(format!("ledger.{}.{}.tmp", std::process::id(), nonce));
    let result = (|| -> Result<()> {
        let mut file = private_options().create_new(true).open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, store.join(destination))?;
        File::open(store)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests;
