# PR Pulse Review view — design

Date: 2026-10-20 · Status: draft for approval

Builds on `2026-10-17-pr-pulse-design.md` (the "base spec"). Everything not changed here (visual
rules, width tiers, fetch lock, error banners, help overlay, fixtures mode) carries over.

## 1. Why

Jay leads several initiatives. Each one has engineers whose PRs he is responsible for reviewing.
Today he rebuilds the same GitHub search for each engineer (author, not draft, not approved). Then
he tries to work out which PR has waited longest, but GitHub shows age since *creation*. A PR
opened two weeks ago as a draft and marked ready this morning looks older than one that has been
waiting four days. With several engineers across several initiatives, he can't tell what to
review next.

The Review view is one pane that answers two questions:

1. **Triage:** "Which PR has been waiting on reviewers longest?" Age since marked ready drives this.
2. **Find:** "Where's the PR someone said at standup was coming soon?" Jay spots it by scanning
   titles, or by opening the details to read the description.

## 2. Goals, non-goals, anti-goals

Goals
- One list of ready, not-yet-approved PRs by other people across the configured repos, grouped by
  engineer, with the longest-waiting PRs first.
- Age counts from when the PR was marked ready, not from when it was created.
- PRs where Jay's team is a requested code owner stand out (★), so he can prioritize them.
- The details show enough (description, CI, other reviewers) to decide whether to open the PR.

Non-goals
- No text filter (`/`) or search. The team is small and scanning is enough (Jay's decision).
- No mutations: no approving, commenting, or requesting reviewers. Read-only, like the base app.
- No herdr agent integration: no ⏎ jump and no agent dots. These are other people's PRs.
- No "what changed since you looked" marks (base spec §9) in this view.
- No per-initiative or per-engineer allow-list. Grouping by user is how Jay narrows by eye.

Anti-goals
- Blocking the UI on the network.
- Using up the GraphQL rate limit that the other views and Claude sessions share.
- Showing a PR as "waiting" when it is actually waiting on its author (see §5 dimming).

## 3. Launch and configuration

- New positional view: `herdr-pr-pulse review` (adds `ViewKind::Review`). As with the other views,
  the view is chosen at launch and fixed for the life of the process. `--fixtures` works with it.
- **Config file (new):** `$XDG_CONFIG_HOME/herdr-pr-pulse/config.toml`, else
  `~/.config/herdr-pr-pulse/config.toml`. This is the app's first config file. The other views
  ignore it.

  ```toml
  [review]
  repos = ["heard/heard-ts", "heard/docket", "heard/tax-engine"]
  ```

- A missing file, a missing `[review]` table, or an empty `repos` list → the empty state shows the
  file path and a sample snippet. A parse error → an error banner with the file path and the
  parser's message. The file is read at startup, so editing it requires relaunching the pane
  (my call).
- **No GitHub username to configure.** The viewer's login comes from `gh`, as it does today. Jay's
  team memberships are looked up through the same login (§4).

## 4. Data

### Which PRs show

Open PRs in the configured repos that are all of:
- not draft (`isDraft == false`)
- not authored by dependabot (`author.login` is `dependabot` or `dependabot[bot]`, or any `Bot`
  whose login starts with `dependabot`)
- not authored by the viewer
- in the current approval filter (§5)

### Fields per PR (beyond the base `Pr` fields already fetched)

| Need | Source |
|---|---|
| Ready time | Latest `READY_FOR_REVIEW_EVENT` in `timelineItems`; if none, `createdAt`. Going back to draft and ready again restarts the clock (my call). |
| Description | `body`. First 5 non-empty lines after stripping HTML comments (PR templates often carry them). Markdown is shown as plain text. |
| Code owner | `reviewRequests.nodes.asCodeOwner` + `requestedReviewer ... on Team { slug organization { login } }` |
| Viewer's teams | `organization(login: <org>) { teams(first: 100, userLogins: [<viewer>]) { nodes { slug } } }`, once per distinct org among the configured repos |
| Waiting on author | Viewer's latest review state is `CHANGES_REQUESTED`, and the head commit's `committedDate` (or `pushedDate` where present) is not later than that review's `submittedAt` |
| Approved | See §5 |

CI and reviewers reuse the base model's `Ci` and `Reviewer`.

### Code-owner signal

A PR is **code-owner** (★) when any review request has `asCodeOwner == true` and names a team the
viewer belongs to. CODEOWNERS at Heard always names teams, not individuals (Jay). Jay's teams don't
use review auto-assignment (which would replace the team request with named members), and none of
them is a catch-all `*` owner, so the signal stays meaningful (Jay). A request that names the
viewer directly as code owner also counts.

**Checked (2026-10-20):** GitHub removes a team's code-owner request once a team member reviews
(heard-ts #1787, #1771). Jay accepted this: ★ shows until someone on the team has reviewed, and no
CODEOWNERS-file fallback is built.

### Fetching

- Own cache file `review-cache.json`, separate from `cache.json`, with the same 60 s freshness,
  lock, and "one fetch shared by every running copy" behavior. `r` forces a fetch.
- Step 1: one search, `is:pr is:open draft:false archived:false repo:A repo:B …` (one `repo:`
  qualifier per configured repo), excluding `author:@me` and dependabot. Paginate past 50 if
  needed. The client-side filters in "Which PRs show" still apply, so the result doesn't depend
  on exactly how search handles exclusions.
- Step 2: a detail query in chunks, as in the base app, but with **its own slim fragment**: only
  the fields this view shows or filters on (identity, title, author, draft, `reviewDecision`,
  ready time, `body`, CI rollup, latest reviews, review requests, head commit time). No stack,
  review-thread, `behindBy`, or merge-state fields. This keeps the cost per refresh low under
  the 60 s cadence (my call). The team is small and has few open PRs per engineer (Jay). If a
  refresh ever covers dozens of PRs, the next step is to re-fetch detail only for PRs whose
  `updatedAt` changed.
- Team lookup: fetch with step 1 or cache it for the session (builder's call). Membership rarely
  changes.
- Both approval states are fetched together, and the `a` toggle filters client-side, so toggling
  is instant.

## 5. Behavior

### Approval filter

Two states, toggled with `a`: **not approved** (default) and **approved**.

- "Approved" = `reviewDecision == APPROVED`. Heard's repos need exactly one approval (Jay), so
  a PR the viewer approved leaves the default list. No separate "approved by me, needs
  another" state.
- **Differs from the base spec**, where a null `reviewDecision` counts as approved: in this view, a
  null decision (the repo doesn't require reviews) counts as approved only when at least one
  latest review is `APPROVED`. Otherwise every PR in such a repo would drop out of the default
  list.

### Grouping and order

- Grouped by **user** (default) or **repo**, toggled with `g`.
- Groups are ordered by their oldest PR's age since ready, oldest first. PRs inside a group are
  ordered oldest first. Ties go by repo, then PR number.
- Group header: `▾ name  N PRs · oldest 4d`; collapsed: `▸ name  N PRs · oldest 4d`.
- Collapsed groups are kept in a `review-folds.json` keyed by group kind and name, so they survive
  relaunch (my call; matches how the base app keeps `folds.json`).

### Waiting on author (dimmed)

If the viewer requested changes and the author hasn't pushed since, the row stays in the list,
dimmed, with a `↩` glyph in the marker column. It returns to normal once a newer commit lands
(Jay's decision on dimming; the glyph is my call, since `◌` already means draft elsewhere).

### Highlight

Code-owner PRs get a yellow `★` in the marker column. The row text stays normal and gets no
background tint (Jay picked option A). A row can show both ★ and the ↩ dim.

## 6. Layout

Mockup (wide, ~100 cols, grouped by user, details open on #1312):

```
 PR Pulse · Review │ 9 PRs · 4 people · 3 repos      group: user ▾  show: not approved │ ↻ 18s
 ★3 code owner  ↩1 waiting on author
────────────────────────────────────────────────────────────────────────────────────────────
     #      TITLE                                         REPO          USER          READY
▾ maria-chen  3 PRs · oldest 4d
  ★  #1298  billing: retry failed Stripe webhooks          heard-ts      maria-chen    4d
▶ ★  #1312  billing: proration for mid-cycle plan change   heard-ts      maria-chen    2d
     │ Adds proration when a customer moves plans mid-cycle. Uses Stripe's
     │ preview invoice to compute the credit, then applies it on the next
     │ invoice. Behind the `billing.proration` flag.
     │ Closes CORE-2210
     │ CI ✓ 9/9      Reviewers ✓ dan-o approved  ○ @heard/billing (code owner)  ○ priya-s
     #1320  billing: invoice PDF footer copy              heard-ts      maria-chen    5h

▾ dan-o  2 PRs · oldest 3d
     #87    tax: 2026 bracket table                       tax-engine    dan-o         3d
  ↩  #91    tax: quarterly estimate rounding              tax-engine    dan-o         1d   (dim)

▸ priya-s  2 PRs · oldest 2d

────────────────────────────────────────────────────────────────────────────────────────────
 j/k move  ␣ details/fold  o open  r ↻  g group  a approved⇄not  ? help
```

- **Columns:** marker (★ / ↩), PR #, title, repo, user, ready age. These are Jay's columns. Repo
  shows the name without the owner. Age uses `ago.rs` (`5h`, `4d`, `3w`).
- The column matching the current grouping (user when grouped by user, repo when grouped by repo)
  stays visible at wide widths, but it is the first one dropped as width shrinks (my call).
- **Widths:** follows the base spec's four tiers. Wide: all columns. Medium: drop the grouping
  column, and truncate the title. Narrow/Tiny: two-line rows (line 1: marker, #, title; line 2:
  the non-grouping column + age). Exact widths are the builder's call.
- **Header:** view name, PR / people / repo counts, grouping, filter, refresh age (⠹ while
  fetching). **Summary line:** ★ count and ↩ count for the PRs currently shown.
- **Details (␣ on a PR row):** inline under the row, as the base drawer is. Description first (up
  to 5 lines, wrapped to the pane, then clipped), then CI (reuse the base drawer's CI section),
  then Reviewers: everyone except the viewer, with state, and team requests marked
  `(code owner)` when `asCodeOwner`. Bots are listed separately, as in the base drawer. No Merge or
  Changed sections.
- Visual rules from base spec §12 apply. No blue or magenta, and every color is paired with a glyph.

## 7. Keys

| Key | On a group header | On a PR row |
|---|---|---|
| `j`/`k`, `↓`/`↑` | move | move |
| `␣` | collapse / expand group | open / close details |
| `h`/`l`, `←`/`→` | collapse / expand (kept for consistency with other views) | collapse / expand its group |
| `o` | — | open PR in browser (`gh pr view --web`, as today) |
| `r` | refresh | refresh |
| `g` | toggle group user ⇄ repo | same |
| `a` | toggle not approved ⇄ approved | same |
| `?` / `q` / Ctrl-C | help / quit | same |

`g` and `a` are my call; Jay's list didn't name keys for these toggles. `S` (sort), `s` (scope),
`/` (filter), and `⏎` (jump) do nothing in this view and are left out of its help and footer.

## 8. States

- **Loading, refresh, and errors:** as base spec §11 (progressive per-repo loading, last good data
  kept and dimmed on error, banner with the fix).
- **Empty, not configured:** the config path and sample snippet from §3.
- **Empty, nothing waiting:** "Nothing waiting on review in N repos." Add the hint "a show
  approved (k)" when the approved list isn't empty.
- **Team lookup fails** (for example, the `gh` token lacks `read:org`): the list still shows
  without ★, and a one-line dim notice explains why with the `gh auth refresh -s read:org` fix.

## 9. Testing

- Fixtures: a recorded review search + detail response covering several authors across two or
  three repos, a dependabot PR (filtered out), a viewer-authored PR (filtered out), a draft
  (filtered out), a PR that went draft → ready (ready age ≠ created age), a code-owner team
  request for a viewer team, a code-owner request for a non-viewer team, a "waiting on author"
  PR, one approved PR, and a repo with no required reviews.
- Unit tests: inclusion filters, ready-time choice, code-owner match, waiting-on-author rule,
  approval rule (including the null decision), group ordering, description trimming.
- insta snapshots at 120 / 80 / 44 / 34 for: grouped by user, grouped by repo, details open, a
  collapsed group, each empty state.
- An ignored `live_` test that runs the review search against the configured repos.

## 10. Calls made in this design (check these)

- Config at `~/.config/herdr-pr-pulse/config.toml` under `[review] repos`; read only at startup.
- Ready age restarts if a PR goes back to draft and then ready again.
- Null `reviewDecision` counts as approved only with an approving review (differs from base spec).
- Waiting-on-author glyph `↩` (not `◌`, which means draft elsewhere).
- Keys `g` (group) and `a` (approval filter); `h`/`l` still fold.
- Collapsed groups persist in `review-folds.json`.
- Ties sort by repo, then number.
- Separate `review-cache.json`, same 60 s shared-fetch rules, slim detail fragment.
- Add `.superpowers/` (brainstorm mockups) to `.gitignore`.
- Description: first 5 non-empty lines, HTML comments stripped, plain text.
- Viewer's own PRs are excluded.

## 11. Left to the builder

Exact column widths and truncation; query text and pagination; how team membership is cached;
how much of `App` is shared with the Mine view versus split out into a review-specific view
model; fixture recording and anonymization.
