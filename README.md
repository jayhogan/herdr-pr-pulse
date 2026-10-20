# herdr-pr-pulse

A [herdr](https://herdr.dev) pane that shows your open GitHub PRs and stacked-PR
chains, ranked by what needs you first: failing required checks, requested
changes, conflicts, or a PR that is ready to merge. The Workspace view shows the
stacks for the current workspace, with the agent working on each one. The Mine
view shows everything you authored or are assigned.

```
 PR Pulse │ 8 PRs · 4 stacks                            sort: urgency ▾ │ ↻ 18s
 ▲▲1 urgent  ▲1 need you  ◆1 merge  △3 nudge  ◷2 waiting
────────────────────────────────────────────────────────────────────────────────
  ! LYR #       TITLE                 CI            TH  REV   MERGE
▾ heard-ts·audit ◆▲△◷ ○  ◆ merge stack · 1 of 4 · L2 CI ✗
   ╭─ main
  ◆ ├1  #1284 ● audit: schema for au… ✓ 9/9         ·   ✓ ok  ✓ merge now
▶▲▲ ├2  #1287 ● audit: kafka produce… ✗ test-integ… ◇3  ✗ chg ◷ on #1284
  △ ├3  #1291 ● audit: consumer writ… ✓ 9/9         ◇1  ○ 2   ◷ on #1287
  ◷ └4  #1295 ◌ audit: viewer UI      ◐ 5/9         ·   ∅     ◷ on #1291

▾ docket·thread-model ▲◷ ●  ok through 0/2 · L1 ⚠ conflicts
   ╭─ main
  ▲ ├1  #412  ● thread model          ✓ 6/6         ◇2  ✓ ok  ⚠ conflicts
  ◷ └2  #415  ● thread sidebar        ✓ 6/6         ·   ○ 1   ◷ on #412

▾ heard-ts·standalone △  update needed · behind main 6
  △ ──  #1279 ● pii: redact SSN in t… ✓ 7/7         ·   ✓ ok  ↓ behind 6

▾ heard-ts·standalone △ ◉  ∅ reviewers
  △ ──  #1302 ● notes: tokenizer      ◐ 4/7         ·   ∅     ? computing

────────────────────────────────────────────────────────────────────────────────
 j/k ␣ info ⏎ jump o open r ↻ s mine⇄asgn S sort h/l fold ? help
```

A `✦` marks a PR that changed since you last looked. Press `?` in the app for
the full legend.

## Install

Requires a Rust toolchain (1.89+) and the `gh` CLI, logged in (`gh auth status`).
If your org uses SAML SSO, authorize the `gh` token for the org, or GitHub
returns nothing for its repos.

```sh
git clone <this repo> ~/dev/herdr-pr-pulse
herdr plugin link ~/dev/herdr-pr-pulse   # builds, registers the plugin
~/dev/herdr-pr-pulse/scripts/install.sh  # puts the binary on PATH
```

## Use

Run `herdr-pr-pulse workspace`, `herdr-pr-pulse mine` or `herdr-pr-pulse review`
([below](#review-view)) in a pane; the view is chosen at launch. With herdr-plugin-workspace-manager:

```yaml
# in each working workspace
panes:
  - agent: claude
  - command: herdr-pr-pulse workspace
    persist: false
    size: "30%"
# in a home workspace
panes:
  - command: herdr-pr-pulse mine
    persist: false
```

Keys: `j`/`k` or arrows move, `␣` details, `⏎` jump to the PR's agent pane, `o`
open in browser, `r` refresh, `s` authored/assigned (Mine), `S` sort, `h`/`l`
fold a stack, `/` filter, `?` help, `q` quits. `⏎` uses `herdr agent focus`, so
it works for agent panes.

The layout adapts to pane width: Wide (120+ columns), Medium (80-119), Narrow
(40-79), Tiny (under 40).

## Review view

`herdr-pr-pulse review` lists other people's PRs that are waiting on review, in the repos
you configure. It shows PRs that are marked ready (not drafts), skips dependabot's and
your own, and groups them by author, with the longest-waiting first. READY is the time since
the PR was last marked ready for review, not since it was opened.

```
 PR Pulse · Review │ 9 PRs · 5 people · 3 repos     group: user  show: not approved │ ↻ 18s
 ★2 code owner  ↩1 waiting on author
─────────────────────────────────────────────────────────────────────────────────────────
     #      TITLE                                          REPO          USER          READY
▶▾ maria-chen  3 PRs · oldest 4d
  ★  #1298  billing: retry failed Stripe webhooks          heard-ts      maria-chen    4d
  ★  #1312  billing: proration for mid-cycle plan change   heard-ts      maria-chen    2d
     #1320  billing: invoice PDF footer copy               heard-ts      maria-chen    5h

▾ dan-o  3 PRs · oldest 3d
     #87    tax: 2026 bracket table                        tax-engine    dan-o         3d
  ↩  #91    tax: quarterly estimate rounding               tax-engine    dan-o         1d
```

- `★` (yellow): GitHub requested a review from one of your teams as code owner. GitHub
  drops that request once someone on the team reviews, so the star goes away then.
- `↩` (dim row): you requested changes and the author hasn't pushed since.

Configure the repos in `~/.config/herdr-pr-pulse/config.toml` (or under
`$XDG_CONFIG_HOME`). The file is read when the pane starts, so relaunch it after a change:

```toml
[review]
repos = ["Heard-Mental-Health/heard-ts", "Heard-Mental-Health/heard-app"]
```

Keys: `j`/`k` move, `␣` opens a PR's details (first 5 lines of the description, CI,
other reviewers) or folds a group, `h`/`l` fold, `o` open in browser, `r` refresh, `g`
group by user or repo, `a` switch between not approved and approved, `?` help, `q` quits.
The ★ needs your teams, so the `gh` token needs `read:org` (`gh auth refresh -s read:org`).
The view doesn't need a herdr pane; it runs in any terminal.

## How it works

- Each refresh is two GraphQL requests through `gh`: a cheap search for PR ids,
  then a detail query for those PRs plus any remembered by a workspace, with one
  more request to fill in stack layers that are missing.
- Every running copy shares one fetch. Results go to
  `~/.local/state/herdr-pr-pulse/cache.json`; a copy fetches only when the cache
  is over 60 s old (guarded by a lock file), and `r` forces one. herdr is read
  every 2 s.
- The Review view keeps its own `review-cache.json`, fetch lock and `review-folds.json`
  in the same directory, so it never waits on the other views' fetch.
- The state directory also holds `workspaces.json` (PRs seen per checkout),
  `changes.json` (field fingerprints and change times), `seen.json` (last looked
  at times) and `folds.json`.
- CI counts as failing only when a required check fails. Optional failures show
  as `·1 opt ✗` and never raise attention.
- A visit is your pane becoming visible. A PR is marked changed when it changed
  after you last saw it: Workspace views compare to the Workspace seen time, Mine
  compares to the later of the Workspace and Mine seen times, so seeing a PR in
  any Workspace view clears it everywhere.

## Development

```sh
cargo test
cargo clippy --all-targets
cargo run -- mine --fixtures      # no network, no herdr
cargo run -- review --fixtures    # the Review view on fixture data
cargo insta review                # after a UI snapshot change
cargo test live_ -- --ignored     # hits GitHub with your gh login
```
