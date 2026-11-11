//! Undo and redo of what the Project and Filesystem windows did to files,
//! as IntelliJ's ⌘Z there undoes it: what was made (new, copied, pasted,
//! uploaded) goes to the environment's trash, what was moved or renamed goes
//! back, and what was put in the trash comes out of it. Redo does it again.
//! Each undoing is itself what redo undoes, so the two go back and forth.

use gpui::{App, Global, Window};
use gpui_component::WindowExt;
use gpui_component::notification::Notification;
use serde_json::{Value, json};

use crate::code::Code;

/// Something done to files, as undo takes it back.
#[derive(Clone)]
pub enum Done {
    /// Made: by New, a copy, a paste or an upload.
    Made { code: Code, paths: Vec<String> },
    /// Moved or renamed, each from where to where.
    Moved {
        code: Code,
        pairs: Vec<(String, String)>,
    },
    /// Put in the trash, each with the id that brings it back.
    Trashed {
        code: Code,
        items: Vec<(String, String)>,
    },
}

/// What was done, as the notification after undoing it says.
pub fn describe(paths: &[&str], kind: &str) -> String {
    let name = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    match paths {
        [one] => format!("{kind} {}", name(one)),
        many => format!("{kind} {} items", many.len()),
    }
}

impl Done {
    /// What was done, for the notification: making, moving or renaming,
    /// or deleting what.
    pub fn what(&self) -> String {
        match self {
            Done::Made { paths, .. } => describe(
                &paths.iter().map(String::as_str).collect::<Vec<_>>(),
                "making",
            ),
            Done::Moved { pairs, .. } => {
                let froms: Vec<&str> = pairs.iter().map(|(f, _)| f.as_str()).collect();
                let renamed = pairs.len() == 1 && parent(&pairs[0].0) == parent(&pairs[0].1);
                describe(&froms, if renamed { "renaming" } else { "moving" })
            }
            Done::Trashed { items, .. } => describe(
                &items.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
                "deleting",
            ),
        }
    }

    /// What fs.transfer did, from its answer: what was copied, made; what
    /// was moved, moved. None when it did nothing.
    pub fn of_transfer(code: &Code, moved: bool, answer: &Value) -> Option<Done> {
        let pairs: Vec<(String, String)> = answer["done"]
            .as_array()?
            .iter()
            .filter_map(|d| {
                Some((
                    d["from"].as_str()?.to_string(),
                    d["to"].as_str()?.to_string(),
                ))
            })
            .collect();
        if pairs.is_empty() {
            return None;
        }
        Some(if moved {
            Done::Moved {
                code: code.clone(),
                pairs,
            }
        } else {
            Done::Made {
                code: code.clone(),
                paths: pairs.into_iter().map(|(_, to)| to).collect(),
            }
        })
    }

    /// What fs.trash did, from its answer: each path and the id that brings
    /// it back.
    pub fn of_trash(code: &Code, answer: &Value) -> Option<Done> {
        let items = trashed(answer);
        (!items.is_empty()).then(|| Done::Trashed {
            code: code.clone(),
            items,
        })
    }

    /// Takes it back: what that did, which undoes it in turn.
    async fn reverse(self) -> Result<Done, String> {
        match self {
            Done::Made { code, paths } => {
                let answer = code.call("fs.trash", json!({ "paths": paths })).await?;
                Ok(Done::Trashed {
                    items: trashed(&answer),
                    code,
                })
            }
            Done::Trashed { code, items } => {
                let wanted: Vec<Value> = items
                    .iter()
                    .map(|(path, id)| json!({ "path": path, "id": id }))
                    .collect();
                code.call("fs.restore", json!({ "items": wanted })).await?;
                Ok(Done::Made {
                    paths: items.into_iter().map(|(p, _)| p).collect(),
                    code,
                })
            }
            Done::Moved { code, pairs } => {
                for (from, to) in pairs.iter().rev() {
                    code.call("fs.rename", json!({ "from": to, "to": from }))
                        .await?;
                }
                Ok(Done::Moved {
                    pairs: pairs.into_iter().map(|(a, b)| (b, a)).collect(),
                    code,
                })
            }
        }
    }
}

fn parent(p: &str) -> &str {
    p.rsplit_once('/').map_or("", |(d, _)| d)
}

/// The items fs.trash answers with: each path and its id.
fn trashed(answer: &Value) -> Vec<(String, String)> {
    answer
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let id = match &t["id"] {
                Value::String(s) => s.clone(),
                Value::Null => return None,
                v => v.to_string(),
            };
            Some((t["path"].as_str()?.to_string(), id))
        })
        .collect()
}

/// What can be undone and redone, the latest last.
#[derive(Default)]
struct History {
    undo: Vec<Done>,
    redo: Vec<Done>,
}

impl Global for History {}

/// How much is kept to undo.
const KEPT: usize = 100;

/// Keeps what was done to be undone; what was undone can no longer be
/// redone.
pub fn record(done: Done, cx: &mut App) {
    let h = cx.default_global::<History>();
    h.undo.push(done);
    if h.undo.len() > KEPT {
        h.undo.remove(0);
    }
    h.redo.clear();
}

/// Undoes the last thing done to files, saying what in a notification.
pub fn undo(window: &mut Window, cx: &mut App) {
    step(true, window, cx)
}

/// Does again the last thing undone.
pub fn redo(window: &mut Window, cx: &mut App) {
    step(false, window, cx)
}

fn step(undo: bool, window: &mut Window, cx: &mut App) {
    let h = cx.default_global::<History>();
    let next = if undo { h.undo.pop() } else { h.redo.pop() };
    let Some(done) = next else {
        let what = if undo {
            "Nothing to undo"
        } else {
            "Nothing to redo"
        };
        window.push_notification(Notification::info(what), cx);
        return;
    };
    let said = format!("{} {}", if undo { "Undid" } else { "Redid" }, done.what());
    window
        .spawn(cx, async move |cx| {
            let back = done.reverse().await;
            let _ = cx.update(|window, cx| match back {
                Ok(back) => {
                    let h = cx.default_global::<History>();
                    if undo {
                        h.redo.push(back);
                    } else {
                        h.undo.push(back);
                    }
                    window.push_notification(Notification::success(said), cx);
                }
                Err(e) => window.push_notification(Notification::error(e), cx),
            });
        })
        .detach();
}
