//! The menu bar: the app's own menu (Hide, Quit), Edit for the text fields,
//! Go for Search Everywhere and Find in Files, View for the tool windows,
//! and Window. Each item shows the key it is bound to.

use gpui::{App, KeyBinding, Menu, MenuItem, OsAction, actions};
use gpui_component::input::{
    Copy, Cut, GoToDefinition, Paste, Redo, Replace, Search, SelectAll, ShowCompletions, Undo,
};

use crate::docedit::{
    CommentLine, ContextActions, DeleteLine, DuplicateLine, JoinLines, MoveLineDown, MoveLineUp,
    NextProblem, ParameterInfo, PreviousProblem, Reformat, RollbackChange, SelectAllOccurrences,
    SelectNextOccurrence, StageChange, ToggleSoftWrap, UnselectOccurrence,
};
use crate::envtab::{
    CloseTab, FindInFiles, FindUsages, GoToImplementation, GoToLine, GoToTypeDeclaration,
    HideAllToolWindows, RenameSymbol, ReopenClosedTab, ReplaceInFiles, ToggleCommit, ToggleDocker,
    ToggleGit, ToggleProblems, ToggleProject, ToggleStructure, ToggleTerminal,
};
use crate::shell::{GoToSymbol, OpenSettings, SearchEverywhere};

actions!(app, [Quit, Hide, HideOthers, ShowAll, Minimize, Zoom]);

/// Sets the menu bar, binds its own keys and answers its actions.
pub fn init(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, None),
    ]);
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());
    cx.on_action(|_: &Minimize, cx| {
        if let Some(w) = cx.active_window() {
            let _ = w.update(cx, |_, window, _| window.minimize_window());
        }
    });
    cx.on_action(|_: &Zoom, cx| {
        if let Some(w) = cx.active_window() {
            let _ = w.update(cx, |_, window, _| window.zoom_window());
        }
    });
    let app = crate::channel::Channel::current().name();
    cx.set_menus([
        Menu::new(app).items([
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::action(format!("Hide {app}"), Hide),
            MenuItem::action("Hide Others", HideOthers),
            MenuItem::action("Show All", ShowAll),
            MenuItem::separator(),
            MenuItem::action(format!("Quit {app}"), Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Reopen Closed Tab", ReopenClosedTab),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Find…", Search),
            MenuItem::action("Replace…", Replace),
            MenuItem::separator(),
            MenuItem::action("Add Selection for Next Occurrence", SelectNextOccurrence),
            MenuItem::action("Unselect Occurrence", UnselectOccurrence),
            MenuItem::action("Select All Occurrences", SelectAllOccurrences),
        ]),
        Menu::new("Go").items([
            MenuItem::action("Search Everywhere…", SearchEverywhere),
            MenuItem::action("Symbol…", GoToSymbol),
            MenuItem::action("Line…", GoToLine),
            MenuItem::action("Find in Files…", FindInFiles),
            MenuItem::action("Replace in Files…", ReplaceInFiles),
            MenuItem::action("Definition", GoToDefinition),
            MenuItem::action("Implementation(s)", GoToImplementation),
            MenuItem::action("Type Declaration", GoToTypeDeclaration),
            MenuItem::action("Find Usages", FindUsages),
            MenuItem::separator(),
            MenuItem::action("Next Highlighted Error", NextProblem),
            MenuItem::action("Previous Highlighted Error", PreviousProblem),
        ]),
        Menu::new("Code").items([
            MenuItem::action("Code Completion", ShowCompletions),
            MenuItem::action("Show Context Actions", ContextActions),
            MenuItem::separator(),
            MenuItem::action("Reformat Code", Reformat),
            MenuItem::separator(),
            MenuItem::action("Comment with Line Comment", CommentLine),
            MenuItem::action("Duplicate Line or Selection", DuplicateLine),
            MenuItem::action("Delete Line", DeleteLine),
            MenuItem::action("Join Lines", JoinLines),
            MenuItem::separator(),
            MenuItem::action("Move Line Up", MoveLineUp),
            MenuItem::action("Move Line Down", MoveLineDown),
            MenuItem::separator(),
            MenuItem::action("Rollback Change", RollbackChange),
            MenuItem::action("Stage Change", StageChange),
        ]),
        Menu::new("Refactor").items([MenuItem::action("Rename…", RenameSymbol)]),
        Menu::new("View").items([
            MenuItem::action("Project", ToggleProject),
            MenuItem::action("Commit", ToggleCommit),
            MenuItem::action("Structure", ToggleStructure),
            MenuItem::separator(),
            MenuItem::action("Terminal", ToggleTerminal),
            MenuItem::action("Problems", ToggleProblems),
            MenuItem::action("Git", ToggleGit),
            MenuItem::action("Docker", ToggleDocker),
            MenuItem::separator(),
            MenuItem::action("Hide All Tool Windows", HideAllToolWindows),
            MenuItem::separator(),
            MenuItem::action("Parameter Info", ParameterInfo),
            MenuItem::action("Soft-Wrap", ToggleSoftWrap),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]),
    ]);
}
