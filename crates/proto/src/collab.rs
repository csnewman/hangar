//! Shared documents with one authority, as `@codemirror/collab` 6.1 has them,
//! but rebasing over one update at a time on both sides.
//!
//! The [`Authority`] holds a document's text and the changes made to it, in
//! order; a document's version is how many there have been. A client pushes
//! all its unconfirmed changes with the version they are based on; the
//! authority drops any it already took and rebases the rest over what came
//! since ([`rebase`]), so a push is never refused for being behind. A
//! [`Client`] recognises its own changes among those it receives and rebases
//! what it still has unconfirmed on the others', telling its editor what to
//! apply.
//!
//! Both sides map with the same tie-break: where a client's change and
//! another's insert at one position, the other's, accepted first, goes first.

use ropey::Rope;
use serde::{Deserialize, Serialize};

use crate::change::ChangeSet;
use crate::error::Error;

/// A change as the authority ordered it, and who made it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub changes: ChangeSet,
    pub client: String,
}

/// How much of its history an authority keeps by default, roughly in
/// bytes: a client that has missed more than this opens the document again.
pub const DEFAULT_BUDGET: usize = 32 << 20;

/// One document's text and the changes that made it.
#[derive(Clone, Debug)]
pub struct Authority {
    text: Rope,
    // The version of the first update kept.
    first: u64,
    updates: Vec<Update>,
    kept: usize,
    budget: usize,
}

impl Authority {
    pub fn new(text: Rope) -> Self {
        Self::with_budget(text, DEFAULT_BUDGET)
    }

    /// An authority that keeps about `budget` bytes of updates, forgetting
    /// the oldest beyond that.
    pub fn with_budget(text: Rope, budget: usize) -> Self {
        Authority {
            text,
            first: 0,
            updates: vec![],
            kept: 0,
            budget,
        }
    }

    pub fn text(&self) -> &Rope {
        &self.text
    }

    pub fn version(&self) -> u64 {
        self.first + self.updates.len() as u64
    }

    /// Takes `changes` from `client`: all the changes it has not seen
    /// confirmed, in order, the first based on `version`. Those already
    /// taken are dropped and the rest rebased onto the current text. On an
    /// error nothing is taken.
    pub fn push(&mut self, version: u64, client: &str, changes: &[ChangeSet]) -> Result<(), Error> {
        let over = self.since(version)?;
        let changes = rebase(changes, client, over)?;
        let mut text = self.text.clone();
        for c in &changes {
            c.apply_to_rope(&mut text)?;
        }
        self.text = text;
        for c in changes {
            self.kept += c.weight();
            self.updates.push(Update {
                changes: c,
                client: client.to_owned(),
            });
        }
        let mut n = 0;
        while self.kept > self.budget && n < self.updates.len() {
            self.kept -= self.updates[n].changes.weight();
            n += 1;
        }
        self.updates.drain(..n);
        self.first += n as u64;
        Ok(())
    }

    /// The updates after `version`.
    pub fn since(&self, version: u64) -> Result<&[Update], Error> {
        if version > self.version() {
            return Err(Error::Gap {
                have: self.version(),
                got: version,
            });
        }
        if version < self.first {
            return Err(Error::Forgotten {
                version,
                first: self.first,
            });
        }
        Ok(&self.updates[(version - self.first) as usize..])
    }
}

/// `pushed`, all from `client` and based on the version before `over`,
/// with those `over` already holds dropped and the rest moved to apply
/// after `over`, one update at a time, exactly as the client moves them
/// when it receives `over` ([`Client::receive`]).
pub fn rebase(
    pushed: &[ChangeSet],
    client: &str,
    over: &[Update],
) -> Result<Vec<ChangeSet>, Error> {
    let mut pending = pushed.to_vec();
    for u in over {
        step(&mut pending, client, u)?;
    }
    Ok(pending)
}

// Moves `pending`, the unconfirmed changes of `client`, past one update,
// returning what of the update applies after them: None when it is the
// client's own oldest change coming back. Mapping over a composition of
// changes is not always mapping over each in turn, so this is only ever done
// over a single update: the authority and the client then rebase alike
// however the updates were batched on the way.
fn step(
    pending: &mut Vec<ChangeSet>,
    client: &str,
    u: &Update,
) -> Result<Option<ChangeSet>, Error> {
    if u.client == client && !pending.is_empty() {
        pending.remove(0);
        return Ok(None);
    }
    let mut change = u.changes.clone();
    let mut mapped = Vec::with_capacity(pending.len());
    for p in pending.iter() {
        mapped.push(p.map(&change, false)?);
        change = change.map(p, true)?;
    }
    *pending = mapped;
    Ok(Some(change))
}

/// One editor's side of a shared document.
#[derive(Clone, Debug)]
pub struct Client {
    id: String,
    version: u64,
    unconfirmed: Vec<ChangeSet>,
}

impl Client {
    /// A client that has the document at `version`, as it opened it. Its
    /// `id` must be unique among the document's clients.
    pub fn new(id: impl Into<String>, version: u64) -> Self {
        Client {
            id: id.into(),
            version,
            unconfirmed: vec![],
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// The version the client's text is based on: the last one received.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Records a change the editor has made to its text.
    pub fn local(&mut self, changes: ChangeSet) {
        if !changes.is_empty() {
            self.unconfirmed.push(changes);
        }
    }

    /// The changes to push, at [`Client::version`]: all of them, every time,
    /// since the authority drops those it already has.
    pub fn pending(&self) -> &[ChangeSet] {
        &self.unconfirmed
    }

    /// Takes the authority's updates from `version` on, skipping any the
    /// client has already had. Returns the change the editor applies to its
    /// text, None when there is nothing to apply: `receiveUpdates` in
    /// `@codemirror/collab` given one update at a time.
    pub fn receive(
        &mut self,
        version: u64,
        updates: &[Update],
    ) -> Result<Option<ChangeSet>, Error> {
        if version > self.version {
            return Err(Error::Gap {
                have: self.version,
                got: version,
            });
        }
        let skip = (self.version - version) as usize;
        let Some(updates) = updates.get(skip..) else {
            return Ok(None);
        };
        let mut unconfirmed = self.unconfirmed.clone();
        let mut changes: Option<ChangeSet> = None;
        for u in updates {
            if let Some(c) = step(&mut unconfirmed, &self.id, u)? {
                changes = Some(match changes {
                    Some(prev) => prev.compose(&c)?,
                    None => c,
                });
            }
        }
        self.version += updates.len() as u64;
        self.unconfirmed = unconfirmed;
        Ok(changes)
    }
}
