//! What the window had open, kept between launches: the environments' tabs
//! in their order and the one shown, and each environment's terminal
//! sessions in the order of their tabs.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use gpui::{App, Global, Task};
use serde::{Deserialize, Serialize};

use crate::shell::Opened;

/// The window's tabs as kept.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Open {
    #[serde(default)]
    pub tabs: Vec<Opened>,
    /// The tab shown; none is Home.
    #[serde(default)]
    pub active: Option<usize>,
    /// Each environment's terminal sessions in its tabs' order, by place
    /// and then environment.
    #[serde(default)]
    pub terminals: BTreeMap<String, BTreeMap<String, Vec<String>>>,
}

/// The tabs kept, and the file they are kept in: none keeps them for this
/// run alone, as tests and snapshots do.
pub struct Kept {
    open: Open,
    file: Option<PathBuf>,
    saving: Option<Task<()>>,
}

impl Global for Kept {}

/// How long the last change waits before the file is written.
const SAVE_AFTER: Duration = Duration::from_millis(500);

impl Kept {
    /// The tabs kept in file, or none.
    pub fn init(file: Option<PathBuf>, cx: &mut App) {
        let open = file
            .as_ref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        cx.set_global(Kept {
            open,
            file,
            saving: None,
        });
    }

    /// The window's tabs as last kept.
    pub fn open(cx: &App) -> Open {
        cx.try_global::<Kept>()
            .map(|k| k.open.clone())
            .unwrap_or_default()
    }

    /// Keeps the window's tabs and the one shown.
    pub fn set_tabs(tabs: &[Opened], active: Option<usize>, cx: &mut App) {
        Kept::change(cx, |o| {
            o.tabs = tabs.to_vec();
            o.active = active;
        });
    }

    /// An environment's terminal sessions, in the order of their tabs.
    pub fn terminals(place: &str, env: &str, cx: &App) -> Vec<String> {
        cx.try_global::<Kept>()
            .and_then(|k| k.open.terminals.get(place)?.get(env).cloned())
            .unwrap_or_default()
    }

    /// Keeps an environment's terminal sessions in the order of its tabs.
    pub fn set_terminals(place: &str, env: &str, sessions: Vec<String>, cx: &mut App) {
        Kept::change(cx, |o| {
            let envs = o.terminals.entry(place.to_string()).or_default();
            if sessions.is_empty() {
                envs.remove(env);
            } else {
                envs.insert(env.to_string(), sessions);
            }
            if envs.is_empty() {
                o.terminals.remove(place);
            }
        });
    }

    /// Changes what is kept, and writes it a moment later if it differs.
    fn change(cx: &mut App, f: impl FnOnce(&mut Open)) {
        if !cx.has_global::<Kept>() {
            Kept::init(None, cx);
        }
        let k = cx.global_mut::<Kept>();
        let before = k.open.clone();
        f(&mut k.open);
        if k.open == before || k.file.is_none() || k.saving.is_some() {
            return;
        }
        let task = cx.spawn(async move |cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            cx.update(|cx| {
                let k = cx.global_mut::<Kept>();
                k.saving = None;
                if let (Some(file), Ok(b)) = (&k.file, serde_json::to_vec_pretty(&k.open)) {
                    let _ = crate::store::write_atomic(file, &b);
                }
            });
        });
        cx.global_mut::<Kept>().saving = Some(task);
    }
}

/// Puts sessions in the order kept: those kept first, in their order, then
/// the rest (sessions started elsewhere, from the web UI or over SSH) in
/// the order they came.
pub fn ordered<T>(sessions: Vec<T>, kept: &[String], id: impl Fn(&T) -> &str) -> Vec<T> {
    let mut sessions: Vec<Option<T>> = sessions.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(sessions.len());
    for k in kept {
        if let Some(s) = sessions
            .iter_mut()
            .find(|s| s.as_ref().is_some_and(|s| id(s) == k))
        {
            out.extend(s.take());
        }
    }
    out.extend(sessions.into_iter().flatten());
    out
}
