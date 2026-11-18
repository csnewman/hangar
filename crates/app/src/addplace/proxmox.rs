//! Add a place › Proxmox, as the design draws it: the Proxmox's address and
//! its certificate matched with the one Proxmox shows, an API token checked
//! against what a Hangar needs of it, where the Hangar goes, its name and
//! prefix, then making it (`hangar_proxmox::hangar`): a Hangar of the
//! person's own in a VM there, added as a place with its certificate pinned
//! and its first access token in the keychain. A Hangar already there is
//! reconnected instead (`Proxmox::reconnect`), as on a computer that has
//! lost the token it was made with.

use gpui::{
    AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, Styled, Task, Window, div,
    prelude::*, px,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::{h_flex, v_flex};
use hangar_proxmox::hangar::{self, Choices, Found, Made, Plan, Proxmox};

use super::{AddPlaceEvent, AddPlaceView, Step, field};
use crate::palette::Palette;
use crate::place::ProxmoxPlace;
use crate::ui::{button, disabled, primary};

/// What the Hangar's own VM is: enough for Hangar's server and worker.
/// Environments are VMs of their own, sized by their templates.
const CORES: u32 = 2;
const MEMORY_MIB: u32 = 2048;
const DATA_GIB: u32 = 64;

/// The flow's state, beside the view's own.
pub struct Flow {
    url: Entity<InputState>,
    token_id: Entity<InputState>,
    secret: Entity<InputState>,
    prefix: Entity<InputState>,
    /// The Proxmox's API, made whole, and its certificate, once seen.
    api: String,
    fingerprint: String,
    /// The token, once checked, what it may use, and the Hangars already
    /// there.
    token: String,
    choices: Option<Choices>,
    found: Vec<Found>,
    node: usize,
    disks: usize,
    isos: usize,
    network: usize,
    pool: usize,
    /// Whether the prefix was typed, rather than made of the name, and
    /// whether the name is setting it.
    prefix_typed: bool,
    setting_prefix: bool,
    /// Making it, or reconnecting one found: the steps done, the one under
    /// way, and the work.
    reconnecting: bool,
    done: Vec<String>,
    now: Option<String>,
    making: Option<Task<()>>,
}

impl Flow {
    pub fn new(
        name: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<AddPlaceView>,
    ) -> (Flow, Vec<gpui::Subscription>) {
        let url = cx.new(|cx| InputState::new(window, cx).placeholder("pve.home.lan:8006"));
        let token_id = cx.new(|cx| InputState::new(window, cx).placeholder("hangar@pve!desktop"));
        let secret = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx")
                .masked(true)
        });
        let prefix = cx.new(|cx| InputState::new(window, cx));
        let subs = vec![
            cx.subscribe_in(&url, window, |this, _, e: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = e {
                    this.pve_probe(window, cx);
                }
            }),
            cx.subscribe_in(&secret, window, |this, _, e: &InputEvent, _, cx| {
                if let InputEvent::PressEnter { .. } = e {
                    this.pve_check(cx);
                }
            }),
            cx.subscribe_in(name, window, |this, _, e: &InputEvent, window, cx| {
                if let InputEvent::Change = e
                    && this.step == Step::ProxmoxName
                {
                    this.pve_prefix_from_name(window, cx);
                }
            }),
            cx.subscribe_in(&prefix, window, |this, _, e: &InputEvent, _, cx| {
                if let InputEvent::Change = e
                    && !this.pve.setting_prefix
                {
                    this.pve.prefix_typed = true;
                    cx.notify();
                }
            }),
        ];
        let flow = Flow {
            url,
            token_id,
            secret,
            prefix,
            api: String::new(),
            fingerprint: String::new(),
            token: String::new(),
            choices: None,
            found: vec![],
            node: 0,
            disks: 0,
            isos: 0,
            network: 0,
            pool: 0,
            prefix_typed: false,
            setting_prefix: false,
            reconnecting: false,
            done: vec![],
            now: None,
            making: None,
        };
        (flow, subs)
    }

    fn host(&self) -> String {
        crate::store::place_id(&self.api)
    }

    fn chosen_node(&self) -> Option<&hangar::Node> {
        self.choices.as_ref()?.nodes.get(self.node)
    }

    /// Where the Hangar goes, as chosen.
    fn plan(&self, prefix: &str) -> Option<Plan> {
        let c = self.choices.as_ref()?;
        let n = c.nodes.get(self.node)?;
        Some(Plan {
            node: n.name.clone(),
            disks: n.disks.get(self.disks)?.id.clone(),
            isos: n.isos.get(self.isos)?.id.clone(),
            network: n.networks.get(self.network)?.name.clone(),
            pool: c.pools.get(self.pool)?.0.clone(),
            prefix: prefix.to_string(),
            admin: admin_name(),
            cores: CORES,
            memory_mib: MEMORY_MIB,
            data_gib: DATA_GIB,
        })
    }
}

/// A Proxmox's API from what was typed: https, and its port, 8006 unless
/// given.
fn api_url(typed: &str) -> Option<String> {
    let s = typed.trim().trim_end_matches('/');
    let s = s.strip_prefix("https://").unwrap_or(s);
    if s.is_empty() || s.contains(['/', ' ']) || s.starts_with("http://") {
        return None;
    }
    let has_port = s
        .rsplit_once(':')
        .is_some_and(|(h, p)| !h.ends_with(':') && p.parse::<u16>().is_ok() && !s.ends_with(']'));
    Some(if has_port {
        format!("https://{s}")
    } else {
        format!("https://{s}:8006")
    })
}

/// A place's name made of a Hangar's prefix: homelab is Homelab, team-2 is
/// Team 2.
fn name_of(prefix: &str) -> String {
    let mut name = prefix.replace('-', " ");
    if let Some(f) = name.get_mut(0..1) {
        f.make_ascii_uppercase();
    }
    name
}

/// What a step of making or reconnecting a Hangar says.
fn step_line(s: hangar::Step, isos: &str, pool: &str, prefix: &str, vmid: u32) -> String {
    match s {
        hangar::Step::Uploading { name } => format!("Uploading {name} to {isos}"),
        hangar::Step::Making { vmid } => {
            format!("Creating VM {vmid} ({prefix}-hangar) in pool {pool}")
        }
        hangar::Step::Starting => "Starting VM".into(),
        hangar::Step::Settings if vmid != 0 => "Applying new access token and certificate".into(),
        hangar::Step::Settings => "Applying settings".into(),
        hangar::Step::Address => "Waiting for IP address".into(),
        hangar::Step::Answering { url } => format!("Waiting for {url}"),
        hangar::Step::Attaching { name } if vmid != 0 => format!("Attaching {name} to VM {vmid}"),
        hangar::Step::Attaching { name } => format!("Attaching {name}"),
        hangar::Step::Restarting => "Restarting VM".into(),
    }
}

/// The first administrator's name: this computer's user's, as a Hangar
/// username takes it, else owner.
pub fn admin_name() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_default();
    let name: String = user
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .take(63)
        .collect();
    if name.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        name.to_ascii_lowercase()
    } else {
        "owner".into()
    }
}

fn gib(bytes: u64) -> String {
    let g = bytes as f64 / (1u64 << 30) as f64;
    if g >= 1024.0 {
        format!("{:.1} TB", g / 1024.0)
    } else {
        format!("{g:.0} GB")
    }
}

/// A node's processors: cores and threads when both are known.
fn cpus(n: &hangar::Node) -> String {
    match n.cores {
        0 => format!("{} CPUs", n.cpus),
        c if c == n.cpus => format!("{c} cores"),
        c => format!("{c} cores · {} threads", n.cpus),
    }
}

impl AddPlaceView {
    /// Looks at the certificate the Proxmox presents, for the person to
    /// match with the one Proxmox shows.
    pub fn pve_probe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.pve.url.read(cx).value().to_string();
        let Some(api) = api_url(&typed) else {
            self.error = Some(
                "Invalid address. Enter a host name or IP address, optionally with a port.".into(),
            );
            cx.notify();
            return;
        };
        self.busy = true;
        self.error = None;
        self.pve.fingerprint.clear();
        cx.notify();
        let url = api.clone();
        let task = cx.background_spawn(async move {
            crate::api::runtime().block_on(hangar::certificate(&url))
        });
        cx.spawn_in(window, async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match r {
                    Ok(fp) => {
                        v.pve.api = api;
                        v.pve.fingerprint = fp;
                    }
                    Err(e) => v.error = Some(format!("Could not connect to {api}: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Checks the token: what it may use, and what a Hangar needs of it.
    pub fn pve_check(&mut self, cx: &mut Context<Self>) {
        let id = self.pve.token_id.read(cx).value().trim().to_string();
        let secret = self.pve.secret.read(cx).value().trim().to_string();
        if !id.contains('!') || secret.is_empty() {
            self.error = Some("Enter the token ID (user@realm!name) and its secret.".into());
            cx.notify();
            return;
        }
        self.pve_check_token(format!("{id}={secret}"), cx);
    }

    /// Checks a token file (`USER@REALM!ID=SECRET`), as `proxmox.token_file`
    /// holds one, read by the app: the dev build's control port, which
    /// drives the flow with a token nobody types.
    #[cfg(feature = "control")]
    pub fn pve_check_file(&mut self, path: &str, cx: &mut Context<Self>) -> Result<(), String> {
        if self.pve.fingerprint.is_empty() {
            return Err("the Proxmox's address is not checked yet".into());
        }
        let token = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let token = token.trim();
        if !token
            .split_once('=')
            .is_some_and(|(id, s)| id.contains('!') && !s.is_empty())
        {
            return Err("the file holds no USER@REALM!ID=SECRET".into());
        }
        self.pve_check_token(token.to_string(), cx);
        Ok(())
    }

    fn pve_check_token(&mut self, token: String, cx: &mut Context<Self>) {
        let pve = match Proxmox::new(&self.pve.api, &token, &self.pve.fingerprint) {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(e.to_string());
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.error = None;
        self.pve.choices = None;
        cx.notify();
        let task = cx.background_spawn(async move {
            crate::api::runtime().block_on(async {
                let c = pve.choices().await?;
                Ok::<_, std::io::Error>((c, pve.hangars().await.unwrap_or_default()))
            })
        });
        cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |v, cx| {
                v.busy = false;
                match r {
                    Ok((c, found)) => {
                        v.pve.token = token;
                        v.pve.found = found;
                        // The node with the most free memory that has all a
                        // Hangar needs, and the roomiest of each there.
                        v.pve.node = c
                            .nodes
                            .iter()
                            .position(|n| {
                                !n.disks.is_empty() && !n.isos.is_empty() && !n.networks.is_empty()
                            })
                            .unwrap_or(0);
                        (v.pve.disks, v.pve.isos, v.pve.network, v.pve.pool) = (0, 0, 0, 0);
                        v.pve.choices = Some(c);
                    }
                    Err(e) => {
                        v.error = Some(match e.kind() {
                            std::io::ErrorKind::PermissionDenied => {
                                "Authentication failed. Check the token ID and secret.".into()
                            }
                            _ => e.to_string(),
                        })
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Whether the token has all a Hangar needs.
    fn pve_ready(&self) -> bool {
        self.pve
            .choices
            .as_ref()
            .is_some_and(|c| c.needs.iter().all(|n| n.ok))
            && self.pve.plan("x").is_some()
    }

    /// Goes to naming it, its name and prefix made from the node's.
    fn pve_to_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.name.read(cx).value().trim().is_empty() {
            let n = self
                .pve
                .chosen_node()
                .map(|n| n.name.clone())
                .unwrap_or_else(|| "Homelab".into());
            let mut name = n.clone();
            if let Some(f) = name.get_mut(0..1) {
                f.make_ascii_uppercase();
            }
            self.name.update(cx, |s, cx| s.set_value(name, window, cx));
        }
        self.go(Step::ProxmoxName, cx);
        self.pve_prefix_from_name(window, cx);
    }

    /// Sets the prefix from the name, unless one was typed.
    fn pve_prefix_from_name(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pve.prefix_typed {
            return;
        }
        let p = hangar::prefix_of(self.name.read(cx).value().as_ref());
        self.pve.setting_prefix = true;
        self.pve
            .prefix
            .update(cx, |s, cx| s.set_value(p, window, cx));
        self.pve.setting_prefix = false;
        cx.notify();
    }

    /// Why the prefix will not do, if it will not.
    fn pve_prefix_problem(&self, cx: &gpui::App) -> Option<String> {
        let p = self.pve.prefix.read(cx).value().to_string();
        if let Err(e) = hangar::check_prefix(&p) {
            return Some(format!(
                "Prefix {}",
                e.split_once(": ").map_or(e.as_str(), |(_, why)| why)
            ));
        }
        let used = self.pve.choices.as_ref()?.prefixes.contains(&p);
        used.then(|| format!("Prefix {p} is already in use on this Proxmox."))
    }

    /// Makes the Hangar: the release fetched, then made on the Proxmox,
    /// each step said as it goes; then adds it as a place.
    fn pve_make(&mut self, cx: &mut Context<Self>) {
        let prefix = self.pve.prefix.read(cx).value().to_string();
        let name = self.name.read(cx).value().trim().to_string();
        let Some(plan) = self.pve.plan(&prefix) else {
            return;
        };
        let pve = match Proxmox::new(&self.pve.api, &self.pve.token, &self.pve.fingerprint) {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(e.to_string());
                cx.notify();
                return;
            }
        };
        self.go(Step::Making, cx);
        self.pve.reconnecting = false;
        self.pve.done.clear();
        self.pve.now = Some("Downloading release".into());
        let dir = crate::store::Dirs::user()
            .map(|d| d.cache.join("proxmox-release"))
            .unwrap_or_else(|| std::env::temp_dir().join("hangar-proxmox-release"));
        let (tx, rx) = async_channel::unbounded::<Made2>();
        let work = {
            let plan = plan.clone();
            cx.background_spawn(async move {
                let image = format!("{}:latest", hangar::RELEASE_ISO_IMAGE);
                let release = match hangar::fetch_release(&image, &dir) {
                    Ok(r) => r,
                    Err(e) => {
                        let _ = tx.send_blocking(Made2::Failed(format!("Download failed: {e}")));
                        return;
                    }
                };
                let short: String = release.version.chars().take(7).collect();
                let _ = tx.send_blocking(Made2::Step(format!(
                    "Downloaded release {short} ({} MB)",
                    std::fs::metadata(&release.file)
                        .map(|m| m.len() >> 20)
                        .unwrap_or(0)
                )));
                let say = tx.clone();
                let (isos, pool, prefix) =
                    (plan.isos.clone(), plan.pool.clone(), plan.prefix.clone());
                let r = crate::api::runtime().block_on(pve.make(&plan, &release, &move |s| {
                    let line = step_line(s, &isos, &pool, &prefix, 0);
                    let _ = say.send_blocking(Made2::Step(line));
                }));
                let _ = tx.send_blocking(match r {
                    Ok(m) => Made2::Done(Box::new(m)),
                    Err(e) => Made2::Failed(e.to_string()),
                });
            })
        };
        let place = ProxmoxPlace {
            url: self.pve.api.clone(),
            fingerprint: self.pve.fingerprint.clone(),
            node: plan.node.clone(),
            vmid: 0,
            isos: plan.isos.clone(),
            pool: plan.pool.clone(),
            prefix: plan.prefix.clone(),
            disks: plan.disks.clone(),
            network: plan.network.clone(),
            cores: plan.cores,
            memory_mib: plan.memory_mib,
            data_gib: plan.data_gib,
        };
        let token = self.pve.token.clone();
        self.pve.making = Some(cx.spawn(async move |this, cx| {
            let _work = work;
            while let Ok(m) = rx.recv().await {
                let finished = this
                    .update(cx, |v, cx| v.pve_progress(m, &name, &place, &token, cx))
                    .unwrap_or(true);
                if finished {
                    return;
                }
            }
        }));
        cx.notify();
    }

    /// Reconnects the Hangar found here at i: the latest release fetched
    /// if it boots another, then a new access token and certificate given
    /// to it, each step said as it goes; then adds it as a place.
    fn pve_reconnect(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(at) = self.pve.found.get(i).cloned() else {
            return;
        };
        let pve = match Proxmox::new(&self.pve.api, &self.pve.token, &self.pve.fingerprint) {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(e.to_string());
                cx.notify();
                return;
            }
        };
        self.go(Step::Making, cx);
        self.pve.reconnecting = true;
        self.pve.done.clear();
        self.pve.now = Some("Checking the latest release".into());
        let dir = crate::store::Dirs::user()
            .map(|d| d.cache.join("proxmox-release"))
            .unwrap_or_else(|| std::env::temp_dir().join("hangar-proxmox-release"));
        let (tx, rx) = async_channel::unbounded::<Made2>();
        let work = {
            let at = at.clone();
            cx.background_spawn(async move {
                let image = format!("{}:latest", hangar::RELEASE_ISO_IMAGE);
                let latest = match hangar::release_version(&image) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = tx.send_blocking(Made2::Failed(format!(
                            "Could not check for updates: {e}"
                        )));
                        return;
                    }
                };
                let short: String = latest.chars().take(7).collect();
                let release = if hangar::release_key(&latest) == at.release {
                    let _ = tx.send_blocking(Made2::Step(format!("Release {short} is installed")));
                    None
                } else {
                    let _ = tx.send_blocking(Made2::Step("Downloading release".into()));
                    match hangar::fetch_release(&image, &dir) {
                        Ok(r) => {
                            let _ = tx.send_blocking(Made2::Step(format!(
                                "Downloaded release {short} ({} MB)",
                                std::fs::metadata(&r.file)
                                    .map(|m| m.len() >> 20)
                                    .unwrap_or(0)
                            )));
                            Some(r)
                        }
                        Err(e) => {
                            let _ =
                                tx.send_blocking(Made2::Failed(format!("Download failed: {e}")));
                            return;
                        }
                    }
                };
                let say = tx.clone();
                let (isos, pool, prefix, vmid) =
                    (at.isos.clone(), at.pool.clone(), at.prefix.clone(), at.vmid);
                let r = crate::api::runtime().block_on(pve.reconnect(
                    &at,
                    release.as_ref(),
                    &admin_name(),
                    &move |s| {
                        let line = step_line(s, &isos, &pool, &prefix, vmid);
                        let _ = say.send_blocking(Made2::Step(line));
                    },
                ));
                let _ = tx.send_blocking(match r {
                    Ok(m) => Made2::Done(Box::new(m)),
                    Err(e) => Made2::Failed(format!("Could not reconnect: {e}")),
                });
            })
        };
        let place = ProxmoxPlace {
            url: self.pve.api.clone(),
            fingerprint: self.pve.fingerprint.clone(),
            node: at.node.clone(),
            vmid: at.vmid,
            isos: at.isos.clone(),
            pool: at.pool.clone(),
            prefix: at.prefix.clone(),
            disks: at.disks.clone(),
            network: at.network.clone(),
            cores: at.cores,
            memory_mib: at.memory_mib,
            data_gib: at.data_gib,
        };
        let name = name_of(&at.prefix);
        let token = self.pve.token.clone();
        self.pve.making = Some(cx.spawn(async move |this, cx| {
            let _work = work;
            while let Ok(m) = rx.recv().await {
                let finished = this
                    .update(cx, |v, cx| v.pve_progress(m, &name, &place, &token, cx))
                    .unwrap_or(true);
                if finished {
                    return;
                }
            }
        }));
        cx.notify();
    }

    /// The name of the place a Hangar found here is already added as.
    fn pve_added_as(&self, f: &Found, cx: &gpui::App) -> Option<String> {
        self.places
            .read(cx)
            .list()
            .iter()
            .find(|(c, _)| {
                c.proxmox.as_ref().is_some_and(|x| {
                    x.url == self.pve.api && x.prefix == f.prefix && x.vmid == f.vmid
                })
            })
            .map(|(c, _)| c.name.clone())
    }

    /// The Hangars already on this Proxmox, each to reconnect.
    fn pve_found(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        if self.pve.found.is_empty() {
            return div().into_any_element();
        }
        let rows = self.pve.found.iter().enumerate().map(|(i, f)| {
            let detail = format!(
                "VM {} · node {} · {}",
                f.vmid,
                f.node,
                match (f.running, f.address) {
                    (true, Some(a)) => a.to_string(),
                    (true, None) => "running".into(),
                    (false, _) => "stopped".into(),
                }
            );
            let action = match self.pve_added_as(f, cx) {
                Some(name) => div()
                    .text_size(px(11.5))
                    .text_color(p.muted)
                    .child(format!("Added as {name}"))
                    .into_any_element(),
                None if self.busy => disabled(
                    gpui::SharedString::from(format!("pve-reconnect-{i}")),
                    "Reconnect",
                    p,
                )
                .into_any_element(),
                None => button(
                    gpui::SharedString::from(format!("pve-reconnect-{i}")),
                    "Reconnect",
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.pve_reconnect(i, cx)))
                .into_any_element(),
            };
            h_flex()
                .gap(px(8.))
                .px(px(9.))
                .py(px(5.))
                .rounded(px(6.))
                .border_1()
                .border_color(p.border)
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().child(format!("{}-hangar", f.prefix)))
                        .child(div().text_size(px(11.)).text_color(p.muted).child(detail)),
                )
                .child(action)
        });
        v_flex()
            .gap(px(6.))
            .pt(px(4.))
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Existing Hangars"))
            .children(rows)
            .child(div().text_size(px(11.5)).text_color(p.muted).child(
                "Reconnect creates a new access token and certificate for this Mac. Environments and data are kept. The previous access token stops working.",
            ))
            .into_any_element()
    }

    /// Takes a step of making it; whether it is over.
    fn pve_progress(
        &mut self,
        m: Made2,
        name: &str,
        place: &ProxmoxPlace,
        token: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        match m {
            Made2::Step(line) => {
                if let Some(now) = self.pve.now.take()
                    && !now.starts_with("Downloading")
                    && !now.starts_with("Checking")
                {
                    self.pve.done.push(now);
                }
                if line.starts_with("Downloaded") || line.starts_with("Release ") {
                    self.pve.done.push(line);
                } else {
                    self.pve.now = Some(line);
                }
                cx.notify();
                false
            }
            Made2::Failed(e) => {
                self.pve.now = None;
                self.error = Some(e);
                cx.notify();
                true
            }
            Made2::Done(made) => {
                let place = ProxmoxPlace {
                    vmid: made.vmid,
                    ..place.clone()
                };
                let r = self
                    .places
                    .update(cx, |p, cx| p.add_made(name, &made, place, token, cx));
                match r {
                    Ok(id) => cx.emit(AddPlaceEvent::Done(Some(id))),
                    Err(e) => {
                        self.error = Some(e);
                        cx.notify();
                    }
                }
                true
            }
        }
    }

    pub(super) fn pve_render(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let heading = self.heading("Add a place › Proxmox").into_any_element();
        let body = match self.step {
            Step::Proxmox => self.pve_address(p, cx),
            Step::ProxmoxToken => self.pve_token(p, cx),
            Step::ProxmoxWhere => self.pve_where(p, cx),
            Step::ProxmoxName => self.pve_name(p, cx),
            _ => self.pve_making(p, cx),
        };
        v_flex()
            .gap(px(12.))
            .child(heading)
            .child(body)
            .into_any_element()
    }

    fn pve_address(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let seen = !self.pve.fingerprint.is_empty();
        v_flex()
            .gap(px(10.))
            .child(now_step("1", "Proxmox", p))
            .child(
                v_flex()
                    .pl(px(26.))
                    .gap(px(8.))
                    .child(
                        div().text_color(p.muted).child(
                            "Enter the address of the Proxmox web interface. A VM running the Hangar server and worker will be created; environments run as separate VMs.",
                        ),
                    )
                    .child(Input::new(&self.pve.url))
                    .when(seen, |d| {
                        d.child(
                            v_flex()
                                .gap(px(4.))
                                .p(px(8.))
                                .rounded(px(6.))
                                .border_1()
                                .border_color(p.border)
                                .child(div().child("Certificate fingerprint (SHA-256)"))
                                .child(
                                    div()
                                        .id("pve-fingerprint")
                                        .debug_selector(|| "pve-fingerprint".into())
                                        .font_family(crate::diff::mono())
                                        .text_size(px(11.))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(hangar::show_fingerprint(&self.pve.fingerprint)),
                                )
                                .child(div().text_color(p.muted).text_size(px(11.5)).child(
                                    "Compare with Datacenter › Node › System › Certificates. Only this certificate will be trusted.",
                                )),
                        )
                    }),
            )
            .child(self.error_line(p))
            .child(future_step("2", "API token", p))
            .child(future_step("3", "Placement", p))
            .child(future_step("4", "Name", p))
            .child(
                h_flex()
                    .gap(px(8.))
                    .justify_end()
                    .child(
                        button("pve-back", "Back", p)
                            .on_click(cx.listener(|this, _, _, cx| this.go(Step::Kinds, cx))),
                    )
                    .child(if self.busy {
                        disabled("pve-continue", "Checking…", p).into_any_element()
                    } else if seen {
                        primary("pve-matches", "Trust Certificate", p)
                            .on_click(cx.listener(|this, _, _, cx| this.go(Step::ProxmoxToken, cx)))
                            .into_any_element()
                    } else {
                        primary("pve-continue", "Continue", p)
                            .on_click(cx.listener(|this, _, window, cx| this.pve_probe(window, cx)))
                            .into_any_element()
                    }),
            )
            .into_any_element()
    }

    fn pve_done_address(&self, p: &Palette) -> AnyElement {
        let version = self
            .pve
            .choices
            .as_ref()
            .map(|c| format!(" · Proxmox VE {}", c.version))
            .unwrap_or_default();
        done_step(
            "Proxmox",
            format!("{}{version} · certificate trusted", self.pve.host()),
            p,
        )
    }

    fn pve_token(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let needs = self.pve.choices.as_ref().map(|c| c.needs.clone());
        v_flex()
            .gap(px(10.))
            .child(self.pve_done_address(p))
            .child(now_step("2", "API token", p))
            .child(
                v_flex()
                    .pl(px(26.))
                    .gap(px(8.))
                    .child(div().text_color(p.muted).child(
                        "Create a token under Datacenter › Permissions › API Tokens. It needs access to a pool, a disk storage, an ISO storage and a network bridge; no host-level permissions.",
                    ))
                    .child(field("Token ID", p).child(Input::new(&self.pve.token_id)))
                    .child(field("Secret", p).child(Input::new(&self.pve.secret)))
                    .when_some(needs, |d, needs| {
                        d.child(v_flex().gap(px(2.)).children(needs.into_iter().map(|n| {
                            div()
                                .text_size(px(11.5))
                                .text_color(if n.ok { p.text } else { p.bad })
                                .child(format!("{} {}", if n.ok { "✓" } else { "✗" }, n.says))
                        })))
                    })
                    .child(self.pve_found(p, cx)),
            )
            .child(self.error_line(p))
            .child(future_step("3", "Placement", p))
            .child(future_step("4", "Name", p))
            .child(
                h_flex()
                    .gap(px(8.))
                    .justify_end()
                    .child(
                        button("pve-back", "Back", p)
                            .on_click(cx.listener(|this, _, _, cx| this.go(Step::Proxmox, cx))),
                    )
                    .child(if self.busy {
                        disabled("pve-check", "Checking…", p)
                    } else if self.pve.choices.is_some() && !self.pve_ready() {
                        button("pve-check", "Check Again", p)
                            .on_click(cx.listener(|this, _, _, cx| this.pve_check(cx)))
                    } else if self.pve_ready() {
                        primary("pve-continue", "Continue", p).on_click(
                            cx.listener(|this, _, _, cx| this.go(Step::ProxmoxWhere, cx)),
                        )
                    } else {
                        primary("pve-check", "Check", p)
                            .on_click(cx.listener(|this, _, _, cx| this.pve_check(cx)))
                    }),
            )
            .into_any_element()
    }

    fn pve_done_token(&self, p: &Palette) -> AnyElement {
        let id = self
            .pve
            .token
            .split('=')
            .next()
            .unwrap_or_default()
            .to_string();
        let pool = self
            .pve
            .choices
            .as_ref()
            .and_then(|c| c.pools.get(self.pve.pool))
            .map(|(p, _)| p.clone())
            .unwrap_or_default();
        done_step("API token", format!("{id} · pool {pool}"), p)
    }

    fn pve_where(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let Some(c) = &self.pve.choices else {
            return div().into_any_element();
        };
        let node = c.nodes.get(self.pve.node);
        let options = |label: &str, rows: Vec<(String, String)>, chosen: usize, which: Which| {
            h_flex()
                .gap(px(10.))
                .items_start()
                .child(
                    div()
                        .w(px(84.))
                        .flex_none()
                        .pt(px(4.))
                        .text_color(p.muted)
                        .child(label.to_string()),
                )
                .child(v_flex().flex_1().min_w_0().gap(px(4.)).children(
                    rows.into_iter().enumerate().map(|(i, (a, b))| {
                        let on = i == chosen;
                        h_flex()
                            .id((which.id(), i))
                            .gap(px(8.))
                            .px(px(9.))
                            .py(px(4.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(if on { p.accent } else { p.border })
                            .when(on, |d| d.bg(p.accent_soft))
                            .cursor_pointer()
                            .child(div().child(a))
                            .child(div().flex_1())
                            .child(div().text_size(px(11.)).text_color(p.muted).child(b))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                which.choose(this, i);
                                cx.notify();
                            }))
                    }),
                ))
        };
        let nodes = c
            .nodes
            .iter()
            .map(|n| {
                (
                    n.name.clone(),
                    format!(
                        "{} · {} of {} free",
                        cpus(n),
                        gib(n.free_mib << 20),
                        gib(n.memory_mib << 20)
                    ),
                )
            })
            .collect();
        let stores = |s: &[hangar::Storage]| {
            s.iter()
                .map(|s| {
                    (
                        s.id.clone(),
                        format!("{} · {} free", s.kind, gib(s.free_bytes)),
                    )
                })
                .collect::<Vec<_>>()
        };
        let disks = node.map(|n| stores(&n.disks)).unwrap_or_default();
        let isos = node.map(|n| stores(&n.isos)).unwrap_or_default();
        let nets = node
            .map(|n| {
                n.networks
                    .iter()
                    .map(|x| (x.name.clone(), format!("{} {}", x.kind, x.cidr)))
                    .collect()
            })
            .unwrap_or_default();
        let pools = c
            .pools
            .iter()
            .map(|(id, vms)| (id.clone(), format!("{vms} VMs")))
            .collect();
        let ready = self.pve.plan("x").is_some();
        v_flex()
            .gap(px(10.))
            .child(self.pve_done_address(p))
            .child(self.pve_done_token(p))
            .child(now_step("3", "Placement", p))
            .child(
                v_flex()
                    .pl(px(26.))
                    .gap(px(8.))
                    .child(div().text_color(p.muted).child(
                        "Environments are created as separate VMs in the same pool and storage, on the selected network (DHCP).",
                    ))
                    .child(options("Node", nodes, self.pve.node, Which::Node))
                    .child(options("Disk storage", disks, self.pve.disks, Which::Disks))
                    .child(options("ISO storage", isos, self.pve.isos, Which::Isos))
                    .child(options("Network", nets, self.pve.network, Which::Network))
                    .child(options("Pool", pools, self.pve.pool, Which::Pool)),
            )
            .child(future_step("4", "Name", p))
            .child(
                h_flex()
                    .gap(px(8.))
                    .justify_end()
                    .child(
                        button("pve-back", "Back", p).on_click(
                            cx.listener(|this, _, _, cx| this.go(Step::ProxmoxToken, cx)),
                        ),
                    )
                    .child(if ready {
                        primary("pve-continue", "Continue", p).on_click(
                            cx.listener(|this, _, window, cx| this.pve_to_name(window, cx)),
                        )
                    } else {
                        disabled("pve-continue", "Continue", p)
                    }),
            )
            .into_any_element()
    }

    fn pve_done_where(&self, p: &Palette) -> AnyElement {
        let plan = self.pve.plan("x").unwrap_or_default();
        done_step(
            "Placement",
            format!(
                "node {} · disk storage {} · ISO storage {} · network {} · pool {}",
                plan.node, plan.disks, plan.isos, plan.network, plan.pool
            ),
            p,
        )
    }

    fn pve_name(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let problem = self.pve_prefix_problem(cx);
        let prefix = self.pve.prefix.read(cx).value().to_string();
        let others: Vec<String> = self
            .pve
            .choices
            .as_ref()
            .map(|c| c.prefixes.iter().cloned().collect())
            .unwrap_or_default();
        let named = !self.name.read(cx).value().trim().is_empty();
        v_flex()
            .gap(px(10.))
            .child(self.pve_done_address(p))
            .child(self.pve_done_token(p))
            .child(self.pve_done_where(p))
            .child(now_step("4", "Name", p))
            .child(
                v_flex()
                    .pl(px(26.))
                    .gap(px(8.))
                    .child(field("Name", p).child(Input::new(&self.name)))
                    .child(field("Prefix", p).child(Input::new(&self.pve.prefix)))
                    .child(match &problem {
                        Some(why) => div().text_size(px(11.5)).text_color(p.bad).child(why.clone()),
                        None => div().text_size(px(11.5)).text_color(p.muted).child(format!(
                            "VMs and ISOs it creates are named with this prefix (VM {prefix}-hangar). Nothing else on the Proxmox is modified.{}",
                            if others.is_empty() {
                                String::new()
                            } else {
                                format!(" Prefixes in use: {}.", others.join(", "))
                            }
                        )),
                    })
                    .child(
                        field("VM size", p).child(div().child(format!(
                            "{CORES} vCPUs · {} GB RAM · {DATA_GIB} GB disk",
                            MEMORY_MIB / 1024
                        ))),
                    )
                    .child(div().text_size(px(11.5)).text_color(p.muted).child(
                        "Environment VMs are sized by their templates.",
                    )),
            )
            .child(self.error_line(p))
            .child(
                h_flex()
                    .gap(px(8.))
                    .justify_end()
                    .child(
                        button("pve-back", "Back", p).on_click(
                            cx.listener(|this, _, _, cx| this.go(Step::ProxmoxWhere, cx)),
                        ),
                    )
                    .child(if problem.is_none() && named {
                        primary("pve-make", "Create", p)
                            .on_click(cx.listener(|this, _, _, cx| this.pve_make(cx)))
                    } else {
                        disabled("pve-make", "Create", p)
                    }),
            )
            .into_any_element()
    }

    fn pve_making(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let failed = self.error.is_some();
        v_flex()
            .gap(px(8.))
            .children(self.pve.done.iter().map(|l| {
                div()
                    .text_size(px(12.))
                    .child(format!("✓ {l}"))
                    .into_any_element()
            }))
            .when_some(self.pve.now.clone(), |d, now| {
                d.child(
                    h_flex()
                        .gap(px(8.))
                        .text_color(p.muted)
                        .child(div().child("◌"))
                        .child(div().child(now)),
                )
            })
            .child(self.error_line(p))
            .when(!failed, |d| {
                d.child(div().text_size(px(11.5)).text_color(p.muted).child(
                    if self.pve.reconnecting {
                        "This usually takes under a minute, or about two when the release is updated. Environments keep running."
                    } else {
                        "This usually takes about 30 seconds. The first environment takes about a minute longer while its base image downloads."
                    },
                ))
            })
            .when(failed, |d| {
                let back = if self.pve.reconnecting {
                    Step::ProxmoxToken
                } else {
                    Step::ProxmoxName
                };
                d.child(
                    h_flex().justify_end().child(
                        button("pve-back", "Back", p).on_click(
                            cx.listener(move |this, _, _, cx| this.go(back.clone(), cx)),
                        ),
                    ),
                )
            })
            .into_any_element()
    }

    fn error_line(&self, p: &Palette) -> AnyElement {
        match &self.error {
            Some(e) => div().text_color(p.bad).child(e.clone()).into_any_element(),
            None => div().into_any_element(),
        }
    }
}

/// What making it says, as it goes.
enum Made2 {
    Step(String),
    Failed(String),
    Done(Box<Made>),
}

/// Which of the choices an option row picks.
#[derive(Clone, Copy)]
enum Which {
    Node,
    Disks,
    Isos,
    Network,
    Pool,
}

impl Which {
    fn id(self) -> &'static str {
        match self {
            Which::Node => "pve-node",
            Which::Disks => "pve-disks",
            Which::Isos => "pve-isos",
            Which::Network => "pve-network",
            Which::Pool => "pve-pool",
        }
    }

    fn choose(self, v: &mut AddPlaceView, i: usize) {
        match self {
            Which::Node => {
                v.pve.node = i;
                (v.pve.disks, v.pve.isos, v.pve.network) = (0, 0, 0);
            }
            Which::Disks => v.pve.disks = i,
            Which::Isos => v.pve.isos = i,
            Which::Network => v.pve.network = i,
            Which::Pool => v.pve.pool = i,
        }
    }
}

fn badge(text: &'static str, bg: gpui::Hsla, fg: gpui::Hsla) -> gpui::Div {
    div()
        .size(px(18.))
        .flex_none()
        .rounded_full()
        .bg(bg)
        .text_color(fg)
        .text_size(px(11.))
        .flex()
        .items_center()
        .justify_center()
        .child(text)
}

fn now_step(n: &'static str, title: &'static str, p: &Palette) -> AnyElement {
    h_flex()
        .gap(px(8.))
        .child(badge(n, p.accent, p.on_accent))
        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
        .into_any_element()
}

fn future_step(n: &'static str, title: &'static str, p: &Palette) -> AnyElement {
    h_flex()
        .gap(px(8.))
        .opacity(0.55)
        .child(badge(n, p.surface_2, p.muted))
        .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
        .into_any_element()
}

fn done_step(title: &'static str, detail: String, p: &Palette) -> AnyElement {
    h_flex()
        .gap(px(8.))
        .items_start()
        .child(badge("✓", p.good, p.on_accent))
        .child(
            v_flex()
                .min_w_0()
                .child(div().font_weight(FontWeight::SEMIBOLD).child(title))
                .child(div().text_size(px(11.5)).text_color(p.muted).child(detail)),
        )
        .into_any_element()
}
