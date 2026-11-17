//! Settings › a place › Account: who the user is there, its SSH gateway,
//! the keys that sign them in to it, the SSH keys it signs with for them
//! (they never enter an environment), and their access tokens, the app's
//! own marked.

use base64::Engine;
use gpui::{
    ClipboardItem, Context, Entity, FontWeight, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div, prelude::*, px,
};
use gpui_component::input::{Input, InputState};
use gpui_component::{Sizable, h_flex, v_flex};
use sha2::Digest;

use crate::api::{AccessToken, Client, LoginKey, Me, SshKey};
use crate::palette::{self, Palette};
use crate::ui::{button, chip, disabled, primary};

/// What the page has read from the place.
#[derive(Default)]
struct Read {
    me: Option<Me>,
    login_keys: Vec<LoginKey>,
    ssh_keys: Vec<SshKey>,
    tokens: Vec<AccessToken>,
}

/// The password form, while open.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Password {
    /// Setting a first one: no current password.
    Set,
    Change,
}

/// A removal asked about, waiting for Yes.
#[derive(Debug, Clone, PartialEq)]
enum Asking {
    LoginKey(String),
    SshKey(String),
    Token(String),
}

pub struct AccountView {
    client: Option<Client>,
    read: Option<Read>,
    error: Option<String>,
    busy: bool,
    asking: Option<Asking>,
    /// A public key pasted to add.
    pasted: Entity<InputState>,
    /// The name of an SSH key to make.
    key_name: Entity<InputState>,
    /// This computer's own public keys, from ~/.ssh: each one's line and
    /// fingerprint.
    local_keys: Vec<(String, String)>,
    /// The password form, and its fields.
    password: Option<Password>,
    current: Entity<InputState>,
    new_password: Entity<InputState>,
    confirm: Entity<InputState>,
}

impl AccountView {
    pub fn new(client: Option<Client>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let pasted = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("ssh-ed25519 AAAA… comment (contents of ~/.ssh/id_ed25519.pub)")
        });
        let key_name =
            cx.new(|cx| InputState::new(window, cx).placeholder("Key name, e.g. github"));
        let masked = |window: &mut Window, cx: &mut Context<Self>| {
            cx.new(|cx| InputState::new(window, cx).masked(true))
        };
        let (current, new_password, confirm) =
            (masked(window, cx), masked(window, cx), masked(window, cx));
        let mut view = AccountView {
            client,
            read: None,
            error: None,
            busy: false,
            asking: None,
            pasted,
            key_name,
            local_keys: local_keys(),
            password: None,
            current,
            new_password,
            confirm,
        };
        view.reload(cx);
        view
    }

    /// Reads everything again from the place.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            self.error = Some("Sign in to this place to view your account.".into());
            return;
        };
        let task = cx.background_spawn(async move {
            Ok::<_, crate::api::Error>(Read {
                me: Some(client.me()?),
                login_keys: client.login_keys()?,
                ssh_keys: client.ssh_keys()?,
                tokens: client.tokens()?,
            })
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                match r {
                    Ok(read) => {
                        v.read = Some(read);
                        v.error = None;
                    }
                    Err(e) => v.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Does something to the account, then reads it again.
    fn change(
        &mut self,
        f: impl FnOnce(&Client) -> Result<(), crate::api::Error> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.busy = true;
        self.asking = None;
        cx.notify();
        let task = cx.background_spawn(async move { f(&client) });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                v.error = r.err().map(|e| e.to_string());
                v.reload(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn add_key(&mut self, line: String, window: &mut Window, cx: &mut Context<Self>) {
        if line.trim().is_empty() {
            return;
        }
        self.pasted.update(cx, |s, cx| s.set_value("", window, cx));
        self.change(move |c| c.add_login_key(&line).map(|_| ()), cx);
    }

    fn make_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.key_name.read(cx).value().trim().to_string();
        if name.is_empty() {
            self.error = Some("Enter a name for the key.".into());
            cx.notify();
            return;
        }
        self.key_name
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.change(move |c| c.generate_ssh_key(&name).map(|_| ()), cx);
    }

    /// Sets or changes the password as the form says, the form closing
    /// once it is done.
    fn save_password(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(form) = self.password else {
            return;
        };
        let current = self.current.read(cx).value().to_string();
        let new = self.new_password.read(cx).value().to_string();
        if new.chars().count() < 8 {
            self.error = Some("The password must be at least 8 characters.".into());
            cx.notify();
            return;
        }
        if new != self.confirm.read(cx).value().as_ref() {
            self.error = Some("The passwords do not match.".into());
            cx.notify();
            return;
        }
        for f in [&self.current, &self.new_password, &self.confirm] {
            f.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.password = None;
        self.change(
            move |c| {
                let current = (form == Password::Change).then_some(current.as_str());
                c.change_password(current, &new)
            },
            cx,
        );
    }

    /// The Password section: whether one is set, and the form to set or
    /// change it.
    fn password_section(&self, me: &Me, p: &Palette, cx: &mut Context<Self>) -> gpui::AnyElement {
        let set = me.has_password == Some(true);
        let mut section = v_flex().gap(px(8.)).child(section("Password", p));
        let Some(form) = self.password else {
            let (state, help) = match (set, me.admin) {
                (true, _) => ("Set", "Signs you in to this place's web pages."),
                (false, true) => (
                    "Not set",
                    "Set a password to sign in to this place's web pages from other devices.",
                ),
                (false, false) => (
                    "Not set",
                    "An administrator can set a password for your account.",
                ),
            };
            let mut row = h_flex()
                .gap(px(10.))
                .child(div().w(px(84.)).text_color(p.muted).child(state))
                .child(div().flex_1().min_w_0().text_color(p.muted).child(help));
            if set || me.admin {
                let (label, form) = if set {
                    ("Change Password…", Password::Change)
                } else {
                    ("Set Password…", Password::Set)
                };
                row = row.child(button("password-open", label, p).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.password = Some(form);
                        this.error = None;
                        cx.notify();
                    },
                )));
            }
            return section.child(row).into_any_element();
        };
        let field = |label: &'static str, input: &Entity<InputState>| {
            h_flex()
                .gap(px(10.))
                .child(
                    div()
                        .w(px(120.))
                        .flex_none()
                        .text_color(p.muted)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().child(Input::new(input)))
        };
        if form == Password::Change {
            section = section.child(field("Current password", &self.current));
        }
        section
            .child(field("New password", &self.new_password))
            .child(field("Confirm", &self.confirm))
            .child(
                h_flex()
                    .gap(px(8.))
                    .justify_end()
                    .child(button("password-cancel", "Cancel", p).on_click(cx.listener(
                        |this, _, window, cx| {
                            this.password = None;
                            for f in [&this.current, &this.new_password, &this.confirm] {
                                f.update(cx, |s, cx| s.set_value("", window, cx));
                            }
                            cx.notify();
                        },
                    )))
                    .child(if self.busy {
                        disabled("password-save", "Saving…", p).into_any_element()
                    } else {
                        primary(
                            "password-save",
                            if form == Password::Set {
                                "Set Password"
                            } else {
                                "Change Password"
                            },
                            p,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.save_password(window, cx)))
                        .into_any_element()
                    }),
            )
            .into_any_element()
    }

    /// A Remove button, or Yes and No once it is asked.
    fn remove_button(
        &self,
        id: String,
        asking: Asking,
        label: &'static str,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        if self.asking.as_ref() == Some(&asking) {
            let yes = asking.clone();
            return h_flex()
                .gap(px(6.))
                .child(div().text_color(p.bad).child(format!("{label}?")))
                .child(
                    button(SharedString::from(format!("{id}-yes")), "Yes", p).on_click(
                        cx.listener(move |this, _, _, cx| {
                            let a = yes.clone();
                            this.change(
                                move |c| match a {
                                    Asking::LoginKey(id) => c.delete_login_key(&id),
                                    Asking::SshKey(id) => c.delete_ssh_key(&id),
                                    Asking::Token(id) => c.revoke_token(&id),
                                },
                                cx,
                            )
                        }),
                    ),
                )
                .child(
                    button(SharedString::from(format!("{id}-no")), "No", p).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.asking = None;
                            cx.notify();
                        },
                    )),
                )
                .into_any_element();
        }
        button(SharedString::from(id), label, p)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.asking = Some(asking.clone());
                cx.notify();
            }))
            .into_any_element()
    }
}

impl Render for AccountView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let mut page = v_flex().gap(px(10.));
        if let Some(e) = &self.error {
            page = page.child(div().text_color(p.bad).child(e.clone()));
        }
        let Some(read) = &self.read else {
            return page.child(div().text_color(p.muted).child(if self.client.is_some() {
                "Loading account…"
            } else {
                ""
            }));
        };
        let login_keys = read.login_keys.clone();
        let ssh_keys = read.ssh_keys.clone();
        let tokens = read.tokens.clone();
        if let Some(me) = &read.me {
            let name = if me.display_name.is_empty() {
                me.username.clone()
            } else {
                format!("{} ({})", me.display_name, me.username)
            };
            page = page.child(
                h_flex()
                    .gap(px(8.))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(name))
                    .when(me.admin, |d| d.child(chip("Admin", &p))),
            );
            if let Some(gw) = &me.ssh {
                let cmd = if gw.port == 22 {
                    format!("ssh <environment>@{}", gw.host)
                } else {
                    format!("ssh <environment>@{} -p {}", gw.host, gw.port)
                };
                page = page.child(
                    h_flex()
                        .gap(px(8.))
                        .child(div().text_color(p.muted).child("SSH gateway"))
                        .child(div().font_family(crate::diff::mono()).child(cmd)),
                );
            }
        }

        if let Some(me) = &read.me
            && me.has_password.is_some()
        {
            page = page.child(self.password_section(me, &p, cx));
        }

        // Sign-in keys, and this computer's own keys not among them.
        page = page.child(section("Sign-in keys", &p)).child(note(
            "Public keys for signing in to this place's SSH gateway.",
            &p,
        ));
        let mut list = v_flex().gap(px(4.));
        for k in &login_keys {
            list = list.child(
                h_flex()
                    .gap(px(10.))
                    .child(div().w(px(160.)).truncate().child(k.name.clone()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(p.muted)
                            .font_family(crate::diff::mono())
                            .text_size(px(11.))
                            .child(k.fingerprint.clone()),
                    )
                    .child(
                        div()
                            .text_color(p.muted)
                            .child(k.created_at.format("%-d %b %Y").to_string()),
                    )
                    .child(self.remove_button(
                        format!("login-remove-{}", k.id),
                        Asking::LoginKey(k.id.clone()),
                        "Remove",
                        &p,
                        cx,
                    )),
            );
        }
        if login_keys.is_empty() {
            list = list.child(div().text_color(p.muted).child("No sign-in keys."));
        }
        page = page.child(list);
        let offered: Vec<(String, String)> = self
            .local_keys
            .iter()
            .filter(|(_, fp)| !login_keys.iter().any(|k| &k.fingerprint == fp))
            .cloned()
            .collect();
        for (line, _) in offered {
            let label = line
                .split_whitespace()
                .nth(2)
                .unwrap_or("unnamed key")
                .to_string();
            let add = line.clone();
            page = page.child(
                h_flex()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_color(p.muted)
                            .child(format!("Found in ~/.ssh: {label}")),
                    )
                    .child(
                        button(
                            SharedString::from(format!("login-add-local-{label}")),
                            "Add",
                            &p,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| this.add_key(add.clone(), window, cx),
                        )),
                    ),
            );
        }
        page = page.child(
            h_flex()
                .gap(px(8.))
                .child(div().flex_1().child(Input::new(&self.pasted).small()))
                .child(button("login-add", "Add Key", &p).on_click(cx.listener(
                    |this, _, window, cx| {
                        let line = this.pasted.read(cx).value().to_string();
                        this.add_key(line, window, cx)
                    },
                ))),
        );

        // The keys the place signs with.
        page = page.child(section("SSH keys", &p)).child(note(
            "Keys stored by this place and used through the SSH agent in your environments. Private keys never enter an environment; add the public key where access is needed, such as GitHub.",
            &p,
        ));
        let mut list = v_flex().gap(px(4.));
        for k in &ssh_keys {
            let public = k.public_key.clone();
            list = list.child(
                h_flex()
                    .gap(px(10.))
                    .child(div().w(px(160.)).truncate().child(k.name.clone()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(p.muted)
                            .font_family(crate::diff::mono())
                            .text_size(px(11.))
                            .child(k.fingerprint.clone()),
                    )
                    .child(
                        button(
                            SharedString::from(format!("ssh-copy-{}", k.id)),
                            "Copy Public Key",
                            &p,
                        )
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(public.clone()))
                        }),
                    )
                    .child(self.remove_button(
                        format!("ssh-remove-{}", k.id),
                        Asking::SshKey(k.id.clone()),
                        "Remove",
                        &p,
                        cx,
                    )),
            );
        }
        if ssh_keys.is_empty() {
            list = list.child(div().text_color(p.muted).child("No SSH keys."));
        }
        page = page.child(list).child(
            h_flex()
                .gap(px(8.))
                .child(div().w(px(240.)).child(Input::new(&self.key_name).small()))
                .child(if self.busy {
                    disabled("ssh-make", "Generate Key", &p)
                } else {
                    button("ssh-make", "Generate Key", &p)
                        .on_click(cx.listener(|this, _, window, cx| this.make_key(window, cx)))
                }),
        );

        // Access tokens.
        page = page.child(section("Access tokens", &p)).child(note(
            "Tokens authenticate tools and scripts. This app's token is stored in the keychain; revoking it signs the app out of this place.",
            &p,
        ));
        let mut list = v_flex().gap(px(4.));
        for t in &tokens {
            let used = match t.last_used_at {
                Some(at) => format!("used {}", at.format("%-d %b %Y")),
                None => "never used".into(),
            };
            let expires = match t.expires_at {
                Some(at) => format!(" · expires {}", at.format("%-d %b %Y")),
                None => String::new(),
            };
            list = list.child(
                h_flex()
                    .gap(px(10.))
                    .child(
                        h_flex()
                            .w(px(220.))
                            .gap(px(6.))
                            .child(div().truncate().child(t.name.clone()))
                            .when(t.current, |d| d.child(chip("This app", &p))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(p.muted)
                            .child(format!(
                                "created {} · {used}{expires}",
                                t.created_at.format("%-d %b %Y")
                            )),
                    )
                    .child(self.remove_button(
                        format!("token-revoke-{}", t.id),
                        Asking::Token(t.id.clone()),
                        "Revoke",
                        &p,
                        cx,
                    )),
            );
        }
        if tokens.is_empty() {
            list = list.child(div().text_color(p.muted).child("No access tokens."));
        }
        page.child(list).child(
            h_flex().child(
                primary("account-reload", "Refresh", &p)
                    .on_click(cx.listener(|this, _, _, cx| this.reload(cx))),
            ),
        )
    }
}

fn section(title: &str, p: &Palette) -> impl IntoElement {
    div()
        .pt(px(6.))
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(p.muted)
        .child(title.to_uppercase())
}

fn note(text: &str, p: &Palette) -> impl IntoElement {
    div()
        .max_w(px(560.))
        .text_size(px(11.5))
        .text_color(p.muted)
        .child(text.to_string())
}

/// This computer's public keys, from ~/.ssh/*.pub: each one's line and its
/// fingerprint as ssh-keygen -l prints it.
fn local_keys() -> Vec<(String, String)> {
    let Some(dir) = dirs::home_dir().map(|h| h.join(".ssh")) else {
        return vec![];
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    let mut out = vec![];
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("pub") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let line = text.lines().next().unwrap_or_default().trim().to_string();
        if let Some(fp) = fingerprint(&line) {
            out.push((line, fp));
        }
    }
    out.sort();
    out
}

/// A public key's SHA256 fingerprint, from its authorized_keys line.
pub fn fingerprint(line: &str) -> Option<String> {
    let blob = line.split_whitespace().nth(1)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(blob)
        .ok()?;
    let sum = sha2::Sha256::digest(&bytes);
    Some(format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(sum)
    ))
}
