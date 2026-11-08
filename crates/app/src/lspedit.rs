//! What a file's language server gives its editor: hover, and Go to
//! Definition (Cmd-click, or F12), asked through Languages. A definition in
//! another file opens it in the editors.

use std::rc::Rc;

use gpui::{App, AppContext, Task, WeakEntity, Window};
use gpui_component::input::{DefinitionProvider, HoverProvider, Rope, RopeExt};
use lsp_types::{GotoDefinitionResponse, Hover, LocationLink};
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
        let Some(l) = self.languages.upgrade() else {
            return Task::ready(Ok(None));
        };
        let position = text.offset_to_position(offset);
        let params = json!({
            "textDocument": { "uri": file_uri(&self.abs) },
            "position": position,
        });
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
