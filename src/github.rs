//! GitHub access through the `gh` CLI (GraphQL only, read-only).

use std::collections::HashSet;
use std::io::{ErrorKind, Read, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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

/// A gh call that has not finished by then is killed, so a stalled gh cannot hold the fetch
/// lock (and every copy's refresh) forever.
const GH_TIMEOUT: Duration = Duration::from_secs(30);

impl Gh for GhCli {
    fn graphql(&self, query: &str, variables: &Value) -> Result<String, GhError> {
        let body = json!({"query": query, "variables": variables}).to_string();
        run("gh", &["api", "graphql", "--input", "-"], body, GH_TIMEOUT)
    }
}

/// Runs `program` with `input` on stdin and returns its stdout through `finish`. The pipes
/// are fed and drained on helper threads so a full pipe cannot deadlock the wait, and a
/// program still running after `timeout` is killed.
fn run(program: &str, args: &[&str], input: String, timeout: Duration) -> Result<String, GhError> {
    let spawn_err = |e: std::io::Error| classify("", Some(e.kind()));
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(spawn_err)?;
    let mut stdin = child.stdin.take().expect("stdin is piped");
    // Write errors (e.g. BrokenPipe when gh exits early) are judged after gh's exit status.
    let writer = thread::spawn(move || stdin.write_all(input.as_bytes()));
    let stdout = drain(child.stdout.take().expect("stdout is piped"));
    let stderr = drain(child.stderr.take().expect("stderr is piped"));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(GhError::Other(format!(
                    "gh timed out after {}s",
                    timeout.as_secs()
                )));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(spawn_err(e)),
        }
    };
    let written = writer.join().unwrap_or(Ok(()));
    let text = |h: thread::JoinHandle<Vec<u8>>| {
        String::from_utf8_lossy(&h.join().unwrap_or_default()).into_owned()
    };
    finish(written, status.success(), &text(stdout), &text(stderr))
}

/// Reads `pipe` to its end on a helper thread.
fn drain(mut pipe: impl Read + Send + 'static) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        buf
    })
}

/// Combines the stdin write result with gh's exit status. A non-zero exit is classified from
/// stderr (it is the real cause even when the write hit a broken pipe), unless stdout still
/// carries a `data` object: `gh api graphql` exits 1 whenever the body has `errors`, even when
/// only one alias failed, so that body goes on to the SAML check and the parser. A broken
/// pipe on a successful exit is harmless; any other write failure is reported on its own.
fn finish(
    written: std::io::Result<()>,
    success: bool,
    stdout: &str,
    stderr: &str,
) -> Result<String, GhError> {
    if !success {
        let partial = serde_json::from_str::<Value>(stdout).is_ok_and(|v| v["data"].is_object());
        return if partial {
            Ok(stdout.to_string())
        } else {
            Err(classify(stderr, None))
        };
    }
    match written {
        Err(e) if e.kind() != ErrorKind::BrokenPipe => {
            Err(GhError::Other(format!("could not send request to gh: {e}")))
        }
        _ => Ok(stdout.to_string()),
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
pub(crate) fn check_saml(body: &str) -> Result<(), GhError> {
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

/// The CI rollup for alias `n`; `isRequired` needs the PR's own id, so it can't sit in a
/// fragment.
pub(crate) fn rollup_fields(n: usize) -> String {
    format!(
        "statusCheckRollup {{ state contexts(first: 100) {{ nodes {{ __typename \
         ... on CheckRun {{ name status conclusion completedAt isRequired(pullRequestId: $id{n}) }} \
         ... on StatusContext {{ name: context state completedAt: createdAt isRequired(pullRequestId: $id{n}) }} \
         }} }} }}"
    )
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
        let rollup = rollup_fields(n);
        aliases.push_str(&format!(
            "  p{n}: node(id: $id{n}) {{ ...prFields ... on PullRequest {{ {rollup}{base} }} }}\n"
        ));
    }
    let query = format!(
        "query PrPulseDetail({}) {{\n{aliases}  rateLimit {{ cost remaining }}\n}}\n{PR_FIELDS}",
        decls.join(", ")
    );
    (query, Value::Object(vars))
}

pub fn detail(gh: &dyn Gh, targets: &[Target]) -> Result<Vec<Pr>, GhError> {
    if targets.is_empty() {
        return Ok(Vec::new()); // an empty query would be invalid GraphQL
    }
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
            let kind = [
                ("PrPulseReviewSearch", "review-search"),
                ("PrPulseReviewTeams", "review-teams"),
                ("PrPulseReviewDetail", "review-detail"),
                ("PrPulseSearch", "search"),
            ]
            .into_iter()
            .find(|(name, _)| query.contains(name))
            .map_or("detail", |(_, kind)| kind);
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
    fn detail_with_no_targets_skips_gh() {
        let gh = FakeGh::new();
        assert_eq!(detail(&gh, &[]).unwrap(), Vec::new());
        assert!(gh.calls.borrow().is_empty());
    }

    #[test]
    fn finish_prefers_gh_failure_over_broken_pipe() {
        let pipe = || Err(std::io::Error::from(ErrorKind::BrokenPipe));
        assert_eq!(
            finish(pipe(), false, "", "HTTP 401: Bad credentials"),
            Err(GhError::LoginExpired)
        );
        assert_eq!(
            finish(pipe(), false, "", "SAML enforcement"),
            Err(GhError::SamlSso)
        );
        assert_eq!(finish(pipe(), true, "ok", ""), Ok("ok".into()));
        assert_eq!(finish(Ok(()), true, "ok", ""), Ok("ok".into()));
        let other = Err(std::io::Error::from(ErrorKind::PermissionDenied));
        assert!(
            matches!(finish(other, true, "ok", ""), Err(GhError::Other(m)) if m.contains("send request"))
        );
    }

    /// `gh api graphql` exits 1 whenever the body has `errors`, even beside good `data`.
    fn partial_body(error: &str) -> String {
        let mut v: Value = serde_json::from_str(DETAIL).unwrap();
        v["data"]["p99"] = Value::Null;
        v["errors"] = json!([{"type": "NOT_FOUND", "path": ["p99"], "message": error}]);
        v.to_string()
    }

    /// gh exits 1 with `data: null` when GitHub rejects the search; its stderr names why.
    #[test]
    fn finish_reports_a_rejected_search_from_stderr() {
        let body = r#"{"data":null,"errors":[{"message":"cannot be searched"}]}"#;
        let stderr = "gh: The listed users and repositories cannot be searched either because the resources do not exist or you do not have permission to view them.";
        let err = finish(Ok(()), false, body, stderr).unwrap_err();
        assert!(err.message().contains("cannot be searched"), "{err:?}");
    }

    #[test]
    fn finish_keeps_partial_data_on_failure() {
        let body = partial_body("Could not resolve to a node with the global id of 'X'.");
        let stderr = "gh: Could not resolve to a node with the global id of 'X'.";
        assert_eq!(finish(Ok(()), false, &body, stderr), Ok(body.clone()));
        let gh = FakeGh::new().with("detail", finish(Ok(()), false, &body, stderr));
        assert_eq!(detail(&gh, &[target("PR_1284", None)]).unwrap().len(), 9);
        // SAML among the errors still wins.
        let saml = partial_body("Resource protected by organization SAML enforcement.");
        let gh = FakeGh::new().with("detail", finish(Ok(()), false, &saml, "gh: SAML"));
        assert_eq!(
            detail(&gh, &[target("PR_1284", None)]).unwrap_err(),
            GhError::SamlSso
        );
        // No usable data: stderr decides as before.
        let null = r#"{"data":null,"errors":[{"message":"boom"}]}"#;
        assert_eq!(
            finish(Ok(()), false, null, "HTTP 401: Bad credentials"),
            Err(GhError::LoginExpired)
        );
        assert_eq!(
            finish(Ok(()), false, "not json", "boom"),
            Err(GhError::Other("boom".into()))
        );
    }

    #[test]
    fn missing_entries_found() {
        let all = model::parse_detail(DETAIL).unwrap();
        assert!(missing_stack_entries(&all).is_empty());
        let target_id = all.iter().find(|p| p.number == 1295).unwrap().id.clone();
        let rest: Vec<Pr> = all.into_iter().filter(|p| p.number != 1295).collect();
        assert_eq!(missing_stack_entries(&rest), vec![target_id]);
    }

    #[test]
    fn run_passes_stdin_and_classifies_failures() {
        let secs = Duration::from_secs(5);
        let echo = run("sh", &["-c", "cat; echo noise >&2"], "hello".into(), secs);
        assert_eq!(echo, Ok("hello".into()));
        let expired = "cat >/dev/null; echo 'HTTP 401: Bad credentials' >&2; exit 1";
        assert_eq!(
            run("sh", &["-c", expired], "x".into(), secs),
            Err(GhError::LoginExpired)
        );
        assert_eq!(
            run("/nonexistent/gh-pr-pulse", &[], String::new(), secs),
            Err(GhError::NotInstalled)
        );
    }

    #[test]
    fn run_kills_a_stalled_command() {
        let start = std::time::Instant::now();
        let out = run("sleep", &["5"], String::new(), Duration::from_secs(1));
        assert_eq!(out, Err(GhError::Other("gh timed out after 1s".into())));
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "{:?}",
            start.elapsed()
        );
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
