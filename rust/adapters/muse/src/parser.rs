use std::{fs, path::Path, sync::Arc};

use jiff::tz::TimeZone as JiffTimeZone;
use serde::{Deserialize, de::IgnoredAny};

use crate::{
    LoadedEntry, PricingMap, Result, TimestampMs, TokenUsageRaw, UsageEntry, UsageMessage,
    calculate_cost_from_pricing, cli::CostMode, fast::LinePrefilter, format_date_tz,
    format_rfc3339_millis, model_aliases::resolve_model_name, total_usage_tokens,
};
use turbotokens_adapter_common::jsonl;

/// The only payload type that carries per-request usage.
const RUNTIME_SESSION_PAYLOAD: &str = "runtime.session";
/// Session metadata: workspace root and Muse build.
const METADATA_PAYLOAD: &str = "runtime.session.metadata";
/// One completed LLM step. Everything else that repeats usage, such as
/// `goal_usage_attribution`, is ignored.
const MODEL_COMPLETED_EVENT: &str = "model_completed";
const UNKNOWN_MODEL: &str = "unknown";
const DEFAULT_PROJECT_PATH: &str = "Muse";
const PROVIDER_PREFIX: &str = "meta";

/// One `session.jsonl` line. Only the fields turbotokens reads are declared.
#[derive(Debug, Deserialize)]
struct MuseLine {
    /// Present on permission/bootstrap frames that wrap child records as JSON
    /// strings; those frames never carry usage and are skipped whole.
    #[serde(default)]
    retained_frame: Option<IgnoredAny>,
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    id: Option<String>,
    #[serde(default, deserialize_with = "jsonl::lenient_object")]
    stream: Option<MuseStream>,
    #[serde(default, deserialize_with = "jsonl::lenient_i64")]
    sequence: Option<i64>,
    /// Integer microseconds since the Unix epoch, UTC.
    #[serde(default, deserialize_with = "jsonl::lenient_i64")]
    recorded_at: Option<i64>,
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    payload_type: Option<String>,
    #[serde(default, deserialize_with = "jsonl::lenient_object")]
    payload: Option<MusePayload>,
}

#[derive(Debug, Deserialize)]
struct MuseStream {
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MusePayload {
    #[serde(default, deserialize_with = "jsonl::lenient_object")]
    event: Option<MuseEvent>,
    #[serde(default, deserialize_with = "jsonl::lenient_object")]
    record: Option<MuseMetadataRecord>,
}

#[derive(Debug, Deserialize)]
struct MuseEvent {
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    model: Option<String>,
    #[serde(default, deserialize_with = "jsonl::lenient_object")]
    usage: Option<MuseUsage>,
}

/// `payload.event.usage` on a `model_completed` record.
///
/// `input_tokens` is the whole prompt including its cached part,
/// `cache_read_tokens` (mirrored by the older `cached_tokens`) repeats that
/// cached part, and `reasoning_tokens` is already inside `output_tokens`.
#[derive(Debug, Deserialize)]
struct MuseUsage {
    #[serde(default, deserialize_with = "jsonl::lenient_u64")]
    input_tokens: u64,
    #[serde(default, deserialize_with = "jsonl::lenient_u64")]
    output_tokens: u64,
    #[serde(default, deserialize_with = "jsonl::lenient_i64")]
    cache_read_tokens: Option<i64>,
    #[serde(default, deserialize_with = "jsonl::lenient_u64")]
    cached_tokens: u64,
    #[serde(default, deserialize_with = "jsonl::lenient_u64")]
    cache_write_tokens: u64,
}

#[derive(Debug, Deserialize)]
struct MuseMetadataRecord {
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    workspace_root: Option<String>,
    #[serde(default, deserialize_with = "jsonl::lenient_object")]
    build: Option<MuseBuild>,
}

#[derive(Debug, Deserialize)]
struct MuseBuild {
    #[serde(default, deserialize_with = "jsonl::non_empty_string")]
    semver: Option<String>,
}

/// Token counts mapped onto turbotokens' buckets. Their sum is always
/// `input_tokens + output_tokens + cache_write_tokens` from the record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct MuseTokens {
    pub(super) input: u64,
    pub(super) output: u64,
    pub(super) cache_creation: u64,
    pub(super) cache_read: u64,
}

impl MuseTokens {
    fn total(self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_creation)
            .saturating_add(self.cache_read)
    }

    fn usage(self) -> TokenUsageRaw {
        TokenUsageRaw {
            input_tokens: self.input,
            output_tokens: self.output,
            cache_creation_input_tokens: self.cache_creation,
            cache_read_input_tokens: self.cache_read,
            speed: None,
            cache_creation: None,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct MuseUsageEntry {
    /// The record's top-level `id`, the dedupe key across files.
    pub(super) record_id: String,
    pub(super) timestamp: TimestampMs,
    pub(super) session_id: String,
    pub(super) model: String,
    pub(super) tokens: MuseTokens,
}

#[derive(Debug, Default)]
pub(super) struct MuseSessionFile {
    pub(super) entries: Vec<MuseUsageEntry>,
    pub(super) project_path: Option<String>,
    pub(super) version: Option<String>,
}

pub(super) fn read_session_file(path: &Path) -> Result<MuseSessionFile> {
    let content = fs::read(path)?;
    Ok(parse_session_content(&content, path))
}

fn parse_session_content(content: &[u8], path: &Path) -> MuseSessionFile {
    // Every usage line contains the quoted event kind and every metadata line
    // its payload type. Tool results that merely mention "model_completed"
    // carry it escaped inside a JSON string, and anything that still slips
    // through fails the structural check below.
    let prefilter =
        LinePrefilter::any(&[br#""model_completed""#, br#""runtime.session.metadata""#]);
    let fallback_session_id = session_id_from_path(path);
    let mut fallback_timestamp = None;
    let mut file = MuseSessionFile::default();
    for line in jsonl::records::<MuseLine>(content, Some(&prefilter)) {
        if line.retained_frame.is_some() {
            continue;
        }
        match line.payload_type.as_deref() {
            Some(RUNTIME_SESSION_PAYLOAD) => {
                if let Some(entry) =
                    usage_entry(line, &fallback_session_id, path, &mut fallback_timestamp)
                {
                    file.entries.push(entry);
                }
            }
            Some(METADATA_PAYLOAD) => {
                let Some(record) = line.payload.and_then(|payload| payload.record) else {
                    continue;
                };
                if file.project_path.is_none() {
                    file.project_path = record.workspace_root;
                }
                if file.version.is_none() {
                    file.version = record.build.and_then(|build| build.semver);
                }
            }
            _ => {}
        }
    }
    file
}

fn usage_entry(
    line: MuseLine,
    fallback_session_id: &str,
    path: &Path,
    fallback_timestamp: &mut Option<TimestampMs>,
) -> Option<MuseUsageEntry> {
    let event = line.payload?.event?;
    if event.kind.as_deref() != Some(MODEL_COMPLETED_EVENT) {
        return None;
    }
    let tokens = map_tokens(&event.usage?);
    if tokens.total() == 0 {
        return None;
    }
    let session_id = line
        .stream
        .and_then(|stream| stream.id)
        .unwrap_or_else(|| fallback_session_id.to_string());
    let model = event.model.unwrap_or_else(|| UNKNOWN_MODEL.to_string());
    let timestamp = match line.recorded_at.map(timestamp_from_micros) {
        Some(timestamp) => timestamp,
        None => *fallback_timestamp.get_or_insert_with(|| file_modified_timestamp(path)),
    };
    let record_id = line.id.unwrap_or_else(|| {
        // Every observed record has an id; this keeps a hypothetical id-less
        // record stable across reads without colliding with real ids.
        format!(
            "noid:{session_id}:{}:{}:{model}:{}:{}:{}:{}",
            line.sequence.unwrap_or_default(),
            line.recorded_at.unwrap_or_default(),
            tokens.input,
            tokens.output,
            tokens.cache_creation,
            tokens.cache_read,
        )
    });
    Some(MuseUsageEntry {
        record_id,
        timestamp,
        session_id,
        model,
        tokens,
    })
}

/// Maps one `model_completed` usage object onto turbotokens' buckets.
///
/// Fresh input is `input_tokens` minus the cache read, cache read is
/// `cache_read_tokens` (or `cached_tokens` when only that is present, never
/// both), cache creation is `cache_write_tokens`, and output is
/// `output_tokens`, which already includes reasoning. The cache read is
/// bounded by `input_tokens`, so the record's total stays
/// `input_tokens + output_tokens + cache_write_tokens`.
fn map_tokens(usage: &MuseUsage) -> MuseTokens {
    let cache_read = usage
        .cache_read_tokens
        .and_then(|value| u64::try_from(value).ok())
        .unwrap_or(usage.cached_tokens)
        .min(usage.input_tokens);
    MuseTokens {
        input: usage.input_tokens - cache_read,
        output: usage.output_tokens,
        cache_creation: usage.cache_write_tokens,
        cache_read,
    }
}

fn timestamp_from_micros(micros: i64) -> TimestampMs {
    TimestampMs::from_millis(micros.div_euclid(1_000))
}

fn file_modified_timestamp(path: &Path) -> TimestampMs {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .map(TimestampMs::from_millis)
        .unwrap_or(TimestampMs::UNIX_EPOCH)
}

/// The session directory name, used only when a record has no `stream.id`.
fn session_id_from_path(path: &Path) -> String {
    path.parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(UNKNOWN_MODEL)
        .to_string()
}

pub(super) fn muse_entry_to_loaded(
    entry: MuseUsageEntry,
    project_path: Option<&str>,
    version: Option<&str>,
    tz: Option<&JiffTimeZone>,
    mode: CostMode,
    pricing: &PricingMap,
) -> LoadedEntry {
    let usage = entry.tokens.usage();
    let (cost, missing_pricing_model) = muse_cost(&entry.model, usage, mode, pricing);
    let data = UsageEntry {
        session_id: Some(entry.session_id.clone()),
        timestamp: format_rfc3339_millis(entry.timestamp),
        version: version.map(str::to_string),
        message: UsageMessage {
            usage,
            model: Some(entry.model.clone()),
            id: Some(entry.record_id),
        },
        cost_usd: None,
        request_id: None,
        is_api_error_message: None,
        is_sidechain: None,
    };
    LoadedEntry {
        date: format_date_tz(entry.timestamp, tz),
        timestamp: entry.timestamp,
        project: Arc::from("muse"),
        session_id: Arc::from(entry.session_id),
        project_path: Arc::from(project_path.unwrap_or(DEFAULT_PROJECT_PATH)),
        cost,
        extra_total_tokens: 0,
        credits: None,
        message_count: None,
        model: Some(entry.model),
        usage_limit_reset_time: None,
        missing_pricing_model,
        data,
    }
}

/// Muse records no per-request cost, so `display` reports zero. `auto` and
/// `calculate` estimate from an exact pricing entry for the model id, either
/// `meta/<model>` as LiteLLM publishes Meta's API rates or the bare id (for
/// example a user pricing override). Fuzzy matching is deliberately avoided:
/// it would price `muse-spark-1.3` at the longer
/// `meta/muse-spark-1.3-contributor` entry. Models without an exact entry
/// stay at zero and are reported as missing pricing.
fn muse_cost(
    model: &str,
    usage: TokenUsageRaw,
    mode: CostMode,
    pricing: &PricingMap,
) -> (f64, Option<String>) {
    if mode == CostMode::Display {
        return (0.0, None);
    }
    let candidates = [format!("{PROVIDER_PREFIX}/{model}"), model.to_string()];
    for candidate in &candidates {
        if let Some(entry) = pricing.find_exact(candidate) {
            return (calculate_cost_from_pricing(usage, entry), None);
        }
    }
    let missing = (total_usage_tokens(usage) > 0).then(|| resolve_model_name(model).into_owned());
    (0.0, missing)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "01a17081-3c04-7d41-89eb-e49ee20953a7";

    fn parse(lines: &[String]) -> MuseSessionFile {
        let content = lines.join("\n");
        parse_session_content(
            content.as_bytes(),
            Path::new("/data/muse/sessions/2026/10/24/dir-session/session.jsonl"),
        )
    }

    fn model_completed(id: &str, recorded_at: i64, usage: &str) -> String {
        format!(
            r#"{{"schema_version":1,"id":"{id}","stream":{{"kind":"session","id":"{SESSION}"}},"sequence":64,"recorded_at":{recorded_at},"record_type":"event","durability":"durable","causation_id":null,"payload_type":"runtime.session","payload_schema_version":1,"payload":{{"kind":"run","run_id":"run-1","event":{{"kind":"model_completed","usage":{usage},"duration_ms":1988,"finish_reason":"tool_calls","model":"muse-wonder-1.0","estimated_prompt_tokens":38878}},"source_run_record_id":"src-1","source_run_record_sequence":25}}}}"#
        )
    }

    const REAL_USAGE: &str = r#"{"input_tokens":45596,"output_tokens":512,"cached_tokens":38132,"cache_write_tokens":0,"cache_read_tokens":38132,"reasoning_tokens":210}"#;

    #[test]
    fn splits_cached_input_and_keeps_reasoning_inside_output() {
        let file = parse(&[model_completed("rec-1", 1_792_892_878_389_308, REAL_USAGE)]);

        assert_eq!(file.entries.len(), 1);
        let entry = &file.entries[0];
        assert_eq!(entry.record_id, "rec-1");
        assert_eq!(entry.session_id, SESSION);
        assert_eq!(entry.model, "muse-wonder-1.0");
        assert_eq!(
            entry.tokens,
            MuseTokens {
                input: 45_596 - 38_132,
                output: 512,
                cache_creation: 0,
                cache_read: 38_132,
            }
        );
        // input_tokens + output_tokens + cache_write_tokens; reasoning is not added again.
        assert_eq!(entry.tokens.total(), 45_596 + 512);
    }

    #[test]
    fn reads_microsecond_timestamps_as_utc() {
        // 2026-10-24T23:59:59.999999Z must stay on the 24th in UTC.
        let file = parse(&[model_completed("rec-1", 1_792_886_399_999_999, REAL_USAGE)]);
        let entry = &file.entries[0];

        assert_eq!(entry.timestamp.as_millis(), 1_792_886_399_999);
        assert_eq!(
            format_rfc3339_millis(entry.timestamp),
            "2026-10-24T23:59:59.999Z"
        );
        let utc = crate::parse_tz(Some("UTC"));
        assert_eq!(format_date_tz(entry.timestamp, utc.as_ref()), "2026-10-24");
        let los_angeles = crate::parse_tz(Some("America/Los_Angeles"));
        assert_eq!(
            format_date_tz(entry.timestamp, los_angeles.as_ref()),
            "2026-10-24"
        );
        let tokyo = crate::parse_tz(Some("Asia/Tokyo"));
        assert_eq!(
            format_date_tz(entry.timestamp, tokyo.as_ref()),
            "2026-10-25"
        );
    }

    #[test]
    fn ignores_records_that_repeat_or_mention_usage() {
        let attribution = format!(
            r#"{{"schema_version":1,"id":"attr-1","stream":{{"kind":"session","id":"{SESSION}"}},"sequence":63,"recorded_at":1792892878389000,"record_type":"event","payload_type":"runtime.session","payload":{{"kind":"run","run_id":"run-1","event":{{"kind":"goal_usage_attribution","record":{{"schema_version":1,"usage_id":"usage-1","usage_family":"provider","quantity":{{"unit":"tokens","reported":true,"input_tokens":45596,"output_tokens":512,"cached_tokens":38132,"reasoning_tokens":210,"main_llm_steps":1}},"trigger_event":"model_completed"}}}}}}}}"#
        );
        let tool_output = format!(
            r#"{{"schema_version":1,"id":"tool-1","stream":{{"kind":"session","id":"{SESSION}"}},"sequence":70,"recorded_at":1792892878390000,"record_type":"event","payload_type":"runtime.session","payload":{{"kind":"task","event":{{"kind":"output","counts":{{"model_completed":1}},"usage":{REAL_USAGE},"text":"grep found {{\"kind\":\"model_completed\",\"usage\":{{\"input_tokens\":999}}}} and \"model_completed\" again"}}}}}}"#
        );
        let other_payload = format!(
            r#"{{"schema_version":1,"id":"other-1","stream":{{"kind":"session","id":"{SESSION}"}},"sequence":71,"recorded_at":1792892878391000,"record_type":"event","payload_type":"tool_batch.effect.terminal","payload":{{"kind":"run","event":{{"kind":"model_completed","model":"muse-wonder-1.0","usage":{REAL_USAGE}}}}}}}"#
        );
        let child = model_completed("framed-1", 1_792_892_878_392_000, REAL_USAGE)
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let retained_frame = format!(
            r#"{{"retained_frame":"session_permission_transaction","frame_schema_version":1,"outer_log_ordinal":1,"transaction_id":"tx-1","children":[{{"child_index":0,"record_json":"{child}"}}],"content_sha256":"abc","id":"framed-outer","payload_type":"runtime.session","payload":{{"event":{{"kind":"model_completed","model":"muse-wonder-1.0","usage":{REAL_USAGE}}}}}}}"#
        );
        let retained_marker = format!(
            r#"{{"retained_marker":"omitted_live_only","schema_version":1,"stream":{{"kind":"session","id":"{SESSION}"}},"position":{{"id":"pos-1","sequence":74}},"omitted_record":{{"record_type":"status","durability":"ephemeral","payload_type":"runtime.session","payload_schema_version":1,"payload_kind":"task","omission_class":"task_tool_delta_v1"}}}}"#
        );

        let file = parse(&[
            attribution,
            tool_output,
            other_payload,
            retained_frame,
            retained_marker,
            model_completed("rec-1", 1_792_892_878_389_308, REAL_USAGE),
        ]);

        assert_eq!(
            file.entries
                .iter()
                .map(|entry| entry.record_id.as_str())
                .collect::<Vec<_>>(),
            ["rec-1"]
        );
    }

    #[test]
    fn uses_cached_tokens_only_when_cache_read_tokens_is_absent() {
        let only_cached = r#"{"input_tokens":100,"output_tokens":10,"cached_tokens":40}"#;
        let both =
            r#"{"input_tokens":100,"output_tokens":10,"cached_tokens":40,"cache_read_tokens":30}"#;
        let file = parse(&[
            model_completed("only-cached", 1_792_892_878_000_000, only_cached),
            model_completed("both", 1_792_892_879_000_000, both),
        ]);

        assert_eq!(file.entries[0].tokens.cache_read, 40);
        assert_eq!(file.entries[0].tokens.input, 60);
        assert_eq!(file.entries[1].tokens.cache_read, 30);
        assert_eq!(file.entries[1].tokens.input, 70);
        assert!(file.entries.iter().all(|entry| entry.tokens.total() == 110));
    }

    #[test]
    fn keeps_cache_writes_separate_and_bounds_cache_reads_by_input() {
        let writes = r#"{"input_tokens":100,"output_tokens":10,"cache_read_tokens":0,"cache_write_tokens":25}"#;
        let oversized = r#"{"input_tokens":50,"output_tokens":5,"cache_read_tokens":80}"#;
        let file = parse(&[
            model_completed("writes", 1_792_892_878_000_000, writes),
            model_completed("oversized", 1_792_892_879_000_000, oversized),
        ]);

        assert_eq!(
            file.entries[0].tokens,
            MuseTokens {
                input: 100,
                output: 10,
                cache_creation: 25,
                cache_read: 0,
            }
        );
        assert_eq!(
            file.entries[1].tokens,
            MuseTokens {
                input: 0,
                output: 5,
                cache_creation: 0,
                cache_read: 50,
            }
        );
    }

    #[test]
    fn skips_zero_usage_and_falls_back_for_missing_fields() {
        let zero = r#"{"input_tokens":0,"output_tokens":0,"cache_read_tokens":0}"#;
        let no_model = r#"{"id":"no-model","recorded_at":1792892878000000,"payload_type":"runtime.session","payload":{"kind":"run","event":{"kind":"model_completed","usage":{"input_tokens":5,"output_tokens":1}}}}"#
            .to_string();
        let file = parse(&[
            model_completed("zero", 1_792_892_878_000_000, zero),
            no_model,
        ]);

        assert_eq!(file.entries.len(), 1);
        assert_eq!(file.entries[0].record_id, "no-model");
        assert_eq!(file.entries[0].model, UNKNOWN_MODEL);
        assert_eq!(file.entries[0].session_id, "dir-session");
    }

    #[test]
    fn reads_session_metadata_for_project_path_and_version() {
        let metadata = format!(
            r#"{{"schema_version":1,"id":"meta-1","stream":{{"kind":"session","id":"{SESSION}"}},"sequence":3,"recorded_at":1792888881615006,"record_type":"event","payload_type":"runtime.session.metadata","payload_schema_version":1,"payload":{{"kind":"metadata","record":{{"workspace_root":"/Users/example/project","provider_id":"meta","model_id":"muse-wonder-1.0","build":{{"sha":"39490bd065","semver":"1.4.13"}}}}}}}}"#
        );
        let file = parse(&[
            model_completed("rec-1", 1_792_892_878_389_308, REAL_USAGE),
            metadata,
        ]);

        assert_eq!(file.project_path.as_deref(), Some("/Users/example/project"));
        assert_eq!(file.version.as_deref(), Some("1.4.13"));
    }

    fn pricing_with(models: &[(&str, f64, f64, f64)]) -> PricingMap {
        let mut pricing = PricingMap::load_embedded();
        let json = models
            .iter()
            .map(|(model, input, output, cache_read)| {
                format!(
                    r#""{model}":{{"input_cost_per_token":{input},"output_cost_per_token":{output},"cache_read_input_token_cost":{cache_read},"litellm_provider":"meta","mode":"chat"}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        assert_eq!(pricing.load_json(&format!("{{{json}}}")), models.len());
        pricing
    }

    fn tokens() -> MuseTokens {
        MuseTokens {
            input: 1_000,
            output: 100,
            cache_creation: 0,
            cache_read: 10_000,
        }
    }

    #[test]
    fn prices_exact_meta_entries_without_fuzzy_matching() {
        let pricing = pricing_with(&[
            ("meta/muse-spark-1.3", 1.25e-6, 4.25e-6, 1.5e-7),
            ("meta/muse-spark-1.3-contributor", 1e-7, 2e-7, 2e-9),
        ]);
        let usage = tokens().usage();

        let (cost, missing) = muse_cost("muse-spark-1.3", usage, CostMode::Auto, &pricing);
        assert!((cost - (1_000.0 * 1.25e-6 + 100.0 * 4.25e-6 + 10_000.0 * 1.5e-7)).abs() < 1e-12);
        assert_eq!(missing, None);

        let (cost, missing) = muse_cost(
            "muse-spark-1.3-contributor",
            usage,
            CostMode::Calculate,
            &pricing,
        );
        assert!((cost - (1_000.0 * 1e-7 + 100.0 * 2e-7 + 10_000.0 * 2e-9)).abs() < 1e-12);
        assert_eq!(missing, None);
    }

    #[test]
    fn leaves_models_without_an_exact_price_unpriced() {
        let pricing = pricing_with(&[("meta/muse-wonder-1.0", 4e-6, 2e-5, 2e-7)]);
        let usage = tokens().usage();

        let (cost, missing) = muse_cost("muse-wonder-full-1.0", usage, CostMode::Auto, &pricing);
        assert_eq!(cost, 0.0);
        assert_eq!(missing.as_deref(), Some("muse-wonder-full-1.0"));

        let embedded = PricingMap::load_embedded();
        let (cost, missing) = muse_cost("muse-wonder-1.0", usage, CostMode::Auto, &embedded);
        assert_eq!(cost, 0.0, "the embedded snapshot carries no meta/ prices");
        assert_eq!(missing.as_deref(), Some("muse-wonder-1.0"));

        let (cost, missing) = muse_cost("muse-wonder-1.0", usage, CostMode::Display, &pricing);
        assert_eq!((cost, missing), (0.0, None));
    }
}
