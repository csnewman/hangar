//! An environment's Linux desktop in an editor tab, as the web's Desktop
//! tab shows it: its pixels as they come (rfb.rs), at one pixel a point,
//! the desktop sized to the tab as the tab's size settles, the mouse and
//! keys passed on, and the clipboard both ways: what is copied there
//! reaches this computer's, and Cmd-V pastes this computer's there.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{
    App, Bounds, Context, FocusHandle, Focusable, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render,
    RenderImage, ScrollEvent, Styled, Window, canvas, div, prelude::*, px,
};
use gpui_component::h_flex;

use crate::api::Client;
use crate::palette;
use crate::rfb::{self, Command, Event, Framebuffer};

/// How long the tab's size must hold before the desktop is sized to it.
const SETTLE: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, PartialEq)]
enum State {
    Connecting,
    Live,
    Ended(String),
}

pub struct DesktopView {
    client: Client,
    env: String,
    fb: Arc<Mutex<Framebuffer>>,
    conn: Option<rfb::Connection>,
    state: State,
    name: String,
    /// The framebuffer as last drawn, and its version.
    image: Option<(Arc<RenderImage>, u64)>,
    /// The buttons held, and where the pixels start in the window.
    buttons: u8,
    origin: Point<Pixels>,
    /// The size the tab has room for, and the size the desktop was last
    /// asked to be, counted so only a settled size is asked.
    room: (u32, u32),
    asked: (u32, u32),
    sizing: u64,
    focus: FocusHandle,
}

impl DesktopView {
    pub fn new(client: Client, env: &str, cx: &mut Context<Self>) -> Self {
        let mut v = DesktopView {
            client,
            env: env.to_string(),
            fb: Arc::new(Mutex::new(Framebuffer::default())),
            conn: None,
            state: State::Connecting,
            name: String::new(),
            image: None,
            buttons: 0,
            origin: Point::default(),
            room: (0, 0),
            asked: (0, 0),
            sizing: 0,
            focus: cx.focus_handle(),
        };
        v.connect(cx);
        v
    }

    /// The tab's name.
    pub fn title(&self) -> String {
        "Desktop".into()
    }

    pub fn env(&self) -> &str {
        &self.env
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        let (url, auth) = self.client.desktop_socket(&self.env);
        let (tx, rx) = async_channel::unbounded();
        self.state = State::Connecting;
        self.conn = Some(rfb::Connection::open(
            url,
            auth,
            self.fb.clone(),
            move |e| tx.send_blocking(e).is_ok(),
        ));
        cx.spawn(async move |this, cx| {
            while let Ok(e) = rx.recv().await {
                // Frames that came meanwhile are drawn as one.
                let mut events = vec![e];
                while let Ok(more) = rx.try_recv() {
                    events.push(more);
                }
                if this.update(cx, |v, cx| v.heard(events, cx)).is_err() {
                    return;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn heard(&mut self, events: Vec<Event>, cx: &mut Context<Self>) {
        for e in events {
            match e {
                Event::Ready { name, .. } => {
                    self.state = State::Live;
                    self.name = name;
                    self.asked = (0, 0);
                    self.fit(cx);
                }
                Event::Frame | Event::Bell => {}
                Event::Clipboard(text) => {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(text))
                }
                Event::Ended(why) => {
                    self.state = State::Ended(why);
                    self.conn = None;
                }
            }
        }
        cx.notify();
    }

    fn send(&self, c: Command) {
        if let Some(conn) = &self.conn {
            conn.send(c);
        }
    }

    /// Sizes the desktop to the tab, once the tab's size has settled: every
    /// viewer sees the one desktop, so the last to ask wins, as on the web.
    fn fit(&mut self, cx: &mut Context<Self>) {
        let (w, h) = self.room;
        if w < 200 || h < 150 || self.asked == (w, h) || self.state != State::Live {
            return;
        }
        self.sizing += 1;
        let asked = self.sizing;
        let (client, env) = (self.client.clone(), self.env.clone());
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            let still = this.update(cx, |v, _| v.sizing == asked).unwrap_or(false);
            if !still {
                return;
            }
            let done = cx
                .background_spawn(async move { client.resize_desktop(&env, w, h) })
                .await;
            let _ = this.update(cx, |v, cx| {
                if done.is_ok() {
                    v.asked = (w, h);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Where the tab has room for the desktop, in points, even each way
    /// as the desktop takes it.
    fn measured(&mut self, b: Bounds<Pixels>, cx: &mut Context<Self>) {
        let w = (f32::from(b.size.width) as u32) & !1;
        let h = (f32::from(b.size.height) as u32) & !1;
        self.origin = b.origin;
        if (w, h) != self.room {
            self.room = (w, h);
            self.fit(cx);
        }
    }

    /// A place in the window as the desktop's pixel.
    fn at(&self, p: Point<Pixels>) -> (u16, u16) {
        let x = f32::from(p.x - self.origin.x).max(0.) as u16;
        let y = f32::from(p.y - self.origin.y).max(0.) as u16;
        (x, y)
    }

    fn pointer(&self, p: Point<Pixels>) {
        let (x, y) = self.at(p);
        self.send(Command::Pointer {
            x,
            y,
            buttons: self.buttons,
        });
    }

    fn mask(b: MouseButton) -> u8 {
        match b {
            MouseButton::Left => 1,
            MouseButton::Middle => 2,
            MouseButton::Right => 4,
            _ => 0,
        }
    }

    /// A key typed: Cmd-V pastes this computer's clipboard there, Cmd-C
    /// copies as Ctrl-C does there; anything else is pressed and let go,
    /// with the modifiers held around it.
    fn key(&mut self, e: &KeyDownEvent, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        let m = &k.modifiers;
        if m.platform {
            match k.key.as_str() {
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                        self.send(Command::Paste(text));
                    }
                    self.tap(rfb::CONTROL, 'v' as u32);
                }
                "c" => self.tap(rfb::CONTROL, 'c' as u32),
                _ => return,
            }
            cx.stop_propagation();
            return;
        }
        let printable = k
            .key_char
            .as_deref()
            .filter(|c| !c.chars().any(char::is_control));
        let Some(sym) = rfb::keysym(&k.key, if m.control || m.alt { None } else { printable })
        else {
            return;
        };
        let mut held = vec![];
        if m.control {
            held.push(rfb::CONTROL);
        }
        if m.alt {
            held.push(rfb::ALT);
        }
        // A character typed carries its shift; a key named does not.
        if m.shift && (printable.is_none() || m.control || m.alt) {
            held.push(rfb::SHIFT);
        }
        for h in &held {
            self.send(Command::Key {
                keysym: *h,
                down: true,
            });
        }
        self.send(Command::Key {
            keysym: sym,
            down: true,
        });
        self.send(Command::Key {
            keysym: sym,
            down: false,
        });
        for h in held.iter().rev() {
            self.send(Command::Key {
                keysym: *h,
                down: false,
            });
        }
        cx.stop_propagation();
    }

    fn tap(&self, modifier: u32, sym: u32) {
        self.send(Command::Key {
            keysym: modifier,
            down: true,
        });
        self.send(Command::Key {
            keysym: sym,
            down: true,
        });
        self.send(Command::Key {
            keysym: sym,
            down: false,
        });
        self.send(Command::Key {
            keysym: modifier,
            down: false,
        });
    }

    /// The framebuffer as an image, made again when it has changed; the one
    /// it replaces is let go of.
    fn picture(&mut self, window: &mut Window, cx: &mut App) -> Option<Arc<RenderImage>> {
        let f = self.fb.lock().unwrap();
        if f.width == 0 || f.height == 0 {
            return None;
        }
        if let Some((img, version)) = &self.image
            && *version == f.version
        {
            return Some(img.clone());
        }
        let frame = image::RgbaImage::from_raw(f.width as u32, f.height as u32, f.pixels.clone())?;
        let version = f.version;
        drop(f);
        let img = Arc::new(RenderImage::new(vec![image::Frame::new(frame)]));
        if let Some((old, _)) = self.image.replace((img.clone(), version)) {
            cx.drop_image(old, Some(window));
        }
        Some(img)
    }
}

impl Focusable for DesktopView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DesktopView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let picture = self.picture(window, cx);
        let (w, h) = {
            let f = self.fb.lock().unwrap();
            (f.width, f.height)
        };
        let me = cx.entity().downgrade();
        let status = match &self.state {
            State::Connecting => "Connecting to the desktop…".to_string(),
            State::Live => format!(
                "{} · {w}×{h}",
                if self.name.is_empty() {
                    "Desktop"
                } else {
                    &self.name
                }
            ),
            State::Ended(why) => format!("Not connected: {why}"),
        };
        let ended = matches!(self.state, State::Ended(_));
        let bar = h_flex()
            .flex_none()
            .h(px(26.))
            .px(px(10.))
            .gap(px(10.))
            .border_b_1()
            .border_color(p.border)
            .text_size(px(11.))
            .text_color(p.muted)
            .child(div().truncate().child(status))
            .child(div().flex_1())
            .when(!ended, |d| {
                d.child(
                    div()
                        .id("desktop-paste")
                        .text_color(p.accent)
                        .cursor_pointer()
                        .child("Paste Clipboard")
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new(
                                "Put this computer's clipboard on the desktop's (Cmd-V does, and pastes)",
                            )
                            .build(window, cx)
                        })
                        .on_click(cx.listener(|v, _, _, cx| {
                            if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                                v.send(Command::Paste(text));
                            }
                        })),
                )
            })
            .when(ended, |d| {
                d.child(
                    div()
                        .id("desktop-reconnect")
                        .text_color(p.accent)
                        .cursor_pointer()
                        .child("Connect Again")
                        .on_click(cx.listener(|v, _, _, cx| v.connect(cx))),
                )
            });
        let screen = div()
            .id("desktop-screen")
            .debug_selector(|| "desktop-screen".into())
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .bg(gpui::black())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|v, e: &KeyDownEvent, _, cx| v.key(e, cx)))
            .on_mouse_move(cx.listener(|v, e: &MouseMoveEvent, _, _| v.pointer(e.position)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|v, e: &MouseDownEvent, window, cx| {
                    window.focus(&v.focus, cx);
                    v.buttons |= Self::mask(e.button);
                    v.pointer(e.position);
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|v, e: &MouseDownEvent, _, _| {
                    v.buttons |= Self::mask(e.button);
                    v.pointer(e.position);
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|v, e: &MouseDownEvent, _, _| {
                    v.buttons |= Self::mask(e.button);
                    v.pointer(e.position);
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|v, e: &MouseUpEvent, _, _| {
                    v.buttons &= !Self::mask(e.button);
                    v.pointer(e.position);
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|v, e: &MouseUpEvent, _, _| {
                    v.buttons &= !Self::mask(e.button);
                    v.pointer(e.position);
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|v, e: &MouseUpEvent, _, _| {
                    v.buttons &= !Self::mask(e.button);
                    v.pointer(e.position);
                }),
            )
            .on_scroll(cx.listener(|v, e: &ScrollEvent, _, _| {
                let dy = f32::from(e.delta.pixel_delta(px(16.)).y);
                let notches = (dy.abs() / 16.).round().max(1.) as usize;
                let wheel = if dy > 0. { 8 } else { 16 };
                if dy == 0. {
                    return;
                }
                let (x, y) = v.at(e.position);
                for _ in 0..notches.min(10) {
                    v.send(Command::Pointer {
                        x,
                        y,
                        buttons: v.buttons | wheel,
                    });
                    v.send(Command::Pointer {
                        x,
                        y,
                        buttons: v.buttons,
                    });
                }
            }))
            .child(
                canvas(
                    move |b, _, cx| {
                        let _ = me.update(cx, |v, cx| v.measured(b, cx));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
            .when_some(picture, |d, img| {
                d.child(
                    gpui::img(img)
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(px(w as f32))
                        .h(px(h as f32)),
                )
            });
        gpui_component::v_flex()
            .size_full()
            .bg(p.bg)
            .child(bar)
            .child(screen)
    }
}
