//! `p`: review a PR in Plannotator, and hand the feedback to the stack's agent.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};

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

/// The JSON record `plannotator review --json` prints to stdout when the review closes. It
/// has only these two fields (no `noop`, which only Plannotator's host-result file carries).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Outcome {
    pub decision: Decision,
    #[serde(default)]
    pub message: String,
}

/// Plannotator's default message for an approval without notes (its
/// `DEFAULT_REVIEW_APPROVED_PROMPT`). An approval with notes has a different message.
pub const BARE_APPROVAL: &str =
    "# Code Review\nCode review completed \u{2014} no changes requested.";

impl Outcome {
    /// True when the message has something for an agent to act on.
    fn has_feedback(&self) -> bool {
        match self.decision {
            Decision::Dismissed => false,
            Decision::Approved => self.message.trim() != BARE_APPROVAL,
            Decision::Annotated => !self.message.trim().is_empty(),
        }
    }
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

/// What to do with a finished review, given Plannotator's stdout; returns the status line
/// (`None`: say nothing). Feedback that `deliver` can't hand to the agent is saved under
/// `feedback_dir`, and so is output that can't be parsed, since it may hold feedback.
pub fn settle(
    target: &Target,
    result: Result<String, String>,
    deliver: &dyn Fn(&str, &str) -> Result<(), String>,
    feedback_dir: &Path,
    now: DateTime<Local>,
) -> Option<String> {
    let n = target.number;
    let failed = |e: String| Some(format!("Plannotator failed for #{n}: {e}"));
    let stdout = match result {
        Ok(out) => out,
        Err(e) => return failed(e),
    };
    let outcome = parse_result(&stdout);
    let Some(pane) = target.pane.as_deref() else {
        return outcome.err().and_then(failed);
    };
    let outcome = match outcome {
        Ok(o) => o,
        Err(reason) => return Some(save(target, &stdout, &reason, feedback_dir, now)),
    };
    if !outcome.has_feedback() {
        return Some(match outcome.decision {
            Decision::Approved => format!("#{n} approved"),
            Decision::Dismissed => format!("#{n} review dismissed"),
            Decision::Annotated => format!("#{n} review closed, nothing to send"),
        });
    }
    match deliver(pane, &outcome.message) {
        Ok(()) => Some(format!("feedback on #{n} sent to agent")),
        Err(reason) => Some(save(target, &outcome.message, &reason, feedback_dir, now)),
    }
}

/// Writes `text` to `<feedback_dir>/<owner>-<repo>-<N>-<time>.md`; returns the status line.
fn save(
    target: &Target,
    text: &str,
    reason: &str,
    feedback_dir: &Path,
    now: DateTime<Local>,
) -> String {
    let n = target.number;
    let name = format!(
        "{}-{n}-{}.md",
        target.repo.replace('/', "-"),
        now.format("%Y%m%dT%H%M%S")
    );
    let path = feedback_dir.join(name);
    match fs::create_dir_all(feedback_dir).and_then(|()| fs::write(&path, text)) {
        Ok(()) => format!("feedback on #{n} saved to {}: {reason}", path.display()),
        Err(e) => format!("feedback on #{n} not delivered or saved: {reason}; {e}"),
    }
}

/// Runs one review of the PR at this URL to completion and returns its stdout.
pub type Runner = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

/// Hands a message to the agent in a pane.
type Deliver = Box<dyn Fn(&str, &str) -> Result<(), String>>;

/// `<program> review --json <url>`. A spawn failure or a non-zero exit is an error, with the
/// last line Plannotator wrote to stderr (where it explains the failure).
fn command_runner(program: &str) -> Runner {
    let program = program.to_string();
    Arc::new(move |url: &str| {
        let output = Command::new(&program)
            .args(["review", "--json", url])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("could not run {program}: {e}"))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(match stderr.lines().rev().find(|l| !l.trim().is_empty()) {
                Some(last) => format!("{}: {}", output.status, last.trim()),
                None => output.status.to_string(),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    })
}

/// Reviews running in the background, at most one per PR. Each runs on its own thread;
/// `poll` settles the finished ones on the caller's (UI) thread.
pub struct Reviews {
    runner: Runner,
    deliver: Deliver,
    feedback_dir: PathBuf,
    running: HashSet<String>,
    done_tx: Sender<(Target, Result<String, String>)>,
    done_rx: Receiver<(Target, Result<String, String>)>,
}

impl Reviews {
    /// Real Plannotator; feedback goes to agents with `herdr agent prompt`.
    pub fn new(feedback_dir: PathBuf) -> Reviews {
        Reviews::with(
            command_runner("plannotator"),
            Box::new(crate::herdr::prompt_agent),
            feedback_dir,
        )
    }

    pub fn with(runner: Runner, deliver: Deliver, feedback_dir: PathBuf) -> Reviews {
        let (done_tx, done_rx) = mpsc::channel();
        Reviews {
            runner,
            deliver,
            feedback_dir,
            running: HashSet::new(),
            done_tx,
            done_rx,
        }
    }

    /// Starts a review unless this PR's is still open; returns the status line.
    pub fn start(&mut self, target: Target) -> String {
        let n = target.number;
        if !self.running.insert(target.url.clone()) {
            return format!("#{n} already open in Plannotator");
        }
        let (runner, tx) = (self.runner.clone(), self.done_tx.clone());
        std::thread::spawn(move || {
            let result = runner(&target.url);
            let _ = tx.send((target, result));
        });
        format!("reviewing #{n} in Plannotator…")
    }

    /// Settles every review that has finished; returns their status lines.
    pub fn poll(&mut self) -> Vec<String> {
        let mut statuses = Vec::new();
        while let Ok((target, result)) = self.done_rx.try_recv() {
            self.running.remove(&target.url);
            let status = settle(
                &target,
                result,
                &*self.deliver,
                &self.feedback_dir,
                Local::now(),
            );
            statuses.extend(status);
        }
        statuses
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, mpsc};

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

    /// What `plannotator review --json` prints: only `decision` and `message`.
    fn json(decision: &str, message: &str) -> Result<String, String> {
        Ok(serde_json::json!({ "decision": decision, "message": message }).to_string() + "\n")
    }

    /// Settles `result`, recording each delivery; `fail` makes delivery fail with it.
    fn run(
        t: &Target,
        result: Result<String, String>,
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

    fn saved_path(dir: &Path) -> PathBuf {
        dir.join("heard-heard-ts-1287-20261117T143000.md")
    }

    #[test]
    fn parse_takes_last_json_line() {
        let out = "Opening http://localhost:1234\n{\"decision\":\"annotated\",\"message\":\"fix x\\nand y\"}\n";
        let o = parse_result(out).unwrap();
        assert_eq!(
            o,
            Outcome {
                decision: Decision::Annotated,
                message: "fix x\nand y".into(),
            }
        );
    }

    #[test]
    fn parse_rejects_output_without_json() {
        assert!(parse_result("boom\n").is_err());
    }

    #[test]
    fn settle_forwards_annotations() {
        let tmp = tempfile::tempdir().unwrap();
        let t = target(Some("w1:p1"));
        let (status, sent) = run(&t, json("annotated", "fix x"), None, tmp.path());
        assert_eq!(status.as_deref(), Some("feedback on #1287 sent to agent"));
        assert_eq!(sent, vec![("w1:p1".to_string(), "fix x".to_string())]);
    }

    #[test]
    fn settle_approved_with_notes_forwards() {
        let tmp = tempfile::tempdir().unwrap();
        let t = target(Some("w1:p1"));
        let notes =
            "# Code Review \u{2014} Approved with Notes\nCode review completed.\nrename foo";
        let (status, sent) = run(&t, json("approved", notes), None, tmp.path());
        assert_eq!(status.as_deref(), Some("feedback on #1287 sent to agent"));
        assert_eq!(sent.len(), 1);
    }

    #[test]
    fn settle_bare_approval_sends_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let t = target(Some("w1:p1"));
        let (status, sent) = run(&t, json("approved", BARE_APPROVAL), None, tmp.path());
        assert_eq!(status.as_deref(), Some("#1287 approved"));
        assert!(sent.is_empty());
    }

    #[test]
    fn settle_dismissed_sends_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let t = target(Some("w1:p1"));
        let msg = "Review session closed without feedback.";
        let (status, sent) = run(&t, json("dismissed", msg), None, tmp.path());
        assert_eq!(status.as_deref(), Some("#1287 review dismissed"));
        assert!(sent.is_empty());
    }

    #[test]
    fn settle_empty_annotation_sends_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let t = target(Some("w1:p1"));
        let (status, sent) = run(&t, json("annotated", "  \n"), None, tmp.path());
        assert_eq!(
            status.as_deref(),
            Some("#1287 review closed, nothing to send")
        );
        assert!(sent.is_empty());
    }

    #[test]
    fn settle_saves_when_delivery_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let t = target(Some("w1:p1"));
        let (status, _) = run(
            &t,
            json("annotated", "fix x"),
            Some("agent_blocked"),
            tmp.path(),
        );
        let path = saved_path(tmp.path());
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
    fn settle_saves_output_it_cannot_parse() {
        let tmp = tempfile::tempdir().unwrap();
        let t = target(Some("w1:p1"));
        let raw = "{\"decision\":\"rejected\",\"message\":\"fix x\"}\n";
        let (status, sent) = run(&t, Ok(raw.into()), None, tmp.path());
        let path = saved_path(tmp.path());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
        let want = format!("feedback on #1287 saved to {}: invalid", path.display());
        assert!(status.unwrap().starts_with(&want));
        assert!(sent.is_empty());
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
        let (status, sent) = run(&t, json("annotated", "fix x"), None, tmp.path());
        assert_eq!(status, None);
        assert!(sent.is_empty());
    }

    #[test]
    fn runner_failure_includes_stderr() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("fake-plannotator");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'warming up' >&2\necho 'Not in a VCS repo' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let runner = command_runner(script.to_str().unwrap());
        assert_eq!(
            runner("https://x/1"),
            Err("exit status: 1: Not in a VCS repo".into())
        );
    }

    /// A runner that counts its calls and finishes each review with the next value sent.
    fn gated() -> (
        Runner,
        mpsc::Sender<Result<String, String>>,
        Arc<AtomicUsize>,
    ) {
        let (tx, rx) = mpsc::channel();
        let rx = Arc::new(Mutex::new(rx));
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let runner: Runner = Arc::new(move |_url: &str| {
            counted.fetch_add(1, Ordering::SeqCst);
            rx.lock().unwrap().recv().unwrap()
        });
        (runner, tx, calls)
    }

    fn reviews(runner: Runner, dir: &Path) -> Reviews {
        Reviews::with(runner, Box::new(|_, _| Ok(())), dir.to_path_buf())
    }

    /// Polls until a status arrives, for at most 2 s.
    fn poll_until(r: &mut Reviews) -> Vec<String> {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let got = r.poll();
            if !got.is_empty() || std::time::Instant::now() > end {
                return got;
            }
            std::thread::yield_now();
        }
    }

    fn annotated_json() -> String {
        r#"{"decision":"annotated","message":"fix x"}"#.to_string()
    }

    #[test]
    fn start_twice_same_pr_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let (runner, tx, calls) = gated();
        let mut r = reviews(runner, tmp.path());
        let t = target(Some("w1:p1"));
        assert_eq!(r.start(t.clone()), "reviewing #1287 in Plannotator…");
        assert_eq!(r.start(t), "#1287 already open in Plannotator");
        tx.send(Ok(annotated_json())).unwrap();
        poll_until(&mut r);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn poll_settles_and_frees_the_pr() {
        let tmp = tempfile::tempdir().unwrap();
        let (runner, tx, _) = gated();
        let mut r = reviews(runner, tmp.path());
        let t = target(Some("w1:p1"));
        r.start(t.clone());
        assert!(r.poll().is_empty());
        tx.send(Ok(annotated_json())).unwrap();
        assert_eq!(poll_until(&mut r), vec!["feedback on #1287 sent to agent"]);
        assert_eq!(r.start(t), "reviewing #1287 in Plannotator…");
    }

    #[test]
    fn failed_run_clears_running() {
        let tmp = tempfile::tempdir().unwrap();
        let (runner, tx, calls) = gated();
        let mut r = reviews(runner, tmp.path());
        let t = target(Some("w1:p1"));
        r.start(t.clone());
        tx.send(Err("exit status: 1".into())).unwrap();
        assert_eq!(
            poll_until(&mut r),
            vec!["Plannotator failed for #1287: exit status: 1"]
        );
        assert_eq!(r.start(t), "reviewing #1287 in Plannotator…");
        tx.send(Err("x".into())).unwrap();
        poll_until(&mut r);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn different_prs_run_together() {
        let tmp = tempfile::tempdir().unwrap();
        let (runner, _tx, _) = gated();
        let mut r = reviews(runner, tmp.path());
        let mut other = target(Some("w1:p1"));
        other.url = "https://github.com/heard/heard-ts/pull/1291".into();
        other.number = 1291;
        assert_eq!(
            r.start(target(Some("w1:p1"))),
            "reviewing #1287 in Plannotator…"
        );
        assert_eq!(r.start(other), "reviewing #1291 in Plannotator…");
    }
}
