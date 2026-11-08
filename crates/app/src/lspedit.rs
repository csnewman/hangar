//! What a file's language server gives its editor: completions, hover, and
//! Go to Definition (Cmd-click, or F12), asked through Languages. A
//! definition in another file opens it in the editors.

use std::rc::Rc;

use gpui::{App, AppContext, Task, WeakEntity, Window};
use gpui_component::input::{CompletionProvider, DefinitionProvider, HoverProvider, Rope, RopeExt};
use lsp_types::{
    CompletionContext, CompletionResponse, GotoDefinitionResponse, Hover, LocationLink,
};
use serde_json::json;

use crate::languages::{Languages, file_uri};

/// A file's providers: its server, reached through Languages, by the
/// file's absolute path.
pub struct Providers {
    pub languages: WeakEntity<Languages>,
    pub abs: String,
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
