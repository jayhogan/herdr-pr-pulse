# `p`: review a PR in Plannotator — design

Date: 2026-11-17 · Status: draft for approval · Size: small change

## 1. Why

Jay reviews PRs in Plannotator's browser UI. Today he copies the PR URL out of PR Pulse and runs
`plannotator review <url>` himself. Where the feedback should go depends on whose PR it is:

- **Review view** (other people's PRs): the feedback belongs on GitHub. Plannotator's PR review
  UI already submits a GitHub review with line comments.
- **Mine and Workspace views** (Jay's own PRs): the feedback belongs with the agent working on
  that PR, so it can act on it. PR Pulse already knows that agent: it is the stack's agent pane,
  the one `⏎` jumps to.

## 2. Goals, non-goals, anti-goals

Goals
- `p` on a selected PR opens it in Plannotator's review UI, in all three views.
- Mine/Workspace: when Jay finishes with annotations, PR Pulse sends Plannotator's feedback
  message to the stack's agent pane.
- The status line says what happened: review open, feedback sent, or why it wasn't.

Non-goals
- No posting to GitHub from PR Pulse. In Review view Plannotator does the posting.
- No forwarding in Review view. Plannotator's output is dropped.
- No new Plannotator options. Defaults apply, including the local checkout (`--local`).

Anti-goals
- Blocking the UI while the review is open. The review can take many minutes.
- Losing feedback silently. If it can't be delivered, Jay must be told, and the text must be kept.
- Interrupting an agent when there is nothing to act on. A bare approval (no annotations) or a
  dismissal sends nothing.

## 3. Approaches

- **A. PR Pulse waits and forwards (chosen by Jay).** `p` spawns
  `plannotator review --json <url>` on a background thread. When it exits, PR Pulse reads the
  JSON `{decision, message}`. For `annotated` it runs `herdr agent prompt <pane> <message>`.
  Works with any agent. The agent sees nothing until there is feedback.
- B. Send the agent `/plannotator-review <url>`. Only works with Claude sessions that have the
  skill and are idle, and the agent spends context waiting. Rejected.
- C. Run Plannotator in a new herdr pane, with a wrapper that forwards the result. Same as A with
  more parts. Rejected.

## 4. Design

Key: `p`. It is unused in all views. It goes in the key hints next to `o`, in both help overlays,
and in the README key lists.

Flow (all views):
1. The PR is the one `o` would open (on a stack header, its first row). With nothing to open:
   status `select a PR`, nothing runs.
2. Spawn `plannotator review --json <url>` with stdin null and stdout captured, on a thread that
   waits for exit. Status: `reviewing #N in Plannotator…`.
3. Review view: on exit, discard the output. A non-zero exit or spawn failure shows the error in
   the status line.
4. Mine/Workspace: on exit, the thread sends the result back to the event loop over a channel,
   like fetch results. Then:
   - stdout's JSON is only `{decision, message}`. Feedback is sent with
     `herdr agent prompt <pane> <message>` when there is some: `annotated` with a non-empty
     message, or `approved` whose message isn't Plannotator's bare-approval text (that is,
     approved with notes). Status: `feedback on #N sent to agent`.
   - Otherwise nothing is sent. Status by decision: `#N approved`, `#N review dismissed`, or
     `#N review closed, nothing to send`.
   - A non-zero exit → status shows the error, with Plannotator's last stderr line.
   - Unparseable output → saved to the feedback directory like a failed delivery, since it
     may hold feedback.

Which pane: the stack's agent pane, as `⏎` resolves it, captured when `p` is pressed. *My call:*
capture at press time, so the feedback goes to the agent that owned the PR when Jay started,
even if he moves the cursor or the stack changes.

### Revision 2026-11-17: the agent's checkout, not the PR link (Jay's feedback after a live test)

Jay wants feedback on his own PRs to come back to the agent, not go to GitHub. Plannotator's PR
review always offers "post to GitHub", and when used, stdout carries only
`Pull request reviewed on GitHub: <url>`. Plannotator has no switch to hide that button, so in
Mine/Workspace PR Pulse reviews the agent's worktree instead, which has no GitHub posting.

Mine/Workspace, `p` on a PR (the one `o` would open):
- **The stack's agent is on this PR's branch** (the row the agent dot sits on): run
  `plannotator review --json --base origin/<PR base branch> <agent's worktree toplevel>`. The
  feedback is forwarded to the agent as above. Status: `reviewing #N in Plannotator…`.
- **Otherwise** (no agent on the stack, a layer the agent isn't on, or herdr unreachable): fall
  back to `plannotator review --json <PR url>`, with nothing forwarded (Jay's decision). Status:
  `reviewing #N on GitHub in Plannotator (no agent)`. This replaces the earlier "no agent → bell,
  don't open" rule. The bell is gone.

Approaches considered: reviewing the agent's checkout (chosen); reviewing `gh pr diff` through
`--patch-file` (diff only, and only what's pushed); keeping the PR link (feedback can still go to
GitHub). For layers the agent isn't on, Jay chose the PR link over the patch-file fallback.

Narrow key hints (40-79 columns) gain `p review`. `j/k move` shortens to `j/k`, so the row fits
in 40 columns.

When delivery fails (agent blocked on a permission prompt, or the pane closed or herdr went down
during the review):
write the message to `~/.local/state/herdr-pr-pulse/feedback/<repo>-<N>-<timestamp>.md` and show
`feedback on #N saved to <path>: <reason>`. *My call:* this meets the "never lose feedback"
anti-goal without retry logic.

Duplicates: while a review of a PR is running, `p` on that same PR shows `#N already open in
Plannotator` and doesn't spawn another. *My call.* Reviews of different PRs can run at once.

Fixtures mode: status `fixtures: would review <url>`, like `o`.

Quitting PR Pulse while a review is open: the child keeps running (Plannotator stays usable),
and its feedback is not forwarded. *My call:* accepted, not worth handling.

## 5. Left to the builder

- Where the running-review set and the result channel live (`App` vs `Live` in `main.rs`).
- Whether `Action::Review { url, pane }` is one variant or Review view uses a separate one.
- Exact status wording and the narrow-width key hint layout. Snapshots get updated.
- Tests: key handling (`p` with/without selection, duplicate guard), JSON parsing of the three
  decisions, and the forward/save branch with herdr calls stubbed the way existing tests do.
