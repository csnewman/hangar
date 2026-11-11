//! Files between this computer and the environment, through the Code
//! service a few megabytes a message: files and folders uploaded into one
//! of its folders, a name it has asked about (replaced, skipped, or both
//! kept); and a file downloaded as itself, folders and several files as a
//! zip of them, to where this computer's save panel says.

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use gpui::{AnyWindowHandle, App, AsyncApp, ParentElement, Styled, Window, div, prelude::*, px};
use gpui_component::notification::Notification;
use gpui_component::{WindowExt, h_flex, v_flex};
use serde_json::{Value, json};

use crate::code::Code;
use crate::{palette, ui};

/// The most of a file sent in one message.
const CHUNK: usize = 4 << 20;
/// The most read in one message.
const READ: u64 = 8 << 20;

/// An upload under way, as the Project window shows it: into which
/// folder, how many files and which is being sent, and how much of all of
/// them has gone.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Uploading {
    pub to: String,
    pub files: usize,
    pub file: usize,
    pub sent: u64,
    pub total: u64,
}

/// The uploads under way, by a number each.
#[derive(Default)]
struct Uploads {
    next: u64,
    under_way: Vec<(u64, Uploading)>,
}

impl gpui::Global for Uploads {}

/// The uploads under way.
pub fn uploads(cx: &App) -> Vec<Uploading> {
    cx.try_global::<Uploads>()
        .map(|u| u.under_way.iter().map(|(_, u)| u.clone()).collect())
        .unwrap_or_default()
}

/// Changes an upload's progress and redraws, so the Project window shows
/// it; None takes it away.
fn progress(id: u64, change: impl FnOnce(&mut Option<Uploading>), cx: &mut App) {
    let u = cx.default_global::<Uploads>();
    let at = u.under_way.iter().position(|(i, _)| *i == id);
    let mut it = at.map(|i| u.under_way[i].1.clone());
    change(&mut it);
    match (at, it) {
        (Some(i), Some(it)) => u.under_way[i].1 = it,
        (Some(i), None) => {
            u.under_way.remove(i);
        }
        (None, Some(it)) => u.under_way.push((id, it)),
        (None, None) => {}
    }
    cx.refresh_windows();
}

/// A file of this computer's to upload, and where it goes under the
/// folder: its name, or its path within a folder chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub local: PathBuf,
    pub rel: String,
}

/// What to upload for paths chosen or dropped: each file under its name,
/// each folder's files under its name and their paths in it. Links are
/// left out, so a link to a folder above cannot make it endless.
pub fn uploads_of(paths: &[PathBuf]) -> Vec<Upload> {
    fn walk(at: &Path, rel: String, out: &mut Vec<Upload>) {
        let Ok(m) = std::fs::symlink_metadata(at) else {
            return;
        };
        if m.is_file() {
            out.push(Upload {
                local: at.to_path_buf(),
                rel,
            });
        } else if m.is_dir() {
            let Ok(entries) = std::fs::read_dir(at) else {
                return;
            };
            let mut names: Vec<_> = entries.flatten().map(|e| e.file_name()).collect();
            names.sort();
            for n in names {
                walk(&at.join(&n), format!("{rel}/{}", n.to_string_lossy()), out);
            }
        }
    }
    let mut out = Vec::new();
    for p in paths {
        if let Some(name) = p.file_name() {
            walk(p, name.to_string_lossy().into_owned(), &mut out);
        }
    }
    out
}

/// The first part of each upload's path: what it makes in the folder.
pub fn tops(uploads: &[Upload]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for u in uploads {
        let top = u.rel.split('/').next().unwrap_or(&u.rel).to_string();
        if !out.contains(&top) {
            out.push(top);
        }
    }
    out
}

/// A name for name not among taken: "name copy", "name copy 2", its
/// extension kept, as the service names a copy.
pub fn free_name(taken: &HashSet<String>, name: &str) -> String {
    if !taken.contains(name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    };
    (1..)
        .map(|i| {
            if i == 1 {
                format!("{stem} copy{ext}")
            } else {
                format!("{stem} copy {i}{ext}")
            }
        })
        .find(|n| !taken.contains(n))
        .unwrap_or_default()
}

/// What to do with names the folder has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clash {
    Replace,
    Skip,
    KeepBoth,
}

impl Clash {
    /// fs.transfer's word for it.
    pub fn param(self) -> &'static str {
        match self {
            Clash::Replace => "replace",
            Clash::Skip => "skip",
            Clash::KeepBoth => "rename",
        }
    }
}

/// Asks this computer for files, or a folder, and uploads them into to;
/// done has what was made there.
pub fn choose_and_upload(
    code: Code,
    to: String,
    folder: bool,
    window: &mut Window,
    cx: &mut App,
    done: impl FnOnce(Vec<String>, &mut Window, &mut App) + 'static,
) {
    let chosen = cx.prompt_for_paths(gpui::PathPromptOptions {
        files: !folder,
        directories: folder,
        multiple: true,
        prompt: Some("Upload".into()),
    });
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(paths))) = chosen.await else {
            return;
        };
        let uploads = cx.background_spawn(async move { uploads_of(&paths) }).await;
        let _ = handle.update(cx, move |_, window, cx| {
            upload(code, uploads, to, window, cx, done)
        });
    })
    .detach();
}

/// Uploads files into the folder to, asking first about names it has.
pub fn upload(
    code: Code,
    uploads: Vec<Upload>,
    to: String,
    window: &mut Window,
    cx: &mut App,
    done: impl FnOnce(Vec<String>, &mut Window, &mut App) + 'static,
) {
    if uploads.is_empty() {
        return;
    }
    let handle = window.window_handle();
    let id = {
        let u = cx.default_global::<Uploads>();
        u.next += 1;
        u.next
    };
    cx.spawn(async move |cx| {
        let sent = send(&code, uploads, &to, handle, id, cx).await;
        cx.update(|cx| progress(id, |u| *u = None, cx));
        match sent {
            Ok(Some(made)) => {
                let _ = handle.update(cx, |_, window, cx| {
                    crate::filehistory::record(
                        crate::filehistory::Done::Made {
                            code: code.clone(),
                            paths: made.clone(),
                        },
                        cx,
                    );
                    let what = match made.as_slice() {
                        [one] => one.rsplit('/').next().unwrap_or(one).to_string(),
                        many => format!("{} items", many.len()),
                    };
                    window.push_notification(
                        Notification::success(format!("Uploaded {what} to {to}")),
                        cx,
                    );
                    done(made, window, cx);
                });
            }
            Ok(None) => {}
            Err(e) => {
                let _ = handle.update(cx, |_, window, cx| {
                    window.push_notification(Notification::error(format!("Upload: {e}")), cx)
                });
            }
        }
    })
    .detach();
}

/// The upload itself: None when it was called off at the question.
async fn send(
    code: &Code,
    mut uploads: Vec<Upload>,
    to: &str,
    handle: AnyWindowHandle,
    id: u64,
    cx: &mut AsyncApp,
) -> Result<Option<Vec<String>>, String> {
    let listed = code.call("fs.list", json!({ "path": to })).await?;
    let have: HashSet<String> = listed
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["name"].as_str().map(str::to_string))
        .collect();
    let all = tops(&uploads);
    let clashes: Vec<String> = all.iter().filter(|t| have.contains(*t)).cloned().collect();
    let mut rename = std::collections::HashMap::new();
    if !clashes.is_empty() {
        let (tx, rx) = async_channel::bounded(1);
        let (names, at) = (clashes.clone(), to.to_string());
        handle
            .update(cx, move |_, window, cx| {
                ask_clash(&names, &at, "Uploading", tx, window, cx)
            })
            .map_err(|e| e.to_string())?;
        match rx.recv().await.ok().flatten() {
            None => return Ok(None),
            Some(Clash::Skip) => uploads.retain(|u| !clashes.iter().any(|c| top(u) == *c)),
            Some(Clash::Replace) => {
                let paths: Vec<String> = clashes.iter().map(|c| join(to, c)).collect();
                code.call("fs.trash", json!({ "paths": paths })).await?;
            }
            Some(Clash::KeepBoth) => {
                let mut taken: HashSet<String> = have.iter().chain(&all).cloned().collect();
                for c in &clashes {
                    let n = free_name(&taken, c);
                    taken.insert(n.clone());
                    rename.insert(c.clone(), n);
                }
            }
        }
    }
    let mut made = Vec::new();
    let sizes: Vec<PathBuf> = uploads.iter().map(|u| u.local.clone()).collect();
    let total: u64 = cx
        .background_spawn(async move {
            sizes
                .iter()
                .filter_map(|p| std::fs::metadata(p).ok())
                .map(|m| m.len())
                .sum()
        })
        .await;
    let files = uploads.len();
    cx.update(|cx| {
        progress(
            id,
            |u| {
                *u = Some(Uploading {
                    to: to.to_string(),
                    files,
                    file: 1,
                    sent: 0,
                    total,
                })
            },
            cx,
        )
    });
    for (n, u) in uploads.into_iter().enumerate() {
        cx.update(|cx| progress(id, |p| p.iter_mut().for_each(|p| p.file = n + 1), cx));
        let mut parts: Vec<String> = u.rel.split('/').map(str::to_string).collect();
        if let Some(n) = rename.get(&parts[0]) {
            parts[0] = n.clone();
        }
        let path = join(to, &parts.join("/"));
        let token = code.call("fs.uploadStart", json!({ "path": path })).await?;
        let token = token.as_str().unwrap_or_default().to_string();
        let sent = write_all(code, &token, &u.local, id, cx).await;
        if let Err(e) = sent {
            let _ = code.call("fs.uploadAbort", json!({ "token": token })).await;
            return Err(e);
        }
        code.call("fs.uploadEnd", json!({ "token": token, "replace": false }))
            .await?;
        let top = join(to, &parts[0]);
        if !made.contains(&top) {
            made.push(top);
        }
    }
    Ok(Some(made))
}

fn top(u: &Upload) -> &str {
    u.rel.split('/').next().unwrap_or(&u.rel)
}

/// A file's contents, a part at a time, each counted in the upload's
/// progress.
async fn write_all(
    code: &Code,
    token: &str,
    local: &Path,
    id: u64,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let mut at = 0u64;
    loop {
        let path = local.to_path_buf();
        let part = cx
            .background_spawn(async move {
                let mut f = std::fs::File::open(&path)?;
                f.seek(SeekFrom::Start(at))?;
                let mut buf = Vec::with_capacity(CHUNK);
                f.take(CHUNK as u64).read_to_end(&mut buf)?;
                Ok::<_, std::io::Error>(buf)
            })
            .await
            .map_err(|e| format!("{}: {e}", local.display()))?;
        if part.is_empty() {
            return Ok(());
        }
        at += part.len() as u64;
        let data = BASE64.encode(&part);
        code.call("fs.uploadWrite", json!({ "token": token, "data": data }))
            .await?;
        let n = part.len() as u64;
        cx.update(|cx| progress(id, |p| p.iter_mut().for_each(|p| p.sent += n), cx));
        if part.len() < CHUNK {
            return Ok(());
        }
    }
}

/// A path in a folder.
fn join(folder: &str, rel: &str) -> String {
    format!("{}/{rel}", folder.trim_end_matches('/'))
}

/// Asks what to do with names a folder has, saying what is being done
/// ("Uploading", "Moving"); None calls it off.
pub fn ask_clash(
    names: &[String],
    to: &str,
    doing: &'static str,
    answer: async_channel::Sender<Option<Clash>>,
    window: &mut Window,
    cx: &mut App,
) {
    let title = match names {
        [one] => format!("{one} already exists in {to}"),
        many => format!("{} names already exist in {to}", many.len()),
    };
    let listed: Vec<String> = if names.len() > 1 {
        names.iter().take(8).cloned().collect()
    } else {
        vec![]
    };
    let more = names.len().saturating_sub(8);
    // Closing the dialog any other way calls the upload off.
    let off = answer.clone();
    window.open_dialog(cx, move |dialog, _, cx| {
        let p = palette::of(cx);
        let choice = |id: &'static str, label: &'static str, c: Clash, primary: bool| {
            let answer = answer.clone();
            let b = if primary {
                ui::primary(id, label, &p)
            } else {
                ui::button(id, label, &p)
            };
            b.debug_selector(move || id.to_string())
                .on_click(move |_, window, cx| {
                    let _ = answer.try_send(Some(c));
                    window.close_dialog(cx);
                })
        };
        let off = off.clone();
        let mut body = v_flex()
            .gap(px(10.))
            .child(div().text_color(p.muted).child(format!(
                "{doing} would put another there. What is replaced goes to the trash."
            )));
        for n in &listed {
            body = body.child(div().text_size(px(11.5)).child(format!("· {n}")));
        }
        if more > 0 {
            body = body.child(
                div()
                    .text_size(px(11.5))
                    .text_color(p.muted)
                    .child(format!("and {more} more")),
            );
        }
        dialog
            .title(title.clone())
            .w(px(460.))
            .on_close(move |_, _, _| {
                let _ = off.try_send(None);
            })
            .child(
                body.child(
                    h_flex()
                        .justify_end()
                        .gap(px(8.))
                        .child({
                            let answer = answer.clone();
                            ui::button("dialog-cancel", "Cancel", &p).on_click(
                                move |_, window, cx| {
                                    let _ = answer.try_send(None);
                                    window.close_dialog(cx);
                                },
                            )
                        })
                        .child(choice("clash-skip", "Skip", Clash::Skip, false))
                        .child(choice("clash-both", "Keep Both", Clash::KeepBoth, false))
                        .child(choice("clash-replace", "Replace", Clash::Replace, true)),
                ),
            )
    });
}

/// Downloads files and folders of the environment's to this computer: one
/// file as itself, otherwise a zip named after the first, or after many.
pub fn download(
    code: Code,
    paths: Vec<(String, bool)>,
    many: &str,
    window: &mut Window,
    cx: &mut App,
) {
    let Some((first, dir)) = paths.first().cloned() else {
        return;
    };
    let name = first.rsplit('/').next().unwrap_or(&first).to_string();
    let single = paths.len() == 1 && !dir;
    let suggested = match (single, paths.len()) {
        (true, _) => name,
        (false, 1) => format!("{name}.zip"),
        _ => format!("{many}.zip"),
    };
    let downloads = dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"));
    let asked = cx.prompt_for_new_path(&downloads, Some(&suggested));
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        let Ok(Ok(Some(save))) = asked.await else {
            return;
        };
        let result = fetch(&code, &paths, single, &save, cx).await;
        let _ = handle.update(cx, |_, window, cx| match result {
            Ok(()) => window.push_notification(
                Notification::success(format!("Downloaded {}", save.display())),
                cx,
            ),
            Err(e) => window.push_notification(Notification::error(format!("Download: {e}")), cx),
        });
    })
    .detach();
}

/// Reads the file, or a zip of the paths, into save.
async fn fetch(
    code: &Code,
    paths: &[(String, bool)],
    single: bool,
    save: &Path,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let (src, zipped) = if single {
        (paths[0].0.clone(), false)
    } else {
        let list: Vec<&String> = paths.iter().map(|(p, _)| p).collect();
        let z = code.call("fs.zip", json!({ "paths": list })).await?;
        (z["path"].as_str().unwrap_or_default().to_string(), true)
    };
    let result = copy_to(code, &src, save, cx).await;
    if zipped {
        let _ = code.call("fs.unzip", json!({ "path": src })).await;
    }
    result
}

async fn copy_to(code: &Code, src: &str, save: &Path, cx: &mut AsyncApp) -> Result<(), String> {
    let file = save.to_path_buf();
    cx.background_spawn(async move { std::fs::File::create(&file).map(|_| ()) })
        .await
        .map_err(|e| format!("{}: {e}", save.display()))?;
    let mut at = 0u64;
    loop {
        let b: Value = code
            .call(
                "fs.bytes",
                json!({ "path": src, "offset": at, "length": READ }),
            )
            .await?;
        let data = BASE64
            .decode(b["data"].as_str().unwrap_or_default())
            .map_err(|e| e.to_string())?;
        let size = b["size"].as_u64().unwrap_or(0);
        if data.is_empty() {
            return Ok(());
        }
        at += data.len() as u64;
        let file = save.to_path_buf();
        cx.background_spawn(async move {
            let mut f = std::fs::OpenOptions::new().append(true).open(&file)?;
            f.write_all(&data)
        })
        .await
        .map_err(|e| format!("{}: {e}", save.display()))?;
        if at >= size {
            return Ok(());
        }
    }
}
