//! Where others are in a shared document: each tab's carets and selections
//! as it tells them (doc.presence), at the version of the document it had,
//! brought forward over what has changed since; and the name and colour
//! this app's own are told with, as the web UI's Code tab chooses them.

use std::collections::VecDeque;

use gpui::{Hsla, Rgba};
use hangar_proto::{ChangeSet, MapMode};
use serde::{Deserialize, Serialize};

/// A tab's presence: who, in what colour, and its selections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Presence {
    pub name: String,
    pub color: String,
    pub ranges: Vec<Sel>,
}

/// A selection in UTF-16 positions: where it began and its caret. Equal,
/// it is a caret alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sel {
    pub anchor: usize,
    pub head: usize,
}

/// The web UI's colours for people, chosen by name.
const COLOURS: [&str; 8] = [
    "#2a78d6", "#d95926", "#1f9d55", "#8250df", "#c9302c", "#0f8fa3", "#b7791f", "#d6336c",
];

/// A person's colour, as the web UI chooses it from their username, so they
/// look the same in both.
pub fn colour_for(name: &str) -> &'static str {
    let h = name
        .encode_utf16()
        .fold(0u32, |h, c| h.wrapping_mul(31).wrapping_add(c as u32));
    COLOURS[h as usize % COLOURS.len()]
}

/// A colour written "#rrggbb".
pub fn parse_colour(s: &str) -> Option<Hsla> {
    let hex = s.strip_prefix('#').filter(|h| h.len() == 6)?;
    let n = u32::from_str_radix(hex, 16).ok()?;
    Some(
        Rgba {
            r: ((n >> 16) & 0xff) as f32 / 255.,
            g: ((n >> 8) & 0xff) as f32 / 255.,
            b: (n & 0xff) as f32 / 255.,
            a: 1.,
        }
        .into(),
    )
}

/// How many received changes are kept, to bring a presence made at an
/// earlier version up to date.
const KEEP: usize = 200;

/// The changes received lately, each with the version it was made on.
#[derive(Debug, Default, Clone)]
pub struct Recent(VecDeque<(u64, ChangeSet)>);

impl Recent {
    pub fn add(&mut self, version: u64, change: ChangeSet) {
        self.0.push_back((version, change));
        while self.0.len() > KEEP {
            self.0.pop_front();
        }
    }

    /// A position in the document at version, in it at synced; None when
    /// what changed between is no longer kept.
    pub fn place(&self, version: u64, synced: u64, pos: usize) -> Option<usize> {
        if version > synced {
            return None;
        }
        let over: Vec<&ChangeSet> = self
            .0
            .iter()
            .filter(|(v, _)| *v >= version && *v < synced)
            .map(|(_, c)| c)
            .collect();
        if (over.len() as u64) < synced - version {
            return None;
        }
        over.into_iter()
            .try_fold(pos, |p, c| c.map_pos(p, 1, MapMode::Simple).ok().flatten())
    }
}
