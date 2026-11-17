# `p`: Review in Plannotator — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `p` opens the selected PR in `plannotator review`. In Mine/Workspace, any feedback goes
to the stack's agent pane through `herdr agent prompt`.

**Architecture:** A new `src/plannotator.rs` owns everything about a review once it starts. It
spawns `plannotator review --json <url>` on a thread, keeps the set of PRs under review, takes
finished results over a channel, and settles each one: forward to the agent, save to a file, or
just report. The key handlers in `App` and `ReviewApp` only decide what to ask for. They return a
new `Action::Review`, or `Action::Bell` when there is no agent. The two event loops' `Live`
structs own a `Reviews` and poll it every tick, the same way they poll fetch events.

**Tech Stack:** Rust, ratatui/crossterm, serde_json, chrono. The `plannotator` and `herdr` CLIs
are run as child processes.

**Spec:** `docs/superpowers/specs/2026-11-17-plannotator-review-key-design.md`

## Global Constraints

- Key: `p`, in all three views. No other keys change.
- Command: `plannotator review --json <PR url>`, stdin null, stdout piped, stderr null. No other flags.
- Forward to the agent only when the JSON's `noop` is `false`. Forward the JSON's `message` verbatim.
- Deliver with `$HERDR_BIN_PATH agent prompt <pane_id> <message>` (fall back to `herdr`, as `herdr_bin()` does).
- Failed deliveries are saved to `<state dir>/feedback/<owner>-<repo>-<N>-<YYYYmmddTHHMMSS>.md`
  (`/` in the repo becomes `-`). State dir: `StateDir::default_location()`.
- The UI thread never waits on Plannotator or `herdr agent prompt`. Both run off the event loop.
- Status copy, exactly:
  - `reviewing #N in Plannotator…`
  - `#N already open in Plannotator`
  - `no agent pane for #N; not opening Plannotator`
  - `herdr unreachable; not opening Plannotator`
  - `select a PR`
  - `feedback on #N sent to agent`
  - `#N approved`
  - `#N review dismissed`
  - `#N review closed, nothing to send`
  - `feedback on #N saved to <path>: <reason>`
  - `Plannotator failed for #N: <error>`
  - `fixtures: would review <url>`
- Review view: on success, show no status. Errors still show `Plannotator failed for #N: <error>`.

## Review Focus

1. **Plannotator prints extra lines before the JSON** (notices, URL lines). Parse the last line
   that starts with `{`, and don't fail on the rest. Test: `parse_takes_last_json_line`.
2. **The agent pane is blocked on a permission prompt when the review ends.** `herdr agent prompt`
   exits non-zero. The feedback must be saved and the path shown, not dropped. Test:
   `settle_saves_when_delivery_fails`.
3. **Plannotator isn't installed, or exits non-zero with no JSON.** The PR must leave the running
   set, or `p` says "already open" forever. Test: `failed_run_clears_running`.
4. **Messages with newlines and shell metacharacters.** Pass the message as one argv element,
   never through a shell. Test: `prompt_args_keep_message_as_one_arg`.
5. **Pressing `p` on a stack header.** It must act on the same PR as `o` (the first row). Test:
   `p_on_header_reviews_first_row`.

---

### Task 1: Parse and settle a finished review

**Files:**
- Create: `src/plannotator.rs` (add `mod plannotator;` in `src/main.rs`)
- Modify: `src/herdr.rs` (add `prompt_args`, `prompt_agent`)
- Modify: `src/store.rs` (add `pub fn dir(&self) -> &Path` to `StateDir`)

**Interfaces:**
- Produces (in `plannotator`):
  - `pub enum Decision { Approved, Annotated, Dismissed }`
  - `pub struct Outcome { pub decision: Decision, pub message: String, pub noop: bool }`
  - `pub fn parse_result(stdout: &str) -> Result<Outcome, String>`
  - `pub struct Target { pub url: String, pub repo: String, pub number: u32, pub pane: Option<String> }` (`Clone, Debug, PartialEq, Eq`). `pane: None` means don't forward (Review view).
  - `pub fn settle(target: &Target, result: Result<Outcome, String>, deliver: &dyn Fn(&str, &str) -> Result<(), String>, feedback_dir: &Path, now: DateTime<Local>) -> Option<String>`. Returns the status line, or `None` (Review view success).
- Produces (in `herdr`): `pub fn prompt_args(pane_id: &str, text: &str) -> Vec<String>` and `pub fn prompt_agent(pane_id: &str, text: &str) -> Result<(), String>`. On failure the error includes stderr, in the same style as `focus_pane`.

- [ ] **Step 1: Write the failing tests** in `src/plannotator.rs` `#[cfg(test)] mod tests`:

```rust
#[test]
fn parse_takes_last_json_line() {
    let out = "Opening http://localhost:1234\n{\"v\":1,\"surface\":\"review\",\"decision\":\"annotated\",\"message\":\"fix x\\nand y\",\"noop\":false,\"annotationCount\":2}\n";
    let o = parse_result(out).unwrap();
    assert_eq!(o, Outcome { decision: Decision::Annotated, message: "fix x\nand y".into(), noop: false });
}
#[test]
fn parse_rejects_output_without_json() {
    assert!(parse_result("boom\n").is_err());
}
// settle: Target { url: "https://github.com/heard/heard-ts/pull/1287", repo: "heard/heard-ts", number: 1287, pane: Some("w1:p1") }
// now = 2026-11-17T14:30:00 local; feedback_dir = tempdir
#[test] fn settle_forwards_when_not_noop()        // deliver called with ("w1:p1", message); returns Some("feedback on #1287 sent to agent")
#[test] fn settle_approved_with_notes_forwards()  // decision Approved, noop false → delivered, same status
#[test] fn settle_noop_sends_nothing()            // Approved/noop → Some("#1287 approved"); Dismissed → Some("#1287 review dismissed"); Annotated/noop → Some("#1287 review closed, nothing to send"); deliver never called
#[test] fn settle_saves_when_delivery_fails()     // deliver returns Err("agent_blocked"); file feedback_dir/heard-heard-ts-1287-20261117T143000.md contains the message; status == format!("feedback on #1287 saved to {}: agent_blocked", path.display())
#[test] fn settle_run_error_reports()             // result Err("not found") → Some("Plannotator failed for #1287: not found"), nothing saved
#[test] fn settle_without_pane_is_silent()        // pane None, Annotated/noop false → None, deliver never called
```

In `src/herdr.rs` tests:

```rust
#[test]
fn prompt_args_keep_message_as_one_arg() {
    let msg = "line 1\n`rm -rf` $HOME; \"q\"";
    assert_eq!(prompt_args("w1:p1", msg), vec!["agent", "prompt", "w1:p1", msg]);
}
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test plannotator:: herdr::tests::prompt_args`
Expected: compile errors (the items don't exist yet).

- [ ] **Step 3: Implement.** Use serde `Deserialize` for `Outcome`. Use `rename_all = "lowercase"` on `Decision` and ignore unknown fields. If the save itself fails, return `feedback on #N not delivered or saved: <reason>; <io error>`. Create `feedback_dir` with `create_dir_all`. `prompt_agent` uses `Command::new(herdr_bin()).args(prompt_args(..)).output()`.

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test plannotator:: herdr::` then `cargo clippy --all-targets`
Expected: all pass, no warnings.

- [ ] **Step 5: Commit**

```bash
git add src/plannotator.rs src/herdr.rs src/store.rs src/main.rs
git commit -m "Parse Plannotator review results and deliver feedback to an agent"
```

---

### Task 2: Run reviews in the background

**Files:**
- Modify: `src/plannotator.rs`

**Interfaces:**
- Consumes: `Target`, `Outcome`, `parse_result`, `settle` (Task 1); `herdr::prompt_agent`.
- Produces:
  - `pub type Runner = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>` takes the URL and returns stdout. `real_runner()` runs the Global Constraints command and returns `Err` on a spawn failure or a non-zero exit.
  - `pub struct Reviews` with:
    - `pub fn new(feedback_dir: PathBuf) -> Reviews` (real runner, delivers with `herdr::prompt_agent`)
    - `pub fn with(runner: Runner, deliver: Box<dyn Fn(&str, &str) -> Result<(), String>>, feedback_dir: PathBuf) -> Reviews` (for tests)
    - `pub fn start(&mut self, target: Target) -> String`. Returns `reviewing #N in Plannotator…`, or `#N already open in Plannotator` if `target.url` is already running, in which case nothing is spawned.
    - `pub fn poll(&mut self) -> Vec<String>`. Non-blocking (`try_recv`). Removes each finished URL from the running set, calls `settle`, and returns the `Some` statuses.
- The worker thread only runs the runner and sends `(Target, Result<Outcome, String>)`. `settle` (including `herdr agent prompt`) runs in `poll`, on the UI thread. *Decision:* `herdr agent prompt` without `--wait` returns in milliseconds, and `settle` stays single-threaded and easy to test.

- [ ] **Step 1: Write the failing tests** (fake runner on a channel, so the test controls when the review finishes):

```rust
#[test] fn start_twice_same_pr_is_refused()     // second start → "#1287 already open in Plannotator"; runner called once
#[test] fn poll_settles_and_frees_the_pr()      // runner returns annotated JSON → poll() == ["feedback on #1287 sent to agent"]; start again → "reviewing #1287 in Plannotator…"
#[test] fn failed_run_clears_running()          // runner Err("exit status: 1") → poll() == ["Plannotator failed for #1287: exit status: 1"]; start again spawns
#[test] fn different_prs_run_together()         // start #1287 and #1291 → both "reviewing…"
```

Poll in a short loop with a 2 s cap (the result arrives from a thread). No sleeps beyond that.

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test plannotator::tests::start_ plannotator::tests::poll_ plannotator::tests::failed_run plannotator::tests::different_`
Expected: compile errors.

- [ ] **Step 3: Implement** `Reviews`, holding a `HashSet<String>` of running URLs and an `mpsc` pair. Write the failure message from `real_runner` as `exit status: N`, or `could not run plannotator: <e>`.

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test plannotator::` and `cargo clippy --all-targets`
Expected: pass, no warnings. (`Reviews::new` may be unused until Task 4. Add `#[allow(dead_code)]` only if clippy fails, and remove it in Task 4.)

- [ ] **Step 5: Commit**

```bash
git add src/plannotator.rs
git commit -m "Run Plannotator reviews in the background, one per PR"
```

---

### Task 3: The `p` key in all three views

**Files:**
- Modify: `src/app.rs` (`Action`, `handle_key`, tests)
- Modify: `src/review/app.rs` (`handle_key`, tests)

**Interfaces:**
- Consumes: `plannotator::Target` (Task 1).
- Produces: two new `Action` variants, `Review(Target)` and `Bell`. `Bell` means ring the terminal bell. The status line is already set.
- Mine/Workspace (`App::handle_key`, `KeyCode::Char('p')`):
  - `selected_pr()` is `None` → status `select a PR`, return `Action::None`.
  - `!self.herdr_ok` → status `herdr unreachable; not opening Plannotator`, return `Action::Bell`.
  - `selected_group().and_then(|g| g.agent)` is `None` → status `no agent pane for #N; not opening Plannotator`, return `Action::Bell`.
  - Otherwise return `Action::Review(Target { url, repo, number, pane: Some(dot.pane_id) })`.
- Review view (`ReviewApp::handle_key`): `selected_pr()` is `Some(p)` → `Action::Review(Target { .., pane: None })`, else status `select a PR` and `Action::None`.

- [ ] **Step 1: Write the failing tests**, next to the existing `enter_*` and `o_opens_selected_pr` tests, reusing their fixtures:

```rust
// src/app.rs
#[test] fn p_reviews_with_stack_agent()          // like enter_focuses_stack_agent: select 1291 → Action::Review(Target{ url: ".../heard-ts/pull/1291", repo: "heard/heard-ts", number: 1291, pane: Some("w1:p1") })
#[test] fn p_on_header_reviews_first_row()       // cursor on the stack header → Target.number is the first row's (same PR `o` gives)
#[test] fn p_without_agent_bells()               // same setup as enter_without_agent_sets_status → Action::Bell; status "no agent pane for #N; not opening Plannotator"
#[test] fn p_with_herdr_unreachable_bells()      // same setup as enter_with_herdr_unreachable_sets_status → Action::Bell; status "herdr unreachable; not opening Plannotator"
// src/review/app.rs
#[test] fn p_reviews_selected_pr_without_pane()  // on #1298 row → Action::Review(Target{ url: "https://github.com/heard/heard-ts/pull/1298", repo: "heard/heard-ts", number: 1298, pane: None })
#[test] fn p_on_group_header_asks_for_a_pr()     // cursor on a group header → Action::None, status "select a PR"
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test p_`
Expected: compile errors (`Action::Review` doesn't exist).

- [ ] **Step 3: Implement** the variants and key arms. Each `run_action` gets a temporary `_ => {}` arm, or the arms Task 4 adds, so that it compiles. `src/main.rs`'s `run_action` matches exhaustively, so this task adds `(Action::Review(_) | Action::Bell, _) => {}` for now.

- [ ] **Step 4: Run the tests and confirm they pass**

Run: `cargo test` and `cargo clippy --all-targets`
Expected: all pass.

- [ ] **Step 5: Commit**

```bash
git add src/app.rs src/review/app.rs src/main.rs
git commit -m "p key: ask to review the selected PR in Plannotator"
```

---

### Task 4: Wire it into the event loops, hints, help and README

**Files:**
- Modify: `src/main.rs` (`Live`, `run_action`, tests)
- Modify: `src/review/mod.rs` (`Live`, `run_action`, tests)
- Modify: `src/ui/chrome.rs`, `src/ui/review.rs` (footer hints), `src/ui/help.rs` (both KEYS sections)
- Modify: `README.md` (both "Keys:" paragraphs)
- Update: affected `src/ui/snapshots/*.snap`

**Interfaces:**
- Consumes: `Reviews::{new, start, poll}` (Task 2); `Action::{Review, Bell}` (Task 3).
- Both `Live` structs gain `reviews: Reviews`, built with `Reviews::new(StateDir::default_location().dir().join("feedback"))`. In `tick`, `for s in self.reviews.poll() { app.set_status(s) }`.
- `run_action`:
  - `(Action::Review(t), Some(live))` → `app.set_status(live.reviews.start(t))`
  - `(Action::Review(t), None)` → `app.set_status(format!("fixtures: would review {}", t.url))`
  - `(Action::Bell, _)` → write `"\x07"` to stdout and flush; ignore the io error.
- Hints: add `("p", " review  ")` after `o` in Wide, and `("p", " review ")` in Medium if the 80-column snapshots still fit (otherwise Wide only). Leave Narrow and Tiny alone. Help: `k("p", "review in Plannotator")` after the `o` line, in both views. README: add "`p` review in Plannotator (Mine/Workspace send your feedback to the stack's agent)" to the Mine/Workspace keys, and "`p` review in Plannotator" to the Review keys.

- [ ] **Step 1: Write the failing tests** next to the existing `fixtures_actions_only_set_status` tests:

```rust
// src/main.rs and src/review/mod.rs
#[test] fn fixtures_review_only_sets_status()
// run_action(app, Action::Review(Target{ url: "https://x/1", repo: "o/r", number: 1, pane: None }), None) → false;
// status == "fixtures: would review https://x/1"
```

- [ ] **Step 2: Run them and confirm they fail**

Run: `cargo test fixtures_review`
Expected: FAIL (the placeholder arm sets no status).

- [ ] **Step 3: Implement** the wiring, hints, help and README lines above.

- [ ] **Step 4: Update snapshots and run everything**

Run: `cargo test`, then `cargo insta review` (accept only diffs that add the `p` hint or help line), then `cargo test` and `cargo clippy --all-targets` again.
Expected: all pass. Snapshot diffs touch only footer/help lines.

- [ ] **Step 5: Manual check in herdr** (can't be automated):
  1. `cargo run -- mine` in a herdr pane. Press `p` on a stack with an agent. Plannotator opens, and the status shows `reviewing #N in Plannotator…`.
  2. Add an annotation and send it. The agent pane receives the message, and the status shows `feedback on #N sent to agent`.
  3. Press `p` on a stack with no agent. You hear a bell, Plannotator doesn't open, and the status explains why.
  4. `cargo run -- review`: `p` opens Plannotator. Closing it shows no status.

- [ ] **Step 6: Commit**

```bash
git add src/main.rs src/review/mod.rs src/ui README.md
git commit -m "Open Plannotator from every view and forward feedback to the agent"
```
