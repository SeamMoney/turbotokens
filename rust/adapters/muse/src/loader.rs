use std::collections::{HashMap, HashSet};

use crate::{
    LoadedEntry, PricingMap, Result, cli::SharedArgs, debug_log, parse_tz, read_files_parallel,
};

use super::{
    parser::{
        MuseSessionFile, MuseTokens, MuseUsageEntry, muse_entry_to_loaded, read_session_file,
    },
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
    // Read session logs in parallel, then decide and dedupe sequentially in
    // discovery order so the surviving copy of a record is the same as in a
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
    let reminders = ReminderIndex::new(&loaded);
    let mut entries = Vec::new();
    let mut seen_record_ids = HashSet::new();
    let mut seen_usage_ids = HashSet::new();
    for file in loaded {
        let project_path = file.project_path.as_deref();
        let version = file.version.as_deref();
        for entry in file.entries {
            if !reminders.counts(&entry, file.has_model_completed) {
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

/// What every log says about reminder children, gathered before any reminder
/// attribution is counted.
struct ReminderIndex {
    /// Sessions logged with at least one `model_completed` record, by log
    /// directory name and by `stream.id`.
    logged_sessions: HashSet<String>,
    /// Child-side attribution copies: owner id to the token counts recorded.
    child_copies: HashMap<String, HashSet<MuseTokens>>,
}

impl ReminderIndex {
    fn new(files: &[MuseSessionFile]) -> Self {
        let mut logged_sessions = HashSet::new();
        let mut child_copies = HashMap::<String, HashSet<MuseTokens>>::new();
        for file in files {
            logged_sessions.extend(file.logged_sessions.iter().cloned());
            for entry in &file.entries {
                if let Some(reminder) = entry.reminder.as_ref().filter(|r| r.child_side) {
                    child_copies
                        .entry(reminder.owner_id.clone())
                        .or_default()
                        .insert(entry.tokens);
                }
            }
        }
        Self {
            logged_sessions,
            child_copies,
        }
    }

    /// Whether `entry` counts. `model_completed` records always do. A reminder
    /// attribution is the fallback record of a side-call and fails safe:
    ///
    /// - a parent-side record is dropped when the child is logged with a
    ///   `model_completed` record (by directory or stream id), or when the
    ///   child's own copy of the attribution, with identical token counts,
    ///   exists in any log; otherwise it counts;
    /// - a child-side copy is dropped when its log, or any log of that child,
    ///   has a `model_completed` record; otherwise (a child that never
    ///   completed a step) it counts, and the parent-side record defers to it.
    fn counts(&self, entry: &MuseUsageEntry, file_has_model_completed: bool) -> bool {
        let Some(reminder) = &entry.reminder else {
            return true;
        };
        if self.logged_sessions.contains(&reminder.owner_id) {
            return false;
        }
        if reminder.child_side {
            return !file_has_model_completed;
        }
        !self
            .child_copies
            .get(&reminder.owner_id)
            .is_some_and(|copies| copies.contains(&entry.tokens))
    }
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

    /// 1,000 input (900 cached) + 100 output: 1,100 tokens.
    const REMINDER: muse::Reminder<'static> = muse::Reminder {
        usage_id: "usage-parent",
        owner_id: "child-1",
        owner_session_id: None,
        usage_family: "provider",
        reported: true,
        input: 1_000,
        output: 100,
        cached: 900,
    };

    fn reminder_time() -> i64 {
        muse::micros("2026-10-24T12:00:00Z")
    }

    fn parent_attribution(id: &str) -> String {
        muse::reminder_attribution(id, "parent", reminder_time(), REMINDER)
    }

    fn child_copy(stream: &str) -> String {
        muse::reminder_attribution(
            "attr-child-copy",
            stream,
            reminder_time() - 2_000_000,
            muse::Reminder {
                usage_id: "usage-child-copy",
                owner_session_id: Some(stream),
                ..REMINDER
            },
        )
    }

    fn step(id: &str, stream: &str, recorded_at: i64, model: &str) -> String {
        muse::model_completed(
            id,
            stream,
            recorded_at,
            model,
            muse::Usage {
                input: 1_000,
                output: 100,
                cache_read: 900,
                cache_write: 0,
                reasoning: 5,
            },
        )
    }

    fn load_logs(logs: &[(&str, Vec<String>)]) -> Vec<LoadedEntry> {
        let fixture = turbotokens_test_support::Fixture::new();
        for (path, lines) in logs {
            let _ = fixture.write_file(format!("sessions/{path}"), lines.join("\n") + "\n");
        }
        let _env = isolated(fixture.root());
        load_entries(&shared("UTC"), &PricingMap::load_embedded()).unwrap()
    }

    fn counted(entries: &[LoadedEntry]) -> Vec<(&str, u64)> {
        entries
            .iter()
            .map(|entry| {
                (
                    entry.data.message.id.as_deref().unwrap(),
                    total(entry.data.message.usage),
                )
            })
            .collect()
    }

    #[test]
    fn counts_an_unlogged_reminder_from_its_parent_attribution() {
        let entries = load_logs(&[(
            "2026/10/24/parent/session.jsonl",
            vec![parent_attribution("attr-parent")],
        )]);

        assert_eq!(counted(&entries), [("attr-parent", 1_100)]);
        assert_eq!(entries[0].model.as_deref(), Some("muse-spark-1.3"));
        assert_eq!(entries[0].session_id.as_ref(), "parent");
        assert_eq!(entries[0].data.message.usage.input_tokens, 100);
        assert_eq!(entries[0].data.message.usage.cache_read_input_tokens, 900);
        assert_eq!(entries[0].data.message.usage.output_tokens, 100);
    }

    #[test]
    fn a_logged_reminder_child_counts_only_through_its_own_steps() {
        let entries = load_logs(&[
            (
                "2026/10/24/parent/session.jsonl",
                vec![parent_attribution("attr-parent")],
            ),
            (
                "2026/10/24/parent/subagent/child-1/session.jsonl",
                vec![
                    child_copy("child-1"),
                    step(
                        "child-step",
                        "child-1",
                        reminder_time() - 1_000_000,
                        "muse-spark-1.3",
                    ),
                ],
            ),
        ]);

        assert_eq!(counted(&entries), [("child-step", 1_100)]);
        assert_eq!(entries[0].session_id.as_ref(), "child-1");
    }

    #[test]
    fn a_child_folder_without_steps_does_not_hide_the_reminder() {
        // Crashed child: its folder exists but it never completed a step.
        let metadata_only = load_logs(&[
            (
                "2026/10/24/parent/session.jsonl",
                vec![parent_attribution("attr-parent")],
            ),
            (
                "2026/10/24/parent/subagent/child-1/session.jsonl",
                vec![muse::metadata(
                    "meta-child",
                    "child-1",
                    reminder_time() - 3_000_000,
                    "/w",
                )],
            ),
        ]);
        assert_eq!(counted(&metadata_only), [("attr-parent", 1_100)]);

        // It wrote its own copy of the attribution, still without a step: the
        // side-call counts once, from the child's copy.
        let with_copy = load_logs(&[
            (
                "2026/10/24/parent/session.jsonl",
                vec![parent_attribution("attr-parent")],
            ),
            (
                "2026/10/24/parent/subagent/child-1/session.jsonl",
                vec![child_copy("child-1")],
            ),
        ]);
        assert_eq!(counted(&with_copy), [("attr-child-copy", 1_100)]);
    }

    #[test]
    fn a_child_logged_outside_a_folder_named_after_it_is_not_counted_twice() {
        // The folder name differs, but the log's stream is the child.
        let renamed_folder = load_logs(&[
            (
                "2026/10/24/parent/session.jsonl",
                vec![parent_attribution("attr-parent")],
            ),
            (
                "2026/10/24/parent/subagent/renamed/session.jsonl",
                vec![
                    child_copy("child-1"),
                    step(
                        "child-step",
                        "child-1",
                        reminder_time() - 1_000_000,
                        "muse-spark-1.3",
                    ),
                ],
            ),
        ]);
        assert_eq!(counted(&renamed_folder), [("child-step", 1_100)]);

        // Folder and stream both differ; the child's own attribution copy
        // (same owner id and token counts) still identifies it.
        let renamed_stream = load_logs(&[
            (
                "2026/10/24/parent/session.jsonl",
                vec![parent_attribution("attr-parent")],
            ),
            (
                "2026/10/24/parent/subagent/renamed/session.jsonl",
                vec![
                    child_copy("renamed-stream"),
                    step(
                        "child-step",
                        "renamed-stream",
                        reminder_time() - 1_000_000,
                        "muse-spark-1.3",
                    ),
                ],
            ),
        ]);
        assert_eq!(counted(&renamed_stream), [("child-step", 1_100)]);
    }

    #[test]
    fn dedupes_reminder_attributions_by_usage_id() {
        let entries = load_logs(&[(
            "2026/10/24/parent/session.jsonl",
            vec![
                parent_attribution("attr-parent"),
                parent_attribution("attr-parent-copy"),
            ],
        )]);

        assert_eq!(counted(&entries), [("attr-parent", 1_100)]);
    }

    #[test]
    fn a_reminder_runs_on_spark_under_wonder_and_on_the_main_model_otherwise() {
        let reminder_model = |main_models: &[&str]| {
            let mut lines = main_models
                .iter()
                .enumerate()
                .map(|(index, model)| {
                    step(
                        &format!("main-{index}"),
                        "parent",
                        reminder_time() - 10_000_000 + index as i64,
                        model,
                    )
                })
                .collect::<Vec<_>>();
            lines.push(parent_attribution("attr-parent"));
            let entries = load_logs(&[("2026/10/24/parent/session.jsonl", lines)]);
            entries
                .into_iter()
                .find(|entry| entry.data.message.id.as_deref() == Some("attr-parent"))
                .and_then(|entry| entry.model)
                .unwrap()
        };

        assert_eq!(reminder_model(&[]), "muse-spark-1.3");
        assert_eq!(reminder_model(&["muse-wonder-1.0"]), "muse-spark-1.3");
        assert_eq!(reminder_model(&["muse-wonder-full-1.0"]), "muse-spark-1.3");
        assert_eq!(
            reminder_model(&["muse-spark-1.3-contributor"]),
            "muse-spark-1.3-contributor"
        );
        // The latest main model before the attribution decides.
        assert_eq!(
            reminder_model(&["muse-wonder-1.0", "muse-spark-1.3-contributor"]),
            "muse-spark-1.3-contributor"
        );
        assert_eq!(
            reminder_model(&["muse-spark-1.3-contributor", "muse-wonder-1.0"]),
            "muse-spark-1.3"
        );
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
    /// against an independent `serde_json::Value` implementation of the same
    /// rules: exactly the same records, token buckets, and models, so both
    /// over- and under-counting fail. The logs are snapshotted first so a
    /// running Muse cannot make the two parses see different bytes. Skipped
    /// (returns early) on machines without Muse data.
    #[test]
    fn local_muse_data_matches_an_independent_parse() {
        use super::super::paths::{MUSE_SESSIONS_DIR_NAME, collect_session_files, data_roots};
        use std::collections::{HashMap, HashSet};

        let roots = {
            // Hold the environment lock while reading the real roots.
            let _env = EnvVarsGuard::set_many([]);
            data_roots()
        };
        let snapshot = turbotokens_test_support::Fixture::new();
        let mut copies = Vec::new();
        for (index, root) in roots.iter().enumerate() {
            let sessions = root.join(MUSE_SESSIONS_DIR_NAME);
            let mut files = Vec::new();
            collect_session_files(&sessions, &mut files);
            let copy_root = snapshot.path(index.to_string());
            for file in files {
                let target = copy_root
                    .join(MUSE_SESSIONS_DIR_NAME)
                    .join(file.strip_prefix(&sessions).unwrap());
                fs::create_dir_all(target.parent().unwrap()).unwrap();
                fs::copy(&file, &target).unwrap();
            }
            copies.push(copy_root.display().to_string());
        }
        let _env = EnvVarsGuard::set_many([
            (MUSE_DATA_DIR_ENV, Some(OsString::from(copies.join(",")))),
            ("XDG_DATA_HOME", None),
        ]);
        let files = discover_session_files();
        if files.is_empty() {
            eprintln!("skipping: no local Muse session logs");
            return;
        }

        type Tokens = (u64, u64, u64, u64);
        let records = |file: &Path| {
            fs::read(file)
                .unwrap()
                .split(|byte| *byte == b'\n')
                .filter_map(|line| serde_json::from_slice::<Value>(line).ok())
                .filter(|record| {
                    record.get("retained_frame").is_none()
                        && record["payload_type"] == "runtime.session"
                })
                .collect::<Vec<_>>()
        };
        let dir_name = |file: &Path| {
            file.parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap()
                .to_string()
        };
        // A qualifying reminder attribution: (owner id, child-side, tokens).
        let reminder = |record: &Value, file: &Path| -> Option<(String, bool, Tokens)> {
            let event = &record["payload"]["event"];
            let attribution = &event["record"];
            let quantity = &attribution["quantity"];
            if event["kind"] != "goal_usage_attribution"
                || attribution["owner"]["owner_type"] != "reminder_child"
                || attribution["usage_family"] != "provider"
                || quantity["reported"] != true
            {
                return None;
            }
            let owner_id = attribution["owner"]["owner_id"].as_str()?.to_string();
            let stream = record["stream"]["id"]
                .as_str()
                .map_or_else(|| dir_name(file), str::to_string);
            let child_side = owner_id == stream
                || attribution["owner"]["session_id"].as_str() == Some(stream.as_str())
                || owner_id == dir_name(file);
            let field = |name: &str| quantity[name].as_u64().unwrap_or_default();
            let cached = field("cached_tokens").min(field("input_tokens"));
            let tokens = (
                field("input_tokens") - cached,
                field("output_tokens"),
                0,
                cached,
            );
            Some((owner_id, child_side, tokens))
        };
        let is_step = |record: &Value| record["payload"]["event"]["kind"] == "model_completed";

        let parsed = files
            .iter()
            .map(|file| (file, records(file)))
            .collect::<Vec<_>>();
        let mut logged = HashSet::new();
        let mut copies_by_owner = HashMap::<String, HashSet<Tokens>>::new();
        for (file, records) in &parsed {
            for record in records {
                if is_step(record) {
                    logged.insert(dir_name(file));
                    logged.insert(
                        record["stream"]["id"]
                            .as_str()
                            .map_or_else(|| dir_name(file), str::to_string),
                    );
                } else if let Some((owner, true, tokens)) = reminder(record, file) {
                    copies_by_owner.entry(owner).or_default().insert(tokens);
                }
            }
        }

        let mut expected = BTreeMap::<String, (Tokens, String)>::new();
        let mut usage_ids = HashSet::new();
        for (file, records) in &parsed {
            let file_has_step = records.iter().any(is_step);
            let mut main_model: Option<String> = None;
            for record in records {
                let id = record["id"].as_str().expect("record id").to_string();
                let (tokens, model) = if is_step(record) {
                    let event = &record["payload"]["event"];
                    if let Some(model) = event["model"].as_str() {
                        main_model = Some(model.to_string());
                    }
                    let usage = &event["usage"];
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
                    (
                        tokens,
                        event["model"].as_str().unwrap_or("unknown").to_string(),
                    )
                } else if let Some((owner, child_side, tokens)) = reminder(record, file) {
                    let counts = !logged.contains(&owner)
                        && if child_side {
                            !file_has_step
                        } else {
                            !copies_by_owner
                                .get(&owner)
                                .is_some_and(|copies| copies.contains(&tokens))
                        };
                    let usage_id = record["payload"]["event"]["record"]["usage_id"].to_string();
                    if !counts || usage_ids.contains(&usage_id) {
                        continue;
                    }
                    if tokens.0 + tokens.1 + tokens.3 > 0 {
                        usage_ids.insert(usage_id);
                    }
                    let model = match main_model.as_deref() {
                        Some(model) if !model.starts_with("muse-wonder") => model.to_string(),
                        _ => "muse-spark-1.3".to_string(),
                    };
                    (tokens, model)
                } else {
                    continue;
                };
                if tokens.0 + tokens.1 + tokens.2 + tokens.3 == 0 {
                    continue;
                }
                expected.entry(id).or_insert((tokens, model));
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
                        (
                            usage.input_tokens,
                            usage.output_tokens,
                            usage.cache_creation_input_tokens,
                            usage.cache_read_input_tokens,
                        ),
                        entry.model.clone().unwrap(),
                    ),
                )
            })
            .collect::<BTreeMap<_, _>>();

        assert_eq!(actual.len(), entries.len(), "record ids are unique");
        assert!(!expected.is_empty());
        let total = |map: &BTreeMap<String, (Tokens, String)>| {
            map.values()
                .map(|((input, output, write, read), _)| input + output + write + read)
                .sum::<u64>()
        };
        assert_eq!(total(&actual), total(&expected));
        assert_eq!(actual, expected);
        assert!(entries.iter().all(|entry| entry.project.as_ref() == "muse"));
        assert!(entries.iter().all(|entry| entry.timestamp.as_millis() > 0));
    }
}
