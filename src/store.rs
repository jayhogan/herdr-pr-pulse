//! State directory: locked JSON files shared by every pane's instance, plus the
//! shared GitHub fetch cache and its fetch lock.

use std::env;
use std::ffi::OsString;
use std::fs::{self, File, TryLockError};
use std::io;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::github::GhError;
use crate::model::Pr;

/// `cache.json`: the last GitHub fetch, shared by all instances.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Cache {
    pub version: u32,
    pub fetched_at: Option<DateTime<Utc>>,
    pub viewer: String,
    pub authored: Vec<String>,
    pub assigned: Vec<String>,
    pub prs: Vec<Pr>,
    pub error: Option<GhError>,
    pub error_at: Option<DateTime<Utc>>,
}

/// `$XDG_STATE_HOME`, else `$HOME/.local/state`. An empty or relative `XDG_STATE_HOME`
/// counts as unset, as the XDG spec says.
fn state_home(xdg: Option<OsString>, home: Option<OsString>) -> PathBuf {
    xdg.map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| PathBuf::from(home.unwrap_or_default()).join(".local/state"))
}

/// Directory of JSON files, each guarded by an advisory lock on `<name>.lock`.
pub struct StateDir {
    dir: PathBuf,
}

impl StateDir {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// `${XDG_STATE_HOME:-~/.local/state}/herdr-pr-pulse`.
    pub fn default_location() -> Self {
        let home = state_home(env::var_os("XDG_STATE_HOME"), env::var_os("HOME"));
        Self::new(home.join("herdr-pr-pulse"))
    }

    fn lock_file(&self, file_name: &str) -> io::Result<File> {
        fs::create_dir_all(&self.dir)?;
        File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(file_name))
    }

    /// Missing file -> default. Corrupt file -> moved to `<name>.bak`, default.
    pub fn load<T: Default + DeserializeOwned>(&self, name: &str) -> io::Result<T> {
        let path = self.dir.join(name);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(T::default()),
            Err(e) => return Err(e),
        };
        match serde_json::from_slice(&bytes) {
            Ok(value) => Ok(value),
            Err(e) => {
                eprintln!("herdr-pr-pulse: {name} is corrupt ({e}); starting fresh");
                fs::rename(&path, self.dir.join(format!("{name}.bak")))?;
                Ok(T::default())
            }
        }
    }

    /// Locks `<name>.lock`, loads the latest value, applies `change`, writes it
    /// back if `change` returned true, and returns the resulting value.
    pub fn update<T: Default + Serialize + DeserializeOwned>(
        &self,
        name: &str,
        change: impl FnOnce(&mut T) -> bool,
    ) -> io::Result<T> {
        let lock = self.lock_file(&format!("{name}.lock"))?;
        lock.lock()?;
        let mut value: T = self.load(name)?;
        if change(&mut value) {
            let tmp = self.dir.join(format!("{name}.tmp"));
            fs::write(&tmp, serde_json::to_vec_pretty(&value)?)?;
            fs::rename(&tmp, self.dir.join(name))?;
        }
        Ok(value)
        // `lock` is released when dropped here.
    }

    /// Non-blocking: `None` when another instance holds the fetch lock. The
    /// lock is held until the returned file is dropped.
    pub fn try_fetch_lock(&self) -> io::Result<Option<File>> {
        self.try_lock_named("fetch.lock")
    }

    /// Blocking variant, for a manual refresh.
    pub fn fetch_lock(&self) -> io::Result<File> {
        self.lock_named("fetch.lock")
    }

    /// `try_fetch_lock` for the lock file `name`.
    pub fn try_lock_named(&self, name: &str) -> io::Result<Option<File>> {
        let file = self.lock_file(name)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(file)),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(e)) => Err(e),
        }
    }

    /// `fetch_lock` for the lock file `name`.
    pub fn lock_named(&self, name: &str) -> io::Result<File> {
        let file = self.lock_file(name)?;
        file.lock()?;
        Ok(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> (tempfile::TempDir, StateDir) {
        let tmp = tempfile::tempdir().unwrap();
        let state = StateDir::new(tmp.path().join("state"));
        (tmp, state)
    }

    #[test]
    fn empty_xdg_state_home_counts_as_unset() {
        let some = |s: &str| Some(OsString::from(s));
        assert_eq!(
            state_home(some("/x/state"), some("/home/j")),
            PathBuf::from("/x/state")
        );
        let fallback = PathBuf::from("/home/j/.local/state");
        assert_eq!(state_home(None, some("/home/j")), fallback);
        assert_eq!(state_home(some(""), some("/home/j")), fallback);
        // The XDG spec ignores a relative path too.
        assert_eq!(state_home(some("state"), some("/home/j")), fallback);
    }

    #[test]
    fn update_round_trips() {
        let (_tmp, state) = dir();
        let out: Cache = state
            .update("cache.json", |c: &mut Cache| {
                c.version = 1;
                c.viewer = "jay".into();
                c.authored.push("a/b#1".into());
                true
            })
            .unwrap();
        assert_eq!(out.viewer, "jay");
        let loaded: Cache = state.load("cache.json").unwrap();
        assert_eq!(loaded, out);
    }

    #[test]
    fn update_without_change_does_not_write() {
        let (_tmp, state) = dir();
        let _: Cache = state.update("cache.json", |_| false).unwrap();
        assert!(!state.dir.join("cache.json").exists());
    }

    #[test]
    fn missing_file_is_default() {
        let (_tmp, state) = dir();
        let c: Cache = state.load("cache.json").unwrap();
        assert_eq!(c, Cache::default());
    }

    #[test]
    fn corrupt_file_is_backed_up() {
        let (_tmp, state) = dir();
        fs::create_dir_all(&state.dir).unwrap();
        fs::write(state.dir.join("cache.json"), b"{not json").unwrap();
        let c: Cache = state.load("cache.json").unwrap();
        assert_eq!(c, Cache::default());
        assert!(state.dir.join("cache.json.bak").exists());
        assert!(!state.dir.join("cache.json").exists());
    }

    #[test]
    fn fetch_lock_is_exclusive() {
        let (_tmp, state) = dir();
        let first = state.try_fetch_lock().unwrap();
        assert!(first.is_some());
        assert!(state.try_fetch_lock().unwrap().is_none());
        drop(first);
        assert!(state.try_fetch_lock().unwrap().is_some());
    }
}
