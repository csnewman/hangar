//! Add a Place, as the design has it: the three kinds of place, then a
//! server by its address, signed in through the browser. The app asks the
//! server for a grant, opens its page approving "Hangar Desktop on <this
//! computer>", and collects the access token it makes once the person
//! approves; the code is shown in case the browser asks for it. A token
//! made on the server's Account page can be pasted instead. With no
//! places yet it is the first run's page.

use std::time::Duration;

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, ParentElement,
    Render, SharedString, Styled, Task, Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};

use crate::api::{Client, SignInGrant, SignedIn};
use crate::palette::{self, Palette};
use crate::places::Places;
use crate::store;
use crate::ui::{button, chip, disabled, primary};

pub enum AddPlaceEvent {
    /// The place added, or None when the user gave up.
    Done(Option<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// The kinds of place.
    Kinds,
    /// A server's address, to check.
    Address,
    /// Waiting for the person to approve the app in their browser.
    Browser,
    /// An access token, pasted.
    Token,
}

/// A server checked, and the grant waited on.
struct Waiting {
    url: String,
    version: String,
    grant: SignInGrant,
}

pub struct AddPlaceView {
    places: Entity<Places>,
    step: Step,
    url: Entity<InputState>,
    token: Entity<InputState>,
    name: Entity<InputState>,
    waiting: Option<Waiting>,
    busy: bool,
    error: Option<String>,
    /// The first run: no place yet, so no way back.
    first: bool,
    /// Asking the server whether the grant is approved.
    poll: Option<Task<()>>,
    focus: FocusHandle,
    _subs: Vec<gpui::Subscription>,
}

impl EventEmitter<AddPlaceEvent> for AddPlaceView {}

impl Focusable for AddPlaceView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl AddPlaceView {
    pub fn new(places: Entity<Places>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("hangar.example.com"));
        let token = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("hgr_…")
                .masked(true)
        });
        let name = cx.new(|cx| InputState::new(window, cx));
        let subs = vec![
            cx.subscribe_in(&url, window, |this, _, e: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = e {
                    this.check_address(window, cx);
                }
            }),
        ];
        let first = places.read(cx).list().is_empty();
        AddPlaceView {
            places,
            step: Step::Kinds,
            url,
            token,
            name,
            waiting: None,
            busy: false,
            error: None,
            first,
            poll: None,
            focus: cx.focus_handle(),
            _subs: subs,
        }
    }

    pub fn step(&self) -> &Step {
        &self.step
    }

    /// The code the browser may ask for, while it is waited on.
    pub fn code(&self) -> Option<&str> {
        self.waiting.as_ref().map(|w| w.grant.user_code.as_str())
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Goes to a step, dropping any wait.
    pub fn go(&mut self, step: Step, cx: &mut Context<Self>) {
        self.step = step;
        self.error = None;
        self.poll = None;
        self.waiting = None;
        self.busy = false;
        cx.notify();
    }

    /// Signs a place at url in again: its address checked and the browser
    /// opened at once.
    pub fn sign_in(&mut self, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.url
            .update(cx, |s, cx| s.set_value(url.to_string(), window, cx));
        self.go(Step::Address, cx);
        self.check_address(window, cx);
    }

    /// Checks the address is a Hangar this app speaks to, asks it for a
    /// grant, and opens its page in the browser.
    pub fn check_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.url.read(cx).value().to_string();
        let Some(url) = store::normalise_url(&typed) else {
            self.error = Some("That is not a server's address.".into());
            cx.notify();
            return;
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        let name = format!("Hangar Desktop on {}", computer_name());
        let client = Client::new(&url, "");
        let task = cx.background_spawn(async move {
            let version = client.version().map_err(|e| e.to_string())?;
            if !version.versions.iter().any(|v| v == crate::api::VERSION) {
                return Err(format!(
                    "This server ({}) does not speak the app's API, {}.",
                    version.server,
                    crate::api::VERSION
                ));
            }
            let grant = client.start_sign_in(&name).map_err(|e| match e {
                crate::api::Error::Status(404, _) => {
                    "This server cannot sign the app in through the browser: paste an access token instead."
                        .to_string()
                }
                e => e.to_string(),
            })?;
            Ok((version.server, grant))
        });
        let host = store::place_id(&url);
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await;
            let _ = this.update_in(cx, |v, window, cx| {
                v.busy = false;
                match r {
                    Ok((version, grant)) => {
                        let label = host.split(':').next().unwrap_or(&host).to_string();
                        if v.name.read(cx).value().is_empty() {
                            v.name.update(cx, |s, cx| s.set_value(label, window, cx));
                        }
                        cx.open_url(&format!("{url}{}", grant.verification_path));
                        v.waiting = Some(Waiting {
                            url,
                            version,
                            grant,
                        });
                        v.step = Step::Browser;
                        v.wait(cx);
                    }
                    Err(e) => v.error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Asks, every interval the server gave, whether the grant is approved,
    /// and adds the place once it is.
    fn wait(&mut self, cx: &mut Context<Self>) {
        let Some(w) = &self.waiting else { return };
        let client = Client::new(&w.url, "");
        let device = w.grant.device_code.clone();
        let every = Duration::from_secs(w.grant.interval.clamp(1, 30));
        let until = w.grant.expires_at;
        self.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(every).await;
                let (client, device) = (client.clone(), device.clone());
                let r = cx
                    .background_spawn(async move { client.finish_sign_in(&device) })
                    .await;
                let done = this
                    .update(cx, |v, cx| v.answered(r, until, cx))
                    .unwrap_or(true);
                if done {
                    return;
                }
            }
        }));
    }

    /// Takes the server's answer about the grant, saying whether the wait
    /// is over.
    fn answered(
        &mut self,
        r: Result<SignedIn, crate::api::Error>,
        until: chrono::DateTime<chrono::Utc>,
        cx: &mut Context<Self>,
    ) -> bool {
        match r {
            Ok(SignedIn::Token(token)) => {
                let url = self
                    .waiting
                    .as_ref()
                    .map(|w| w.url.clone())
                    .unwrap_or_default();
                self.add(&url, &token, cx);
                true
            }
            Ok(SignedIn::Waiting) if chrono::Utc::now() < until => false,
            Ok(SignedIn::Refused) => {
                self.back_to_address("The sign-in was refused in the browser.", cx);
                true
            }
            Ok(SignedIn::Waiting) | Ok(SignedIn::Gone) => {
                self.back_to_address("The sign-in expired before it was approved. Try again.", cx);
                true
            }
            // A dropped connection is asked again, until the grant expires.
            Err(e) if chrono::Utc::now() < until => {
                self.error = Some(format!("Asking the server again: {e}"));
                cx.notify();
                false
            }
            Err(e) => {
                self.back_to_address(&e.to_string(), cx);
                true
            }
        }
    }

    fn back_to_address(&mut self, why: &str, cx: &mut Context<Self>) {
        self.go(Step::Address, cx);
        self.error = Some(why.to_string());
    }

    /// Adds the place, or signs it in again, with its token, under the name
    /// given.
    fn add(&mut self, url: &str, token: &str, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        let r = self.places.update(cx, |p, cx| {
            let id = p.add_server(url, token, cx)?;
            if !name.is_empty() {
                p.rename(&id, &name, cx);
            }
            Ok::<_, String>(id)
        });
        match r {
            Ok(id) => {
                self.poll = None;
                cx.emit(AddPlaceEvent::Done(Some(id)));
            }
            Err(e) => {
                self.error = Some(e);
                cx.notify();
            }
        }
    }

    /// Adds a server with a token pasted.
    pub fn submit_token(&mut self, cx: &mut Context<Self>) {
        let url = self.url.read(cx).value().to_string();
        let token = self.token.read(cx).value().to_string();
        let r = self
            .places
            .update(cx, |p, cx| p.add_server(&url, &token, cx));
        match r {
            Ok(id) => cx.emit(AddPlaceEvent::Done(Some(id))),
            Err(e) => {
                self.error = Some(e);
                cx.notify();
            }
        }
    }

    /// The URL and token inputs, for tests and the shell's prefill.
    pub fn inputs(&self) -> (Entity<InputState>, Entity<InputState>) {
        (self.url.clone(), self.token.clone())
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        self.poll = None;
        cx.emit(AddPlaceEvent::Done(None));
    }
}

impl Render for AddPlaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let body = match self.step {
            Step::Kinds => self.kinds(&p, cx).into_any_element(),
            Step::Address => self.address(&p, cx).into_any_element(),
            Step::Browser => self.browser(&p, window, cx).into_any_element(),
            Step::Token => self.token_step(&p, cx).into_any_element(),
        };
        let width = if self.step == Step::Kinds { 620. } else { 480. };
        v_flex()
            .id("add-place")
            .track_focus(&self.focus)
            .size_full()
            .items_center()
            .justify_center()
            .p(px(20.))
            .child(
                v_flex()
                    .w(px(width))
                    .max_w_full()
                    .gap(px(12.))
                    .p(px(20.))
                    .rounded(px(10.))
                    .bg(p.surface)
                    .border_1()
                    .border_color(p.border)
                    .child(body),
            )
    }
}

impl AddPlaceView {
    fn heading(&self, text: &str) -> impl IntoElement {
        div()
            .text_size(px(16.))
            .font_weight(FontWeight::SEMIBOLD)
            .child(text.to_string())
    }

    fn kinds(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let card = |id: &'static str,
                    title: &'static str,
                    what: &'static str,
                    action: gpui::AnyElement| {
            h_flex()
                .id(id)
                .gap(px(14.))
                .p(px(12.))
                .rounded(px(8.))
                .border_1()
                .border_color(p.border)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap(px(2.))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                        .child(div().text_color(p.muted).child(what)),
                )
                .child(action)
        };
        let soon = |p: &Palette| chip("Not yet", p).into_any_element();
        v_flex()
            .gap(px(12.))
            .child(self.heading(if self.first {
                "Where should your environments run?"
            } else {
                "Add a place"
            }))
            .when(self.first, |d| {
                d.child(div().text_color(p.muted).child(
                    "You can add more places at any time; most people end up with two or three.",
                ))
            })
            .child(card(
                "kind-local",
                "Local",
                "Run Hangar on this computer, in the background. The app installs and upgrades it.",
                soon(p),
            ))
            .child(card(
                "kind-ssh",
                "A machine over SSH",
                "Your devbox. Needs SSH and KVM there; the app installs Hangar as you, without root.",
                soon(p),
            ))
            .child(card(
                "kind-server",
                "A Hangar server",
                "Any Hangar at an address: your team's, your own cluster or big box. You sign in in the browser; the app keeps the token in the keychain.",
                primary("kind-server-go", "Connect…", p)
                    .on_click(cx.listener(|this, _, _, cx| this.go(Step::Address, cx)))
                    .into_any_element(),
            ))
            .when(!self.first, |d| {
                d.child(
                    h_flex().justify_end().child(
                        button("add-cancel", "Cancel", p)
                            .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                    ),
                )
            })
    }

    fn address(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap(px(12.))
            .child(self.heading("Add a place › A Hangar server"))
            .child(
                div()
                    .text_color(p.muted)
                    .child("Its address. The app checks it, then you sign in in your browser."),
            )
            .child(field("Address", p).child(Input::new(&self.url)))
            .when_some(self.error.clone(), |d, e| {
                d.child(div().text_color(p.bad).child(e))
            })
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("use-token")
                            .text_color(p.accent)
                            .cursor_pointer()
                            .child("Use an access token instead")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.go(Step::Token, cx);
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        button("add-back", "Back", p)
                            .on_click(cx.listener(|this, _, _, cx| this.go(Step::Kinds, cx))),
                    )
                    .child(if self.busy {
                        disabled("add-continue", "Checking…", p)
                    } else {
                        primary("add-continue", "Continue", p).on_click(
                            cx.listener(|this, _, window, cx| this.check_address(window, cx)),
                        )
                    }),
            )
    }

    fn browser(
        &self,
        p: &Palette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let _ = window;
        let Some(w) = &self.waiting else {
            return v_flex();
        };
        let host = store::place_id(&w.url);
        let open = format!("{}{}", w.url, w.grant.verification_path);
        let step = |n: &'static str, title: &'static str| {
            h_flex()
                .gap(px(8.))
                .child(
                    div()
                        .size(px(18.))
                        .flex_none()
                        .rounded_full()
                        .bg(p.accent_soft)
                        .text_color(p.accent)
                        .text_size(px(11.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(n),
                )
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
        };
        v_flex()
            .gap(px(12.))
            .child(self.heading("Add a place › A Hangar server"))
            .child(
                h_flex()
                    .gap(px(8.))
                    .child(div().text_color(p.good).child("✓"))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Address"))
                    .child(div().truncate().child(w.url.clone()))
                    .child(
                        div()
                            .text_color(p.muted)
                            .child(format!("Hangar {}", w.version)),
                    ),
            )
            .child(step("2", "Finish signing in in your browser"))
            .child(div().pl(px(26.)).text_color(p.muted).child(format!(
                "{host} is open in your browser. Sign in there and approve “Hangar Desktop on {}”.",
                computer_name()
            )))
            .child(
                h_flex()
                    .pl(px(26.))
                    .gap(px(10.))
                    .child(div().text_color(p.muted).child("If it asks for a code:"))
                    .child(
                        div()
                            .id("sign-in-code")
                            .debug_selector(|| "sign-in-code".into())
                            .font_family(crate::diff::mono())
                            .text_size(px(18.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(w.grant.user_code.clone()),
                    ),
            )
            .child(
                div().pl(px(26.)).text_color(p.muted).child(
                    self.error
                        .clone()
                        .unwrap_or_else(|| "Waiting for your browser…".into()),
                ),
            )
            .child(step("3", "Name it"))
            .child(
                v_flex()
                    .pl(px(26.))
                    .gap(px(4.))
                    .child(Input::new(&self.name))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(p.muted)
                            .child("Shown in the list; you can rename it later."),
                    ),
            )
            .child(
                h_flex()
                    .gap(px(8.))
                    .justify_end()
                    .child(
                        button("add-reopen", "Open the Browser Again", p)
                            .on_click(move |_, _, cx| cx.open_url(&open)),
                    )
                    .child(
                        button("add-cancel", "Cancel", p)
                            .on_click(cx.listener(|this, _, _, cx| this.go(Step::Address, cx))),
                    ),
            )
    }

    fn token_step(&self, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap(px(12.))
            .child(self.heading("Add a place › A Hangar server"))
            .child(
                div()
                    .text_color(p.muted)
                    .child("Its address, and an access token made on its Account page."),
            )
            .child(field("Address", p).child(Input::new(&self.url)))
            .child(field("Access token", p).child(Input::new(&self.token)))
            .when_some(self.error.clone(), |d, e| {
                d.child(div().text_color(p.bad).child(e))
            })
            .child(
                h_flex()
                    .gap(px(8.))
                    .justify_end()
                    .child(
                        button("add-back", "Back", p)
                            .on_click(cx.listener(|this, _, _, cx| this.go(Step::Address, cx))),
                    )
                    .child(
                        primary("add-submit", "Add", p)
                            .on_click(cx.listener(|this, _, _, cx| this.submit_token(cx))),
                    ),
            )
    }
}

fn field(label: &str, p: &Palette) -> gpui::Div {
    v_flex().gap(px(4.)).child(
        div()
            .text_size(px(11.5))
            .text_color(p.muted)
            .child(SharedString::from(label.to_string())),
    )
}

/// What this computer is called, as the person knows it: macOS's computer
/// name, else its host name.
pub fn computer_name() -> String {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        #[cfg(target_os = "macos")]
        if let Ok(out) = std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
            && out.status.success()
        {
            let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
        host_name().unwrap_or_else(|| "this computer".into())
    })
    .clone()
}

#[cfg(unix)]
fn host_name() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: buf is writable for its length, which is passed.
    let r = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if r != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]).to_string();
    let name = name.strip_suffix(".local").unwrap_or(&name).to_string();
    (!name.is_empty()).then_some(name)
}

#[cfg(not(unix))]
fn host_name() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}
