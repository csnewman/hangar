//! Shared documents with one authority, as `@codemirror/collab` has them.
//!
//! The [`Authority`] holds a document's text and the changes made to it, in
//! order; a document's version is how many there have been. It accepts a
//! client's changes only when they are based on its current version, so it
//! never transforms anything. A [`Client`] keeps the changes it has made that
//! the authority has not yet confirmed, and when others' changes reach it,
//! rebases its own on them and tells its editor what to apply.

use serde::{Deserialize, Serialize};

use crate::change::ChangeSet;
use crate::error::Error;
use crate::text::Text;

/// A change as the authority ordered it, and who made it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Update {
    pub changes: ChangeSet,
    pub client: String,
}

/// One document's text and the changes that made it.
#[derive(Clone, Debug)]
pub struct Authority {
    text: Text,
    // The version of the first update kept.
    first: u64,
    updates: Vec<Update>,
}

impl Authority {
    pub fn new(text: Text) -> Self {
        Authority {
            text,
            first: 0,
            updates: vec![],
        }
    }

    pub fn text(&self) -> &Text {
        &self.text
    }

    pub fn version(&self) -> u64 {
        self.first + self.updates.len() as u64
    }

    /// Takes `changes` from `client`, made one after another starting at
    /// `version`. False, and nothing taken, when `version` is not the
    /// current one: the client has changes to receive first. An error,
    /// and nothing taken, when a change does not fit the text.
    pub fn push(
        &mut self,
        version: u64,
        client: &str,
        changes: &[ChangeSet],
    ) -> Result<bool, Error> {
        if version != self.version() {
            return Ok(false);
        }
        let mut text = None;
        for c in changes {
            text = Some(c.apply(text.as_ref().unwrap_or(&self.text))?);
        }
        if let Some(text) = text {
            self.text = text;
        }
        self.updates.extend(changes.iter().map(|c| Update {
            changes: c.clone(),
            client: client.to_owned(),
        }));
        Ok(true)
    }

    /// The updates after `version`, or None when some of them have been
    /// forgotten: a client that far behind opens the document again.
    pub fn since(&self, version: u64) -> Option<&[Update]> {
        if version < self.first || version > self.version() {
            return None;
        }
        Some(&self.updates[(version - self.first) as usize..])
    }

    /// Forgets the updates before `version`.
    pub fn forget_before(&mut self, version: u64) {
        let n = version
            .saturating_sub(self.first)
            .min(self.updates.len() as u64);
        self.updates.drain(..n as usize);
        self.first += n;
    }
}

/// One editor's side of a shared document.
#[derive(Clone, Debug)]
pub struct Client {
    id: String,
    version: u64,
    unconfirmed: Vec<ChangeSet>,
}

impl Client {
    /// A client that has the document at `version`, as it opened it.
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

    /// The changes to push, at [`Client::version`].
    pub fn pending(&self) -> &[ChangeSet] {
        &self.unconfirmed
    }

    /// Takes the authority's updates from `version` on. Updates the client
    /// has already had are skipped. Returns the change the editor applies to
    /// its text, None when every update was its own.
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
        // The authority took the client's push only at its current version,
        // so the client's own updates come first.
        let own = updates.iter().take_while(|u| u.client == self.id).count();
        let others = &updates[own..];
        if others.iter().any(|u| u.client == self.id) {
            return Err(Error::Order);
        }
        let mut unconfirmed = self.unconfirmed[own.min(self.unconfirmed.len())..].to_vec();
        let Some((first, rest)) = others.split_first() else {
            self.version += updates.len() as u64;
            self.unconfirmed = unconfirmed;
            return Ok(None);
        };
        let mut changes = first.changes.clone();
        for u in rest {
            changes = changes.compose(&u.changes)?;
        }
        for u in &mut unconfirmed {
            let mapped = u.map(&changes, false)?;
            changes = changes.map(u, true)?;
            *u = mapped;
        }
        self.version += updates.len() as u64;
        self.unconfirmed = unconfirmed;
        Ok(Some(changes))
    }
}
