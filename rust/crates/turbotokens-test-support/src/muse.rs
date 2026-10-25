//! Muse `session.jsonl` fixtures shared by the Muse adapter and unified report tests.
//!
//! Lines follow the shape Muse writes: one JSON record per line, usage on
//! `runtime.session` records whose `payload.event.kind` is `model_completed`,
//! and `recorded_at` in integer microseconds since the Unix epoch.

use std::{fs, path::Path};

/// Raw `payload.event.usage` counters as Muse records them: `input` includes
/// `cache_read`, and `reasoning` is part of `output`.
#[derive(Clone, Copy)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
}

/// Tokens from the two unlogged reminder side-calls in [`write_fixture`].
pub const FIXTURE_REMINDER_TOKENS: u64 = 330 + 550;

/// Total tokens in [`write_fixture`]: every counted `model_completed`
/// record's `input + output + cache_write` plus the unlogged reminder
/// side-calls' `input + output`, once each.
pub const FIXTURE_TOTAL_TOKENS: u64 = 110 + 220 + 55 + 88 + 1_100 + FIXTURE_REMINDER_TOKENS;

/// A reminder child's usage as Muse attributes it in a `goal_usage_attribution`
/// record: `input` includes `cached`.
#[derive(Clone, Copy)]
pub struct Reminder<'a> {
    pub usage_id: &'a str,
    /// The reminder child's session id (`owner.owner_id`).
    pub owner_id: &'a str,
    pub usage_family: &'a str,
    pub reported: bool,
    pub input: u64,
    pub output: u64,
    pub cached: u64,
}

/// Microseconds since the Unix epoch for an RFC 3339 timestamp.
pub fn micros(timestamp: &str) -> i64 {
    timestamp
        .parse::<jiff::Timestamp>()
        .expect("valid RFC 3339 fixture timestamp")
        .as_microsecond()
}

pub fn model_completed(
    id: &str,
    session: &str,
    recorded_at: i64,
    model: &str,
    usage: Usage,
) -> String {
    format!(
        concat!(
            r#"{{"schema_version":1,"id":"{id}","stream":{{"kind":"session","id":"{session}"}},"#,
            r#""sequence":64,"recorded_at":{recorded_at},"record_type":"event","durability":"durable","#,
            r#""causation_id":null,"payload_type":"runtime.session","payload_schema_version":1,"#,
            r#""payload":{{"kind":"run","run_id":"run-{session}","event":{{"kind":"model_completed","#,
            r#""usage":{{"input_tokens":{input},"output_tokens":{output},"cached_tokens":{cache_read},"#,
            r#""cache_write_tokens":{cache_write},"cache_read_tokens":{cache_read},"reasoning_tokens":{reasoning}}},"#,
            r#""duration_ms":1988,"finish_reason":"tool_calls","model":"{model}","estimated_prompt_tokens":{input}}},"#,
            r#""source_run_record_id":"src-{id}","source_run_record_sequence":25}}}}"#,
        ),
        id = id,
        session = session,
        recorded_at = recorded_at,
        model = model,
        input = usage.input,
        output = usage.output,
        cache_read = usage.cache_read,
        cache_write = usage.cache_write,
        reasoning = usage.reasoning,
    )
}

/// The per-step attribution record Muse writes next to each `model_completed`.
/// It repeats the same usage and must never be counted.
pub fn goal_usage_attribution(id: &str, session: &str, recorded_at: i64, usage: Usage) -> String {
    format!(
        concat!(
            r#"{{"schema_version":1,"id":"{id}","stream":{{"kind":"session","id":"{session}"}},"#,
            r#""sequence":63,"recorded_at":{recorded_at},"record_type":"event","durability":"durable","#,
            r#""causation_id":null,"payload_type":"runtime.session","payload_schema_version":1,"#,
            r#""payload":{{"kind":"run","run_id":"run-{session}","event":{{"kind":"goal_usage_attribution","#,
            r#""record":{{"schema_version":1,"usage_id":"usage-{id}","usage_family":"provider","#,
            r#""quantity":{{"unit":"tokens","reported":true,"input_tokens":{input},"output_tokens":{output},"#,
            r#""cached_tokens":{cache_read},"reasoning_tokens":{reasoning},"main_llm_steps":1}},"#,
            r#""owner":{{"requester_kind":"main","session_id":"{session}","owner_type":"main_root"}}}}}},"#,
            r#""source_run_record_id":"src-{id}","source_run_record_sequence":24}}}}"#,
        ),
        id = id,
        session = session,
        recorded_at = recorded_at,
        input = usage.input,
        output = usage.output,
        cache_read = usage.cache_read,
        reasoning = usage.reasoning,
    )
}

/// A reminder side-call's attribution, written in the log of the session the
/// reminder ran for (or in the reminder child's own log).
pub fn reminder_attribution(
    id: &str,
    session: &str,
    recorded_at: i64,
    reminder: Reminder<'_>,
) -> String {
    format!(
        concat!(
            r#"{{"schema_version":1,"id":"{id}","stream":{{"kind":"session","id":"{session}"}},"#,
            r#""sequence":90,"recorded_at":{recorded_at},"record_type":"event","durability":"durable","#,
            r#""causation_id":null,"payload_type":"runtime.session","payload_schema_version":1,"#,
            r#""payload":{{"kind":"run","run_id":"run-{session}","event":{{"kind":"goal_usage_attribution","#,
            r#""record":{{"schema_version":1,"usage_id":"{usage_id}","usage_family":"{usage_family}","#,
            r#""quantity":{{"unit":"tokens","reported":{reported},"input_tokens":{input},"output_tokens":{output},"#,
            r#""cached_tokens":{cached},"reasoning_tokens":3,"main_llm_steps":0}},"#,
            r#""owner":{{"requester_kind":"reminder","session_id":"run-{session}","run_id":"run-{session}","#,
            r#""owner_id":"{owner_id}","owner_type":"reminder_child"}},"goal_attribution":{{"mode":"none"}}}}}},"#,
            r#""source_run_record_id":"src-{id}","source_run_record_sequence":40}}}}"#,
        ),
        id = id,
        session = session,
        recorded_at = recorded_at,
        usage_id = reminder.usage_id,
        usage_family = reminder.usage_family,
        reported = reminder.reported,
        input = reminder.input,
        output = reminder.output,
        cached = reminder.cached,
        owner_id = reminder.owner_id,
    )
}

pub fn metadata(id: &str, session: &str, recorded_at: i64, workspace_root: &str) -> String {
    format!(
        concat!(
            r#"{{"schema_version":1,"id":"{id}","stream":{{"kind":"session","id":"{session}"}},"#,
            r#""sequence":3,"recorded_at":{recorded_at},"record_type":"event","durability":"durable","#,
            r#""causation_id":null,"payload_type":"runtime.session.metadata","payload_schema_version":1,"#,
            r#""payload":{{"kind":"metadata","record":{{"workspace_root":"{workspace_root}","#,
            r#""provider_id":"meta","model_id":"muse-wonder-1.0","web_search_mode":"client","#,
            r#""build":{{"sha":"39490bd065","semver":"1.4.13"}}}}}}}}"#,
        ),
        id = id,
        session = session,
        recorded_at = recorded_at,
        workspace_root = workspace_root,
    )
}

/// A task output whose text quotes a usage record, as when an agent greps logs.
pub fn tool_output_quoting_usage(id: &str, session: &str, recorded_at: i64) -> String {
    format!(
        concat!(
            r#"{{"schema_version":1,"id":"{id}","stream":{{"kind":"session","id":"{session}"}},"#,
            r#""sequence":80,"recorded_at":{recorded_at},"record_type":"event","durability":"durable","#,
            r#""causation_id":null,"payload_type":"runtime.session","payload_schema_version":1,"#,
            r#""payload":{{"kind":"task","event":{{"kind":"output","counts":{{"model_completed":1}},"#,
            r#""text":"{{\"kind\":\"model_completed\",\"usage\":{{\"input_tokens\":9999,\"output_tokens\":9}}}}"}}}}}}"#,
        ),
        id = id,
        session = session,
        recorded_at = recorded_at,
    )
}

/// A permission frame wrapping a child record as a JSON string.
pub fn retained_frame(child_record: &str) -> String {
    let escaped = child_record.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        concat!(
            r#"{{"retained_frame":"session_permission_transaction","frame_schema_version":1,"#,
            r#""outer_log_ordinal":1,"transaction_id":"tx-1","children":[{{"child_index":0,"#,
            r#""record_json":"{escaped}"}}],"content_sha256":"0000"}}"#,
        ),
        escaped = escaped,
    )
}

/// Writes a representative Muse data directory under `root` (the directory a
/// `MUSE_DATA_DIR` points at):
///
/// - `session-a` (main): two counted steps, an attribution, a quoted tool
///   result, and a permission frame wrapping a step;
/// - `session-a/subagent/child-a`: one step at 23:59:59.999999 UTC with a
///   cache write;
/// - reminder side-calls: `session-a` attributes an unlogged child (counted
///   as `muse-spark-1.3`) and the logged child `child-r`, whose own log under
///   `subagent/` holds the counted step and its own attribution;
/// - `session-b`: one step, an unlogged reminder, a `tool` attribution and an
///   unreported one (neither counted), plus a forked copy of the session
///   holding the same record id and the same reminder under a new record id;
/// - a hidden `.msp-view-g6` directory and a `tool-outputs` directory, each
///   with a `session.jsonl` that must never be read.
pub fn write_fixture(root: &Path) {
    let sessions = root.join("sessions");
    let a1_usage = Usage {
        input: 100,
        output: 10,
        cache_read: 25,
        cache_write: 0,
        reasoning: 4,
    };
    let a1_time = micros("2099-01-02T00:00:00Z");
    let logged_reminder = Reminder {
        usage_id: "usage-rem-2",
        owner_id: "child-r",
        usage_family: "provider",
        reported: true,
        input: 80,
        output: 8,
        cached: 60,
    };
    let session_a = [
        metadata(
            "meta-a",
            "session-a",
            a1_time - 5_000_000,
            "/workspace/project-a",
        ),
        goal_usage_attribution("attr-a1", "session-a", a1_time - 1, a1_usage),
        model_completed("rec-a1", "session-a", a1_time, "muse-wonder-1.0", a1_usage),
        tool_output_quoting_usage("tool-a1", "session-a", a1_time + 1_000_000),
        retained_frame(&model_completed(
            "framed-a1",
            "session-a",
            a1_time + 2_000_000,
            "muse-wonder-1.0",
            a1_usage,
        )),
        reminder_attribution(
            "attr-rem-unlogged",
            "session-a",
            micros("2099-01-02T06:00:00Z"),
            Reminder {
                usage_id: "usage-rem-1",
                owner_id: "reminder-unlogged",
                usage_family: "provider",
                reported: true,
                input: 300,
                output: 30,
                cached: 200,
            },
        ),
        reminder_attribution(
            "attr-rem-logged",
            "session-a",
            micros("2099-01-15T12:30:00Z"),
            logged_reminder,
        ),
        model_completed(
            "rec-a2",
            "session-a",
            micros("2099-01-15T12:00:00Z"),
            "muse-wonder-1.0",
            Usage {
                input: 200,
                output: 20,
                cache_read: 150,
                cache_write: 0,
                reasoning: 12,
            },
        ),
    ];
    write_lines(
        &sessions.join("2099/01/02/session-a/session.jsonl"),
        &session_a,
    );

    let child_time = micros("2099-01-15T23:59:59.999999Z");
    write_lines(
        &sessions.join("2099/01/02/session-a/subagent/child-a/session.jsonl"),
        &[
            metadata(
                "meta-c",
                "child-a",
                child_time - 1_000,
                "/workspace/project-a",
            ),
            model_completed(
                "rec-c1",
                "child-a",
                child_time,
                "muse-spark-1.3",
                Usage {
                    input: 40,
                    output: 5,
                    cache_read: 0,
                    cache_write: 10,
                    reasoning: 0,
                },
            ),
        ],
    );

    let child_r_time = micros("2099-01-15T12:29:59Z");
    write_lines(
        &sessions.join("2099/01/02/session-a/subagent/child-r/session.jsonl"),
        &[
            metadata(
                "meta-r",
                "child-r",
                child_r_time - 1_000,
                "/workspace/project-a",
            ),
            reminder_attribution(
                "attr-r1-own",
                "child-r",
                child_r_time - 1,
                Reminder {
                    usage_id: "usage-r1-own",
                    ..logged_reminder
                },
            ),
            model_completed(
                "rec-r1",
                "child-r",
                child_r_time,
                "muse-spark-1.3",
                Usage {
                    input: 80,
                    output: 8,
                    cache_read: 60,
                    cache_write: 0,
                    reasoning: 2,
                },
            ),
        ],
    );

    let reminder_b = Reminder {
        usage_id: "usage-rem-3",
        owner_id: "reminder-b",
        usage_family: "provider",
        reported: true,
        input: 500,
        output: 50,
        cached: 400,
    };
    let reminder_b_time = micros("2099-02-01T01:00:00Z");
    let b1 = model_completed(
        "rec-b1",
        "session-b",
        micros("2099-02-01T00:00:00Z"),
        "muse-spark-1.3-contributor",
        Usage {
            input: 1_000,
            output: 100,
            cache_read: 900,
            cache_write: 0,
            reasoning: 60,
        },
    );
    write_lines(
        &sessions.join("2099/02/01/session-b/session.jsonl"),
        &[
            metadata(
                "meta-b",
                "session-b",
                micros("2099-01-31T23:00:00Z"),
                "/workspace/project-b",
            ),
            b1.clone(),
            reminder_attribution("attr-rem-b", "session-b", reminder_b_time, reminder_b),
            reminder_attribution(
                "attr-rem-tool",
                "session-b",
                reminder_b_time + 1,
                Reminder {
                    usage_id: "usage-rem-tool",
                    owner_id: "reminder-tool",
                    usage_family: "tool",
                    ..reminder_b
                },
            ),
            reminder_attribution(
                "attr-rem-unreported",
                "session-b",
                reminder_b_time + 2,
                Reminder {
                    usage_id: "usage-rem-unreported",
                    owner_id: "reminder-unreported",
                    reported: false,
                    ..reminder_b
                },
            ),
        ],
    );
    write_lines(
        &sessions.join("2099/02/01/session-b-fork/session.jsonl"),
        &[
            b1,
            reminder_attribution("attr-rem-b-copy", "session-b", reminder_b_time, reminder_b),
        ],
    );

    let never_read = model_completed(
        "rec-never",
        "session-a",
        a1_time,
        "muse-wonder-1.0",
        Usage {
            input: 1_000_000,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            reasoning: 0,
        },
    );
    write_lines(
        &sessions.join(".msp-view-g6/session-a/session.jsonl"),
        std::slice::from_ref(&never_read),
    );
    write_lines(
        &sessions.join("2099/01/02/session-a/tool-outputs/session.jsonl"),
        std::slice::from_ref(&never_read),
    );
    fs::write(
        sessions.join(".msp-view-g6/session-a/journal-0.bin"),
        [0_u8, 159, 146, 150],
    )
    .expect("write fixture journal");
}

fn write_lines(path: &Path, lines: &[String]) {
    fs::create_dir_all(path.parent().expect("fixture file has a parent"))
        .expect("create fixture directory");
    let mut content = lines.join("\n");
    content.push('\n');
    fs::write(path, content).expect("write fixture session log");
}
