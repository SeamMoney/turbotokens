mod loader;
mod report;
mod types;

use turbotokens_adapter_codex::CodexGroup;
#[cfg(test)]
use turbotokens_adapter_codex::CodexModelUsage;
use turbotokens_adapter_common::filter_loaded_entries_by_date;
use turbotokens_core::*;

mod adapter {
    pub use turbotokens_adapter_amp as amp;
    pub use turbotokens_adapter_antigravity as antigravity;
    pub use turbotokens_adapter_claude as claude;
    pub use turbotokens_adapter_codebuff as codebuff;
    pub use turbotokens_adapter_codex as codex;
    pub use turbotokens_adapter_copilot as copilot;
    pub use turbotokens_adapter_droid as droid;
    pub use turbotokens_adapter_gemini as gemini;
    pub use turbotokens_adapter_goose as goose;
    pub use turbotokens_adapter_grok as grok;
    pub use turbotokens_adapter_hermes as hermes;
    pub use turbotokens_adapter_kilo as kilo;
    pub use turbotokens_adapter_kimi as kimi;
    pub use turbotokens_adapter_muse as muse;
    pub use turbotokens_adapter_openclaw as openclaw;
    pub use turbotokens_adapter_opencode as opencode;
    pub use turbotokens_adapter_pi as pi;
    pub use turbotokens_adapter_qwen as qwen;
    pub use turbotokens_adapter_zcode as zcode;
}

use crate::{
    Result,
    cli::{AgentCommandArgs, AgentReportKind, SharedArgs},
    print_json_or_jq, wants_json,
};

/// One day of unified usage across every detected agent — the same per-day
/// aggregation the unified daily report prints, exposed for the visual
/// commands (`heatmap`, `wrapped`).
#[derive(Debug, Clone)]
pub struct DailyAggregate {
    /// `YYYY-MM-DD`.
    pub date: String,
    pub total_tokens: u64,
    pub total_cost: f64,
    /// Per-agent split of the day, sorted by agent name.
    pub agents: Vec<AgentAggregate>,
    /// Per-model split of the day, sorted by cost descending.
    pub models: Vec<ModelAggregate>,
}

#[derive(Debug, Clone)]
pub struct AgentAggregate {
    pub agent: String,
    pub total_tokens: u64,
    pub total_cost: f64,
}

#[derive(Debug, Clone)]
pub struct ModelAggregate {
    pub model: String,
    pub total_tokens: u64,
    pub total_cost: f64,
}

/// One project's unified usage, aggregated from the session report rows of
/// the agents that record real project paths.
#[derive(Debug, Clone)]
pub struct ProjectAggregate {
    pub project_path: String,
    pub total_tokens: u64,
    pub total_cost: f64,
}

/// Loads the unified per-day aggregates, honoring the shared date window.
pub fn load_daily_aggregates(shared: &SharedArgs) -> Result<Vec<DailyAggregate>> {
    let result = loader::load_rows(AgentReportKind::Daily, shared)?;
    Ok(result.rows.iter().map(daily_aggregate).collect())
}

/// Loads per-project aggregates from the unified session rows. Agents without
/// a real project path (most non-directory agents) contribute no rows.
pub fn load_project_aggregates(shared: &SharedArgs) -> Result<Vec<ProjectAggregate>> {
    let result = loader::load_rows(AgentReportKind::Session, shared)?;
    let mut projects = Vec::<ProjectAggregate>::new();
    for row in &result.rows {
        let Some(project_path) = row.project_path.as_deref() else {
            continue;
        };
        if project_path.is_empty() || row.total_tokens == 0 {
            continue;
        }
        match projects
            .iter_mut()
            .find(|project| project.project_path == project_path)
        {
            Some(project) => {
                project.total_tokens += row.total_tokens;
                project.total_cost += row.total_cost;
            }
            None => projects.push(ProjectAggregate {
                project_path: project_path.to_string(),
                total_tokens: row.total_tokens,
                total_cost: row.total_cost,
            }),
        }
    }
    projects.sort_by_key(|project| std::cmp::Reverse(project.total_tokens));
    Ok(projects)
}

fn daily_aggregate(row: &types::AllRow) -> DailyAggregate {
    let agents = row
        .agent_breakdowns
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|breakdown| AgentAggregate {
            agent: breakdown.agent.to_string(),
            total_tokens: breakdown.total_tokens,
            total_cost: breakdown.total_cost,
        })
        .collect();
    let models = row
        .model_breakdowns
        .iter()
        .map(|breakdown| ModelAggregate {
            model: breakdown.model_name.clone(),
            total_tokens: breakdown.input_tokens
                + breakdown.output_tokens
                + breakdown.cache_creation_tokens
                + breakdown.cache_read_tokens
                + breakdown.extra_total_tokens,
            total_cost: breakdown.cost,
        })
        .collect();
    DailyAggregate {
        date: row.period.clone(),
        total_tokens: row.total_tokens,
        total_cost: row.total_cost,
        agents,
        models,
    }
}

pub fn run(args: AgentCommandArgs) -> Result<()> {
    let kind = args.kind;
    let shared = args.shared;
    let include_agents = args.by_agent;
    if let Some(sections) = args.sections {
        let sections = requested_sections(kind, sections);
        let result = loader::load_sections(&sections, &shared)?;
        if wants_json(&shared) {
            return report::print_sections_report_json(
                &result.sections,
                kind,
                include_agents,
                shared.jq.as_deref(),
                shared.no_cost,
            );
        }
        for (section_kind, rows) in &result.sections {
            report::print_table(
                rows,
                *section_kind,
                &shared,
                result.detected_agents_for(*section_kind),
            )?;
        }
        return Ok(());
    }
    let result = loader::load_rows(kind, &shared)?;
    if wants_json(&shared) {
        let output = report::report_json_with_agents(&result.rows, kind, include_agents);
        return print_json_or_jq(output, shared.jq.as_deref(), shared.no_cost);
    }
    report::print_table(&result.rows, kind, &shared, &result.detected_agents)
}

fn requested_sections(
    command_kind: AgentReportKind,
    sections: Vec<AgentReportKind>,
) -> Vec<AgentReportKind> {
    let mut requested = vec![command_kind];
    for section in [
        AgentReportKind::Daily,
        AgentReportKind::Weekly,
        AgentReportKind::Monthly,
        AgentReportKind::Session,
    ] {
        if section != command_kind && sections.contains(&section) {
            requested.push(section);
        }
    }
    requested
}

#[cfg(test)]
use loader::{aggregate_rows, codex_group_row, load_agent_rows_parallel, load_rows, load_sections};
#[cfg(test)]
use report::{
    all_report_title, all_table_columns, all_table_row, report_json, report_json_with_agents,
    sections_report_json,
};
#[cfg(test)]
use types::{AgentLoadSpec, AgentRows, AllRow};

#[cfg(test)]
mod tests;
