//! A file open in the editor, shared through the Code service: the
//! service is the document's authority (hangar_proto::collab), the editor
//! one of its clients, as each browser tab of the web UI's Code tab is.
//!
//! gpui-component's editor puts its caret after an edit made from
//! elsewhere and takes that edit into its own undo history
//! (tests/editor.rs), so DocSync keeps both itself: the selection is mapped
//! through what others change, and Undo and Redo are taken before the
//! editor sees them and work through the user's own changes only, mapped
//! through everyone else's. DocSync is the document's side of that, apart
//! from drawing; DocEditor puts it in the window.

use std::ops::Range;
use std::time::{Duration, Instant};

use gpui::{
    App, Context, Entity, EntityInputHandler, FocusHandle, Focusable, IntoElement, ParentElement,
    Render, Styled, Subscription, WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::input::{Editor, EditorState, InputEditsEvent, Position, Redo, Rope, Undo};
use hangar_proto::collab::{self, Update};
use hangar_proto::{Change, ChangeSet, Error, MapMode, Text};
use serde_json::{Value, json};

use crate::api::Client;
use crate::code::{Code, Notice};
use crate::palette;

/// Changes typed within this of each other are one step of undo.
pub const UNDO_GROUP: Duration = Duration::from_millis(800);

/// A document's sync with its authority: the changes to send, the
/// authority's to take, and the user's own steps of undo and redo.
pub struct DocSync {
    client: collab::Client,
    /// The user's own changes, inverted: each undoes one step, at the
    /// document as it is.
    undo: Vec<ChangeSet>,
    redo: Vec<ChangeSet>,
    last_typed: Option<Instant>,
}

impl DocSync {
    /// The document opened at version, as the authority's client id.
    pub fn new(id: &str, version: u64) -> Self {
        DocSync {
            client: collab::Client::new(id, version),
            undo: vec![],
            redo: vec![],
            last_typed: None,
        }
    }

    pub fn version(&self) -> u64 {
        self.client.version()
    }

    /// The authority's id for this client.
    pub fn id(&self) -> &str {
        self.client.id()
    }

    /// Takes a change of the user's that is not a step of undo: what was
    /// typed while the authority could not be told, carried onto the
    /// document it has.
    pub fn carry(&mut self, change: ChangeSet) {
        self.client.local(change);
        self.undo.clear();
        self.redo.clear();
        self.last_typed = None;
    }

    /// The changes to push, at version.
    pub fn pending(&self) -> &[ChangeSet] {
        self.client.pending()
    }

    /// Takes an edit the user made: start..old_end in before (bytes)
    /// became start..new_end in after, at now. Typing close together is
    /// one step of undo.
    pub fn typed(
        &mut self,
        before: &Rope,
        after: &Rope,
        start: usize,
        old_end: usize,
        new_end: usize,
        now: Instant,
    ) {
        let Some((change, inverse)) = single(before, after, start, old_end, new_end) else {
            return;
        };
        self.client.local(change);
        let joins = self
            .last_typed
            .is_some_and(|t| now.saturating_duration_since(t) < UNDO_GROUP);
        match self.undo.last_mut() {
            // Undoing both is undoing this one, then the one before.
            Some(prev) if joins => match inverse.compose(prev) {
                Ok(both) => *prev = both,
                Err(_) => self.undo.push(inverse),
            },
            _ => self.undo.push(inverse),
        }
        self.last_typed = Some(now);
        self.redo.clear();
    }

    /// Takes the authority's updates from version: the change to put into
    /// the editor, if any, with undo and redo mapped through it.
    pub fn receive(
        &mut self,
        version: u64,
        updates: &[Update],
    ) -> Result<Option<ChangeSet>, Error> {
        let Some(change) = self.client.receive(version, updates)? else {
            return Ok(None);
        };
        self.undo = map_all(&self.undo, &change);
        self.redo = map_all(&self.redo, &change);
        self.last_typed = None;
        Ok(Some(change))
    }

    /// The change that undoes the user's last step, made to text; it is
    /// sent as theirs, and can be redone.
    pub fn undo(&mut self, text: &Rope) -> Option<ChangeSet> {
        let change = self.undo.pop()?;
        self.redo.push(invert(&change, text)?);
        self.client.local(change.clone());
        self.last_typed = None;
        Some(change)
    }

    pub fn redo(&mut self, text: &Rope) -> Option<ChangeSet> {
        let change = self.redo.pop()?;
        self.undo.push(invert(&change, text)?);
        self.client.local(change.clone());
        self.last_typed = None;
        Some(change)
    }
}

/// The texts the editor will hold after changes the app put into it. The
/// editor tells of an edit once the update making it is over, so the app
/// knows its own by the text each leaves, not by a flag set while making
/// them; each is passed over as it is told, as not the user's.
#[derive(Default)]
pub struct Echoes(std::collections::VecDeque<Rope>);

impl Echoes {
    pub fn expect(&mut self, text: Rope) {
        self.0.push_back(text);
    }

    /// Whether an edit leaving the text after is one of the app's; it, and
    /// any expected before it, are then done with.
    pub fn take(&mut self, after: &Rope) -> bool {
        let found = self
            .0
            .iter()
            .position(|t| t.len() == after.len() && t == after);
        match found {
            Some(i) => {
                self.0.drain(..=i);
                true
            }
            None => false,
        }
    }
}

/// A change as replacements to make one after another: UTF-16 ranges of
/// the text before it, last first, so each is still where it says.
pub fn replacements(change: &ChangeSet) -> Vec<(Range<usize>, String)> {
    let mut parts = vec![];
    change.iter_changes(false, |fa, ta, _, _, text| {
        parts.push((fa..ta, text.to_string()))
    });
    parts.reverse();
    parts
}

/// Applies a change to a rope, as the editor does.
pub fn apply(change: &ChangeSet, rope: &mut Rope) {
    for (range, text) in replacements(change) {
        let (a, b) = (
            rope.utf16_to_byte_idx(range.start),
            rope.utf16_to_byte_idx(range.end),
        );
        rope.remove(a..b);
        rope.insert(a, &text);
    }
}

/// A selection (UTF-16, anchor..head) mapped through a change: a caret
/// stays after what is inserted at it, a range keeps what it covered.
pub fn map_selection(change: &ChangeSet, sel: Range<usize>) -> Range<usize> {
    let map = |p: usize, assoc: i32| {
        change
            .map_pos(p, assoc, MapMode::Simple)
            .ok()
            .flatten()
            .unwrap_or(p)
    };
    if sel.start == sel.end {
        let p = map(sel.start, 1);
        p..p
    } else {
        map(sel.start, 1)..map(sel.end, -1)
    }
}

/// The change from one text to another, in UTF-16 positions, as few
/// replacements as a character diff finds; None when they are the same.
pub fn difference(before: &str, after: &str) -> Option<ChangeSet> {
    use similar::{DiffOp, TextDiff};
    let diff = TextDiff::from_chars(before, after);
    let (old, new): (Vec<char>, Vec<char>) = (before.chars().collect(), after.chars().collect());
    let u16s = |cs: &[char]| cs.iter().map(|c| c.len_utf16()).sum::<usize>();
    let mut changes = vec![];
    for op in diff.ops() {
        let (o, n) = (op.old_range(), op.new_range());
        if matches!(op, DiffOp::Equal { .. }) {
            continue;
        }
        let from = u16s(&old[..o.start]);
        changes.push(Change {
            from,
            to: from + u16s(&old[o.clone()]),
            insert: Text::from(new[n].iter().collect::<String>().as_str()),
        });
    }
    if changes.is_empty() {
        return None;
    }
    ChangeSet::of(changes, u16s(&old)).ok()
}

/// One edit as a change from before to after, and its inverse.
fn single(
    before: &Rope,
    after: &Rope,
    start: usize,
    old_end: usize,
    new_end: usize,
) -> Option<(ChangeSet, ChangeSet)> {
    if old_end > before.len() || new_end > after.len() || start > old_end.min(new_end) {
        return None;
    }
    let from = before.byte_to_utf16_idx(start);
    let to = before.byte_to_utf16_idx(old_end);
    let inserted = Text::from(after.slice(start..new_end).to_string().as_str());
    let removed = Text::from(before.slice(start..old_end).to_string().as_str());
    let new_to = from + inserted.len();
    let change = ChangeSet::of(
        [Change {
            from,
            to,
            insert: inserted,
        }],
        before.len_utf16(),
    )
    .ok()?;
    let inverse = ChangeSet::of(
        [Change {
            from,
            to: new_to,
            insert: removed,
        }],
        after.len_utf16(),
    )
    .ok()?;
    Some((change, inverse))
}

/// The change that undoes change, made to before.
fn invert(change: &ChangeSet, before: &Rope) -> Option<ChangeSet> {
    let mut parts = vec![];
    change.iter_changes(false, |fa, ta, fb, tb, _| {
        let (a, b) = (before.utf16_to_byte_idx(fa), before.utf16_to_byte_idx(ta));
        parts.push(Change {
            from: fb,
            to: tb,
            insert: Text::from(before.slice(a..b).to_string().as_str()),
        });
    });
    ChangeSet::of(parts, change.new_len()).ok()
}

/// Steps of undo or redo mapped through a change made before them; one
/// that cannot be is dropped, with those before it.
fn map_all(steps: &[ChangeSet], change: &ChangeSet) -> Vec<ChangeSet> {
    let mut out = vec![];
    for s in steps {
        match s.map(change, false) {
            Ok(m) => out.push(m),
            Err(_) => out.clear(),
        }
    }
    out
}

/// The language the editor highlights a file as, by its name.
pub fn language(path: &str) -> &'static str {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    match ext {
        "rs" => "rust",
        "go" => "go",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "py" => "python",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "sh" | "bash" | "zsh" => "bash",
        "css" => "css",
        "html" | "htm" => "html",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        _ if name == "Makefile" => "make",
        _ => "text",
    }
}

/// Whether what the editor shows is on disk.
#[derive(Debug, Clone, PartialEq)]
pub enum Saved {
    Saved,
    /// Changes not yet written: being sent, or written shortly after.
    Saving,
    /// The service could not write the file: why. It writes it again on
    /// the next change.
    Failed(String),
}

/// Where the document is.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    Opening,
    Open,
    /// The connection was lost and is being made again: why. What is typed
    /// meanwhile is kept, and sent once the document is open again.
    Reconnecting(String),
    /// The file was deleted while open.
    Deleted,
    Failed(String),
}

pub struct DocEditor {
    code: Code,
    /// The file's absolute path, once open, and the path asked for.
    path: String,
    asked: String,
    doc: Option<u64>,
    /// The authority's document as the service holds it: changed when the
    /// service starts over, which a reopened document cannot carry on from.
    epoch: String,
    sync: Option<DocSync>,
    /// The version the service last said is on disk, and why its last
    /// write failed, if it did.
    saved: u64,
    save_error: Option<String>,
    editor: Entity<EditorState>,
    /// The editor's text as of the last change seen, to measure the next
    /// one against.
    shadow: Rope,
    /// The texts the app's own edits leave, to pass over when told.
    echoes: Echoes,
    pushing: bool,
    writable: bool,
    state: State,
    /// Where to put the cursor once the file is open: 0-based line and
    /// UTF-16 column.
    go_to: Option<(u32, u32)>,
    /// Whether the editor has been drawn with the file's text: opening
    /// puts it back at the top as it is next drawn, so the cursor is moved
    /// (and scrolled to) only after.
    drawn: bool,
    /// The language server's diagnostics for the file, drawn in it.
    diagnostics: Vec<lsp_types::Diagnostic>,
    _subs: Vec<Subscription>,
}

impl DocEditor {
    /// Opens path (absolute, or from root) through a Code connection in
    /// root.
    pub fn new(
        client: &Client,
        env: &str,
        root: &str,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language(path))
                .line_number(true)
                .searchable(true)
        });
        let edits = cx.subscribe_in(&editor, window, |this, _, e: &InputEditsEvent, _, cx| {
            this.typed(e, cx)
        });
        let (tx, rx) = async_channel::unbounded::<Notice>();
        let code = Code::connect(client, env, root, move |n| tx.send_blocking(n).is_ok());
        cx.spawn_in(window, async move |this: WeakEntity<Self>, cx| {
            while let Ok(n) = rx.recv().await {
                if this
                    .update_in(cx, |d, window, cx| d.heard(n, window, cx))
                    .is_err()
                {
                    return;
                }
            }
        })
        .detach();
        DocEditor {
            code,
            path: path.to_string(),
            asked: path.to_string(),
            doc: None,
            epoch: String::new(),
            sync: None,
            saved: 0,
            save_error: None,
            editor,
            shadow: Rope::new(),
            echoes: Echoes::default(),
            pushing: false,
            writable: false,
            state: State::Opening,
            go_to: None,
            drawn: false,
            diagnostics: vec![],
            _subs: vec![edits],
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// The path asked for when it was opened.
    pub fn asked(&self) -> &str {
        &self.asked
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// Whether what it shows is on disk, once it is open.
    pub fn saved(&self) -> Option<Saved> {
        let s = self.sync.as_ref()?;
        if let Some(e) = &self.save_error {
            return Some(Saved::Failed(e.clone()));
        }
        Some(if !s.pending().is_empty() || s.version() > self.saved {
            Saved::Saving
        } else {
            Saved::Saved
        })
    }

    pub fn editor(&self) -> &Entity<EditorState> {
        &self.editor
    }

    /// The caret's line and column, from 1.
    pub fn position(&self, cx: &App) -> (u32, u32) {
        let p = self.editor.read(cx).cursor_position();
        (p.line + 1, p.character + 1)
    }

    fn heard(&mut self, n: Notice, window: &mut Window, cx: &mut Context<Self>) {
        match n {
            Notice::Ready(_) => {
                // Open before, it carries on from what it had: the service
                // sends what it missed.
                let params = match &self.sync {
                    Some(s) => json!({
                        "path": self.path,
                        "epoch": self.epoch,
                        "client": s.id(),
                        "since": s.version(),
                    }),
                    None => json!({ "path": self.asked }),
                };
                let call = self.code.call("doc.open", params);
                cx.spawn_in(window, async move |this, cx| {
                    let result = call.await;
                    let _ = this.update_in(cx, |d, window, cx| match result {
                        Ok(v) if d.sync.is_some() => d.reopened(v, window, cx),
                        Ok(v) => d.opened(v, window, cx),
                        Err(e) => {
                            d.state = State::Failed(e);
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
            Notice::Lost { why, .. } if self.state != State::Deleted => {
                if self.sync.is_some() {
                    self.state = State::Reconnecting(why);
                    self.doc = None;
                    self.pushing = false;
                    cx.notify();
                }
            }
            Notice::Event(e, p) if self.doc.is_some() && p["doc"].as_u64() == self.doc => {
                match e.as_str() {
                    "doc.changes" => {
                        let updates: Vec<Update> =
                            serde_json::from_value(p["updates"].clone()).unwrap_or_default();
                        self.remote(p["version"].as_u64().unwrap_or(0), &updates, window, cx);
                    }
                    "doc.saved" => {
                        self.save_error = p["error"].as_str().map(str::to_string);
                        if let Some(v) = p["version"].as_u64() {
                            self.saved = v;
                        }
                        cx.notify();
                    }
                    "doc.deleted" => {
                        self.state = State::Deleted;
                        cx.notify();
                    }
                    "doc.restored" => {
                        self.state = State::Open;
                        cx.notify();
                    }
                    _ => {}
                }
            }
            Notice::Closed(why) if self.state != State::Deleted => {
                self.state = State::Failed(why);
                self.sync = None;
                cx.notify();
            }
            _ => {}
        }
    }

    /// Takes the service's answer to opening the document again after the
    /// connection was lost: what it missed, when the service could carry on;
    /// else the text afresh, onto which what was typed meanwhile is put.
    fn reopened(&mut self, v: Value, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sync) = &self.sync else {
            return self.opened(v, window, cx);
        };
        let carried = v["epoch"].as_str() == Some(self.epoch.as_str())
            && v["client"].as_str() == Some(sync.id())
            && v.get("updates").is_some();
        self.doc = v["doc"].as_u64();
        self.writable = v["writable"].as_bool().unwrap_or(false);
        self.state = State::Open;
        if carried {
            let updates: Vec<Update> =
                serde_json::from_value(v["updates"].clone()).unwrap_or_default();
            self.remote(v["version"].as_u64().unwrap_or(0), &updates, window, cx);
            cx.notify();
            return;
        }
        // The service started over: its text is the document, and the
        // editor's differences from it are the user's, sent as such.
        let text = v["text"].as_str().unwrap_or_default().to_string();
        let mut sync = DocSync::new(
            v["client"].as_str().unwrap_or_default(),
            v["version"].as_u64().unwrap_or(0),
        );
        if let Some(change) = difference(&text, &self.shadow.to_string()) {
            sync.carry(change);
        }
        self.epoch = v["epoch"].as_str().unwrap_or_default().to_string();
        self.saved = v["saved"].as_u64().unwrap_or(0);
        self.save_error = None;
        self.sync = Some(sync);
        self.push(cx);
        cx.notify();
    }

    fn opened(&mut self, v: Value, window: &mut Window, cx: &mut Context<Self>) {
        let text = v["text"].as_str().unwrap_or_default().to_string();
        self.epoch = v["epoch"].as_str().unwrap_or_default().to_string();
        let version = v["version"].as_u64().unwrap_or(0);
        self.saved = v["saved"].as_u64().unwrap_or(version);
        self.save_error = None;
        self.doc = v["doc"].as_u64();
        self.path = v["path"].as_str().unwrap_or(&self.asked).to_string();
        self.writable = v["writable"].as_bool().unwrap_or(false);
        self.sync = Some(DocSync::new(
            v["client"].as_str().unwrap_or_default(),
            v["version"].as_u64().unwrap_or(0),
        ));
        // Setting the value is not told as an edit.
        self.echoes = Echoes::default();
        self.editor.update(cx, |e, cx| {
            e.set_value(text, window, cx);
            e.set_selected_range(0..0, cx);
        });
        self.shadow = self.editor.read(cx).text().clone();
        self.state = State::Open;
        self.drawn = false;
        self.show_diagnostics(cx);
        cx.notify();
    }

    /// Draws the language server's diagnostics in the file.
    pub fn set_diagnostics(&mut self, d: Vec<lsp_types::Diagnostic>, cx: &mut Context<Self>) {
        self.diagnostics = d;
        self.show_diagnostics(cx);
    }

    fn show_diagnostics(&mut self, cx: &mut Context<Self>) {
        let d = self.diagnostics.clone();
        self.editor.update(cx, |e, cx| {
            let text = e.text().clone();
            if let Some(set) = e.diagnostics_mut() {
                set.reset(&text);
                set.extend(d);
            }
            cx.notify();
        });
    }

    /// Puts the cursor at a 0-based line and UTF-16 column, once the file
    /// is open.
    pub fn go_to(&mut self, line: u32, column: u32, window: &mut Window, cx: &mut Context<Self>) {
        if self.state != State::Open || !self.drawn {
            self.go_to = Some((line, column));
            cx.notify();
            return;
        }
        self.editor.update(cx, |e, cx| {
            e.set_cursor_position(
                Position {
                    line,
                    character: column,
                },
                window,
                cx,
            )
        });
    }

    /// Takes what the user typed, to send and to undo.
    fn typed(&mut self, e: &InputEditsEvent, cx: &mut Context<Self>) {
        let now = Instant::now();
        for (edit, after) in e.edits() {
            if self.echoes.take(after) {
                self.shadow = after.clone();
                continue;
            }
            if let Some(sync) = &mut self.sync {
                sync.typed(
                    &self.shadow,
                    after,
                    edit.start_byte,
                    edit.old_end_byte,
                    edit.new_end_byte,
                    now,
                );
            }
            self.shadow = after.clone();
        }
        self.push(cx);
    }

    /// Sends the changes not yet confirmed, one push at a time.
    fn push(&mut self, cx: &mut Context<Self>) {
        let (Some(s), Some(doc)) = (&self.sync, self.doc) else {
            return;
        };
        let away = matches!(self.state, State::Reconnecting(_) | State::Failed(_));
        if self.pushing || s.pending().is_empty() || away {
            return;
        }
        self.pushing = true;
        let changes: Vec<Value> = s.pending().iter().map(ChangeSet::to_json).collect();
        let call = self.code.call(
            "doc.push",
            json!({ "doc": doc, "version": s.version(), "changes": changes }),
        );
        cx.spawn(async move |this, cx| {
            if call.await.is_err() {
                // Behind the authority: its changes are on their way, and
                // the push is made again on them.
                cx.background_executor()
                    .timer(Duration::from_millis(200))
                    .await;
            }
            let _ = this.update(cx, |d, cx| {
                d.pushing = false;
                d.push(cx);
            });
        })
        .detach();
    }

    /// Puts the authority's updates into the editor.
    fn remote(
        &mut self,
        version: u64,
        updates: &[Update],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(s) = &mut self.sync else { return };
        match s.receive(version, updates) {
            Ok(Some(change)) => self.apply(&change, false, window, cx),
            Ok(None) => {}
            Err(e) => {
                self.state = State::Failed(format!("lost track of the document: {e}"));
                self.sync = None;
                cx.notify();
                return;
            }
        }
        self.push(cx);
    }

    /// Puts a change into the editor as it stands. Another's keeps the
    /// user's selection where it was; the user's own (an undo, a redo)
    /// leaves the caret at its end.
    fn apply(
        &mut self,
        change: &ChangeSet,
        own: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let before = self.shadow.clone();
        let sel = self.editor.read(cx).selected_range();
        let sel16 = before.byte_to_utf16_idx(sel.start.min(before.len()))
            ..before.byte_to_utf16_idx(sel.end.min(before.len()));
        let mut end = None;
        change.iter_changes(false, |_, _, _, tb, _| end = Some(tb));
        let left = self.editor.update(cx, |e, cx| {
            let mut left = vec![];
            for (range, text) in replacements(change) {
                e.replace_text_in_range(Some(range), &text, window, cx);
                left.push(e.text().clone());
            }
            left
        });
        for t in left {
            self.echoes.expect(t);
        }
        self.shadow = self.editor.read(cx).text().clone();
        let to = match (own, end) {
            (true, Some(end)) => end..end,
            _ => map_selection(change, sel16),
        };
        let s = &self.shadow;
        let cap = |p: usize| s.utf16_to_byte_idx(p.min(s.len_utf16()));
        let range = cap(to.start)..cap(to.end);
        self.editor
            .update(cx, |e, cx| e.set_selected_range(range, cx));
    }

    fn undo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.writable {
            return;
        }
        let Some(change) = self.sync.as_mut().and_then(|s| s.undo(&self.shadow)) else {
            return;
        };
        self.apply(&change, true, window, cx);
        self.push(cx);
    }

    fn redo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.writable {
            return;
        }
        let Some(change) = self.sync.as_mut().and_then(|s| s.redo(&self.shadow)) else {
            return;
        };
        self.apply(&change, true, window, cx);
        self.push(cx);
    }
}

impl Focusable for DocEditor {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.editor.read(cx).focus_handle(cx)
    }
}

impl Render for DocEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        if self.state == State::Open {
            if !self.drawn {
                self.drawn = true;
                if self.go_to.is_some() {
                    cx.notify();
                }
            } else if let Some((line, column)) = self.go_to.take() {
                self.go_to(line, column, window, cx);
            }
        }
        let notice = match &self.state {
            State::Opening => Some("Opening…".to_string()),
            State::Deleted => Some("The file has been deleted.".to_string()),
            State::Failed(e) => Some(format!("Not connected: {e}")),
            State::Reconnecting(why) => Some(format!(
                "Reconnecting ({why}); what you type is kept and saved once it is back"
            )),
            State::Open if self.save_error.is_some() => {
                self.save_error.as_ref().map(|e| format!("Not saved: {e}"))
            }
            State::Open if !self.writable => Some("Read-only".to_string()),
            State::Open => None,
        };
        div()
            .size_full()
            .relative()
            .capture_action(cx.listener(|d, _: &Undo, window, cx| {
                cx.stop_propagation();
                d.undo(window, cx);
            }))
            .capture_action(cx.listener(|d, _: &Redo, window, cx| {
                cx.stop_propagation();
                d.redo(window, cx);
            }))
            .child(
                Editor::new(&self.editor)
                    .h_full()
                    .bordered(false)
                    .readonly(!self.writable || self.state != State::Open),
            )
            .when_some(notice, |d, n| {
                d.child(
                    div()
                        .absolute()
                        .top(px(6.))
                        .right(px(14.))
                        .px(px(8.))
                        .py(px(2.))
                        .rounded(px(5.))
                        .bg(p.surface_2)
                        .text_size(px(11.))
                        .text_color(p.muted)
                        .child(n),
                )
            })
    }
}
