//! A server's client API, /api/v1 (api/client.yaml): what
//! the app reads and asks of a server place. Its types and calls are
//! generated from the spec (hangar-openapi); this is the app's blocking face
//! on them, each call run to its end on a runtime of this module's, so each
//! place's connection runs on a thread of its own as the rest of the app
//! expects.

use std::future::Future;
use std::io::{self, BufRead, BufReader, Read};
use std::sync::OnceLock;
use std::time::Duration;

use hangar_openapi::clientapi::{self as generated, types};
use serde::Deserialize;

pub use types::{
    Access, AccessToken, ActivityEvent, AgentSession, Environment, EnvironmentPack, FileConflict,
    ImageRollback, ImageUpdate, LogPart, LoginKey, Me, PackCopy, Process, ProcessList, Repo,
    Resources, SignInGrant, SshGateway, SshKey, StartProgress, Task, Template, TerminalSession,
    Version,
};

/// An environment's machine, as its settings change it.
pub type MachineSettings = types::EnvironmentSettings;
/// What making an environment asks for.
pub type NewEnvironment = types::CreateEnvironment;
/// What an environment uses, while it runs.
pub type Stats = types::EnvironmentStats;
/// Who did something to an environment.
pub type Actor = types::ActivityEventActor;
/// What something was done to.
pub type Target = types::ActivityEventTarget;

/// The API version the app speaks.
pub const VERSION: &str = "v1";

/// How long a read on the event stream may wait. The server sends a
/// comment every 30 seconds, so a stream silent for longer is dead.
const STREAM_READ: Duration = Duration::from_secs(75);

/// How long any other request may take.
const CALL: Duration = Duration::from_secs(30);

/// Where a sign-in through the browser stands.
#[derive(Debug, Clone, PartialEq)]
pub enum SignedIn {
    /// Approved: the access token, made for the app.
    Token(String),
    /// Not yet approved.
    Waiting,
    /// The person refused it.
    Refused,
    /// It expired or was collected.
    Gone,
}

/// Why a request failed.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The token is missing, wrong or revoked.
    Unauthorized,
    /// The server answered with an error.
    Status(u16, String),
    /// The server could not be reached, or the connection failed.
    Transport(String),
    /// The server's answer was not what this API says.
    Decode(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unauthorized => write!(f, "not signed in"),
            Error::Status(code, msg) if msg.is_empty() => write!(f, "server returned HTTP {code}"),
            Error::Status(code, msg) => write!(f, "{msg} ({code})"),
            Error::Transport(msg) => write!(f, "{msg}"),
            Error::Decode(msg) => write!(f, "unexpected response: {msg}"),
        }
    }
}

impl std::error::Error for Error {}

/// One change on the event stream.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// An environment, all of it.
    Environment(Box<Environment>),
    /// An environment deleted, or out of reach.
    Removed(String),
    /// Every environment has been sent, or for a stream resumed every one
    /// changed since; changes follow. The IDs are every environment there
    /// is (none from a server older than resuming).
    Synced(Vec<String>),
}

/// The runtime every call runs on: reqwest's connections need tokio's,
/// whichever thread waits on the call.
pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("hangar-api")
            .enable_all()
            .build()
            .expect("a tokio runtime")
    })
}

/// The certificates places are pinned to, by their origin (`store::place_id`).
fn pins() -> &'static std::sync::RwLock<std::collections::HashMap<String, String>> {
    static PINS: OnceLock<std::sync::RwLock<std::collections::HashMap<String, String>>> =
        OnceLock::new();
    PINS.get_or_init(Default::default)
}

/// Takes the server at url's HTTPS only with the certificate whose SHA-256
/// is fingerprint: a Hangar the app made, whose certificate
/// it made and no authority signs.
pub fn pin(url: &str, fingerprint: &str) {
    pins()
        .write()
        .unwrap()
        .insert(crate::store::place_id(url), fingerprint.to_string());
}

/// TLS for url: to its pinned certificate, if it has one, else checked as
/// the system checks certificates.
pub fn tls_for(url: &str) -> Result<rustls::ClientConfig, String> {
    let pinned = pins()
        .read()
        .unwrap()
        .get(&crate::store::place_id(url))
        .cloned();
    match pinned {
        Some(fp) => {
            hangar_hub::tls::pinned(&fp, &[&rustls::version::TLS13, &rustls::version::TLS12])
                .map_err(|e| e.to_string())
        }
        None => {
            use rustls_platform_verifier::ConfigVerifierExt;
            rustls::ClientConfig::with_platform_verifier().map_err(|e| e.to_string())
        }
    }
}

/// A server's client API, signed in with an access token.
#[derive(Clone)]
pub struct Client {
    base: String,
    token: String,
    /// The generated client for calls, held to CALL, and for the event
    /// stream, whose reads are held to STREAM_READ; or why neither could be
    /// made.
    api: Result<(generated::Client, generated::Client), String>,
}

impl Client {
    /// A client for the server at url (its address, such as
    /// https://hangar.example.com), signing in with token.
    pub fn new(url: &str, token: &str) -> Client {
        let base = format!("{}/api/{VERSION}", url.trim_end_matches('/'));
        let make = |calls: bool| -> Result<generated::Client, String> {
            let mut headers = reqwest::header::HeaderMap::new();
            if !token.is_empty() {
                let auth = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|e| e.to_string())?;
                headers.insert(reqwest::header::AUTHORIZATION, auth);
            }
            let builder = reqwest::Client::builder()
                .tls_backend_preconfigured(tls_for(url)?)
                .connect_timeout(crate::ws::DIAL_WITHIN)
                .default_headers(headers)
                .user_agent(concat!("hangar-app/", env!("CARGO_PKG_VERSION")));
            let builder = if calls {
                builder.timeout(CALL)
            } else {
                builder.read_timeout(STREAM_READ)
            };
            let http = builder.build().map_err(|e| e.to_string())?;
            Ok(generated::Client::new_with_client(&base, http))
        };
        let api = make(true).and_then(|calls| Ok((calls, make(false)?)));
        Client {
            base,
            token: token.to_string(),
            api,
        }
    }

    pub fn version(&self) -> Result<Version, Error> {
        self.call(|c| c.get_version())
    }

    pub fn me(&self) -> Result<Me, Error> {
        self.call(|c| c.get_me())
    }

    /// Asks the server to let an app that calls itself name sign in
    /// through the browser. Needs no token.
    pub fn start_sign_in(&self, name: &str) -> Result<SignInGrant, Error> {
        let body = types::StartSignIn { name: name.into() };
        self.call(|c| c.start_sign_in(&body))
    }

    /// Asks whether a grant is approved, collecting its token once it is.
    /// Needs no token.
    pub fn finish_sign_in(&self, device_code: &str) -> Result<SignedIn, Error> {
        let body = types::FinishSignIn {
            device_code: device_code.into(),
        };
        match self.answer(|c| c.finish_sign_in_step(&body)) {
            Ok(hangar_openapi::SignInStep::Token(t)) => Ok(SignedIn::Token(t.token)),
            Ok(hangar_openapi::SignInStep::Waiting(_)) => Ok(SignedIn::Waiting),
            Err(Error::Status(403, _)) => Ok(SignedIn::Refused),
            Err(Error::Status(404, _)) => Ok(SignedIn::Gone),
            Err(e) => Err(e),
        }
    }

    /// A link that signs a browser in to the server's web pages once, as
    /// the user this token is: a path on the server, which then goes on to
    /// the path to.
    pub fn sign_in_link(&self, to: &str) -> Result<String, Error> {
        let body = types::NewSignInLink {
            to: (!to.is_empty()).then(|| to.to_string()),
        };
        self.call(|c| c.create_sign_in_link(&body)).map(|l| l.path)
    }

    /// Changes the user's password, given the current one, or sets an
    /// administrator's first.
    pub fn change_password(&self, current: Option<&str>, new: &str) -> Result<(), Error> {
        let body = types::ChangePassword {
            current_password: current.map(str::to_string),
            new_password: new
                .parse()
                .map_err(|e| Error::Status(400, format!("{e}")))?,
        };
        self.call(|c| c.change_password(&body))
    }

    /// This place as linked places know it, the person the user is, and
    /// the places the user's account here is linked with.
    pub fn links(&self) -> Result<types::Links, Error> {
        self.call(|c| c.list_links())
    }

    /// Links the user's account here with another place, as person.
    pub fn put_link(
        &self,
        place: &str,
        name: &str,
        public_key: &str,
        url: &str,
        person: &str,
    ) -> Result<types::Link, Error> {
        let body = types::NewLink {
            name: Some(name.to_string()),
            public_key: public_key.to_string(),
            url: url.to_string(),
            person: person.to_string(),
        };
        self.call(|c| c.put_link(place, &body))
    }

    pub fn delete_link(&self, place: &str) -> Result<(), Error> {
        self.call(|c| c.delete_link(place))
    }

    /// Sends the user's files to each linked place at most so many bytes
    /// a second; 0 is no limit.
    pub fn set_bandwidth(&self, bandwidth: i64) -> Result<(), Error> {
        let body = types::LinkSettings { bandwidth };
        self.call(|c| c.set_link_settings(&body)).map(|_| ())
    }

    /// The packs the user may use.
    pub fn packs(&self) -> Result<Vec<types::Pack>, Error> {
        let first = self.call(|c| c.list_packs())?;
        self.all(first)
    }

    /// A request's body, from JSON, as the call's type: the pages build
    /// their bodies as the spec reads, and a field the spec refuses is an
    /// error here, not at the server.
    fn body<T: serde::de::DeserializeOwned>(v: serde_json::Value) -> Result<T, Error> {
        serde_json::from_value(v).map_err(|e| Error::Status(400, e.to_string()))
    }

    /// A template whole: its recipe, collaborators and what the user may
    /// do with it.
    pub fn template(&self, id: &str) -> Result<types::TemplateDetail, Error> {
        self.call(|c| c.get_template(id))
    }

    pub fn create_template(
        &self,
        input: serde_json::Value,
    ) -> Result<types::TemplateDetail, Error> {
        let b: types::TemplateInput = Self::body(input)?;
        self.call(|c| c.create_template(&b))
    }

    pub fn update_template(
        &self,
        id: &str,
        input: serde_json::Value,
    ) -> Result<types::TemplateDetail, Error> {
        let b: types::TemplateInput = Self::body(input)?;
        self.call(|c| c.update_template(id, &b))
    }

    pub fn delete_template(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.delete_template(id))
    }

    /// Who besides its owner uses and edits a template: people and teams.
    pub fn set_template_collaborators(
        &self,
        id: &str,
        users: &[String],
        teams: &[String],
    ) -> Result<types::TemplateDetail, Error> {
        let b: types::Collaborators =
            Self::body(serde_json::json!({ "user_ids": users, "team_ids": teams }))?;
        self.call(|c| c.set_template_collaborators(id, &b))
    }

    /// Everyone at the place, to choose collaborators and members from.
    pub fn people(&self) -> Result<Vec<types::Person>, Error> {
        let first = self.call(|c| c.list_people())?;
        self.all(first)
    }

    pub fn teams(&self) -> Result<Vec<types::Team>, Error> {
        let first = self.call(|c| c.list_teams())?;
        self.all(first)
    }

    /// A team with its members.
    pub fn team(&self, id: &str) -> Result<types::Team, Error> {
        self.call(|c| c.get_team(id))
    }

    pub fn create_team(&self, input: serde_json::Value) -> Result<types::Team, Error> {
        let b: types::CreateTeam = Self::body(input)?;
        self.call(|c| c.create_team(&b))
    }

    pub fn update_team(&self, id: &str, input: serde_json::Value) -> Result<types::Team, Error> {
        let b: types::TeamInput = Self::body(input)?;
        self.call(|c| c.update_team(id, &b))
    }

    pub fn delete_team(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.delete_team(id))
    }

    /// Adds someone to a team, or changes their role: viewer, member or
    /// admin.
    pub fn set_team_member(&self, id: &str, user: &str, role: &str) -> Result<types::Team, Error> {
        let b: types::TeamMembership = Self::body(serde_json::json!({ "role": role }))?;
        self.call(|c| c.set_team_member(id, user, &b))
    }

    pub fn remove_team_member(&self, id: &str, user: &str) -> Result<types::Team, Error> {
        self.call(|c| c.remove_team_member(id, user))
    }

    /// The image repositories of the place's registry the user may see.
    pub fn images(&self) -> Result<Vec<types::ImageRepository>, Error> {
        let first = self.call(|c| c.list_image_repositories())?;
        self.all(first)
    }

    /// A repository with its tags.
    pub fn image(&self, id: &str) -> Result<types::ImageRepository, Error> {
        self.call(|c| c.get_image_repository(id))
    }

    pub fn update_image(
        &self,
        id: &str,
        input: serde_json::Value,
    ) -> Result<types::ImageRepository, Error> {
        let b: types::ImageRepositoryInput = Self::body(input)?;
        self.call(|c| c.update_image_repository(id, &b))
    }

    pub fn set_image_collaborators(
        &self,
        id: &str,
        users: &[String],
        teams: &[String],
    ) -> Result<types::ImageRepository, Error> {
        let b: types::Collaborators =
            Self::body(serde_json::json!({ "user_ids": users, "team_ids": teams }))?;
        self.call(|c| c.set_image_repository_collaborators(id, &b))
    }

    pub fn delete_image(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.delete_image_repository(id))
    }

    pub fn delete_image_tag(&self, id: &str, tag: &str) -> Result<types::ImageRepository, Error> {
        self.call(|c| c.delete_image_tag(id, tag))
    }

    /// The place's users, for an administrator.
    pub fn users(&self) -> Result<Vec<types::User>, Error> {
        let first = self.call(|c| c.list_users())?;
        self.all(first)
    }

    pub fn create_user(&self, input: serde_json::Value) -> Result<types::User, Error> {
        let b: types::CreateUser = Self::body(input)?;
        self.call(|c| c.create_user(&b))
    }

    /// Changes a user: only the fields given.
    pub fn update_user(&self, id: &str, input: serde_json::Value) -> Result<types::User, Error> {
        let b: types::UpdateUser = Self::body(input)?;
        self.call(|c| c.update_user(id, &b))
    }

    pub fn delete_user(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.delete_user(id))
    }

    /// The place's workers, for an administrator.
    pub fn workers(&self) -> Result<Vec<types::Worker>, Error> {
        let first = self.call(|c| c.list_workers())?;
        self.all(first)
    }

    pub fn worker(&self, id: &str) -> Result<types::Worker, Error> {
        self.call(|c| c.get_worker(id))
    }

    /// A worker's recent log, from offset on.
    pub fn worker_log(&self, id: &str, offset: Option<i64>) -> Result<types::LogPart, Error> {
        self.call(|c| c.get_worker_log(id, offset))
    }

    pub fn revoke_worker(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.revoke_worker(id))
    }

    pub fn delete_worker(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.delete_worker(id))
    }

    /// Has a worker delete a copy of an image nothing uses.
    pub fn remove_worker_image(&self, id: &str, image: &types::ImageCopy) -> Result<(), Error> {
        self.call(|c| c.remove_worker_image(id, image))
    }

    /// Has a worker delete environments the server does not know.
    pub fn remove_unknown(&self, id: &str, environments: &[String]) -> Result<(), Error> {
        let b: types::RemoveUnknownEnvironments =
            Self::body(serde_json::json!({ "environments": environments }))?;
        self.call(|c| c.remove_unknown_environments(id, &b))
    }

    /// The audit log, newest first, a page of it before an event's ID.
    pub fn audit(&self, before: Option<i64>, limit: i64) -> Result<Vec<types::AuditEvent>, Error> {
        let first = self.call(|c| c.list_audit(before, Some(limit), None))?;
        Ok(first.items)
    }

    pub fn delete_pack(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.delete_pack(id))
    }

    /// Makes a shared copy of a pack, of the user's or of a team.
    pub fn create_copy(&self, pack: &str, input: serde_json::Value) -> Result<types::Copy, Error> {
        let b: types::CopyInput = Self::body(input)?;
        self.call(|c| c.create_copy(pack, &b))
    }

    pub fn rename_copy(&self, id: &str, name: &str) -> Result<types::Copy, Error> {
        let b: types::CopyName = Self::body(serde_json::json!({ "name": name }))?;
        self.call(|c| c.rename_copy(id, &b))
    }

    pub fn delete_copy(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.delete_copy(id))
    }

    /// The copy the user uses of a pack by default; None their own.
    pub fn set_default_copy(&self, pack: &str, copy: Option<&str>) -> Result<(), Error> {
        let b: types::CopyChoice = Self::body(serde_json::json!({ "copy_id": copy }))?;
        self.call(|c| c.set_default_copy(pack, &b))
    }

    /// A copy, its pack, and its files without their contents.
    pub fn copy_files(&self, id: &str) -> Result<types::CopyFiles, Error> {
        self.call(|c| c.get_copy(id))
    }

    pub fn copy_file(&self, id: &str, path: &str) -> Result<types::PackFileContent, Error> {
        self.call(|c| c.get_copy_file(id, path))
    }

    /// Writes a file of a copy; mode None keeps the file's, else a new
    /// file's default.
    pub fn put_copy_file(
        &self,
        id: &str,
        path: &str,
        content: &str,
        mode: Option<i64>,
    ) -> Result<types::PackFile, Error> {
        let mut v = serde_json::json!({ "content": content });
        if let Some(m) = mode {
            v["mode"] = m.into();
        }
        let b: types::PutPackFile = Self::body(v)?;
        self.call(|c| c.put_copy_file(id, path, &b))
    }

    pub fn set_copy_file_mode(
        &self,
        id: &str,
        path: &str,
        mode: i64,
    ) -> Result<types::PackFile, Error> {
        let b: types::PackFileMode = Self::body(serde_json::json!({ "mode": mode }))?;
        self.call(|c| c.set_copy_file_mode(id, path, &b))
    }

    pub fn delete_copy_file(&self, id: &str, path: &str) -> Result<(), Error> {
        self.call(|c| c.delete_copy_file(id, path))
    }

    /// The environment's files whose copy and its own differ.
    pub fn conflicts(&self, id: &str) -> Result<Vec<types::FileConflict>, Error> {
        let first = self.call(|c| c.list_environment_conflicts(id))?;
        Ok(first.items)
    }

    pub fn resolve_conflict(&self, id: &str, input: serde_json::Value) -> Result<(), Error> {
        let b: types::ResolveConflict = Self::body(input)?;
        self.call(|c| c.resolve_environment_conflict(id, &b))
    }

    /// Makes a pack of the user's own.
    pub fn create_pack(&self, input: &types::PackInput) -> Result<types::Pack, Error> {
        self.call(|c| c.create_pack(input))
    }

    /// Changes a pack: its name, description, attach rule and paths.
    pub fn update_pack(&self, id: &str, input: &types::PackInput) -> Result<types::Pack, Error> {
        self.call(|c| c.update_pack(id, input))
    }

    /// A pack's copies the user may use, their own first.
    pub fn copies(&self, pack: &str) -> Result<Vec<types::Copy>, Error> {
        let first = self.call(|c| c.list_copies(pack))?;
        self.all(first)
    }

    /// The linked places one's own pack is for, by place ID; none for every
    /// one.
    pub fn set_pack_places(&self, id: &str, places: &[String]) -> Result<types::Pack, Error> {
        let b: types::PackPlaces = Self::body(serde_json::json!({ "places": places }))?;
        self.call(|c| c.set_pack_places(id, &b))
    }

    /// Merges a pack renamed apart from another of the same name into it.
    pub fn merge_pack(&self, id: &str) -> Result<types::Pack, Error> {
        self.call(|c| c.merge_pack(id))
    }

    pub fn keep_pack_apart(&self, id: &str) -> Result<types::Pack, Error> {
        self.call(|c| c.keep_pack_apart(id))
    }

    /// The versions of a copy's files a newer change replaced.
    pub fn copy_history(&self, copy: &str) -> Result<Vec<types::ReplacedFile>, Error> {
        let first = self.call(|c| c.list_copy_history(copy))?;
        self.all(first)
    }

    pub fn restore_copy_file(&self, copy: &str, entry: &str) -> Result<(), Error> {
        self.call(|c| c.restore_copy_file(copy, entry))
    }

    pub fn login_keys(&self) -> Result<Vec<LoginKey>, Error> {
        let first = self.call(|c| c.list_login_keys())?;
        self.all(first)
    }

    /// Adds a public key, in authorized_keys form, that signs the user in
    /// over SSH; its comment names it.
    pub fn add_login_key(&self, public_key: &str) -> Result<LoginKey, Error> {
        let body = types::AddLoginKey {
            public_key: public_key.trim().into(),
            ..Default::default()
        };
        self.call(|c| c.add_login_key(&body))
    }

    pub fn delete_login_key(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.delete_login_key(id))
    }

    pub fn tokens(&self) -> Result<Vec<AccessToken>, Error> {
        let first = self.call(|c| c.list_tokens())?;
        self.all(first)
    }

    pub fn revoke_token(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.revoke_token(id))
    }

    pub fn ssh_keys(&self) -> Result<Vec<SshKey>, Error> {
        let first = self.call(|c| c.list_ssh_keys())?;
        self.all(first)
    }

    pub fn generate_ssh_key(&self, name: &str) -> Result<SshKey, Error> {
        let body = types::GenerateSshKey {
            name: name.trim().into(),
        };
        self.call(|c| c.generate_ssh_key(&body))
    }

    pub fn delete_ssh_key(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.delete_ssh_key(id))
    }

    pub fn environments(&self) -> Result<Vec<Environment>, Error> {
        let first = self.call(|c| c.list_environments())?;
        self.all(first)
    }

    pub fn start(&self, id: &str) -> Result<Environment, Error> {
        self.call(|c| c.start_environment(id))
    }

    pub fn stop(&self, id: &str) -> Result<Environment, Error> {
        self.call(|c| c.stop_environment(id))
    }

    pub fn suspend(&self, id: &str) -> Result<Environment, Error> {
        self.call(|c| c.suspend_environment(id))
    }

    /// Gives a stopped environment its template's settings, keeping its
    /// disks.
    pub fn reset(&self, id: &str) -> Result<Environment, Error> {
        self.call(|c| c.reset_environment(id))
    }

    /// Deletes an environment and its disks: at once if it is stopped, else
    /// once it has stopped.
    pub fn delete(&self, id: &str) -> Result<(), Error> {
        self.answer(|c| c.delete_environment_or_gone(id))
            .map(|_| ())
    }

    pub fn set_task(&self, id: &str, text: &str) -> Result<Task, Error> {
        let body = types::SetTask { text: text.into() };
        self.call(|c| c.set_task(id, &body))
    }

    pub fn end_task(&self, id: &str) -> Result<(), Error> {
        self.call(|c| c.end_task(id))
    }

    /// The environment's terminal sessions.
    pub fn terminals(&self, id: &str) -> Result<Vec<TerminalSession>, Error> {
        let first = self.call(|c| c.list_terminals(id))?;
        self.all(first)
    }

    /// Part of one of the environment's logs, kept by its worker: what
    /// follows offset, or the end.
    pub fn environment_log(
        &self,
        id: &str,
        log: &str,
        offset: Option<i64>,
    ) -> Result<LogPart, Error> {
        let log = types::EnvironmentLogName(log.into());
        self.call(|c| c.get_environment_log(id, &log, offset))
    }

    /// What the environment uses, while it runs.
    pub fn stats(&self, id: &str) -> Result<Option<Stats>, Error> {
        self.environment(id).map(|e| e.stats)
    }

    /// The processes running in the environment, asked of it.
    pub fn processes(&self, id: &str) -> Result<ProcessList, Error> {
        self.call(|c| c.list_processes(id))
    }

    /// Stops a process: TERM asks it to, KILL forces it.
    pub fn signal_process(&self, id: &str, pid: i64, signal: &str) -> Result<(), Error> {
        let body = types::SignalProcessBody {
            signal: signal.into(),
        };
        self.call(|c| c.signal_process(id, pid, &body))
    }

    /// What has been done to the environment, newest first: a page of it,
    /// before an event's ID.
    pub fn activity(
        &self,
        id: &str,
        before: Option<i64>,
        limit: usize,
    ) -> Result<Vec<ActivityEvent>, Error> {
        self.call(|c| c.list_activity(id, before, Some(limit as i64)))
            .map(|page: types::ActivityList| page.items)
    }

    /// The templates the user may make an environment from.
    pub fn templates(&self) -> Result<Vec<Template>, Error> {
        let first = self.call(|c| c.list_templates())?;
        self.all(first)
    }

    /// Makes an environment; it starts once a worker takes it.
    pub fn create_environment(&self, new: &NewEnvironment) -> Result<Environment, Error> {
        self.call(|c| c.create_environment(new))
    }

    /// One environment, as it is.
    pub fn environment(&self, id: &str) -> Result<Environment, Error> {
        self.call(|c| c.get_environment(id))
    }

    /// Changes an environment's machine: a stopped one's at its next start,
    /// a running one's memory as it runs. Answered with the environment.
    pub fn update_settings(&self, id: &str, s: &MachineSettings) -> Result<Environment, Error> {
        self.call(|c| c.update_environment_settings(id, s))
    }

    /// Each online worker's capacity, largest first.
    pub fn worker_sizes(&self) -> Result<Vec<Resources>, Error> {
        let first = self.call(|c| c.list_worker_sizes())?;
        self.all(first)
    }

    /// The packs an environment has, each with the copy it uses.
    pub fn environment_packs(&self, id: &str) -> Result<Vec<EnvironmentPack>, Error> {
        let first = self.call(|c| c.list_environment_packs(id))?;
        self.all(first)
    }

    /// Chooses the copy of a pack an environment uses; None undoes the
    /// choice.
    pub fn choose_copy(&self, id: &str, pack: &str, copy: Option<&str>) -> Result<(), Error> {
        let body = types::ChooseEnvironmentCopyBody {
            copy_id: copy.map(str::to_string),
        };
        self.call(|c| c.choose_environment_copy(id, pack, &body))
    }

    /// Changes an environment's image: upgrade, rollback, keep or cancel.
    /// force upgrades though the newer copy has not been compared yet.
    pub fn change_image(&self, id: &str, change: &str, force: bool) -> Result<Environment, Error> {
        let body = types::ChangeEnvironmentImageBody {
            force: force.then_some(true),
        };
        self.call(|c| c.change_environment_image(id, change, &body))
    }

    /// Says whether anyone may reach the environment's own web servers
    /// without signing in to Hangar, answered with the environment.
    pub fn set_ports_public(&self, id: &str, public: bool) -> Result<Environment, Error> {
        let body = types::SetEnvironmentPortsBody { public };
        self.call(|c| c.set_environment_ports(id, &body))
    }

    /// Ends a terminal session, and what runs in it.
    pub fn close_terminal(&self, id: &str, session: &str) -> Result<(), Error> {
        self.call(|c| c.close_terminal(id, session))
    }

    /// Where a terminal session's WebSocket is, and the header that signs
    /// it in: session, or a new one at cols by rows.
    /// The WebSocket through which this place carries its link to the
    /// place other when the app relays it, and its authorization.
    pub fn link_relay_socket(&self, other: &str) -> (String, String) {
        let base = match self.base.split_once("://") {
            Some(("https", rest)) => format!("wss://{rest}"),
            Some((_, rest)) => format!("ws://{rest}"),
            None => self.base.clone(),
        };
        (format!("{base}/me/links/{other}/relay"), self.auth())
    }

    /// This place's link endpoint, which other places dial: the far end
    /// of a relayed link.
    pub fn link_connect_socket(&self) -> (String, String) {
        let root = self
            .base
            .strip_suffix(&format!("/api/{VERSION}"))
            .unwrap_or(&self.base);
        let root = match root.split_once("://") {
            Some(("https", rest)) => format!("wss://{rest}"),
            Some((_, rest)) => format!("ws://{rest}"),
            None => root.to_string(),
        };
        (format!("{root}/api/link/v1/connect"), self.auth())
    }

    pub fn terminal_socket(
        &self,
        id: &str,
        session: Option<&str>,
        cols: u16,
        rows: u16,
    ) -> (String, String) {
        self.terminal_socket_in(id, session, cols, rows, None, None)
    }

    /// As terminal_socket, a new session starting in dir, its shell told
    /// it runs in the terminal emulator term names, and that version.
    pub fn terminal_socket_in(
        &self,
        id: &str,
        session: Option<&str>,
        cols: u16,
        rows: u16,
        dir: Option<&str>,
        term: Option<(&str, &str)>,
    ) -> (String, String) {
        let base = match self.base.split_once("://") {
            Some(("https", rest)) => format!("wss://{rest}"),
            Some((_, rest)) => format!("ws://{rest}"),
            None => self.base.clone(),
        };
        let mut url = format!("{base}/environments/{id}/terminal?cols={cols}&rows={rows}");
        if let Some(s) = session {
            url.push_str("&session=");
            url.push_str(s);
        }
        if let Some(d) = dir {
            url.push_str("&dir=");
            url.push_str(&encode(d));
        }
        if let Some((name, version)) = term {
            url.push_str("&term=");
            url.push_str(&encode(name));
            url.push_str("&term_version=");
            url.push_str(&encode(version));
        }
        (url, self.auth())
    }

    /// Where the environment's desktop's WebSocket is (RFB to its VNC
    /// server), and the header that signs it in.
    pub fn desktop_socket(&self, id: &str) -> (String, String) {
        let base = match self.base.split_once("://") {
            Some(("https", rest)) => format!("wss://{rest}"),
            Some((_, rest)) => format!("ws://{rest}"),
            None => self.base.clone(),
        };
        (format!("{base}/environments/{id}/desktop"), self.auth())
    }

    /// Sets the size of the environment's desktop, in pixels.
    pub fn resize_desktop(&self, id: &str, width: u32, height: u32) -> Result<(), Error> {
        let body = types::ResizeDesktopBody {
            width: width.into(),
            height: height.into(),
        };
        self.call(|c| c.resize_desktop(id, &body))
    }

    /// Where the environment's Code service's WebSocket is, starting in
    /// root, and the header that signs it in.
    pub fn code_socket(&self, id: &str, root: &str) -> (String, String) {
        let (terminal, auth) = self.terminal_socket(id, None, 0, 0);
        let base = terminal.split("/environments/").next().unwrap_or_default();
        let mut url = format!("{base}/environments/{id}/code");
        if !root.is_empty() {
            url.push_str("?root=");
            url.push_str(&encode(root));
        }
        (url, auth)
    }

    /// Says the user has looked at an agent's session.
    pub fn seen(&self, id: &str, session: &str) -> Result<(), Error> {
        self.call(|c| c.see_agent(id, session))
    }

    /// Opens the event stream: every environment, Synced, then changes; or
    /// coming back with the ID of the last event read, every environment
    /// changed since, then Synced.
    pub fn events(&self, last: Option<&str>) -> Result<Events<impl BufRead + use<>>, Error> {
        use generated::ClientInfo;
        let (_, stream) = self.api.as_ref().map_err(|e| Error::Transport(e.clone()))?;
        let mut req = stream.client().get(format!("{}/events", self.base));
        if let Some(id) = last {
            req = req.header("last-event-id", id);
        }
        let resp = runtime().block_on(async move {
            let r = req
                .send()
                .await
                .map_err(|e| Error::Transport(e.to_string()))?;
            let status = r.status().as_u16();
            if status == 401 {
                return Err(Error::Unauthorized);
            }
            if !r.status().is_success() {
                let said = r.json::<types::Error>().await.map(|e| e.error);
                return Err(Error::Status(status, said.unwrap_or_default()));
            }
            Ok(r)
        })?;
        Ok(Events::new(BufReader::new(StreamReader {
            stream: Box::pin(resp.bytes_stream()),
            chunk: bytes::Bytes::new(),
        })))
    }

    fn auth(&self) -> String {
        format!("Bearer {}", self.token)
    }

    /// Runs a generated call to its end, its answer's body or why it
    /// failed.
    fn call<'a, T, E: Message, F>(
        &'a self,
        f: impl FnOnce(&'a generated::Client) -> F,
    ) -> Result<T, Error>
    where
        F: Future<Output = Result<generated::ResponseValue<T>, generated::Error<E>>>,
    {
        self.answer(|c| {
            let call = f(c);
            async move { call.await.map(generated::ResponseValue::into_inner) }
        })
    }

    /// Every item of a list, from its first page on: a server may page any
    /// list, saying where each next page is.
    fn all<P: Page>(&self, first: P) -> Result<Vec<P::Item>, Error> {
        let (mut items, mut next) = first.parts();
        let mut pages = 1;
        while let Some(path) = next.take() {
            pages += 1;
            if pages > MAX_PAGES {
                return Err(Error::Decode(format!(
                    "list has more than {MAX_PAGES} pages"
                )));
            }
            let (more, after) = self.page::<P>(&path)?.parts();
            items.extend(more);
            next = after;
        }
        Ok(items)
    }

    /// The page of a list at path, beneath the server.
    fn page<P: Page>(&self, path: &str) -> Result<P, Error> {
        use generated::ClientInfo;
        let (calls, _) = self.api.as_ref().map_err(|e| Error::Transport(e.clone()))?;
        let root = self.base.trim_end_matches(&format!("/api/{VERSION}"));
        let url = format!("{root}{path}");
        let http = calls.client().clone();
        runtime().block_on(async move {
            let r = http
                .get(url)
                .send()
                .await
                .map_err(|e| Error::Transport(e.to_string()))?;
            let status = r.status().as_u16();
            if status == 401 {
                return Err(Error::Unauthorized);
            }
            if !r.status().is_success() {
                let said = r.json::<types::Error>().await.map(|e| e.error);
                return Err(Error::Status(status, said.unwrap_or_default()));
            }
            r.json::<P>()
                .await
                .map_err(|e| Error::Decode(e.to_string()))
        })
    }

    /// Runs a call that gives its answer as it is (hangar-openapi's calls
    /// for the operations that answer two ways) to its end.
    fn answer<'a, T, E: Message, F>(
        &'a self,
        f: impl FnOnce(&'a generated::Client) -> F,
    ) -> Result<T, Error>
    where
        F: Future<Output = Result<T, generated::Error<E>>>,
    {
        let (calls, _) = self.api.as_ref().map_err(|e| Error::Transport(e.clone()))?;
        let call = f(calls);
        runtime().block_on(async move {
            match call.await {
                Ok(v) => Ok(v),
                Err(e) => Err(error(e).await),
            }
        })
    }
}

/// The most pages a list is followed through, against a server that
/// names a next page for ever.
const MAX_PAGES: usize = 1000;

/// A page of a list (api/client.yaml's Lists): its items, and the path of
/// the next page.
trait Page: serde::de::DeserializeOwned + Send + 'static {
    type Item;
    fn parts(self) -> (Vec<Self::Item>, Option<String>);
}

macro_rules! pages {
    ($($list:ident => $item:ident),* $(,)?) => {$(
        impl Page for types::$list {
            type Item = types::$item;
            fn parts(self) -> (Vec<types::$item>, Option<String>) {
                (self.items, self.next)
            }
        }
    )*};
}

pages!(
    LoginKeyList => LoginKey,
    AccessTokenList => AccessToken,
    SshKeyList => SshKey,
    EnvironmentList => Environment,
    TemplateList => Template,
    EnvironmentPackList => EnvironmentPack,
    WorkerSizeList => Resources,
    TerminalSessionList => TerminalSession,
    PackList => Pack,
    CopyList => Copy,
    ReplacedFileList => ReplacedFile,
    PersonList => Person,
    TeamList => Team,
    ImageRepositoryList => ImageRepository,
    UserList => User,
    WorkerList => Worker,
);

/// What an error answer says: the spec's Error body, or nothing for a call
/// the spec gives no error body.
trait Message: std::fmt::Debug {
    fn message(self) -> String;
}

impl Message for types::Error {
    fn message(self) -> String {
        self.error
    }
}

impl Message for () {
    fn message(self) -> String {
        String::new()
    }
}

/// A failed call, as the app tells it.
async fn error<E: Message>(e: generated::Error<E>) -> Error {
    let status = |code: u16, msg: String| {
        if code == 401 {
            Error::Unauthorized
        } else {
            Error::Status(code, msg)
        }
    };
    match e {
        generated::Error::ErrorResponse(r) => {
            let code = r.status().as_u16();
            status(code, r.into_inner().message())
        }
        generated::Error::UnexpectedResponse(r) => {
            #[derive(Deserialize)]
            struct Body {
                error: String,
            }
            let code = r.status().as_u16();
            let msg = r.json::<Body>().await.map(|b| b.error).unwrap_or_default();
            status(code, msg)
        }
        generated::Error::InvalidResponsePayload(_, e) => Error::Decode(e.to_string()),
        e => Error::Transport(e.to_string()),
    }
}

/// The event stream's bytes, read as they arrive by whichever thread reads
/// the stream, each read held to STREAM_READ.
struct StreamReader {
    stream: std::pin::Pin<
        Box<dyn futures_core::Stream<Item = reqwest::Result<bytes::Bytes>> + Send + Sync>,
    >,
    chunk: bytes::Bytes,
}

impl Read for StreamReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.chunk.is_empty() {
            let next = runtime().block_on(async {
                tokio::time::timeout(
                    STREAM_READ,
                    std::future::poll_fn(|cx| self.stream.as_mut().poll_next(cx)),
                )
                .await
            });
            match next {
                Err(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "event stream timed out",
                    ));
                }
                Ok(None) => return Ok(0),
                Ok(Some(Err(e))) => return Err(io::Error::other(e)),
                Ok(Some(Ok(bytes))) => self.chunk = bytes,
            }
        }
        let n = buf.len().min(self.chunk.len());
        buf[..n].copy_from_slice(&self.chunk[..n]);
        self.chunk = self.chunk.slice(n..);
        Ok(n)
    }
}

/// The events of a server-sent event stream, as this API sends them.
/// Events of types the app does not know are passed over.
pub struct Events<R> {
    r: R,
    line: String,
    last_id: Option<String>,
}

impl<R: BufRead> Events<R> {
    pub fn new(r: R) -> Self {
        Events {
            r,
            line: String::new(),
            last_id: None,
        }
    }

    /// The ID of the last event read that had one, to come back with.
    pub fn last_id(&self) -> Option<&str> {
        self.last_id.as_deref()
    }
}

impl<R: BufRead> Iterator for Events<R> {
    type Item = Result<Event, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        let mut name = String::new();
        let mut data = String::new();
        loop {
            self.line.clear();
            match self.r.read_line(&mut self.line) {
                Ok(0) => return None,
                Ok(_) => {}
                Err(e) => return Some(Err(Error::Transport(e.to_string()))),
            }
            let line = self.line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if data.is_empty() && name.is_empty() {
                    continue;
                }
                let event = parse(&name, &data);
                name.clear();
                data.clear();
                match event {
                    Ok(Some(e)) => return Some(Ok(e)),
                    Ok(None) => continue,
                    Err(e) => return Some(Err(e)),
                }
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "event" => name = value.to_string(),
                "data" => {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(value);
                }
                "id" => self.last_id = Some(value.to_string()),
                _ => {}
            }
        }
    }
}

fn parse(name: &str, data: &str) -> Result<Option<Event>, Error> {
    let decode = |e: serde_json::Error| Error::Decode(format!("invalid {name} event: {e}"));
    Ok(Some(match name {
        "environment" => Event::Environment(Box::new(serde_json::from_str(data).map_err(decode)?)),
        "removed" => {
            #[derive(Deserialize)]
            struct Removed {
                id: String,
            }
            Event::Removed(serde_json::from_str::<Removed>(data).map_err(decode)?.id)
        }
        "synced" => {
            #[derive(Deserialize)]
            struct Synced {
                #[serde(default)]
                ids: Vec<String>,
            }
            Event::Synced(serde_json::from_str::<Synced>(data).map_err(decode)?.ids)
        }
        _ => return Ok(None),
    }))
}

/// A query value, percent-encoded.
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                (b as char).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect()
}
