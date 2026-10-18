//! PR model and parsing of the GitHub GraphQL responses (search and detail).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckOutcome {
    Pass,
    Fail,
    Running,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub required: bool,
    pub outcome: CheckOutcome,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CiState {
    Passing,
    Failing,
    Running,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Ci {
    pub state: CiState,
    pub counted: u32,
    pub passed: u32,
    pub running: u32,
    /// Counted checks that failed, oldest failure first.
    pub failing: Vec<Check>,
    /// Empty when falling back to all checks.
    pub optional_failing: Vec<Check>,
    pub passing_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
    Pending,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reviewer {
    pub login: String,
    pub is_bot: bool,
    pub state: ReviewState,
    pub unresolved_threads: u32,
    pub at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MergeState {
    Clean,
    HasHooks,
    Unstable,
    Blocked,
    Behind,
    Dirty,
    Draft,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrState {
    Open,
    Merged,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackRef {
    pub id: String,
    pub number: u32,
    pub position: u32,
    pub size: u32,
    /// (position, PR node id)
    pub entries: Vec<(u32, String)>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pr {
    pub id: String,
    /// owner/name
    pub repo: String,
    pub number: u32,
    pub title: String,
    pub url: String,
    pub author: String,
    pub assignees: Vec<String>,
    pub is_draft: bool,
    pub state: PrState,
    pub closed_at: Option<DateTime<Utc>>,
    pub head_ref: String,
    pub head_oid: String,
    pub base_ref: String,
    pub merge: MergeState,
    /// None = reviews not required.
    pub approved: Option<bool>,
    pub changes_requested: bool,
    pub behind_by: Option<u32>,
    pub stack: Option<StackRef>,
    pub ci: Ci,
    pub reviewers: Vec<Reviewer>,
    pub unresolved_threads: u32,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub id: String,
    pub repo: String,
    pub head_ref: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub viewer: String,
    pub authored: Vec<SearchHit>,
    pub assigned: Vec<SearchHit>,
}

impl Ci {
    /// Required checks are counted when at least one is required, otherwise all checks.
    /// Skipped checks are never counted.
    pub fn from_checks(checks: Vec<Check>) -> Ci {
        let any_required = checks.iter().any(|c| c.required);
        let (counted, rest): (Vec<Check>, Vec<Check>) = checks
            .into_iter()
            .filter(|c| c.outcome != CheckOutcome::Skipped)
            .partition(|c| !any_required || c.required);
        let count = |o| counted.iter().filter(|c| c.outcome == o).count() as u32;
        let (passed, running) = (count(CheckOutcome::Pass), count(CheckOutcome::Running));
        let mut failing: Vec<Check> = counted
            .iter()
            .filter(|c| c.outcome == CheckOutcome::Fail)
            .cloned()
            .collect();
        failing.sort_by_key(|c| c.completed_at);
        let mut optional_failing: Vec<Check> = rest
            .into_iter()
            .filter(|c| c.outcome == CheckOutcome::Fail)
            .collect();
        optional_failing.sort_by_key(|c| c.completed_at);
        let state = if counted.is_empty() {
            CiState::None
        } else if !failing.is_empty() {
            CiState::Failing
        } else if running > 0 {
            CiState::Running
        } else {
            CiState::Passing
        };
        Ci {
            state,
            counted: counted.len() as u32,
            passed,
            running,
            failing,
            optional_failing,
            passing_names: counted
                .iter()
                .filter(|c| c.outcome == CheckOutcome::Pass)
                .map(|c| c.name.clone())
                .collect(),
        }
    }
}

// ---- raw JSON shapes ----

#[derive(Deserialize)]
struct Nodes<T> {
    #[serde(default = "Vec::new")]
    nodes: Vec<Option<T>>,
}

impl<T> Nodes<T> {
    fn items(self) -> Vec<T> {
        self.nodes.into_iter().flatten().collect()
    }
}

fn nodes_of<T>(n: Option<Nodes<T>>) -> Vec<T> {
    n.map(Nodes::items).unwrap_or_default()
}

#[derive(Deserialize)]
struct RawActor {
    login: String,
    #[serde(rename = "__typename")]
    typename: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRepo {
    name_with_owner: String,
}

#[derive(Deserialize)]
struct RawLogin {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCompare {
    behind_by: Option<u32>,
}

#[derive(Deserialize)]
struct RawBaseRef {
    compare: Option<RawCompare>,
}

#[derive(Deserialize)]
struct RawStackPr {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawStackEntryNode {
    position: u32,
    pull_request: RawStackPr,
}

#[derive(Deserialize)]
struct RawStack {
    id: String,
    number: u32,
    entries: Option<Nodes<RawStackEntryNode>>,
}

#[derive(Deserialize)]
struct RawStackEntry {
    position: u32,
    stack: RawStack,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawContext {
    #[serde(rename = "__typename")]
    typename: String,
    name: Option<String>,
    status: Option<String>,
    conclusion: Option<String>,
    state: Option<String>,
    completed_at: Option<DateTime<Utc>>,
    is_required: Option<bool>,
}

#[derive(Deserialize)]
struct RawRollup {
    contexts: Option<Nodes<RawContext>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawThread {
    is_resolved: bool,
    comments: Option<Nodes<RawComment>>,
}

#[derive(Deserialize)]
struct RawComment {
    author: Option<RawActor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawReview {
    state: String,
    submitted_at: Option<DateTime<Utc>>,
    author: Option<RawActor>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRequest {
    requested_reviewer: Option<RawRequested>,
}

#[derive(Deserialize)]
struct RawRequested {
    #[serde(rename = "__typename")]
    typename: Option<String>,
    login: Option<String>,
    slug: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawPr {
    id: String,
    number: u32,
    title: String,
    url: String,
    state: String,
    #[serde(default)]
    is_draft: bool,
    closed_at: Option<DateTime<Utc>>,
    updated_at: DateTime<Utc>,
    author: Option<RawLogin>,
    assignees: Option<Nodes<RawLogin>>,
    repository: RawRepo,
    head_ref_name: String,
    head_ref_oid: String,
    base_ref_name: String,
    merge_state_status: Option<String>,
    review_decision: Option<String>,
    base_ref: Option<RawBaseRef>,
    stack_entry: Option<RawStackEntry>,
    status_check_rollup: Option<RawRollup>,
    review_threads: Option<Nodes<RawThread>>,
    latest_reviews: Option<Nodes<RawReview>>,
    review_requests: Option<Nodes<RawRequest>>,
}

fn convert_check(c: RawContext) -> Check {
    let is_check_run = c.typename == "CheckRun";
    let outcome = if is_check_run {
        match (c.status.as_deref(), c.conclusion.as_deref()) {
            (Some("COMPLETED"), Some("SUCCESS")) => CheckOutcome::Pass,
            (Some("COMPLETED"), Some("SKIPPED" | "NEUTRAL")) => CheckOutcome::Skipped,
            (Some("COMPLETED"), _) => CheckOutcome::Fail,
            _ => CheckOutcome::Running,
        }
    } else {
        match c.state.as_deref() {
            Some("SUCCESS") => CheckOutcome::Pass,
            Some("FAILURE" | "ERROR") => CheckOutcome::Fail,
            _ => CheckOutcome::Running,
        }
    };
    Check {
        name: c.name.unwrap_or_default(),
        required: c.is_required.unwrap_or(false),
        outcome,
        completed_at: c.completed_at,
    }
}

fn merge_state(s: Option<&str>) -> MergeState {
    match s {
        Some("CLEAN") => MergeState::Clean,
        Some("HAS_HOOKS") => MergeState::HasHooks,
        Some("UNSTABLE") => MergeState::Unstable,
        Some("BLOCKED") => MergeState::Blocked,
        Some("BEHIND") => MergeState::Behind,
        Some("DIRTY") => MergeState::Dirty,
        Some("DRAFT") => MergeState::Draft,
        _ => MergeState::Unknown,
    }
}

/// Union of latest reviews, pending requests and unresolved-thread authors.
/// A review's state wins over a request. Sorted humans first, then by urgency
/// (changes requested, pending, commented, approved); stable within a rank.
fn build_reviewers(
    reviews: Vec<RawReview>,
    requests: Vec<RawRequest>,
    threads: &[RawThread],
) -> Vec<Reviewer> {
    let mut out: Vec<Reviewer> = Vec::new();
    let new = |login: String, is_bot, state, at| Reviewer {
        login,
        is_bot,
        state,
        unresolved_threads: 0,
        at,
    };
    for r in reviews {
        let Some(a) = r.author else { continue };
        let state = match r.state.as_str() {
            "APPROVED" => ReviewState::Approved,
            "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
            "COMMENTED" => ReviewState::Commented,
            _ => ReviewState::Pending,
        };
        let is_bot = a.typename.as_deref() == Some("Bot");
        out.push(new(a.login, is_bot, state, r.submitted_at));
    }
    for r in requests {
        let Some(who) = r.requested_reviewer else {
            continue;
        };
        let Some(login) = who.slug.or(who.login) else {
            continue;
        };
        if !out.iter().any(|x| x.login == login) {
            let is_bot = who.typename.as_deref() == Some("Bot");
            out.push(new(login, is_bot, ReviewState::Pending, None));
        }
    }
    for t in threads.iter().filter(|t| !t.is_resolved) {
        let first = t
            .comments
            .as_ref()
            .and_then(|c| c.nodes.iter().flatten().next());
        let Some(a) = first.and_then(|c| c.author.as_ref()) else {
            continue;
        };
        match out.iter_mut().find(|x| x.login == a.login) {
            Some(x) => x.unresolved_threads += 1,
            None => {
                let mut r = new(
                    a.login.clone(),
                    a.typename.as_deref() == Some("Bot"),
                    ReviewState::Commented,
                    None,
                );
                r.unresolved_threads = 1;
                out.push(r);
            }
        }
    }
    let rank = |s: ReviewState| match s {
        ReviewState::ChangesRequested => 0,
        ReviewState::Pending => 1,
        ReviewState::Commented => 2,
        ReviewState::Approved => 3,
    };
    out.sort_by_key(|r| (r.is_bot, rank(r.state)));
    out
}

fn convert_pr(raw: RawPr) -> Pr {
    let checks: Vec<Check> = raw
        .status_check_rollup
        .and_then(|r| r.contexts)
        .map(Nodes::items)
        .unwrap_or_default()
        .into_iter()
        .map(convert_check)
        .collect();
    let threads = nodes_of(raw.review_threads);
    let unresolved_threads = threads.iter().filter(|t| !t.is_resolved).count() as u32;
    let reviewers = build_reviewers(
        nodes_of(raw.latest_reviews),
        nodes_of(raw.review_requests),
        &threads,
    );
    let decision = raw.review_decision.as_deref();
    let changes_requested = decision == Some("CHANGES_REQUESTED")
        || reviewers
            .iter()
            .any(|r| r.state == ReviewState::ChangesRequested);
    let stack = raw.stack_entry.map(|e| {
        let mut entries: Vec<(u32, String)> = nodes_of(e.stack.entries)
            .into_iter()
            .map(|n| (n.position, n.pull_request.id))
            .collect();
        entries.sort();
        StackRef {
            id: e.stack.id,
            number: e.stack.number,
            position: e.position,
            size: entries.len() as u32,
            entries,
        }
    });
    Pr {
        id: raw.id,
        repo: raw.repository.name_with_owner,
        number: raw.number,
        title: raw.title,
        url: raw.url,
        author: raw
            .author
            .map(|a| a.login)
            .unwrap_or_else(|| "ghost".into()),
        assignees: nodes_of(raw.assignees)
            .into_iter()
            .map(|a| a.login)
            .collect(),
        is_draft: raw.is_draft,
        state: match raw.state.as_str() {
            "MERGED" => PrState::Merged,
            "CLOSED" => PrState::Closed,
            _ => PrState::Open,
        },
        closed_at: raw.closed_at,
        head_ref: raw.head_ref_name,
        head_oid: raw.head_ref_oid,
        base_ref: raw.base_ref_name,
        merge: merge_state(raw.merge_state_status.as_deref()),
        approved: decision.map(|d| d == "APPROVED"),
        changes_requested,
        behind_by: raw
            .base_ref
            .and_then(|b| b.compare)
            .and_then(|c| c.behind_by),
        stack,
        ci: Ci::from_checks(checks),
        reviewers,
        unresolved_threads,
        updated_at: raw.updated_at,
    }
}

/// The `data` object of a response, or a readable error (including GraphQL `errors`).
fn data_of(json: &str) -> Result<Value, String> {
    let mut v: Value = serde_json::from_str(json).map_err(|e| format!("invalid JSON: {e}"))?;
    match v.get_mut("data").map(Value::take) {
        Some(d) if d.is_object() => Ok(d),
        _ => {
            let msg = v
                .pointer("/errors/0/message")
                .and_then(Value::as_str)
                .unwrap_or("response has no data");
            Err(format!("GitHub error: {msg}"))
        }
    }
}

pub fn parse_search(json: &str) -> Result<SearchResult, String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawHit {
        id: String,
        repository: RawRepo,
        head_ref_name: String,
    }
    #[derive(Deserialize)]
    struct RawSearch {
        viewer: RawLogin,
        authored: Nodes<RawHit>,
        assigned: Nodes<RawHit>,
    }
    let raw: RawSearch =
        serde_json::from_value(data_of(json)?).map_err(|e| format!("bad search response: {e}"))?;
    let hits = |n: Nodes<RawHit>| {
        n.items()
            .into_iter()
            .map(|h| SearchHit {
                id: h.id,
                repo: h.repository.name_with_owner,
                head_ref: h.head_ref_name,
            })
            .collect()
    };
    Ok(SearchResult {
        viewer: raw.viewer.login,
        authored: hits(raw.authored),
        assigned: hits(raw.assigned),
    })
}

/// Every `pN` alias under `data`, in numeric order. Null aliases (PR not visible) are skipped.
pub fn parse_detail(json: &str) -> Result<Vec<Pr>, String> {
    let Value::Object(data) = data_of(json)? else {
        unreachable!("data_of returns an object")
    };
    let mut aliases: Vec<(u32, Value)> = data
        .into_iter()
        .filter_map(|(k, v)| Some((k.strip_prefix('p')?.parse().ok()?, v)))
        .collect();
    aliases.sort_by_key(|(i, _)| *i);
    aliases
        .into_iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(i, v)| {
            serde_json::from_value::<RawPr>(v)
                .map(convert_pr)
                .map_err(|e| format!("bad PR in alias p{i}: {e}"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DETAIL: &str = include_str!("../tests/fixtures/detail.json");
    const BEFORE: &str = include_str!("../tests/fixtures/detail-before.json");
    const SEARCH: &str = include_str!("../tests/fixtures/search.json");

    fn pr(prs: &[Pr], n: u32) -> &Pr {
        prs.iter().find(|p| p.number == n).unwrap()
    }

    fn check(name: &str, required: bool, outcome: CheckOutcome) -> Check {
        Check {
            name: name.into(),
            required,
            outcome,
            completed_at: None,
        }
    }

    #[test]
    fn parses_fixture_detail() {
        let prs = parse_detail(DETAIL).unwrap();
        assert_eq!(prs.len(), 9);
        let p = pr(&prs, 1287);
        assert_eq!(p.ci.state, CiState::Failing);
        assert_eq!(p.ci.failing[0].name, "test-integration");
        assert_eq!((p.ci.passed, p.ci.counted), (8, 9));
        assert_eq!(p.repo, "heard/heard-ts");
        assert_eq!(p.merge, MergeState::Blocked);
        assert_eq!(p.approved, Some(false));
        assert!(p.changes_requested);
        assert_eq!(p.unresolved_threads, 3);
        assert_eq!(p.updated_at.to_rfc3339(), "2026-10-17T14:26:08+00:00");
        assert_eq!(pr(&prs, 1270).state, PrState::Merged);
        assert_eq!(pr(&prs, 1295).ci.running, 4);
        assert_eq!(pr(&prs, 1302).approved, None);
        assert_eq!(parse_detail(BEFORE).unwrap().len(), 9);
    }

    #[test]
    fn reviewers_merge_requests_and_reviews() {
        let prs = parse_detail(DETAIL).unwrap();
        let got: Vec<_> = pr(&prs, 1287)
            .reviewers
            .iter()
            .map(|r| (r.login.as_str(), r.state, r.unresolved_threads, r.is_bot))
            .collect();
        assert_eq!(
            got,
            [
                ("art", ReviewState::ChangesRequested, 2, false),
                ("kayla", ReviewState::Pending, 1, false),
                ("claude[bot]", ReviewState::Commented, 0, true),
            ]
        );
    }

    #[test]
    fn stack_ref_parsed() {
        let prs = parse_detail(DETAIL).unwrap();
        let s = pr(&prs, 1291).stack.as_ref().unwrap();
        assert_eq!((s.position, s.size, s.entries.len()), (3, 4, 4));
        assert_eq!(s.id, "PRS_audit");
        assert_eq!(s.entries[0], (1, "PR_1284".to_string()));
        assert!(pr(&prs, 412).stack.is_none());
    }

    #[test]
    fn optional_failure_is_not_failing() {
        let prs = parse_detail(DETAIL).unwrap();
        let p = pr(&prs, 1279);
        assert_eq!(p.ci.state, CiState::Passing);
        assert_eq!(p.ci.optional_failing[0].name, "lint-docs");
        assert_eq!(p.behind_by, Some(6));
    }

    fn with_pr_field(number_alias: &str, field: &str, value: Value) -> String {
        let mut v: Value = serde_json::from_str(DETAIL).unwrap();
        v["data"][number_alias][field] = value;
        v.to_string()
    }

    #[test]
    fn no_checks_is_none() {
        let json = with_pr_field("p0", "statusCheckRollup", Value::Null);
        let prs = parse_detail(&json).unwrap();
        assert_eq!(prs[0].ci.state, CiState::None);
    }

    #[test]
    fn no_required_checks_falls_back_to_all() {
        let ci = Ci::from_checks(vec![
            check("a", false, CheckOutcome::Pass),
            check("b", false, CheckOutcome::Fail),
        ]);
        assert_eq!(ci.state, CiState::Failing);
        assert_eq!((ci.counted, ci.passed), (2, 1));
        assert!(ci.optional_failing.is_empty());
    }

    #[test]
    fn skipped_checks_are_not_counted() {
        let ci = Ci::from_checks(vec![
            check("a", true, CheckOutcome::Pass),
            check("b", true, CheckOutcome::Skipped),
        ]);
        assert_eq!((ci.state, ci.counted), (CiState::Passing, 1));
    }

    #[test]
    fn null_compare_has_no_behind() {
        let prs = parse_detail(&with_pr_field("p0", "baseRef", Value::Null)).unwrap();
        assert_eq!(prs[0].behind_by, None);
        let json = with_pr_field(
            "p0",
            "baseRef",
            serde_json::json!({"name": "main", "compare": null}),
        );
        assert_eq!(parse_detail(&json).unwrap()[0].behind_by, None);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_detail("nope").is_err());
        assert!(parse_detail(r#"{"errors":[{"message":"Bad credentials"}]}"#).is_err());
    }

    #[test]
    fn parses_search() {
        let s = parse_search(SEARCH).unwrap();
        assert_eq!(s.viewer, "jayhogan");
        assert_eq!(s.authored.len(), 7);
        assert_eq!(
            s.assigned[0],
            SearchHit {
                id: "PR_415".into(),
                repo: "heard/docket".into(),
                head_ref: "thread-model/sidebar".into()
            }
        );
    }

    #[test]
    fn before_fixture_differs_as_specified() {
        let prs = parse_detail(BEFORE).unwrap();
        assert_eq!(pr(&prs, 1287).ci.state, CiState::Passing);
        assert!(pr(&prs, 1287).reviewers.iter().all(|r| r.login != "art"));
        assert_eq!(pr(&prs, 1284).merge, MergeState::Blocked);
        assert_eq!(pr(&prs, 1291).unresolved_threads, 0);
        assert_eq!(pr(&prs, 412).merge, MergeState::Clean);
    }
}
