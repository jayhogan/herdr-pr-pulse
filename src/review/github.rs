//! The Review view's GitHub queries: search of the configured repos, the viewer's teams,
//! and a slim detail query.

use serde_json::{Value, json};

use crate::github::{Gh, GhError, check_saml, rollup_fields};
use crate::review::model::{self, ReviewPr};

const SEARCH_QUERY: &str = include_str!("../query/review_search.graphql");
const REVIEW_FIELDS: &str = include_str!("../query/review_fields.graphql");
const TEAMS_QUERY: &str = "query PrPulseReviewTeams($org: String!, $login: String!) { \
     organization(login: $org) { teams(first: 100, userLogins: [$login]) { nodes { slug } } } }";

/// GitHub search text for ready PRs in `repos`, leaving out the viewer's and dependabot's.
pub fn search_text(repos: &[String]) -> String {
    let mut q =
        String::from("is:pr is:open draft:false archived:false -author:@me -author:app/dependabot");
    for repo in repos {
        q.push_str(" repo:");
        q.push_str(repo);
    }
    q
}

/// Runs `query`, checks for SAML and parses the body with `parse`.
fn call<T>(
    gh: &dyn Gh,
    query: &str,
    vars: &Value,
    parse: fn(&str) -> Result<T, String>,
) -> Result<T, GhError> {
    let body = gh.graphql(query, vars)?;
    check_saml(&body)?;
    parse(&body).map_err(GhError::Other)
}

/// The viewer's login and every hit as (PR node id, owner/name), following every page.
pub fn search(gh: &dyn Gh, repos: &[String]) -> Result<(String, Vec<(String, String)>), GhError> {
    let q = search_text(repos);
    let mut hits = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let vars = json!({"q": q, "after": after});
        let page = call(gh, SEARCH_QUERY, &vars, model::parse_search_page)?;
        hits.extend(page.hits);
        match page.next {
            Some(next) => after = Some(next),
            None => return Ok((page.viewer, hits)),
        }
    }
}

/// The slugs of the viewer's teams in `org`.
pub fn teams(gh: &dyn Gh, org: &str, viewer: &str) -> Result<Vec<String>, GhError> {
    let vars = json!({"org": org, "login": viewer});
    call(gh, TEAMS_QUERY, &vars, model::parse_teams)
}

pub fn detail_query(ids: &[String]) -> (String, Value) {
    let mut vars = serde_json::Map::new();
    let mut decls = Vec::new();
    let mut aliases = String::new();
    for (n, id) in ids.iter().enumerate() {
        vars.insert(format!("id{n}"), json!(id));
        decls.push(format!("$id{n}: ID!"));
        let rollup = rollup_fields(n);
        aliases.push_str(&format!(
            "  p{n}: node(id: $id{n}) {{ ...reviewFields ... on PullRequest {{ {rollup} }} }}\n"
        ));
    }
    let query = format!(
        "query PrPulseReviewDetail({}) {{\n{aliases}  rateLimit {{ cost remaining }}\n}}\n{REVIEW_FIELDS}",
        decls.join(", ")
    );
    (query, Value::Object(vars))
}

pub fn detail(gh: &dyn Gh, ids: &[String]) -> Result<Vec<ReviewPr>, GhError> {
    if ids.is_empty() {
        return Ok(Vec::new()); // an empty query would be invalid GraphQL
    }
    let (query, vars) = detail_query(ids);
    call(gh, &query, &vars, model::parse_review_detail)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;
    use crate::github::fake::FakeGh;

    /// Answers each call with the next canned body, recording the variables.
    struct Pages {
        bodies: RefCell<Vec<String>>,
        calls: RefCell<Vec<Value>>,
    }

    impl Gh for Pages {
        fn graphql(&self, _query: &str, variables: &Value) -> Result<String, GhError> {
            self.calls.borrow_mut().push(variables.clone());
            Ok(self.bodies.borrow_mut().remove(0))
        }
    }

    fn page(next: Option<&str>, id: &str) -> String {
        json!({"data": {"viewer": {"login": "jayhogan"}, "search": {
            "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next},
            "nodes": [{"id": id, "repository": {"nameWithOwner": "heard/a"}}]}}})
        .to_string()
    }

    #[test]
    fn search_text_lists_each_repo() {
        assert_eq!(
            search_text(&["heard/a".into(), "heard/b".into()]),
            "is:pr is:open draft:false archived:false -author:@me -author:app/dependabot repo:heard/a repo:heard/b"
        );
    }

    #[test]
    fn search_follows_pages() {
        let gh = Pages {
            bodies: RefCell::new(vec![page(Some("C1"), "A"), page(None, "B")]),
            calls: RefCell::new(Vec::new()),
        };
        let (viewer, hits) = search(&gh, &["heard/a".into()]).unwrap();
        assert_eq!(viewer, "jayhogan");
        assert_eq!(
            hits.iter().map(|h| h.0.as_str()).collect::<Vec<_>>(),
            ["A", "B"]
        );
        let calls = gh.calls.borrow();
        assert_eq!(calls.len(), 2);
        assert!(calls[0]["after"].is_null());
        assert_eq!(calls[1]["after"], "C1");
        assert_eq!(calls[0]["q"], search_text(&["heard/a".into()]));
    }

    #[test]
    fn search_error_names_the_message() {
        let body = r#"{"data":null,"errors":[{"message":"The listed users and repositories cannot be searched"}]}"#;
        let gh = FakeGh::new().with("review-search", Ok(body.into()));
        let err = search(&gh, &["nope/missing".into()]).unwrap_err();
        assert!(err.message().contains("cannot be searched"), "{err:?}");
    }

    #[test]
    fn teams_parses_and_sends_org_and_login() {
        let body = include_str!("../../tests/fixtures/review-teams.json");
        let gh = FakeGh::new().with("review-teams", Ok(body.into()));
        assert_eq!(
            teams(&gh, "heard", "jayhogan").unwrap(),
            ["billing", "platform"]
        );
        let calls = gh.calls.borrow();
        assert_eq!(calls[0].0, "review-teams");
        assert_eq!(calls[0].1, json!({"org": "heard", "login": "jayhogan"}));
    }

    #[test]
    fn detail_query_aliases_and_fragment() {
        let (q, vars) = detail_query(&["A".into(), "B".into()]);
        assert!(q.contains("PrPulseReviewDetail"));
        assert!(q.contains("p1: node(id: $id1)"));
        assert!(q.contains("fragment reviewFields"));
        assert!(q.contains("isRequired(pullRequestId: $id1)"));
        assert!(!q.contains("stackEntry") && !q.contains("reviewThreads"));
        assert_eq!(vars["id1"], "B");
    }

    #[test]
    fn detail_parses_fixture() {
        let body = include_str!("../../tests/fixtures/review-detail.json");
        let gh = FakeGh::new().with("review-detail", Ok(body.into()));
        assert_eq!(detail(&gh, &["PR_1298".into()]).unwrap().len(), 14);
    }

    #[test]
    fn detail_with_no_ids_skips_gh() {
        let gh = FakeGh::new();
        assert!(detail(&gh, &[]).unwrap().is_empty());
        assert!(gh.calls.borrow().is_empty());
    }

    /// Runs the real queries against the configured repos with your gh login.
    #[test]
    #[ignore]
    fn live_review_search() {
        let crate::config::ReviewConfig::Repos(repos) = crate::config::load_review() else {
            panic!("configure [review] repos first")
        };
        let gh = crate::github::GhCli;
        let (viewer, hits) = search(&gh, &repos).unwrap();
        let ids: Vec<String> = hits.iter().take(5).map(|h| h.0.clone()).collect();
        let prs = detail(&gh, &ids).unwrap();
        assert_eq!(prs.len(), ids.len());
        let org = repos[0].split('/').next().unwrap();
        println!(
            "{viewer}: {} hits, teams in {org}: {:?}",
            hits.len(),
            teams(&gh, org, &viewer)
        );
    }
}
