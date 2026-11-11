//! An environment's tab, laid out as IntelliJ is: the environment's
//! toolbar, tool windows docked from stripes on either side, the editors
//! in the middle, and a status bar. Drawn to the design's environment tab.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    App, Context, DragMoveEvent, Entity, FocusHandle, Focusable, FontWeight, IntoElement,
    KeyBinding, ParentElement, Render, SharedString, Styled, Window, actions, div, prelude::*, px,
    relative, svg,
};
use gpui_component::Sizable;
use gpui_component::button::ButtonVariants;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::menu::{ContextMenuExt, DropdownMenu, PopupMenuItem};
use gpui_component::{h_flex, v_flex};

use crate::api::{AgentSession, Client, Environment, TerminalSession};
use crate::assets;
use crate::commit::CommitView;
use crate::diff::{DiffView, ShowDiff, Spec};
use crate::docedit::{self, DocEditor};
use crate::docker::{DockerView, RunInTerminal};
use crate::envlogs::LogsView;
use crate::find::{FindView, OpenAt, Search};
use crate::findpop::{Done, FindPopup};
use crate::gitlog::GitView;
use crate::languages::{self, Languages};
use crate::layout::{self, Edge, Layout, Sizes};
use crate::lspedit;
use crate::model::{self, Card, Tone};
use crate::palette::{self, Palette};
use crate::place::PlaceConfig;
use crate::places::Places;
use crate::ports::PortsView;
use crate::problems::ProblemsView;
use crate::project::{OpenFile, ProjectView};
use crate::termview::{self, TerminalView};
use crate::tools::Stripes;
use crate::ui::{button, dot, primary, spark, tone};
use crate::workspace::Workspace;

pub use crate::tools::{Group, Slot, Tool};

impl Tool {
    /// Whether it shows what the environment's own services say, and so
    /// nothing while it is not running.
    fn needs_running(self) -> bool {
        matches!(
            self,
            Tool::Git | Tool::Find | Tool::Problems | Tool::Docker | Tool::Ports
        )
    }

    /// What it will hold, while the app cannot reach the environment's own
    /// services.
    fn waiting(self) -> &'static str {
        match self {
            Tool::Project => "The environment's projects and their files.",
            Tool::Filesystem => "The machine's files, from /.",
            Tool::Commit => "What has changed, to commit and push.",
            Tool::Structure => "The open file's outline.",
            Tool::Terminal => "The environment's terminal sessions, Claude's among them.",
            Tool::Problems => "What the language servers report.",
            Tool::Git => "Branches, the commit graph and each commit's changes.",
            Tool::Docker => "The environment's containers, their logs and images.",
            Tool::Find => "Matches across the projects.",
            Tool::Log => "The environment's console and its services' logs.",
            Tool::Ports => "The ports the environment listens on, each with its address.",
            Tool::Agents => "",
        }
    }
}

/// A stripe's icon being dragged, and what is drawn under the pointer.
#[derive(Clone)]
pub struct DraggedTool(pub Tool);

/// An editor tab being dragged: its group, its place there, and its name.
#[derive(Clone)]
pub struct DraggedTab {
    right: bool,
    i: usize,
    title: String,
}

impl Render for DraggedTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        div()
            .px(px(12.))
            .py(px(5.))
            .rounded(px(4.))
            .bg(p.surface)
            .border_1()
            .border_color(p.border_strong)
            .shadow_md()
            .text_size(px(11.5))
            .text_color(p.text)
            .child(self.title.clone())
    }
}

impl Render for DraggedTool {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        div()
            .size(px(28.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .bg(p.surface)
            .border_1()
            .border_color(p.border_strong)
            .shadow_md()
            .child(
                svg()
                    .path(assets::icon(self.0.icon()))
                    .size(px(16.))
                    .text_color(p.accent),
            )
    }
}

actions!(
    env_tab,
    [
        ToggleProject,
        ToggleCommit,
        ToggleStructure,
        ToggleTerminal,
        ToggleProblems,
        ToggleGit,
        ToggleDocker,
        FindInFiles,
        ReplaceInFiles,
        CloseTab,
        ReopenClosedTab,
        GoToLine,
        FindUsages,
        RenameSymbol,
        GoToImplementation,
        GoToTypeDeclaration,
        HideAllToolWindows,
        HideActiveToolWindow
    ]
);

/// Binds the tool windows' keys: IntelliJ's, Command on macOS and Alt
/// elsewhere.
pub fn bind_keys(cx: &mut App) {
    let ctx = Some("EnvTab");
    #[cfg(target_os = "macos")]
    let keys = [
        "cmd-1",
        "cmd-0",
        "cmd-7",
        "alt-f12",
        "cmd-6",
        "cmd-9",
        "cmd-8",
        "cmd-shift-f12",
    ];
    #[cfg(not(target_os = "macos"))]
    let keys = [
        "alt-1",
        "alt-0",
        "alt-7",
        "alt-f12",
        "alt-6",
        "alt-9",
        "alt-8",
        "ctrl-shift-f12",
    ];
    cx.bind_keys([
        KeyBinding::new(keys[0], ToggleProject, ctx),
        KeyBinding::new(keys[1], ToggleCommit, ctx),
        KeyBinding::new(keys[2], ToggleStructure, ctx),
        KeyBinding::new(keys[3], ToggleTerminal, ctx),
        KeyBinding::new(keys[4], ToggleProblems, ctx),
        KeyBinding::new(keys[5], ToggleGit, ctx),
        KeyBinding::new(keys[6], ToggleDocker, ctx),
        KeyBinding::new(keys[7], HideAllToolWindows, ctx),
        KeyBinding::new("shift-escape", HideActiveToolWindow, ctx),
    ]);
    // Find and replace as IntelliJ has them, in a file and in files. The
    // text fields' own context (gpui-component's "Input", the editor's
    // among them) binds Shift-Cmd-F to replacing in the field; bound here
    // after it, these take its place there too.
    let mac = cfg!(target_os = "macos");
    let (find_files, replace_files, replace) = if mac {
        ("cmd-shift-f", "cmd-shift-r", "cmd-r")
    } else {
        ("ctrl-shift-f", "ctrl-shift-r", "ctrl-r")
    };
    let input = Some("Input");
    let (close, reopen) = if mac {
        ("cmd-w", "cmd-shift-t")
    } else {
        ("ctrl-f4", "ctrl-shift-t")
    };
    let go_to_line = if mac { "cmd-l" } else { "ctrl-g" };
    let (implementation, type_declaration) = if mac {
        ("cmd-alt-b", "cmd-shift-b")
    } else {
        ("ctrl-alt-b", "ctrl-shift-b")
    };
    cx.bind_keys([
        KeyBinding::new(go_to_line, GoToLine, ctx),
        KeyBinding::new("alt-f7", FindUsages, ctx),
        KeyBinding::new("alt-f7", FindUsages, input),
        KeyBinding::new("shift-f6", RenameSymbol, ctx),
        KeyBinding::new("shift-f6", RenameSymbol, input),
        KeyBinding::new(implementation, GoToImplementation, ctx),
        KeyBinding::new(implementation, GoToImplementation, input),
        KeyBinding::new(type_declaration, GoToTypeDeclaration, ctx),
        KeyBinding::new(type_declaration, GoToTypeDeclaration, input),
        KeyBinding::new(go_to_line, GoToLine, input),
        KeyBinding::new(close, CloseTab, ctx),
        KeyBinding::new(reopen, ReopenClosedTab, ctx),
        KeyBinding::new(find_files, FindInFiles, ctx),
        KeyBinding::new(find_files, FindInFiles, input),
        KeyBinding::new(replace_files, ReplaceInFiles, ctx),
        KeyBinding::new(replace_files, ReplaceInFiles, input),
        KeyBinding::new(replace, gpui_component::input::Replace, input),
    ]);
}

/// A terminal tab's menu: a new session; closing this tab or the others,
/// their sessions left running; ending this one's.
fn terminal_tab_menu(
    menu: gpui_component::menu::PopupMenu,
    view: gpui::WeakEntity<EnvTab>,
    i: usize,
    only: bool,
) -> gpui_component::menu::PopupMenu {
    let on = |f: fn(&mut EnvTab, usize, &mut Window, &mut Context<EnvTab>)| {
        let view = view.clone();
        move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |t, cx| f(t, i, window, cx));
        }
    };
    menu.item(
        PopupMenuItem::new("New Session")
            .on_click(on(|t, _, window, cx| t.new_terminal(window, cx))),
    )
    .separator()
    .item(PopupMenuItem::new("Close Tab").on_click(on(|t, i, _, cx| t.close_terminal_tab(i, cx))))
    .item(
        PopupMenuItem::new("Close Other Tabs")
            .disabled(only)
            .on_click(on(|t, i, _, cx| t.close_other_terminal_tabs(i, cx))),
    )
    .item(
        PopupMenuItem::new("End Session…")
            .on_click(on(|t, i, window, cx| t.end_terminal(i, window, cx))),
    )
    .separator()
    .item(
        PopupMenuItem::new("Move to Editor")
            .on_click(on(|t, i, window, cx| t.terminal_to_editor(i, window, cx))),
    )
}

fn shortcut(t: Tool) -> Option<String> {
    let m = if cfg!(target_os = "macos") {
        "⌘"
    } else {
        "Alt+"
    };
    Some(match t {
        Tool::Project => format!("{m}1"),
        Tool::Commit => format!("{m}0"),
        Tool::Structure => format!("{m}7"),
        Tool::Terminal => if cfg!(target_os = "macos") {
            "⌥F12"
        } else {
            "Alt+F12"
        }
        .to_string(),
        Tool::Problems => format!("{m}6"),
        Tool::Git => format!("{m}9"),
        Tool::Docker => format!("{m}8"),
        Tool::Find => if cfg!(target_os = "macos") {
            "⌘⇧F"
        } else {
            "Ctrl+Shift+F"
        }
        .to_string(),
        _ => return None,
    })
}

pub struct EnvTab {
    places: Entity<Places>,
    place: String,
    env: String,
    focus: FocusHandle,
    /// The tool window open in each slot.
    open: HashMap<Slot, Tool>,
    /// The task, while it is being edited.
    task_edit: Option<Entity<InputState>>,
    /// The tool windows Hide All put away, to bring back.
    hidden: Option<HashMap<Slot, Tool>>,
    /// How long the way to the environment and back last took, in
    /// milliseconds, and what times it.
    latency: Option<f64>,
    pinger: Option<gpui::Task<()>>,
    /// The editor tabs' sideways scroll, and the tab shown and the number
    /// of tabs when it was last scrolled into view.
    tab_scroll: gpui::ScrollHandle,
    tab_in_view: (Option<usize>, usize),
    /// Whether a stripe's icon is being dragged, which shows the right
    /// stripe even with nothing on it, to drop onto.
    dragging: bool,
    /// The tool window last opened, which Hide Active hides.
    last: Option<Tool>,
    /// The Terminal window's sessions.
    terms: Terms,
    /// The environment's projects and their Code connections, made when
    /// a window showing them is first shown on a running environment.
    workspace: Option<Entity<Workspace>>,
    /// The branch widget in the toolbar, with the workspace.
    branches: Option<Entity<crate::branches::BranchWidget>>,
    /// Whether the Commit window's message is to take the keys once the
    /// window is shown.
    focus_commit: bool,
    /// The Project, Commit and Git windows, made when each is first shown.
    project: Option<Entity<ProjectView>>,
    /// The Filesystem window's tree: the machine's files.
    filesystem: Option<Entity<ProjectView>>,
    commit: Option<Entity<CommitView>>,
    git: Option<Entity<GitView>>,
    docker: Option<Entity<DockerView>>,
    logs: Option<Entity<LogsView>>,
    ports: Option<Entity<PortsView>>,
    find: Option<Entity<FindView>>,
    structure: Option<Entity<crate::structure::StructureView>>,
    /// Find in Files' pop-up while it is open.
    find_popup: Option<Entity<FindPopup>>,
    find_sub: Option<gpui::Subscription>,
    /// A search to list in the Find window once it is made.
    pending_find: Option<Search>,
    /// A file's history asked for before the Git window was made.
    pending_history: Option<crate::project::ShowHistory>,
    /// A commit asked for before the Git window was made.
    pending_commit: Option<String>,
    /// Edits a language server asked for, made once a window is to hand.
    pending_edits: Vec<lsp_types::WorkspaceEdit>,
    /// What language servers had to tell, shown once a window is to hand.
    pending_messages: Vec<(u8, String)>,
    /// A language server's usages to list in the Find window once it is
    /// made: what of, and each project's lines.
    pending_usages: Option<(String, crate::find::Lines)>,
    /// The search Find in Files last made, which it opens on.
    last_search: Option<Search>,
    problems: Option<Entity<ProblemsView>>,
    /// The language servers, made with the workspace.
    languages: Option<Entity<Languages>>,
    /// The files and diffs open in the editor group in use, and the one
    /// shown; its row of tabs is tab_scroll.
    files: Vec<Tab>,
    file: Option<usize>,
    /// The editors' other group, split beside the one in use, and whether
    /// it is on the right. Using it swaps the two.
    other: Option<EditorGroup>,
    other_right: bool,
    /// Whether the two groups are one over the other (Split Down) rather
    /// than side by side; the second is then the lower.
    split_down: bool,
    /// The tabs pinned, which stay first in their group and are left by
    /// Close Others and Close All but Pinned.
    pinned: std::collections::HashSet<gpui::EntityId>,
    /// Whether an editor tab is being dragged, for the drop zone that
    /// splits.
    dragging_tab: bool,
    /// The files closed, last last, to open again: each with the root it
    /// was opened from.
    closed: Vec<(String, String)>,
    /// A file to show in the Project window once it is made.
    pending_reveal: Option<String>,
    subs: Vec<gpui::Subscription>,
}

/// Where the caret of the file shown is, for its language server: the
/// server, the file, the word there, and the params naming the place.
struct Caret {
    langs: Entity<Languages>,
    abs: String,
    word: String,
    params: serde_json::Value,
}

/// An editor tab: a file, or a file's diff.
/// A tab, for its menu: its place in its group, how many the group has,
/// whether the group is the right one, and whether there are two.
#[derive(Clone, Copy)]
struct TabAt {
    i: usize,
    count: usize,
    right: bool,
    split: bool,
    pinned: bool,
}

/// An editor group, as it is kept while the other is in use.
struct EditorGroup {
    files: Vec<Tab>,
    file: Option<usize>,
    tab_scroll: gpui::ScrollHandle,
    tab_in_view: (Option<usize>, usize),
}

impl EditorGroup {
    fn empty() -> Self {
        EditorGroup {
            files: vec![],
            file: None,
            tab_scroll: gpui::ScrollHandle::new(),
            tab_in_view: (None, 0),
        }
    }
}

#[derive(Clone)]
enum Tab {
    Doc(Entity<DocEditor>),
    Diff(Entity<DiffView>),
    /// A terminal session moved into the editor area.
    Term(Entity<TerminalView>),
    /// A file shown other than as text: a picture, its bytes, or a PDF or
    /// medium this computer's app opens.
    View(Entity<crate::viewer::FileViewer>),
}

impl Tab {
    /// The file's absolute path.
    fn path(&self, cx: &App) -> String {
        match self {
            Tab::Doc(d) => d.read(cx).path().to_string(),
            Tab::Diff(d) => d.read(cx).spec().abs(),
            Tab::Term(_) => String::new(),
            Tab::View(v) => v.read(cx).path().to_string(),
        }
    }

    fn title(&self, cx: &App) -> String {
        match self {
            Tab::Doc(d) => {
                let path = d.read(cx).path();
                path.rsplit('/').next().unwrap_or(path).to_string()
            }
            Tab::Diff(d) => d.read(cx).title(),
            Tab::Term(t) => {
                let title = t.read(cx).title();
                if title.is_empty() {
                    "Terminal".into()
                } else {
                    title.to_string()
                }
            }
            Tab::View(v) => {
                let path = v.read(cx).path();
                path.rsplit('/').next().unwrap_or(path).to_string()
            }
        }
    }

    /// Whether this shows what another tab at path does: the same file, or
    /// the same file's diff, or the very same terminal.
    fn twin_of(&self, other: &Tab, path: &str, cx: &App) -> bool {
        match (self, other) {
            (Tab::Term(a), Tab::Term(b)) => a == b,
            (Tab::Term(_), _) | (_, Tab::Term(_)) => false,
            (Tab::View(a), Tab::View(b)) => {
                let (a, b) = (a.read(cx), b.read(cx));
                a.path() == b.path() && a.view() == b.view()
            }
            (Tab::View(_), _) | (_, Tab::View(_)) => false,
            (a, b) => a.path(cx) == path && matches!(a, Tab::Doc(_)) == matches!(b, Tab::Doc(_)),
        }
    }

    /// What the tab shows, as pinning knows it.
    fn entity_id(&self) -> gpui::EntityId {
        match self {
            Tab::Doc(d) => d.entity_id(),
            Tab::Diff(d) => d.entity_id(),
            Tab::Term(t) => t.entity_id(),
            Tab::View(v) => v.entity_id(),
        }
    }

    fn focus_handle(&self, cx: &App) -> gpui::FocusHandle {
        match self {
            Tab::Doc(d) => d.focus_handle(cx),
            Tab::Diff(d) => d.focus_handle(cx),
            Tab::Term(t) => t.focus_handle(cx),
            Tab::View(v) => v.focus_handle(cx),
        }
    }

    fn element(&self) -> gpui::AnyElement {
        match self {
            Tab::Doc(d) => d.clone().into_any_element(),
            Tab::Diff(d) => d.clone().into_any_element(),
            Tab::Term(t) => t.clone().into_any_element(),
            Tab::View(v) => v.clone().into_any_element(),
        }
    }
}

/// The Terminal tool window's tabs: one per session, attached when first
/// shown.
#[derive(Default)]
struct Terms {
    /// Whether the environment's sessions have been asked for.
    listed: bool,
    tabs: Vec<TermTab>,
    active: Option<usize>,
    /// The sessions not open here, while the menu listing them is open.
    menu: Option<Vec<TerminalSession>>,
}

struct TermTab {
    /// The session's ID, until a new session's is known.
    session: Option<String>,
    title: String,
    view: Option<Entity<TerminalView>>,
}

impl TermTab {
    fn id(&self, cx: &App) -> Option<String> {
        self.session.clone().or_else(|| {
            self.view
                .as_ref()
                .and_then(|v| v.read(cx).session().map(str::to_string))
        })
    }

    fn label(&self, cx: &App) -> String {
        let title = self
            .view
            .as_ref()
            .map(|v| v.read(cx).title().to_string())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| self.title.clone());
        if title.is_empty() {
            "Terminal".into()
        } else {
            title
        }
    }
}

impl EnvTab {
    pub fn new(places: Entity<Places>, place: &str, env: &str, cx: &mut Context<Self>) -> Self {
        cx.observe(&places, |_, _, cx| cx.notify()).detach();
        let open = HashMap::from([
            (Slot::Left, Tool::Project),
            (Slot::Bottom, Tool::Terminal),
            (Slot::Right, Tool::Agents),
        ]);
        EnvTab {
            places,
            place: place.into(),
            env: env.into(),
            focus: cx.focus_handle(),
            open,
            task_edit: None,
            hidden: None,
            latency: None,
            pinger: None,
            tab_scroll: gpui::ScrollHandle::new(),
            tab_in_view: (None, 0),
            dragging: false,
            last: None,
            terms: Terms::default(),
            workspace: None,
            branches: None,
            focus_commit: false,
            project: None,
            filesystem: None,
            commit: None,
            git: None,
            docker: None,
            logs: None,
            ports: None,
            find: None,
            structure: None,
            find_popup: None,
            find_sub: None,
            pending_find: None,
            pending_history: None,
            pending_commit: None,
            pending_edits: Vec::new(),
            pending_messages: Vec::new(),
            pending_usages: None,
            last_search: None,
            problems: None,
            languages: None,
            files: vec![],
            file: None,
            closed: vec![],
            other: None,
            other_right: true,
            split_down: false,
            pinned: Default::default(),
            dragging_tab: false,
            pending_reveal: None,
            subs: vec![],
        }
    }

    /// Shows the Commit window, its message taking the keys, as the branch
    /// widget's Commit… does.
    pub fn open_commit(&mut self, cx: &mut Context<Self>) {
        if !self.is_open(Tool::Commit) {
            self.toggle(Tool::Commit, cx);
        }
        self.focus_commit = true;
        cx.notify();
    }

    pub fn branch_widget(&self) -> Option<Entity<crate::branches::BranchWidget>> {
        self.branches.clone()
    }

    pub fn is_open(&self, tool: Tool) -> bool {
        self.open.values().any(|t| *t == tool)
    }

    /// Opens a tool window in its slot, or closes it if it is open there.
    pub fn toggle(&mut self, tool: Tool, cx: &mut Context<Self>) {
        let slot = Layout::slot(tool, cx);
        if self.open.get(&slot) == Some(&tool) {
            self.open.remove(&slot);
        } else {
            self.open.insert(slot, tool);
            self.last = Some(tool);
            self.hidden = None;
        }
        cx.notify();
    }

    /// Puts every tool window away, leaving the editors the room, or brings
    /// back what it put away.
    pub fn hide_all(&mut self, cx: &mut Context<Self>) {
        if self.open.is_empty() {
            if let Some(h) = self.hidden.take() {
                self.open = h;
            }
        } else {
            self.hidden = Some(std::mem::take(&mut self.open));
        }
        cx.notify();
    }

    /// Moves a tool's icon into a stripe's group, before another of its
    /// tools or at its end; open, the tool window moves with it.
    pub fn move_tool(&mut self, t: Tool, to: Group, before: Option<Tool>, cx: &mut Context<Self>) {
        let was_open = self.is_open(t);
        Layout::arrange(cx, |s| {
            s.moved(t, to, before);
        });
        if was_open {
            self.open.retain(|_, x| *x != t);
            self.open.insert(to.slot(), t);
        }
        self.dragging = false;
        cx.notify();
    }

    /// Keeps each open tool window in the slot its stripe gives it, as a
    /// tool moved in another tab moves here too: one whose slot changed
    /// moves there, unless another is open there.
    fn settle(&mut self, stripes: &Stripes) {
        let fix = |open: &mut HashMap<Slot, Tool>| {
            let moved: Vec<(Slot, Tool)> = open
                .iter()
                .filter(|(s, t)| stripes.slot(**t) != **s)
                .map(|(s, t)| (*s, *t))
                .collect();
            for (s, t) in moved {
                open.remove(&s);
                open.entry(stripes.slot(t)).or_insert(t);
            }
        };
        fix(&mut self.open);
        if let Some(h) = self.hidden.as_mut() {
            fix(h);
        }
    }

    /// Hides the tool window last opened, if it is still open.
    pub fn hide_active(&mut self, cx: &mut Context<Self>) {
        if let Some(t) = self.last.take()
            && self.is_open(t)
        {
            self.open.retain(|_, x| *x != t);
            cx.notify();
        }
    }

    fn client(&self, cx: &App) -> Option<Client> {
        self.places.read(cx).client(&self.place)
    }

    /// Asks for the environment's sessions, once, opening a tab for each;
    /// with none, a new one starts.
    fn list_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terms.listed {
            return;
        }
        let Some(client) = self.client(cx) else {
            return;
        };
        self.terms.listed = true;
        let env = self.env.clone();
        let task = cx.background_spawn(async move { client.terminals(&env) });
        cx.spawn_in(window, async move |this, cx| {
            let list = task.await;
            let _ = this.update_in(cx, |t, window, cx| match list {
                Ok(list) if !list.is_empty() => {
                    for s in list {
                        // A session started here may be listed once it
                        // has attached.
                        if !t
                            .terms
                            .tabs
                            .iter()
                            .any(|x| x.id(cx).as_deref() == Some(s.id.as_str()))
                        {
                            t.terms.tabs.push(TermTab {
                                session: Some(s.id),
                                title: s.title,
                                view: None,
                            });
                        }
                    }
                    if t.terms.active.is_none() {
                        t.select_terminal(0, false, window, cx);
                    }
                    cx.notify();
                }
                Ok(_) if t.terms.tabs.is_empty() => t.new_terminal(window, cx),
                Ok(_) => {}
                Err(_) => t.terms.listed = false,
            });
        })
        .detach();
    }

    /// Starts a new session in a tab of its own.
    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_session(None, None, window, cx)
    }

    /// Starts a new session in a tab of its own, in the Terminal window,
    /// in a folder or the environment's own, typing a command into it once
    /// it has started.
    pub fn new_session(
        &mut self,
        dir: Option<String>,
        run: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client(cx) else {
            return;
        };
        self.open
            .insert(Layout::slot(Tool::Terminal, cx), Tool::Terminal);
        let env = self.env.clone();
        let view = cx.new(|cx| {
            let mut v = match &dir {
                Some(dir) => TerminalView::starting_in(&client, &env, dir, cx),
                None => TerminalView::new(&client, &env, None, cx),
            };
            if let Some(run) = run {
                v.run_once_started(run);
            }
            v
        });
        cx.observe(&view, |_, _, cx| cx.notify()).detach();
        self.terms.tabs.push(TermTab {
            session: None,
            title: String::new(),
            view: Some(view),
        });
        self.terms.menu = None;
        self.select_terminal(self.terms.tabs.len() - 1, true, window, cx);
    }

    /// Shows a session's tab, attaching to it the first time, and gives it
    /// the keyboard if focus says so.
    fn select_terminal(
        &mut self,
        i: usize,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client(cx) else {
            return;
        };
        let Some(tab) = self.terms.tabs.get_mut(i) else {
            return;
        };
        if tab.view.is_none() {
            let (env, session) = (self.env.clone(), tab.session.clone());
            let view = cx.new(|cx| TerminalView::new(&client, &env, session.as_deref(), cx));
            cx.observe(&view, |_, _, cx| cx.notify()).detach();
            tab.view = Some(view);
        }
        self.terms.active = Some(i);
        if focus && let Some(v) = &tab.view {
            window.focus(&v.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Opens a session's tab in the Terminal window, adding it if it is not
    /// open here.
    pub fn open_terminal(&mut self, session: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open
            .insert(Layout::slot(Tool::Terminal, cx), Tool::Terminal);
        self.terms.listed = true;
        let i = match self
            .terms
            .tabs
            .iter()
            .position(|t| t.id(cx).as_deref() == Some(session))
        {
            Some(i) => i,
            None => {
                self.terms.tabs.push(TermTab {
                    session: Some(session.into()),
                    title: String::new(),
                    view: None,
                });
                self.terms.tabs.len() - 1
            }
        };
        self.terms.menu = None;
        self.select_terminal(i, true, window, cx);
    }

    /// Answers an asking agent session as pressing key in its terminal
    /// would: Claude Code's choices are numbered.
    pub fn answer(&mut self, agent: &str, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let places = self.places.read(cx);
        let session = places
            .get(&self.place)
            .and_then(|(_, s)| s.environment(&self.env))
            .and_then(|e| e.agents.iter().find(|a| a.id == agent))
            .and_then(|a| a.terminal.clone());
        if let Some(session) = session {
            self.type_into(&session, key, window, cx);
        }
    }

    /// Opens a file in the editors, or shows it if it is open: its path
    /// from root, a project's or /.
    pub fn open_file(
        &mut self,
        root: &str,
        path: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let abs = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("{}/{path}", root.trim_end_matches('/'))
        };
        let i = match self.files.iter().position(|f| match f {
            Tab::Doc(f) => {
                let f = f.read(cx);
                f.path() == abs || f.asked() == abs
            }
            Tab::View(v) => v.read(cx).path() == abs,
            Tab::Diff(_) | Tab::Term(_) => false,
        }) {
            Some(i) => i,
            None => {
                let tab = match crate::viewer::opens_as(&abs) {
                    crate::viewer::View::Text => match self.new_doc(root, &abs, window, cx) {
                        Some(view) => Tab::Doc(view),
                        None => return,
                    },
                    way => match self.new_viewer(&abs, way, cx) {
                        Some(v) => Tab::View(v),
                        None => return,
                    },
                };
                self.files.push(tab);
                self.files.len() - 1
            }
        };
        self.show_file(i, window, cx);
    }

    /// Opens a file in a way chosen, as Open With does: as text, or in a
    /// viewer, a tab already showing it so used again.
    pub fn open_with(
        &mut self,
        root: &str,
        path: &str,
        way: crate::viewer::View,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::viewer::View;
        let abs = if path.starts_with('/') {
            path.to_string()
        } else {
            format!("{}/{path}", root.trim_end_matches('/'))
        };
        let at = self.files.iter().position(|f| match (f, way) {
            (Tab::Doc(f), View::Text) => f.read(cx).path() == abs,
            (Tab::View(v), way) => {
                let v = v.read(cx);
                v.path() == abs && v.view() == way
            }
            _ => false,
        });
        let i = match at {
            Some(i) => i,
            None => {
                let tab = match way {
                    View::Text => match self.new_doc(root, &abs, window, cx) {
                        Some(view) => Tab::Doc(view),
                        None => return,
                    },
                    way => match self.new_viewer(&abs, way, cx) {
                        Some(v) => Tab::View(v),
                        None => return,
                    },
                };
                self.files.push(tab);
                self.files.len() - 1
            }
        };
        self.show_file(i, window, cx);
    }

    /// A viewer on a file, through the connection that reaches it.
    fn new_viewer(
        &mut self,
        abs: &str,
        way: crate::viewer::View,
        cx: &mut Context<Self>,
    ) -> Option<Entity<crate::viewer::FileViewer>> {
        let code = self.code_for(abs, cx)?;
        let view = cx.new(|cx| crate::viewer::FileViewer::new(code, abs, way, cx));
        self.subs.push(cx.observe(&view, |_, _, cx| cx.notify()));
        Some(view)
    }

    /// Shows a file opened as text, found not to be, as its bytes in the
    /// same place.
    fn show_as_bytes(&mut self, doc: &Entity<DocEditor>, cx: &mut Context<Self>) {
        let Some(i) = self
            .files
            .iter()
            .position(|f| matches!(f, Tab::Doc(d) if d == doc))
        else {
            return;
        };
        let abs = doc.read(cx).path().to_string();
        let Some(view) = self.new_viewer(&abs, crate::viewer::View::Hex, cx) else {
            return;
        };
        let was = std::mem::replace(&mut self.files[i], Tab::View(view));
        self.drop_tab(was, cx);
        cx.notify();
    }

    /// An editor on a file, opened in its language server.
    fn new_doc(
        &mut self,
        root: &str,
        abs: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<DocEditor>> {
        let client = self.client(cx)?;
        let env = self.env.clone();
        // Shown to the others by the place's name for this user, in the
        // colour the web UI gives them.
        let me = self
            .places
            .read(cx)
            .get(&self.place)
            .and_then(|(_, st)| st.me.clone())
            .map(|m| {
                let colour = crate::presence::colour_for(&m.username).to_string();
                let name = if m.display_name.is_empty() {
                    m.username
                } else {
                    m.display_name
                };
                (name, colour)
            });
        let view = cx.new(|cx| DocEditor::new(&client, &env, root, abs, me, window, cx));
        self.subs.push(cx.observe(&view, |_, _, cx| cx.notify()));
        self.subs.push(
            cx.subscribe(&view, |t, _, e: &crate::blame::ShowCommit, cx| {
                t.show_commit(e.0.clone(), cx)
            }),
        );
        self.subs
            .push(cx.subscribe(&view, |t, doc, _: &docedit::NotText, cx| {
                t.show_as_bytes(&doc, cx)
            }));
        self.subs.push(cx.subscribe_in(
            &view,
            window,
            |t, _, e: &docedit::ShowChanges, window, cx| {
                let (root, path) = t.place_of(&e.0, cx);
                t.open_diff(Spec::working(&root, &path), window, cx)
            },
        ));
        self.attach_language(&view, cx);
        Some(view)
    }

    /// Every tab, in both groups.
    fn all_tabs(&self) -> impl Iterator<Item = &Tab> {
        self.files
            .iter()
            .chain(self.other.iter().flat_map(|g| g.files.iter()))
    }

    /// Whether the editors are split in two.
    pub fn split_in_two(&self) -> bool {
        self.other.is_some()
    }

    /// Whether the group in use is the right one.
    fn using_right(&self) -> bool {
        self.other.is_some() && !self.other_right
    }

    /// Makes the group on one side the one in use, as a click in it does.
    pub fn use_side(&mut self, right: bool, cx: &mut Context<Self>) {
        if self.other.is_some() && self.using_right() != right {
            self.swap_groups();
            cx.notify();
        }
    }

    fn swap_groups(&mut self) {
        let Some(other) = self.other.take() else {
            return;
        };
        let was = EditorGroup {
            files: std::mem::replace(&mut self.files, other.files),
            file: std::mem::replace(&mut self.file, other.file),
            tab_scroll: std::mem::replace(&mut self.tab_scroll, other.tab_scroll),
            tab_in_view: std::mem::replace(&mut self.tab_in_view, other.tab_in_view),
        };
        self.other = Some(was);
        self.other_right = !self.other_right;
    }

    /// Shows a tab in the other group, as IntelliJ's Split Right does, a
    /// group being made on the right if there is one; moved there, not
    /// copied, if asked. The other group is then the one in use.
    pub fn split(
        &mut self,
        i: usize,
        moved: bool,
        down: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.files.get(i).cloned() else {
            return;
        };
        let path = tab.path(cx);
        let tab = if moved {
            self.take_tab(i);
            tab
        } else {
            // A copy is an editor of its own on the same shared document,
            // unless the other group has the file already.
            match &tab {
                Tab::Doc(_) => {
                    let (root, _) = self.place_of(&path, cx);
                    let open = self.other.as_ref().is_some_and(|g| {
                        g.files
                            .iter()
                            .any(|f| matches!(f, Tab::Doc(_)) && f.path(cx) == path)
                    });
                    if open {
                        tab
                    } else {
                        match self.new_doc(&root, &path, window, cx) {
                            Some(d) => Tab::Doc(d),
                            None => return,
                        }
                    }
                }
                Tab::Diff(d) => {
                    let Some(ws) = self.workspace.clone() else {
                        return;
                    };
                    let spec = d.read(cx).spec().clone();
                    Tab::Diff(cx.new(|cx| DiffView::new(ws, spec, cx)))
                }
                // A session is shown once: it moves.
                Tab::Term(t) => {
                    let t = t.clone();
                    self.take_tab(i);
                    Tab::Term(t)
                }
                Tab::View(v) => {
                    let (abs, way) = (v.read(cx).path().to_string(), v.read(cx).view());
                    match self.new_viewer(&abs, way, cx) {
                        Some(v) => Tab::View(v),
                        None => return,
                    }
                }
            }
        };
        if self.other.is_none() {
            self.other = Some(EditorGroup::empty());
            self.other_right = true;
            self.split_down = down;
        }
        let emptied = self.files.is_empty();
        self.swap_groups();
        if emptied {
            // The group it was moved from had nothing else: it goes.
            self.other = None;
        }
        let at = match self
            .files
            .iter()
            .position(|f| f.twin_of(&tab, path.as_str(), cx))
        {
            Some(at) => {
                // Shown by a tab there already: this one is let go, unless
                // it is that very tab.
                let same = matches!((&tab, &self.files[at]), (Tab::Doc(a), Tab::Doc(b)) if a == b);
                if !same {
                    self.drop_tab(tab, cx);
                }
                at
            }
            None => {
                self.files.push(tab);
                self.files.len() - 1
            }
        };
        self.show_file(at, window, cx);
    }

    /// Gathers both groups' tabs into the one in use, as IntelliJ's Unsplit
    /// does; a file open in both is kept once.
    pub fn unsplit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(other) = self.other.take() else {
            return;
        };
        for tab in other.files {
            let path = tab.path(cx);
            let twin = self
                .files
                .iter()
                .any(|f| f.twin_of(&tab, path.as_str(), cx));
            if twin {
                self.drop_tab(tab, cx);
            } else {
                self.files.push(tab);
            }
        }
        if let Some(i) = self.file {
            self.show_file(i, window, cx);
        }
        cx.notify();
    }

    /// Moves a tab dropped in a group: before the tab at `at` or at the end;
    /// to its own group, it changes places there; with one group, dropped
    /// on the right, it splits.
    pub fn move_tab(
        &mut self,
        from: bool,
        i: usize,
        to: bool,
        at: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dragging_tab = false;
        if self.other.is_none() {
            if to && !from {
                return self.split(i, true, false, window, cx);
            }
            if from != to {
                return;
            }
        }
        self.use_side(from, cx);
        let Some(tab) = self.take_tab(i) else {
            return;
        };
        if from == to {
            let at = at
                .map(|a| if a > i { a - 1 } else { a })
                .unwrap_or(self.files.len())
                .min(self.files.len());
            self.files.insert(at, tab);
            return self.show_file(at, window, cx);
        }
        if self.files.is_empty() {
            // The group it left had nothing else: it goes.
            self.swap_groups();
            self.other = None;
        } else {
            self.use_side(to, cx);
        }
        let path = tab.path(cx);
        let twin = self
            .files
            .iter()
            .position(|f| f.twin_of(&tab, path.as_str(), cx));
        let shown = match twin {
            Some(t) => {
                self.drop_tab(tab, cx);
                t
            }
            None => {
                let at = at.unwrap_or(self.files.len()).min(self.files.len());
                self.files.insert(at, tab);
                at
            }
        };
        self.show_file(shown, window, cx);
    }

    /// Takes a tab out of the group in use, as moving it does: not closed.
    fn take_tab(&mut self, i: usize) -> Option<Tab> {
        if i >= self.files.len() {
            return None;
        }
        let tab = self.files.remove(i);
        self.file = match self.file {
            Some(a) if a == i => (!self.files.is_empty()).then(|| i.min(self.files.len() - 1)),
            Some(a) if a > i => Some(a - 1),
            a => a,
        };
        Some(tab)
    }

    /// Lets a tab go that another stands for, its file left open in its
    /// language server by that other.
    fn drop_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if let (Tab::Doc(doc), Some(l)) = (tab, &self.languages) {
            let asked = doc.read(cx).asked().to_string();
            l.update(cx, |l, cx| l.close(&asked, cx));
        }
    }

    /// Opens a file in its language server, and gives its editor the
    /// diagnostics it has, hover and Go to Definition.
    fn attach_language(&mut self, doc: &Entity<DocEditor>, cx: &mut Context<Self>) {
        let Some(l) = self.languages.clone() else {
            return;
        };
        let abs = doc.read(cx).asked().to_string();
        let d = l.read(cx).diagnostics_of(&abs).to_vec();
        let weak = l.downgrade();
        let offered = lspedit::Offered::default();
        let seen = offered.clone();
        doc.update(cx, |v, cx| {
            v.set_diagnostics(d, cx);
            v.set_languages(weak, seen);
        });
        l.update(cx, |l, cx| l.open(&abs, cx));
        let tab = cx.entity().downgrade();
        let editor = doc.read(cx).editor().clone();
        let edits = tab.clone();
        lspedit::attach(
            &editor,
            lspedit::Providers {
                languages: l.downgrade(),
                abs,
                offered,
                apply: std::rc::Rc::new(move |edit, window, cx| {
                    let _ = edits.update(cx, |t, cx| {
                        t.apply_edits(crate::lspops::by_file(edit), window, cx)
                    });
                }),
            },
            move |abs, line, column, window, cx| {
                let _ = tab.update(cx, |t, cx| {
                    let (root, path) = t.place_of(&abs, cx);
                    t.open_file_at(&root, &path, line, column, window, cx)
                });
            },
            cx,
        );
    }

    /// A file's project root and its path there, or / and its absolute
    /// path for a file in none.
    fn place_of(&self, abs: &str, cx: &App) -> (String, String) {
        let project = self
            .workspace
            .as_ref()
            .and_then(|w| w.read(cx).project_of(abs).map(|f| f.root.clone()));
        match project {
            Some(root) => {
                let path = abs[root.trim_end_matches('/').len() + 1..].to_string();
                (root, path)
            }
            None => ("/".into(), abs.to_string()),
        }
    }

    /// Shows a diff in the diff tab, as IntelliJ's preview does: there is
    /// one, which each diff shown takes over.
    pub fn open_diff(&mut self, spec: Spec, window: &mut Window, cx: &mut Context<Self>) {
        let at = self.files.iter().position(|f| matches!(f, Tab::Diff(_)));
        if let Some(i) = at
            && let Tab::Diff(d) = &self.files[i]
            && d.read(cx).spec() == &spec
        {
            self.show_file(i, window, cx);
            return;
        }
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        let tab = Tab::Diff(cx.new(|cx| DiffView::new(ws, spec, cx)));
        let i = match at {
            Some(i) => {
                self.files[i] = tab;
                i
            }
            None => {
                self.files.push(tab);
                self.files.len() - 1
            }
        };
        self.show_file(i, window, cx);
    }

    /// Gives the keyboard to the file shown, as Escape in a tool window
    /// does.
    pub fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(f) = self.file.and_then(|i| self.files.get(i)) {
            window.focus(&f.focus_handle(cx), cx);
        }
    }

    fn show_file(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.file = Some(i);
        if let Some(f) = self.files.get(i) {
            window.focus(&f.focus_handle(cx), cx);
        }
        if crate::settings::Settings::get(cx).select_opened {
            self.select_opened(false, cx);
        }
        cx.notify();
    }

    /// Selects the file shown in the Project window, or in Filesystem when
    /// no project holds it; asked for, the window opens, else it is only
    /// selected there for when it is.
    pub fn select_opened(&mut self, asked: bool, cx: &mut Context<Self>) {
        let Some(abs) = self.active_file().map(|f| f.read(cx).path().to_string()) else {
            return;
        };
        if asked {
            return self.reveal_in_project(&abs, cx);
        }
        let in_project = self
            .workspace
            .as_ref()
            .is_some_and(|w| w.read(cx).project_of(&abs).is_some());
        let view = if in_project {
            self.project.clone()
        } else {
            self.filesystem.clone()
        };
        if let Some(v) = view {
            v.update(cx, |v, cx| v.reveal(&abs, cx));
        }
    }

    pub fn close_file(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.files.len() {
            return;
        }
        let id = self.files[i].entity_id();
        let elsewhere = self
            .other
            .iter()
            .flat_map(|g| g.files.iter())
            .any(|f| f.entity_id() == id);
        if !elsewhere {
            self.pinned.remove(&id);
        }
        if let Tab::Doc(doc) = self.files.remove(i) {
            let abs = doc.read(cx).path().to_string();
            if let Some(l) = &self.languages {
                let asked = doc.read(cx).asked().to_string();
                l.update(cx, |l, cx| l.close(&asked, cx));
            }
            let root = self
                .workspace
                .as_ref()
                .and_then(|w| w.read(cx).project_of(&abs).map(|f| f.root.clone()))
                .unwrap_or_else(|| "/".into());
            self.closed.retain(|(_, a)| a != &abs);
            self.closed.push((root, abs));
            if self.closed.len() > 30 {
                self.closed.remove(0);
            }
        }
        self.file = match self.file {
            Some(a) if a == i => (!self.files.is_empty()).then(|| i.min(self.files.len() - 1)),
            Some(a) if a > i => Some(a - 1),
            a => a,
        };
        // A group whose last tab closes goes, the other taking its place.
        if self.files.is_empty() && self.other.is_some() {
            self.swap_groups();
            self.other = None;
        }
        cx.notify();
    }

    /// Closes every tab but those keep says to keep, as the tab menu's Close
    /// Others, Close Tabs to the Left or Right, and Close All do.
    pub fn close_tabs(&mut self, keep: impl Fn(usize) -> bool, cx: &mut Context<Self>) {
        for i in (0..self.files.len()).rev() {
            if !keep(i) {
                self.close_file(i, cx);
            }
        }
    }

    /// Closes the tabs of the group in use not kept, the pinned ones kept
    /// too.
    pub fn close_unpinned(&mut self, keep: impl Fn(usize) -> bool, cx: &mut Context<Self>) {
        let pinned: Vec<bool> = (0..self.files.len()).map(|j| self.is_pinned(j)).collect();
        self.close_tabs(|j| keep(j) || pinned[j], cx);
    }

    /// Whether a tab of the group in use is pinned.
    fn is_pinned(&self, i: usize) -> bool {
        self.files
            .get(i)
            .is_some_and(|f| self.pinned.contains(&f.entity_id()))
    }

    /// Pins a tab of the group in use, as IntelliJ's Pin Tab: it goes after
    /// the pinned tabs before it; unpinned, after those that stay pinned.
    pub fn pin_tab(&mut self, i: usize, pin: bool, cx: &mut Context<Self>) {
        let Some(id) = self.files.get(i).map(Tab::entity_id) else {
            return;
        };
        if pin {
            self.pinned.insert(id);
        } else {
            self.pinned.remove(&id);
        }
        let shown = self
            .file
            .and_then(|f| self.files.get(f))
            .map(Tab::entity_id);
        let tab = self.files.remove(i);
        let to = self
            .files
            .iter()
            .take_while(|f| self.pinned.contains(&f.entity_id()))
            .count();
        self.files.insert(to, tab);
        self.file = shown.and_then(|id| self.files.iter().position(|f| f.entity_id() == id));
        cx.notify();
    }

    /// Asks for a line, and a column after a colon, and puts the caret of
    /// the file shown there, as IntelliJ's Go to Line.
    pub fn go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.active_file() else {
            return;
        };
        let (line, col) = doc.read(cx).position(cx);
        crate::treeops::ask_name(
            "Go to Line:Column",
            &format!("{line}:{col}"),
            window,
            cx,
            move |typed, window, cx| {
                if let Some((line, col)) = docedit::line_column(&typed) {
                    doc.update(cx, |d, cx| {
                        d.go_to(line - 1, col - 1, window, cx);
                        window.focus(&d.focus_handle(cx), cx);
                    });
                }
            },
        );
    }

    /// The file shown, its server, and where its caret is: the word there
    /// and the request params naming the place.
    fn at_caret(&self, cx: &App) -> Option<Caret> {
        use gpui_component::input::RopeExt;
        let doc = self.active_file()?;
        let langs = self.languages.clone()?;
        let abs = doc.read(cx).path().to_string();
        let (text, caret) = doc.read(cx).text_and_caret(cx);
        let word = crate::lspops::word_at(&text.to_string(), caret);
        let params = serde_json::json!({
            "textDocument": { "uri": languages::file_uri(&abs) },
            "position": text.offset_to_position(caret),
        });
        Some(Caret {
            langs,
            abs,
            word,
            params,
        })
    }

    /// The connection that reaches a file: its project's, else the editor
    /// folder's, which reads the whole machine.
    fn code_for(&self, abs: &str, cx: &App) -> Option<crate::code::Code> {
        let ws = self.workspace.as_ref()?.read(cx);
        ws.project_of(abs)
            .map(|f| f.code.clone())
            .or_else(|| ws.primary())
    }

    /// Reformats the file shown as its language server formats it
    /// (IntelliJ's Reformat Code), as one change, the caret kept on its line.
    pub fn reformat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(doc), Some(langs)) = (self.active_file(), self.languages.clone()) else {
            return;
        };
        let abs = doc.read(cx).path().to_string();
        let (line, column) = doc.read(cx).position(cx);
        let asked = langs.update(cx, |l, cx| {
            l.request(
                &abs,
                "textDocument/formatting",
                serde_json::json!({
                    "textDocument": { "uri": languages::file_uri(&abs) },
                    "options": { "tabSize": 4, "insertSpaces": true },
                }),
                cx,
            )
        });
        cx.spawn_in(window, async move |_, cx| {
            let edits = asked.await.and_then(|v| {
                serde_json::from_value::<Option<Vec<lsp_types::TextEdit>>>(v)
                    .map_err(|e| e.to_string())
            });
            let _ = cx.update(|window, cx| match edits {
                Ok(edits) => {
                    let text = doc.read(cx).text_and_caret(cx).0;
                    if let Some(change) =
                        crate::lspops::changeset(&text, &edits.unwrap_or_default())
                    {
                        doc.update(cx, |d, cx| {
                            d.apply_own(change, window, cx);
                            d.go_to(line.saturating_sub(1), column.saturating_sub(1), window, cx);
                        });
                    }
                }
                Err(e) => {
                    use gpui_component::WindowExt;
                    window.push_notification(
                        gpui_component::notification::Notification::error(format!(
                            "Reformat Code: {e}"
                        )),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// Lists every use of the symbol at the caret in the Find window, as
    /// its language server finds them (IntelliJ's Find Usages, Alt-F7).
    pub fn find_usages(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Caret {
            langs,
            abs,
            word,
            mut params,
        }) = self.at_caret(cx)
        else {
            return;
        };
        params["context"] = serde_json::json!({ "includeDeclaration": true });
        let asked = langs.update(cx, |l, cx| {
            l.request(&abs, "textDocument/references", params, cx)
        });
        cx.spawn_in(window, async move |this, cx| {
            let found = asked.await.and_then(|v| {
                serde_json::from_value::<Option<Vec<lsp_types::Location>>>(v)
                    .map_err(|e| e.to_string())
            });
            let _ = this.update_in(cx, |t, window, cx| match found {
                Ok(l) => t.list_places(
                    format!("Usages of {word}"),
                    l.unwrap_or_default(),
                    window,
                    cx,
                ),
                Err(e) => {
                    use gpui_component::WindowExt;
                    window.push_notification(
                        gpui_component::notification::Notification::error(format!(
                            "Find Usages: {e}"
                        )),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// IntelliJ's Go to Implementation (⌥⌘B) and Type Declaration (⇧⌘B):
    /// asked of the file's server at the caret, one place opened, several
    /// listed in the Find window under what they are of.
    pub fn go_to_places(
        &mut self,
        method: &'static str,
        what: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Caret {
            langs,
            abs,
            word,
            params,
        }) = self.at_caret(cx)
        else {
            return;
        };
        let asked = langs.update(cx, |l, cx| l.request(&abs, method, params, cx));
        cx.spawn_in(window, async move |this, cx| {
            let found = asked.await.and_then(|v| {
                serde_json::from_value::<Option<lsp_types::GotoDefinitionResponse>>(v)
                    .map_err(|e| e.to_string())
            });
            let _ = this.update_in(cx, |t, window, cx| {
                use gpui_component::WindowExt;
                use gpui_component::notification::Notification;
                let places: Vec<lsp_types::Location> = match found {
                    Ok(r) => crate::lspedit::links(r)
                        .into_iter()
                        .map(|l| lsp_types::Location {
                            uri: l.target_uri,
                            range: l.target_selection_range,
                        })
                        .collect(),
                    Err(e) => {
                        window.push_notification(Notification::error(format!("{what}: {e}")), cx);
                        return;
                    }
                };
                match places.as_slice() {
                    [] => window.push_notification(
                        Notification::info(format!("No {} of {word}", what.to_lowercase())),
                        cx,
                    ),
                    [one] => {
                        if let Some(abs) = languages::uri_path(one.uri.as_str()) {
                            let (root, path) = t.place_of(&abs, cx);
                            let at = one.range.start;
                            t.open_file_at(&root, &path, at.line, at.character, window, cx);
                        }
                    }
                    _ => t.list_places(format!("{what} of {word}"), places, window, cx),
                }
            });
        })
        .detach();
    }

    /// Lists places in the Find window under a title, each with its line's
    /// text: from its editor for a file open here, else read from disk.
    pub fn list_places(
        &mut self,
        title: String,
        found: Vec<lsp_types::Location>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let open: HashMap<String, String> = self
            .files
            .iter()
            .filter_map(|f| match f {
                Tab::Doc(d) => {
                    let d = d.read(cx);
                    Some((d.path().to_string(), d.text_and_caret(cx).0.to_string()))
                }
                Tab::Diff(_) | Tab::Term(_) | Tab::View(_) => None,
            })
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let mut by_file: std::collections::BTreeMap<String, Vec<lsp_types::Range>> =
                Default::default();
            for l in found {
                if let Some(abs) = languages::uri_path(l.uri.as_str()) {
                    by_file.entry(abs).or_default().push(l.range);
                }
            }
            let mut by_root: std::collections::BTreeMap<String, Vec<crate::find::Match>> =
                Default::default();
            for (abs, mut ranges) in by_file {
                let text = match open.get(&abs) {
                    Some(t) => t.clone(),
                    None => {
                        let Ok(Some(code)) = this.update(cx, |t, cx| t.code_for(&abs, cx)) else {
                            continue;
                        };
                        match code
                            .call("fs.read", serde_json::json!({ "path": abs }))
                            .await
                        {
                            Ok(v) => v["text"].as_str().unwrap_or_default().to_string(),
                            Err(_) => continue,
                        }
                    }
                };
                let root = this
                    .update(cx, |t, cx| {
                        t.workspace
                            .as_ref()
                            .and_then(|w| w.read(cx).project_of(&abs).map(|f| f.root.clone()))
                    })
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "/".into());
                let rel = abs
                    .strip_prefix(&format!("{}/", root.trim_end_matches('/')))
                    .unwrap_or(&abs)
                    .to_string();
                let lines: Vec<&str> = text.lines().collect();
                ranges.sort_by_key(|r| (r.start.line, r.start.character));
                for r in ranges {
                    let Some(line) = lines.get(r.start.line as usize) else {
                        continue;
                    };
                    let end = if r.end.line == r.start.line {
                        r.end.character as usize
                    } else {
                        line.encode_utf16().count()
                    };
                    by_root
                        .entry(root.clone())
                        .or_default()
                        .push(crate::find::Match {
                            path: rel.clone(),
                            line: r.start.line + 1,
                            text: line.to_string(),
                            ranges: vec![[r.start.character as usize, end]],
                        });
                }
            }
            let _ = this.update(cx, |t, cx| {
                let found = by_root.into_iter().collect();
                if !t.is_open(Tool::Find) {
                    t.toggle(Tool::Find, cx);
                }
                match &t.find {
                    Some(f) => f.update(cx, |f, cx| f.show_usages(title, found, cx)),
                    None => t.pending_usages = Some((title, found)),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Renames the symbol at the caret everywhere its language server finds
    /// it (IntelliJ's Rename, Shift-F6), asking the new name.
    pub fn rename_symbol(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Caret { word, .. }) = self.at_caret(cx) else {
            return;
        };
        if word.is_empty() {
            return;
        }
        let this = cx.entity().downgrade();
        crate::treeops::ask_name("Rename", &word, window, cx, move |name, window, cx| {
            let _ = this.update(cx, |t, cx| t.rename_to(name, window, cx));
        });
    }

    /// Renames the symbol at the caret to name, everywhere its language
    /// server finds it.
    pub fn rename_to(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Caret {
            langs, abs, params, ..
        }) = self.at_caret(cx)
        else {
            return;
        };
        let mut params = params;
        params["newName"] = name.into();
        let asked = langs.update(cx, |l, cx| {
            l.request(&abs, "textDocument/rename", params, cx)
        });
        cx.spawn_in(window, async move |this, cx| {
            let edit = asked.await.and_then(|v| {
                serde_json::from_value::<Option<lsp_types::WorkspaceEdit>>(v)
                    .map_err(|e| e.to_string())
            });
            let _ = this.update_in(cx, |t, window, cx| match edit {
                Ok(Some(edit)) => t.apply_edits(crate::lspops::by_file(edit), window, cx),
                Ok(None) => {}
                Err(e) => {
                    use gpui_component::WindowExt;
                    window.push_notification(
                        gpui_component::notification::Notification::error(format!("Rename: {e}")),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// Makes a language server's edits: in a file open here as the user's
    /// own change, undone with one Undo; in any other through its shared
    /// document.
    pub fn apply_edits(
        &mut self,
        files: std::collections::BTreeMap<String, Vec<lsp_types::TextEdit>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for (abs, edits) in files {
            let open = self.all_tabs().find_map(|f| match f {
                Tab::Doc(d) if d.read(cx).path() == abs => Some(d.clone()),
                _ => None,
            });
            match open {
                Some(doc) => {
                    let text = doc.read(cx).text_and_caret(cx).0;
                    if let Some(change) = crate::lspops::changeset(&text, &edits) {
                        doc.update(cx, |d, cx| d.apply_own(change, window, cx));
                    }
                }
                None => {
                    let Some(code) = self.code_for(&abs, cx) else {
                        continue;
                    };
                    let edit = crate::lspops::edit_closed(code, abs.clone(), edits);
                    cx.spawn_in(window, async move |_, cx| {
                        if let Err(e) = edit.await {
                            let _ = cx.update(|window, cx| {
                                use gpui_component::WindowExt;
                                window.push_notification(
                                    gpui_component::notification::Notification::error(format!(
                                        "Could not change {abs}: {e}"
                                    )),
                                    cx,
                                );
                            });
                        }
                    })
                    .detach();
                }
            }
        }
    }

    /// Opens the file last closed again, as IntelliJ's Reopen Closed Tab.
    pub fn reopen_closed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((root, abs)) = self.closed.pop() {
            self.open_file(&root, &abs, window, cx);
        }
    }

    /// Shows a file in the Project window, or in Filesystem when it is in
    /// no project, opening the window.
    pub fn reveal_in_project(&mut self, abs: &str, cx: &mut Context<Self>) {
        let in_project = self
            .workspace
            .as_ref()
            .is_some_and(|w| w.read(cx).project_of(abs).is_some());
        let tool = if in_project {
            Tool::Project
        } else {
            Tool::Filesystem
        };
        if !self.is_open(tool) {
            self.toggle(tool, cx);
        }
        let view = if in_project {
            &self.project
        } else {
            &self.filesystem
        };
        match view {
            Some(v) => {
                v.update(cx, |v, cx| v.reveal(abs, cx));
            }
            None => self.pending_reveal = Some(abs.to_string()),
        }
        cx.notify();
    }

    /// The menu on an editor tab.
    fn tab_menu(
        menu: gpui_component::menu::PopupMenu,
        at: TabAt,
        abs: String,
        rel: String,
        tab: gpui::WeakEntity<Self>,
    ) -> gpui_component::menu::PopupMenu {
        use gpui_component::menu::PopupMenuItem;
        type Act = Rc<dyn Fn(&mut EnvTab, &mut Window, &mut Context<EnvTab>)>;
        let TabAt {
            i,
            count,
            right,
            split,
            pinned,
        } = at;
        // Each acts in the tab's group, which it makes the one in use.
        let on = |label: &str, f: Act| {
            let tab = tab.clone();
            PopupMenuItem::new(label.to_string()).on_click(move |_, window, cx| {
                let f = f.clone();
                let _ = tab.update(cx, |t, cx| {
                    t.use_side(right, cx);
                    f(t, window, cx)
                });
            })
        };
        let copy = |label: &str, text: String| {
            PopupMenuItem::new(label.to_string())
                .on_click(move |_, _, cx| crate::treeops::copy(text.clone(), cx))
        };
        let reveal = abs.clone();
        menu.item(on("Close", Rc::new(move |t, _, cx| t.close_file(i, cx))))
            .item(
                on(
                    "Close Others",
                    Rc::new(move |t, _, cx| t.close_unpinned(|j| j == i, cx)),
                )
                .disabled(count < 2),
            )
            .item(
                on(
                    "Close Tabs to the Left",
                    Rc::new(move |t, _, cx| t.close_unpinned(|j| j >= i, cx)),
                )
                .disabled(i == 0),
            )
            .item(
                on(
                    "Close Tabs to the Right",
                    Rc::new(move |t, _, cx| t.close_unpinned(|j| j <= i, cx)),
                )
                .disabled(i + 1 >= count),
            )
            .item(on(
                "Close All",
                Rc::new(|t, _, cx| t.close_tabs(|_| false, cx)),
            ))
            .item(on(
                "Close All but Pinned",
                Rc::new(|t, _, cx| t.close_unpinned(|_| false, cx)),
            ))
            .separator()
            .item(on(
                if pinned { "Unpin Tab" } else { "Pin Tab" },
                Rc::new(move |t, _, cx| t.pin_tab(i, !pinned, cx)),
            ))
            .separator()
            .item(on(
                "Reopen Closed Tab",
                Rc::new(|t, window, cx| t.reopen_closed(window, cx)),
            ))
            .separator()
            // IntelliJ's splitting: to the right while there is one group,
            // then between the two.
            .item(on(
                if split {
                    "Open in Opposite Group"
                } else {
                    "Split Right"
                },
                Rc::new(move |t, window, cx| t.split(i, false, false, window, cx)),
            ))
            .when(!split, |m| {
                m.item(on(
                    "Split Down",
                    Rc::new(move |t, window, cx| t.split(i, false, true, window, cx)),
                ))
            })
            .item(
                on(
                    if split {
                        "Move to Opposite Group"
                    } else {
                        "Split and Move Right"
                    },
                    Rc::new(move |t, window, cx| t.split(i, true, false, window, cx)),
                )
                .disabled(!split && count < 2),
            )
            .when(!split, |m| {
                m.item(
                    on(
                        "Split and Move Down",
                        Rc::new(move |t, window, cx| t.split(i, true, true, window, cx)),
                    )
                    .disabled(count < 2),
                )
            })
            .when(split, |m| {
                m.item(on(
                    "Unsplit",
                    Rc::new(|t, window, cx| t.unsplit(window, cx)),
                ))
            })
            .separator()
            // A terminal has no path; it can go back to the Terminal window.
            .map(|m| {
                if abs.is_empty() {
                    return m.item(on(
                        "Move to Terminal",
                        Rc::new(move |t, window, cx| t.terminal_to_window(i, window, cx)),
                    ));
                }
                m.item(copy("Copy Path", abs))
                    .item(copy("Copy Relative Path", rel))
                    .item(on(
                        "Reveal in Project",
                        Rc::new(move |t, _, cx| t.reveal_in_project(&reveal, cx)),
                    ))
            })
    }

    /// The files open, by absolute path, and the one shown.
    pub fn open_files(&self, cx: &App) -> (Vec<String>, Option<usize>) {
        (self.files.iter().map(|f| f.path(cx)).collect(), self.file)
    }

    /// The editor of the file shown, when it is a file's.
    pub fn active_file(&self) -> Option<Entity<DocEditor>> {
        match self.file.and_then(|i| self.files.get(i)) {
            Some(Tab::Doc(d)) => Some(d.clone()),
            _ => None,
        }
    }

    /// The diff shown, when it is one.
    pub fn active_diff(&self) -> Option<Entity<DiffView>> {
        match self.file.and_then(|i| self.files.get(i)) {
            Some(Tab::Diff(d)) => Some(d.clone()),
            _ => None,
        }
    }

    /// The Project window's view, once it has been shown.
    pub fn project(&self) -> Option<Entity<ProjectView>> {
        self.project.clone()
    }

    /// Opens Find in Files' pop-up on a running environment, or brings it
    /// back to the front, its query taking what is typed.
    pub fn find_in_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_find(false, window, cx);
    }

    /// The same pop-up with a replacement, as IntelliJ's Replace in Files.
    pub fn replace_in_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_find(true, window, cx);
    }

    /// Find or Replace in Files on a folder, as the Project window's menu
    /// asks: the last search, looking there.
    pub fn find_in(
        &mut self,
        scope: crate::find::Scope,
        replace: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.find_popup.is_some() {
            self.close_find(window, cx);
        }
        let mut search = self.last_search.clone().unwrap_or_default();
        search.scope = scope;
        self.last_search = Some(search);
        self.open_find(replace, window, cx);
    }

    fn open_find(&mut self, replace: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(f) = &self.find_popup {
            f.update(cx, |f, cx| f.set_replacing(replace, cx));
            window.focus(&f.focus_handle(cx), cx);
            return;
        }
        let env = self
            .places
            .read(cx)
            .get(&self.place)
            .and_then(|(_, s)| s.environment(&self.env).cloned())
            .filter(|e| e.phase == "running");
        let Some(env) = env else {
            return;
        };
        let ws = self.workspace(&env, cx);
        let last = self.last_search.clone();
        let popup = cx.new(|cx| FindPopup::new(ws, last, replace, window, cx));
        self.find_sub = Some(
            cx.subscribe_in(&popup, window, |t, _, e: &Done, window, cx| {
                t.found(e, window, cx)
            }),
        );
        window.focus(&popup.focus_handle(cx), cx);
        self.find_popup = Some(popup);
        cx.notify();
    }

    /// Find in Files' pop-up, while it is open.
    pub fn find_popup(&self) -> Option<Entity<FindPopup>> {
        self.find_popup.clone()
    }

    /// Closes Find in Files' pop-up, keeping its search for next time.
    pub fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.find_sub = None;
        if let Some(f) = self.find_popup.take()
            && let Ok(s) = f.read(cx).current(cx)
        {
            self.last_search = Some(s);
        }
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn found(&mut self, done: &Done, window: &mut Window, cx: &mut Context<Self>) {
        self.close_find(window, cx);
        match done {
            Done::Open(at) => self.open_file_at(&at.root, &at.path, at.line, at.column, window, cx),
            Done::ToWindow(search) => {
                if !self.is_open(Tool::Find) {
                    self.toggle(Tool::Find, cx);
                }
                self.pending_find = Some(search.clone());
            }
            Done::Close => {}
        }
    }

    /// The environment's projects, once a window showing them has been
    /// shown.
    pub fn current_workspace(&self) -> Option<Entity<Workspace>> {
        self.workspace.clone()
    }

    /// The language servers, once there is a workspace.
    pub fn languages(&self) -> Option<Entity<Languages>> {
        self.languages.clone()
    }

    /// The Find window's view, once it has been shown.
    pub fn find_view(&self) -> Option<Entity<FindView>> {
        self.find.clone()
    }

    /// Opens a file in the editors with the cursor at a 0-based line and
    /// UTF-16 column.
    pub fn open_file_at(
        &mut self,
        root: &str,
        path: &str,
        line: u32,
        column: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file(root, path, window, cx);
        if let Some(f) = self.active_file() {
            f.update(cx, |f, cx| f.go_to(line, column, window, cx));
        }
    }

    /// What a Project or Filesystem window asks of the editors and the Git
    /// window: a diff, a comparison with the file shown, a file's history.
    fn follow_tree(
        &mut self,
        view: &Entity<ProjectView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.subs.push(cx.subscribe_in(
            view,
            window,
            |t, _, e: &crate::project::OpenWith, window, cx| {
                t.open_with(&e.root, &e.path, e.way, window, cx)
            },
        ));
        self.subs.push(
            cx.subscribe_in(view, window, |t, _, e: &ShowDiff, window, cx| {
                t.open_diff(e.0.clone(), window, cx)
            }),
        );
        self.subs.push(cx.subscribe_in(
            view,
            window,
            |t, _, e: &crate::project::CompareWithEditor, window, cx| {
                t.compare_with_editor(e, window, cx)
            },
        ));
        self.subs.push(cx.subscribe_in(
            view,
            window,
            |t, _, e: &crate::project::ShowHistory, _, cx| t.show_history(e.clone(), cx),
        ));
        self.subs.push(cx.subscribe_in(
            view,
            window,
            |t, _, e: &crate::project::Annotate, window, cx| t.annotate(e, window, cx),
        ));
        self.subs.push(cx.subscribe_in(
            view,
            window,
            |t, _, e: &crate::project::OpenInTerminal, window, cx| {
                t.new_session(Some(e.0.clone()), None, window, cx)
            },
        ));
    }

    /// Compares a file with the one shown in the editor: the file on the
    /// left, the editor's on the right.
    fn compare_with_editor(
        &mut self,
        e: &crate::project::CompareWithEditor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(shown) = self.active_file().map(|f| f.read(cx).path().to_string()) else {
            gpui_component::WindowExt::push_notification(
                window,
                gpui_component::notification::Notification::info("No file is open in the editor."),
                cx,
            );
            return;
        };
        let root = self
            .workspace
            .as_ref()
            .and_then(|ws| ws.read(cx).project_of(&shown).map(|f| f.root.clone()))
            .unwrap_or_else(|| "/".into());
        let path = shown
            .strip_prefix(&format!("{}/", root.trim_end_matches('/')))
            .unwrap_or(&shown)
            .to_string();
        let other = crate::diff::Other::File {
            root: e.root.clone(),
            abs: e.abs.clone(),
        };
        self.open_diff(Spec::compare(&root, &path, other), window, cx);
    }

    /// Shows a commit in the Git window, opening it: one an annotation
    /// names.
    fn show_commit(&mut self, hash: String, cx: &mut Context<Self>) {
        if !self.is_open(Tool::Git) {
            self.toggle(Tool::Git, cx);
        }
        match self.git.clone() {
            Some(view) => view.update(cx, |v, cx| v.select(hash, cx)),
            None => self.pending_commit = Some(hash),
        }
        cx.notify();
    }

    /// Opens a file annotated with git blame.
    fn annotate(
        &mut self,
        e: &crate::project::Annotate,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_file(&e.root, &e.path, window, cx);
        if let Some(f) = self.active_file() {
            f.update(cx, |f, cx| f.set_blame(true, cx));
        }
    }

    /// Shows a file's history in the Git window, opening it.
    fn show_history(&mut self, h: crate::project::ShowHistory, cx: &mut Context<Self>) {
        if !self.is_open(Tool::Git) {
            self.toggle(Tool::Git, cx);
        }
        match self.git.clone() {
            Some(view) => view.update(cx, |v, cx| v.show_history(&h.root, &h.path, cx)),
            None => self.pending_history = Some(h),
        }
        cx.notify();
    }

    /// The Git window's view, once it has been shown.
    pub fn git_view(&self) -> Option<Entity<GitView>> {
        self.git.clone()
    }

    /// The Commit window's view, once it has been shown.
    pub fn commit_view(&self) -> Option<Entity<CommitView>> {
        self.commit.clone()
    }

    /// The environment's workspace, connected the first time it is asked
    /// for.
    fn workspace(&mut self, env: &Environment, cx: &mut Context<Self>) -> Entity<Workspace> {
        if let Some(ws) = &self.workspace {
            return ws.clone();
        }
        let (places, place, id) = (self.places.clone(), self.place.clone(), self.env.clone());
        let editor = env.editor_path.clone().unwrap_or_default();
        let ws = cx.new(|cx| Workspace::new(places, &place, &id, &editor, cx));
        self.workspace = Some(ws.clone());
        let branches = cx.new(|cx| crate::branches::BranchWidget::new(ws.clone(), cx));
        self.subs.push(
            cx.subscribe(&branches, |t, _, _: &crate::branches::OpenCommit, cx| {
                t.open_commit(cx)
            }),
        );
        self.branches = Some(branches);
        self.pinger = Some(crate::latency::watch(
            ws.clone(),
            cx.entity().downgrade(),
            |t: &mut Self, ms, cx| {
                if t.latency != ms {
                    t.latency = ms;
                    cx.notify();
                }
            },
            cx,
        ));
        let langs = cx.new(|cx| Languages::new(ws.clone(), cx));
        self.subs
            .push(cx.subscribe(&langs, |t, l, e: &languages::Event, cx| {
                if let languages::Event::ApplyEdit(edit) = e {
                    t.pending_edits.push(edit.clone());
                    cx.notify();
                }
                if let languages::Event::Message(kind, text) = e {
                    t.pending_messages.push((*kind, text.clone()));
                    cx.notify();
                }
                if let languages::Event::Diagnostics(abs) = e {
                    let d = l.read(cx).diagnostics_of(abs).to_vec();
                    for f in t.all_tabs() {
                        if let Tab::Doc(doc) = f
                            && (doc.read(cx).path() == abs || doc.read(cx).asked() == abs)
                        {
                            doc.update(cx, |doc, cx| doc.set_diagnostics(d.clone(), cx));
                        }
                    }
                }
            }));
        self.languages = Some(langs);
        // Files opened before there was a workspace.
        let docs: Vec<Tab> = self.all_tabs().cloned().collect();
        for f in docs {
            if let Tab::Doc(doc) = f {
                self.attach_language(&doc, cx);
            }
        }
        ws
    }

    /// Types into the terminal shown, as the user would.
    pub fn type_in_terminal(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(v) = self
            .terms
            .active
            .and_then(|i| self.terms.tabs[i].view.clone())
        {
            v.update(cx, |v, _| v.type_text(text));
        }
    }

    /// Types into a session, as the user would there: an agent's answer.
    fn type_into(
        &mut self,
        session: &str,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_terminal(session, window, cx);
        if let Some(v) = self
            .terms
            .active
            .and_then(|i| self.terms.tabs[i].view.clone())
        {
            v.update(cx, |v, _| v.type_text(text));
        }
    }

    /// Takes a tab away, leaving its session running.
    fn close_terminal_tab(&mut self, i: usize, cx: &mut Context<Self>) {
        if i >= self.terms.tabs.len() {
            return;
        }
        self.terms.tabs.remove(i);
        self.terms.active = match self.terms.active {
            Some(a) if a == i => {
                (!self.terms.tabs.is_empty()).then(|| i.min(self.terms.tabs.len() - 1))
            }
            Some(a) if a > i => Some(a - 1),
            a => a,
        };
        cx.notify();
    }

    /// Moves a session from the Terminal window into the editor area, as
    /// IntelliJ's Move to Editor does: a tab of its own beside the files.
    fn terminal_to_editor(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.select_terminal(i, false, window, cx);
        let Some(view) = self.terms.tabs.get(i).and_then(|t| t.view.clone()) else {
            return;
        };
        self.close_terminal_tab(i, cx);
        self.files.push(Tab::Term(view.clone()));
        self.show_file(self.files.len() - 1, window, cx);
        window.focus(&view.focus_handle(cx), cx);
    }

    /// Moves a session from the editor area back to the Terminal window,
    /// shown there.
    fn terminal_to_window(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Tab::Term(view)) = self.files.get(i).cloned() else {
            return;
        };
        self.close_file(i, cx);
        self.open
            .insert(Layout::slot(Tool::Terminal, cx), Tool::Terminal);
        self.terms.listed = true;
        self.terms.tabs.push(TermTab {
            session: view.read(cx).session().map(str::to_string),
            title: view.read(cx).title().to_string(),
            view: Some(view),
        });
        let at = self.terms.tabs.len() - 1;
        self.select_terminal(at, true, window, cx);
    }

    /// The tab in the editor area holding the terminal with the keyboard.
    fn focused_editor_terminal(&self, window: &Window, cx: &App) -> Option<usize> {
        self.files.iter().position(|f| match f {
            Tab::Term(t) => t.focus_handle(cx).contains_focused(window, cx),
            _ => false,
        })
    }

    /// Ends the session of a terminal in the editor area, once asked, and
    /// closes its tab.
    fn end_editor_terminal(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Tab::Term(view)) = self.files.get(i).cloned() else {
            return;
        };
        let label = self.files[i].title(cx);
        let session = view.read(cx).session().map(str::to_string);
        let me = cx.entity().downgrade();
        crate::treeops::confirm(
            &format!("End {label}?"),
            "The session ends, and what runs in it.",
            "End Session",
            window,
            cx,
            move |_, cx| {
                let session = session.clone();
                let view = view.clone();
                let _ = me.update(cx, |t, cx| {
                    let at = t
                        .files
                        .iter()
                        .position(|f| matches!(f, Tab::Term(v) if v == &view));
                    if let Some(at) = at {
                        t.close_file(at, cx);
                    }
                    let (Some(session), Some(client)) = (session, t.client(cx)) else {
                        return;
                    };
                    let env = t.env.clone();
                    cx.background_spawn(async move { client.close_terminal(&env, &session) })
                        .detach();
                });
            },
        );
    }

    /// Closes the tab shown, as Close Tab in its terminal does, and gives
    /// the keyboard to the one shown after it.
    fn close_shown_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(i) = self.terms.active {
            self.close_terminal_tab(i, cx);
            if let Some(a) = self.terms.active {
                self.select_terminal(a, true, window, cx);
            }
        }
    }

    /// Takes every tab away but one, leaving their sessions running.
    fn close_other_terminal_tabs(&mut self, i: usize, cx: &mut Context<Self>) {
        if i < self.terms.tabs.len() {
            let keep = self.terms.tabs.swap_remove(i);
            self.terms.tabs = vec![keep];
            self.terms.active = Some(0);
            cx.notify();
        }
    }

    /// Ends a tab's session, and what runs in it, once asked; its tab goes.
    fn end_terminal(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.terms.tabs.get(i) else {
            return;
        };
        let (label, session) = (tab.label(cx), tab.id(cx));
        let view = tab.view.as_ref().map(|v| v.entity_id());
        let me = cx.entity().downgrade();
        crate::treeops::confirm(
            &format!("End {label}?"),
            "The session ends, and what runs in it.",
            "End Session",
            window,
            cx,
            move |_, cx| {
                let session = session.clone();
                let _ = me.update(cx, |t, cx| {
                    // The tab, found again: others may have closed since.
                    let at = t.terms.tabs.iter().position(|x| {
                        x.id(cx) == session && x.view.as_ref().map(|v| v.entity_id()) == view
                    });
                    if let Some(at) = at {
                        t.close_terminal_tab(at, cx);
                    }
                    let (Some(session), Some(client)) = (session, t.client(cx)) else {
                        return;
                    };
                    let env = t.env.clone();
                    cx.background_spawn(async move { client.close_terminal(&env, &session) })
                        .detach();
                });
            },
        );
    }

    /// Opens the menu of sessions not open here, or closes it.
    fn toggle_terminal_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terms.menu.take().is_some() {
            cx.notify();
            return;
        }
        let Some(client) = self.client(cx) else {
            return;
        };
        let env = self.env.clone();
        let task = cx.background_spawn(async move { client.terminals(&env) });
        cx.spawn_in(window, async move |this, cx| {
            let list = task.await.unwrap_or_default();
            let _ = this.update(cx, |t, cx| {
                let open: Vec<Option<String>> = t.terms.tabs.iter().map(|x| x.id(cx)).collect();
                t.terms.menu = Some(
                    list.into_iter()
                        .filter(|s| !open.contains(&Some(s.id.clone())))
                        .collect(),
                );
                cx.notify();
            });
        })
        .detach();
    }

    fn edit_task(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_string();
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("What is this environment for?")
                .default_value(text)
        });
        cx.subscribe_in(&input, window, |this, _, e: &InputEvent, _, cx| {
            if let InputEvent::PressEnter { .. } = e {
                this.save_task(cx);
            }
        })
        .detach();
        input.update(cx, |i, cx| i.focus(window, cx));
        self.task_edit = Some(input);
        cx.notify();
    }

    fn save_task(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.task_edit.take() else {
            return;
        };
        let text = input.read(cx).value().to_string();
        let (place, env) = (self.place.clone(), self.env.clone());
        self.places
            .update(cx, |p, cx| p.set_task(&place, &env, &text, cx));
        cx.notify();
    }
}

impl Focusable for EnvTab {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for EnvTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        for (kind, text) in std::mem::take(&mut self.pending_messages) {
            window.defer(cx, move |window, cx| {
                use gpui_component::WindowExt;
                use gpui_component::notification::Notification;
                let n = match kind {
                    1 => Notification::error(text),
                    2 => Notification::warning(text),
                    _ => Notification::info(text),
                };
                window.push_notification(n, cx);
            });
        }
        for edit in std::mem::take(&mut self.pending_edits) {
            let tab = cx.entity().downgrade();
            window.defer(cx, move |window, cx| {
                let _ = tab.update(cx, |t, cx| {
                    t.apply_edits(crate::lspops::by_file(edit), window, cx)
                });
            });
        }
        let p = palette::of(cx);
        let places = self.places.read(cx);
        let Some((config, state)) = places.get(&self.place) else {
            return div().p_4().child("This place is gone.").into_any_element();
        };
        let Some(env) = state.environment(&self.env).cloned() else {
            return div()
                .p_4()
                .text_color(p.muted)
                .child("This environment has been deleted, or is out of reach.")
                .into_any_element();
        };
        let config = config.clone();
        let card = model::card(&config, &env, chrono::Utc::now());

        let stripes = Layout::stripes(cx);
        self.settle(&stripes);
        if !cx.has_active_drag() {
            self.dragging = false;
            self.dragging_tab = false;
        }
        let left = self.open.get(&Slot::Left).copied();
        let left_below = self.open.get(&Slot::LeftBelow).copied();
        let right = self.open.get(&Slot::Right).copied();
        let right_below = self.open.get(&Slot::RightBelow).copied();
        let bottom = self.open.get(&Slot::Bottom).copied();
        let waiting = env.agents.iter().filter(|a| a.needs_you).count();
        if env.phase == "running" {
            if self.project.is_none() && self.is_open(Tool::Project) {
                let ws = self.workspace(&env, cx);
                let view = cx.new(|cx| ProjectView::new(ws, cx));
                self.follow_tree(&view, window, cx);
                self.subs.push(
                    cx.subscribe_in(&view, window, |t, _, e: &OpenFile, window, cx| {
                        t.open_file(&e.root, &e.path, window, cx)
                    }),
                );
                self.subs.push(cx.subscribe_in(
                    &view,
                    window,
                    |t, _, e: &crate::project::FindIn, window, cx| {
                        t.find_in(e.scope.clone(), e.replace, window, cx)
                    },
                ));
                self.subs.push(cx.subscribe_in(
                    &view,
                    window,
                    |t, _, _: &crate::project::Leave, window, cx| t.focus_editor(window, cx),
                ));
                self.project = Some(view);
            }
            if self.structure.is_none()
                && self.is_open(Tool::Structure)
                && let Some(langs) = self.languages.clone()
            {
                self.structure = Some(cx.new(|_| crate::structure::StructureView::new(langs)));
            }
            if let Some(s) = self.structure.clone() {
                let doc = self.active_file();
                s.update(cx, |s, cx| s.follow(doc, cx));
            }
            if self.filesystem.is_none() && self.is_open(Tool::Filesystem) {
                let ws = self.workspace(&env, cx);
                let view = cx.new(|cx| ProjectView::machine(ws, cx));
                self.follow_tree(&view, window, cx);
                self.subs.push(
                    cx.subscribe_in(&view, window, |t, _, e: &OpenFile, window, cx| {
                        t.open_file(&e.root, &e.path, window, cx)
                    }),
                );
                self.subs.push(cx.subscribe_in(
                    &view,
                    window,
                    |t, _, _: &crate::project::Leave, window, cx| t.focus_editor(window, cx),
                ));
                self.filesystem = Some(view);
            }
            if let Some(abs) = self.pending_reveal.take() {
                let shown = [self.project.clone(), self.filesystem.clone()]
                    .into_iter()
                    .flatten()
                    .any(|v| v.update(cx, |v, cx| v.reveal(&abs, cx)));
                if !shown && self.project.is_none() && self.filesystem.is_none() {
                    self.pending_reveal = Some(abs);
                }
            }
            if self.commit.is_none() && self.is_open(Tool::Commit) {
                let ws = self.workspace(&env, cx);
                let view = cx.new(|cx| CommitView::new(ws, window, cx));
                self.subs.push(
                    cx.subscribe_in(&view, window, |t, _, e: &OpenFile, window, cx| {
                        t.open_file(&e.root, &e.path, window, cx)
                    }),
                );
                self.subs.push(cx.subscribe_in(
                    &view,
                    window,
                    |t, _, e: &crate::project::Reveal, _, cx| t.reveal_in_project(&e.abs, cx),
                ));
                self.subs.push(
                    cx.subscribe_in(&view, window, |t, _, e: &ShowDiff, window, cx| {
                        t.open_diff(e.0.clone(), window, cx)
                    }),
                );
                self.commit = Some(view);
            }
            if self.focus_commit
                && let Some(view) = &self.commit
            {
                self.focus_commit = false;
                let message = view.read(cx).message().focus_handle(cx);
                window.focus(&message, cx);
            }
            if self.find.is_none() && self.is_open(Tool::Find) {
                let ws = self.workspace(&env, cx);
                let view = cx.new(|cx| FindView::new(ws, window, cx));
                self.subs.push(
                    cx.subscribe_in(&view, window, |t, _, e: &OpenAt, window, cx| {
                        t.open_file_at(&e.root, &e.path, e.line, e.column, window, cx)
                    }),
                );
                window.focus(&view.focus_handle(cx), cx);
                self.find = Some(view);
            }
            if let (Some(view), Some((title, found))) =
                (self.find.clone(), self.pending_usages.take())
            {
                view.update(cx, |v, cx| v.show_usages(title, found, cx));
            }
            if let (Some(view), Some(s)) = (self.find.clone(), self.pending_find.take()) {
                view.update(cx, |v, cx| v.show(s, window, cx));
            }
            if self.problems.is_none() && self.is_open(Tool::Problems) {
                let ws = self.workspace(&env, cx);
                if let Some(langs) = self.languages.clone() {
                    let view = cx.new(|cx| ProblemsView::new(ws, langs, cx));
                    self.subs.push(cx.subscribe_in(
                        &view,
                        window,
                        |t, _, e: &OpenAt, window, cx| {
                            t.open_file_at(&e.root, &e.path, e.line, e.column, window, cx)
                        },
                    ));
                    self.problems = Some(view);
                }
            }
            if self.docker.is_none() && self.is_open(Tool::Docker) {
                let ws = self.workspace(&env, cx);
                let view = cx.new(|cx| DockerView::new(ws, cx));
                self.subs.push(
                    cx.subscribe_in(&view, window, |t, _, e: &OpenFile, window, cx| {
                        t.open_file(&e.root, &e.path, window, cx)
                    }),
                );
                self.subs.push(cx.subscribe_in(
                    &view,
                    window,
                    |t, _, e: &RunInTerminal, window, cx| {
                        t.new_session(None, Some(e.0.clone()), window, cx)
                    },
                ));
                self.subs.push(cx.observe(&view, |_, _, cx| cx.notify()));
                self.docker = Some(view);
            }
            let ports_open = self.is_open(Tool::Ports);
            if self.ports.is_none() && ports_open {
                let ws = self.workspace(&env, cx);
                let (places, place, id) =
                    (self.places.clone(), self.place.clone(), self.env.clone());
                let view = cx.new(|cx| PortsView::new(ws, places, &place, &id, cx));
                self.subs.push(cx.observe(&view, |_, _, cx| cx.notify()));
                self.ports = Some(view);
            }
            if let Some(v) = &self.ports
                && v.read(cx).paused() == ports_open
            {
                v.update(cx, |v, cx| v.set_paused(!ports_open, cx));
            }
            if self.git.is_none() && self.is_open(Tool::Git) {
                let ws = self.workspace(&env, cx);
                let view = cx.new(|cx| GitView::new(ws, cx));
                self.subs.push(
                    cx.subscribe_in(&view, window, |t, _, e: &ShowDiff, window, cx| {
                        t.open_diff(e.0.clone(), window, cx)
                    }),
                );
                self.git = Some(view);
            }
            if let (Some(view), Some(h)) = (self.git.clone(), self.pending_history.take()) {
                view.update(cx, |v, cx| v.show_history(&h.root, &h.path, cx));
            }
            if let (Some(view), Some(hash)) = (self.git.clone(), self.pending_commit.take()) {
                view.update(cx, |v, cx| v.select(hash, cx));
            }
        }
        // The worker keeps the logs whatever the environment is doing.
        let logs_open = self.is_open(Tool::Log);
        if self.logs.is_none()
            && logs_open
            && let Some(client) = self.client(cx)
        {
            let env = self.env.clone();
            self.logs = Some(cx.new(|cx| LogsView::new(client, &env, cx)));
        }
        if let Some(v) = &self.logs
            && v.read(cx).paused() == logs_open
        {
            v.update(cx, |v, cx| v.set_paused(!logs_open, cx));
        }
        let sizes = Layout::sizes(cx);

        let find_popup = self.find_popup.clone();
        v_flex()
            .size_full()
            .relative()
            .key_context("EnvTab")
            .on_action(cx.listener(|t, _: &HideAllToolWindows, _, cx| t.hide_all(cx)))
            .on_action(cx.listener(|t, _: &HideActiveToolWindow, _, cx| t.hide_active(cx)))
            .on_drag_move(cx.listener(|_, e: &DragMoveEvent<Edge>, _, cx| {
                let edge = *e.drag(cx);
                let inset = match edge {
                    Edge::Left | Edge::Right => 38.,
                    Edge::Bottom => 24.,
                    Edge::Sidebar | Edge::LeftSplit | Edge::RightSplit => return,
                };
                let size = Sizes::dragged(edge, e.event.position, e.bounds, inset);
                Layout::update(cx, |s| s.set(edge, size));
            }))
            .track_focus(&self.focus)
            .on_action(cx.listener(|t, _: &ToggleProject, _, cx| t.toggle(Tool::Project, cx)))
            .on_action(cx.listener(|t, _: &ToggleCommit, _, cx| t.toggle(Tool::Commit, cx)))
            .on_action(cx.listener(|t, _: &ToggleStructure, _, cx| t.toggle(Tool::Structure, cx)))
            .on_action(cx.listener(|t, _: &ToggleTerminal, _, cx| t.toggle(Tool::Terminal, cx)))
            .on_action(cx.listener(|t, _: &ToggleProblems, _, cx| t.toggle(Tool::Problems, cx)))
            .on_action(cx.listener(|t, _: &ToggleGit, _, cx| t.toggle(Tool::Git, cx)))
            .on_action(cx.listener(|t, _: &ToggleDocker, _, cx| t.toggle(Tool::Docker, cx)))
            .on_action(cx.listener(|t, _: &FindInFiles, window, cx| t.find_in_files(window, cx)))
            .on_action(cx.listener(|t, _: &GoToLine, window, cx| t.go_to_line(window, cx)))
            .on_action(cx.listener(|t, _: &FindUsages, window, cx| t.find_usages(window, cx)))
            .on_action(cx.listener(|t, _: &RenameSymbol, window, cx| t.rename_symbol(window, cx)))
            .on_action(cx.listener(|t, _: &GoToImplementation, window, cx| {
                t.go_to_places("textDocument/implementation", "Implementations", window, cx)
            }))
            .on_action(cx.listener(|t, _: &GoToTypeDeclaration, window, cx| {
                t.go_to_places(
                    "textDocument/typeDefinition",
                    "Type declarations",
                    window,
                    cx,
                )
            }))
            .on_action(cx.listener(|t, _: &CloseTab, _, cx| {
                if let Some(i) = t.file {
                    t.close_file(i, cx);
                }
            }))
            .on_action(
                cx.listener(|t, _: &ReopenClosedTab, window, cx| t.reopen_closed(window, cx)),
            )
            .on_action(
                cx.listener(|t, _: &ReplaceInFiles, window, cx| t.replace_in_files(window, cx)),
            )
            .on_action(
                cx.listener(|t, _: &crate::docedit::Reformat, window, cx| t.reformat(window, cx)),
            )
            .child(self.toolbar(&config, &env, &card, &p, cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(self.stripe(&stripes, true, waiting, &p, cx))
                    .child(
                        // As IntelliJ: the side windows end above the bottom
                        // one, which takes the width between the stripes.
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_h_0()
                                    .items_stretch()
                                    .when(left.is_some() || left_below.is_some(), |d| {
                                        d.child(
                                            self.side(true, left, left_below, &env, &card, &p, cx),
                                        )
                                    })
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .bg(p.surface)
                                            .child(self.editors(&p, cx)),
                                    )
                                    .when(right.is_some() || right_below.is_some(), |d| {
                                        d.child(self.side(
                                            false,
                                            right,
                                            right_below,
                                            &env,
                                            &card,
                                            &p,
                                            cx,
                                        ))
                                    }),
                            )
                            .when_some(bottom, |d, t| {
                                d.child(self.bottom(
                                    t,
                                    sizes.get(Edge::Bottom),
                                    &env,
                                    &p,
                                    window,
                                    cx,
                                ))
                            }),
                    )
                    .when(!stripes.side_empty(false) || self.dragging, |d| {
                        d.child(self.stripe(&stripes, false, waiting, &p, cx))
                    }),
            )
            .child(self.status_bar(&config, &env, &p, cx))
            .when_some(find_popup, |d, popup| {
                d.child(
                    div()
                        .id("find-scrim")
                        .absolute()
                        .inset_0()
                        .bg(gpui::black().opacity(0.18))
                        .on_click(cx.listener(|t, _, window, cx| t.close_find(window, cx))),
                )
                .child(
                    h_flex()
                        .absolute()
                        .top(px(36.))
                        .left_0()
                        .right_0()
                        .justify_center()
                        .child(popup),
                )
            })
            .into_any_element()
    }
}

impl EnvTab {
    /// The environment's ⋯ menu, as the design's toolbar has it: stopping
    /// it, its SSH command, resetting it to its template, the web UI's
    /// page, and deleting it; what cannot be taken back is asked first.
    fn env_menu(&self, env: &Environment, web: String, cx: &mut Context<Self>) -> impl IntoElement {
        let (place, id, name) = (self.place.clone(), self.env.clone(), env.name.clone());
        let ssh = self
            .places
            .read(cx)
            .get(&self.place)
            .and_then(|(_, s)| s.me.as_ref())
            .and_then(|me| model::ssh_command(me, env));
        let stoppable = !matches!(env.phase.as_str(), "stopped" | "failed" | "deleting")
            && env.desired != "stopped";
        let stopped = matches!(env.phase.as_str(), "stopped" | "failed");
        let places = self.places.clone();
        gpui_component::button::Button::new("env-more")
            .ghost()
            .small()
            .icon(gpui_component::Icon::empty().path(assets::icon("more")))
            .tooltip("More")
            .dropdown_menu_with_anchor(gpui::Anchor::TopRight, move |menu, _, cx| {
                crate::ui::modal(cx);
                let act = |action: model::Action| {
                    let (places, place, id) = (places.clone(), place.clone(), id.clone());
                    move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
                        places.update(cx, |pl, cx| pl.act(&place, &id, action, cx));
                    }
                };
                let ask = |title: String, body: &'static str, ok: &'static str, action| {
                    let act = act(action);
                    move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                        let act = act.clone();
                        crate::treeops::confirm(&title, body, ok, window, cx, move |window, cx| {
                            act(&gpui::ClickEvent::default(), window, cx)
                        });
                    }
                };
                let copy_ssh = ssh.clone();
                let web = web.clone();
                menu.item(
                    PopupMenuItem::new("Stop")
                        .disabled(!stoppable)
                        .on_click(act(model::Action::Stop)),
                )
                .item(
                    PopupMenuItem::new("Copy SSH Command")
                        .disabled(ssh.is_none())
                        .on_click(move |_, window, cx| {
                            if let Some(c) = &copy_ssh {
                                crate::treeops::copy(c.clone(), cx);
                                use gpui_component::WindowExt;
                                window.push_notification(
                                    gpui_component::notification::Notification::info(format!(
                                        "Copied: {c}"
                                    )),
                                    cx,
                                );
                            }
                        }),
                )
                .item(
                    PopupMenuItem::new("Reset to Template…")
                        .disabled(!stopped)
                        .on_click(ask(
                            format!("Reset {name} to its template?"),
                            "It takes its template's current settings; its disks and everything on them are kept.",
                            "Reset",
                            model::Action::Reset,
                        )),
                )
                .item(
                    PopupMenuItem::new("Open in Browser")
                        .on_click(move |_, _, cx| cx.open_url(&web)),
                )
                .separator()
                .item(PopupMenuItem::new("Delete…").on_click(ask(
                    format!("Delete {name}?"),
                    "Its disks and everything on them go with it. This cannot be undone.",
                    "Delete",
                    model::Action::Delete,
                )))
            })
    }

    fn toolbar(
        &self,
        config: &PlaceConfig,
        env: &Environment,
        card: &Card,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let pill = |p: &Palette| {
            h_flex()
                .gap(px(5.))
                .px(px(8.))
                .py(px(1.))
                .rounded(px(10.))
                .border_1()
                .border_color(p.border)
                .text_size(px(10.5))
                .text_color(p.muted)
        };
        let where_ = match env.phase.as_str() {
            "running" => format!("Running on {}", config.name),
            "suspended" => format!("Suspended on {}", config.name),
            "stopped" => format!("Stopped on {}", config.name),
            "failed" => format!("Failed on {}", config.name),
            phase => format!("{} on {}", capital(phase), config.name),
        };
        let phase_tone = match env.phase.as_str() {
            "running" => Tone::Good,
            "suspended" => Tone::Ring,
            "stopped" => Tone::Idle,
            "failed" => Tone::Bad,
            _ => Tone::Busy,
        };
        let (place, id) = (self.place.clone(), self.env.clone());
        let task_text = env
            .task
            .as_ref()
            .map(|t| t.text.clone())
            .unwrap_or_default();
        let lifecycle = match env.phase.as_str() {
            "running" => Some(("Suspend", model::Action::Suspend)),
            "suspended" => Some(("Resume", model::Action::Resume)),
            "stopped" | "failed" => Some(("Start", model::Action::Start)),
            _ => None,
        };
        let web = format!("{}/environments/{}", config.url, env.id);

        let title =
            match &self.task_edit {
                Some(input) => h_flex()
                    .gap(px(6.))
                    .w(px(420.))
                    .child(div().flex_1().child(Input::new(input)))
                    .child(
                        primary("task-save", "Save", p)
                            .on_click(cx.listener(|t, _, _, cx| t.save_task(cx))),
                    )
                    .child(button("task-cancel", "Cancel", p).on_click(cx.listener(
                        |t, _, _, cx| {
                            t.task_edit = None;
                            cx.notify();
                        },
                    )))
                    .when(env.task.is_some(), |d| {
                        let (place, id) = (place.clone(), id.clone());
                        d.child(button("task-done", "Mark done", p).on_click(cx.listener(
                            move |t, _, _, cx| {
                                t.task_edit = None;
                                t.places
                                    .update(cx, |pl, cx| pl.set_task(&place, &id, "", cx));
                            },
                        )))
                    }),
                None => h_flex()
                    .gap(px(6.))
                    .min_w_0()
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(card.title.clone()),
                    )
                    .when(card.agent_task, |d| d.child(spark(p)))
                    .when(card.title != card.name, |d| {
                        d.child(
                            div()
                                .text_size(px(11.5))
                                .text_color(p.muted)
                                .child(card.name.clone()),
                        )
                    })
                    .child(
                        div()
                            .id("task-edit")
                            .text_color(p.muted)
                            .cursor_pointer()
                            .child("✎")
                            .on_click(cx.listener(move |t, _, window, cx| {
                                t.edit_task(&task_text, window, cx)
                            })),
                    ),
            };

        h_flex()
            .gap(px(10.))
            .px(px(12.))
            .py(px(8.))
            .bg(p.surface)
            .border_b_1()
            .border_color(p.border)
            .child(title)
            .child(pill(p).child(dot(phase_tone, p)).child(where_))
            .when(env.phase == "running", |d| {
                d.when_some(self.branches.clone(), |d, b| d.child(b))
            })
            .child(div().flex_1())
            .when_some(lifecycle, |d, (label, action)| {
                let (place, id) = (place.clone(), id.clone());
                d.child(
                    button("lifecycle", label, p).on_click(cx.listener(move |t, _, _, cx| {
                        t.places
                            .update(cx, |pl, cx| pl.act(&place, &id, action, cx));
                    })),
                )
            })
            .child(self.env_menu(env, web, cx))
    }

    fn stripe(
        &self,
        stripes: &Stripes,
        left: bool,
        waiting: usize,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let [top, mid, foot] = Group::of_side(left);
        let accent = p.accent;
        let tab = cx.entity().downgrade();
        // An icon: clicked, its window opens or closes; dragged, it moves,
        // landing before the icon it is let go on, which shows a line above
        // it meanwhile.
        let icon = |t: Tool, group: Group, cx: &mut Context<Self>| {
            let on = self.is_open(t);
            let badge = (t == Tool::Agents && waiting > 0).then(|| waiting.to_string());
            let hover = p.surface_2;
            let tab = tab.clone();
            div()
                .id(SharedString::from(format!("stripe-drop-{}", t.name())))
                .pt(px(2.))
                .border_t_2()
                .border_color(gpui::transparent_black())
                .drag_over::<DraggedTool>(move |s, _, _, _| s.border_color(accent))
                .on_drop(cx.listener(move |this, d: &DraggedTool, _, cx| {
                    this.move_tool(d.0, group, Some(t), cx)
                }))
                .child(
                    div()
                        .id(SharedString::from(format!("stripe-{}", t.name())))
                        .debug_selector(move || format!("stripe-{}", t.name()))
                        .relative()
                        .size(px(28.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .when(on, |d| d.bg(p.accent_soft))
                        .when(!on, move |d| d.hover(move |s| s.bg(hover)))
                        .child(
                            svg()
                                .path(assets::icon(t.icon()))
                                .size(px(16.))
                                .text_color(if on { p.accent } else { p.muted }),
                        )
                        .when_some(badge, |d, b| {
                            d.child(
                                div()
                                    .absolute()
                                    .right(px(-3.))
                                    .top(px(-3.))
                                    .px(px(4.))
                                    .rounded(px(7.))
                                    .bg(p.accent)
                                    .text_color(p.on_accent)
                                    .text_size(px(9.))
                                    .line_height(px(14.))
                                    .child(b),
                            )
                        })
                        .tooltip(move |window, cx| {
                            let label = match shortcut(t) {
                                Some(k) => format!("{} {k}", t.name()),
                                None => t.name().to_string(),
                            };
                            gpui_component::tooltip::Tooltip::new(label).build(window, cx)
                        })
                        .on_drag(DraggedTool(t), move |d, _, _, cx| {
                            let _ = tab.update(cx, |this, cx| {
                                this.dragging = true;
                                cx.notify();
                            });
                            cx.new(|_| d.clone())
                        })
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle(t, cx))),
                )
        };
        // The space after a group's icons: an icon let go there goes to the
        // group's end.
        let tail = |group: Group, grow: bool, cx: &mut Context<Self>| {
            div()
                .id(SharedString::from(format!("stripe-end-{group:?}")))
                .debug_selector(move || format!("stripe-end-{group:?}"))
                .w(px(28.))
                .when(grow, |d| d.flex_1().min_h(px(10.)))
                .when(!grow, |d| d.h(px(10.)).flex_none())
                .border_t_2()
                .border_color(gpui::transparent_black())
                .drag_over::<DraggedTool>(move |s, _, _, _| s.border_color(accent))
                .on_drop(cx.listener(move |this, d: &DraggedTool, _, cx| {
                    this.move_tool(d.0, group, None, cx)
                }))
        };
        let mut s = v_flex()
            .w(px(38.))
            .flex_none()
            .items_center()
            .py(px(4.))
            .bg(p.sidebar)
            .border_color(p.border)
            .when(left, |d| d.border_r_1())
            .when(!left, |d| d.border_l_1());
        for t in stripes.group(top) {
            s = s.child(icon(*t, top, cx));
        }
        s = s
            .child(tail(top, false, cx))
            .child(div().w(px(20.)).h(px(1.)).flex_none().bg(p.border_strong));
        for t in stripes.group(mid) {
            s = s.child(icon(*t, mid, cx));
        }
        s = s.child(tail(mid, true, cx));
        for t in stripes.group(foot) {
            s = s.child(icon(*t, foot, cx));
        }
        s.child(tail(foot, false, cx))
    }

    /// A side's tool windows: the one docked there, and the one below the
    /// line under it, split by a handle when both are open.
    #[allow(clippy::too_many_arguments)]
    fn side(
        &self,
        left: bool,
        top: Option<Tool>,
        below: Option<Tool>,
        env: &Environment,
        card: &Card,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let sizes = Layout::sizes(cx);
        let (edge, split, edge_id, split_id) = if left {
            (Edge::Left, Edge::LeftSplit, "left-edge", "left-split")
        } else {
            (Edge::Right, Edge::RightSplit, "right-edge", "right-split")
        };
        let both = top.is_some() && below.is_some();
        v_flex()
            .relative()
            .w(sizes.get(edge))
            .flex_none()
            .h_full()
            .bg(p.surface)
            .on_drag_move(cx.listener(move |_, e: &DragMoveEvent<Edge>, _, cx| {
                if *e.drag(cx) == split {
                    let f = Sizes::dragged(split, e.event.position, e.bounds, 0.);
                    Layout::update(cx, |s| s.set(split, f));
                }
            }))
            .child(layout::handle(edge_id, edge))
            .when_some(top, |d, t| {
                d.child(
                    div()
                        .relative()
                        .w_full()
                        .min_h_0()
                        .overflow_hidden()
                        .when(both, |d| d.h(relative(sizes.fraction(split))).flex_none())
                        .when(!both, |d| d.flex_1())
                        .child(self.tool_window(t, env, card, p, cx))
                        .when(both, |d| d.child(layout::handle(split_id, split))),
                )
            })
            .when_some(below, |d, t| {
                d.child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .overflow_hidden()
                        .when(top.is_some(), |d| d.border_t_1().border_color(p.border))
                        .child(self.tool_window(t, env, card, p, cx)),
                )
            })
    }

    fn tool_window(
        &self,
        t: Tool,
        env: &Environment,
        card: &Card,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let trees = matches!(t, Tool::Project | Tool::Filesystem);
        let settings = crate::settings::Settings::get(cx);
        let (always, hidden, ignored) = (
            settings.select_opened,
            settings.show_hidden,
            settings.show_ignored,
        );
        let hover = p.surface_2;
        let head = h_flex()
            .h(px(20.))
            .flex_none()
            .gap(px(8.))
            .mb(px(8.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(t.name())
            .when_some(self.summary(t, cx), |d, about| {
                d.child(
                    div()
                        .font_weight(FontWeight::NORMAL)
                        .text_color(p.muted)
                        .child(about),
                )
            })
            .child(div().flex_1())
            .when(trees, |d| {
                d.child(
                    div()
                        .id(SharedString::from(format!("select-opened-{}", t.name())))
                        .size(px(20.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .cursor_pointer()
                        .hover(move |d| d.bg(hover))
                        .child(
                            svg()
                                .path(assets::icon("locate"))
                                .size(px(14.))
                                .text_color(p.muted),
                        )
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new("Select Opened File")
                                .build(window, cx)
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.select_opened(true, cx))),
                )
                .child(
                    gpui_component::button::Button::new(SharedString::from(format!(
                        "options-{}",
                        t.name()
                    )))
                    .ghost()
                    .xsmall()
                    .icon(gpui_component::Icon::empty().path(assets::icon("more")))
                    .dropdown_menu_with_anchor(
                        gpui::Anchor::TopRight,
                        move |menu, _, cx| {
                            crate::ui::modal(cx);
                            menu.item(
                                PopupMenuItem::new("Always Select Opened File")
                                    .checked(always)
                                    .on_click(|_, _, cx| {
                                        crate::settings::Settings::update(cx, |s| {
                                            s.select_opened = !s.select_opened
                                        })
                                    }),
                            )
                            .separator()
                            .item(
                                PopupMenuItem::new("Show Hidden Files")
                                    .checked(hidden)
                                    .on_click(|_, _, cx| {
                                        crate::settings::Settings::update(cx, |s| {
                                            s.show_hidden = !s.show_hidden
                                        })
                                    }),
                            )
                            .item(
                                PopupMenuItem::new("Show Ignored Files")
                                    .checked(ignored)
                                    .on_click(|_, _, cx| {
                                        crate::settings::Settings::update(cx, |s| {
                                            s.show_ignored = !s.show_ignored
                                        })
                                    }),
                            )
                        },
                    ),
                )
            })
            .child(
                div()
                    .id(SharedString::from(format!("hide-{}", t.name())))
                    .text_color(p.muted)
                    .font_weight(FontWeight::NORMAL)
                    .cursor_pointer()
                    .child("–")
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(t, cx))),
            );
        let body = match t {
            Tool::Agents => self.agents(env, card, p, cx).into_any_element(),
            Tool::Project | Tool::Filesystem | Tool::Commit | Tool::Structure
                if env.phase != "running" =>
            {
                div()
                    .text_size(px(11.5))
                    .text_color(p.muted)
                    .child("The environment is not running.")
                    .into_any_element()
            }
            Tool::Filesystem if self.filesystem.is_some() => self
                .filesystem
                .clone()
                .map(|v| v.into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            Tool::Project if self.project.is_some() => self
                .project
                .clone()
                .map(|v| v.into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            Tool::Commit if self.commit.is_some() => self
                .commit
                .clone()
                .map(|v| v.into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            Tool::Structure if self.structure.is_some() => self
                .structure
                .clone()
                .map(|v| v.into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            t if t.needs_running() && env.phase != "running" => div()
                .text_size(px(11.5))
                .text_color(p.muted)
                .child("The environment is not running.")
                .into_any_element(),
            t if self.panel_view(t).is_some() => self
                .panel_view(t)
                .unwrap_or_else(|| div().into_any_element()),
            t => div()
                .text_size(px(11.5))
                .text_color(p.muted)
                .child(t.waiting())
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .p(px(8.))
            .min_h_0()
            .text_size(px(12.))
            .child(head)
            .child(v_flex().flex_1().min_h_0().child(body))
    }

    fn agents(
        &self,
        env: &Environment,
        card: &Card,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let now = chrono::Utc::now();
        let mut list = v_flex().gap(px(4.));
        let mut sessions: Vec<&AgentSession> =
            env.agents.iter().filter(|a| a.state != "ended").collect();
        sessions.sort_by_key(|a| !a.needs_you);
        if sessions.is_empty() {
            return list.child(
                div()
                    .text_size(px(11.5))
                    .text_color(p.muted)
                    .child("No coding agent is running here. Start Claude Code in a terminal and it shows here."),
            );
        }
        for a in sessions {
            let asking = a.state == "asking";
            let lead = card.session.as_deref() == Some(a.id.as_str());
            let state_tone = agent_tone(a);
            list = list.child(
                v_flex()
                    .gap(px(4.))
                    .px(px(6.))
                    .py(px(7.))
                    .rounded(px(6.))
                    .text_size(px(11.5))
                    .when(lead && !asking, |d| d.bg(p.accent_soft))
                    .when(asking, |d| {
                        d.bg(p.warn_bg).border_1().border_color(p.warn_border)
                    })
                    .when(!lead && !asking, |d| {
                        d.border_b_1().border_color(p.border).rounded_none()
                    })
                    .child(
                        h_flex()
                            .gap(px(6.))
                            .child(dot(state_tone, p))
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(a.agent.clone()),
                            )
                            .child(
                                div()
                                    .ml_auto()
                                    .text_size(px(10.5))
                                    .text_color(p.muted)
                                    .child(format!("{} · {}", a.state, model::age(a.since, now))),
                            ),
                    )
                    .when_some(env.task.as_ref(), |d, t| {
                        d.child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(t.text.clone()),
                        )
                    })
                    .when(asking, |d| {
                        d.when_some(a.step.clone(), |d, s| {
                            d.child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(format!("{s}?")),
                            )
                        })
                        .when_some(a.message.clone(), |d, m| {
                            d.child(div().text_color(p.muted).child(m))
                        })
                        .when(
                            a.terminal.is_some()
                                && a.message
                                    .as_deref()
                                    .is_some_and(|m| m.contains("permission")),
                            |d| {
                                let mut b = h_flex().flex_wrap().gap(px(4.));
                                for (n, (label, key)) in
                                    [("Yes", "1"), ("Yes, and don't ask again", "2"), ("No", "3")]
                                        .into_iter()
                                        .enumerate()
                                {
                                    let agent = a.id.clone();
                                    let id = SharedString::from(format!("answer-{}-{n}", a.id));
                                    let btn = if n == 0 {
                                        primary(id, label, p)
                                    } else {
                                        button(id, label, p)
                                    };
                                    b = b.child(
                                        btn.py(px(1.)).px(px(8.)).text_size(px(11.)).on_click(
                                            cx.listener(move |t, _, window, cx| {
                                                t.answer(&agent, key, window, cx)
                                            }),
                                        ),
                                    );
                                }
                                d.child(b)
                            },
                        )
                        .when(a.terminal.is_none(), |d| {
                            d.child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(p.muted)
                                    .child("Answer it where it runs."),
                            )
                        })
                    })
                    .when_some(a.terminal.clone(), |d, session| {
                        d.child(
                            h_flex().child(
                                button(
                                    SharedString::from(format!("open-{}", a.id)),
                                    "Open session",
                                    p,
                                )
                                .py(px(1.))
                                .px(px(8.))
                                .text_size(px(11.))
                                .on_click(cx.listener(
                                    move |t, _, window, cx| t.open_terminal(&session, window, cx),
                                )),
                            ),
                        )
                    })
                    .when(!asking, |d| {
                        d.when_some(a.step.clone(), |d, s| {
                            d.child(div().text_color(p.muted).child(s))
                        })
                    }),
            );
        }
        list
    }

    /// The editor area: one group of tabs, or two side by side, the one in
    /// use taking what is opened.
    fn editors(&mut self, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.other.is_none() {
            return self.group_view(true, false, p, cx);
        }
        let right = self.using_right();
        let used = self.group_view(true, right, p, cx);
        let other = self.group_view(false, !right, p, cx);
        let (l, r) = if right { (other, used) } else { (used, other) };
        let down = self.split_down;
        let pane =
            |side: &'static str, body: gpui::AnyElement, right: bool, cx: &mut Context<Self>| {
                v_flex()
                    .id(side)
                    .debug_selector(move || side.to_string())
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .when(down, |d| d.w_full())
                    .when(!down, |d| d.h_full())
                    // A click anywhere in a group makes it the one in use.
                    .capture_any_mouse_down(cx.listener(move |t, _, _, cx| t.use_side(right, cx)))
                    .child(body)
            };
        let first = pane("editors-left", l, false, cx);
        let second = pane("editors-right", r, true, cx);
        // Split Down: the second group under the first.
        if down {
            return v_flex()
                .flex_1()
                .min_h_0()
                .size_full()
                .child(first)
                .child(div().h(px(1.)).w_full().flex_none().bg(p.border))
                .child(second)
                .into_any_element();
        }
        h_flex()
            .flex_1()
            .min_h_0()
            .size_full()
            .child(first)
            .child(div().w(px(1.)).h_full().flex_none().bg(p.border))
            .child(second)
            .into_any_element()
    }

    /// A group's row of tabs and the tab shown: the group in use, or the
    /// other; on the right or not.
    fn group_view(
        &mut self,
        used: bool,
        right: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let split = self.other.is_some();
        let (files, file, scroll) = {
            let g = if used { None } else { self.other.as_mut() };
            // The tab shown is scrolled into view whenever it, or the tabs,
            // change.
            let (files, file, scroll, in_view) = match g {
                Some(g) => (&g.files, g.file, &g.tab_scroll, &mut g.tab_in_view),
                None => (
                    &self.files,
                    self.file,
                    &self.tab_scroll,
                    &mut self.tab_in_view,
                ),
            };
            let shown = (file, files.len());
            if *in_view != shown {
                *in_view = shown;
                if let Some(i) = file {
                    scroll.scroll_to_item(i);
                }
            }
            (files.clone(), file, scroll.clone())
        };
        let Some(active) = file.and_then(|i| files.get(i)).cloned() else {
            return self.no_file(p).into_any_element();
        };
        let side = if right { "r" } else { "l" };
        let selector = if right { "right-tab" } else { "file-tab" };
        // One row of tabs, scrolled sideways by the wheel when they do not
        // fit, with a list of them all at its end.
        let mut tabs = h_flex()
            .id(SharedString::from(format!("file-tabs-{side}")))
            .flex_1()
            .min_w_0()
            .overflow_x_scroll()
            .track_scroll(&scroll);
        for (i, f) in files.iter().enumerate() {
            let on = file == Some(i);
            let path = f.path(cx);
            let name = f.title(cx);
            let pinned = self.pinned.contains(&f.entity_id());
            let hover = p.surface_2;
            // The shown tab of the group not in use is drawn quieter.
            let strong = on && (used || !split);
            let accent = p.accent;
            tabs = tabs.child(
                h_flex()
                    .id(SharedString::from(format!("file-tab-{side}{i}")))
                    .debug_selector(move || format!("{selector}-{i}"))
                    .flex_none()
                    .gap(px(6.))
                    .px(px(12.))
                    .py(px(5.))
                    .border_r_1()
                    .border_color(p.border)
                    .cursor_pointer()
                    .when(on, |d| d.bg(p.bg))
                    .when(strong, |d| d.text_color(p.text))
                    .when(!strong, |d| d.text_color(p.muted))
                    .when(!on, move |d| d.hover(move |s| s.bg(hover)))
                    // Dragged, it moves: before the tab it is let go on.
                    .on_drag(
                        DraggedTab {
                            right,
                            i,
                            title: name.clone(),
                        },
                        {
                            let tab = cx.entity().downgrade();
                            move |d, _, _, cx| {
                                let _ = tab.update(cx, |t, cx| {
                                    t.dragging_tab = true;
                                    cx.notify();
                                });
                                cx.new(|_| d.clone())
                            }
                        },
                    )
                    .drag_over::<DraggedTab>(move |s, _, _, _| s.border_l_2().border_color(accent))
                    .on_drop(cx.listener(move |t, d: &DraggedTab, window, cx| {
                        t.move_tab(d.right, d.i, right, Some(i), window, cx)
                    }))
                    .child(div().max_w(px(220.)).truncate().child(name))
                    .child(if pinned {
                        // A pinned tab shows its pin, which unpins it.
                        div()
                            .id(SharedString::from(format!("file-pin-{side}{i}")))
                            .debug_selector(move || format!("file-pin-{i}"))
                            .child(
                                svg()
                                    .path(assets::icon("pin"))
                                    .size(px(11.))
                                    .text_color(p.muted),
                            )
                            .on_click(cx.listener(move |t, _: &gpui::ClickEvent, _, cx| {
                                cx.stop_propagation();
                                t.use_side(right, cx);
                                t.pin_tab(i, false, cx);
                            }))
                            .into_any_element()
                    } else {
                        div()
                            .id(SharedString::from(format!("file-close-{side}{i}")))
                            .text_color(p.muted)
                            .child("×")
                            .on_click(cx.listener(move |t, _: &gpui::ClickEvent, _, cx| {
                                cx.stop_propagation();
                                t.use_side(right, cx);
                                t.close_file(i, cx);
                            }))
                            .into_any_element()
                    })
                    .tooltip({
                        let path = path.clone();
                        move |window, cx| {
                            gpui_component::tooltip::Tooltip::new(path.clone()).build(window, cx)
                        }
                    })
                    .on_click(cx.listener(move |t, _, window, cx| {
                        t.use_side(right, cx);
                        t.show_file(i, window, cx)
                    }))
                    // As in IntelliJ, the middle button closes a tab.
                    .on_mouse_down(
                        gpui::MouseButton::Middle,
                        cx.listener(move |t, _, _, cx| {
                            t.use_side(right, cx);
                            t.close_file(i, cx)
                        }),
                    )
                    .context_menu({
                        let (count, tab) = (files.len(), cx.entity().downgrade());
                        let rel = self
                            .workspace
                            .as_ref()
                            .and_then(|w| {
                                let w = w.read(cx);
                                let root =
                                    w.project_of(&path)?.root.trim_end_matches('/').to_string();
                                path.strip_prefix(&format!("{root}/")).map(str::to_string)
                            })
                            .unwrap_or_else(|| path.clone());
                        move |menu, _, cx| {
                            crate::ui::modal(cx);
                            Self::tab_menu(
                                menu,
                                TabAt {
                                    i,
                                    count,
                                    right,
                                    split,
                                    pinned,
                                },
                                path.clone(),
                                rel.clone(),
                                tab.clone(),
                            )
                        }
                    }),
            );
        }
        let overflowing = scroll.max_offset().x > px(0.);
        let all: Vec<(usize, String)> = files
            .iter()
            .enumerate()
            .map(|(i, f)| (i, f.title(cx).to_string()))
            .collect();
        let tab = cx.entity().downgrade();
        let soft = p.accent_soft;
        let bar = h_flex()
            .flex_none()
            .border_b_1()
            .border_color(p.border)
            .text_size(px(11.5))
            // Let go on the row past the tabs, a tab goes to the end.
            .on_drop(cx.listener(move |t, d: &DraggedTab, window, cx| {
                t.move_tab(d.right, d.i, right, None, window, cx)
            }))
            .child(tabs)
            .when(overflowing, |d| {
                d.child(
                    div().flex_none().px(px(4.)).child(
                        gpui_component::button::Button::new(SharedString::from(format!(
                            "file-tabs-all-{side}"
                        )))
                        .ghost()
                        .xsmall()
                        .icon(gpui_component::Icon::empty().path(assets::icon("chevron-down")))
                        .tooltip("Every open file")
                        .dropdown_menu_with_anchor(
                            gpui::Anchor::TopRight,
                            move |mut menu, _, cx| {
                                crate::ui::modal(cx);
                                for (i, title) in &all {
                                    let (i, tab) = (*i, tab.clone());
                                    menu = menu.item(
                                        PopupMenuItem::new(title.clone())
                                            .checked(file == Some(i))
                                            .on_click(move |_, window, cx| {
                                                let _ = tab.update(cx, |t, cx| {
                                                    t.use_side(right, cx);
                                                    t.show_file(i, window, cx)
                                                });
                                            }),
                                    );
                                }
                                menu
                            },
                        ),
                    ),
                )
            });
        let dragging = self.dragging_tab;
        let body = div()
            .relative()
            .flex_1()
            .min_h_0()
            // A terminal moved here: its Close Tab and End Session are this
            // tab's.
            .on_action(cx.listener(|t, _: &termview::CloseSession, window, cx| {
                if let Some(i) = t.focused_editor_terminal(window, cx) {
                    t.close_file(i, cx);
                }
            }))
            .on_action(cx.listener(|t, _: &termview::EndSession, window, cx| {
                if let Some(i) = t.focused_editor_terminal(window, cx) {
                    t.end_editor_terminal(i, window, cx);
                }
            }))
            .child(active.element())
            // While a tab is dragged: on the other group's editor it moves
            // there; with one group, on the right half it splits right.
            .when(dragging && split && !used, |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .drag_over::<DraggedTab>(move |s, _, _, _| s.bg(soft))
                        .on_drop(cx.listener(move |t, d: &DraggedTab, window, cx| {
                            t.move_tab(d.right, d.i, right, None, window, cx)
                        })),
                )
            })
            .when(dragging && !split, |d| {
                d.child(
                    div()
                        .id("split-zone")
                        .debug_selector(|| "split-zone".into())
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right_0()
                        .w(gpui::relative(0.5))
                        .drag_over::<DraggedTab>(move |s, _, _, _| s.bg(soft))
                        .on_drop(cx.listener(move |t, d: &DraggedTab, window, cx| {
                            t.move_tab(d.right, d.i, true, None, window, cx)
                        })),
                )
            });
        v_flex()
            .flex_1()
            .min_h_0()
            .child(bar)
            .child(body)
            .into_any_element()
    }

    fn no_file(&self, p: &Palette) -> impl IntoElement {
        let mut keys = v_flex().gap(px(6.)).items_center();
        for t in [Tool::Project, Tool::Terminal, Tool::Commit, Tool::Git] {
            if let Some(k) = shortcut(t) {
                keys = keys.child(
                    h_flex()
                        .gap(px(10.))
                        .child(div().w(px(110.)).text_right().child(t.name()))
                        .child(div().w(px(70.)).text_color(p.accent).child(k)),
                );
            }
        }
        v_flex()
            .flex_1()
            .min_h_0()
            .items_center()
            .justify_center()
            .gap(px(14.))
            .text_color(p.muted)
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("No file open"),
            )
            .child(keys)
    }

    fn bottom(
        &mut self,
        t: Tool,
        height: gpui::Pixels,
        env: &Environment,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if t == Tool::Terminal {
            return self
                .terminal_window(height, env, p, window, cx)
                .into_any_element();
        }
        let view = self.panel_view(t);
        let body = match view {
            Some(view) if env.phase == "running" || !t.needs_running() => {
                div().flex_1().min_h_0().child(view).into_any_element()
            }
            _ if env.phase != "running" && t.needs_running() => div()
                .px(px(10.))
                .py(px(6.))
                .text_size(px(11.))
                .text_color(p.muted)
                .child("The environment is not running.")
                .into_any_element(),
            _ => div()
                .px(px(10.))
                .py(px(6.))
                .text_size(px(11.))
                .text_color(p.muted)
                .child(t.waiting())
                .into_any_element(),
        };
        v_flex()
            .relative()
            .h(height)
            .flex_none()
            .bg(p.surface)
            .child(layout::handle("bottom-edge", Edge::Bottom))
            .child(
                h_flex()
                    .gap(px(6.))
                    .px(px(10.))
                    .py(px(5.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(p.text)
                    .border_b_1()
                    .border_color(p.border)
                    .child(t.name())
                    .when_some(self.summary(t, cx), |d, about| {
                        d.child(
                            div()
                                .font_weight(FontWeight::NORMAL)
                                .text_color(p.muted)
                                .child(about),
                        )
                    }),
            )
            .child(body)
            .into_any_element()
    }

    /// The view a tool window shows that is the same wherever it docks.
    fn panel_view(&self, t: Tool) -> Option<gpui::AnyElement> {
        match t {
            Tool::Git => self.git.clone().map(|v| v.into_any_element()),
            Tool::Find => self.find.clone().map(|v| v.into_any_element()),
            Tool::Problems => self.problems.clone().map(|v| v.into_any_element()),
            Tool::Docker => self.docker.clone().map(|v| v.into_any_element()),
            Tool::Log => self.logs.clone().map(|v| v.into_any_element()),
            Tool::Ports => self.ports.clone().map(|v| v.into_any_element()),
            _ => None,
        }
    }

    /// What a tool window's title says beside its name.
    fn summary(&self, t: Tool, cx: &App) -> Option<String> {
        match t {
            Tool::Docker => self.docker.as_ref()?.read(cx).summary(),
            Tool::Ports => self.ports.as_ref()?.read(cx).summary(),
            _ => None,
        }
    }

    /// The Terminal tool window: a tab per session, then the one shown.
    fn terminal_window(
        &mut self,
        height: gpui::Pixels,
        env: &Environment,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let running = env.phase == "running";
        if running {
            self.list_terminals(window, cx);
        }
        let mut tabs = h_flex().gap(px(2.)).min_w_0().overflow_hidden();
        for (i, tab) in self.terms.tabs.iter().enumerate() {
            let on = self.terms.active == Some(i);
            let id = tab.id(cx);
            let agent = env
                .agents
                .iter()
                .find(|a| a.terminal.is_some() && a.terminal == id && a.state != "ended");
            let hover = p.term_tab;
            tabs = tabs.child(
                h_flex()
                    .id(("term-tab", i))
                    .debug_selector(move || format!("term-tab-{i}"))
                    .gap(px(5.))
                    .px(px(10.))
                    .py(px(2.))
                    .rounded_t(px(5.))
                    .cursor_pointer()
                    .text_color(if on { p.term_fg } else { p.idle })
                    .when(on, |d| d.bg(p.term_tab))
                    .when(!on, move |d| d.hover(move |s| s.bg(hover.opacity(0.6))))
                    .when_some(agent, |d, a| d.child(dot(agent_tone(a), p).size(px(7.))))
                    .child(div().max_w(px(160.)).truncate().child(tab.label(cx)))
                    .child(
                        div()
                            .id(("term-close", i))
                            .text_color(p.idle)
                            .child("×")
                            .on_click(cx.listener(move |t, _: &gpui::ClickEvent, _, cx| {
                                cx.stop_propagation();
                                t.close_terminal_tab(i, cx);
                            })),
                    )
                    .on_click(
                        cx.listener(move |t, _, window, cx| t.select_terminal(i, true, window, cx)),
                    )
                    // A right-click shows the tab, so it is clear what the
                    // menu is about.
                    .on_mouse_down(
                        gpui::MouseButton::Right,
                        cx.listener(move |t, _, window, cx| {
                            t.select_terminal(i, false, window, cx)
                        }),
                    )
                    .context_menu({
                        let me = cx.entity().downgrade();
                        let only = self.terms.tabs.len() == 1;
                        move |menu, _, cx| {
                            crate::ui::modal(cx);
                            terminal_tab_menu(menu, me.clone(), i, only)
                        }
                    }),
            );
        }
        let menu = self.terms.menu.clone();
        let body = match self
            .terms
            .active
            .and_then(|i| self.terms.tabs.get(i))
            .and_then(|t| t.view.clone())
        {
            Some(v) if running => v.into_any_element(),
            _ => div()
                .px(px(10.))
                .py(px(6.))
                .text_size(px(11.))
                .text_color(p.idle)
                .child(if running {
                    "Starting a terminal…"
                } else {
                    "The environment is not running."
                })
                .into_any_element(),
        };
        v_flex()
            .relative()
            .h(height)
            .flex_none()
            .bg(p.term_bg)
            // The session commands from a terminal's menu and keys.
            .on_action(
                cx.listener(|t, _: &termview::NewSession, window, cx| t.new_terminal(window, cx)),
            )
            .on_action(cx.listener(|t, _: &termview::CloseSession, window, cx| {
                t.close_shown_terminal(window, cx)
            }))
            .on_action(cx.listener(|t, _: &termview::EndSession, window, cx| {
                if let Some(i) = t.terms.active {
                    t.end_terminal(i, window, cx)
                }
            }))
            .child(layout::handle("bottom-edge", Edge::Bottom))
            .child(
                h_flex()
                    .gap(px(2.))
                    .px(px(6.))
                    .pt(px(4.))
                    .text_size(px(11.))
                    .child(
                        div()
                            .pl(px(4.))
                            .pr(px(10.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.term_fg)
                            .child("Terminal"),
                    )
                    .child(tabs)
                    .when(running, |d| {
                        d.child(
                            div()
                                .id("term-new")
                                .px(px(6.))
                                .text_color(p.idle)
                                .cursor_pointer()
                                .child("+")
                                .on_click(
                                    cx.listener(|t, _, window, cx| t.new_terminal(window, cx)),
                                ),
                        )
                        .child(
                            div()
                                .id("term-more")
                                .px(px(4.))
                                .text_color(p.idle)
                                .cursor_pointer()
                                .child("▾")
                                .on_click(cx.listener(|t, _, window, cx| {
                                    t.toggle_terminal_menu(window, cx)
                                })),
                        )
                    }),
            )
            .child(div().flex_1().min_h_0().bg(p.term_tab).child(body))
            .when_some(menu, |d, list| {
                let mut m = v_flex()
                    .absolute()
                    .top(px(26.))
                    .left(px(80.))
                    .min_w(px(240.))
                    .p(px(4.))
                    .rounded(px(6.))
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.border)
                    .text_size(px(12.))
                    .text_color(p.text)
                    .child(
                        div()
                            .id("term-menu-new")
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(p.surface_2))
                            .child("New session")
                            .on_click(cx.listener(|t, _, window, cx| t.new_terminal(window, cx))),
                    );
                if list.is_empty() {
                    m = m.child(
                        div()
                            .px(px(8.))
                            .py(px(4.))
                            .text_color(p.muted)
                            .child("No other sessions"),
                    );
                }
                for (i, s) in list.into_iter().enumerate() {
                    let id = s.id.clone();
                    let label = if s.title.is_empty() {
                        s.id.clone()
                    } else {
                        s.title.clone()
                    };
                    m = m.child(
                        div()
                            .id(("term-menu", i))
                            .px(px(8.))
                            .py(px(4.))
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|s| s.bg(p.surface_2))
                            .child(label)
                            .on_click(cx.listener(move |t, _, window, cx| {
                                t.open_terminal(&id, window, cx)
                            })),
                    );
                }
                d.child(m)
            })
    }

    fn status_bar(
        &self,
        config: &PlaceConfig,
        env: &Environment,
        p: &Palette,
        cx: &App,
    ) -> impl IntoElement {
        // The file shown: where it is, from its environment's home, the
        // caret's line and column, and its language.
        let file = self.active_file().map(|f| {
            let f = f.read(cx);
            let (line, col) = f.position(cx);
            let path = f.path().to_string();
            let crumbs = path.trim_start_matches('/').replace('/', " › ");
            // Its line separators, encoding and indentation, and its
            // language server, or its language where it has none.
            let (props, indent) = f.file_props();
            let mut about: Vec<String> = vec![format!("{line}:{col}")];
            if let Some(props) = props {
                about.extend(props.separator.clone());
                if let Some(enc) = &props.encoding {
                    about.push(if props.bom {
                        format!("{enc} BOM")
                    } else {
                        enc.clone()
                    });
                }
            }
            about.extend(indent.map(|i| i.to_string()));
            let server = self
                .languages
                .as_ref()
                .and_then(|l| l.read(cx).server_label(f.asked()));
            about.push(server.unwrap_or_else(|| docedit::language(&path).to_string()));
            (crumbs, about)
        });
        // Whether the file shown is on disk.
        let saved = self.active_file().and_then(|f| f.read(cx).saved());
        let size = match (env.cpus, env.memory_mib) {
            (Some(c), Some(m)) => format!("{c} vCPUs · {} GB", (m as f64 / 1024.).round()),
            _ => String::new(),
        };
        h_flex()
            .h(px(24.))
            .flex_none()
            .gap(px(14.))
            .px(px(12.))
            .bg(p.sidebar)
            .border_t_1()
            .border_color(p.border)
            .text_size(px(11.))
            .text_color(p.muted)
            .whitespace_nowrap()
            .child(match &file {
                Some((crumbs, _)) => crumbs.clone(),
                None => format!("{} › {}", config.name, env.name),
            })
            .child(div().flex_1())
            .when_some(saved, |d, s| match s {
                docedit::Saved::Saved => d.child("Saved"),
                docedit::Saved::Saving => d.child("Saving…"),
                docedit::Saved::Failed(e) => d.child(
                    div()
                        .text_color(p.bad)
                        .max_w(px(360.))
                        .truncate()
                        .child(format!("Not saved: {e}")),
                ),
            })
            .when_some(file, |d, (_, about)| d.children(about))
            .when_some(self.latency, |d, ms| {
                let (t, icon) = crate::latency::rating(ms);
                d.child(
                    h_flex()
                        .id("latency")
                        .gap(px(4.))
                        .text_color(tone(t, p))
                        .child(
                            svg()
                                .path(assets::icon(icon))
                                .size(px(12.))
                                .text_color(tone(t, p)),
                        )
                        .child(format!("{} ms", ms.round()))
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new(
                                "How long a request takes to reach the environment and come \
                                 back, through Hangar's server and its worker",
                            )
                            .build(window, cx)
                        }),
                )
            })
            .when_some(env.worker.clone(), |d, w| d.child(format!("on {w}")))
            .child(size)
            .child(div().size(px(6.)).rounded_full().bg(tone(
                if env.phase == "running" {
                    Tone::Good
                } else {
                    Tone::Idle
                },
                p,
            )))
    }
}

fn capital(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default()
}

/// An agent session's dot.
fn agent_tone(a: &AgentSession) -> Tone {
    match a.state.as_str() {
        "asking" => Tone::Wait,
        "finished" if a.needs_you => Tone::Good,
        "working" => Tone::Busy,
        _ => Tone::Idle,
    }
}
