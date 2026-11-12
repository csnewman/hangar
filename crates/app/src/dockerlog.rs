//! A container's log, followed: its last thousand lines from the Code
//! service's docker.logs, then what it writes, until it stops or the
//! connection is lost. The Docker window shows one beside its list, and an
//! editor tab shows one of its own, as the web's Show Logs does.

use gpui::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement, Render, Styled,
    WeakEntity, Window, div, prelude::*, px,
};
use gpui_component::{h_flex, v_flex};
use serde_json::{Value, json};

use crate::logview::LogPane;
use crate::palette;
use crate::workspace::{Event, PRIMARY, Workspace};

/// The container followed.
struct Follow {
    id: String,
    /// The connection it is followed on, which stops it.
    code: crate::code::Code,
    /// The stream docker.logs gave, once it has answered.
    stream: Option<u64>,
    /// Why it stopped following.
    ended: Option<String>,
}

/// What a log said before docker.logs answered with its stream's number.
enum Early {
    Text(u64, String),
    End(u64, String),
}

/// What a log said when the connection carrying it was lost.
const LOST: &str = "the connection was lost";

pub struct ContainerLog {
    workspace: Entity<Workspace>,
    /// The container's name, for a tab's title.
    name: String,
    pane: Entity<LogPane>,
    follow: Option<Follow>,
    /// Each docker.logs asked counts; an answer to an older one is
    /// stopped, not shown.
    asked: u64,
    early: Vec<Early>,
    focus: FocusHandle,
    _sub: gpui::Subscription,
}

impl ContainerLog {
    pub fn new(workspace: Entity<Workspace>, id: &'static str, cx: &mut Context<Self>) -> Self {
        let sub = cx.subscribe(&workspace, |v, _, e: &Event, cx| match e {
            Event::Heard {
                root,
                event,
                params,
            } if root == PRIMARY => match event.as_str() {
                "docker.log" => v.heard_log(params, cx),
                "docker.logEnd" => v.heard_end(params, cx),
                _ => {}
            },
            Event::Lost { root } if root == PRIMARY => {
                if let Some(f) = &mut v.follow {
                    f.stream = None;
                    f.ended = Some(LOST.into());
                }
                cx.notify();
            }
            _ => {}
        });
        ContainerLog {
            workspace,
            name: String::new(),
            pane: cx.new(|cx| LogPane::new(id, "Waiting for output…", cx)),
            follow: None,
            asked: 0,
            early: Vec::new(),
            focus: cx.focus_handle(),
            _sub: sub,
        }
    }

    /// The lines shown.
    pub fn pane(&self) -> &Entity<LogPane> {
        &self.pane
    }

    /// The container followed, by ID, and its name.
    pub fn following(&self) -> Option<&str> {
        self.follow.as_ref().map(|f| f.id.as_str())
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Why the log shown is not being followed.
    pub fn ended(&self) -> Option<&str> {
        self.follow.as_ref().and_then(|f| f.ended.as_deref())
    }

    /// Follows a container's output: its last thousand lines, then what it
    /// writes.
    pub fn follow(&mut self, id: String, name: String, cx: &mut Context<Self>) {
        self.stop(cx);
        self.name = name;
        let Some(code) = self.workspace.read(cx).primary() else {
            return;
        };
        self.asked += 1;
        let asked = self.asked;
        self.early.clear();
        self.pane.update(cx, |l, cx| {
            l.clear(cx);
            l.set_empty("Waiting for output…", cx);
        });
        self.follow = Some(Follow {
            id: id.clone(),
            code: code.clone(),
            stream: None,
            ended: None,
        });
        let call = code.call("docker.logs", json!({ "id": id, "tail": 1000 }));
        cx.spawn(async move |this, cx| {
            let got = call.await;
            let _ = this.update(cx, |v, cx| {
                let stream = got.as_ref().ok().and_then(|r| r["stream"].as_u64());
                if v.asked != asked {
                    if let (Some(s), Some(code)) = (stream, v.workspace.read(cx).primary()) {
                        drop(code.call("docker.logsStop", json!({ "stream": s })));
                    }
                    return;
                }
                let Some(f) = &mut v.follow else { return };
                match (got, stream) {
                    (Ok(_), Some(s)) => {
                        f.stream = Some(s);
                        for e in std::mem::take(&mut v.early) {
                            match e {
                                Early::Text(n, text) if n == s => {
                                    v.pane.update(cx, |l, cx| l.push(&text, cx))
                                }
                                Early::End(n, why) if n == s => v.end(why, cx),
                                _ => {}
                            }
                        }
                    }
                    (Err(e), _) => v.end(e, cx),
                    (Ok(_), None) => v.end("Docker gave no log".into(), cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Follows the same container again, after its log ended.
    pub fn again(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.following().map(str::to_string) {
            let name = self.name.clone();
            self.follow(id, name, cx);
        }
    }

    /// Stops following.
    pub fn stop(&mut self, _: &mut Context<Self>) {
        self.asked += 1;
        if let Some(Follow {
            stream: Some(s),
            code,
            ..
        }) = self.follow.take()
        {
            drop(code.call("docker.logsStop", json!({ "stream": s })));
        }
    }

    fn end(&mut self, why: String, cx: &mut Context<Self>) {
        if let Some(f) = &mut self.follow {
            f.stream = None;
            f.ended = Some(why.clone());
        }
        if self.pane.read(cx).lines().is_empty() {
            self.pane
                .update(cx, |l, cx| l.set_empty(format!("No output: {why}."), cx));
        }
        cx.notify();
    }

    fn heard_log(&mut self, params: &Value, cx: &mut Context<Self>) {
        let (Some(n), Some(text)) = (params["stream"].as_u64(), params["text"].as_str()) else {
            return;
        };
        match &self.follow {
            Some(f) if f.stream == Some(n) => self.pane.update(cx, |l, cx| l.push(text, cx)),
            Some(f) if f.stream.is_none() && f.ended.is_none() => {
                self.early.push(Early::Text(n, text.to_string()))
            }
            _ => {}
        }
    }

    fn heard_end(&mut self, params: &Value, cx: &mut Context<Self>) {
        let Some(n) = params["stream"].as_u64() else {
            return;
        };
        let why = params["why"].as_str().unwrap_or("it ended").to_string();
        match &self.follow {
            Some(f) if f.stream == Some(n) => self.end(why, cx),
            Some(f) if f.stream.is_none() && f.ended.is_none() => {
                self.early.push(Early::End(n, why))
            }
            _ => {}
        }
    }
}

impl Drop for ContainerLog {
    fn drop(&mut self) {
        // The request goes as it is made; its answer is not waited for.
        if let Some(Follow {
            stream: Some(s),
            code,
            ..
        }) = &self.follow
        {
            drop(code.call("docker.logsStop", json!({ "stream": s })));
        }
    }
}

impl Focusable for ContainerLog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// As an editor tab: what is followed, why it stopped and Follow Again
/// over the lines.
impl Render for ContainerLog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let me: WeakEntity<Self> = cx.entity().downgrade();
        let ended = self.ended().map(str::to_string);
        v_flex()
            .size_full()
            .track_focus(&self.focus)
            .child(
                h_flex()
                    .flex_none()
                    .h(px(26.))
                    .px(px(10.))
                    .gap(px(8.))
                    .border_b_1()
                    .border_color(p.border)
                    .text_size(px(11.))
                    .text_color(p.muted)
                    .child(match &ended {
                        Some(why) => format!("{}: not following, {why}", self.name),
                        None => format!("{}: following its output", self.name),
                    })
                    .when(ended.is_some(), |d| {
                        d.child(
                            div()
                                .id("log-tab-again")
                                .text_color(p.accent)
                                .cursor_pointer()
                                .child("Follow Again")
                                .on_click(move |_, _, cx| {
                                    let _ = me.update(cx, |v, cx| v.again(cx));
                                }),
                        )
                    }),
            )
            .child(div().flex_1().min_h_0().child(self.pane.clone()))
    }
}
