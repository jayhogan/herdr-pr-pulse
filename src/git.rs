//! Which repo and branch a workspace's checkout is on.

use std::collections::HashMap;
use std::process::Command;

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Checkout {
    /// `owner/name` of the `origin` remote.
    pub repo: String,
    pub branch: String,
    pub toplevel: String,
}

pub trait Branches {
    fn checkout(&mut self, cwd: &str) -> Option<Checkout>;
}

/// Turns a GitHub remote URL (scp, https or ssh form) into `owner/name`.
pub fn parse_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let path = if let Some(rest) = url.split_once("://").map(|(_, r)| r) {
        rest.split_once('/')?.1
    } else {
        url.split_once(':')?.1
    };
    let path = path.trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

fn git(cwd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Runs git. The branch is read on every call; the remote and toplevel are cached per cwd.
#[derive(Default)]
pub struct GitBranches {
    stable: HashMap<String, Option<(String, String)>>,
}

impl Branches for GitBranches {
    fn checkout(&mut self, cwd: &str) -> Option<Checkout> {
        let stable = self.stable.entry(cwd.to_string()).or_insert_with(|| {
            let toplevel = git(cwd, &["rev-parse", "--show-toplevel"])?;
            let repo = parse_remote(&git(cwd, &["remote", "get-url", "origin"])?)?;
            Some((repo, toplevel))
        });
        let (repo, toplevel) = stable.clone()?;
        let branch = git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?;
        if branch == "HEAD" {
            return None;
        }
        Some(Checkout {
            repo,
            branch,
            toplevel,
        })
    }
}

/// Test and `--fixtures` implementation backed by `branches.json`.
pub struct FixtureBranches {
    map: HashMap<String, Checkout>,
}

impl FixtureBranches {
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json)
            .map(|map| FixtureBranches { map })
            .map_err(|e| format!("invalid branches json: {e}"))
    }

    pub fn embedded() -> Self {
        Self::from_json(include_str!("../tests/fixtures/branches.json"))
            .expect("branches.json is valid")
    }
}

impl Branches for FixtureBranches {
    fn checkout(&mut self, cwd: &str) -> Option<Checkout> {
        self.map.get(cwd).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &std::path::Path, args: &[&str]) {
        let st = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    }

    #[test]
    fn parse_remote_forms() {
        for u in [
            "git@github.com:heard/docket.git",
            "https://github.com/heard/docket",
            "ssh://git@github.com/heard/docket.git",
        ] {
            assert_eq!(parse_remote(u).as_deref(), Some("heard/docket"), "{u}");
        }
        assert_eq!(parse_remote("nonsense"), None);
    }

    #[test]
    fn git_branches_real_repo() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        run(p, &["init", "-b", "main"]);
        run(p, &["commit", "--allow-empty", "-m", "init"]);
        run(
            p,
            &["remote", "add", "origin", "git@github.com:heard/docket.git"],
        );
        run(p, &["checkout", "-b", "feat/x"]);
        let cwd = p.to_str().unwrap();
        let top = std::fs::canonicalize(p)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();

        let mut g = GitBranches::default();
        assert_eq!(
            g.checkout(cwd),
            Some(Checkout {
                repo: "heard/docket".into(),
                branch: "feat/x".into(),
                toplevel: top
            })
        );
        // Branch is re-read each call.
        run(p, &["checkout", "-b", "feat/y"]);
        assert_eq!(g.checkout(cwd).unwrap().branch, "feat/y");
        // Detached HEAD has no branch.
        run(p, &["checkout", "--detach"]);
        assert_eq!(g.checkout(cwd), None);
    }

    #[test]
    fn non_repo_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = GitBranches::default();
        assert_eq!(g.checkout(dir.path().to_str().unwrap()), None);
        assert_eq!(g.checkout("/definitely/not/here"), None);
    }

    #[test]
    fn fixture_branches() {
        let mut f = FixtureBranches::embedded();
        let c = f.checkout("/work/docket-thread").unwrap();
        assert_eq!(
            (c.repo.as_str(), c.branch.as_str()),
            ("heard/docket", "thread-model/model")
        );
        assert_eq!(f.checkout("/nope"), None);
    }
}
