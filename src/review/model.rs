//! A PR as the Review view sees it, and parsing of the review queries' responses.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{self, Nodes, Pr, PrState, ReviewState, nodes_of as items};

/// How many description lines the details show.
const DESCRIPTION_LINES: usize = 5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewPr {
    pub pr: Pr,
    pub created_at: DateTime<Utc>,
    /// Latest READY_FOR_REVIEW_EVENT, else created_at.
    pub ready_at: DateTime<Utc>,
    /// First 5 non-empty lines of the body, HTML comments removed, trimmed.
    pub description: Vec<String>,
    /// Team slugs and user logins whose review was requested as code owner.
    pub code_owner_requests: Vec<String>,
    pub head_committed_at: Option<DateTime<Utc>>,
    /// Each reviewer's latest approval or change request (`latestOpinionatedReviews`): unlike
    /// the reviewers list, a later comment doesn't replace it.
    #[serde(default)]
    pub opinions: Vec<Opinion>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Opinion {
    pub login: String,
    pub state: ReviewState,
    pub at: Option<DateTime<Utc>>,
}

/// One page of the review search.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchPage {
    pub viewer: String,
    /// (PR node id, owner/name)
    pub hits: Vec<(String, String)>,
    /// The cursor for the next page, when there is one.
    pub next: Option<String>,
}

impl ReviewPr {
    /// Open, ready, and by someone else who is not dependabot.
    pub fn included(&self, viewer: &str) -> bool {
        let p = &self.pr;
        p.state == PrState::Open
            && !p.is_draft
            && p.author != viewer
            && !p.author.starts_with("dependabot")
    }

    /// GitHub's decision; where reviews aren't required (no decision), any approving review.
    pub fn approved(&self) -> bool {
        match self.pr.approved {
            Some(approved) => approved,
            None => self
                .opinions
                .iter()
                .any(|o| o.state == ReviewState::Approved),
        }
    }

    /// The viewer requested changes and no commit landed after that review.
    pub fn waiting_on_author(&self, viewer: &str) -> bool {
        self.opinions.iter().any(|r| {
            r.login == viewer
                && r.state == ReviewState::ChangesRequested
                && r.at
                    .is_some_and(|at| self.head_committed_at.is_none_or(|c| c <= at))
        })
    }

    /// One of the viewer's `teams` (or the viewer) is requested as code owner.
    pub fn code_owner(&self, viewer: &str, teams: &[String]) -> bool {
        self.code_owner_requests
            .iter()
            .any(|who| who == viewer || teams.contains(who))
    }

    /// The repo's owner, where its teams live.
    pub fn org(&self) -> &str {
        self.pr.repo.split('/').next().unwrap_or(&self.pr.repo)
    }
}

/// The body without HTML comments, as trimmed non-empty lines, at most `DESCRIPTION_LINES`.
pub fn description_lines(body: Option<&str>) -> Vec<String> {
    let mut text = body.unwrap_or("");
    let mut visible = String::new();
    while let Some(start) = text.find("<!--") {
        visible.push_str(&text[..start]);
        // An unclosed comment runs to the end.
        text = text[start..]
            .find("-->")
            .map_or("", |end| &text[start + end + 3..]);
    }
    visible.push_str(text);
    visible
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(DESCRIPTION_LINES)
        .map(String::from)
        .collect()
}

// ---- raw JSON shapes ----

#[derive(Deserialize)]
struct RawLogin {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRepo {
    name_with_owner: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCreated {
    created_at: DateTime<Utc>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawRequest {
    #[serde(default)]
    as_code_owner: bool,
    requested_reviewer: Option<RawRequested>,
}

#[derive(Deserialize)]
struct RawRequested {
    login: Option<String>,
    slug: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawCommitDate {
    committed_date: DateTime<Utc>,
}

#[derive(Deserialize)]
struct RawCommit {
    commit: RawCommitDate,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawOpinion {
    state: String,
    submitted_at: Option<DateTime<Utc>>,
    author: Option<RawLogin>,
}

/// The fields the review fragment adds to `prFields`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawExtras {
    created_at: DateTime<Utc>,
    body: Option<String>,
    timeline_items: Option<Nodes<RawCreated>>,
    review_requests: Option<Nodes<RawRequest>>,
    commits: Option<Nodes<RawCommit>>,
    latest_opinionated_reviews: Option<Nodes<RawOpinion>>,
}

fn review_pr(v: Value) -> Result<ReviewPr, String> {
    let pr = model::pr_from_value(v.clone())?;
    let x: RawExtras = serde_json::from_value(v).map_err(|e| e.to_string())?;
    let ready_at = items(x.timeline_items)
        .into_iter()
        .map(|e| e.created_at)
        .max()
        .unwrap_or(x.created_at);
    Ok(ReviewPr {
        pr,
        created_at: x.created_at,
        ready_at,
        description: description_lines(x.body.as_deref()),
        code_owner_requests: items(x.review_requests)
            .into_iter()
            .filter(|r| r.as_code_owner)
            .filter_map(|r| r.requested_reviewer.and_then(|w| w.slug.or(w.login)))
            .collect(),
        head_committed_at: items(x.commits).last().map(|c| c.commit.committed_date),
        opinions: items(x.latest_opinionated_reviews)
            .into_iter()
            .filter_map(|r| {
                let state = match r.state.as_str() {
                    "APPROVED" => ReviewState::Approved,
                    "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
                    _ => return None,
                };
                Some(Opinion {
                    login: r.author?.login,
                    state,
                    at: r.submitted_at,
                })
            })
            .collect(),
    })
}

pub fn parse_search_page(json: &str) -> Result<SearchPage, String> {
    #[derive(Deserialize)]
    struct RawHit {
        id: String,
        repository: RawRepo,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawPageInfo {
        has_next_page: bool,
        end_cursor: Option<String>,
    }
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RawSearch {
        page_info: RawPageInfo,
        nodes: Vec<Option<Value>>,
    }
    #[derive(Deserialize)]
    struct Raw {
        viewer: RawLogin,
        search: RawSearch,
    }
    let raw: Raw = serde_json::from_value(model::data_of(json)?)
        .map_err(|e| format!("bad search response: {e}"))?;
    // Non-PR nodes come back as `{}`.
    let hits = raw
        .search
        .nodes
        .into_iter()
        .flatten()
        .filter_map(|n| serde_json::from_value::<RawHit>(n).ok())
        .map(|h| (h.id, h.repository.name_with_owner))
        .collect();
    let page = raw.search.page_info;
    Ok(SearchPage {
        viewer: raw.viewer.login,
        hits,
        next: page.end_cursor.filter(|_| page.has_next_page),
    })
}

/// The viewer's team slugs in one org; an org the viewer can't see has none.
pub fn parse_teams(json: &str) -> Result<Vec<String>, String> {
    #[derive(Deserialize)]
    struct RawSlug {
        slug: String,
    }
    #[derive(Deserialize)]
    struct RawOrg {
        teams: Option<Nodes<RawSlug>>,
    }
    #[derive(Deserialize)]
    struct Raw {
        organization: Option<RawOrg>,
    }
    let raw: Raw = serde_json::from_value(model::data_of(json)?)
        .map_err(|e| format!("bad teams response: {e}"))?;
    Ok(items(raw.organization.and_then(|o| o.teams))
        .into_iter()
        .map(|t| t.slug)
        .collect())
}

/// Every `pN` alias under `data`, in numeric order. Null aliases (PR not visible) are skipped.
pub fn parse_review_detail(json: &str) -> Result<Vec<ReviewPr>, String> {
    let Value::Object(data) = model::data_of(json)? else {
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
        .map(|(i, v)| review_pr(v).map_err(|e| format!("bad PR in alias p{i}: {e}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DETAIL: &str = include_str!("../../tests/fixtures/review-detail.json");

    fn get(prs: &[ReviewPr], n: u32) -> &ReviewPr {
        prs.iter().find(|p| p.pr.number == n).unwrap()
    }

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn ready_time_uses_latest_ready_event() {
        let prs = parse_review_detail(DETAIL).unwrap();
        assert_eq!(get(&prs, 1312).ready_at, at("2026-10-18T15:00:00Z"));
        assert_eq!(get(&prs, 1298).ready_at, get(&prs, 1298).created_at);
        assert_eq!(get(&prs, 1298).created_at, at("2026-10-16T15:00:00Z"));
    }

    #[test]
    fn inclusion_rules() {
        let prs = parse_review_detail(DETAIL).unwrap();
        let shown: Vec<u32> = prs
            .iter()
            .filter(|p| p.included("jayhogan"))
            .map(|p| p.pr.number)
            .collect();
        for n in [1330, 1331, 1332] {
            assert!(!shown.contains(&n), "{n}");
        }
        assert!(shown.contains(&1333));
        assert_eq!(shown.len(), 11);
        assert_eq!(get(&prs, 1333).pr.author, "ghost");
    }

    #[test]
    fn approval_rule_with_null_decision() {
        let prs = parse_review_detail(DETAIL).unwrap();
        assert!(get(&prs, 421).approved());
        assert!(get(&prs, 431).approved());
        assert!(!get(&prs, 430).approved());
        assert!(!get(&prs, 1298).approved());
    }

    #[test]
    fn waiting_on_author_only_without_newer_commit() {
        let prs = parse_review_detail(DETAIL).unwrap();
        assert!(get(&prs, 91).waiting_on_author("jayhogan"));
        assert!(!get(&prs, 95).waiting_on_author("jayhogan"));
        assert!(!get(&prs, 87).waiting_on_author("jayhogan"));
    }

    #[test]
    fn code_owner_needs_a_viewer_team() {
        let prs = parse_review_detail(DETAIL).unwrap();
        let teams = vec!["billing".to_string(), "platform".to_string()];
        assert!(get(&prs, 1298).code_owner("jayhogan", &teams));
        assert!(!get(&prs, 418).code_owner("jayhogan", &teams));
        assert!(!get(&prs, 1298).code_owner("jayhogan", &[]));
        // priya-s is requested on #1312, but not as code owner.
        assert!(!get(&prs, 1312).code_owner("priya-s", &[]));
        assert_eq!(get(&prs, 1298).org(), "heard");
    }

    #[test]
    fn description_from_fixture() {
        let prs = parse_review_detail(DETAIL).unwrap();
        assert_eq!(
            get(&prs, 1312).description,
            [
                "Adds proration when a customer moves plans mid-cycle.",
                "Uses Stripe's preview invoice to compute the credit,",
                "then applies it on the next invoice.",
                "Behind the `billing.proration` flag.",
                "Closes CORE-2210",
            ]
        );
        assert!(get(&prs, 430).description.is_empty());
    }

    #[test]
    fn description_trims_comments_and_caps_at_five() {
        let d = description_lines(Some(
            "<!-- template\nhint -->\n\nOne\r\n  Two  \n\nThree\nFour\nFive\nSix",
        ));
        assert_eq!(d, ["One", "Two", "Three", "Four", "Five"]);
    }

    #[test]
    fn description_empty_and_comment_only() {
        assert!(description_lines(None).is_empty());
        assert!(description_lines(Some("")).is_empty());
        assert!(description_lines(Some("<!-- only -->\n")).is_empty());
        // An unclosed comment hides the rest, as GitHub renders it.
        assert!(description_lines(Some("<!-- open\nText")).is_empty());
    }

    #[test]
    fn parses_search_page_and_teams() {
        let s = parse_search_page(include_str!("../../tests/fixtures/review-search.json")).unwrap();
        assert_eq!(s.viewer, "jayhogan");
        assert_eq!(s.hits.len(), 14);
        assert_eq!(
            s.hits[0],
            ("PR_1298".to_string(), "heard/heard-ts".to_string())
        );
        assert_eq!(s.next, None);
        assert_eq!(
            parse_teams(include_str!("../../tests/fixtures/review-teams.json")).unwrap(),
            ["billing", "platform"]
        );
        assert_eq!(
            parse_teams(r#"{"data":{"organization":null}}"#).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn search_page_with_next_cursor() {
        let body = r#"{"data":{"viewer":{"login":"v"},"search":{"pageInfo":{"hasNextPage":true,"endCursor":"C1"},"nodes":[]}}}"#;
        assert_eq!(parse_search_page(body).unwrap().next.as_deref(), Some("C1"));
    }
}
