//! Settings › a place › This Hangar, for a Hangar the app made on a
//! Proxmox: where it runs, its release, which the app moves it on from
//! (the VM restarts into the new release; its environments carry on), and
//! removing it with everything it made there.

use gpui::{
    Context, Entity, FontWeight, IntoElement, ParentElement, Render, Styled, Task, Window, div,
    prelude::*, px,
};
use gpui_component::{h_flex, v_flex};
use hangar_proxmox::hangar::{self, Made, Proxmox};

use crate::palette::{self, Palette};
use crate::place::{PlaceConfig, ProxmoxPlace};
use crate::places::Places;
use crate::ui::{button, disabled, primary};

/// What the page is doing, or did.
#[derive(Debug, Clone, PartialEq)]
enum Doing {
    Nothing,
    Upgrading,
    Upgraded(String),
    /// Removing asked about, waiting for Yes.
    AskingRemove,
    Removing,
}

pub struct ThisHangarView {
    places: Entity<Places>,
    place: String,
    doing: Doing,
    error: Option<String>,
    /// The release it runs, as its server says, and the latest published.
    runs: Option<String>,
    latest: Option<String>,
    /// An upgrade's steps: those done, and the one under way.
    done: Vec<String>,
    now: Option<String>,
    work: Option<Task<()>>,
}

/// What an upgrade says as it goes.
enum Up {
    Step(String),
    Done(String),
    Failed(String),
}

const CHECKING: &str = "Checking version";

impl ThisHangarView {
    pub fn new(places: Entity<Places>, place: &str, cx: &mut Context<Self>) -> Self {
        let mut v = ThisHangarView {
            places,
            place: place.to_string(),
            doing: Doing::Nothing,
            error: None,
            runs: None,
            latest: None,
            done: Vec::new(),
            now: None,
            work: None,
        };
        v.look(cx);
        v
    }

    /// Asks the Hangar what it runs, and the registry what the latest is.
    fn look(&mut self, cx: &mut Context<Self>) {
        let client = self.places.read(cx).client(&self.place);
        let task = cx.background_spawn(async move {
            let runs = client
                .and_then(|c| c.version().ok())
                .map(|v| build_of(&v.server).to_string());
            let latest = latest_version().ok();
            (runs, latest)
        });
        self.work = Some(cx.spawn(async move |this, cx| {
            let (runs, latest) = task.await;
            let _ = this.update(cx, |v, cx| {
                v.runs = runs;
                v.latest = latest;
                // An upgrade ends once the Hangar says which release it runs.
                if v.now.as_deref() == Some(CHECKING)
                    && let Doing::Upgraded(want) = &v.doing
                {
                    v.now = None;
                    match &v.runs {
                        Some(r) if want.starts_with(r.as_str()) || r.starts_with(want.as_str()) => {
                            v.done.push(format!("Running {}", short(r)))
                        }
                        Some(r) => {
                            v.error = Some(format!(
                                "Running {}, not {}. The new version may have failed to start.",
                                short(r),
                                short(want)
                            ))
                        }
                        None => v.error = Some("Could not read the version.".into()),
                    }
                }
                cx.notify();
            });
        }));
    }

    /// The place, and what the app keeps of the VM: the Proxmox to reach
    /// and its token.
    fn made(&self, cx: &gpui::App) -> Option<(PlaceConfig, Proxmox, Made)> {
        let places = self.places.read(cx);
        let (config, _) = places.get(&self.place)?;
        let at = config.proxmox.clone()?;
        let token = places.proxmox_token(&self.place)?;
        let pve = Proxmox::new(&at.url, &token, &at.fingerprint).ok()?;
        let made = Made {
            node: at.node,
            vmid: at.vmid,
            isos: at.isos,
            pool: at.pool,
            prefix: at.prefix,
            address: crate::store::place_id(&config.url)
                .parse()
                .unwrap_or(std::net::Ipv4Addr::UNSPECIFIED),
            url: config.url.clone(),
            fingerprint: config.pin.clone(),
            token: String::new(),
        };
        Some((config.clone(), pve, made))
    }

    fn upgrade(&mut self, cx: &mut Context<Self>) {
        let Some((_, pve, made)) = self.made(cx) else {
            self.error = Some("No Proxmox token is stored for this place.".into());
            cx.notify();
            return;
        };
        self.doing = Doing::Upgrading;
        self.error = None;
        self.done.clear();
        self.now = Some("Downloading release".into());
        cx.notify();
        let dir = cache_dir();
        let (tx, rx) = async_channel::unbounded::<Up>();
        let work = cx.background_spawn(async move {
            let image = format!("{}:latest", hangar::RELEASE_ISO_IMAGE);
            let release = match hangar::fetch_release(&image, &dir) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send_blocking(Up::Failed(format!("Download failed: {e}")));
                    return;
                }
            };
            let size = std::fs::metadata(&release.file)
                .map(|m| m.len() >> 20)
                .unwrap_or(0);
            let _ = tx.send_blocking(Up::Step(format!(
                "Downloaded release {} ({size} MB)",
                short(&release.version)
            )));
            let say = tx.clone();
            let (isos, vmid, url) = (made.isos.clone(), made.vmid, made.url.clone());
            let r = crate::api::runtime().block_on(pve.upgrade(&made, &release, &move |s| {
                let line = match s {
                    hangar::Step::Uploading { name } => format!("Uploading {name} to {isos}"),
                    hangar::Step::Attaching { name } => format!("Attaching {name} to VM {vmid}"),
                    hangar::Step::Restarting => "Restarting VM".into(),
                    _ => format!("Waiting for {url}"),
                };
                let _ = say.send_blocking(Up::Step(line));
            }));
            let _ = tx.send_blocking(match r {
                Ok(()) => Up::Done(release.version),
                Err(e) => Up::Failed(e.to_string()),
            });
        });
        self.work = Some(cx.spawn(async move |this, cx| {
            let _work = work;
            while let Ok(m) = rx.recv().await {
                let over = matches!(m, Up::Done(_) | Up::Failed(_));
                let _ = this.update(cx, |v, cx| {
                    let finished = v.now.take();
                    match m {
                        Up::Step(line) if line.starts_with("Downloaded") => v.done.push(line),
                        Up::Step(line) => {
                            v.done.extend(finished);
                            v.now = Some(line);
                        }
                        Up::Done(version) => {
                            v.done.extend(finished);
                            v.now = Some(CHECKING.into());
                            v.doing = Doing::Upgraded(version);
                            v.look(cx);
                        }
                        Up::Failed(e) => {
                            v.doing = Doing::Nothing;
                            v.error = Some(e);
                        }
                    }
                    cx.notify();
                });
                if over {
                    return;
                }
            }
        }));
    }

    fn remove(&mut self, cx: &mut Context<Self>) {
        let Some((_, pve, made)) = self.made(cx) else {
            self.error = Some("No Proxmox token is stored for this place.".into());
            cx.notify();
            return;
        };
        self.doing = Doing::Removing;
        self.error = None;
        cx.notify();
        let task = cx.background_spawn(async move {
            crate::api::runtime()
                .block_on(pve.remove(&made))
                .map_err(|e| e.to_string())
        });
        let place = self.place.clone();
        self.work = Some(cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| match r {
                Ok(()) => {
                    v.places.update(cx, |p, cx| p.forget(&place, cx));
                    v.doing = Doing::Nothing;
                    cx.notify();
                }
                Err(e) => {
                    v.doing = Doing::Nothing;
                    v.error = Some(e);
                    cx.notify();
                }
            });
        }));
    }
}

/// Where the app keeps a release it fetched to upload.
fn cache_dir() -> std::path::PathBuf {
    crate::store::Dirs::user()
        .map(|d| d.cache.join("proxmox-release"))
        .unwrap_or_else(|| std::env::temp_dir().join("hangar-proxmox-release"))
}

/// The latest published release's version, as its image's label says.
fn latest_version() -> std::io::Result<String> {
    let image = format!("{}:latest", hangar::RELEASE_ISO_IMAGE);
    let r = hangar_images::Reference::parse(&image).map_err(std::io::Error::other)?;
    hangar_images::set_guest_arch("amd64");
    let images = hangar_images::Client::new(Default::default(), None);
    let digest = images.resolve(&r)?;
    let config = images.config(&r, &digest)?;
    config["config"]["Labels"]["org.opencontainers.image.version"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| std::io::Error::other("the latest release does not say its version"))
}

fn short(v: &str) -> String {
    v.chars().take(12).collect()
}

/// The build in a server's description of itself ("hangar-server 0.44"),
/// which names the program first.
fn build_of(server: &str) -> &str {
    server.rsplit(' ').next().unwrap_or(server)
}

/// Its VM, and its size when the app made it.
fn vm_line(at: &ProxmoxPlace) -> String {
    let mut s = format!("{} {}-hangar", at.vmid, at.prefix);
    if at.cores > 0 {
        s += &format!(
            " · {} vCPUs · {} GB RAM · {} GB disk",
            at.cores,
            at.memory_mib / 1024,
            at.data_gib
        );
    }
    s
}

impl Render for ThisHangarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette::of(cx);
        let places = self.places.read(cx);
        let Some((config, _)) = places.get(&self.place) else {
            return v_flex().child(div().text_color(p.muted).child("Removed."));
        };
        let Some(at) = config.proxmox.clone() else {
            return v_flex();
        };
        let name = config.name.clone();
        let row = |k: &str, v: String, p: &Palette| {
            h_flex()
                .max_w(px(560.))
                .gap(px(8.))
                .child(div().flex_none().text_color(p.muted).child(k.to_string()))
                .child(
                    div().flex_1().min_w_0().flex().justify_end().child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::MEDIUM)
                            .child(v),
                    ),
                )
        };
        let newer = match (&self.runs, &self.latest) {
            (Some(r), Some(l)) => !l.starts_with(r.as_str()) && !r.starts_with(l.as_str()),
            _ => false,
        };
        let (done, now) = (self.done.clone(), self.now.clone());
        let (release_note, release_aside) = match (&self.doing, &self.runs, &self.latest) {
            (Doing::Upgrading, ..) => ("Upgrading…".to_string(), None),
            (Doing::Upgraded(v), ..) => (format!("Upgraded to {}", short(v)), None),
            (_, Some(r), Some(l)) if newer => (
                format!("Update available: {}", short(l)),
                Some(format!(" · installed {}", short(r))),
            ),
            (_, Some(r), Some(_)) => (short(r), Some(" · up to date".to_string())),
            (_, Some(r), None) => (short(r), None),
            _ => ("Checking for updates…".into(), None),
        };
        let fingerprint = hangar::show_fingerprint(&config.pin);
        let fingerprint = format!(
            "{}…:{}",
            &fingerprint[..fingerprint.len().min(6)],
            &fingerprint[fingerprint.len().saturating_sub(2)..]
        );
        v_flex()
            .gap(px(6.))
            .child(section("Deployment", &p))
            .child(row(
                "Proxmox",
                format!("{} · node {}", crate::store::place_id(&at.url), at.node),
                &p,
            ))
            .child(row("VM", vm_line(&at), &p))
            .child(row("Address", config.url.clone(), &p))
            .child(row("Certificate", format!("Pinned · SHA-256 {fingerprint}"), &p))
            .child(row("Prefix", at.prefix.clone(), &p))
            .when(!at.disks.is_empty(), |d| d.child(row("Disk storage", at.disks.clone(), &p)))
            .child(row("ISO storage", at.isos.clone(), &p))
            .when(!at.network.is_empty(), |d| d.child(row("Network", at.network.clone(), &p)))
            .child(row("Pool", at.pool.clone(), &p))
            .child(section("Version", &p))
            .child(
                h_flex()
                    .gap(px(10.))
                    .p(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if newer { p.accent } else { p.border })
                    .when(newer, |d| d.bg(p.accent_soft))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).child(release_note))
                                    .when_some(release_aside, |d, a| {
                                        d.child(div().text_color(p.muted).child(a))
                                    }),
                            )
                            .child(div().text_size(px(11.)).text_color(p.muted).child(
                                "The VM restarts during an upgrade; environments keep running. If the new version fails to start, the previous version is restored.",
                            ))
                            .when(!done.is_empty() || now.is_some(), |d| {
                                d.child(
                                    v_flex()
                                        .mt(px(6.))
                                        .gap(px(2.))
                                        .text_size(px(11.5))
                                        .children(done.iter().map(|l| div().child(format!("✓ {l}"))))
                                        .when_some(now.clone(), |d, n| {
                                            d.child(
                                                div().text_color(p.muted).child(format!("◌ {n}")),
                                            )
                                        }),
                                )
                            }),
                    )
                    .child(match self.doing {
                        Doing::Upgrading | Doing::Removing => {
                            disabled("hangar-upgrade", "Upgrade", &p)
                        }
                        _ if newer => primary("hangar-upgrade", "Upgrade", &p)
                            .on_click(cx.listener(|this, _, _, cx| this.upgrade(cx))),
                        _ => button("hangar-upgrade", "Reinstall", &p)
                            .on_click(cx.listener(|this, _, _, cx| this.upgrade(cx))),
                    }),
            )
            .child(section("Remove", &p))
            .child(match self.doing {
                Doing::AskingRemove => h_flex()
                    .gap(px(8.))
                    .p(px(8.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(p.bad)
                    .child(div().flex_1().min_w_0().child(format!(
                        "Remove {name}? VM {}, all environment VMs and their disks, and all ISOs prefixed {}- will be deleted from Proxmox. This cannot be undone.",
                        at.vmid, at.prefix
                    )))
                    .child(
                        button("hangar-remove-no", "Cancel", &p).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.doing = Doing::Nothing;
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        crate::ui::styled_button(
                            "hangar-remove-yes",
                            "Remove",
                            p.bad,
                            p.bad,
                            p.on_accent,
                            p.bad.opacity(0.88),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.remove(cx))),
                    )
                    .into_any_element(),
                Doing::Removing => h_flex()
                    .text_color(p.muted)
                    .child(format!("Removing {name}…"))
                    .into_any_element(),
                _ => h_flex()
                    .id("hangar-remove")
                    .gap(px(8.))
                    .px(px(6.))
                    .py(px(3.))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(p.border)
                    .cursor_pointer()
                    .hover(|d| d.bg(p.surface_2))
                    .child(
                        div()
                            .flex_none()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(p.bad)
                            .child(format!("Remove {name}…")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .justify_end()
                            .child(div().min_w_0().truncate().text_size(px(10.5)).text_color(p.muted).child(format!(
                                "Deletes the VM, its environments and all ISOs prefixed {}-",
                                at.prefix
                            ))),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.doing = Doing::AskingRemove;
                        cx.notify();
                    }))
                    .into_any_element(),
            })
            .when_some(self.error.clone(), |d, e| {
                d.child(div().text_color(p.bad).child(e))
            })
    }
}

fn section(title: &str, p: &Palette) -> gpui::Div {
    div()
        .mt(px(6.))
        .text_size(px(10.5))
        .text_color(p.muted)
        .child(title.to_uppercase())
}
