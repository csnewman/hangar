//! Every place the app knows, as the window sees them: each one's
//! connection running on its own thread, its updates brought into the app,
//! and what is known of it kept on disk.

use std::sync::Arc;
use std::time::Duration;

use gpui::{App, AppContext, Context, Entity, Task};

use crate::api::{self, Client};
use crate::model::Action;
use crate::place::{self, Connection, Connector, PlaceConfig, PlaceState, Update};
use crate::store::{self, Dirs, Tokens};

/// How long changes wait before what is known is written to disk, so a
/// burst of them is one write.
const SAVE_AFTER: Duration = Duration::from_secs(2);

struct Link {
    client: Option<Client>,
    _connector: Option<Connector>,
    dirty: bool,
}

pub struct Places {
    dirs: Option<Dirs>,
    tokens: Arc<dyn Tokens>,
    /// Places not kept in the list, given for this run only.
    transient: Vec<String>,
    places: Vec<(PlaceConfig, PlaceState)>,
    links: Vec<Link>,
    updates: async_channel::Sender<(String, Update)>,
    saving: Option<Task<()>>,
    /// Whether places connect: not for one made by offline.
    connects: bool,
    /// A failed action, to be shown.
    pub error: Option<String>,
}

impl Places {
    /// The places listed in dirs, and transient ones besides, each
    /// connecting with its token from tokens.
    pub fn new(
        dirs: Option<Dirs>,
        tokens: Arc<dyn Tokens>,
        transient: Vec<PlaceConfig>,
        cx: &mut Context<Self>,
    ) -> Self {
        let (tx, rx) = async_channel::unbounded::<(String, Update)>();
        cx.spawn(async move |this, cx| {
            while let Ok((id, u)) = rx.recv().await {
                let gone = this
                    .update(cx, |p: &mut Places, cx| p.apply(&id, u, cx))
                    .is_err();
                if gone {
                    return;
                }
            }
        })
        .detach();
        let mut listed = dirs
            .as_ref()
            .and_then(|d| d.places().ok())
            .unwrap_or_default();
        let transient_ids = transient.iter().map(|p| p.id.clone()).collect();
        listed.retain(|p| !transient.iter().any(|t| t.id == p.id));
        listed.extend(transient);
        let mut this = Places {
            dirs,
            tokens,
            transient: transient_ids,
            connects: true,
            places: vec![],
            links: vec![],
            updates: tx,
            saving: None,
            error: None,
        };
        for config in listed {
            this.add_listed(config);
        }
        this
    }

    /// Places that never connect: what was last known of each, and
    /// changes to the list, without the network. For tests.
    pub fn offline(tokens: Arc<dyn Tokens>, cx: &mut Context<Self>) -> Self {
        let mut p = Places::new(None, tokens, vec![], cx);
        p.connects = false;
        p
    }

    fn add_listed(&mut self, config: PlaceConfig) {
        let state = self
            .dirs
            .as_ref()
            .and_then(|d| PlaceState::load(&d.cache, &config.id))
            .unwrap_or_default();
        self.places.push((config, state));
        self.links.push(Link {
            client: None,
            _connector: None,
            dirty: false,
        });
        self.connect(self.places.len() - 1);
    }

    /// Makes every place's connection again now, as after the machine
    /// slept: one waiting out a dead stream or a retry would be stale
    /// until its timer ran out.
    pub fn reconnect(&mut self, cx: &mut Context<Self>) {
        if !self.connects {
            return;
        }
        for i in 0..self.places.len() {
            self.connect(i);
        }
        cx.notify();
    }

    /// Starts a place's connection, with the token it has.
    fn connect(&mut self, i: usize) {
        let (config, state) = &mut self.places[i];
        let Some(token) = self.tokens.get(&config.id) else {
            state.connection = Connection::SignIn;
            self.links[i] = Link {
                client: None,
                _connector: None,
                dirty: false,
            };
            return;
        };
        let client = Client::new(&config.url, &token);
        if !self.connects {
            self.links[i] = Link {
                client: Some(client),
                _connector: None,
                dirty: false,
            };
            return;
        }
        let tx = self.updates.clone();
        let id = config.id.clone();
        let connector = place::connect(client.clone(), move |u| {
            tx.send_blocking((id.clone(), u)).is_ok()
        });
        self.links[i] = Link {
            client: Some(client),
            _connector: Some(connector),
            dirty: false,
        };
    }

    /// Takes what a place's connection says: its connection, who is signed
    /// in, and its environments.
    pub fn apply(&mut self, id: &str, u: Update, cx: &mut Context<Self>) {
        let Some(i) = self.places.iter().position(|(c, _)| c.id == id) else {
            return;
        };
        let keeps = !matches!(u, Update::Connection(_));
        self.places[i].1.apply(u);
        if keeps {
            self.links[i].dirty = true;
            self.save_soon(cx);
        }
        cx.notify();
    }

    fn save_soon(&mut self, cx: &mut Context<Self>) {
        if self.saving.is_some() || self.dirs.is_none() {
            return;
        }
        self.saving = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_AFTER).await;
            let _ = this.update(cx, |p: &mut Places, _| {
                p.saving = None;
                p.save();
            });
        }));
    }

    fn save(&mut self) {
        let Some(dirs) = &self.dirs else { return };
        for (i, (config, state)) in self.places.iter().enumerate() {
            if std::mem::take(&mut self.links[i].dirty) {
                let _ = state.save(&dirs.cache, &config.id);
            }
        }
    }

    pub fn list(&self) -> &[(PlaceConfig, PlaceState)] {
        &self.places
    }

    pub fn get(&self, id: &str) -> Option<&(PlaceConfig, PlaceState)> {
        self.places.iter().find(|(c, _)| c.id == id)
    }

    /// Adds a server place, or signs an existing one in again, with an
    /// access token, and connects to it.
    pub fn add_server(
        &mut self,
        url: &str,
        token: &str,
        cx: &mut Context<Self>,
    ) -> Result<String, String> {
        let url = store::normalise_url(url).ok_or("That is not a server's address.")?;
        let token = token.trim();
        if token.is_empty() {
            return Err("An access token is needed: make one on the server's Account page.".into());
        }
        let id = store::place_id(&url);
        self.tokens.set(&id, token)?;
        match self.places.iter().position(|(c, _)| c.id == id) {
            Some(i) => {
                self.places[i].0.url = url;
                self.connect(i);
            }
            None => {
                let name = id.split(':').next().unwrap_or(&id).to_string();
                self.add_listed(PlaceConfig {
                    id: id.clone(),
                    name,
                    url,
                });
            }
        }
        self.save_list();
        cx.notify();
        Ok(id)
    }

    /// Takes the places' list as the Settings window leaves it: each
    /// place's ID and name, in order. A name left empty keeps the one it
    /// had; a place not listed is forgotten, its token and what was known
    /// of it with it. Places added meanwhile keep their place at the end.
    pub fn arrange(&mut self, list: &[(String, String)], cx: &mut Context<Self>) {
        let gone: Vec<String> = self
            .places
            .iter()
            .map(|(c, _)| c.id.clone())
            .filter(|id| !list.iter().any(|(l, _)| l == id))
            .collect();
        for id in &gone {
            self.tokens.delete(id);
            if let Some(d) = &self.dirs {
                PlaceState::forget(&d.cache, id);
            }
        }
        let mut pairs: Vec<((PlaceConfig, PlaceState), Link)> = std::mem::take(&mut self.places)
            .into_iter()
            .zip(std::mem::take(&mut self.links))
            .filter(|((c, _), _)| !gone.contains(&c.id))
            .collect();
        let mut out = Vec::with_capacity(pairs.len());
        for (id, name) in list {
            if let Some(i) = pairs.iter().position(|((c, _), _)| &c.id == id) {
                let ((mut c, s), l) = pairs.remove(i);
                if !name.trim().is_empty() {
                    c.name = name.trim().to_string();
                }
                out.push(((c, s), l));
            }
        }
        out.extend(pairs);
        (self.places, self.links) = out.into_iter().unzip();
        self.transient.retain(|t| !gone.contains(t));
        self.save_list();
        cx.notify();
    }

    fn save_list(&self) {
        let Some(dirs) = &self.dirs else { return };
        let listed: Vec<PlaceConfig> = self
            .places
            .iter()
            .filter(|(c, _)| !self.transient.contains(&c.id))
            .map(|(c, _)| c.clone())
            .collect();
        let _ = dirs.save_places(&listed);
    }

    pub fn client(&self, place: &str) -> Option<Client> {
        let i = self.places.iter().position(|(c, _)| c.id == place)?;
        self.links[i].client.clone()
    }

    /// Carries out a card's action that changes the environment; the event
    /// stream brings the change back.
    pub fn act(&mut self, place: &str, env: &str, action: Action, cx: &mut Context<Self>) {
        let Some(client) = self.client(place) else {
            return;
        };
        let env = env.to_string();
        let task = cx.background_spawn(async move {
            match action {
                Action::Suspend => client.suspend(&env).map(|_| ()),
                Action::Resume | Action::Start => client.start(&env).map(|_| ()),
                Action::Stop => client.stop(&env).map(|_| ()),
                Action::Reset => client.reset(&env).map(|_| ()),
                Action::Delete => client.delete(&env),
                _ => Ok(()),
            }
        });
        self.report(task, cx);
    }

    /// Sets an environment's task, or marks it done for an empty text.
    pub fn set_task(&mut self, place: &str, env: &str, text: &str, cx: &mut Context<Self>) {
        let Some(client) = self.client(place) else {
            return;
        };
        let (env, text) = (env.to_string(), text.trim().to_string());
        let task = cx.background_spawn(async move {
            if text.is_empty() {
                client.end_task(&env)
            } else {
                client.set_task(&env, &text).map(|_| ())
            }
        });
        self.report(task, cx);
    }

    /// Asks for an environment to stop, keeping its disks.
    pub fn stop(&mut self, place: &str, env: &str, cx: &mut Context<Self>) {
        let Some(client) = self.client(place) else {
            return;
        };
        let env = env.to_string();
        let task = cx.background_spawn(async move { client.stop(&env).map(|_| ()) });
        self.report(task, cx);
    }

    /// Says the user has looked at an agent's session.
    pub fn seen(&mut self, place: &str, env: &str, session: &str, cx: &mut Context<Self>) {
        let Some(client) = self.client(place) else {
            return;
        };
        let (env, session) = (env.to_string(), session.to_string());
        let task = cx.background_spawn(async move { client.seen(&env, &session) });
        self.report(task, cx);
    }

    fn report(&mut self, task: Task<Result<(), api::Error>>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            if let Err(e) = task.await {
                let _ = this.update(cx, |p: &mut Places, cx| {
                    p.error = Some(e.to_string());
                    cx.notify();
                });
            }
        })
        .detach();
    }
}

/// Places for a run: the user's own, and one given by HANGAR_URL and
/// HANGAR_TOKEN for this run only.
pub fn from_env(cx: &mut App) -> Entity<Places> {
    let mut transient = vec![];
    let mut tokens: Arc<dyn Tokens> = Arc::new(store::Keychain);
    if let (Ok(url), Ok(token)) = (std::env::var("HANGAR_URL"), std::env::var("HANGAR_TOKEN"))
        && let Some(url) = store::normalise_url(&url)
    {
        let id = store::place_id(&url);
        let mem = store::MemoryTokens::default();
        let _ = mem.set(&id, &token);
        tokens = Arc::new(Layered(Arc::new(mem), tokens));
        transient.push(PlaceConfig {
            name: id.split(':').next().unwrap_or(&id).to_string(),
            id,
            url,
        });
    }
    let dirs = Dirs::user();
    cx.new(|cx| Places::new(dirs, tokens, transient, cx))
}

/// Tokens from the first store that has them, set in the second.
struct Layered(Arc<dyn Tokens>, Arc<dyn Tokens>);

impl Tokens for Layered {
    fn get(&self, place: &str) -> Option<String> {
        self.0.get(place).or_else(|| self.1.get(place))
    }
    fn set(&self, place: &str, token: &str) -> Result<(), String> {
        self.1.set(place, token)
    }
    fn delete(&self, place: &str) {
        self.0.delete(place);
        self.1.delete(place);
    }
}
