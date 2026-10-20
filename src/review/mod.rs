//! The Review view: other people's ready PRs in the configured repos, waiting on review.

pub mod app;
pub mod fetch;
pub mod github;
pub mod model;

use chrono::{DateTime, Utc};

use crate::review::fetch::ReviewCache;

const SEARCH: &str = include_str!("../../tests/fixtures/review-search.json");
const DETAIL: &str = include_str!("../../tests/fixtures/review-detail.json");
const TEAMS: &str = include_str!("../../tests/fixtures/review-teams.json");
const FIXTURE_FETCHED_AT: &str = "2026-10-20T15:00:00Z";

/// The configured repos in fixtures mode.
pub const FIXTURE_REPOS: [&str; 3] = ["heard/heard-ts", "heard/tax-engine", "heard/docket"];

/// The cache a fetch of the fixture files would write.
pub fn fixture_cache() -> Result<ReviewCache, String> {
    let search = model::parse_search_page(SEARCH)?;
    let teams = model::parse_teams(TEAMS)?;
    let fetched_at: DateTime<Utc> = FIXTURE_FETCHED_AT.parse().map_err(|e| format!("{e}"))?;
    Ok(ReviewCache {
        version: 1,
        fetched_at: Some(fetched_at),
        viewer: search.viewer,
        teams: Some([("heard".to_string(), teams)].into()),
        prs: model::parse_review_detail(DETAIL)?,
        error: None,
        error_at: None,
    })
}
