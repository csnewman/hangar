//! Annotate with Git Blame: who last changed each line of a file and when,
//! from the Code service's git.blame, shown beside the editor's lines.

use std::collections::HashMap;

use chrono::{DateTime, Local};
use serde::Deserialize;

/// A commit a line was last changed in.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Commit {
    pub author: String,
    pub time: f64,
    pub summary: String,
}

/// Each line's commit, by hash; a line not yet committed has the all-zero
/// hash.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
#[serde(default)]
pub struct Blame {
    pub commits: HashMap<String, Commit>,
    pub lines: Vec<String>,
}

pub const UNCOMMITTED: &str = "0000000000000000000000000000000000000000";

impl Blame {
    /// A line's commit (0-based), with its hash.
    pub fn commit_of(&self, line: usize) -> Option<(&str, &Commit)> {
        let hash = self.lines.get(line)?;
        Some((hash.as_str(), self.commits.get(hash)?))
    }

    /// What is shown beside a line: when and by whom, or that it is not
    /// committed.
    pub fn label(&self, line: usize, now: DateTime<Local>) -> Option<String> {
        let (hash, c) = self.commit_of(line)?;
        if hash == UNCOMMITTED {
            return Some("Not committed".into());
        }
        Some(format!(
            "{}  {}",
            crate::gitlog::when(c.time, now),
            c.author
        ))
    }

    /// What a line's tooltip says: the commit, its author and subject.
    pub fn about(&self, line: usize) -> Option<String> {
        let (hash, c) = self.commit_of(line)?;
        if hash == UNCOMMITTED {
            return Some("Changed here, not yet committed.".into());
        }
        let short: String = hash.chars().take(8).collect();
        Some(format!("{short} {}\n{}", c.author, c.summary))
    }
}

/// Asks for a commit in the Git window: one a line's annotation names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShowCommit(pub String);
