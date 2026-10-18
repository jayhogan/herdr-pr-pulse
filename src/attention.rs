//! Attention levels, reasons and mergeable-through counts (spec §6-§7).

pub use crate::herdr::AgentState;
use crate::model::{CiState, MergeState, Pr};
use crate::stacks::Group;

/// Ordered so that `Urgent` is the worst.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Attention {
    Waiting,
    Ready,
    Nudge,
    NeedsYou,
    Urgent,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Reason {
    CiFailing(String),
    ChangesRequested,
    Conflicts,
    Behind(u32),
    Threads(u32),
    NoReviewers,
    CiRunning,
    ReviewsPending,
    Computing,
    Draft,
}

#[derive(Default, Debug, PartialEq, Eq)]
pub struct Counts {
    pub urgent: usize,
    pub needs_you: usize,
    pub nudge: usize,
    pub ready: usize,
    pub waiting: usize,
}

fn ci_failing(pr: &Pr) -> bool {
    pr.ci.state == CiState::Failing
}

fn is_behind(pr: &Pr) -> bool {
    pr.merge == MergeState::Behind
}

fn has_no_reviewers(pr: &Pr) -> bool {
    !pr.is_draft && pr.reviewers.is_empty()
}

/// Could merge now if every layer below were merged.
pub fn ready_in_order(pr: &Pr) -> bool {
    matches!(pr.ci.state, CiState::Passing | CiState::None)
        && pr.approved != Some(false)
        && !pr.changes_requested
        && !matches!(pr.merge, MergeState::Dirty | MergeState::Behind)
        && !pr.is_draft
}

pub fn level(pr: &Pr, is_bottom_open: bool, agent: Option<AgentState>) -> Attention {
    if ci_failing(pr) && agent == Some(AgentState::Idle) {
        Attention::Urgent
    } else if ci_failing(pr) || pr.changes_requested || pr.merge == MergeState::Dirty {
        Attention::NeedsYou
    } else if is_behind(pr) || pr.unresolved_threads > 0 || has_no_reviewers(pr) {
        Attention::Nudge
    } else if is_bottom_open
        && ready_in_order(pr)
        && matches!(
            pr.merge,
            MergeState::Clean | MergeState::HasHooks | MergeState::Unstable
        )
    {
        Attention::Ready
    } else {
        Attention::Waiting
    }
}

/// Everything holding the PR back, most serious first.
pub fn reasons(pr: &Pr) -> Vec<Reason> {
    let mut out = Vec::new();
    if ci_failing(pr) {
        let name = pr.ci.failing.first().map(|c| c.name.clone());
        out.push(Reason::CiFailing(name.unwrap_or_default()));
    }
    if pr.changes_requested {
        out.push(Reason::ChangesRequested);
    }
    if pr.merge == MergeState::Dirty {
        out.push(Reason::Conflicts);
    }
    if is_behind(pr) {
        out.push(Reason::Behind(pr.behind_by.unwrap_or(0)));
    }
    if pr.unresolved_threads > 0 {
        out.push(Reason::Threads(pr.unresolved_threads));
    }
    if has_no_reviewers(pr) {
        out.push(Reason::NoReviewers);
    }
    if pr.ci.state == CiState::Running {
        out.push(Reason::CiRunning);
    }
    if pr.approved == Some(false) && !pr.changes_requested {
        out.push(Reason::ReviewsPending);
    }
    if pr.merge == MergeState::Unknown {
        out.push(Reason::Computing);
    }
    if pr.is_draft {
        out.push(Reason::Draft);
    }
    out
}

/// Consecutive open layers from the bottom that satisfy `ready_in_order`.
pub fn mergeable_through(group: &Group) -> usize {
    group
        .open_layers()
        .take_while(|p| ready_in_order(p))
        .count()
}

/// First open layer (1-based) that is not ready, with its most serious reason.
pub fn first_blocker(group: &Group) -> Option<(usize, Reason)> {
    let (i, p) = group
        .open_layers()
        .enumerate()
        .find(|(_, p)| !ready_in_order(p))?;
    reasons(p).into_iter().next().map(|r| (i + 1, r))
}

pub fn count(levels: impl IntoIterator<Item = Attention>) -> Counts {
    let mut c = Counts::default();
    for l in levels {
        match l {
            Attention::Urgent => c.urgent += 1,
            Attention::NeedsYou => c.needs_you += 1,
            Attention::Nudge => c.nudge += 1,
            Attention::Ready => c.ready += 1,
            Attention::Waiting => c.waiting += 1,
        }
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{self, Pr};
    use crate::stacks::{self, Group};

    fn fixture() -> Vec<Pr> {
        model::parse_detail(include_str!("../tests/fixtures/detail.json")).unwrap()
    }

    fn pr(n: u32) -> Pr {
        fixture().into_iter().find(|p| p.number == n).unwrap()
    }

    fn group_of(n: u32) -> Group {
        stacks::group(&fixture())
            .into_iter()
            .find(|g| g.layers.iter().any(|p| p.number == n))
            .unwrap()
    }

    fn lvl(g: &Group, n: u32, agent: Option<AgentState>) -> Attention {
        let bottom = g.open_layers().next().unwrap().number;
        level(
            g.layers.iter().find(|p| p.number == n).unwrap(),
            bottom == n,
            agent,
        )
    }

    #[test]
    fn audit_levels() {
        let g = group_of(1284);
        assert_eq!(lvl(&g, 1284, None), Attention::Ready);
        assert_eq!(lvl(&g, 1287, Some(AgentState::Idle)), Attention::Urgent);
        assert_eq!(
            lvl(&g, 1287, Some(AgentState::Working)),
            Attention::NeedsYou
        );
        assert_eq!(lvl(&g, 1287, None), Attention::NeedsYou);
        assert_eq!(lvl(&g, 1291, None), Attention::Nudge);
        assert_eq!(lvl(&g, 1295, None), Attention::Waiting);
    }

    #[test]
    fn docket_levels() {
        let g = group_of(412);
        assert_eq!(lvl(&g, 412, None), Attention::NeedsYou);
        assert_eq!(lvl(&g, 415, None), Attention::Waiting);
    }

    #[test]
    fn standalone_levels() {
        let g = group_of(1279);
        assert_eq!(lvl(&g, 1279, None), Attention::Nudge);
        assert_eq!(reasons(&pr(1279))[0], Reason::Behind(6));
        let g = group_of(1302);
        assert_eq!(lvl(&g, 1302, None), Attention::Nudge);
        assert_eq!(reasons(&pr(1302))[0], Reason::NoReviewers);
    }

    #[test]
    fn mergeable_through_counts_in_order() {
        assert_eq!(mergeable_through(&group_of(1284)), 1);
        assert_eq!(mergeable_through(&group_of(412)), 0);
        assert_eq!(mergeable_through(&group_of(1279)), 0);
    }

    #[test]
    fn first_blocker_reports_layer() {
        assert_eq!(
            first_blocker(&group_of(1284)),
            Some((2, Reason::CiFailing("test-integration".into())))
        );
        assert_eq!(first_blocker(&group_of(412)), Some((1, Reason::Conflicts)));
    }

    #[test]
    fn plain_draft_is_the_blocker() {
        let audit = group_of(1284);
        let mut draft = pr(1284);
        draft.number = 9001;
        draft.is_draft = true;
        draft.ci.state = CiState::Passing;
        draft.merge = MergeState::Draft;
        draft.approved = None;
        draft.changes_requested = false;
        draft.unresolved_threads = 0;
        assert_eq!(reasons(&draft), [Reason::Draft]);
        let failing = audit
            .layers
            .iter()
            .find(|p| p.number == 1287)
            .unwrap()
            .clone();
        let mut g = audit.clone();
        g.layers = vec![audit.layers[0].clone(), draft, failing];
        assert_eq!(mergeable_through(&g), 1);
        assert_eq!(first_blocker(&g), Some((2, Reason::Draft)));
    }

    #[test]
    fn stack_rank_is_worst() {
        let worst = [Attention::Ready, Attention::Urgent, Attention::Waiting]
            .into_iter()
            .max();
        assert_eq!(worst, Some(Attention::Urgent));
    }

    #[test]
    fn reviews_not_required_counts_as_approved() {
        let mut p = pr(1284);
        p.approved = None;
        p.ci.state = CiState::Passing;
        p.merge = MergeState::Clean;
        p.changes_requested = false;
        p.unresolved_threads = 0;
        assert_eq!(level(&p, true, None), Attention::Ready);
    }

    #[test]
    fn counts_levels() {
        let c = count([Attention::Ready, Attention::Ready, Attention::Urgent]);
        assert_eq!((c.ready, c.urgent, c.waiting), (2, 1, 0));
    }
}
