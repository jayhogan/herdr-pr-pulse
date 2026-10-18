# herdr-pr-pulse — design

Date: 2026-10-17 · Status: draft for approval

Design canvas (mockups, the source of truth for layout): https://claude.ai/artifact/AzYJKRJJtWuBPCad66Tv7z
Boards: 1 Mine 120 cols · 1b light · 2 Workspace 44/34 cols · 3 Mine 80 cols · 4 loading/empty/error ·
5 legend + keys · 6 "what changed" (option A chosen).

## 1. Why

Jay works many Linear issues in parallel, one herdr workspace per issue, one Claude session per
worktree. PRs need ongoing care: CI failures, automated review comments, human reviews, conflicts.
The hard part is timing. CI takes 4–6 minutes after a push and human reviews arrive anywhere from
10 minutes to 2 days later, by which time Jay has moved on to another workspace. Nothing pulls
him back.

PR Pulse is a herdr plugin TUI that answers **"what needs my attention right now?" within two
seconds of a glance**, and highlights **what changed since I last looked**.

## 2. Goals, non-goals, anti-goals

Goals
- One glance shows which PRs need action, ranked by urgency, across all repos.
- Late-arriving events (review, CI result, conflict) stand out when Jay comes back to a workspace.
- Ties PRs to the herdr agent working them: an idle agent on a red PR is the most urgent thing.
- ⏎ jumps straight to the agent pane for a PR's stack.
- Works in a 34-column side pane and on a 120-column screen; dark and light terminals.

Non-goals (v1)
- No mutations: no merge, approve, rerun, comment, checkout. Read-only.
- No notifications outside the TUI.
- No "since you looked" event feed (board 6 option C). Parked; maybe later.

Anti-goals (would be a failure even if it works)
- Blocking the UI on the network.
- Color as the only signal; any blue or magenta.
- Burning the GitHub GraphQL rate limit that Jay's Claude sessions also depend on.
- Marking a change as "seen" because Jay was looking at a different workspace.

## 3. Constraints and template

Match `https://github.com/jayhogan/herdr-skill-timeline` (cloned and read):
- Rust 2024 edition; ratatui 0.30; crossterm 0.29; serde/serde_json; chrono (`default-features =
  false`, `std`, `clock`, `serde`). No async runtime. Dev-deps: `tempfile`, plus `insta`.
- Flat `src/*.rs` modules with `//!` module docs; errors are `Result<T, String>` with
  human-readable messages; subprocesses via `std::process::Command`; `HERDR_BIN_PATH` env var
  (fallback `herdr`) for the herdr binary.
- `main.rs` checks `HERDR_ENV=1` and `HERDR_PANE_ID`, runs `ratatui::init()` / `restore()`, and a
  `crossterm::event::poll` loop with a refresh deadline.
- `herdr-plugin.toml` manifest with `[[build]] cargo build --release`; `scripts/install.sh`
  symlinks the binary into `~/.local/bin`; `herdr plugin link` to install.
- State under `~/.local/state/herdr-pr-pulse/`.
- Tests: `cargo test` with fixtures in `tests/fixtures/`; `cargo clippy --all-targets` clean.
- README sections as in skill-timeline: install, use (workspace-manager layout snippet),
  how it works, development.

Additions: `insta` snapshot tests; a background `std::thread` + `std::sync::mpsc` for GitHub
fetches so the UI never blocks.

Auth: always shell out to `gh` (`gh api graphql`, `gh pr view --web` / `open` for the browser).
Never read or store tokens.

## 4. Research findings

### herdr (0.9.3, protocol 22)

`herdr api snapshot` returns `{result: {snapshot: {...}}}` with:
- `workspaces[]`: `workspace_id`, `label`, `focused`, `worktree{checkout_path, repo_root,
  repo_name, is_linked_worktree}`.
- `tabs[]`, `layouts[]` (geometry; not needed).
- `panes[]`: `pane_id`, `workspace_id`, `tab_id`, `cwd`, `foreground_cwd`, `agent_status`,
  `focused`.
- `agents[]`: `pane_id`, `workspace_id`, `agent` (`"claude"`), `agent_status`, `cwd`,
  `foreground_cwd`, `agent_session{kind,value}`, `terminal_title_stripped`.
- Top level: `focused_workspace_id`, `focused_pane_id`.

Agent status **is exposed**: enum `idle | working | blocked | done | unknown`. Treat `done` as
idle. No transcript fallback is needed.

Pane cwd → branch is **not** in the snapshot: run `git -C <cwd> rev-parse --abbrev-ref HEAD` and
`git -C <cwd> remote get-url origin` (parse `owner/repo`), cached by cwd and re-checked each
2 s poll only for the branch (remote is stable).

Focusing a pane: the socket API has `pane.focus {pane_id}`; the CLI has `herdr agent focus
<target>`. Verify in milestone 3 which CLI form accepts a pane id and switches workspace;
prefer the CLI (consistent with `api snapshot` usage).

Visibility: PR Pulse's pane is on screen when `focused_workspace_id` is its own workspace AND
that workspace's `active_tab_id` is its own pane's `tab_id` AND the tab's layout is not zoomed
on another pane (`layouts[].zoomed` + `focused_pane_id`). Checked every 2 s poll.

### GitHub GraphQL (introspected and queried live against Jay's PRs)

- `PullRequest.stack: PullRequestStack` → `{id, number, size, baseRefName, entries}`. **No name
  field.**
- `PullRequest.stackEntry: PullRequestStackEntry` → `{id, position, stack, pullRequest}`.
  `position` is 1-based, **1 = bottom (nearest base)**. Confirmed on heard-ts #1770/1771/1772
  (stack 1773, size 3).
- Behind-by: `baseRef { compare(headRef: <headRefName>) { behindBy } }`. Returns null when the
  head ref is gone; then show nothing.
- REST fallback `GET /repos/{o}/{r}/stacks` returns stacks with `base.ref`, `open`, and ordered
  `pull_requests[]` including `merged_at`. Use it only if a GraphQL stack field errors.
- Cost (measured): the full query as one `search` per scope with all nested fields costs
  **55 points**; limit is 5,000/hour. A cheap `search` returning only ids costs **1**; the full
  detail query via `nodes(ids:)` for 8 PRs costs **4**.

## 5. Data fetching

**Two GraphQL requests per refresh** (deviation from "one batched query"; decided for cost,
see §4):
1. Search: `author:@me` and `assignee:@me` aliases (`is:open is:pr archived:false`, first 50)
   returning ids only.
2. Detail: `nodes(ids: [...])` over the union of search results plus the PR ids remembered by
   any workspace (§8), so merged/closed PRs keep updating. Per PR: number, title, url, state,
   isDraft, mergedAt/closedAt, author, assignees, repository, headRefName, headRefOid,
   baseRefName, mergeStateStatus, reviewDecision, behindBy, stack/stackEntry,
   statusCheckRollup (state + contexts: CheckRun name/status/conclusion/completedAt and
   StatusContext), `stack.entries` (ids + positions of every layer), reviewThreads (isResolved, first comment's author login + `__typename`),
   latestReviews (state, submittedAt, author login + `__typename`), reviewRequests
   (requested reviewer login/slug + `__typename`). Include `rateLimit{cost remaining}`.
3. Stack fill-in (only when needed): layers listed in `stack.entries` that step 2 didn't return
   (a colleague's layer, a layer never checked out, a merged layer) are fetched with one more
   `nodes(ids:)` detail request. Stacks are always shown whole, in both views.

**One fetch shared by all running copies.** Jay runs a PR Pulse pane per workspace. Results are
written to `~/.local/state/herdr-pr-pulse/cache.json` with a fetch timestamp; a copy only fetches
when the cache is older than 60 s, guarded by a lock file (`create_new`; stale after 30 s). Other
copies read the cache. Manual `r` forces a fetch regardless.

Timing: GitHub every 60 s + `r`; herdr snapshot every 2 s. PRs whose `mergeStateStatus` is
`UNKNOWN` are re-polled once after ~10 s (detail query for just those ids).

Errors from `gh` are classified:
- not logged in / 401 / "gh auth login" → banner "GitHub login expired. Run `gh auth login`".
- message containing "SAML" → banner "Token not authorized for SAML SSO. Run
  `gh auth refresh`, then authorize the token for the org at github.com/settings/tokens".
- network / other → banner with the first line of stderr.
Keep the last good data, dimmed, with stale age in the header.

## 6. Stacks

- Group by native `stack.id`; order by `stackEntry.position`, bottom first.
- PRs without a native stack: chain by base→head (a PR whose `baseRefName` is another of these
  PRs' `headRefName` sits above it). A PR with no relations is a one-layer "standalone" group.
- Stack name (GitHub provides none): longest common prefix of the layers' head branch names, cut
  back to the last `-`/`/`/`_` separator; if that is shorter than 3 characters, use the bottom
  layer's branch name. Standalone groups are titled "standalone".
- Merged/closed layers (only present in the Workspace view, §8) sit at the bottom, are excluded
  from "mergeable through layer k of N" counts, and render dim.
- "Mergeable through layer k of N": the count of consecutive layers from the bottom that are
  *ready in order*: CI passing, approved (or `reviewDecision` null, i.e. reviews not required),
  not conflicting, not draft. `gh stack merge` merges bottom-up, so this is how far a merge would
  get today. Only the bottom layer gets ◆ (the one actionable now); upper ready-in-order layers
  show ◷ waiting. The header shows the first blocking reason at layer k+1. For a native stack
  with k ≥ 1 the header reads `◆ merge stack · k of N` (`◆ stack k/N` when narrow) followed by
  that reason; other groups keep "mergeable through layer k of N".
- Layers authored by someone else stay in the stack (needed for N and "waiting on #n") and show
  `@author` after the title; they get an attention level but don't count toward the summary.
- A stack ranks by its worst layer.

## 7. Attention model (pure functions, unit-tested)

**CI failing means a required check failed** (Jay's decision). Use `CheckRun.isRequired(
pullRequestNumber:)` (confirmed in the schema); a StatusContext counts as required when its
context name is in the branch's required list, else optional. Optional failures never trigger ▲
or ▲▲. The CI column counts required checks (`✓ 9/9`, `✗ first-failed-required`); if only
optional checks fail it shows `✓ n/m` plus `·1 opt ✗` where width allows, and the drawer lists
optional failures under their own "Optional" heading. With no required checks configured, fall
back to all checks.

Per open PR, highest wins:
- **▲▲ urgent**: CI failing AND the stack's agent is idle (§8). Whole row red background.
- **▲ needs you**: CI failing, changes requested, or conflicts with base (`DIRTY`).
- **△ nudge**: behind base, unresolved threads > 0, or non-draft with no reviewers (no requests
  and no reviews). Drafts never get the no-reviewers nudge.
- **◆ ready**: bottom open layer of a stack (or standalone), ready in order (§7), and merge
  `CLEAN`/`HAS_HOOKS`/`UNSTABLE`. On a *native* stack's bottom layer `BLOCKED` also qualifies:
  GitHub evaluates rules for the whole stack ("Able to merge as a stack"), so the per-PR
  `mergeStateStatus` can read BLOCKED although the stack merges (no API exposes stack-level
  mergeability). `UNKNOWN` stays not ready. Standalone PRs and branch chains keep BLOCKED →
  not ready.
- **◷ waiting**: everything else (upper layer blocked by a lower one, reviews pending, CI
  running).

`mergeStateStatus` → merge column: `CLEAN`, `HAS_HOOKS`, `UNSTABLE` → ✓ clean (UNSTABLE means
only non-required checks fail); `BLOCKED` → ⊘ blocked; `BEHIND` → ↓ n; `DIRTY` → ⚠ conflicts;
`UNKNOWN` → ? computing; `DRAFT` → shown via ◌ state, merge column from the other fields. For ◆,
`CLEAN`/`HAS_HOOKS`/`UNSTABLE` all qualify. A ◆ native-stack bottom layer that GitHub reports
BLOCKED shows `✓ stack ok` in the merge column, and its drawer says "ready to merge as a stack
(k of N)" with a dim note that GitHub marks the PR blocked on its own.

Agent state and ▲▲ apply in both views (Mine uses herdr data from all workspaces). When herdr is
unreachable, agent state is unknown and ▲▲ cannot fire; rows fall back to ▲.

## 8. Agents, stacks, and workspace memory

Jay's workflow: **one worktree and one Claude session per stack**, moving between layers. Rarely
the same agent fixes something on an unrelated side branch/PR.

- For each pane, cwd → (repo, branch) → PR → its stack. That pane's agent is **the agent for the
  whole stack**, whichever layer is checked out. If several panes map to one stack, prefer a
  pane running an agent, then the focused one.
- **Workspace memory**: each workspace remembers every PR branch any of its panes has checked
  out, keyed by the workspace's `worktree.checkout_path` (stable across herdr restarts, unlike
  `workspace_id`). Stored in `workspaces.json` as `{checkout_path: [{repo, number, pr_id}]}`.
  - A remembered PR stays even after the pane moves to another branch, and **even after the PR is
    merged or closed**.
  - An entry is deleted when its checkout path no longer exists on disk, or when herdr is
    reachable and no workspace uses that path.
  - A workspace with no `worktree` object falls back to keying by each pane's cwd repo root.
- ▲▲ for a remembered stack whose agent is now on a side branch: still uses the workspace
  agent's state (the workspace's agent is idle either way).
- Agent dot (● working green, ◉ blocked yellow bold, ○ idle dim, ? unknown) appears **once per
  stack**: in the Workspace view on the checked-out layer's row (which also gets `⎇`), and in the
  stack header in both views. Other layers show no dot. Stacks with no current pane show no dot.
  This replaces the mockups' dimmed "no pane" rows (board 2, #1295).
- ⏎ on any layer focuses the stack's agent pane (switching workspace if needed). No pane →
  status line "no agent pane for this stack". Disabled when herdr is unreachable.

## 9. What changed since you looked (new; board 6 option A)

**Change detection.** For each PR, keep a fingerprint and per-field "changed at" times in
`changes.json`: CI rollup state (pass/fail/running/none), each reviewer's latest state, unresolved
thread count, merge state (clean/blocked/behind/conflicts), head commit sha, open/merged/closed.
A change is: CI result flipped; a review arrived or a reviewer's state changed; unresolved threads
**increased**; merge state changed; new commit pushed; PR merged or closed. Threads decreasing does
not count. A PR's change time is its latest field change. On first sight of a PR, record the
fingerprint with no change.

**Seen.** Per PR, two times in `seen.json`: `ws_seen` and `mine_seen`.
- Each running copy detects a *visit*: its own pane becoming visible (§4: own workspace focused,
  own tab active, not hidden behind a zoomed pane). Switching into the workspace on another tab
  is not a visit. Starting up while visible counts as a visit. Two copies in one workspace both
  stamp; harmless.
- On a visit, the copy stamps the PRs it shows: a Workspace copy stamps `ws_seen`; a Mine copy
  stamps `mine_seen`. PRs that newly appear during a visit are stamped when they appear.
- Workspace view marks a PR when `change_time > ws_seen` (the value from before this visit).
- Mine view marks a PR when `change_time > max(ws_seen, mine_seen)` (values from before this
  visit).
  So seeing a PR in any Workspace view clears it everywhere; seeing it in Mine clears it only in
  Mine. Visiting workspace B never clears PRs only workspace C shows.
- Marks persist for the whole visit (computed against the pre-visit seen values); changes during
  a visit are marked and clear on the next visit.
- A PR shown in two workspaces' Workspace views: seeing it in either counts.

**Display.**
- `✦` (bold, default fg) in the gutter column of each changed row; that row's # and title bold.
  The cursor `▶` takes the gutter when both apply; the bold # and title still show the change.
- Summary line ends with `✦ n changed since you looked 14m ago` (120/80 cols) or `✦n new`
  (44/34 cols). Omitted when n = 0.
- Drawer gains a **Changed** line for a marked PR, e.g. `✗ CI failed 6m ago · @art requested
  changes 9m ago`.
- No new color: the mark is a glyph + bold.

## 10. Views

**The view is chosen at launch and fixed for the life of the process** (Jay's decision): run
`herdr-pr-pulse workspace` or `herdr-pr-pulse mine`. To change views, quit and relaunch. With no
argument it starts in the Workspace view, since a PR Pulse pane usually lives inside a working
workspace. There is no in-app view switch.

**Mine view** (boards 1, 1b, 3): open PRs authored and/or assigned, across repos. `s` cycles
both → authored → assigned (default both). Columns at 120 cols: attention, layer tree (`├─ n`,
`╭─ main`), #, state (● ready / ◌ draft), title, CI (✓ n/m, ✗ first-failed-check, ◐ running n/m,
– none), threads (◇ n / · 0), review (✓ / ✗ / ○ n pending / ∅ none), merge (✓ clean, ⊘ blocked,
↓ n behind, ⚠ conflicts, ? computing, ◷ waiting on #n). Upper stack layers show "◷ waiting on #n"
instead of a merge state. Below 120 cols, use board 3's abbreviated words and widths.

**Workspace view** (board 2): stacks this workspace has worked on (§8), including merged/closed
layers (`⊙ merged 2d ago` / `✕ closed`, dim, no attention level). Two lines per PR. Line 1: tree,
attention, #, title, agent dot. Line 2: CI, review, threads, merge. Works at 44 cols (typical)
and 34 (minimum), with board 2's narrow abbreviations. Header shows the focused pane's branch
(`⎇`).

**Row layout follows width, not view.** One shared row renderer: ≥ 120 cols → one-line table
(board 1); 80–119 → one-line abbreviated (board 3); < 80 → two-line rows (board 2, with the 34-col
abbreviations below 40). So Mine in a 44-col pane looks like board 2, and Workspace on a wide
screen looks like board 1 (with the agent dot in the stack header and `⎇` on the checked-out
row). Snapshot tests cover both views at all four widths.

Both views: header bar (reverse video) shows view, scope, PR/stack/repo counts, sort, refresh age
(⠹ while fetching). Summary line under it: counts per attention level (+ changed count).

**Drawer (␣)** opens inline below the selected row: CI (failing check names + age, count of
passing), Reviews (split **People** / **Bots** by author `__typename == "Bot"`; each reviewer's
state and unresolved thread count), Merge (detail lines), and Changed (§9). Side by side at ≥100
cols; stacked below that.

**Keys**: j/k ↑/↓ move · ⏎ jump · o open PR in browser · r refresh · s scope (Mine) · S sort
(urgency default; others: updated, repo) · h/l ←/→ fold/unfold stack (manual only; persisted in
`folds.json`) · ␣ drawer · / filter (repo, #, title text) · ? help overlay (board 5, doubles as
legend) · q / Ctrl-C quit.

**CLI**: `herdr-pr-pulse [workspace|mine] [--fixtures]`. `--fixtures` renders from
`tests/fixtures` with no network and no herdr required (it pretends to run inside the fixture
snapshot's audit-log workspace). The README's workspace-manager layout snippet shows a
`herdr-pr-pulse workspace` pane in each working workspace and a `herdr-pr-pulse mine` pane in a
home workspace.

## 11. States (board 4)

- Loading: progressive, one line per repo (✓ done / ⠹ in progress / · queued) then skeleton rows.
  With the two-step fetch, repos are known after step 1; mark each ✓ as step 2 returns. A
  refresh keeps existing rows and shows ⠹ in the header.
- Empty: friendly message + hint (e.g. "s show PRs assigned to you (2)"). Workspace view lists
  the panes and their branches, flagging branches with no PR.
- Errors: last good data dimmed, stale age in header, red banner with the fix (§5). herdr
  unreachable: agent state "?", ⏎ disabled, PR data stays live.

## 12. Visual rules

16-color ANSI only: red 1, green 2, yellow 3, cyan 6, bright black 8 (dim), default fg, reverse
video header bar. Also allowed from board 5: bright white 15 for text on the red ▲▲ row, and
the cursor row background (ANSI 0 dark / 7 light; use ratatui `Color::Reset`-safe styling or
`Modifier::REVERSED`-free selection, builder's choice). Never blue or magenta. Every color pairs
with a glyph or word. ∅ no reviewers is yellow (dim on drafts), per the legend.

## 13. Modules (proposed)

Flat `src/`, mirroring the template:

| module | role | pure? |
|---|---|---|
| `main.rs` | args (`workspace`/`mine`, `--fixtures`), env checks, event loop, spawn fetch thread | |
| `herdr.rs` | snapshot types/parse/fetch, `focus_pane` | parse yes |
| `git.rs` | cwd → (repo slug, branch), cached | |
| `github.rs` | `gh api graphql` runner, query text (`src/*.graphql` via `include_str!`), error classification | |
| `model.rs` | `Pr`, `Check`, `Review`, `MergeState`, … + GraphQL → model parsing | yes |
| `stacks.rs` | grouping, ordering, naming, "mergeable through" | yes |
| `attention.rs` | per-PR level, stack rank, summary counts | yes |
| `changes.rs` | fingerprints, change times, seen logic | yes |
| `workspaces.rs` | workspace memory, agent ↔ stack mapping | yes |
| `store.rs` | JSON files in `~/.local/state/herdr-pr-pulse/`, cache + lock | |
| `app.rs` | `App` state, key handling, view-model building | mostly |
| `ui/mod.rs`, `ui/mine.rs`, `ui/workspace.rs`, `ui/drawer.rs`, `ui/states.rs`, `ui/help.rs` | rendering | |
| `ago.rs` | relative times (copy from template) | yes |

Fetch thread: `main` sends `FetchRequest` (scheduled / forced / unknown-merge re-poll) over an mpsc
channel; the worker runs `gh` and returns `FetchResult` over a second channel; the loop drains it
with `try_recv` each tick.

## 14. Testing and fixtures

`tests/fixtures/`: recorded GraphQL responses (search + detail) and a herdr snapshot covering the
mockups' sample data: audit-log 4-layer stack (#1284 ◆, #1287 ▲▲ with idle agent, #1291, #1295
draft), docket 2-layer stack (#412 conflicts, #415), standalone #1279 (behind 6) and #1302
(blocked agent, no reviewers, merge computing), plus at least one merged layer and one bot
reviewer. A second recorded "later" response drives the what-changed tests (§9 scenario on
board 6). Unit tests for stacks, attention, changes, workspaces. insta snapshots via
`TestBackend` at 120, 80, 44, 34 cols for each view, drawer open, help, and each state.

## 15. Milestones

1. Scaffold from template, fixtures, Mine view render (`--fixtures`), insta snapshots.
2. Live GitHub data: two-step query, shared cache + lock, fetch thread, UNKNOWN re-poll, error
   classification.
3. herdr integration: agent ↔ stack mapping, workspace memory, Workspace view, ⏎ jump.
4. Drawer, filter, sort, fold persistence, help overlay.
5. Loading / empty / error states.
6. What changed since you looked (§9).

## 16. Calls made in this design (check these)

- Two GraphQL requests per refresh instead of one, plus a shared cache across copies (cost).
- Stack names derived from branch-name common prefix.
- Merged/closed glyphs `⊙ merged` / `✕ closed`, dim.
- Workspace memory keyed by worktree checkout path; cleanup rules in §8.
- Agent dot once per stack; mockups' dimmed "no pane" rows dropped.
- Spec wins over mockups: ▲▲ shown in Mine; `s` cycles three scopes; ∅ yellow.
- Drawer side-by-side threshold 100 cols.
- Extra sorts: updated, repo.
- With no view argument, PR Pulse starts in the Workspace view.
- A visit requires PR Pulse's pane to be on screen (own tab active, not zoomed away), not just
  its workspace focused.
- "Mergeable through k" counts layers ready in order; only the bottom one shows ◆.
- Stacks always shown whole in both views, fetching missing layers (incl. colleagues' and merged
  ones) in a third request when needed; colleagues' layers show `@author`.
- Merge-state mapping in §7; `reviewDecision` null counts as approved.
- Row layout chosen by width, shared across views.
- Optional-check failures shown as `·1 opt ✗` and in the drawer; repos with no required checks
  fall back to all checks.

## 17. Left to the builder

Exact column widths beyond the boards; spinner frames; lock-file mechanics; query pagination
beyond 50; internal type names; how fixtures are recorded and anonymized.
