//! Workspace memory and which agent pane belongs to which stack (spec §8).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::app::AgentDot;
use crate::git::{Branches, Checkout};
use crate::herdr::{AgentState, Pane, Snapshot};
use crate::stacks::Group;

/// The state file holding [`Memory`].
pub const FILE: &str = "workspaces.json";
const VERSION: u32 = 1;

/// `workspaces.json`: every PR branch each checkout has had, keyed by checkout path.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    pub version: u32,
    pub by_checkout: BTreeMap<String, Vec<Remembered>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Remembered {
    pub repo: String,
    pub number: u32,
    pub pr_id: String,
    pub head_ref: String,
}

impl Memory {
    /// Adds `r` under `checkout`; true when that changed anything. A PR already there is
    /// updated in place (its head ref can change).
    pub fn remember(&mut self, checkout: &str, r: Remembered) -> bool {
        self.version = VERSION;
        let list = self.by_checkout.entry(checkout.to_string()).or_default();
        match list.iter_mut().find(|x| x.pr_id == r.pr_id) {
            Some(x) if *x == r => false,
            Some(x) => {
                *x = r;
                true
            }
            None => {
                list.push(r);
                true
            }
        }
    }

    /// Drops checkouts whose path is gone, and (when herdr answered, `Some`) those no
    /// workspace uses. True when anything was dropped.
    pub fn prune(
        &mut self,
        live_checkouts: Option<&BTreeSet<String>>,
        exists: impl Fn(&str) -> bool,
    ) -> bool {
        let before = self.by_checkout.len();
        self.by_checkout
            .retain(|path, _| exists(path) && live_checkouts.is_none_or(|l| l.contains(path)));
        self.by_checkout.len() != before
    }

    /// Every remembered PR once, in checkout order.
    pub fn all(&self) -> Vec<&Remembered> {
        let mut seen = BTreeSet::new();
        self.by_checkout
            .values()
            .flatten()
            .filter(|r| seen.insert(r.pr_id.as_str()))
            .collect()
    }

    /// `(repo, pr id)` of every remembered PR, once each.
    pub fn all_ids(&self) -> Vec<(String, String)> {
        self.all()
            .into_iter()
            .map(|r| (r.repo.clone(), r.pr_id.clone()))
            .collect()
    }

    pub fn ids_for(&self, checkout: &str) -> BTreeSet<String> {
        self.by_checkout
            .get(checkout)
            .into_iter()
            .flatten()
            .map(|r| r.pr_id.clone())
            .collect()
    }
}

/// A pane PR Pulse cares about: one running an agent, or PR Pulse's own pane.
#[derive(Debug, Clone, PartialEq)]
pub struct PaneCheckout {
    pub pane_id: String,
    pub workspace_id: String,
    pub checkout_key: String,
    pub repo: String,
    pub branch: String,
    /// `None` when the pane runs no agent.
    pub agent: Option<AgentState>,
    pub focused: bool,
}

/// Agent panes plus `own` (PR Pulse's pane), with their repo and branch. Panes outside a
/// git checkout (or on a detached HEAD) are left out. git runs once per distinct cwd.
pub fn pane_checkouts(
    snap: &Snapshot,
    branches: &mut dyn Branches,
    own: Option<&str>,
) -> Vec<PaneCheckout> {
    let mut by_cwd: HashMap<&str, Option<Checkout>> = HashMap::new();
    let mut out = Vec::new();
    for pane in &snap.panes {
        let has_agent = snap.agents.iter().any(|a| a.pane_id == pane.pane_id);
        if !has_agent && own != Some(pane.pane_id.as_str()) {
            continue;
        }
        let cwd = pane_cwd(pane);
        if cwd.is_empty() {
            continue;
        }
        let checkout = by_cwd.entry(cwd).or_insert_with(|| branches.checkout(cwd));
        let Some(c) = checkout.clone() else {
            continue;
        };
        out.push(PaneCheckout {
            pane_id: pane.pane_id.clone(),
            workspace_id: pane.workspace_id.clone(),
            checkout_key: snap.checkout_key(&pane.workspace_id).unwrap_or(c.toplevel),
            repo: c.repo,
            branch: c.branch,
            agent: has_agent.then(|| snap.agent_state(&pane.pane_id)),
            focused: pane.focused,
        });
    }
    out
}

/// Where a pane's foreground process runs, else its shell's cwd.
fn pane_cwd(pane: &Pane) -> &str {
    if pane.foreground_cwd.is_empty() {
        &pane.cwd
    } else {
        &pane.foreground_cwd
    }
}

/// Checkout keys some workspace still uses: each worktree's checkout path, and for a
/// workspace without a worktree the repo root of every one of its panes (any pane, any
/// branch, so an agent restarting or a detached HEAD does not drop its memory).
pub fn live_checkouts(snap: &Snapshot, branches: &mut dyn Branches) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for w in &snap.workspaces {
        match snap.checkout_key(&w.workspace_id) {
            Some(path) => {
                out.insert(path);
            }
            None => {
                let cwds: BTreeSet<&str> = snap
                    .panes
                    .iter()
                    .filter(|p| p.workspace_id == w.workspace_id)
                    .map(pane_cwd)
                    .filter(|c| !c.is_empty())
                    .collect();
                out.extend(cwds.into_iter().filter_map(|c| branches.toplevel(c)));
            }
        }
    }
    out
}

/// The agent for each group (by key) and the layer its pane has checked out, if any.
///
/// A pane belongs to a group when its branch is one of the group's head refs, or when its
/// checkout remembers one of the group's PRs (it has moved to a side branch). Only panes
/// running an agent count (never PR Pulse's own pane). Among several: one on a layer's
/// branch first, then the focused one, then the first.
pub fn stack_agents(
    groups: &[Group],
    panes: &[PaneCheckout],
    memory: &Memory,
) -> BTreeMap<String, (AgentDot, Option<u32>)> {
    let mut out = BTreeMap::new();
    for g in groups {
        let best = panes
            .iter()
            .filter(|p| p.agent.is_some())
            .filter_map(|p| {
                let on_layer = g
                    .layers
                    .iter()
                    .find(|l| l.repo == p.repo && l.head_ref == p.branch)
                    .map(|l| l.number);
                let remembered = || {
                    let ids = memory.ids_for(&p.checkout_key);
                    g.layers.iter().any(|l| ids.contains(&l.id))
                };
                (on_layer.is_some() || remembered()).then_some((p, on_layer))
            })
            // max_by_key keeps the last of equals; reverse so the first pane wins ties.
            .rev()
            .max_by_key(|(p, on_layer)| (on_layer.is_some(), p.focused));
        if let Some((p, on_layer)) = best {
            let dot = AgentDot {
                state: p.agent.unwrap_or(AgentState::Unknown),
                pane_id: p.pane_id.clone(),
            };
            out.insert(g.key.clone(), (dot, on_layer));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::FixtureBranches;
    use crate::herdr::parse_snapshot;
    use crate::model;
    use crate::stacks;

    const BRANCHES: &str = include_str!("../tests/fixtures/branches.json");

    fn snapshot() -> Snapshot {
        parse_snapshot(include_str!("../tests/fixtures/snapshot.json")).unwrap()
    }

    fn memory() -> Memory {
        serde_json::from_str(include_str!("../tests/fixtures/workspaces.json")).unwrap()
    }

    fn groups() -> Vec<Group> {
        stacks::group(&model::parse_detail(include_str!("../tests/fixtures/detail.json")).unwrap())
    }

    fn rem(n: u32, head: &str) -> Remembered {
        Remembered {
            repo: "heard/heard-ts".into(),
            number: n,
            pr_id: format!("PR_{n}"),
            head_ref: head.into(),
        }
    }

    fn audit_key(groups: &[Group]) -> String {
        groups
            .iter()
            .find(|g| g.name == "audit")
            .unwrap()
            .key
            .clone()
    }

    #[test]
    fn remember_dedups() {
        let mut m = Memory::default();
        assert!(m.remember("/a", rem(1, "x")));
        assert!(!m.remember("/a", rem(1, "x")));
        assert!(m.remember("/a", rem(2, "y")));
        assert!(m.remember("/b", rem(1, "x")));
        assert_eq!(m.by_checkout["/a"].len(), 2);
        assert_eq!(
            m.ids_for("/a"),
            BTreeSet::from(["PR_1".to_string(), "PR_2".to_string()])
        );
        assert_eq!(
            m.all_ids(),
            [
                ("heard/heard-ts".to_string(), "PR_1".to_string()),
                ("heard/heard-ts".to_string(), "PR_2".to_string())
            ]
        );
        assert!(m.ids_for("/nope").is_empty());
    }

    #[test]
    fn prune_missing_path() {
        let mut m = memory();
        let live: BTreeSet<String> = m.by_checkout.keys().cloned().collect();
        assert!(!m.prune(Some(&live), |_| true));
        assert!(m.prune(Some(&live), |p| p != "/work/docket-thread"));
        assert!(!m.by_checkout.contains_key("/work/docket-thread"));
        assert_eq!(m.by_checkout.len(), 2);
        // herdr answered and no workspace uses the path any more.
        let live = BTreeSet::from(["/work/heard-ts-audit".to_string()]);
        assert!(m.prune(Some(&live), |_| true));
        assert_eq!(
            m.by_checkout.keys().collect::<Vec<_>>(),
            ["/work/heard-ts-audit"]
        );
    }

    #[test]
    fn prune_skipped_when_herdr_unreachable() {
        let mut m = memory();
        assert!(!m.prune(None, |_| true));
        assert_eq!(m, memory());
        // A path gone from disk is still dropped.
        assert!(m.prune(None, |p| p != "/work/heard-ts-notes"));
        assert_eq!(m.by_checkout.len(), 2);
    }

    #[test]
    fn pane_checkouts_agents_and_own_pane() {
        let panes = pane_checkouts(&snapshot(), &mut FixtureBranches::embedded(), Some("w1:p2"));
        let ids: Vec<&str> = panes.iter().map(|p| p.pane_id.as_str()).collect();
        assert_eq!(ids, ["w1:p1", "w1:p2", "w2:p1", "w3:p1"]);
        let own = &panes[1];
        assert_eq!(own.agent, None);
        assert!(own.focused);
        assert_eq!(own.checkout_key, "/work/heard-ts-audit");
        assert_eq!(own.branch, "audit/kafka-producer");
        assert_eq!(panes[2].agent, Some(AgentState::Working));
        assert_eq!(panes[2].repo, "heard/docket");
    }

    #[test]
    fn checkout_key_falls_back_to_toplevel() {
        let mut snap = snapshot();
        snap.workspaces[1].worktree = None;
        let mut b = FixtureBranches::from_json(&BRANCHES.replace(
            r#""toplevel": "/work/docket-thread""#,
            r#""toplevel": "/work/docket""#,
        ))
        .unwrap();
        let panes = pane_checkouts(&snap, &mut b, None);
        let p = panes.iter().find(|p| p.pane_id == "w2:p1").unwrap();
        assert_eq!(p.checkout_key, "/work/docket");
    }

    #[test]
    fn stack_agent_from_current_branch() {
        let gs = groups();
        let panes = pane_checkouts(&snapshot(), &mut FixtureBranches::embedded(), Some("w1:p2"));
        let agents = stack_agents(&gs, &panes, &Memory::default());
        let (dot, checked_out) = &agents[&audit_key(&gs)];
        assert_eq!(
            (dot.state, dot.pane_id.as_str(), *checked_out),
            (AgentState::Idle, "w1:p1", Some(1287))
        );
        let docket = gs.iter().find(|g| g.repo == "heard/docket").unwrap();
        assert_eq!(agents[&docket.key].0.state, AgentState::Working);
        // #1279 has no pane.
        let pii = gs
            .iter()
            .find(|g| g.layers.iter().any(|p| p.number == 1279))
            .unwrap();
        assert!(!agents.contains_key(&pii.key));
    }

    #[test]
    fn stack_agent_via_memory_when_on_side_branch() {
        let gs = groups();
        let mut b =
            FixtureBranches::from_json(&BRANCHES.replace("audit/kafka-producer", "fix/skill-bug"))
                .unwrap();
        let panes = pane_checkouts(&snapshot(), &mut b, Some("w1:p2"));
        assert!(!stack_agents(&gs, &panes, &Memory::default()).contains_key(&audit_key(&gs)));
        let agents = stack_agents(&gs, &panes, &memory());
        let (dot, checked_out) = &agents[&audit_key(&gs)];
        assert_eq!(
            (dot.state, dot.pane_id.as_str(), *checked_out),
            (AgentState::Idle, "w1:p1", None)
        );
    }

    /// A Workspace-view app on the fixture PRs, seeing every path as present.
    fn ws_app() -> crate::app::App {
        use crate::app::{App, ViewKind};
        let prs = model::parse_detail(include_str!("../tests/fixtures/detail.json")).unwrap();
        let mut a = App::new(
            ViewKind::Workspace,
            chrono::FixedOffset::east_opt(0).unwrap(),
        );
        a.set_own_pane("w1:p2".into());
        a.set_path_exists(|_| true);
        a.set_prs(
            prs,
            &[],
            &[],
            "jayhogan",
            "2026-10-17T14:32:08Z".parse().unwrap(),
        );
        a
    }

    fn state_with(m: Memory) -> (tempfile::TempDir, crate::store::StateDir) {
        let tmp = tempfile::tempdir().unwrap();
        let state = crate::store::StateDir::new(tmp.path().to_path_buf());
        state
            .update(FILE, |x: &mut Memory| {
                *x = m;
                true
            })
            .unwrap();
        (tmp, state)
    }

    #[test]
    fn own_pane_is_never_a_stack_agent() {
        // w1's agent is gone; only PR Pulse's own pane is on an audit branch.
        let mut snap = snapshot();
        snap.agents.retain(|a| a.pane_id != "w1:p1");
        let gs = groups();
        let panes = pane_checkouts(&snap, &mut FixtureBranches::embedded(), Some("w1:p2"));
        assert!(panes.iter().any(|p| p.pane_id == "w1:p2"), "kept for ⎇");
        assert!(!stack_agents(&gs, &panes, &memory()).contains_key(&audit_key(&gs)));

        let (_tmp, state) = state_with(Memory::default());
        let mut a = ws_app();
        a.apply_snapshot(
            Ok(snap),
            &mut FixtureBranches::embedded(),
            &state,
            chrono::Utc::now(),
        );
        let m: Memory = state.load(FILE).unwrap();
        assert!(m.ids_for("/work/heard-ts-audit").is_empty(), "{m:?}");
        assert_eq!(
            m.ids_for("/work/docket-thread"),
            BTreeSet::from(["PR_412".into()])
        );
    }

    /// Answers like the fixture, except that every checkout is on a detached HEAD.
    struct Detached(FixtureBranches);
    impl Branches for Detached {
        fn checkout(&mut self, _cwd: &str) -> Option<Checkout> {
            None
        }
        fn toplevel(&mut self, cwd: &str) -> Option<String> {
            self.0.toplevel(cwd)
        }
    }

    #[test]
    fn worktreeless_memory_survives_detached_or_absent_agent() {
        let mut snap = snapshot();
        snap.workspaces[1].worktree = None; // docket: keyed by the pane's toplevel
        let start = memory();
        let (_tmp, state) = state_with(start.clone());
        let mut a = ws_app();
        // Mid-rebase: the agent pane is on a detached HEAD.
        let mut detached = Detached(FixtureBranches::embedded());
        a.apply_snapshot(Ok(snap.clone()), &mut detached, &state, chrono::Utc::now());
        assert_eq!(state.load::<Memory>(FILE).unwrap(), start);
        // The agent exited; a plain shell pane is still there.
        snap.agents.retain(|x| x.pane_id != "w2:p1");
        a.apply_snapshot(
            Ok(snap.clone()),
            &mut FixtureBranches::embedded(),
            &state,
            chrono::Utc::now(),
        );
        assert_eq!(state.load::<Memory>(FILE).unwrap(), start);
        // The pane is closed too: nothing uses the path any more.
        snap.panes.retain(|p| p.pane_id != "w2:p1");
        a.apply_snapshot(
            Ok(snap),
            &mut FixtureBranches::embedded(),
            &state,
            chrono::Utc::now(),
        );
        let m: Memory = state.load(FILE).unwrap();
        assert!(!m.by_checkout.contains_key("/work/docket-thread"));
    }

    #[test]
    fn snapshot_without_workspaces_prunes_nothing() {
        let start = memory();
        let (_tmp, state) = state_with(start.clone());
        let mut a = ws_app();
        let empty = parse_snapshot(r#"{"result":{"snapshot":{}}}"#).unwrap();
        a.apply_snapshot(
            Ok(empty),
            &mut FixtureBranches::embedded(),
            &state,
            chrono::Utc::now(),
        );
        assert_eq!(state.load::<Memory>(FILE).unwrap(), start);
    }

    #[test]
    fn layer_branch_beats_focused_memory_match() {
        let gs = groups();
        let pane = |id: &str, branch: &str, focused: bool| PaneCheckout {
            pane_id: id.into(),
            workspace_id: id[..2].into(),
            checkout_key: format!("/work/{id}"),
            repo: "heard/heard-ts".into(),
            branch: branch.into(),
            agent: Some(AgentState::Working),
            focused,
        };
        let panes = [
            pane("w5:p1", "fix/skill-bug", true),
            pane("w6:p1", "audit/consumer", false),
        ];
        let mut m = Memory::default();
        m.remember("/work/w5:p1", rem(1287, "audit/kafka-producer"));
        let agents = stack_agents(&gs, &panes, &m);
        let (dot, checked_out) = &agents[&audit_key(&gs)];
        assert_eq!((dot.pane_id.as_str(), *checked_out), ("w6:p1", Some(1291)));
    }

    #[test]
    fn merged_pr_stays_remembered() {
        use crate::app::{App, ViewKind};
        use crate::model::PrState;
        use chrono::{FixedOffset, Utc};

        let tmp = tempfile::tempdir().unwrap();
        let state = crate::store::StateDir::new(tmp.path().to_path_buf());
        let mut prs = model::parse_detail(include_str!("../tests/fixtures/detail.json")).unwrap();
        let flags = prs.iter_mut().find(|p| p.number == 1270).unwrap();
        flags.state = PrState::Open;
        let at = "2026-10-17T14:32:08Z".parse().unwrap();
        let mut a = App::new(ViewKind::Workspace, FixedOffset::east_opt(0).unwrap());
        a.set_own_pane("w1:p2".into());
        a.set_path_exists(|_| true);
        a.set_prs(prs.clone(), &[], &[], "jayhogan", at);

        // The audit pane is on #1270's branch while it is open.
        let mut on_flags =
            FixtureBranches::from_json(&BRANCHES.replace("audit/kafka-producer", "audit/flags"))
                .unwrap();
        a.apply_snapshot(Ok(snapshot()), &mut on_flags, &state, Utc::now());
        let m: Memory = state.load(FILE).unwrap();
        assert_eq!(
            m.ids_for("/work/heard-ts-audit"),
            BTreeSet::from(["PR_1270".into()])
        );

        // #1270 merges and the pane moves on.
        prs.iter_mut().find(|p| p.number == 1270).unwrap().state = PrState::Merged;
        a.set_prs(prs, &[], &[], "jayhogan", at);
        a.apply_snapshot(
            Ok(snapshot()),
            &mut FixtureBranches::embedded(),
            &state,
            Utc::now(),
        );
        let m: Memory = state.load(FILE).unwrap();
        assert_eq!(
            m.ids_for("/work/heard-ts-audit"),
            BTreeSet::from(["PR_1270".into(), "PR_1287".into()])
        );
        assert_eq!(
            m.ids_for("/work/docket-thread"),
            BTreeSet::from(["PR_412".into()])
        );
    }

    #[test]
    fn unreachable_herdr_keeps_memory() {
        use crate::app::{App, ViewKind};
        use chrono::{FixedOffset, Utc};

        let tmp = tempfile::tempdir().unwrap();
        let state = crate::store::StateDir::new(tmp.path().to_path_buf());
        state
            .update(FILE, |m: &mut Memory| {
                *m = memory();
                true
            })
            .unwrap();
        let mut a = App::new(ViewKind::Workspace, FixedOffset::east_opt(0).unwrap());
        // Every path is "gone", but herdr is unreachable: nothing is pruned.
        a.set_path_exists(|_| false);
        a.apply_snapshot(
            Err("herdr not running".into()),
            &mut FixtureBranches::embedded(),
            &state,
            Utc::now(),
        );
        let m: Memory = state.load(FILE).unwrap();
        assert_eq!(m, memory());
    }
}
