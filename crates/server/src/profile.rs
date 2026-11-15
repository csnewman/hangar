//! The session the server holds with every running environment whose
//! worker's tunnel reaches it (internal/profile's sessions), on the
//! guest agent's port 105: one JSON line per message each way.
//!
//! The server sends the public keys the guest's SSH agent offers, the
//! copies it routes shared files to and their paths, how conflicts are
//! resolved, its task, and Hangar's registry; each again when it changes.
//! The guest asks for signatures with its owner's keys, credentials for
//! the registry, and to set its task, and says which copies it routes to,
//! the conflicts it finds and resolves, and its coding agents' sessions.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use base64::Engine;
use bytes::BytesMut;
use chrono::Utc;
use hangar_api::{Access, Spec};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::audit::{self, Actor, Event};
use crate::http::Shared;
use crate::store::environments::Only;
use crate::store::rows::AgentSession;
use crate::tunnel::{self, Conn, Header};

/// Starts every registry credential, so the registry tells one from an
/// access token.
pub const REGISTRY_CREDENTIAL_PREFIX: &str = "hge_";

/// What a registry credential is sealed bound to.
pub const REGISTRY_CREDENTIAL_BINDING: &str = "registry-credential";

/// How long a registry credential works.
const CREDENTIAL_LIFETIME: chrono::Duration = chrono::Duration::hours(1);

/// How often the sessions are looked over without being asked.
const RECONCILE: Duration = Duration::from_secs(3);

/// How often a session sends again what has changed, whatever it heard.
const RESEND: Duration = Duration::from_secs(5);

/// The longest line a guest may send.
const MAX_LINE: usize = 16 << 20;

/// The most coding agents' sessions an environment's report keeps, and the
/// longest of their texts.
const MAX_AGENTS: usize = 64;
const MAX_AGENT_TEXT: usize = 300;

/// An environment a session is held with.
#[derive(Debug, Clone, PartialEq)]
struct Target {
    env: Uuid,
    name: String,
    owner: Uuid,
    owner_name: String,
    worker: Uuid,
    access: Access,
}

/// Keeps a session with each environment that should have one until the
/// process ends.
pub async fn run(app: Shared) {
    let mut held: HashMap<Uuid, (Target, tokio::task::JoinHandle<()>)> = HashMap::new();
    let changes = app.store.signals.watch_changes();
    loop {
        match wanted(&app).await {
            Ok(want) => {
                held.retain(|id, (t, task)| {
                    let keep = want.get(id) == Some(t) && !task.is_finished();
                    if !keep {
                        task.abort();
                    }
                    keep
                });
                for (id, t) in want {
                    if held.contains_key(&id) {
                        continue;
                    }
                    let task = tokio::spawn(session(app.clone(), t.clone()));
                    held.insert(id, (t, task));
                }
            }
            Err(e) => eprintln!("hangar-server: profile: listing running environments: {e}"),
        }
        tokio::select! {
            _ = tokio::time::sleep(RECONCILE) => {}
            _ = changes.ready() => {
                let _ = changes.take();
            }
        }
    }
}

/// The running environments whose workers' tunnels this server holds.
async fn wanted(app: &Shared) -> crate::store::Result<HashMap<Uuid, Target>> {
    let workers: HashSet<Uuid> = app.tunnels.connected().into_iter().collect();
    if workers.is_empty() {
        return Ok(HashMap::new());
    }
    let mut tx = app.store.begin().await?;
    let names: HashMap<Uuid, String> = tx
        .people()
        .await?
        .into_iter()
        .map(|u| (u.id, u.username))
        .collect();
    Ok(tx
        .environments(Only::All)
        .await?
        .into_iter()
        .filter(|e| matches!(e.phase.as_str(), "starting" | "running") && e.desired == "running")
        .filter_map(|e| {
            let worker = e.worker_id.filter(|w| workers.contains(w))?;
            let spec: Spec = serde_json::from_value(e.spec).unwrap_or_default();
            Some((
                e.id,
                Target {
                    env: e.id,
                    name: e.name,
                    owner: e.owner_id,
                    owner_name: names.get(&e.owner_id).cloned().unwrap_or_default(),
                    worker,
                    access: spec.access,
                },
            ))
        })
        .collect())
}

/// What a session last sent, so it sends again only what changed.
#[derive(Default)]
struct Sent {
    keys: Option<Vec<String>>,
    sets: Option<Vec<(Uuid, Vec<String>)>>,
    task: Option<Option<Uuid>>,
    /// The conflicts' resolutions sent, by copy and path.
    resolving: HashSet<(Uuid, String)>,
}

async fn session(app: Shared, t: Target) {
    if let Err(e) = serve(&app, &t).await {
        eprintln!("hangar-server: profile session with {} ended: {e}", t.env);
    }
}

async fn send(conn: &mut Conn, m: &Value) -> std::io::Result<()> {
    let mut b = serde_json::to_vec(m).map_err(std::io::Error::other)?;
    b.push(b'\n');
    tokio::time::timeout(Duration::from_secs(30), conn.write_all(&b))
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "the guest took no message for 30 seconds",
            )
        })?
}

fn bad(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// Holds one session until it fails or the process ends; the next look
/// over the sessions starts another if the environment still wants one.
/// Its reads and writes are polled from this one task, as a tunnel's
/// stream needs.
async fn serve(app: &Shared, t: &Target) -> std::io::Result<()> {
    let env = t.env.to_string();
    let mut conn = app
        .tunnels
        .open(
            t.worker,
            &Header {
                kind: tunnel::KIND_PROFILE,
                environment: &env,
            },
        )
        .await
        .map_err(bad)?;
    let mut sent = Sent::default();
    // The keys first, which a clone as the environment starts waits for;
    // then the shared files, which the agent routes to the worker's.
    send_keys(app, t, &mut conn, &mut sent).await?;
    if let Some(host) = registry_host(app)
        && !t.access.no_registry
    {
        send(&mut conn, &json!({ "type": "registry", "host": host })).await?;
    }
    send_sets(app, t, &mut conn, &mut sent).await?;
    send_resolutions(app, t, &mut conn, &mut sent).await?;
    send_task(app, t, &mut conn, &mut sent).await?;

    let changes = app.store.signals.watch_changes();
    let mut tick = tokio::time::interval(RESEND);
    tick.reset();
    let mut buf = BytesMut::new();
    loop {
        tokio::select! {
            r = conn.read_buf(&mut buf) => {
                if r? == 0 {
                    return Ok(());
                }
                while let Some(n) = buf.iter().position(|&b| b == b'\n') {
                    let line = buf.split_to(n + 1);
                    let m: Value = serde_json::from_slice(&line[..n]).map_err(|e| bad(format!("a malformed message from the agent: {e}")))?;
                    heard(app, t, &mut conn, &mut sent, &m).await?;
                }
                if buf.len() > MAX_LINE {
                    return Err(bad("the agent sent a line too long"));
                }
            }
            _ = tick.tick() => {}
            _ = app.store.signals.shared.notified() => {}
            _ = changes.ready() => {
                let (ids, all) = changes.take();
                if !all && !ids.contains(&t.env) {
                    continue;
                }
            }
        }
        send_sets(app, t, &mut conn, &mut sent).await?;
        send_keys(app, t, &mut conn, &mut sent).await?;
        send_resolutions(app, t, &mut conn, &mut sent).await?;
        send_task(app, t, &mut conn, &mut sent).await?;
    }
}

/// Hangar's registry's host, when it runs one.
fn registry_host(app: &Shared) -> Option<String> {
    app.cfg
        .public
        .as_ref()
        .filter(|_| app.cfg.registry)
        .map(|p| p.name("registry"))
}

/// Sends the public keys the environment's SSH agent offers, when they are
/// not what was last sent.
async fn send_keys(
    app: &Shared,
    t: &Target,
    conn: &mut Conn,
    sent: &mut Sent,
) -> std::io::Result<()> {
    let keys: Vec<String> = if t.access.no_ssh_keys {
        Vec::new()
    } else {
        let mut tx = app.store.begin().await.map_err(bad)?;
        tx.ssh_keys(t.owner)
            .await
            .map_err(bad)?
            .into_iter()
            .map(|k| k.public_key)
            .collect()
    };
    if sent.keys.as_ref() == Some(&keys) {
        return Ok(());
    }
    let m = if keys.is_empty() {
        json!({ "type": "keys" })
    } else {
        json!({ "type": "keys", "keys": keys })
    };
    sent.keys = Some(keys);
    send(conn, &m).await
}

/// Sends the copies the environment uses and their paths, when they are
/// not what was last sent.
async fn send_sets(
    app: &Shared,
    t: &Target,
    conn: &mut Conn,
    sent: &mut Sent,
) -> std::io::Result<()> {
    let sets = crate::packs::environment_sets(&app.store, t.env)
        .await
        .map_err(|e| bad(format!("{e:?}")))?;
    if sent.sets.as_ref() == Some(&sets) {
        return Ok(());
    }
    let list: Vec<Value> = sets
        .iter()
        .map(|(id, paths)| json!({ "id": id.to_string(), "paths": paths }))
        .collect();
    let m = if list.is_empty() {
        json!({ "type": "paths" })
    } else {
        json!({ "type": "paths", "sets": list })
    };
    sent.sets = Some(sets);
    send(conn, &m).await
}

/// Sends how the environment's conflicts are resolved, each once a
/// session.
async fn send_resolutions(
    app: &Shared,
    t: &Target,
    conn: &mut Conn,
    sent: &mut Sent,
) -> std::io::Result<()> {
    let conflicts = crate::packs::conflicts(&app.store, t.env)
        .await
        .map_err(bad)?;
    for c in conflicts {
        let Some(r) = &c.resolution else {
            continue;
        };
        if !sent.resolving.insert((c.copy_id, c.path.clone())) {
            continue;
        }
        send(
            conn,
            &json!({ "type": "resolve", "copy": c.copy_id.to_string(), "path": c.path, "resolution": r }),
        )
        .await?;
    }
    Ok(())
}

fn task_json(t: &crate::store::rows::Task) -> Value {
    let mut out =
        json!({ "id": t.id.to_string(), "text": t.text, "set_by": t.set_by, "set_at": t.set_at });
    if let Some(d) = t.done_at {
        out["done_at"] = json!(d);
    }
    out
}

/// Sends the environment's task, when it is not the one last sent.
async fn send_task(
    app: &Shared,
    t: &Target,
    conn: &mut Conn,
    sent: &mut Sent,
) -> std::io::Result<()> {
    let task = app
        .store
        .begin()
        .await
        .map_err(bad)?
        .current_tasks(&[t.env])
        .await
        .map_err(bad)?
        .pop();
    let id = task.as_ref().map(|t| t.id);
    if sent.task == Some(id) {
        return Ok(());
    }
    sent.task = Some(id);
    let m = match &task {
        Some(task) => json!({ "type": "task", "task": task_json(task) }),
        None => json!({ "type": "task" }),
    };
    send(conn, &m).await
}

fn text<'a>(m: &'a Value, k: &str) -> &'a str {
    m.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Takes one message from the guest, answering a request.
async fn heard(
    app: &Shared,
    t: &Target,
    conn: &mut Conn,
    sent: &mut Sent,
    m: &Value,
) -> std::io::Result<()> {
    let id = m.get("id").and_then(Value::as_i64).unwrap_or(0);
    match text(m, "type") {
        "routed" => {
            let routed: Vec<String> = m
                .get("routed")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            crate::packs::set_routed(&app.store, t.env, &routed)
                .await
                .map_err(|e| bad(format!("{e:?}")))?;
        }
        "conflict" => {
            // A conflict that is not one is the guest's mistake, not the
            // session's end.
            let _ = crate::packs::add_conflict(&app.store, t.env, text(m, "copy"), text(m, "path"))
                .await;
        }
        "resolved" => {
            if let Ok(copy) = Uuid::parse_str(text(m, "copy")) {
                sent.resolving.remove(&(copy, text(m, "path").to_string()));
            }
            crate::packs::conflict_done(&app.store, t.env, text(m, "copy"), text(m, "path"))
                .await
                .map_err(|e| bad(format!("{e:?}")))?;
        }
        "agents" => {
            let sessions = valid_agents(t.env, m.get("agents").and_then(Value::as_array));
            let mut tx = app.store.begin().await.map_err(bad)?;
            tx.report_agents(t.env, &sessions).await.map_err(bad)?;
            tx.commit().await.map_err(bad)?;
        }
        "get_credential" => {
            let mut reply = json!({ "type": "credential", "id": id });
            match (registry_host(app), &app.cfg.sealer) {
                (None, _) => reply["error"] = json!("this server runs no registry"),
                _ if t.access.no_registry => {
                    reply["error"] =
                        json!("this environment's template gives it no credential for the registry")
                }
                (Some(_), None) => reply["error"] = json!(crate::keys::NO_SECRET_KEY),
                (Some(host), Some(sealer)) => {
                    reply["host"] = json!(host);
                    reply["username"] = json!(t.owner_name);
                    reply["secret"] = json!(issue_registry_credential(sealer, t.owner, t.env));
                }
            }
            send(conn, &reply).await?;
        }
        "set_task" => {
            let mut reply = json!({ "type": "task_set", "id": id });
            match set_task(app, t, text(m, "text")).await {
                Ok(Some(task)) => reply["task"] = task_json(&task),
                Ok(None) => {}
                Err(why) => reply["error"] = json!(why),
            }
            send(conn, &reply).await?;
        }
        "sign" => {
            let mut reply = json!({ "type": "signed", "id": id });
            let (signed, fingerprint) = sign(app, t, m).await;
            let actor = Actor {
                user: Some(t.owner),
                name: t.owner_name.clone(),
                via: format!("environment:{} ({})", t.env, t.name),
                ..Actor::default()
            };
            let mut details = json!({ "fingerprint": fingerprint });
            if let Err(e) = &signed {
                details["error"] = json!(e);
            }
            let record = async {
                let mut tx = app.store.begin().await?;
                audit::record(
                    &mut tx,
                    &actor,
                    Event::new("ssh_key.sign", "ssh_key", &fingerprint, &fingerprint)
                        .related("owner", t.owner)
                        .details(details),
                )
                .await?;
                tx.commit().await
            };
            if let Err(e) = record.await {
                eprintln!("hangar-server: recording a signature: {e}");
            }
            match signed {
                Ok(sig) => {
                    reply["signature"] =
                        json!(base64::engine::general_purpose::STANDARD.encode(sig))
                }
                Err(e) => reply["error"] = json!(e),
            }
            send(conn, &reply).await?;
        }
        _ => {}
    }
    Ok(())
}

/// Sets the environment's task as its coding agent does; an empty text
/// marks it done.
async fn set_task(
    app: &Shared,
    t: &Target,
    text: &str,
) -> Result<Option<crate::store::rows::Task>, String> {
    let failed = |e: crate::fail::Fail| match e {
        crate::fail::Fail::Invalid(m)
        | crate::fail::Fail::Conflict(m)
        | crate::fail::Fail::NotFound(m)
        | crate::fail::Fail::Forbidden(m) => m,
        crate::fail::Fail::Store(e) => e.to_string(),
    };
    crate::environments::agent_set_task(&app.store, t.env, text)
        .await
        .map_err(failed)
}

/// A credential for Hangar's registry, for a person, asked for by one of
/// their environments: who it is for, which environment asked, and when it
/// stops working, sealed, and good for CREDENTIAL_LIFETIME.
pub fn issue_registry_credential(sealer: &crate::seal::Sealer, user: Uuid, env: Uuid) -> String {
    let claims = json!({
        "u": user.to_string(),
        "e": env.to_string(),
        "x": (Utc::now() + CREDENTIAL_LIFETIME).timestamp(),
    });
    let sealed = sealer.seal(claims.to_string().as_bytes(), REGISTRY_CREDENTIAL_BINDING);
    format!(
        "{REGISTRY_CREDENTIAL_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sealed)
    )
}

/// Signs with the owner's key that matches the one asked for, as an SSH
/// agent would with the flags given; and the key's fingerprint.
async fn sign(app: &Shared, t: &Target, m: &Value) -> (Result<Vec<u8>, String>, String) {
    let b64 = base64::engine::general_purpose::STANDARD;
    let key = b64.decode(text(m, "key")).unwrap_or_default();
    let data = b64.decode(text(m, "data")).unwrap_or_default();
    let flags = m.get("flags").and_then(Value::as_u64).unwrap_or(0);
    let fingerprint = russh::keys::ssh_key::PublicKey::from_bytes(&key)
        .map(|k| crate::keys::fingerprint(&k))
        .unwrap_or_default();
    if t.access.no_ssh_keys {
        return (
            Err("this environment's template gives it none of its owner's SSH keys".into()),
            fingerprint,
        );
    }
    let Some(sealer) = &app.cfg.sealer else {
        return (Err(crate::keys::NO_SECRET_KEY.into()), fingerprint);
    };
    let rows = match app.store.begin().await {
        Ok(mut tx) => tx.ssh_keys(t.owner).await,
        Err(e) => Err(e),
    };
    let rows = match rows {
        Ok(r) => r,
        Err(e) => return (Err(e.to_string()), fingerprint),
    };
    for row in rows {
        let Some(public) = row
            .public_key
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .join(" ")
            .parse::<russh::keys::ssh_key::PublicKey>()
            .ok()
        else {
            continue;
        };
        let wire = ssh_key_bytes(&public);
        if wire.as_deref() != Some(key.as_slice()) {
            continue;
        }
        let bound = format!("{}/ssh/{}", t.owner, crate::keys::fingerprint(&public));
        let signed = sealer
            .open(&row.private_key, &bound)
            .and_then(|pem| String::from_utf8(pem).map_err(|e| e.to_string()))
            .and_then(|pem| russh::keys::decode_secret_key(&pem, None).map_err(|e| e.to_string()))
            .and_then(|k| sign_with(&k, &data, flags));
        return (signed, fingerprint);
    }
    (Err("no such key".into()), fingerprint)
}

/// A public key in SSH wire form.
fn ssh_key_bytes(k: &russh::keys::ssh_key::PublicKey) -> Option<Vec<u8>> {
    use russh::keys::ssh_key::encoding::Encode;
    k.key_data().encode_vec().ok()
}

/// A signature of data by key, in SSH wire form: for RSA, SHA-512 with
/// flag 4 and SHA-256 with flag 2, as the agent protocol says.
fn sign_with(key: &russh::keys::PrivateKey, data: &[u8], flags: u64) -> Result<Vec<u8>, String> {
    use russh::keys::ssh_key::HashAlg;
    use russh::keys::ssh_key::encoding::Encode;
    use russh::keys::ssh_key::private::KeypairData;
    let sig: russh::keys::ssh_key::Signature = match key.key_data() {
        KeypairData::Rsa(rsa) => {
            let hash = if flags & 4 != 0 {
                Some(HashAlg::Sha512)
            } else if flags & 2 != 0 {
                Some(HashAlg::Sha256)
            } else {
                None
            };
            signature::Signer::try_sign(&(rsa, hash), data).map_err(|e| e.to_string())?
        }
        _ => signature::Signer::try_sign(key, data).map_err(|e| e.to_string())?,
    };
    sig.encode_vec().map_err(|e| e.to_string())
}

/// s at most n bytes long, cut at a character's start.
fn cut(s: &str, n: usize) -> String {
    if s.len() <= n {
        return s.to_string();
    }
    let mut n = n;
    while n > 0 && !s.is_char_boundary(n) {
        n -= 1;
    }
    s[..n].to_string()
}

/// A report of coding agents' sessions kept to the limits: the newest, of
/// the states and agents known, their texts cut short. It is the
/// environment's own account, kept only as far as it is sensible.
fn valid_agents(env: Uuid, input: Option<&Vec<Value>>) -> Vec<AgentSession> {
    let mut out: Vec<AgentSession> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for a in input.into_iter().flatten() {
        let state = text(a, "state");
        let agent = text(a, "agent");
        let id = text(a, "id");
        if !matches!(state, "asking" | "finished" | "working" | "idle" | "ended")
            || id.is_empty()
            || id.len() > 128
            || !matches!(agent, "claude" | "codex")
            || !seen.insert(id.to_string())
        {
            continue;
        }
        let Some(since) = a
            .get("since")
            .and_then(Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        else {
            continue;
        };
        out.push(AgentSession {
            environment_id: env,
            id: id.to_string(),
            agent: agent.to_string(),
            terminal: cut(text(a, "terminal"), 64),
            state: state.to_string(),
            step: cut(text(a, "step"), MAX_AGENT_TEXT),
            message: cut(text(a, "message"), MAX_AGENT_TEXT),
            since: since.with_timezone(&Utc),
            seen_at: None,
        });
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.since));
    out.truncate(MAX_AGENTS);
    out
}
