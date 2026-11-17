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
   - The JSON carries `noop`. When `noop` is false (annotations, or "approve with notes") →
     `herdr agent prompt <pane> <message>`. Status: `feedback on #N sent to agent`.
   - When `noop` is true, nothing is sent. Status by decision: `#N approved`,
     `#N review dismissed`, or `#N review closed, nothing to send` (`annotated` with no feedback,
     or feedback Plannotator already posted to GitHub).
   - Unparseable output or a non-zero exit → status shows the error.

Which pane: the stack's agent pane, as `⏎` resolves it, captured when `p` is pressed. *My call:*
capture at press time, so the feedback goes to the agent that owned the PR when Jay started,
even if he moves the cursor or the stack changes.

When there's no agent pane (or herdr is unreachable) at press time, in Mine/Workspace:
Plannotator does not open (Jay's decision). PR Pulse rings the terminal bell and shows
`no agent pane for #N; not opening Plannotator` (or `herdr unreachable; …`) in the status line,
like `⏎` reports a missing agent today. *My call:* bell plus status line rather than a modal
dialog. The app has no dialogs, and the status line clears on the next key.

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
