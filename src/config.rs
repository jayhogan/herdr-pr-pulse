//! `config.toml`: the Review view's repo list (spec §3 of the review design).

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// What the Review view gets from the config file.
#[derive(Debug, Clone, PartialEq)]
pub enum ReviewConfig {
    /// No file, no `[review]` table, or no repos in it.
    Missing {
        path: PathBuf,
    },
    Invalid {
        path: PathBuf,
        message: String,
    },
    /// `owner/name`, without repeats, in file order.
    Repos(Vec<String>),
}

#[derive(Deserialize)]
struct File {
    review: Option<Review>,
}

#[derive(Deserialize)]
struct Review {
    #[serde(default)]
    repos: Vec<String>,
}

/// `$XDG_CONFIG_HOME`, else `$HOME/.config`, then `herdr-pr-pulse/config.toml`. An empty or
/// relative `XDG_CONFIG_HOME` counts as unset, as the XDG spec says.
pub fn path(xdg: Option<OsString>, home: Option<OsString>) -> PathBuf {
    xdg.map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(home.unwrap_or_default()).join(".config"))
        .join("herdr-pr-pulse/config.toml")
}

fn is_repo(s: &str) -> bool {
    let mut parts = s.split('/');
    let valid =
        |p: Option<&str>| p.is_some_and(|p| !p.is_empty() && !p.contains(char::is_whitespace));
    valid(parts.next()) && valid(parts.next()) && parts.next().is_none()
}

pub fn parse_review(text: &str, path: &Path) -> ReviewConfig {
    let invalid = |message: String| ReviewConfig::Invalid {
        path: path.into(),
        message,
    };
    let file: File = match toml::from_str(text) {
        Ok(f) => f,
        Err(e) => return invalid(e.message().to_string()),
    };
    let mut repos: Vec<String> = Vec::new();
    for repo in file.review.map(|r| r.repos).unwrap_or_default() {
        if !is_repo(&repo) {
            return invalid(format!("repos: \"{repo}\" is not owner/name"));
        }
        if !repos.contains(&repo) {
            repos.push(repo);
        }
    }
    if repos.is_empty() {
        ReviewConfig::Missing { path: path.into() }
    } else {
        ReviewConfig::Repos(repos)
    }
}

/// Where this process reads its config.
pub fn default_path() -> PathBuf {
    path(env::var_os("XDG_CONFIG_HOME"), env::var_os("HOME"))
}

/// Reads the config file once, at startup.
pub fn load_review() -> ReviewConfig {
    let path = default_path();
    match fs::read_to_string(&path) {
        Ok(text) => parse_review(&text, &path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => ReviewConfig::Missing { path },
        Err(e) => ReviewConfig::Invalid {
            message: e.to_string(),
            path,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_prefers_absolute_xdg() {
        assert_eq!(
            path(Some("/x/cfg".into()), Some("/home/j".into())),
            PathBuf::from("/x/cfg/herdr-pr-pulse/config.toml")
        );
        assert_eq!(
            path(Some("rel".into()), Some("/home/j".into())),
            PathBuf::from("/home/j/.config/herdr-pr-pulse/config.toml")
        );
        assert_eq!(
            path(None, Some("/home/j".into())),
            PathBuf::from("/home/j/.config/herdr-pr-pulse/config.toml")
        );
    }

    #[test]
    fn parses_repos_dedup_in_order() {
        let p = Path::new("/c.toml");
        assert_eq!(
            parse_review(
                "[review]\nrepos = [\"heard/a\", \"heard/b\", \"heard/a\"]\n",
                p
            ),
            ReviewConfig::Repos(vec!["heard/a".into(), "heard/b".into()])
        );
    }

    #[test]
    fn missing_table_or_empty_list_is_missing() {
        let p = Path::new("/c.toml");
        assert_eq!(
            parse_review("", p),
            ReviewConfig::Missing { path: p.into() }
        );
        assert_eq!(
            parse_review("[review]\nrepos = []\n", p),
            ReviewConfig::Missing { path: p.into() }
        );
    }

    #[test]
    fn bad_repo_and_bad_toml_are_invalid() {
        let p = Path::new("/c.toml");
        assert_eq!(
            parse_review("[review]\nrepos = [\"heard\"]\n", p),
            ReviewConfig::Invalid {
                path: p.into(),
                message: "repos: \"heard\" is not owner/name".into()
            }
        );
        assert!(matches!(
            parse_review("[review\n", p),
            ReviewConfig::Invalid { .. }
        ));
        assert!(matches!(
            parse_review("[review]\nrepos = [\"a/b/c\", \"a /b\"]\n", p),
            ReviewConfig::Invalid { .. }
        ));
    }
}
