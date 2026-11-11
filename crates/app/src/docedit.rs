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
use std::collections::HashMap;

use gpui::Hsla;
use gpui_component::h_flex;
use gpui_component::input::{
    Editor, EditorState, GutterDecoration, GutterDecorationCollection, InputEditsEvent, Position,
    RangeDecoration, RangeDecorationCollection, RangeDecorationStyle, Redo, Rope, Undo,
};
use gpui_component::menu::PopupMenu;
use hangar_proto::collab::{self, Update};
use hangar_proto::{Change, ChangeSet, Error, MapMode, Text};
use serde_json::{Value, json};

use crate::api::Client;
use crate::code::{Code, Notice};
use crate::palette;
use crate::presence::{Presence, Sel, parse_colour};

/// How long others' edits stay marked.
const EDITED_FOR: Duration = Duration::from_secs(5);

/// Another tab in the document: its name and colour, and its carets and
/// selections drawn, which move with the text as it is edited.
struct Remote {
    name: String,
    colour: Hsla,
    marks: RangeDecorationCollection,
    gutter: GutterDecorationCollection,
}

/// Changes typed within this of each other are one step of undo.
pub const UNDO_GROUP: Duration = Duration::from_millis(800);

/// A document's sync with its authority: the changes to send, the
/// authority's to take, and the user's own steps of undo and redo.
pub struct DocSync {
    client: collab::Client,
    /// The authority's changes received lately, to place others' carets
    /// made at an earlier version.
    recent: crate::presence::Recent,
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
            recent: Default::default(),
            undo: vec![],
            redo: vec![],
            last_typed: None,
        }
    }

    pub fn version(&self) -> u64 {
        self.client.version()
    }

    /// A position another tab gave at version, in the editor's text: moved
    /// over what the authority has changed since, then over this client's
    /// changes it has not yet confirmed. None when it is from too long ago.
    pub fn place(&self, version: u64, pos: usize) -> Option<usize> {
        let p = self.recent.place(version, self.version(), pos)?;
        self.pending()
            .iter()
            .try_fold(p, |p, c| c.map_pos(p, 1, MapMode::Simple).ok().flatten())
    }

    /// The authority's id for this client.
    pub fn id(&self) -> &str {
        self.client.id()
    }

    /// Takes a change the user asked for in one go, made to before (a
    /// rename's edits): sent as theirs, and one step of undo.
    pub fn own(&mut self, change: ChangeSet, before: &Rope) {
        if let Some(inverse) = invert(&change, before) {
            self.undo.push(inverse);
        }
        self.client.local(change);
        self.redo.clear();
        self.last_typed = None;
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
        let had = self.client.version();
        for (k, u) in updates.iter().enumerate() {
            let v = version + k as u64;
            if v >= had {
                self.recent.add(v, u.changes.clone());
            }
        }
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

/// A place typed as Go to Line takes it: "45", or "45:9", from 1; the
/// column is 1 when not given.
pub fn line_column(typed: &str) -> Option<(u32, u32)> {
    let (line, col) = match typed.split_once(':') {
        Some((l, c)) => (l.trim(), c.trim()),
        None => (typed.trim(), "1"),
    };
    let (line, col) = (line.parse::<u32>().ok()?, col.parse::<u32>().ok()?);
    (line > 0 && col > 0).then_some((line, col))
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

gpui::actions!(
    doc_editor,
    [
        DuplicateLine,
        DeleteLine,
        MoveLineUp,
        MoveLineDown,
        CommentLine,
        JoinLines,
        Reformat,
        ToggleSoftWrap,
        ToggleBlame,
        ParameterInfo,
        ContextActions,
        SelectNextOccurrence,
        UnselectOccurrence,
        SelectAllOccurrences,
        NextProblem,
        PreviousProblem,
    ]
);

/// The editor's keys for IntelliJ's line commands, in a file's editor only
/// (other text fields keep their own), bound after gpui-component's so they
/// take Cmd-Backspace's place there.
pub fn bind_keys(cx: &mut App) {
    use gpui::KeyBinding;
    let ctx = Some("DocEditor > Input");
    #[cfg(target_os = "macos")]
    let keys = [
        "cmd-d",
        "cmd-backspace",
        "cmd-/",
        "cmd-alt-l",
        "cmd-p",
        "cmd-.",
        "cmd-b",
    ];
    #[cfg(not(target_os = "macos"))]
    let keys = [
        "ctrl-d",
        "ctrl-y",
        "ctrl-/",
        "ctrl-alt-l",
        "ctrl-p",
        "ctrl-.",
        "ctrl-b",
    ];
    // IntelliJ's Add Selection for Next Occurrence, Unselect Occurrence and
    // Select All Occurrences.
    #[cfg(target_os = "macos")]
    let occurrences = ["ctrl-g", "ctrl-shift-g", "ctrl-cmd-g"];
    #[cfg(not(target_os = "macos"))]
    let occurrences = ["alt-j", "alt-shift-j", "ctrl-alt-shift-j"];
    cx.bind_keys([
        KeyBinding::new(keys[0], DuplicateLine, ctx),
        KeyBinding::new(keys[1], DeleteLine, ctx),
        KeyBinding::new("alt-shift-up", MoveLineUp, ctx),
        KeyBinding::new("alt-shift-down", MoveLineDown, ctx),
        KeyBinding::new(keys[2], CommentLine, ctx),
        KeyBinding::new("ctrl-shift-j", JoinLines, ctx),
        KeyBinding::new(keys[3], Reformat, ctx),
        KeyBinding::new(keys[4], ParameterInfo, ctx),
        // IntelliJ's Show Context Actions: the server's quick fixes and
        // other code actions; the editor's own key for them opens the same.
        KeyBinding::new("alt-enter", ContextActions, ctx),
        KeyBinding::new(keys[5], ContextActions, ctx),
        // IntelliJ's Basic Completion and Go to Declaration; F12 as VS Code
        // has it.
        KeyBinding::new("ctrl-space", gpui_component::input::ShowCompletions, ctx),
        KeyBinding::new(keys[6], gpui_component::input::GoToDefinition, ctx),
        KeyBinding::new("f12", gpui_component::input::GoToDefinition, ctx),
        KeyBinding::new(occurrences[0], SelectNextOccurrence, ctx),
        KeyBinding::new(occurrences[1], UnselectOccurrence, ctx),
        KeyBinding::new(occurrences[2], SelectAllOccurrences, ctx),
        KeyBinding::new("f2", NextProblem, ctx),
        KeyBinding::new("shift-f2", PreviousProblem, ctx),
    ]);
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

/// How a Markdown file is shown: its text, its preview, or both side by
/// side, as the web UI's Code tab opens it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preview {
    Text,
    Both,
    Rendered,
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

/// A file's editor as the app draws it: the height it is given, with no
/// border of its own. gpui-component lays an input out as a row with a gap
/// between its parts, and its popovers (hover, a diagnostic, completions)
/// join that row while open, drawn elsewhere but taking a gap from the
/// text's width, so a soft-wrapped line moved as the pointer rested on a
/// symbol; with no gap the text keeps its width.
pub fn code_editor(state: &Entity<EditorState>) -> Editor {
    Editor::new(state).h_full().bordered(false).gap_0()
}

/// The editor's menu, as IntelliJ's: going to and renaming the symbol at
/// the caret, reformatting and commenting, the clipboard, and soft wrap.
/// Each item names its action, so the menu shows its key.
fn editor_menu(
    menu: gpui_component::native_menu::NativeMenu,
    comment: bool,
    wrap: bool,
    blame: bool,
    _: &App,
) -> gpui_component::native_menu::NativeMenu {
    use gpui_component::input::{Copy, Cut, GoToDefinition, Paste, SelectAll};
    menu.menu("Go to Definition", Box::new(GoToDefinition))
        .menu("Find Usages", Box::new(crate::envtab::FindUsages))
        .menu("Rename…", Box::new(crate::envtab::RenameSymbol))
        .separator()
        .menu("Reformat Code", Box::new(Reformat))
        .menu_with_disabled("Comment with Line Comment", !comment, Box::new(CommentLine))
        .menu("Duplicate Line or Selection", Box::new(DuplicateLine))
        .separator()
        .menu("Cut", Box::new(Cut))
        .menu("Copy", Box::new(Copy))
        .menu("Paste", Box::new(Paste))
        .menu("Select All", Box::new(SelectAll))
        .separator()
        .menu_with_check("Soft-Wrap", wrap, Box::new(ToggleSoftWrap))
        .menu_with_check("Annotate with Git Blame", blame, Box::new(ToggleBlame))
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
    /// How a Markdown file is shown; None for any other.
    preview: Option<Preview>,
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
    /// The soft wrap last given to the editor, from the settings.
    wrapped: Option<bool>,
    /// The language server's diagnostics for the file, drawn in it.
    diagnostics: Vec<lsp_types::Diagnostic>,
    /// Who last changed each line, while it is annotated; why it could
    /// not be.
    blame: Option<crate::blame::Blame>,
    blame_note: Option<String>,
    /// Who this app's user is to the others: a name and a colour.
    me: Option<(String, String)>,
    /// The other tabs in the document, and those whose presence came from
    /// a version not yet received.
    remotes: HashMap<String, Remote>,
    early: HashMap<String, (u64, Presence)>,
    /// Others' latest edits, marked for a moment, and a count of the marks
    /// made, so an older one's timer leaves a newer mark.
    edited: Option<(RangeDecorationCollection, GutterDecorationCollection)>,
    edited_gen: u64,
    /// The selection last told to the others, and whether telling is due.
    sent: Option<(usize, usize)>,
    sending: bool,
    /// The file's language servers, and the signatures shown of the call
    /// the caret is in, with a count of the times asked, so only the last
    /// answer is shown.
    languages: Option<WeakEntity<crate::languages::Languages>>,
    /// The completions last offered, to know which was taken.
    offered: Option<crate::lspedit::Offered>,
    /// The file's line separators and encoding as it was opened, and its
    /// indentation, for the status bar.
    props: Option<crate::props::Props>,
    indent: Option<crate::lineops::Indent>,
    signatures: Option<lsp_types::SignatureHelp>,
    signatures_asked: u64,
    /// Where the caret was when they were last asked for.
    sig_caret: Option<usize>,
    /// The code actions at the caret as ⌥↩ lists them, where the caret
    /// was, and whether the list's first item is still to be marked.
    context_menu: Option<(Entity<PopupMenu>, Subscription)>,
    context_at: Option<gpui::Bounds<gpui::Pixels>>,
    context_unmarked: bool,
    /// The other uses of the symbol at the caret, shaded, where the caret
    /// was when they were asked for, and a count of the times asked.
    uses: Option<RangeDecorationCollection>,
    uses_caret: Option<usize>,
    uses_asked: u64,
    /// The selections ⌃G has made.
    occurrences: crate::occur::Occurrences,
    /// The problem F2 went to, said over the caret until it moves: where
    /// the caret was put, and the problem.
    problem_shown: Option<(usize, lsp_types::Diagnostic)>,
    /// The bracket at the caret and its partner, framed.
    brackets: Option<RangeDecorationCollection>,
    _subs: Vec<Subscription>,
}

impl DocEditor {
    /// Opens path (absolute, or from root) through a Code connection in
    /// root; me is the name and colour the others see this user by.
    pub fn new(
        client: &Client,
        env: &str,
        root: &str,
        path: &str,
        me: Option<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language(path))
                .line_number(true)
                .searchable(true)
        });
        let edits = cx.subscribe_in(
            &editor,
            window,
            |this, _, e: &InputEditsEvent, window, cx| this.typed(e, window, cx),
        );
        let moved = cx.observe(&editor, |this, _, cx| {
            this.selection_moved(cx);
            this.caret_moved(cx);
            // The annotations follow the lines as they scroll.
            if this.blame.is_some() {
                cx.notify();
            }
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
            preview: (language(path) == "markdown").then_some(Preview::Both),
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
            wrapped: None,
            diagnostics: vec![],
            blame: None,
            blame_note: None,
            me,
            remotes: HashMap::new(),
            early: HashMap::new(),
            edited: None,
            edited_gen: 0,
            sent: None,
            sending: false,
            languages: None,
            offered: None,
            props: None,
            indent: None,
            signatures: None,
            signatures_asked: 0,
            sig_caret: None,
            context_menu: None,
            context_at: None,
            context_unmarked: false,
            uses: None,
            uses_caret: None,
            uses_asked: 0,
            occurrences: Default::default(),
            problem_shown: None,
            brackets: None,
            _subs: vec![edits, moved],
        }
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn blame(&self) -> Option<&crate::blame::Blame> {
        self.blame.as_ref()
    }

    /// Annotate with Git Blame, or the annotations taken away.
    pub fn set_blame(&mut self, on: bool, cx: &mut Context<Self>) {
        if !on {
            self.blame = None;
            self.blame_note = None;
            cx.notify();
            return;
        }
        if self.blame.is_none() {
            self.read_blame(cx);
        }
    }

    fn read_blame(&mut self, cx: &mut Context<Self>) {
        let call = self
            .code
            .call("git.blame", serde_json::json!({ "path": self.path }));
        cx.spawn(async move |this, cx| {
            let got = call.await.and_then(|v| {
                serde_json::from_value::<crate::blame::Blame>(v).map_err(|e| e.to_string())
            });
            let _ = this.update(cx, |d, cx| {
                match got {
                    Ok(b) => {
                        d.blame = Some(b);
                        d.blame_note = None;
                    }
                    Err(e) => d.blame_note = Some(format!("No annotations: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The column of annotations beside the editor: each line shown, its
    /// commit's date and author, at the line's height.
    fn blame_column(
        &self,
        p: &crate::palette::Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        use gpui_component::RopeExt;
        let Some(blame) = &self.blame else {
            return div().into_any_element();
        };
        let e = self.editor.read(cx);
        let top = e.input_bounds().origin.y;
        let height = e.line_height().unwrap_or(px(18.));
        let text = e.text();
        let now = chrono::Local::now();
        let mut col = div()
            .id("blame")
            .relative()
            .w(px(180.))
            .h_full()
            .flex_none()
            .overflow_hidden()
            .border_r_1()
            .border_color(p.border)
            .bg(p.surface);
        let mut last: Option<&str> = None;
        for line in 0..text.lines_len().min(blame.lines.len()) {
            let at = text.line_start_offset(line);
            let Some(b) = e.range_to_bounds(&(at..at)) else {
                last = None;
                continue;
            };
            let hash = blame.lines[line].as_str();
            // A run of lines from one commit is labelled once.
            let first = last != Some(hash);
            last = Some(hash);
            let (label, about) = (
                blame.label(line, now).unwrap_or_default(),
                blame.about(line).unwrap_or_default(),
            );
            let show = hash.to_string();
            col = col.child(
                div()
                    .id(("blame-line", line))
                    .debug_selector(move || format!("blame-{line}"))
                    .absolute()
                    .top(b.top() - top)
                    .left_0()
                    .w_full()
                    .h(height)
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .truncate()
                    .when(first, |d| d.child(label))
                    .hover(|s| s.bg(p.surface_2))
                    .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(about.clone()).build(window, cx)
                    })
                    .when(show != crate::blame::UNCOMMITTED, |d| {
                        d.cursor_pointer().on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(crate::blame::ShowCommit(show.clone()))
                        }))
                    }),
            );
        }
        col.into_any_element()
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
    /// The file's line separators and encoding, once read, and its
    /// indentation, for the status bar.
    pub fn file_props(&self) -> (Option<&crate::props::Props>, Option<crate::lineops::Indent>) {
        (self.props.as_ref(), self.indent)
    }

    /// Reads the file's line separators and encoding from disk.
    fn read_props(&mut self, cx: &mut Context<Self>) {
        let call = self.code.call("fs.textProps", json!({ "path": self.path }));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(props)) = call
                .await
                .map(serde_json::from_value::<crate::props::Props>)
            else {
                return;
            };
            let _ = this.update(cx, |d, cx| {
                d.props = Some(props);
                cx.notify();
            });
        })
        .detach();
    }

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
                    "doc.presence" => {
                        let client = p["client"].as_str().unwrap_or_default().to_string();
                        let state = serde_json::from_value(p["state"].clone()).ok();
                        self.presence(client, p["version"].as_u64().unwrap_or(0), state, cx);
                    }
                    "doc.saved" => {
                        self.save_error = p["error"].as_str().map(str::to_string);
                        if let Some(v) = p["version"].as_u64() {
                            self.saved = v;
                        }
                        if self.blame.is_some() {
                            self.read_blame(cx);
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
            self.presences(&v, cx);
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
        self.presences(&v, cx);
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
        self.indent = crate::lineops::detect_indent(&self.shadow.to_string());
        self.read_props(cx);
        self.show_diagnostics(cx);
        self.presences(&v, cx);
        cx.notify();
    }

    /// Takes the others' presence as the document was opened with them.
    fn presences(&mut self, v: &Value, cx: &mut Context<Self>) {
        for r in std::mem::take(&mut self.remotes).into_values() {
            r.marks.dispose(cx);
            r.gutter.dispose(cx);
        }
        self.early.clear();
        self.sent = None;
        for p in v["presence"].as_array().into_iter().flatten() {
            let state = serde_json::from_value(p["state"].clone()).ok();
            let client = p["client"].as_str().unwrap_or_default().to_string();
            self.presence(client, p["version"].as_u64().unwrap_or(0), state, cx);
        }
    }

    /// The names and colours of the others in the document.
    pub fn others(&self) -> Vec<(String, Hsla)> {
        let mut o: Vec<_> = self
            .remotes
            .values()
            .map(|r| (r.name.clone(), r.colour))
            .collect();
        o.sort_by(|a, b| a.0.cmp(&b.0));
        o
    }

    /// Draws another tab's carets and selections, made at version, where
    /// they are now; None is the tab gone.
    fn presence(
        &mut self,
        client: String,
        version: u64,
        state: Option<Presence>,
        cx: &mut Context<Self>,
    ) {
        self.early.remove(&client);
        let Some(state) = state else {
            if let Some(r) = self.remotes.remove(&client) {
                r.marks.dispose(cx);
                r.gutter.dispose(cx);
            }
            cx.notify();
            return;
        };
        let Some(sync) = &self.sync else {
            return;
        };
        if version > sync.version() {
            self.early.insert(client, (version, state));
            return;
        }
        let s = &self.shadow;
        let len16 = s.len_utf16();
        // Made too long ago to move: shown where it said, until it moves.
        let at = |p: usize| sync.place(version, p).unwrap_or(p).min(len16);
        let byte = |p: usize| s.utf16_to_byte_idx(p);
        let colour = parse_colour(&state.color).unwrap_or(palette::of(cx).peer);
        let (mut marks, mut gutter) = (vec![], vec![]);
        for Sel { anchor, head } in &state.ranges {
            let (a, h) = (at(*anchor), at(*head));
            if a != h {
                marks.push(
                    RangeDecoration::new(byte(a.min(h))..byte(a.max(h)))
                        .with_style(RangeDecorationStyle::Fill)
                        .with_color(colour.opacity(0.22)),
                );
            }
            // The caret: a frame round the character after it, or before
            // it at the end.
            let caret = if h < len16 {
                byte(h)..byte(h + 1)
            } else {
                byte(h.saturating_sub(1))..byte(h)
            };
            if caret.start < caret.end {
                marks.push(
                    RangeDecoration::new(caret)
                        .with_style(RangeDecorationStyle::Frame)
                        .with_color(colour),
                );
            }
            gutter.push(GutterDecoration::new(byte(h)..byte(h)).with_color(colour));
        }
        match self.remotes.get_mut(&client) {
            Some(r) => {
                r.name = state.name;
                r.colour = colour;
                r.marks.set(marks, cx);
                r.gutter.set(gutter, cx);
            }
            None => {
                let (marks, gutter) = self.editor.update(cx, |e, cx| {
                    (
                        e.create_range_decorations_collection(marks, cx),
                        e.create_gutter_decorations_collection(gutter, cx),
                    )
                });
                self.remotes.insert(
                    client,
                    Remote {
                        name: state.name,
                        colour,
                        marks,
                        gutter,
                    },
                );
            }
        }
        cx.notify();
    }

    /// Marks for a moment what others just changed: the text they put in.
    fn mark_edited(&mut self, change: &ChangeSet, cx: &mut Context<Self>) {
        let s = &self.shadow;
        let mut ranges = vec![];
        change.iter_changes(false, |_, _, fb, tb, _| {
            if tb > fb {
                ranges.push(s.utf16_to_byte_idx(fb)..s.utf16_to_byte_idx(tb));
            }
        });
        if ranges.is_empty() {
            return;
        }
        let colour = palette::of(cx).peer;
        let marks: Vec<_> = ranges
            .iter()
            .map(|r| {
                RangeDecoration::new(r.clone())
                    .with_style(RangeDecorationStyle::Fill)
                    .with_color(colour.opacity(0.28))
            })
            .collect();
        let gutter: Vec<_> = ranges
            .iter()
            .map(|r| GutterDecoration::new(r.clone()).with_color(colour))
            .collect();
        match &self.edited {
            Some((m, g)) => {
                m.append(marks, cx);
                g.append(gutter, cx);
            }
            None => {
                self.edited = Some(self.editor.update(cx, |e, cx| {
                    (
                        e.create_range_decorations_collection(marks, cx),
                        e.create_gutter_decorations_collection(gutter, cx),
                    )
                }));
            }
        }
        self.edited_gen += 1;
        let generation = self.edited_gen;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(EDITED_FOR).await;
            let _ = this.update(cx, |d, cx| {
                if d.edited_gen == generation
                    && let Some((m, g)) = &d.edited
                {
                    m.clear(cx);
                    g.clear(cx);
                }
            });
        })
        .detach();
    }

    /// Tells the others where this user's caret and selection are, shortly
    /// after they move. Told at the version the authority confirmed, so
    /// only while no change of this user's awaits confirming.
    /// Gives the editor its file's language servers, which Parameter Info
    /// asks.
    pub fn set_languages(
        &mut self,
        languages: WeakEntity<crate::languages::Languages>,
        offered: crate::lspedit::Offered,
    ) {
        self.languages = Some(languages);
        self.offered = Some(offered);
    }

    /// Makes a completion's other edits once it is taken, as its server
    /// asks (the import an item from elsewhere needs): asked of the server
    /// first when it left them to be filled in. The completion taken is
    /// the one offered whose text was just put in.
    fn completed(&mut self, inserted: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(offered) = &self.offered else {
            return;
        };
        let Some(item) = crate::lspedit::taken(offered, inserted) else {
            return;
        };
        let edits = item.additional_text_edits.clone().unwrap_or_default();
        if !edits.is_empty() {
            self.make_edits(&edits, window, cx);
            return;
        }
        let Some(l) = self.languages.as_ref().and_then(|l| l.upgrade()) else {
            return;
        };
        if item.data.is_none() {
            return;
        }
        let abs = self.asked.clone();
        let call = l.update(cx, |l, cx| {
            l.request(&abs, "completionItem/resolve", json!(item), cx)
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(v) = call.await else { return };
            let Ok(item) = serde_json::from_value::<lsp_types::CompletionItem>(v) else {
                return;
            };
            let edits = item.additional_text_edits.unwrap_or_default();
            let _ = this.update_in(cx, |d, window, cx| d.make_edits(&edits, window, cx));
        })
        .detach();
    }

    /// Makes a language server's edits to the file as the user's own.
    fn make_edits(
        &mut self,
        edits: &[lsp_types::TextEdit],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = self.editor.read(cx).text().clone();
        if let Some(change) = crate::lspops::changeset(&text, edits) {
            self.apply_own(change, window, cx);
        }
    }

    /// The signatures shown of the call the caret is in, if any.
    pub fn signatures(&self) -> Option<&lsp_types::SignatureHelp> {
        self.signatures.as_ref()
    }

    /// Asks for Parameter Info after text typed: a character the server
    /// names, or any change while it is shown. It waits a moment, so the
    /// change reaches the server first and a run of keys asks once.
    fn typed_for_signatures(&mut self, inserted: &str, cx: &mut Context<Self>) {
        let Some(l) = self.languages.as_ref().and_then(|l| l.upgrade()) else {
            return;
        };
        let (triggers, retriggers) = l.read(cx).signature_triggers(&self.asked);
        let shown = self.signatures.is_some();
        if let Some(ask) = crate::sighelp::asks(inserted, triggers, retriggers, shown) {
            self.ask_signatures(ask, Duration::from_millis(40), cx);
        }
    }

    /// Asks again as the caret moves while signatures are shown: within the
    /// call the parameter moves on, and out of it they go.
    fn caret_moved(&mut self, cx: &mut Context<Self>) {
        let at = self.editor.read(cx).cursor();
        if self.problem_shown.as_ref().is_some_and(|(p, _)| *p != at) {
            self.problem_shown = None;
            cx.notify();
        }
        if self.uses_caret != Some(at) {
            self.uses_caret = Some(at);
            self.ask_uses(cx);
            self.frame_brackets(at, cx);
        }
        if self.signatures.is_none() || self.sig_caret == Some(at) {
            return;
        }
        self.sig_caret = Some(at);
        self.ask_signatures(crate::sighelp::Ask::Again, Duration::from_millis(40), cx);
    }

    /// Frames the bracket at the caret and its partner, as IntelliJ does.
    fn frame_brackets(&mut self, at: usize, cx: &mut Context<Self>) {
        let found = crate::brackets::matching(&self.editor.read(cx).text().to_string(), at);
        let colour = palette::of(cx).muted;
        let marks: Vec<_> = found
            .into_iter()
            .flat_map(|(a, b)| [a, b])
            .map(|r| {
                RangeDecoration::new(r)
                    .with_style(RangeDecorationStyle::Frame)
                    .with_color(colour)
            })
            .collect();
        match &self.brackets {
            Some(b) => b.set(marks, cx),
            None if marks.is_empty() => {}
            None => {
                self.brackets = Some(
                    self.editor
                        .update(cx, |e, cx| e.create_range_decorations_collection(marks, cx)),
                )
            }
        }
    }

    /// IntelliJ's Next and Previous Highlighted Error (F2, ⇧F2): the caret
    /// put at the next problem in the file, round to the top, and what it
    /// says shown over it.
    fn go_to_problem(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_component::RopeExt;
        let e = self.editor.read(cx);
        let at = e.text().offset_to_position(e.cursor());
        let Some(d) = crate::problems::next_problem(&self.diagnostics, at, back).cloned() else {
            return;
        };
        let start = d.range.start;
        self.editor.update(cx, |e, cx| {
            e.set_cursor_position(
                Position {
                    line: start.line,
                    character: start.character,
                },
                window,
                cx,
            )
        });
        let put = self.editor.read(cx).cursor();
        self.problem_shown = Some((put, d));
        cx.notify();
    }

    /// The problem F2 went to, over the caret: a mark in its severity's
    /// colour and what the server says, first line in full.
    fn problem_note(&self, p: &palette::Palette, cx: &App) -> Option<gpui::AnyElement> {
        let (_, d) = self.problem_shown.as_ref()?;
        let e = self.editor.read(cx);
        let at = e.cursor();
        let caret = e.range_to_bounds(&(at..at))?;
        let colour = crate::problems::severity_colour(d.severity, p);
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(gpui::point(caret.left() - px(8.), caret.top() - px(3.)))
                    .anchor(gpui::Anchor::BottomLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        h_flex()
                            .debug_selector(|| "problem-note".into())
                            .max_w(px(560.))
                            .items_start()
                            .gap(px(6.))
                            .px(px(8.))
                            .py(px(5.))
                            .rounded(px(5.))
                            .border_1()
                            .border_color(p.border_strong)
                            .bg(p.surface)
                            .shadow_md()
                            .text_size(px(12.))
                            .child(div().w(px(3.)).h(px(14.)).rounded(px(1.5)).bg(colour))
                            .child(div().text_color(p.text).child(d.message.clone())),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    /// IntelliJ's occurrence commands: what each does to the selections
    /// (occur::Occurrences), done to the editor's.
    fn occurrence(
        &mut self,
        step: impl FnOnce(
            &mut crate::occur::Occurrences,
            &str,
            Range<usize>,
        ) -> Option<crate::occur::Step>,
        cx: &mut Context<Self>,
    ) {
        use crate::occur::Step;
        let e = self.editor.read(cx);
        let (text, active) = (e.text().to_string(), e.selected_range());
        let Some(step) = step(&mut self.occurrences, &text, active) else {
            return;
        };
        self.editor.update(cx, |e, cx| match step {
            Step::Select(r) => e.set_selected_range(r, cx),
            Step::Add(r) => e.add_selection(r, cx),
            Step::Set(all) => {
                let mut all = all.into_iter();
                if let Some(first) = all.next() {
                    e.set_selected_range(first, cx);
                }
                for r in all {
                    e.add_selection(r, cx);
                }
            }
        });
    }

    /// Shades the other uses of the symbol at the caret once it rests, as
    /// IntelliJ highlights usages of the element at the caret: from the
    /// server's textDocument/documentHighlight, where it is written in
    /// amber and read in the accent. The shading goes as the caret moves.
    fn ask_uses(&mut self, cx: &mut Context<Self>) {
        self.uses_asked += 1;
        let asked = self.uses_asked;
        if let Some(u) = &self.uses {
            u.clear(cx);
        }
        let Some(l) = self.languages.as_ref().and_then(|l| l.upgrade()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let call = this.update(cx, |d, cx| {
                use gpui_component::RopeExt;
                if d.uses_asked != asked || d.state != State::Open {
                    return None;
                }
                let e = d.editor.read(cx);
                let params = json!({
                    "textDocument": { "uri": crate::languages::file_uri(&d.asked) },
                    "position": e.text().offset_to_position(e.cursor()),
                });
                let abs = d.asked.clone();
                Some(l.update(cx, |l, cx| {
                    l.request(&abs, "textDocument/documentHighlight", params, cx)
                }))
            });
            let Ok(Some(call)) = call else { return };
            let found = call
                .await
                .ok()
                .and_then(|v| {
                    serde_json::from_value::<Option<Vec<lsp_types::DocumentHighlight>>>(v).ok()
                })
                .flatten()
                .unwrap_or_default();
            let _ = this.update(cx, |d, cx| {
                if d.uses_asked == asked {
                    d.shade_uses(found, cx);
                }
            });
        })
        .detach();
    }

    fn shade_uses(&mut self, found: Vec<lsp_types::DocumentHighlight>, cx: &mut Context<Self>) {
        use gpui_component::RopeExt;
        let p = palette::of(cx);
        let text = self.editor.read(cx).text().clone();
        let marks: Vec<_> = found
            .iter()
            .filter_map(|h| {
                let (a, b) = (
                    text.position_to_offset(&h.range.start),
                    text.position_to_offset(&h.range.end),
                );
                (a < b && b <= text.len()).then_some((a..b, h.kind))
            })
            .map(|(r, kind)| {
                let colour = if kind == Some(lsp_types::DocumentHighlightKind::WRITE) {
                    p.busy.opacity(0.22)
                } else {
                    p.accent.opacity(0.16)
                };
                RangeDecoration::new(r)
                    .with_style(RangeDecorationStyle::Fill)
                    .with_color(colour)
            })
            .collect();
        match &self.uses {
            Some(u) => u.set(marks, cx),
            None => {
                self.uses = Some(
                    self.editor
                        .update(cx, |e, cx| e.create_range_decorations_collection(marks, cx)),
                )
            }
        }
    }

    /// Asks the file's server for the signatures of the call at the caret,
    /// after a wait; only the last answer asked for is shown, and none
    /// takes them away.
    fn ask_signatures(
        &mut self,
        ask: crate::sighelp::Ask,
        after: Duration,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::RopeExt;
        self.signatures_asked += 1;
        let asked = self.signatures_asked;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(after).await;
            let call = this.update(cx, |d, cx| {
                if d.signatures_asked != asked {
                    return None;
                }
                let l = d.languages.as_ref()?.upgrade()?;
                let e = d.editor.read(cx);
                let at = e.cursor();
                d.sig_caret = Some(at);
                let params = json!({
                    "textDocument": { "uri": crate::languages::file_uri(&d.asked) },
                    "position": e.text().offset_to_position(at),
                    "context": ask.context(d.signatures.as_ref()),
                });
                let abs = d.asked.clone();
                Some(l.update(cx, |l, cx| {
                    l.request(&abs, "textDocument/signatureHelp", params, cx)
                }))
            });
            let Ok(Some(call)) = call else { return };
            let got = call
                .await
                .ok()
                .and_then(|v| serde_json::from_value::<Option<lsp_types::SignatureHelp>>(v).ok())
                .flatten()
                .filter(|h| !h.signatures.is_empty());
            let _ = this.update(cx, |d, cx| {
                if d.signatures_asked == asked {
                    d.signatures = got;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Asks the language server what it can do at the selection, as
    /// IntelliJ's Show Context Actions (⌥↩), and lists it at the caret.
    fn context_actions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(provider) = self
            .editor
            .read(cx)
            .lsp()
            .code_action_providers
            .first()
            .cloned()
        else {
            return;
        };
        let range = self.editor.read(cx).selected_range();
        let task = provider.code_actions(self.editor.clone(), range, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let actions = task.await.unwrap_or_default();
            let _ = this.update_in(cx, |d, window, cx| {
                d.list_context_actions(provider, actions, window, cx)
            });
        })
        .detach();
    }

    /// The actions as a menu under the caret: the fixes first, by a light
    /// bulb, then the rest, by a wrench; choosing one does it, saying in a
    /// notification what went wrong.
    fn list_context_actions(
        &mut self,
        provider: std::rc::Rc<dyn gpui_component::input::CodeActionProvider>,
        actions: Vec<lsp_types::CodeAction>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use gpui_component::Icon;
        use gpui_component::menu::PopupMenuItem;
        let at = self.editor.read(cx).cursor();
        self.context_at = self.editor.read(cx).range_to_bounds(&(at..at));
        let (editor, focus) = (self.editor.clone(), self.editor.focus_handle(cx));
        let menu = PopupMenu::build(window, cx, move |mut menu, _, cx| {
            crate::ui::modal(cx);
            menu = menu.action_context(focus).max_h(px(420.)).scrollable(true);
            if actions.is_empty() {
                return menu.item(PopupMenuItem::new("No actions here").disabled(true));
            }
            let fixes = actions.iter().filter(|a| crate::lspedit::is_fix(a)).count();
            for (i, a) in actions.into_iter().enumerate() {
                if i == fixes && i > 0 {
                    menu = menu.separator();
                }
                let icon = if i < fixes { "bulb" } else { "wrench" };
                let (provider, editor) = (provider.clone(), editor.clone());
                menu = menu.item(
                    PopupMenuItem::new(a.title.clone())
                        .icon(Icon::empty().path(crate::assets::icon(icon)))
                        .on_click(move |_, window, cx| {
                            let task = provider.perform_code_action(
                                editor.clone(),
                                a.clone(),
                                true,
                                window,
                                cx,
                            );
                            window
                                .spawn(cx, async move |cx| {
                                    if let Err(e) = task.await {
                                        let _ = cx.update(|window, cx| {
                                            use gpui_component::WindowExt;
                                            window.push_notification(
                                                gpui_component::notification::Notification::error(
                                                    e.to_string(),
                                                ),
                                                cx,
                                            );
                                        });
                                    }
                                })
                                .detach();
                        }),
                );
            }
            menu
        });
        window.focus(&menu.focus_handle(cx), cx);
        let sub = cx.subscribe(&menu, |d, _, _: &gpui::DismissEvent, cx| {
            d.context_menu = None;
            cx.notify();
        });
        self.context_menu = Some((menu, sub));
        self.context_unmarked = true;
        cx.notify();
    }

    /// Parameter Info as it is drawn: over the caret's line, each signature
    /// in the server's order, the one it takes the call for in full colour,
    /// the parameter the caret is at in bold, and what the server says of it
    /// under them.
    fn parameter_info(&self, p: &palette::Palette, cx: &App) -> Option<gpui::AnyElement> {
        use gpui::{FontWeight, HighlightStyle, StyledText};
        let help = self.signatures.as_ref()?;
        let e = self.editor.read(cx);
        let at = e.cursor();
        let caret = e.range_to_bounds(&(at..at))?;
        let mut body = gpui_component::v_flex().gap(px(2.));
        for line in crate::sighelp::lines(help).into_iter().take(8) {
            let colour = if line.active { p.text } else { p.muted };
            let mut text = StyledText::new(line.label);
            if let Some(r) = line.param {
                text = text.with_highlights([(
                    r,
                    HighlightStyle {
                        font_weight: Some(FontWeight::BOLD),
                        color: Some(p.text),
                        ..Default::default()
                    },
                )]);
            }
            body = body.child(div().text_color(colour).child(text));
        }
        let about = crate::sighelp::about(help);
        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(gpui::point(caret.left() - px(8.), caret.top() - px(3.)))
                    .anchor(gpui::Anchor::BottomLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .debug_selector(|| "parameter-info".into())
                            .max_w(px(640.))
                            .px(px(8.))
                            .py(px(5.))
                            .rounded(px(5.))
                            .border_1()
                            .border_color(p.border_strong)
                            .bg(p.surface)
                            .shadow_md()
                            .child(body.font_family(crate::diff::mono()).text_size(px(12.)))
                            .when_some(about, |d, a| {
                                d.child(
                                    div()
                                        .mt(px(4.))
                                        .pt(px(4.))
                                        .border_t_1()
                                        .border_color(p.border)
                                        .text_size(px(11.5))
                                        .text_color(p.muted)
                                        .child(a),
                                )
                            }),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    fn selection_moved(&mut self, cx: &mut Context<Self>) {
        if self.sending || self.me.is_none() || self.doc.is_none() || self.state != State::Open {
            return;
        }
        if self.selection(cx) == self.sent {
            return;
        }
        self.sending = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(80))
                .await;
            let _ = this.update(cx, |d, cx| {
                d.sending = false;
                d.send_presence(cx);
            });
        })
        .detach();
    }

    /// The selection as anchor and caret, in bytes.
    fn selection(&self, cx: &App) -> Option<(usize, usize)> {
        let e = self.editor.read(cx);
        let (range, head) = (e.selected_range(), e.cursor());
        let anchor = if head == range.end {
            range.start
        } else {
            range.end
        };
        Some((anchor, head))
    }

    fn send_presence(&mut self, cx: &mut Context<Self>) {
        let (Some(sync), Some(doc), Some((name, colour))) = (&self.sync, self.doc, &self.me) else {
            return;
        };
        if !sync.pending().is_empty() {
            return;
        }
        let Some((anchor, head)) = self.selection(cx) else {
            return;
        };
        let s = &self.shadow;
        let at = |b: usize| s.byte_to_utf16_idx(b.min(s.len()));
        let state = Presence {
            name: name.clone(),
            color: colour.clone(),
            ranges: vec![Sel {
                anchor: at(anchor),
                head: at(head),
            }],
        };
        let call = self.code.call(
            "doc.presence",
            json!({ "doc": doc, "version": sync.version(), "state": state }),
        );
        self.sent = Some((anchor, head));
        cx.background_executor().spawn(call).detach();
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
    fn typed(&mut self, e: &InputEditsEvent, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        let mut inserted = None;
        for (edit, after) in e.edits() {
            if self.echoes.take(after) {
                self.shadow = after.clone();
                continue;
            }
            inserted = Some(after.slice(edit.start_byte..edit.new_end_byte).to_string());
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
        if let Some(inserted) = inserted {
            // What was shaded may have moved; it is asked for again once the
            // caret rests.
            self.uses_caret = None;
            self.typed_for_signatures(&inserted, cx);
            self.completed(&inserted, window, cx);
        }
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
            Ok(Some(change)) => {
                self.apply(&change, false, window, cx);
                self.mark_edited(&change, cx);
            }
            Ok(None) => {}
            Err(e) => {
                self.state = State::Failed(format!("lost track of the document: {e}"));
                self.sync = None;
                cx.notify();
                return;
            }
        }
        for (client, (v, p)) in std::mem::take(&mut self.early) {
            self.presence(client, v, Some(p), cx);
        }
        self.push(cx);
        self.selection_moved(cx);
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

    /// Makes a change of the user's in one go, as Rename's edits to this
    /// file: put in the editor, sent, and undone with one Undo.
    pub fn apply_own(&mut self, change: ChangeSet, window: &mut Window, cx: &mut Context<Self>) {
        if !self.writable {
            return;
        }
        let Some(sync) = self.sync.as_mut() else {
            return;
        };
        sync.own(change.clone(), &self.shadow);
        self.apply(&change, true, window, cx);
        self.push(cx);
    }

    /// Runs one of the line commands on the text and selection, as one
    /// change of the user's: sent, and undone with one Undo.
    pub fn line_command(
        &mut self,
        f: impl FnOnce(&str, std::ops::Range<usize>) -> Option<crate::lineops::Change>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.writable || !matches!(self.state, State::Open | State::Reconnecting(_)) {
            return;
        }
        let (text, sel) = {
            let e = self.editor.read(cx);
            (e.text().clone(), e.selected_range())
        };
        let plain = text.to_string();
        let Some(c) = f(&plain, sel) else {
            return;
        };
        let at = |b: usize| text.byte_to_utf16_idx(b.min(text.len()));
        let change = Change {
            from: at(c.replace.start),
            to: at(c.replace.end),
            insert: Text::from(c.with.as_str()),
        };
        let Ok(change) = ChangeSet::of(vec![change], text.len_utf16()) else {
            return;
        };
        self.apply_own(change, window, cx);
        let range = c.selection.clone();
        self.editor
            .update(cx, |e, cx| e.set_selected_range(range, cx));
    }

    /// The file's line comment, by its language.
    fn comment(&self) -> Option<&'static str> {
        crate::lineops::comment_for(language(&self.path))
    }

    /// How a Markdown file is shown, if it is one.
    pub fn preview(&self) -> Option<Preview> {
        self.preview
    }

    pub fn set_preview(&mut self, p: Preview, cx: &mut Context<Self>) {
        if self.preview.is_some() {
            self.preview = Some(p);
            cx.notify();
        }
    }

    /// The editor's text and the caret's byte offset in it.
    pub fn text_and_caret(&self, cx: &App) -> (Rope, usize) {
        let e = self.editor.read(cx);
        (e.text().clone(), e.cursor())
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

impl DocEditor {
    /// A Markdown file's text beside its preview, or either alone, with the
    /// switch between them.
    fn with_preview(
        &self,
        mode: Preview,
        editor: Editor,
        p: &palette::Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let text = self.editor.read(cx).text().to_string();
        let modes = [
            ("Text", Preview::Text),
            ("Both", Preview::Both),
            ("Preview", Preview::Rendered),
        ];
        let switch = h_flex()
            .absolute()
            .bottom(px(8.))
            .left(px(14.))
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(6.))
            .bg(p.surface_2)
            .text_size(px(11.))
            .children(modes.map(|(label, m)| {
                div()
                    .id(label)
                    .px(px(8.))
                    .rounded(px(4.))
                    .cursor_pointer()
                    .when(m == mode, |d| d.bg(p.surface).text_color(p.text))
                    .when(m != mode, |d| d.text_color(p.muted))
                    .child(label)
                    .on_click(cx.listener(move |d, _, _, cx| d.set_preview(m, cx)))
            }));
        let rendered = div()
            .id("md-preview")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .px(px(20.))
            .py(px(14.))
            .bg(p.surface)
            .child(
                gpui_component::text::TextView::markdown("md-preview-text", text).selectable(true),
            );
        h_flex()
            .size_full()
            .relative()
            .when(mode != Preview::Rendered, |d| {
                d.child(div().flex_1().min_w_0().h_full().child(editor))
            })
            .when(mode == Preview::Both, |d| {
                d.child(div().w(px(1.)).h_full().bg(p.border))
            })
            .when(mode != Preview::Text, |d| d.child(rendered))
            .child(switch)
    }
}

impl gpui::EventEmitter<crate::blame::ShowCommit> for DocEditor {}

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
            State::Open if self.blame_note.is_some() => self.blame_note.clone(),
            State::Open if !self.writable => Some("Read-only".to_string()),
            State::Open => None,
        };
        if std::mem::take(&mut self.context_unmarked) {
            crate::ui::mark_first(window, cx);
        }
        let wrap = crate::settings::Settings::get(cx).soft_wrap;
        if self.wrapped != Some(wrap) {
            self.wrapped = Some(wrap);
            self.editor
                .update(cx, |e, cx| e.set_soft_wrap(wrap, window, cx));
        }
        use crate::lineops;
        div()
            .size_full()
            .relative()
            .key_context("DocEditor")
            .on_action(cx.listener(|d, _: &DuplicateLine, window, cx| {
                d.line_command(|t, s| Some(lineops::duplicate(t, s)), window, cx)
            }))
            .on_action(cx.listener(|d, _: &DeleteLine, window, cx| {
                d.line_command(|t, s| Some(lineops::delete_lines(t, s)), window, cx)
            }))
            .on_action(cx.listener(|d, _: &MoveLineUp, window, cx| {
                d.line_command(|t, s| lineops::move_lines(t, s, true), window, cx)
            }))
            .on_action(cx.listener(|d, _: &MoveLineDown, window, cx| {
                d.line_command(|t, s| lineops::move_lines(t, s, false), window, cx)
            }))
            .on_action(cx.listener(|d, _: &CommentLine, window, cx| {
                if let Some(prefix) = d.comment() {
                    d.line_command(
                        |t, s| Some(lineops::toggle_comment(t, s, prefix)),
                        window,
                        cx,
                    )
                }
            }))
            .on_action(cx.listener(|d, _: &JoinLines, window, cx| {
                d.line_command(lineops::join_lines, window, cx)
            }))
            .on_action(cx.listener(|_, _: &ToggleSoftWrap, _, cx| {
                crate::settings::Settings::update(cx, |s| s.soft_wrap = !s.soft_wrap)
            }))
            .on_action(cx.listener(|d, _: &ToggleBlame, _, cx| {
                let on = d.blame.is_none();
                d.set_blame(on, cx)
            }))
            .on_action(cx.listener(|d, _: &ParameterInfo, _, cx| {
                d.ask_signatures(crate::sighelp::Ask::Invoked, Duration::ZERO, cx)
            }))
            .on_action(
                cx.listener(|d, _: &ContextActions, window, cx| d.context_actions(window, cx)),
            )
            .on_action(
                cx.listener(|d, _: &NextProblem, window, cx| d.go_to_problem(false, window, cx)),
            )
            .on_action(
                cx.listener(|d, _: &PreviousProblem, window, cx| d.go_to_problem(true, window, cx)),
            )
            .on_action(cx.listener(|d, _: &SelectNextOccurrence, _, cx| {
                d.occurrence(|o, text, active| o.next(text, active), cx)
            }))
            .on_action(cx.listener(|d, _: &UnselectOccurrence, _, cx| {
                d.occurrence(|o, _, active| o.unselect(active), cx)
            }))
            .on_action(cx.listener(|d, _: &SelectAllOccurrences, _, cx| {
                d.occurrence(|o, text, active| o.all(text, active), cx)
            }))
            // Escape takes Parameter Info away, and does what it does in the
            // editor besides.
            .capture_key_down(cx.listener(|d, e: &gpui::KeyDownEvent, _, cx| {
                if e.keystroke.key == "escape" {
                    if d.signatures.is_some() {
                        d.signatures = None;
                        d.signatures_asked += 1;
                        cx.notify();
                    }
                    if d.problem_shown.take().is_some() {
                        cx.notify();
                    }
                }
            }))
            .capture_action(cx.listener(|d, _: &Undo, window, cx| {
                cx.stop_propagation();
                d.undo(window, cx);
            }))
            .capture_action(cx.listener(|d, _: &Redo, window, cx| {
                cx.stop_propagation();
                d.redo(window, cx);
            }))
            .child({
                let comment = self.comment().is_some();
                let blamed = self.blame.is_some();
                let editor = code_editor(&self.editor)
                    .readonly(
                        !self.writable
                            || !matches!(self.state, State::Open | State::Reconnecting(_)),
                    )
                    .context_menu(move |menu, _, cx| editor_menu(menu, comment, wrap, blamed, cx));
                match self.preview {
                    None if blamed => h_flex()
                        .size_full()
                        .child(self.blame_column(&p, cx))
                        .child(div().flex_1().min_w_0().h_full().child(editor))
                        .into_any_element(),
                    None => div().size_full().child(editor).into_any_element(),
                    Some(mode) => self.with_preview(mode, editor, &p, cx).into_any_element(),
                }
            })
            .when(!self.remotes.is_empty(), |d| {
                // Who else has the file open, in the colours their carets
                // are drawn in.
                d.child(
                    h_flex()
                        .absolute()
                        .bottom(px(6.))
                        .right(px(14.))
                        .gap(px(4.))
                        .children(self.others().into_iter().map(|(name, colour)| {
                            div()
                                .px(px(6.))
                                .rounded(px(4.))
                                .bg(colour)
                                .text_size(px(10.5))
                                .text_color(gpui::white())
                                .child(name)
                        })),
                )
            })
            .children(self.parameter_info(&p, cx))
            .children(self.problem_note(&p, cx))
            .when_some(self.context_menu.as_ref(), |d, (menu, _)| {
                let at = self.context_at.map(|b| b.bottom_left());
                d.child(
                    gpui::deferred(
                        gpui::anchored()
                            .when_some(at, |a, at| a.position(at))
                            .snap_to_window_with_margin(px(8.))
                            .child(menu.clone()),
                    )
                    .with_priority(100),
                )
            })
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
