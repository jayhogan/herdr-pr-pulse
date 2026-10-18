//! GitHub access through the `gh` CLI (GraphQL only, read-only).

use std::collections::HashSet;
use std::io::{ErrorKind, Write};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::model::{self, Pr, SearchResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GhError {
    NotInstalled,
    LoginExpired,
    SamlSso,
    Other(String),
}

impl GhError {
    pub fn message(&self) -> String {
        match self {
            GhError::NotInstalled => "gh not found".into(),
            GhError::LoginExpired => "GitHub login expired".into(),
            GhError::SamlSso => "Token not authorized for SAML SSO".into(),
            GhError::Other(s) => s.clone(),
        }
    }

    pub fn fix(&self) -> String {
        match self {
            GhError::NotInstalled => {
                "Install the GitHub CLI and make sure `gh` is on PATH.".into()
            }
            GhError::LoginExpired => "Run `gh auth login` in any pane, then press r.".into(),
            GhError::SamlSso => "Run `gh auth refresh`, then authorize the token for your org at github.com/settings/tokens, then press r.".into(),
            GhError::Other(_) => "Press r to retry.".into(),
        }
    }
}

/// Maps `gh` stderr (or a spawn failure) to an error the UI can tell apart.
pub fn classify(stderr: &str, spawn: Option<ErrorKind>) -> GhError {
    match spawn {
        Some(ErrorKind::NotFound) => return GhError::NotInstalled,
        Some(kind) => {
            return GhError::Other(format!(
                "could not run gh: {}",
                kind.to_string().to_lowercase()
            ));
        }
        None => {}
    }
    if stderr.contains("SAML") {
        GhError::SamlSso
    } else if stderr.contains("HTTP 401")
        || stderr.contains("Bad credentials")
        || stderr.contains("gh auth login")
    {
        GhError::LoginExpired
    } else {
        let first = stderr.lines().next().unwrap_or("").trim();
        GhError::Other(if first.is_empty() {
            "gh failed".into()
        } else {
            first.into()
        })
    }
}

pub trait Gh: Send {
    fn graphql(&self, query: &str, variables: &Value) -> Result<String, GhError>;
}

/// Runs `gh api graphql --input -` with `{"query","variables"}` on stdin.
pub struct GhCli;

impl Gh for GhCli {
    fn graphql(&self, query: &str, variables: &Value) -> Result<String, GhError> {
        let spawn_err = |e: std::io::Error| classify("", Some(e.kind()));
        let mut child = Command::new("gh")
            .args(["api", "graphql", "--input", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(spawn_err)?;
        let body = json!({"query": query, "variables": variables}).to_string();
        child
            .stdin
            .take()
            .expect("stdin is piped")
            .write_all(body.as_bytes())
            .map_err(spawn_err)?;
        let out = child.wait_with_output().map_err(spawn_err)?;
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        if out.status.success() {
            Ok(text(&out.stdout))
        } else {
            Err(classify(&text(&out.stderr), None))
        }
    }
}

/// A PR to fetch detail for. `head_ref` enables the behind-by comparison.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub id: String,
    pub head_ref: Option<String>,
}

const SEARCH_QUERY: &str = include_str!("query/search.graphql");
const PR_FIELDS: &str = include_str!("query/pr_fields.graphql");

/// A 200 response whose `errors` mention SAML maps to `SamlSso`.
fn check_saml(body: &str) -> Result<(), GhError> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Ok(()); // the parser reports malformed bodies
    };
    let saml = v["errors"].as_array().is_some_and(|errs| {
        errs.iter()
            .any(|e| e["message"].as_str().is_some_and(|m| m.contains("SAML")))
    });
    if saml { Err(GhError::SamlSso) } else { Ok(()) }
}

pub fn search(gh: &dyn Gh) -> Result<SearchResult, GhError> {
    let body = gh.graphql(SEARCH_QUERY, &json!({}))?;
    check_saml(&body)?;
    model::parse_search(&body).map_err(GhError::Other)
}

pub fn detail_query(targets: &[Target]) -> (String, Value) {
    let mut vars = serde_json::Map::new();
    let mut decls = Vec::new();
    let mut aliases = String::new();
    for (n, t) in targets.iter().enumerate() {
        vars.insert(format!("id{n}"), json!(t.id));
        decls.push(format!("$id{n}: ID!"));
        let base = match &t.head_ref {
            Some(head) => {
                vars.insert(format!("head{n}"), json!(head));
                decls.push(format!("$head{n}: String!"));
                format!(" baseRef {{ name compare(headRef: $head{n}) {{ behindBy }} }}")
            }
            None => String::new(),
        };
        aliases.push_str(&format!(
            "  p{n}: node(id: $id{n}) {{ ...prFields ... on PullRequest {{ \
             statusCheckRollup {{ state contexts(first: 100) {{ nodes {{ __typename \
             ... on CheckRun {{ name status conclusion completedAt isRequired(pullRequestId: $id{n}) }} \
             ... on StatusContext {{ name: context state completedAt: createdAt isRequired(pullRequestId: $id{n}) }} \
             }} }} }}{base} }} }}\n"
        ));
    }
    let query = format!(
        "query PrPulseDetail({}) {{\n{aliases}  rateLimit {{ cost remaining }}\n}}\n{PR_FIELDS}",
        decls.join(", ")
    );
    (query, Value::Object(vars))
}

pub fn detail(gh: &dyn Gh, targets: &[Target]) -> Result<Vec<Pr>, GhError> {
    let (query, vars) = detail_query(targets);
    let body = gh.graphql(&query, &vars)?;
    check_saml(&body)?;
    model::parse_detail(&body).map_err(GhError::Other)
}

/// Stack entry ids that are not among `prs` (so they still need fetching), without repeats.
pub fn missing_stack_entries(prs: &[Pr]) -> Vec<String> {
    let have: HashSet<&str> = prs.iter().map(|p| p.id.as_str()).collect();
    let mut seen = HashSet::new();
    prs.iter()
        .filter_map(|p| p.stack.as_ref())
        .flat_map(|s| s.entries.iter().map(|(_, id)| id))
        .filter(|id| !have.contains(id.as_str()) && seen.insert(id.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
pub(crate) mod fake {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use super::*;

    /// Canned responses keyed by "search" or "detail" (detected from the query text),
    /// plus a record of every call's variables.
    pub struct FakeGh {
        responses: RefCell<HashMap<&'static str, Result<String, GhError>>>,
        pub calls: RefCell<Vec<(&'static str, Value)>>,
    }

    impl FakeGh {
        pub fn new() -> Self {
            FakeGh {
                responses: RefCell::new(HashMap::new()),
                calls: RefCell::new(Vec::new()),
            }
        }

        pub fn with(self, kind: &'static str, response: Result<String, GhError>) -> Self {
            self.responses.borrow_mut().insert(kind, response);
            self
        }
    }

    impl Gh for FakeGh {
        fn graphql(&self, query: &str, variables: &Value) -> Result<String, GhError> {
            let kind = if query.contains("PrPulseSearch") {
                "search"
            } else {
                "detail"
            };
            self.calls.borrow_mut().push((kind, variables.clone()));
            self.responses
                .borrow()
                .get(kind)
                .cloned()
                .unwrap_or_else(|| Err(GhError::Other(format!("no canned {kind} response"))))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeGh;
    use super::*;

    const SEARCH: &str = include_str!("../tests/fixtures/search.json");
    const DETAIL: &str = include_str!("../tests/fixtures/detail.json");

    fn target(id: &str, head: Option<&str>) -> Target {
        Target {
            id: id.into(),
            head_ref: head.map(Into::into),
        }
    }

    #[test]
    fn classify_not_installed() {
        assert_eq!(
            classify("", Some(ErrorKind::NotFound)),
            GhError::NotInstalled
        );
    }

    #[test]
    fn classify_saml() {
        let e = "GraphQL: Resource protected by organization SAML enforcement. You must grant your token access";
        assert_eq!(classify(e, None), GhError::SamlSso);
    }

    #[test]
    fn classify_expired() {
        assert_eq!(
            classify("HTTP 401: Bad credentials", None),
            GhError::LoginExpired
        );
        assert_eq!(
            classify(
                "To get started with GitHub CLI, please run:  gh auth login",
                None
            ),
            GhError::LoginExpired
        );
    }

    #[test]
    fn classify_other() {
        assert_eq!(
            classify("boom\nsecond line", None),
            GhError::Other("boom".into())
        );
        assert_eq!(
            classify("", Some(ErrorKind::PermissionDenied)),
            GhError::Other("could not run gh: permission denied".into())
        );
    }

    #[test]
    fn messages_and_fixes() {
        assert_eq!(GhError::NotInstalled.message(), "gh not found");
        assert_eq!(
            GhError::NotInstalled.fix(),
            "Install the GitHub CLI and make sure `gh` is on PATH."
        );
        assert_eq!(GhError::LoginExpired.message(), "GitHub login expired");
        assert_eq!(
            GhError::LoginExpired.fix(),
            "Run `gh auth login` in any pane, then press r."
        );
        assert_eq!(
            GhError::SamlSso.message(),
            "Token not authorized for SAML SSO"
        );
        assert_eq!(
            GhError::SamlSso.fix(),
            "Run `gh auth refresh`, then authorize the token for your org at github.com/settings/tokens, then press r."
        );
        assert_eq!(GhError::Other("x".into()).message(), "x");
        assert_eq!(GhError::Other("x".into()).fix(), "Press r to retry.");
    }

    #[test]
    fn detail_query_aliases_each_id() {
        let (q, vars) = detail_query(&[target("A", Some("feat/a")), target("B", None)]);
        assert!(q.contains("p0: node(id: $id0)"));
        assert!(q.contains("p1: node(id: $id1)"));
        assert!(q.contains("isRequired(pullRequestId: $id1)"));
        assert!(q.contains("name: context"));
        assert!(q.contains("completedAt: createdAt"));
        assert!(q.contains("fragment prFields on PullRequest"));
        assert!(q.contains("rateLimit { cost remaining }"));
        assert!(q.contains("compare(headRef: $head0)"));
        assert!(!q.contains("$head1"));
        assert_eq!(vars, json!({"id0": "A", "id1": "B", "head0": "feat/a"}));
    }

    #[test]
    fn detail_parses_fake_response() {
        let gh = FakeGh::new().with("detail", Ok(DETAIL.into()));
        let prs = detail(&gh, &[target("PR_1284", None)]).unwrap();
        assert_eq!(prs.len(), 9);
    }

    #[test]
    fn search_parses_and_records_call() {
        let gh = FakeGh::new().with("search", Ok(SEARCH.into()));
        let s = search(&gh).unwrap();
        assert_eq!(s.viewer, "jayhogan");
        assert_eq!(gh.calls.borrow()[0].0, "search");
    }

    #[test]
    fn graphql_saml_error_in_200_body() {
        let body = r#"{"data":null,"errors":[{"message":"Resource protected by organization SAML enforcement."}]}"#;
        let gh = FakeGh::new()
            .with("search", Ok(body.into()))
            .with("detail", Ok(body.into()));
        assert_eq!(search(&gh).unwrap_err(), GhError::SamlSso);
        assert_eq!(
            detail(&gh, &[target("A", None)]).unwrap_err(),
            GhError::SamlSso
        );
    }

    #[test]
    fn errors_propagate_and_parse_failures_are_other() {
        let gh = FakeGh::new()
            .with("search", Err(GhError::LoginExpired))
            .with("detail", Ok("not json".into()));
        assert_eq!(search(&gh).unwrap_err(), GhError::LoginExpired);
        assert!(matches!(
            detail(&gh, &[target("A", None)]),
            Err(GhError::Other(_))
        ));
    }

    #[test]
    fn missing_entries_found() {
        let all = model::parse_detail(DETAIL).unwrap();
        assert!(missing_stack_entries(&all).is_empty());
        let target_id = all.iter().find(|p| p.number == 1295).unwrap().id.clone();
        let rest: Vec<Pr> = all.into_iter().filter(|p| p.number != 1295).collect();
        assert_eq!(missing_stack_entries(&rest), vec![target_id]);
    }

    /// Live read-only smoke test: `cargo test live_ -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_search() {
        let s = search(&GhCli).expect("search");
        assert!(!s.viewer.is_empty());
        let mut seen = std::collections::HashSet::new();
        let targets: Vec<Target> = s
            .authored
            .iter()
            .chain(&s.assigned)
            .filter(|h| seen.insert(h.id.clone()))
            .take(2)
            .map(|h| target(&h.id, Some(&h.head_ref)))
            .collect();
        println!(
            "search: {} authored, {} assigned",
            s.authored.len(),
            s.assigned.len()
        );
        if targets.is_empty() {
            return;
        }
        let (q, vars) = detail_query(&targets);
        let raw = GhCli.graphql(&q, &vars).expect("detail graphql");
        let v: Value = serde_json::from_str(&raw).unwrap();
        println!(
            "detail rateLimit: {}",
            v.pointer("/data/rateLimit").unwrap()
        );
        let prs = detail(&GhCli, &targets).expect("detail");
        println!("detail: {} PRs", prs.len());
        assert!(!prs.is_empty());
    }
}
