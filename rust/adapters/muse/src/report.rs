use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::{
    BucketKind, LoadedEntry, Result, SessionAccumulator,
    cli::{AgentReportKind, WeekDay},
    summarize_by_key, summarize_summaries_by_bucket, totals_json,
};

pub(crate) fn report_from_rows(rows: &[crate::UsageSummary], kind: AgentReportKind) -> Value {
    let rows_json = rows
        .iter()
        .map(|row| {
            turbotokens_core::agent_summary_json(row, kind, kind == AgentReportKind::Session)
        })
        .collect::<Vec<_>>();
    json!({
        rows_key(kind): rows_json,
        "totals": totals_json(rows),
    })
}

pub fn summarize_entries(
    entries: &[LoadedEntry],
    kind: AgentReportKind,
) -> Result<Vec<crate::UsageSummary>> {
    match kind {
        AgentReportKind::Daily => summarize_by_key(
            entries,
            |entry| entry.date.clone(),
            |date| (date.to_string(), None),
        ),
        AgentReportKind::Monthly => {
            let daily = summarize_entries(entries, AgentReportKind::Daily)?;
            Ok(summarize_summaries_by_bucket(
                &daily,
                BucketKind::Monthly,
                WeekDay::Sunday,
            ))
        }
        AgentReportKind::Session => {
            let mut groups = BTreeMap::<String, SessionAccumulator>::new();
            for entry in entries {
                groups
                    .entry(entry.session_id.to_string())
                    .or_default()
                    .add_entry(entry);
            }
            groups
                .into_values()
                .map(SessionAccumulator::into_summary)
                .collect()
        }
        AgentReportKind::Weekly => {
            let daily = summarize_entries(entries, AgentReportKind::Daily)?;
            Ok(summarize_summaries_by_bucket(
                &daily,
                BucketKind::Weekly,
                WeekDay::Sunday,
            ))
        }
    }
}

fn rows_key(kind: AgentReportKind) -> &'static str {
    match kind {
        AgentReportKind::Daily => "daily",
        AgentReportKind::Weekly => "weekly",
        AgentReportKind::Monthly => "monthly",
        AgentReportKind::Session => "sessions",
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;
    use crate::{PricingMap, cli::SharedArgs, load_entries};
    use turbotokens_test_support::{EnvVarsGuard, fs_fixture, muse};

    fn fixture_entries() -> Vec<LoadedEntry> {
        let fixture = fs_fixture!({});
        muse::write_fixture(fixture.root());
        let _env = EnvVarsGuard::set_many([
            (
                "MUSE_DATA_DIR",
                Some(fixture.root().as_os_str().to_os_string()),
            ),
            ("XDG_DATA_HOME", None::<OsString>),
        ]);
        let shared = SharedArgs {
            timezone: Some("UTC".to_string()),
            offline: true,
            single_thread: true,
            ..SharedArgs::default()
        };
        load_entries(&shared, &PricingMap::load_embedded()).unwrap()
    }

    #[test]
    fn reports_daily_and_session_totals() {
        let entries = fixture_entries();

        let daily = summarize_entries(&entries, AgentReportKind::Daily).unwrap();
        let report = report_from_rows(&daily, AgentReportKind::Daily);
        assert_eq!(report["daily"][0]["date"], "2099-01-02");
        assert_eq!(report["daily"][0]["totalTokens"], 110);
        assert_eq!(report["daily"][1]["date"], "2099-01-15");
        assert_eq!(report["daily"][1]["totalTokens"], 275);
        assert_eq!(report["daily"][2]["totalTokens"], 1_100);
        assert_eq!(report["totals"]["totalTokens"], muse::FIXTURE_TOTAL_TOKENS);
        assert_eq!(report["totals"]["totalCost"], 0.0);

        let sessions = summarize_entries(&entries, AgentReportKind::Session).unwrap();
        let report = report_from_rows(&sessions, AgentReportKind::Session);
        assert_eq!(report["sessions"][0]["sessionId"], "child-a");
        assert_eq!(report["sessions"][1]["sessionId"], "session-a");
        assert_eq!(report["sessions"][1]["projectPath"], "/workspace/project-a");
        assert_eq!(report["sessions"][1]["totalTokens"], 330);
        assert_eq!(report["sessions"][2]["sessionId"], "session-b");
        assert_eq!(report["totals"]["totalTokens"], muse::FIXTURE_TOTAL_TOKENS);
    }

    #[test]
    fn snapshots_focused_muse_json_reports_for_daily_monthly_weekly_and_session() {
        let entries = fixture_entries();
        let daily = summarize_entries(&entries, AgentReportKind::Daily).unwrap();
        let monthly = summarize_entries(&entries, AgentReportKind::Monthly).unwrap();
        let weekly = summarize_entries(&entries, AgentReportKind::Weekly).unwrap();
        let session = summarize_entries(&entries, AgentReportKind::Session).unwrap();

        insta::assert_json_snapshot!(
            "focused_muse_daily_json",
            report_from_rows(&daily, AgentReportKind::Daily)
        );
        insta::assert_json_snapshot!(
            "focused_muse_monthly_json",
            report_from_rows(&monthly, AgentReportKind::Monthly)
        );
        insta::assert_json_snapshot!(
            "focused_muse_weekly_json",
            report_from_rows(&weekly, AgentReportKind::Weekly)
        );
        insta::assert_json_snapshot!(
            "focused_muse_session_json",
            report_from_rows(&session, AgentReportKind::Session)
        );
    }
}
