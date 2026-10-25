use std::{collections::HashSet, path::Path};

use crate::{
    LoadedEntry, PricingMap, Result, cli::SharedArgs, debug_log, parse_tz, read_files_parallel,
};

use super::{
    parser::{MuseSessionFile, muse_entry_to_loaded, read_session_file},
    paths::discover_session_files,
};

pub fn load_entries(shared: &SharedArgs, pricing: &PricingMap) -> Result<Vec<LoadedEntry>> {
    crate::progress::track_usage_load(crate::progress::UsageLoadAgent("Muse"), shared.json, || {
        load_entries_inner(shared, pricing)
    })
}

fn load_entries_inner(shared: &SharedArgs, pricing: &PricingMap) -> Result<Vec<LoadedEntry>> {
    let tz = parse_tz(shared.timezone.as_deref());
    let files = discover_session_files();
    // Session ids that have a log: each log's directory is named after its
    // session, including reminder children logged under `subagent/<id>/`.
    let logged_sessions = files
        .iter()
        .filter_map(|file| file.parent().and_then(Path::file_name))
        .filter_map(|name| name.to_str())
        .collect::<HashSet<_>>();
    // Read session logs in parallel, then dedupe sequentially in discovery
    // order so the surviving copy of a record is the same as in a
    // single-threaded read. Forked or copied sessions carry the same ids.
    let loaded = read_files_parallel(&files, shared.single_thread, |file| {
        read_session_file(file).unwrap_or_else(|error| {
            debug_log(
                shared,
                format!(
                    "Failed to read Muse session log {}: {error}",
                    file.display()
                ),
            );
            MuseSessionFile::default()
        })
    });
    let mut entries = Vec::new();
    let mut seen_record_ids = HashSet::new();
    let mut seen_usage_ids = HashSet::new();
    for file in loaded {
        let project_path = file.project_path.as_deref();
        let version = file.version.as_deref();
        for entry in file.entries {
            // A logged reminder child is counted through its own
            // model_completed records; its attribution would count it twice.
            if entry
                .reminder_owner_id
                .as_deref()
                .is_some_and(|owner| logged_sessions.contains(owner))
            {
                continue;
            }
            if seen_record_ids.contains(&entry.record_id)
                || entry
                    .usage_id
                    .as_ref()
                    .is_some_and(|usage_id| seen_usage_ids.contains(usage_id))
            {
                continue;
            }
            seen_record_ids.insert(entry.record_id.clone());
            if let Some(usage_id) = &entry.usage_id {
                seen_usage_ids.insert(usage_id.clone());
            }
            entries.push(muse_entry_to_loaded(
                entry,
                project_path,
                version,
                tz.as_ref(),
                shared.mode,
                pricing,
            ));
        }
    }
    entries.sort_by_key(|entry| entry.timestamp);
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, ffi::OsString, fs, path::Path};

    use serde_json::Value;

    use super::super::paths::MUSE_DATA_DIR_ENV;
    use super::*;
    use crate::{TokenUsageRaw, total_usage_tokens};
    use turbotokens_test_support::{EnvVarsGuard, fs_fixture, muse};

    fn shared(timezone: &str) -> SharedArgs {
        SharedArgs {
            timezone: Some(timezone.to_string()),
            offline: true,
            single_thread: true,
            ..SharedArgs::default()
        }
    }

    fn isolated(data_dir: &Path) -> EnvVarsGuard {
        EnvVarsGuard::set_many([
            (MUSE_DATA_DIR_ENV, Some(data_dir.as_os_str().to_os_string())),
            ("XDG_DATA_HOME", None::<OsString>),
        ])
    }

    fn total(usage: TokenUsageRaw) -> u64 {
        total_usage_tokens(usage)
    }

    #[test]
    fn loads_the_shared_fixture_with_dedupe_subagents_and_skipped_dirs() {
        let fixture = fs_fixture!({});
        muse::write_fixture(fixture.root());
        let _env = isolated(fixture.root());

        let entries = load_entries(&shared("UTC"), &PricingMap::load_embedded()).unwrap();

        let ids = entries
            .iter()
            .map(|entry| entry.data.message.id.as_deref().unwrap())
            .collect::<Vec<_>>();
        // rec-b1 and the session-b reminder appear in two files and are
        // counted once; the hidden .msp-view and tool-outputs copies are never
        // read; the logged reminder child counts through rec-r1 only.
        assert_eq!(
            ids,
            [
                "rec-a1",
                "attr-rem-unlogged",
                "rec-a2",
                "rec-r1",
                "rec-c1",
                "rec-b1",
                "attr-rem-b",
            ]
        );
        assert_eq!(
            entries
                .iter()
                .map(|entry| total(entry.data.message.usage))
                .sum::<u64>(),
            muse::FIXTURE_TOTAL_TOKENS
        );

        let a1 = &entries[0];
        assert_eq!(a1.date, "2099-01-02");
        assert_eq!(a1.session_id.as_ref(), "session-a");
        assert_eq!(a1.project_path.as_ref(), "/workspace/project-a");
        assert_eq!(a1.data.version.as_deref(), Some("1.4.13"));
        assert_eq!(a1.model.as_deref(), Some("muse-wonder-1.0"));
        assert_eq!(a1.data.message.usage.input_tokens, 75);
        assert_eq!(a1.data.message.usage.cache_read_input_tokens, 25);
        assert_eq!(a1.data.message.usage.output_tokens, 10);
        // meta/muse-wonder-1.0 from the embedded snapshot.
        assert!((a1.cost - (75.0 * 4e-6 + 10.0 * 2e-5 + 25.0 * 2e-7)).abs() < 1e-12);
        assert_eq!(a1.missing_pricing_model, None);

        // An unlogged reminder side-call: muse-spark-1.3, cached part split out.
        let reminder = &entries[1];
        assert_eq!(reminder.session_id.as_ref(), "session-a");
        assert_eq!(reminder.date, "2099-01-02");
        assert_eq!(reminder.model.as_deref(), Some("muse-spark-1.3"));
        assert_eq!(reminder.project_path.as_ref(), "/workspace/project-a");
        assert_eq!(reminder.data.message.usage.input_tokens, 100);
        assert_eq!(reminder.data.message.usage.cache_read_input_tokens, 200);
        assert_eq!(reminder.data.message.usage.output_tokens, 30);
        assert_eq!(reminder.data.message.usage.cache_creation_input_tokens, 0);
        assert!(
            (reminder.cost - (100.0 * 1.25e-6 + 30.0 * 4.25e-6 + 200.0 * 1.5e-7)).abs() < 1e-12
        );

        // The logged reminder child is its own session, counted from its log.
        let logged_child = &entries[3];
        assert_eq!(logged_child.session_id.as_ref(), "child-r");
        assert_eq!(total(logged_child.data.message.usage), 88);

        // The subagent log is its own session, recorded at 23:59:59.999999 UTC.
        let child = &entries[4];
        assert_eq!(child.session_id.as_ref(), "child-a");
        assert_eq!(child.date, "2099-01-15");
        assert_eq!(child.data.timestamp, "2099-01-15T23:59:59.999Z");
        assert_eq!(child.data.message.usage.cache_creation_input_tokens, 10);
        assert_eq!(total(child.data.message.usage), 55);

        let b1 = &entries[5];
        assert_eq!(b1.session_id.as_ref(), "session-b");
        assert_eq!(b1.project_path.as_ref(), "/workspace/project-b");
        assert_eq!(b1.model.as_deref(), Some("muse-spark-1.3-contributor"));

        let reminder_b = &entries[6];
        assert_eq!(reminder_b.session_id.as_ref(), "session-b");
        assert_eq!(reminder_b.project_path.as_ref(), "/workspace/project-b");
        assert_eq!(total(reminder_b.data.message.usage), 550);
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry
                    .data
                    .message
                    .id
                    .as_deref()
                    .unwrap()
                    .starts_with("attr-"))
                .map(|entry| total(entry.data.message.usage))
                .sum::<u64>(),
            muse::FIXTURE_REMINDER_TOKENS
        );
    }

    #[test]
    fn counts_a_reminder_attribution_only_while_its_child_has_no_log() {
        let attribution = |id: &str| {
            muse::reminder_attribution(
                id,
                "parent",
                muse::micros("2026-10-24T12:00:00Z"),
                muse::Reminder {
                    usage_id: "usage-1",
                    owner_id: "child-1",
                    usage_family: "provider",
                    reported: true,
                    input: 1_000,
                    output: 50,
                    cached: 900,
                },
            )
        };
        let child_step = muse::model_completed(
            "child-step",
            "child-1",
            muse::micros("2026-10-24T11:59:59Z"),
            "muse-spark-1.3",
            muse::Usage {
                input: 1_000,
                output: 50,
                cache_read: 900,
                cache_write: 0,
                reasoning: 5,
            },
        );

        let unlogged = fs_fixture!({
            "sessions/2026/10/24/parent/session.jsonl": attribution("attr-1"),
        });
        {
            let _env = isolated(unlogged.root());
            let entries = load_entries(&shared("UTC"), &PricingMap::load_embedded()).unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].model.as_deref(), Some("muse-spark-1.3"));
            assert_eq!(entries[0].session_id.as_ref(), "parent");
            assert_eq!(entries[0].data.message.usage.input_tokens, 100);
            assert_eq!(entries[0].data.message.usage.cache_read_input_tokens, 900);
            assert_eq!(entries[0].data.message.usage.output_tokens, 50);
        }

        // Once the child is logged anywhere under the sessions root, only its
        // own model_completed record counts.
        let logged = fs_fixture!({
            "sessions/2026/10/24/parent/session.jsonl": attribution("attr-1"),
            "sessions/2026/10/24/parent/subagent/child-1/session.jsonl": child_step,
        });
        {
            let _env = isolated(logged.root());
            let entries = load_entries(&shared("UTC"), &PricingMap::load_embedded()).unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].data.message.id.as_deref(), Some("child-step"));
            assert_eq!(entries[0].session_id.as_ref(), "child-1");
            assert_eq!(total(entries[0].data.message.usage), 1_050);
        }

        // The same usage_id under a new record id is one side-call.
        let copied = fs_fixture!({
            "sessions/2026/10/24/parent/session.jsonl": [attribution("attr-1"), attribution("attr-1-copy")].join("\n"),
        });
        let _env = isolated(copied.root());
        let entries = load_entries(&shared("UTC"), &PricingMap::load_embedded()).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].data.message.id.as_deref(), Some("attr-1"));
    }

    #[test]
    fn groups_microsecond_timestamps_by_the_requested_timezone() {
        let fixture = fs_fixture!({});
        muse::write_fixture(fixture.root());
        let _env = isolated(fixture.root());

        let tokyo = load_entries(&shared("Asia/Tokyo"), &PricingMap::load_embedded()).unwrap();
        let child = tokyo
            .iter()
            .find(|entry| entry.session_id.as_ref() == "child-a")
            .unwrap();
        assert_eq!(child.date, "2099-01-16");
    }

    #[test]
    fn dedupes_records_by_id_across_files_and_keeps_distinct_ids() {
        let line = |id: &str, session: &str| {
            muse::model_completed(
                id,
                session,
                muse::micros("2026-10-24T12:00:00Z"),
                "muse-wonder-1.0",
                muse::Usage {
                    input: 100,
                    output: 10,
                    cache_read: 40,
                    cache_write: 0,
                    reasoning: 3,
                },
            )
        };
        let fixture = fs_fixture!({
            "sessions/2026/10/24/s1/session.jsonl": [line("dup", "s1"), line("only-s1", "s1")].join("\n"),
            "sessions/2026/10/24/s1-fork/session.jsonl": [line("dup", "s1"), line("only-fork", "s1-fork")].join("\n"),
        });
        let _env = isolated(fixture.root());

        let entries = load_entries(&shared("UTC"), &PricingMap::load_embedded()).unwrap();

        let mut ids = entries
            .iter()
            .map(|entry| entry.data.message.id.clone().unwrap())
            .collect::<Vec<_>>();
        ids.sort();
        assert_eq!(ids, ["dup", "only-fork", "only-s1"]);
        assert_eq!(
            entries
                .iter()
                .map(|entry| total(entry.data.message.usage))
                .sum::<u64>(),
            3 * 110
        );
    }

    #[test]
    fn missing_data_dir_yields_no_entries() {
        let fixture = fs_fixture!({});
        let _env = isolated(&fixture.path("missing"));

        assert!(
            load_entries(&shared("UTC"), &PricingMap::load_embedded())
                .unwrap()
                .is_empty()
        );
    }

    /// Reads the real Muse store when one exists and checks the adapter
    /// against an independent `serde_json::Value` walk of every log. Skipped
    /// (returns early) on machines without Muse data. Logs are append-only
    /// while Muse runs, so the independent walk happens first and the adapter
    /// must account for every record it saw, with identical token buckets.
    #[test]
    fn local_muse_data_matches_an_independent_parse() {
        // Hold the environment lock without changing anything, so tests that
        // point MUSE_DATA_DIR or HOME at fixtures cannot interleave.
        let _env = EnvVarsGuard::set_many([]);
        let files = discover_session_files();
        if files.is_empty() {
            eprintln!("skipping: no local Muse session logs");
            return;
        }

        let logged = files
            .iter()
            .filter_map(|file| file.parent()?.file_name()?.to_str())
            .collect::<std::collections::HashSet<_>>();
        let mut expected = BTreeMap::<String, (u64, u64, u64, u64)>::new();
        let mut usage_ids = std::collections::HashSet::new();
        for file in &files {
            let Ok(content) = fs::read(file) else {
                continue;
            };
            for line in content.split(|byte| *byte == b'\n') {
                let Ok(record) = serde_json::from_slice::<Value>(line) else {
                    continue;
                };
                if record.get("retained_frame").is_some()
                    || record["payload_type"] != "runtime.session"
                {
                    continue;
                }
                let event = &record["payload"]["event"];
                if event["kind"] == "goal_usage_attribution" {
                    let attribution = &event["record"];
                    let quantity = &attribution["quantity"];
                    let owner_id = attribution["owner"]["owner_id"].as_str();
                    if attribution["owner"]["owner_type"] != "reminder_child"
                        || attribution["usage_family"] != "provider"
                        || quantity["reported"] != true
                        || owner_id.is_none_or(|owner| logged.contains(owner))
                        || !usage_ids.insert(attribution["usage_id"].to_string())
                    {
                        continue;
                    }
                    let field = |name: &str| quantity[name].as_u64().unwrap_or_default();
                    let cached = field("cached_tokens").min(field("input_tokens"));
                    let id = record["id"].as_str().expect("record id").to_string();
                    expected.entry(id).or_insert((
                        field("input_tokens") - cached,
                        field("output_tokens"),
                        0,
                        cached,
                    ));
                    continue;
                }
                if event["kind"] != "model_completed" {
                    continue;
                }
                let usage = &record["payload"]["event"]["usage"];
                let field = |name: &str| usage[name].as_u64().unwrap_or_default();
                let cache_read = usage["cache_read_tokens"]
                    .as_u64()
                    .unwrap_or_else(|| field("cached_tokens"))
                    .min(field("input_tokens"));
                let tokens = (
                    field("input_tokens") - cache_read,
                    field("output_tokens"),
                    field("cache_write_tokens"),
                    cache_read,
                );
                if tokens.0 + tokens.1 + tokens.2 + tokens.3 == 0 {
                    continue;
                }
                let id = record["id"].as_str().expect("record id").to_string();
                expected.entry(id).or_insert(tokens);
            }
        }

        let entries = load_entries(&shared("UTC"), &PricingMap::load_embedded()).unwrap();
        let actual = entries
            .iter()
            .map(|entry| {
                let usage = entry.data.message.usage;
                (
                    entry.data.message.id.clone().unwrap(),
                    (
                        usage.input_tokens,
                        usage.output_tokens,
                        usage.cache_creation_input_tokens,
                        usage.cache_read_input_tokens,
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();

        assert_eq!(actual.len(), entries.len(), "record ids are unique");
        assert!(!expected.is_empty());
        for (id, tokens) in &expected {
            assert_eq!(actual.get(id), Some(tokens), "record {id}");
        }
        assert!(entries.iter().all(|entry| entry.project.as_ref() == "muse"));
        assert!(entries.iter().all(|entry| entry.timestamp.as_millis() > 0));
    }
}
