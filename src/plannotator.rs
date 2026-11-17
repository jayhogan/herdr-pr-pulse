//! `p`: review a PR in Plannotator, and hand the feedback to the stack's agent.

use std::fs;
use std::path::Path;

use chrono::{DateTime, Local};
use serde::Deserialize;

/// How the review ended, from `plannotator review --json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Approved,
    Annotated,
    Dismissed,
}

/// The JSON record `plannotator review --json` prints when the review closes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Outcome {
    pub decision: Decision,
    #[serde(default)]
    pub message: String,
    /// True when there is nothing for an agent to act on.
    #[serde(default)]
    pub noop: bool,
}

/// The PR under review, and the agent pane its feedback goes to (`None`: don't forward).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub url: String,
    pub repo: String,
    pub number: u32,
    pub pane: Option<String>,
}

/// The last line of `stdout` that starts with `{`, parsed as an `Outcome`.
pub fn parse_result(stdout: &str) -> Result<Outcome, String> {
    let line = stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with('{'))
        .ok_or("no JSON result from plannotator")?;
    serde_json::from_str(line).map_err(|e| format!("invalid plannotator JSON: {e}"))
}

/// What to do with a finished review; returns the status line (`None`: say nothing). Feedback
/// that `deliver` can't hand to the agent is saved under `feedback_dir`.
pub fn settle(
    target: &Target,
    result: Result<Outcome, String>,
    deliver: &dyn Fn(&str, &str) -> Result<(), String>,
    feedback_dir: &Path,
    now: DateTime<Local>,
) -> Option<String> {
    let n = target.number;
    let outcome = match result {
        Ok(o) => o,
        Err(e) => return Some(format!("Plannotator failed for #{n}: {e}")),
    };
    let pane = target.pane.as_deref()?;
    if outcome.noop {
        return Some(match outcome.decision {
            Decision::Approved => format!("#{n} approved"),
            Decision::Dismissed => format!("#{n} review dismissed"),
            Decision::Annotated => format!("#{n} review closed, nothing to send"),
        });
    }
    let Err(reason) = deliver(pane, &outcome.message) else {
        return Some(format!("feedback on #{n} sent to agent"));
    };
    let name = format!(
        "{}-{n}-{}.md",
        target.repo.replace('/', "-"),
        now.format("%Y%m%dT%H%M%S")
    );
    let path = feedback_dir.join(name);
    let saved = fs::create_dir_all(feedback_dir).and_then(|()| fs::write(&path, &outcome.message));
    Some(match saved {
        Ok(()) => format!("feedback on #{n} saved to {}: {reason}", path.display()),
        Err(e) => format!("feedback on #{n} not delivered or saved: {reason}; {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    use chrono::TimeZone;

    fn target(pane: Option<&str>) -> Target {
        Target {
            url: "https://github.com/heard/heard-ts/pull/1287".into(),
            repo: "heard/heard-ts".into(),
            number: 1287,
            pane: pane.map(String::from),
        }
    }

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 17, 14, 30, 0).unwrap()
    }

    fn outcome(decision: Decision, noop: bool) -> Outcome {
        Outcome {
            decision,
            message: "fix x".into(),
            noop,
        }
    }

    /// Settles `result`, recording each delivery; `fail` makes delivery fail with it.
    fn run(
        t: &Target,
        result: Result<Outcome, String>,
        fail: Option<&str>,
        dir: &Path,
    ) -> (Option<String>, Vec<(String, String)>) {
        let sent = RefCell::new(Vec::new());
        let deliver = |pane: &str, msg: &str| {
            sent.borrow_mut().push((pane.to_string(), msg.to_string()));
            fail.map_or(Ok(()), |e| Err(e.to_string()))
        };
        let status = settle(t, result, &deliver, dir, now());
        (status, sent.into_inner())
    }

    #[test]
    fn parse_takes_last_json_line() {
        let out = "Opening http://localhost:1234\n{\"v\":1,\"surface\":\"review\",\"decision\":\"annotated\",\"message\":\"fix x\\nand y\",\"noop\":false,\"annotationCount\":2}\n";
        let o = parse_result(out).unwrap();
        assert_eq!(
            o,
            Outcome {
                decision: Decision::Annotated,
                message: "fix x\nand y".into(),
                noop: false
            }
        );
    }

    #[test]
    fn parse_rejects_output_without_json() {
        assert!(parse_result("boom\n").is_err());
    }

    #[test]
    fn settle_forwards_when_not_noop() {
        let tmp = tempfile::tempdir().unwrap();
        let t = target(Some("w1:p1"));
        let (status, sent) = run(
            &t,
            Ok(outcome(Decision::Annotated, false)),
            None,
            tmp.path(),
        );
        assert_eq!(status.as_deref(), Some("feedback on #1287 sent to agent"));
        assert_eq!(sent, vec![("w1:p1".to_string(), "fix x".to_string())]);
    }

    #[test]
    fn settle_approved_with_notes_forwards() {
        let tmp = tempfile::tempdir().unwrap();
        let t = target(Some("w1:p1"));
        let (status, sent) = run(&t, Ok(outcome(Decision::Approved, false)), None, tmp.path());
        assert_eq!(status.as_deref(), Some("feedback on #1287 sent to agent"));
        assert_eq!(sent.len(), 1);
    }

    #[test]
    fn settle_noop_sends_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let t = target(Some("w1:p1"));
        for (decision, want) in [
            (Decision::Approved, "#1287 approved"),
            (Decision::Dismissed, "#1287 review dismissed"),
            (Decision::Annotated, "#1287 review closed, nothing to send"),
        ] {
            let (status, sent) = run(&t, Ok(outcome(decision, true)), None, tmp.path());
            assert_eq!(status.as_deref(), Some(want));
            assert!(sent.is_empty());
        }
    }

    #[test]
    fn settle_saves_when_delivery_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let t = target(Some("w1:p1"));
        let (status, _) = run(
            &t,
            Ok(outcome(Decision::Annotated, false)),
            Some("agent_blocked"),
            tmp.path(),
        );
        let path = tmp.path().join("heard-heard-ts-1287-20261117T143000.md");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fix x");
        assert_eq!(
            status,
            Some(format!(
                "feedback on #1287 saved to {}: agent_blocked",
                path.display()
            ))
        );
    }

    #[test]
    fn settle_run_error_reports() {
        let tmp = tempfile::tempdir().unwrap();
        let t = target(Some("w1:p1"));
        let (status, sent) = run(&t, Err("not found".into()), None, tmp.path());
        assert_eq!(
            status.as_deref(),
            Some("Plannotator failed for #1287: not found")
        );
        assert!(sent.is_empty());
        assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
    }

    #[test]
    fn settle_without_pane_is_silent() {
        let tmp = tempfile::tempdir().unwrap();
        let t = target(None);
        let (status, sent) = run(
            &t,
            Ok(outcome(Decision::Annotated, false)),
            None,
            tmp.path(),
        );
        assert_eq!(status, None);
        assert!(sent.is_empty());
    }
}
