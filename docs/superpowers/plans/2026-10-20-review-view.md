# Review View Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `herdr-pr-pulse review`, a pane listing other people's ready, unapproved PRs in configured repos, grouped by engineer, oldest-waiting first, with ★ on PRs a viewer team code-owns.

**Architecture:** A self-contained `src/review/` module tree (config-driven fetch worker with its own cache and lock, its own `ReviewApp` view model and event loop) plus one renderer `src/ui/review.rs`. It reuses the base app's parsing (`Pr`, `Ci`, `Reviewer`), `gh` runner, `fetch::decide`/`Poller`, `StateDir`, theme, cell helpers, `ago`, and the drawer's CI/Reviews sections. The Mine/Workspace `App` is not touched beyond small visibility changes.

**Tech Stack:** Rust 2024 (MSRV 1.89), ratatui 0.30, crossterm 0.29, serde/serde_json, chrono, new dependency `toml = "0.9"`; tests with insta + tempfile.

**Spec:** `docs/superpowers/specs/2026-10-20-review-view-design.md` (base spec: `docs/superpowers/specs/2026-10-17-pr-pulse-design.md`)

## Global Constraints

- CLI: `herdr-pr-pulse [workspace|mine|review] [--fixtures]`; view fixed for the process's life.
- Config: `$XDG_CONFIG_HOME/herdr-pr-pulse/config.toml`, else `~/.config/herdr-pr-pulse/config.toml`; `[review] repos = ["owner/name", …]`; read once at startup.
- State files: `review-cache.json`, `review-folds.json`, lock `review-fetch.lock`, all in the existing `StateDir` (`~/.local/state/herdr-pr-pulse/`). Cache is fresh for 60 s; `r` forces a fetch.
- Colors: ANSI 16 via `ui::theme` only; never blue or magenta; every color paired with a glyph or word. ★ is `theme::yellow()`; waiting-on-author rows are `theme::dim()` with `↩`.
- Widths: `ui::rows::density` tiers — Wide ≥120, Medium 80–119, Narrow 40–79, Tiny <40.
- Read-only: no mutations against GitHub.
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` must pass at every commit (CI runs all three).
- Commit messages end with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

- A configured repo that doesn't exist or isn't visible makes GitHub reject the whole search ("cannot be searched"): expect the error banner with GitHub's message and the config path, not a crash or a silent empty list. → Task 4 `search_error_names_the_message`, Task 6 `error_keeps_rows_and_sets_banner`.
- A PR body that is null, empty, or only template comments: expect the details to show `no description` (dim), not a blank gap. → Task 3 `description_empty_and_comment_only`, Task 7 `review_drawer_no_description_120`.
- A PR whose author was deleted (`author: null` → `ghost`): expect it listed under `ghost`, not dropped or panicking. → Task 3 fixture #1333, Task 6 `groups_by_user_in_age_order`.
- Long titles and logins in a 34-column pane: expect clipping with `…`, no wrap or panic. → Task 7 `review_user_34` with #1312's long title.
- Toggling `a`/`g` or a refresh that removes the selected PR: expect the cursor to stay on the same PR if still listed, else clamp to the last row, never out of range. → Task 6 `cursor_survives_toggle_and_refresh`.

## Shared fixture data (used by Tasks 3–7)

`now = 2026-10-20T15:00:00Z`, viewer `jayhogan`, viewer teams in org `heard`: `billing`, `platform`. Files: `tests/fixtures/review-search.json`, `review-detail.json`, `review-teams.json`. All PRs `OPEN`; repo `heard/heard-ts` unless noted; `reviewDecision` `REVIEW_REQUIRED` unless noted.

| # | repo | author | created | ready event | notes |
|---|---|---|---|---|---|
| 1298 | heard-ts | maria-chen | 10-16T15:00 | — | review request team `billing`, `asCodeOwner: true`; CI 9/9 pass |
| 1312 | heard-ts | maria-chen | 10-10T09:00 | 10-18T15:00 | title `billing: proration for mid-cycle plan change when the customer upgrades`; body = an HTML comment block, then 7 non-empty lines; team `billing` code owner; dan-o COMMENTED; user priya-s requested |
| 1320 | heard-ts | maria-chen | 10-20T10:00 | — | — |
| 87 | tax-engine | dan-o | 10-17T15:00 | — | — |
| 95 | tax-engine | dan-o | 10-17T20:00 | — | jayhogan CHANGES_REQUESTED 10-18T10:00; head commit 10-19T09:00 |
| 91 | tax-engine | dan-o | 10-19T15:00 | — | jayhogan CHANGES_REQUESTED 10-19T18:00; head commit 10-19T16:00 |
| 418 | docket | sam-k | 10-20T09:00 | — | team `design` (not viewer's), `asCodeOwner: true` |
| 421 | docket | sam-k | 10-19T10:00 | — | `reviewDecision: APPROVED` |
| 430 | docket | priya-s | 10-19T09:00 | — | `reviewDecision: null`, no reviews; `body: null` |
| 431 | docket | priya-s | 10-18T09:00 | — | `reviewDecision: null`, sam-k APPROVED |
| 1330 | heard-ts | jayhogan | 10-15T09:00 | — | excluded (viewer) |
| 1331 | heard-ts | dependabot (Bot) | 10-14T09:00 | — | excluded (dependabot) |
| 1332 | heard-ts | maria-chen | 10-14T09:00 | — | `isDraft: true`, excluded |
| 1333 | heard-ts | `author: null` | 10-19T12:00 | — | listed as `ghost` |

Derived (the tests assert these): not-approved list = 1298, 1312, 1320, 87, 95, 91, 418, 430, 1333 (9); approved = 421, 431. By user: maria-chen [1298, 1312, 1320], dan-o [87, 95, 91], priya-s [430], ghost [1333], sam-k [418]. By repo: heard-ts [1298, 1312, 1333, 1320], tax-engine [87, 95, 91], docket [430, 418]. ★ = 1298, 1312. ↩ = 91.

---

### Task 1: Verify the code-owner signal live, and ignore mockup files

**Files:**
- Modify: `.gitignore`

Spec §4 makes this the first step: the ★ rule depends on GitHub keeping a team's `asCodeOwner` review request after a teammate reviews on the team's behalf.

- [ ] **Step 1: Find a PR where a member of a code-owner team reviewed (commented or requested changes) without approving**

Run (substituting a repo the viewer can see): `gh pr list -R heard/heard-ts --state all --search "review:changes_requested" --limit 20 --json number,url`
Pick one whose CODEOWNERS team includes the reviewer.

- [ ] **Step 2: Query its review requests**

Run: `gh api graphql -f query='query{repository(owner:"heard",name:"heard-ts"){pullRequest(number:N){reviewRequests(first:20){nodes{asCodeOwner requestedReviewer{__typename ... on Team{slug}}}} latestReviews(first:20){nodes{state author{login}}}}}}'`
Expected for the plan to proceed: the team still appears with `asCodeOwner: true`, or it was removed. Record which in the commit message.

- [ ] **Step 3: Decide**

If the team request stays: continue with this plan. If it is removed: **stop and report to Jay** — the spec's fallback (parsing CODEOWNERS against changed files) is a different, larger plan. Do not start Task 2.

- [ ] **Step 4: Add `.superpowers/` to `.gitignore` and commit**

```bash
git add .gitignore
git commit -m "Ignore brainstorm mockups; record code-owner request check (<result>)"
```

---

### Task 2: Config file

**Files:**
- Create: `src/config.rs`
- Modify: `Cargo.toml` (add `toml = "0.9"`), `src/main.rs` (`mod config;`)

**Interfaces:**
- Produces:
  - `pub fn path(xdg: Option<OsString>, home: Option<OsString>) -> PathBuf` — `$XDG_CONFIG_HOME` (absolute only, like `store::state_home`) else `$HOME/.config`, joined with `herdr-pr-pulse/config.toml`.
  - `pub enum ReviewConfig { Missing { path: PathBuf }, Invalid { path: PathBuf, message: String }, Repos(Vec<String>) }` (derive `Debug, Clone, PartialEq`).
  - `pub fn parse_review(text: &str, path: &Path) -> ReviewConfig`
  - `pub fn load_review() -> ReviewConfig` — reads `path(env XDG_CONFIG_HOME, env HOME)`; missing file → `Missing`.

Rules: no `[review]` table or empty `repos` → `Missing`. A repo not matching `owner/name` (exactly one `/`, both sides non-empty, no spaces) → `Invalid` with message `repos: "<value>" is not owner/name`. TOML errors → `Invalid` with the parser's message. Duplicates are removed, first occurrence kept, order otherwise preserved.

- [ ] **Step 1: Write the failing tests in `src/config.rs`**

```rust
#[test] fn path_prefers_absolute_xdg() {
    assert_eq!(path(Some("/x/cfg".into()), Some("/home/j".into())), PathBuf::from("/x/cfg/herdr-pr-pulse/config.toml"));
    assert_eq!(path(Some("rel".into()), Some("/home/j".into())), PathBuf::from("/home/j/.config/herdr-pr-pulse/config.toml"));
    assert_eq!(path(None, Some("/home/j".into())), PathBuf::from("/home/j/.config/herdr-pr-pulse/config.toml"));
}
#[test] fn parses_repos_dedup_in_order() {
    let p = Path::new("/c.toml");
    assert_eq!(parse_review("[review]\nrepos = [\"heard/a\", \"heard/b\", \"heard/a\"]\n", p),
               ReviewConfig::Repos(vec!["heard/a".into(), "heard/b".into()]));
}
#[test] fn missing_table_or_empty_list_is_missing() {
    let p = Path::new("/c.toml");
    assert_eq!(parse_review("", p), ReviewConfig::Missing { path: p.into() });
    assert_eq!(parse_review("[review]\nrepos = []\n", p), ReviewConfig::Missing { path: p.into() });
}
#[test] fn bad_repo_and_bad_toml_are_invalid() {
    let p = Path::new("/c.toml");
    assert_eq!(parse_review("[review]\nrepos = [\"heard\"]\n", p),
               ReviewConfig::Invalid { path: p.into(), message: "repos: \"heard\" is not owner/name".into() });
    assert!(matches!(parse_review("[review\n", p), ReviewConfig::Invalid { .. }));
}
```

- [ ] **Step 2: Run** `cargo test config::` — Expected: FAIL (module/functions missing).
- [ ] **Step 3: Implement** with `serde::Deserialize` structs (`File { review: Option<Review> }`, `Review { #[serde(default)] repos: Vec<String> }`) and `toml::from_str`. Add `#[allow(dead_code)]` only if clippy flags `load_review` before Task 8 wires it; remove it in Task 8.
- [ ] **Step 4: Run** `cargo test config:: && cargo clippy --all-targets -- -D warnings` — Expected: PASS, no warnings.
- [ ] **Step 5: Commit** `git add Cargo.toml Cargo.lock src/config.rs src/main.rs && git commit -m "Review view: config file with repo list"`

---

### Task 3: Review PR model and parsing

**Files:**
- Create: `src/review/mod.rs` (`pub mod model;` for now), `src/review/model.rs`, `tests/fixtures/review-search.json`, `tests/fixtures/review-detail.json`, `tests/fixtures/review-teams.json`
- Modify: `src/model.rs` (visibility only), `src/main.rs` (`mod review;`)

**Interfaces:**
- Consumes: `model::Pr`, `model::ReviewState`, `model::Reviewer`.
- Changes in `src/model.rs`: make `data_of` `pub(crate)`; add `pub(crate) fn pr_from_value(v: Value) -> Result<Pr, String>` (deserialize `RawPr`, `convert_pr`), and use it inside `parse_detail`.
- Produces (`src/review/model.rs`, all types derive `Debug, Clone, PartialEq, Serialize, Deserialize`):
  ```rust
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
  }
  pub struct SearchPage { pub viewer: String, pub hits: Vec<(String /*id*/, String /*owner/name*/)>, pub next: Option<String> }
  pub fn parse_search_page(json: &str) -> Result<SearchPage, String>
  pub fn parse_teams(json: &str) -> Result<Vec<String>, String>
  pub fn parse_review_detail(json: &str) -> Result<Vec<ReviewPr>, String>   // pN aliases, numeric order, nulls skipped
  pub fn description_lines(body: Option<&str>) -> Vec<String>
  impl ReviewPr {
      pub fn included(&self, viewer: &str) -> bool        // open, !draft, author != viewer, author !starts_with("dependabot")
      pub fn approved(&self) -> bool                       // pr.approved == Some(true), or None and any reviewer Approved
      pub fn waiting_on_author(&self, viewer: &str) -> bool // viewer reviewer ChangesRequested with at=t, and head_committed_at <= t (None counts as not pushed)
      pub fn code_owner(&self, viewer: &str, teams: &[String]) -> bool // any request in teams, or == viewer
      pub fn org(&self) -> &str                            // owner part of pr.repo
  }
  ```
- Response shapes the parsers read (Task 4's queries must produce exactly these): search `data.viewer.login`, `data.search.pageInfo { hasNextPage endCursor }`, `data.search.nodes[] { id repository { nameWithOwner } }`; teams `data.organization.teams.nodes[].slug` (`organization: null` → empty); detail alias extras `createdAt`, `body`, `timelineItems.nodes[].createdAt`, `reviewRequests.nodes[] { asCodeOwner requestedReviewer { login | slug } }`, `commits.nodes[0].commit.committedDate`.

- [ ] **Step 1: Write the fixtures** per the shared table (detail fixture carries every field `RawPr` requires plus the extras; the search fixture lists all 14 ids, `hasNextPage: false`).
- [ ] **Step 2: Write the failing tests in `src/review/model.rs`**

```rust
const DETAIL: &str = include_str!("../../tests/fixtures/review-detail.json");
fn now() -> DateTime<Utc> { "2026-10-20T15:00:00Z".parse().unwrap() }
fn get(prs: &[ReviewPr], n: u32) -> &ReviewPr { prs.iter().find(|p| p.pr.number == n).unwrap() }

#[test] fn ready_time_uses_latest_ready_event() {
    let prs = parse_review_detail(DETAIL).unwrap();
    assert_eq!(get(&prs, 1312).ready_at, "2026-10-18T15:00:00Z".parse::<DateTime<Utc>>().unwrap());
    assert_eq!(get(&prs, 1298).ready_at, get(&prs, 1298).created_at);
}
#[test] fn inclusion_rules() {
    let prs = parse_review_detail(DETAIL).unwrap();
    let shown: Vec<u32> = prs.iter().filter(|p| p.included("jayhogan")).map(|p| p.pr.number).collect();
    for n in [1330, 1331, 1332] { assert!(!shown.contains(&n), "{n}"); }
    assert!(shown.contains(&1333));
    assert_eq!(get(&prs, 1333).pr.author, "ghost");
}
#[test] fn approval_rule_with_null_decision() {
    let prs = parse_review_detail(DETAIL).unwrap();
    assert!(get(&prs, 421).approved());
    assert!(get(&prs, 431).approved());
    assert!(!get(&prs, 430).approved());
    assert!(!get(&prs, 1298).approved());
}
#[test] fn waiting_on_author_only_without_newer_commit() {
    let prs = parse_review_detail(DETAIL).unwrap();
    assert!(get(&prs, 91).waiting_on_author("jayhogan"));
    assert!(!get(&prs, 95).waiting_on_author("jayhogan"));
    assert!(!get(&prs, 87).waiting_on_author("jayhogan"));
}
#[test] fn code_owner_needs_a_viewer_team() {
    let prs = parse_review_detail(DETAIL).unwrap();
    let teams = vec!["billing".to_string(), "platform".to_string()];
    assert!(get(&prs, 1298).code_owner("jayhogan", &teams));
    assert!(!get(&prs, 418).code_owner("jayhogan", &teams));
    assert!(!get(&prs, 1298).code_owner("jayhogan", &[]));
}
#[test] fn description_trims_comments_and_caps_at_five() {
    let d = description_lines(Some("<!-- template\nhint -->\n\nOne\r\n  Two  \n\nThree\nFour\nFive\nSix"));
    assert_eq!(d, ["One", "Two", "Three", "Four", "Five"]);
}
#[test] fn description_empty_and_comment_only() {
    assert!(description_lines(None).is_empty());
    assert!(description_lines(Some("")).is_empty());
    assert!(description_lines(Some("<!-- only -->\n")).is_empty());
}
#[test] fn parses_search_page_and_teams() {
    let s = parse_search_page(include_str!("../../tests/fixtures/review-search.json")).unwrap();
    assert_eq!(s.viewer, "jayhogan");
    assert_eq!(s.hits.len(), 14);
    assert_eq!(s.next, None);
    assert_eq!(parse_teams(include_str!("../../tests/fixtures/review-teams.json")).unwrap(), ["billing", "platform"]);
    assert_eq!(parse_teams(r#"{"data":{"organization":null}}"#).unwrap(), Vec::<String>::new());
}
```

- [ ] **Step 3: Run** `cargo test review::model` — Expected: FAIL (items missing).
- [ ] **Step 4: Implement.** Parse each alias twice from the same `Value`: `model::pr_from_value(v.clone())` for the `Pr`, and a private `RawExtras` struct for the rest. HTML comments are removed across lines before splitting.
- [ ] **Step 5: Run** `cargo test` — Expected: all pass (existing model tests unaffected).
- [ ] **Step 6: Commit** `git commit -m "Review view: PR model, rules and fixtures"` (add the files above).

---

### Task 4: Review GraphQL queries

**Files:**
- Create: `src/review/github.rs`, `src/query/review_search.graphql`, `src/query/review_fields.graphql`
- Modify: `src/github.rs` (visibility + fake), `src/review/mod.rs` (`pub mod github;`)

**Interfaces:**
- Changes in `src/github.rs`: `check_saml` → `pub(crate)`; extract the per-alias `statusCheckRollup { … isRequired(pullRequestId: $idN) … }` text into `pub(crate) fn rollup_fields(n: usize) -> String` and use it in `detail_query`; `FakeGh` kind detection checks, in order, `PrPulseReviewSearch` → `"review-search"`, `PrPulseReviewTeams` → `"review-teams"`, `PrPulseReviewDetail` → `"review-detail"`, then the existing two.
- Produces (`src/review/github.rs`):
  ```rust
  pub fn search_text(repos: &[String]) -> String
  // "is:pr is:open draft:false archived:false -author:@me -author:app/dependabot repo:A repo:B"
  pub fn search(gh: &dyn Gh, repos: &[String]) -> Result<(String /*viewer*/, Vec<(String, String)>), GhError> // follows `next` until None
  pub fn teams(gh: &dyn Gh, org: &str, viewer: &str) -> Result<Vec<String>, GhError>
  pub fn detail_query(ids: &[String]) -> (String, Value)   // operation name PrPulseReviewDetail; aliases p0..pN: node(id: $idN) { ...reviewFields ... on PullRequest { <rollup_fields(N)> } }
  pub fn detail(gh: &dyn Gh, ids: &[String]) -> Result<Vec<ReviewPr>, GhError> // empty ids → Ok(vec![]) without calling gh
  ```
  `review_search.graphql`: `query PrPulseReviewSearch($q: String!, $after: String) { viewer { login } search(query: $q, type: ISSUE, first: 100, after: $after) { pageInfo { hasNextPage endCursor } nodes { ... on PullRequest { id repository { nameWithOwner } } } } rateLimit { cost remaining } }`. Teams query `PrPulseReviewTeams($org: String!, $login: String!)` inline in `github.rs` per the Task 3 shape. `review_fields.graphql` fragment `reviewFields on PullRequest`: the scalars `RawPr` requires, `createdAt body reviewDecision`, `latestReviews(first: 20)`, `reviewRequests(first: 20) { nodes { asCodeOwner requestedReviewer { __typename ... on User { login } ... on Bot { login } ... on Team { slug } } } }`, `timelineItems(itemTypes: [READY_FOR_REVIEW_EVENT], last: 1) { nodes { ... on ReadyForReviewEvent { createdAt } } }`, `commits(last: 1) { nodes { commit { committedDate } } }`. No `stackEntry`, `reviewThreads`, `baseRef`, `assignees`, `mergeStateStatus`.
  Every call runs `check_saml` then the Task 3 parser, mapping parse errors to `GhError::Other`.

- [ ] **Step 1: Write the failing tests in `src/review/github.rs`**

```rust
#[test] fn search_text_lists_each_repo() {
    assert_eq!(search_text(&["heard/a".into(), "heard/b".into()]),
        "is:pr is:open draft:false archived:false -author:@me -author:app/dependabot repo:heard/a repo:heard/b");
}
#[test] fn search_follows_pages() { /* FakeGh can hold one response per kind, so build a small
    test-local Gh that returns page 1 (hasNextPage true, endCursor "C1", 1 hit) then page 2
    (hasNextPage false, 1 hit); assert 2 hits and that the second call's variables have after = "C1" */ }
#[test] fn search_error_names_the_message() {
    let body = r#"{"data":null,"errors":[{"message":"The listed users and repositories cannot be searched"}]}"#;
    let gh = FakeGh::new().with("review-search", Ok(body.into()));
    let err = search(&gh, &["nope/missing".into()]).unwrap_err();
    assert!(err.message().contains("cannot be searched"), "{err:?}");
}
#[test] fn detail_query_aliases_and_fragment() {
    let (q, vars) = detail_query(&["A".into(), "B".into()]);
    assert!(q.contains("PrPulseReviewDetail") && q.contains("p1: node(id: $id1)") && q.contains("fragment reviewFields"));
    assert!(!q.contains("stackEntry") && !q.contains("reviewThreads"));
    assert_eq!(vars["id1"], "B");
}
#[test] fn detail_with_no_ids_skips_gh() {
    let gh = FakeGh::new();
    assert!(detail(&gh, &[]).unwrap().is_empty());
    assert!(gh.calls.borrow().is_empty());
}
#[test] #[ignore] fn live_review_search() { /* GhCli, repos from config::load_review(); assert Ok */ }
```

- [ ] **Step 2: Run** `cargo test review::github` — Expected: FAIL.
- [ ] **Step 3: Implement** the queries and functions above.
- [ ] **Step 4: Run** `cargo test && cargo clippy --all-targets -- -D warnings` — Expected: PASS.
- [ ] **Step 5: Validate the query text live once:** `cargo test live_review_search -- --ignored` with a real config. Expected: PASS. Note the `rateLimit.cost` of one detail chunk in the commit message.
- [ ] **Step 6: Commit** `git commit -m "Review view: GraphQL search, teams and slim detail queries"`

---

### Task 5: Fetch worker and cache

**Files:**
- Create: `src/review/fetch.rs`
- Modify: `src/store.rs`, `src/review/mod.rs` (`pub mod fetch;`)

**Interfaces:**
- Changes in `src/store.rs`: add `pub fn try_lock_named(&self, name: &str) -> io::Result<Option<File>>` and `pub fn lock_named(&self, name: &str) -> io::Result<File>`; `try_fetch_lock`/`fetch_lock` delegate with `"fetch.lock"`.
- Consumes: `fetch::{decide, Plan, FetchRequest, RepoStatus, Poller}`, Task 4's `search`/`teams`/`detail`.
- Produces:
  ```rust
  pub const CACHE: &str = "review-cache.json";
  pub const LOCK: &str = "review-fetch.lock";
  #[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
  pub struct ReviewCache {
      pub version: u32,                                   // 1
      pub fetched_at: Option<DateTime<Utc>>,
      pub viewer: String,
      /// org → viewer's team slugs; None when the lookup failed (no ★, notice shown).
      pub teams: Option<BTreeMap<String, Vec<String>>>,
      pub prs: Vec<ReviewPr>,
      pub error: Option<GhError>,
      pub error_at: Option<DateTime<Utc>>,
  }
  #[derive(Debug, Clone, PartialEq)]
  pub enum ReviewEvent { Started, Progress(Vec<(String, RepoStatus)>), Done(ReviewCache) }
  pub fn spawn(gh: Box<dyn Gh>, state: StateDir, repos: Vec<String>) -> (Sender<FetchRequest>, Receiver<ReviewEvent>)
  ```
  Behavior mirrors `fetch::Worker::handle` with `LOCK` and `CACHE` (same `decide` table; `Recheck` is never sent, treat it like `Forced`). A full fetch: search; progress lists every configured repo (`Queued`, then `Loading`/`Done(n)` per repo as its detail returns; repos with no hits go straight to `Done(0)`); detail per repo in chunks of 25; teams once per distinct org — any team error sets `teams: None` and does not fail the fetch. Search or detail error → keep previous `prs`, set `error`/`error_at`. The cache stores every fetched PR (filtering happens in the app). Write under `state.update(CACHE, …)`, as `fetch::Worker::save` does.

- [ ] **Step 1: Write the failing tests in `src/review/fetch.rs`** (pattern: `fetch.rs` tests' `recv`/`until_done` helpers, `tempfile` state dir, `FakeGh` with the Task 3 fixtures)

```rust
#[test] fn full_fetch_writes_cache_with_teams() {
    // FakeGh: review-search, review-teams, review-detail fixtures. Send Forced.
    // until_done → last event Done(c): c.viewer == "jayhogan", c.prs.len() == 14,
    // c.teams == Some({"heard": ["billing","platform"]}), c.error == None;
    // a Progress event lists "heard/docket", "heard/heard-ts", "heard/tax-engine" all Done.
}
#[test] fn team_failure_keeps_list_without_teams() {
    // review-teams → Err(GhError::Other("INSUFFICIENT_SCOPES".into())); Done(c): c.teams == None, c.prs.len() == 14, c.error == None
}
#[test] fn search_failure_keeps_previous_prs() {
    // seed review-cache.json with 1 PR; review-search → Err; Forced → Done(c): c.prs.len() == 1, c.error.is_some()
}
#[test] fn scheduled_uses_fresh_cache_without_gh() {
    // seed cache fetched_at = now - 10s; Scheduled → Done(seeded) and FakeGh.calls empty
}
#[test] fn review_lock_is_separate_from_base_lock() {
    // holding state.try_fetch_lock() does not stop try_lock_named(LOCK)
}
```

(`FakeGh` is `!Send` because of `RefCell`; run the worker logic through a test-only function that takes `&dyn Gh` — for example split `Worker::handle` so tests call it directly, as you see fit — rather than `spawn`.)

- [ ] **Step 2: Run** `cargo test review::fetch store::` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test && cargo clippy --all-targets -- -D warnings` — Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "Review view: fetch worker, review-cache.json and its own lock"`

---

### Task 6: ReviewApp view model and keys

**Files:**
- Create: `src/review/app.rs`
- Modify: `src/review/mod.rs` (`pub mod app;`), `src/app.rs` (only if `Banner`/`LoadProgress`/`Action`/`SPINNER` need `pub` re-use — they already are `pub`)

**Interfaces:**
- Consumes: `config::ReviewConfig`, `ReviewCache`, `ReviewEvent`, `ReviewPr` methods, `app::{Action, Banner, LoadProgress, SPINNER}`, `StateDir`, `ago::short`.
- Produces:
  ```rust
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum GroupBy { User, Repo }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Show { NotApproved, Approved }
  #[derive(Debug, Clone, PartialEq)] pub struct ReviewRow { pub pr: ReviewPr, pub code_owner: bool, pub waiting: bool }
  #[derive(Debug, Clone, PartialEq)] pub struct ReviewGroup { pub key: String /* "user:<login>" | "repo:<owner/name>" */, pub name: String /* login | short repo */, pub oldest: DateTime<Utc>, pub folded: bool, pub rows: Vec<ReviewRow> }
  #[derive(Debug, Clone, PartialEq)] pub enum ReviewBody {
      Loading(LoadProgress),
      NotConfigured { path: String },
      /// Nothing in this filter; `other` = count in the other filter.
      Empty { repos: usize, show: Show, other: usize },
      Groups(Vec<ReviewGroup>),
  }
  #[derive(Debug, PartialEq)] pub struct ReviewScreen {
      pub prs: usize, pub people: usize, pub repos: usize,   // over listed PRs; repos = configured count when Empty
      pub group_by: GroupBy, pub show: Show,
      pub code_owner: usize, pub waiting: usize,
      pub fetched_at: Option<DateTime<Utc>>, pub fetching: bool, pub stale: bool, pub spinner: &'static str,
      pub banner: Option<Banner>, pub teams_missing: bool,
      pub body: ReviewBody,
      pub cursor: usize,                 // index into selectable lines: group headers + rows of unfolded groups
      pub drawer_pr: Option<String>,
      pub help: bool, pub help_scroll: usize,
      pub status: Option<String>,
      pub now: DateTime<Utc>,
  }
  pub struct ReviewApp { /* private */ }
  impl ReviewApp {
      pub fn new(config: ReviewConfig) -> ReviewApp
      pub fn with_state(self, state: StateDir) -> ReviewApp      // loads review-folds.json (BTreeSet<String> of group keys)
      pub fn repos(&self) -> Vec<String>                         // configured repos; empty unless Repos
      pub fn apply(&mut self, ev: ReviewEvent)
      pub fn set_cache(&mut self, cache: ReviewCache)            // = apply(Done(cache)); for fixtures/tests
      pub fn handle_key(&mut self, key: KeyEvent) -> Action
      pub fn screen(&self, now: DateTime<Utc>) -> ReviewScreen
      pub fn advance_spinner(&mut self)
      pub fn set_status(&mut self, status: String)
      pub fn set_help_max(&mut self, max: usize)
  }
  ```
- Rules (spec §5): listed = `included(viewer)` and `approved() == (show == Approved)`. Groups sorted by `oldest` ascending, then name; rows by `ready_at` ascending, then repo, then number. `code_owner` uses `cache.teams[org]` (missing org or `teams: None` → false). `Invalid` config → `banner = Banner { message: "config: <message>", fix: "Fix <path>, then relaunch the pane." }` and body `NotConfigured`. Fetch error → banner from `GhError::message()`/`fix()`, rows kept with `stale = true`. Before any data → `Loading`.
- Keys (spec §7): `j`/`↓`, `k`/`↑` move; `␣` on header toggles fold, on row toggles drawer (one drawer at a time); `h`/`←` folds the selected row's group and moves the cursor to its header, `l`/`→` unfolds; `o` on a row → `Action::Open(url)`, on a header → `Action::None`; `r` → `Action::Refresh`; `g` toggles `GroupBy`; `a` toggles `Show`; `?` help (while open: `j`/`k` scroll, `?`/`Esc` close); `q`/Ctrl-C → `Action::Quit`. Folds persist to `review-folds.json` on every change when a state dir is set. After any change to the list (key, refresh), the cursor stays on the same PR or header if still listed, else clamps to the last selectable line.

- [ ] **Step 1: Write the failing tests in `src/review/app.rs`** (helper `fn app() -> ReviewApp` = `ReviewApp::new(ReviewConfig::Repos(vec!["heard/heard-ts".into(), "heard/tax-engine".into(), "heard/docket".into()]))` + `set_cache` from the three fixtures with `fetched_at = now()`; helper `numbers(&ReviewScreen) -> Vec<Vec<u32>>` per group)

```rust
#[test] fn groups_by_user_in_age_order() {
    let s = app().screen(now());
    let ReviewBody::Groups(gs) = &s.body else { panic!() };
    assert_eq!(gs.iter().map(|g| g.name.as_str()).collect::<Vec<_>>(), ["maria-chen", "dan-o", "priya-s", "ghost", "sam-k"]);
    assert_eq!(numbers(&s), [vec![1298, 1312, 1320], vec![87, 95, 91], vec![430], vec![1333], vec![418]]);
    assert_eq!((s.prs, s.people, s.code_owner, s.waiting), (9, 5, 2, 1));
}
#[test] fn g_groups_by_repo() {
    let mut a = app(); a.handle_key(key('g'));
    assert_eq!(numbers(&a.screen(now())), [vec![1298, 1312, 1333, 1320], vec![87, 95, 91], vec![430, 418]]);
}
#[test] fn a_shows_approved() {
    let mut a = app(); a.handle_key(key('a'));
    let s = a.screen(now());
    assert_eq!(s.show, Show::Approved);
    assert_eq!(numbers(&s).concat().len(), 2);   // 421, 431
}
#[test] fn space_folds_header_and_opens_drawer_on_row() {
    let mut a = app();
    a.handle_key(key(' '));                         // cursor 0 = maria-chen header
    assert!(groups(&a)[0].folded);
    a.handle_key(key(' ')); a.handle_key(key('j')); a.handle_key(key(' '));
    assert_eq!(a.screen(now()).drawer_pr.as_deref(), Some(id_of(&a, 1298)));
    a.handle_key(key(' '));
    assert_eq!(a.screen(now()).drawer_pr, None);
}
#[test] fn o_opens_row_url_and_nothing_on_header() { /* header → Action::None; row 1298 → Action::Open(its url) */ }
#[test] fn folds_persist() { /* with_state(tmp); fold maria-chen; new app with same state → maria-chen folded */ }
#[test] fn cursor_survives_toggle_and_refresh() {
    // select 91 (dan-o's last row); press 'a' then 'a' → still on 91.
    // select 418 (last line); set_cache without 418 → cursor == last selectable line, no panic.
}
#[test] fn not_configured_and_invalid() {
    let s = ReviewApp::new(ReviewConfig::Missing { path: "/c.toml".into() }).screen(now());
    assert_eq!(s.body, ReviewBody::NotConfigured { path: "/c.toml".into() });
    let s = ReviewApp::new(ReviewConfig::Invalid { path: "/c.toml".into(), message: "bad".into() }).screen(now());
    assert_eq!(s.banner.unwrap().message, "config: bad");
}
#[test] fn empty_counts_the_other_filter() { /* cache with only 421 and 431 → Empty { repos: 3, show: NotApproved, other: 2 } */ }
#[test] fn teams_missing_drops_stars_and_flags_notice() { /* cache.teams = None → code_owner == 0, teams_missing */ }
#[test] fn error_keeps_rows_and_sets_banner() { /* Done(cache with error Other("…cannot be searched")) after a good one → stale, rows kept, banner message contains "cannot be searched" */ }
#[test] fn loading_before_data() { /* new app → Loading; Progress event → Loading with those repos */ }
```

- [ ] **Step 2: Run** `cargo test review::app` — Expected: FAIL.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test && cargo clippy --all-targets -- -D warnings` — Expected: PASS.
- [ ] **Step 5: Commit** `git commit -m "Review view: view model, grouping, filters and keys"`

---

### Task 7: Rendering

**Files:**
- Create: `src/ui/review.rs` (+ snapshots under `src/ui/snapshots/`)
- Modify: `src/ui/mod.rs` (`pub mod review;`), `src/ui/drawer.rs`, `src/ui/help.rs`, `src/ui/cells.rs`/`states.rs` (visibility only, as needed)

**Interfaces:**
- Changes in `src/ui/drawer.rs`: `ci_section` takes `ci: &Ci` instead of `&RowView`; `reviews_section` takes `reviewers: &[Reviewer]` and `code_owners: &[String]` (base passes `&[]`), and `review_item` appends dim ` (code owner)` after the state word for a login in `code_owners`. Base drawer output must not change (existing snapshots stay green). Add:
  `pub fn review_drawer_lines(row: &ReviewRow, viewer: &str, width: u16, now: DateTime<Utc>) -> Vec<Line<'static>>` — the description lines (each `     │ <text>`, wrapped to the width then clipped at 5 lines; `no description` dim when empty), then the CI and Reviews sections laid out like the base drawer (`side_by_side` from Medium up, else `stacked`), reviewers excluding `viewer`, closing the rail.
- Changes in `src/ui/help.rs`: add `pub fn review_help_lines(width: u16) -> Vec<Line<'static>>` — legend `★ yellow  a team of yours is a requested code owner`, `↩ dim  you requested changes; no push since`, and the §7 key table.
- Produces: `pub fn render(frame: &mut Frame, screen: &ReviewScreen, scroll: &mut usize) -> usize` (same contract as `rows::render`: returns the help overlay's max scroll).
- Layout (spec §6): header bar `theme::bar()`: Wide `PR Pulse · Review │ 9 PRs · 5 people · 3 repos   group: user  show: not approved │ ↻ 18s` (⠹ spinner while fetching; Medium drops the counts' `people`/`repos`; Narrow/Tiny keep `Review` and the age only). Summary: `★2 code owner  ↩1 waiting on author` (zero counts omitted); when `teams_missing`, a dim line `★ off: can't read your teams · gh auth refresh -s read:org`. Wide/Medium: rule + column titles `#  TITLE  REPO  USER  READY`. Group header `▾ maria-chen  3 PRs · oldest 4d` / `▸ …` folded; cursor `▶` + bold as in the base. Row: marker column (2 wide: `★`, `↩`, or both as `★↩`), `#n`, title, repo (short), user, ready age (`ago::short`). Wide: all columns. Medium: drop the column matching `group_by`. Narrow/Tiny: two lines — line 1 marker, `#n`, title; line 2 indented: the non-grouping column + age. Stale → all rows dim. Footer (Wide): `j/k move  ␣ details/fold  o open  r refresh  g group  a approved⇄not  ? help`; shorter at smaller densities, like `chrome::footer`. Empty bodies: `NotConfigured` → `No repos configured.` + `Add them to <path>:` + the TOML sample; `Empty` → `Nothing waiting on review in 3 repos.` + `a show approved (2)` when `other > 0` (for `Show::Approved`: `No approved PRs in 3 repos.`). Loading → reuse `states::loading_lines` logic for repo progress (make what you need `pub(super)`).

- [ ] **Step 1: Write the failing snapshot and assertion tests in `src/ui/review.rs`** (helpers mirror `ui/mod.rs`: `draw(&ReviewApp, w, h)`, `text_lines`, `text`; the app comes from Task 6's fixture helper — expose it as `#[cfg(test)] pub(crate) fn fixture_app() -> ReviewApp` in `review/app.rs`)

```rust
#[test] fn review_user_120() { insta::assert_snapshot!(text(&draw(&fixture_app(), 120, 30))); }
#[test] fn review_user_80()  { insta::assert_snapshot!(text(&draw(&fixture_app(), 80, 30))); }
#[test] fn review_user_44()  { insta::assert_snapshot!(text(&draw(&fixture_app(), 44, 30))); }
#[test] fn review_user_34()  { insta::assert_snapshot!(text(&draw(&fixture_app(), 34, 30))); }
#[test] fn review_repo_120() { /* press g */ }
#[test] fn review_drawer_120_on_1312() { /* open drawer on 1312: 5 description lines, CI, "@billing" … "(code owner)", no "@jayhogan" */ }
#[test] fn review_drawer_44_on_1312() { }
#[test] fn review_drawer_no_description_120() { /* drawer on 430 shows "no description" */ }
#[test] fn review_folded_80() { /* fold maria-chen */ }
#[test] fn review_not_configured_80() { }
#[test] fn review_empty_80() { }
#[test] fn review_help_80() { }
#[test] fn star_is_yellow_and_waiting_row_is_dim() {
    // buffer cell under "★" on the #1298 line has fg Color::Yellow; the "#91" cells have fg Color::DarkGray
}
#[test] fn long_title_clips_at_34() { /* every line ≤ 34 cells; the #1312 line contains "…" */ }
```

- [ ] **Step 2: Run** `cargo test ui::review` — Expected: FAIL.
- [ ] **Step 3: Implement**, then `cargo insta review` and check each new snapshot against spec §6 by eye (columns, ★/↩, group order, header counts `9 PRs · 5 people · 3 repos`).
- [ ] **Step 4: Run** `cargo test && cargo clippy --all-targets -- -D warnings` — Expected: PASS, including every pre-existing snapshot unchanged.
- [ ] **Step 5: Commit** `git commit -m "Review view: rendering, details drawer and help"`

---

### Task 8: Launch wiring, fixtures mode and docs

**Files:**
- Create: event loop in `src/review/mod.rs`
- Modify: `src/main.rs`, `README.md`, `src/config.rs` (drop any temporary `allow(dead_code)`)

**Interfaces:**
- Consumes: everything above.
- Changes in `src/main.rs`: `Args { view: View, fixtures: bool }` with `#[derive(Debug, PartialEq)] enum View { Pulse(ViewKind), Review }`; `"review"` → `View::Review`; usage `herdr-pr-pulse [workspace|mine|review] [--fixtures]`; `open_in_browser` becomes `pub(crate)`; `run` dispatches `View::Review` to `review::run(fixtures)`.
- Produces (`src/review/mod.rs`): `pub fn run(fixtures: bool) -> Result<(), String>`. Live: `config::load_review()`, `ReviewApp::new(..).with_state(StateDir::default_location())`, `fetch::spawn(Box::new(GhCli), StateDir::default_location(), app.repos())` only when the config has repos, a `Poller`, and a 1 s tick loop like `main::event_loop` (draw with `ui::review::render`; `Action::Open` → `crate::open_in_browser`; `Action::Refresh` → `poller.forced()`; `Action::Focus` never occurs). The review view does **not** require a herdr pane (my call: it has no herdr features). Fixtures: `ReviewConfig::Repos([heard/heard-ts, heard/tax-engine, heard/docket])`, cache from the three fixture files with `fetched_at = 2026-10-20T15:00:00Z`, throwaway state dir removed on exit, actions only set a `fixtures: would open <url>` status.

- [ ] **Step 1: Update `main.rs` tests first**

```rust
#[test] fn review_positional() { assert_eq!(parse(&["review"]).unwrap().view, View::Review); }
#[test] fn rejects_two_views() { assert!(parse(&["review", "mine"]).is_err()); }
// existing tests: replace `kind: ViewKind::X` with `view: View::Pulse(ViewKind::X)`
```
And in `src/review/mod.rs`: `#[test] fn fixtures_load()` — the fixture app's screen has 9 PRs and 5 groups.

- [ ] **Step 2: Run** `cargo test` — Expected: FAIL (new tests).
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check` — Expected: PASS.
- [ ] **Step 5: Run it.** `cargo run -- review --fixtures` in a terminal: check j/k, ␣ on a header and a row, g, a, ?, o (status line), q. Then live, with a real config: `cargo run --release -- review` — the list loads, ★ shows on a known code-owner PR, `r` refreshes.
- [ ] **Step 6: README.** Add the Review view: what it shows, `herdr-pr-pulse review`, the config file path and TOML sample, the keys (`␣` details/fold, `g`, `a`), ★ and ↩ meanings, and that it needs `read:org` on the `gh` token for ★.
- [ ] **Step 7: Commit** `git commit -m "Review view: launch with herdr-pr-pulse review; fixtures mode and README"`
