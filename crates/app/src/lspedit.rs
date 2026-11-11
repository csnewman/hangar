//! What a file's language server gives its editor: completions, hover, Go
//! to Definition (Cmd-click, or F12), and quick fixes and the server's other
//! code actions (⌥↩, as IntelliJ's context actions), asked through
//! Languages. A definition in another file opens it in the editors; an
//! action's edits are made wherever they fall.

use std::ops::Range;
use std::rc::Rc;

use gpui::{App, AppContext, Entity, SharedString, Task, WeakEntity, Window};
use gpui_component::input::{
    CodeActionProvider, CompletionProvider, DefinitionProvider, EditorState, HoverProvider, Rope,
    RopeExt,
};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CompletionContext, CompletionResponse,
    GotoDefinitionResponse, Hover, LocationLink, WorkspaceEdit,
};
use serde_json::json;

use crate::languages::{Languages, file_uri};

/// Makes a workspace edit, in every file it touches.
pub type ApplyEdit = Rc<dyn Fn(WorkspaceEdit, &mut Window, &mut App)>;

/// A file's providers: its server, reached through Languages, by the
/// file's absolute path, and what makes a code action's edits.
pub struct Providers {
    pub languages: WeakEntity<Languages>,
    pub abs: String,
    pub apply: ApplyEdit,
}

impl Providers {
    fn ask<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        method: &str,
        text: &Rope,
        offset: usize,
        cx: &mut App,
    ) -> Task<anyhow::Result<Option<T>>> {
        self.ask_with(method, text, offset, serde_json::Map::new(), cx)
    }

    /// Asks the server about the place at offset, with more params.
    fn ask_with<T: serde::de::DeserializeOwned + Send + 'static>(
        &self,
        method: &str,
        text: &Rope,
        offset: usize,
        more: serde_json::Map<String, serde_json::Value>,
        cx: &mut App,
    ) -> Task<anyhow::Result<Option<T>>> {
        let Some(l) = self.languages.upgrade() else {
            return Task::ready(Ok(None));
        };
        let position = text.offset_to_position(offset);
        let mut params = json!({
            "textDocument": { "uri": file_uri(&self.abs) },
            "position": position,
        });
        params.as_object_mut().unwrap().extend(more);
        let answer = l.update(cx, |l, cx| l.request(&self.abs, method, params, cx));
        cx.background_spawn(async move {
            let v = answer.await.map_err(anyhow::Error::msg)?;
            Ok(serde_json::from_value(v)?)
        })
    }
}

impl HoverProvider for Providers {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Option<Hover>>> {
        self.ask("textDocument/hover", text, offset, cx)
    }
}

impl CompletionProvider for Providers {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        trigger: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let mut more = serde_json::Map::new();
        more.insert("context".into(), json!(trigger));
        let asked =
            self.ask_with::<CompletionResponse>("textDocument/completion", text, offset, more, cx);
        cx.background_spawn(
            async move { Ok(asked.await?.unwrap_or(CompletionResponse::Array(vec![]))) },
        )
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, cx: &mut App) -> bool {
        let Some(c) = new_text.chars().last() else {
            return false;
        };
        if c.is_alphanumeric() || c == '_' {
            return true;
        }
        self.languages.upgrade().is_some_and(|l| {
            l.read(cx)
                .triggers(&self.abs)
                .iter()
                .any(|t| new_text.ends_with(t.as_str()))
        })
    }
}

impl DefinitionProvider for Providers {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Vec<LocationLink>>> {
        let asked = self.ask::<GotoDefinitionResponse>("textDocument/definition", text, offset, cx);
        cx.background_spawn(async move { Ok(links(asked.await?)) })
    }
}

impl CodeActionProvider for Providers {
    fn id(&self) -> SharedString {
        "language-server".into()
    }

    /// Asks the server what it can do at the selection, telling it the
    /// problems there, so its fixes for them come first.
    fn code_actions(
        &self,
        state: Entity<EditorState>,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Vec<CodeAction>>> {
        let Some(l) = self.languages.upgrade() else {
            return Task::ready(Ok(vec![]));
        };
        let text = state.read(cx).text().clone();
        let range = lsp_types::Range {
            start: text.offset_to_position(range.start),
            end: text.offset_to_position(range.end),
        };
        let problems: Vec<lsp_types::Diagnostic> = l
            .read(cx)
            .diagnostics_of(&self.abs)
            .iter()
            .filter(|d| d.range.start <= range.end && range.start <= d.range.end)
            .cloned()
            .collect();
        let params = json!({
            "textDocument": { "uri": file_uri(&self.abs) },
            "range": range,
            "context": { "diagnostics": problems, "triggerKind": 1 },
        });
        let answer = l.update(cx, |l, cx| {
            l.request(&self.abs, "textDocument/codeAction", params, cx)
        });
        cx.background_spawn(async move {
            let v = answer.await.map_err(anyhow::Error::msg)?;
            let list: Option<Vec<CodeActionOrCommand>> = serde_json::from_value(v)?;
            Ok(actions(list.unwrap_or_default()))
        })
    }

    /// Does an action: its edits made, asked of the server first when it
    /// left them to be filled in, then its command run by the server.
    fn perform_code_action(
        &self,
        _: Entity<EditorState>,
        action: CodeAction,
        _: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<()>> {
        let Some(l) = self.languages.upgrade() else {
            return Task::ready(Ok(()));
        };
        let (abs, apply) = (self.abs.clone(), self.apply.clone());
        let resolve = (action.edit.is_none() && action.data.is_some()).then(|| {
            l.update(cx, |l, cx| {
                l.request(&abs, "codeAction/resolve", json!(action), cx)
            })
        });
        window.spawn(cx, async move |cx| {
            let action = match resolve {
                Some(r) => {
                    serde_json::from_value::<CodeAction>(r.await.map_err(anyhow::Error::msg)?)?
                }
                None => action,
            };
            if let Some(edit) = action.edit {
                cx.update(|window, cx| apply(edit, window, cx))?;
            }
            if let Some(c) = action.command {
                let params = json!({ "command": c.command, "arguments": c.arguments });
                let run = cx.update(|_, cx| {
                    l.update(cx, |l, cx| {
                        l.request(&abs, "workspace/executeCommand", params, cx)
                    })
                })?;
                run.await.map_err(anyhow::Error::msg)?;
            }
            Ok(())
        })
    }
}

/// A server's code actions as the menu lists them: a bare command as an
/// action that runs it, those the server says cannot be done left out,
/// and its preferred fixes first, then the other quick fixes, then the
/// rest, each in the server's order.
pub fn actions(list: Vec<CodeActionOrCommand>) -> Vec<CodeAction> {
    let mut out: Vec<CodeAction> = list
        .into_iter()
        .map(|a| match a {
            CodeActionOrCommand::CodeAction(a) => a,
            CodeActionOrCommand::Command(c) => CodeAction {
                title: c.title.clone(),
                command: Some(c),
                ..Default::default()
            },
        })
        .filter(|a| a.disabled.is_none())
        .collect();
    let rank = |a: &CodeAction| match (a.is_preferred == Some(true), is_fix(a)) {
        (true, _) => 0,
        (false, true) => 1,
        _ => 2,
    };
    out.sort_by_key(rank);
    out
}

/// Whether an action fixes a problem, as IntelliJ's quick fixes do, rather
/// than changing code that has none.
pub fn is_fix(a: &CodeAction) -> bool {
    a.is_preferred == Some(true)
        || a.kind
            .as_ref()
            .is_some_and(|k| k.as_str().starts_with(CodeActionKind::QUICKFIX.as_str()))
}

/// A definition answer as links, whichever form the server gave it in.
pub fn links(r: Option<GotoDefinitionResponse>) -> Vec<LocationLink> {
    let link = |l: lsp_types::Location| LocationLink {
        origin_selection_range: None,
        target_uri: l.uri,
        target_range: l.range,
        target_selection_range: l.range,
    };
    match r {
        None => vec![],
        Some(GotoDefinitionResponse::Scalar(l)) => vec![link(l)],
        Some(GotoDefinitionResponse::Array(ls)) => ls.into_iter().map(link).collect(),
        Some(GotoDefinitionResponse::Link(ls)) => ls,
    }
}

/// Gives an editor its file's providers, and has a definition elsewhere
/// opened by open: the file's absolute path and the 0-based line and UTF-16
/// column to put the cursor at.
pub fn attach(
    editor: &gpui::Entity<gpui_component::input::EditorState>,
    providers: Providers,
    open: impl Fn(String, u32, u32, &mut Window, &mut App) + 'static,
    cx: &mut App,
) {
    let me = providers.abs.clone();
    let p = Rc::new(providers);
    let open = Rc::new(open);
    editor.update(cx, |e, cx| {
        let lsp = e.lsp_mut();
        lsp.completion_provider = Some(p.clone());
        lsp.hover_provider = Some(p.clone());
        lsp.code_action_providers = vec![p.clone()];
        lsp.definition_provider = Some(p);
        lsp.show_document = Some(Rc::new(move |params, window, cx| {
            let Some(abs) = crate::languages::uri_path(params.uri.as_str()) else {
                return false;
            };
            if abs == me {
                return false;
            }
            let at = params.selection.map(|r| r.start).unwrap_or_default();
            let open = open.clone();
            window.defer(cx, move |window, cx| {
                open(abs, at.line, at.character, window, cx)
            });
            true
        }));
        e.refresh(cx);
    });
}
