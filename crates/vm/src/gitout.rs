//! What git writes to its error stream: the progress of a clone or fetch,
//! and why one failed.

use std::sync::LazyLock;

use regex::Regex;

/// A line of git's progress: "Receiving objects:  45% (1234/2742)".
static PROGRESS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(Receiving objects|Resolving deltas|Updating files):\s+\d+% \((\d+)/(\d+)\)")
        .unwrap()
});

/// What git adds after the cause of a failed fetch, the same whatever the
/// cause was.
static ADVICE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(Please make sure you have the correct access rights|and the repository exists\.|fatal: Could not read from remote repository\.)$",
    )
    .unwrap()
});

/// git's output in lines. Progress rewrites its line with carriage returns,
/// so those end lines too.
fn lines(output: &str) -> impl DoubleEndedIterator<Item = &str> {
    output.split(['\r', '\n']).filter(|l| !l.is_empty())
}

/// The last progress line in output, as far as it goes: its stage
/// ("Receiving objects"), and how many of how many are done.
pub fn progress(output: &str) -> Option<(String, i64, i64)> {
    lines(output).rev().find_map(|l| {
        let g = PROGRESS.captures(l.trim())?;
        Some((
            g[1].to_string(),
            g[2].parse().unwrap_or(0),
            g[3].parse().unwrap_or(0),
        ))
    })
}

/// The line of output that says why git failed: the last one that is
/// neither progress, nor what it was doing, nor its closing advice.
pub fn failure(output: &str) -> String {
    lines(output)
        .rev()
        .map(str::trim)
        .find(|l| {
            !l.is_empty()
                && !ADVICE.is_match(l)
                && !PROGRESS.is_match(l)
                && !l.starts_with("Cloning into")
        })
        .unwrap_or("no output")
        .to_string()
}
