//! Reading herdr's live state via `herdr api snapshot`, and focusing panes (spec §4).

use std::env;
use std::process::Command;

use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Working,
    Blocked,
    Idle,
    Unknown,
}

impl AgentState {
    /// `done` means the agent is ready for input, like `idle`; anything unrecognised is `Unknown`.
    pub fn parse(s: &str) -> AgentState {
        match s {
            "working" => AgentState::Working,
            "blocked" => AgentState::Blocked,
            "idle" | "done" => AgentState::Idle,
            _ => AgentState::Unknown,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub panes: Vec<Pane>,
    #[serde(default)]
    pub agents: Vec<Agent>,
    #[serde(default)]
    pub layouts: Vec<Layout>,
    #[serde(default)]
    pub focused_workspace_id: Option<String>,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Workspace {
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub active_tab_id: Option<String>,
    #[serde(default)]
    pub worktree: Option<Worktree>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Worktree {
    #[serde(default)]
    pub checkout_path: String,
    #[serde(default)]
    pub repo_root: String,
    #[serde(default)]
    pub repo_name: String,
    #[serde(default)]
    pub is_linked_worktree: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Pane {
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub foreground_cwd: String,
    #[serde(default)]
    pub agent_status: String,
    #[serde(default)]
    pub focused: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Agent {
    #[serde(default)]
    pub pane_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub agent: String,
    #[serde(default)]
    pub agent_status: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub foreground_cwd: String,
    #[serde(default)]
    pub terminal_title_stripped: Option<String>,
    #[serde(default)]
    pub agent_session: Option<AgentSession>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AgentSession {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub value: String,
}

/// One tab's pane layout; only the zoom state matters here.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Layout {
    #[serde(default)]
    pub tab_id: String,
    #[serde(default)]
    pub zoomed: bool,
    #[serde(default)]
    pub focused_pane_id: Option<String>,
}

#[derive(Deserialize)]
struct Envelope {
    result: EnvelopeResult,
}

#[derive(Deserialize)]
struct EnvelopeResult {
    snapshot: Snapshot,
}

/// Parses the JSON printed by `herdr api snapshot`.
pub fn parse_snapshot(json: &str) -> Result<Snapshot, String> {
    serde_json::from_str::<Envelope>(json)
        .map(|e| e.result.snapshot)
        .map_err(|e| format!("invalid snapshot json: {e}"))
}

fn herdr_bin() -> String {
    env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_string())
}

/// Runs `$HERDR_BIN_PATH api snapshot` (falls back to `herdr` on PATH).
pub fn fetch_snapshot() -> Result<Snapshot, String> {
    let bin = herdr_bin();
    let output = Command::new(&bin)
        .args(["api", "snapshot"])
        .output()
        .map_err(|e| format!("could not run {bin}: {e}"))?;
    if !output.status.success() {
        return Err(format!("{bin} api snapshot exited with {}", output.status));
    }
    parse_snapshot(&String::from_utf8_lossy(&output.stdout))
}

/// Focuses the pane (switching workspace and tab) with `herdr agent focus <pane_id>`.
///
/// Chosen from the docs, not yet confirmed live (the task forbade moving the user's screen):
/// `herdr --skill` says agent commands "accept either a unique live agent name or the pane ID
/// currently hosting that agent", and that "explicit focus commands mark the target seen".
/// `herdr pane focus` only moves to a neighbouring pane (`--direction` is required), so it
/// cannot cross workspaces. Remaining live check: run this on an agent pane in another
/// workspace and confirm the workspace switches. It only accepts panes hosting an agent.
pub fn focus_pane(pane_id: &str) -> Result<(), String> {
    let bin = herdr_bin();
    let output = Command::new(&bin)
        .args(["agent", "focus", pane_id])
        .output()
        .map_err(|e| format!("could not run {bin}: {e}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(format!(
        "{bin} agent focus {pane_id} failed: {}",
        stderr.trim()
    ))
}

impl Snapshot {
    pub fn workspace_of(&self, pane_id: &str) -> Option<&str> {
        self.panes
            .iter()
            .find(|p| p.pane_id == pane_id)
            .map(|p| p.workspace_id.as_str())
    }

    /// True when the pane is on screen: its workspace is focused, its tab is that workspace's
    /// active tab, and no other pane is zoomed over it. Missing fields mean not visible.
    pub fn is_visible(&self, pane_id: &str) -> bool {
        let Some(pane) = self.panes.iter().find(|p| p.pane_id == pane_id) else {
            return false;
        };
        let Some(focused) = self.focused_workspace_id.as_deref() else {
            return false;
        };
        if pane.workspace_id.is_empty() || pane.workspace_id != focused {
            return false;
        }
        let active = self
            .workspaces
            .iter()
            .find(|w| w.workspace_id == pane.workspace_id)
            .and_then(|w| w.active_tab_id.as_deref());
        if active.is_none() || active != Some(pane.tab_id.as_str()) {
            return false;
        }
        !self.layouts.iter().any(|l| {
            l.tab_id == pane.tab_id && l.zoomed && l.focused_pane_id.as_deref() != Some(pane_id)
        })
    }

    pub fn agent_state(&self, pane_id: &str) -> AgentState {
        self.agents
            .iter()
            .find(|a| a.pane_id == pane_id)
            .map(|a| AgentState::parse(&a.agent_status))
            .unwrap_or(AgentState::Unknown)
    }

    /// The directory to run git in for a workspace: its worktree's checkout path.
    pub fn checkout_key(&self, workspace_id: &str) -> Option<String> {
        self.workspaces
            .iter()
            .find(|w| w.workspace_id == workspace_id)
            .and_then(|w| w.worktree.as_ref())
            .map(|t| t.checkout_path.clone())
            .filter(|p| !p.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Snapshot {
        parse_snapshot(include_str!("../tests/fixtures/snapshot.json")).unwrap()
    }

    #[test]
    fn visible_only_on_own_active_tab() {
        let mut s = fixture();
        assert!(s.is_visible("w1:p2"));

        let mut other_tab = s.clone();
        other_tab.workspaces[0].active_tab_id = Some("w1:t2".into());
        assert!(!other_tab.is_visible("w1:p2"));

        s.layouts[0].zoomed = true;
        s.layouts[0].focused_pane_id = Some("w1:p1".into());
        assert!(!s.is_visible("w1:p2"));
        assert!(s.is_visible("w1:p1"));

        let mut w2 = fixture();
        w2.focused_workspace_id = Some("w2".into());
        assert!(!w2.is_visible("w1:p2"));
        assert!(w2.is_visible("w2:p1"));
    }

    #[test]
    fn missing_fields_are_not_visible() {
        let s = parse_snapshot(
            r#"{"result":{"snapshot":{"panes":[{"pane_id":"a:p1","workspace_id":"a","tab_id":"a:t1"}]}}}"#,
        )
        .unwrap();
        assert!(!s.is_visible("a:p1"));
        assert!(!s.is_visible("nope"));
    }

    #[test]
    fn agent_state_parsing() {
        assert_eq!(AgentState::parse("done"), AgentState::Idle);
        assert_eq!(AgentState::parse("weird"), AgentState::Unknown);
        let s = fixture();
        assert_eq!(s.agent_state("w1:p1"), AgentState::Idle);
        assert_eq!(s.agent_state("w2:p1"), AgentState::Working);
        assert_eq!(s.agent_state("w3:p1"), AgentState::Blocked);
        assert_eq!(s.agent_state("w1:p2"), AgentState::Unknown);
    }

    #[test]
    fn checkout_key_and_workspace_of() {
        let s = fixture();
        assert_eq!(
            s.checkout_key("w1").as_deref(),
            Some("/work/heard-ts-audit")
        );
        assert_eq!(s.checkout_key("w4"), None);
        assert_eq!(s.workspace_of("w3:p1"), Some("w3"));
        assert_eq!(s.workspace_of("nope"), None);
    }

    #[test]
    fn rejects_garbage_and_tolerates_missing_lists() {
        assert!(parse_snapshot("not json").is_err());
        assert!(parse_snapshot(r#"{"result":{"snapshot":{}}}"#).is_ok());
    }
}
