use turbotokens_adapter_common::{filter_loaded_entries_by_date, read_files_parallel};
use turbotokens_core::*;

mod loader;
mod parser;
mod paths;
mod report;

use crate::{
    PricingMap, Result, UsageTableOptions, cli::AgentCommandArgs, print_json_or_jq,
    print_usage_table_with_options, sort_summaries, wants_json,
};

pub use loader::load_entries;
pub use paths::sessions_dirs;
pub(crate) use report::report_from_rows;
pub use report::summarize_entries;

pub fn run(args: AgentCommandArgs) -> Result<()> {
    let shared = args.shared;
    let pricing = PricingMap::load_with_overrides(
        shared.offline,
        crate::log_level() != Some(0),
        shared.pricing_overrides.iter(),
    );
    let mut entries = load_entries(&shared, &pricing)?;
    filter_loaded_entries_by_date(&mut entries, &shared);
    let mut rows = summarize_entries(&entries, args.kind)?;
    sort_summaries(&mut rows, &shared.order, |row| {
        turbotokens_core::summary_period(row)
    });
    if wants_json(&shared) {
        return print_json_or_jq(
            report_from_rows(&rows, args.kind),
            shared.jq.as_deref(),
            shared.no_cost,
        );
    }
    // Muse has not recorded a cache write so far; the column appears once it does.
    let table_options = UsageTableOptions {
        show_cache_creation: rows.iter().any(|row| row.cache_creation_tokens > 0),
    };
    print_usage_table_with_options(
        "Muse Token Usage Report",
        turbotokens_core::first_column(args.kind),
        &rows,
        &shared,
        false,
        None,
        table_options,
    )?;
    Ok(())
}
