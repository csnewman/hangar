//! What reaches into a running environment through its worker's tunnel:
//! its terminals, its Code service, its desktop, its processes and its
//! logs, in the web UI's API and the client API alike; and the sign-in to
//! its own hosts.

use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, RawQuery, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use bytes::{Buf, BytesMut};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use super::{ApiError, Body, Shared, Signed, Via, code};
use crate::environments::{self as envs, View};
use crate::tunnel::{self, Conn, Header, OpenError};

pub fn frontend() -> Router<Shared> {
    Router::new()
        .route("/environments/{id}/ports/sign-in", get(sign_in_ports))
        .route("/environments/{id}/terminals", get(list_terminals))
        .route(
            "/environments/{id}/terminals/{session}",
            delete(close_terminal),
        )
        .route("/environments/{id}/terminal", get(terminal_socket))
        .route("/environments/{id}/code", get(code_socket))
        .route("/environments/{id}/desktop", get(desktop_socket))
        .route("/environments/{id}/desktop/size", put(resize_desktop))
        .route("/environments/{id}/processes", get(list_processes))
        .route(
            "/environments/{id}/processes/{pid}/signal",
            post(signal_process),
        )
        .route("/environments/{id}/logs/{log}", get(environment_log))
        .route("/workers/{id}/log", get(worker_log))
}

pub fn client() -> Router<Shared> {
    Router::new()
        .route("/environments/{id}/terminals", get(list_terminals))
        .route(
            "/environments/{id}/terminals/{session}",
            delete(close_terminal),
        )
        .route("/environments/{id}/terminal", get(terminal_socket))
        .route("/environments/{id}/code", get(code_socket))
        .route("/environments/{id}/desktop", get(desktop_socket))
        .route("/environments/{id}/desktop/size", post(resize_desktop))
        .route("/environments/{id}/processes", get(list_processes))
        .route(
            "/environments/{id}/processes/{pid}/signal",
            post(signal_process),
        )
        .route("/environments/{id}/logs/{log}", get(environment_log))
}

const NOT_RUNNING: &str = "the environment is not running";
const UNREACHABLE: &str = "the environment's worker cannot be reached from this server just now";
const NOT_PLACED: &str = "the environment has not been placed on a worker yet, so it has no logs";

/// How often a socket is pinged, and how soon its pong must come back: a
/// laptop shut or a network gone leaves a socket nothing at the other end
/// will close, and what it holds in the environment would be held for good.
const SOCKET_PING: Duration = Duration::from_secs(20);

/// The largest terminal frame's payload.
const MAX_TERMINAL_FRAME: usize = 1 << 20;

/// The largest frame the Code service or a client sends: a whole file's
/// shared document on first sync.
const MAX_CODE_FRAME: usize = 64 << 20;

/// Whether a request came to the client API, by its route: a nested
/// router's handlers see the path without its prefix.
fn client_api(parts: &Parts) -> bool {
    parts
        .extensions
        .get::<axum::extract::MatchedPath>()
        .is_some_and(|m| m.as_str().starts_with("/api/v1/"))
}

fn env_id(id: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(id).map_err(|_| ApiError::not_found("environment"))
}

/// An environment the asker may reach, and its worker, while it runs.
async fn running(app: &Shared, s: &Signed, id: &str) -> Result<(View, Uuid), ApiError> {
    let v = envs::get(&app.store, &s.principal, env_id(id)?)
        .await?
        .ok_or_else(|| ApiError::not_found("environment"))?;
    match v.row.worker_id {
        Some(w) if v.row.phase == "running" => Ok((v, w)),
        _ => Err(ApiError::new(StatusCode::CONFLICT, NOT_RUNNING).code(code::NOT_RUNNING)),
    }
}

/// Opens a stream of kind to an environment through its worker.
async fn open(app: &Shared, worker: Uuid, kind: &str, env: &str) -> Result<Conn, ApiError> {
    app.tunnels
        .open(
            worker,
            &Header {
                kind,
                environment: env,
            },
        )
        .await
        .map_err(|e| {
            if !matches!(e, OpenError::NoTunnel) {
                eprintln!("hangar-server: opening a {kind} stream to {env}: {e}");
            }
            ApiError::new(StatusCode::SERVICE_UNAVAILABLE, UNREACHABLE).code(code::UNREACHABLE)
        })
}

fn unreachable_error() -> ApiError {
    ApiError::new(StatusCode::SERVICE_UNAVAILABLE, UNREACHABLE).code(code::UNREACHABLE)
}

/// Writes one JSON line.
async fn send_line(conn: &mut Conn, v: &Value) -> std::io::Result<()> {
    let mut b = serde_json::to_vec(v).map_err(std::io::Error::other)?;
    b.push(b'\n');
    conn.write_all(&b).await
}

/// Reads one line, leaving what follows it in buf.
async fn read_line(conn: &mut Conn, buf: &mut BytesMut) -> std::io::Result<Vec<u8>> {
    loop {
        if let Some(n) = buf.iter().position(|&b| b == b'\n') {
            let line = buf.split_to(n + 1);
            return Ok(line[..n].to_vec());
        }
        if conn.read_buf(buf).await? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
    }
}

/// Sends a request line and reads the reply line, within limit.
async fn ask(conn: &mut Conn, req: &Value, limit: Duration) -> Result<(Value, BytesMut), ApiError> {
    let exchange = async {
        send_line(conn, req).await?;
        let mut buf = BytesMut::new();
        let line = read_line(conn, &mut buf).await?;
        Ok::<_, std::io::Error>((line, buf))
    };
    let (line, buf) = match tokio::time::timeout(limit, exchange).await {
        Ok(Ok(x)) => x,
        _ => return Err(unreachable_error()),
    };
    let reply = serde_json::from_slice(&line).map_err(|e| {
        eprintln!("hangar-server: a reply from the guest: {e}");
        ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
    })?;
    Ok((reply, buf))
}

fn reply_str<'a>(reply: &'a Value, key: &str) -> &'a str {
    reply.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Refuses a WebSocket whose Origin is not this host, which keeps another
/// site from opening one with the person's cookie; the apps send none.
fn same_origin(parts: &Parts) -> Result<(), ApiError> {
    let h = |k| {
        parts
            .headers
            .get(k)
            .and_then(|v: &HeaderValue| v.to_str().ok())
    };
    let Some(origin) = h(header::ORIGIN).filter(|o| !o.is_empty()) else {
        return Ok(());
    };
    let host = origin.split_once("://").map(|(_, rest)| rest);
    match (host, h(header::HOST)) {
        (Some(o), Some(host)) if o.eq_ignore_ascii_case(host) => Ok(()),
        _ => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "a WebSocket from another site is refused",
        )),
    }
}

/// Upgrades parts' request to a WebSocket, offering protocols.
async fn upgrade(
    app: &Shared,
    mut parts: Parts,
    protocols: &[&'static str],
) -> Result<WebSocketUpgrade, ApiError> {
    same_origin(&parts)?;
    let ws = <WebSocketUpgrade as axum::extract::FromRequestParts<Shared>>::from_request_parts(
        &mut parts, app,
    )
    .await
    .map_err(|e| ApiError::new(StatusCode::BAD_REQUEST, e.body_text()))?;
    Ok(ws.protocols(protocols.iter().copied()))
}

async fn close(ws: &mut WebSocket, code: u16, reason: &str) {
    let _ = ws
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

/// Pings a socket every SOCKET_PING, ending it when a pong did not come
/// back in time: ping says whether one may be sent.
struct KeepAlive {
    tick: tokio::time::Interval,
    waiting: bool,
}

impl KeepAlive {
    fn new() -> KeepAlive {
        let mut tick = tokio::time::interval(SOCKET_PING);
        tick.reset();
        KeepAlive {
            tick,
            waiting: false,
        }
    }

    /// Waits for the next ping, and says whether the socket is alive.
    async fn due(&mut self) -> bool {
        self.tick.tick().await;
        !std::mem::replace(&mut self.waiting, true)
    }

    fn pong(&mut self) {
        self.waiting = false;
    }
}

/// What a socket said: a message to act on, a ping of the client's own
/// answered, or its end.
enum Said {
    Binary(bytes::Bytes),
    Other,
    End,
}

/// Takes one message from a socket, answering a client's text "ping" with
/// "pong" when answer_pings: a browser cannot see the socket's own pings.
async fn heard(
    ws: &mut WebSocket,
    msg: Option<Result<Message, axum::Error>>,
    keep: &mut KeepAlive,
    answer_pings: bool,
) -> Said {
    match msg {
        Some(Ok(Message::Binary(b))) => Said::Binary(b),
        Some(Ok(Message::Text(t))) => {
            if answer_pings
                && t.as_str() == "ping"
                && ws.send(Message::Text("pong".into())).await.is_err()
            {
                return Said::End;
            }
            Said::Other
        }
        Some(Ok(Message::Pong(_))) => {
            keep.pong();
            Said::Other
        }
        Some(Ok(Message::Ping(_))) => Said::Other,
        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => Said::End,
    }
}

async fn sign_in_ports(
    State(app): State<Shared>,
    Path(id): Path<String>,
    RawQuery(q): RawQuery,
    mut parts: Parts,
) -> Result<Response, ApiError> {
    let Some(public) = app.cfg.public.clone() else {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "this Hangar has no public URL set, which environments' hosts are named after",
        ));
    };
    let to = url::form_urlencoded::parse(q.unwrap_or_default().as_bytes())
        .find(|(k, _)| k == "to")
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default();
    let env = env_id(&id)?;
    let session = match Signed::authenticate(&mut parts, &app).await {
        Ok(s) => match &s.via {
            Via::Session(token) => Some((token.clone(), s)),
            Via::Token(_) => None,
        },
        Err(_) => None,
    };
    let Some((token, s)) = session else {
        let back = format!(
            "{}?{}",
            crate::ports::sign_in_path(env),
            url::form_urlencoded::Serializer::new(String::new())
                .append_pair("to", &to)
                .finish()
        );
        let next = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("next", &back)
            .finish();
        return Ok(see_other(&format!("/login?{next}")));
    };
    let v = envs::get(&app.store, &s.principal, env)
        .await?
        .ok_or_else(|| ApiError::not_found("environment"))?;
    let loc =
        match crate::ports::sign_in_url(&app.store, &public, &token, env, &v.row.short_id, &to)
            .await
        {
            Ok(found) => found?,
            Err(why) => return Err(ApiError::new(StatusCode::BAD_REQUEST, why)),
        };
    envs::record_access(
        &app.store,
        &s.principal.actor(&s.ip),
        &v,
        "environment.open_ports",
        json!({ "url": to }),
    )
    .await;
    let mut resp = see_other(&loc);
    s.set_issued(&app, &mut resp);
    Ok(resp)
}

fn see_other(to: &str) -> Response {
    let mut r = StatusCode::SEE_OTHER.into_response();
    if let Ok(v) = HeaderValue::from_str(to) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r
}

/// Opens a stream to an environment's terminal sessions and sends it req,
/// giving the reply and the stream, past the reply, for an attach's frames.
async fn terminal(
    app: &Shared,
    s: &Signed,
    id: &str,
    mut req: Value,
) -> Result<(View, Conn, BytesMut, Value), ApiError> {
    let (v, worker) = running(app, s, id).await?;
    let mut conn = open(app, worker, tunnel::KIND_TERMINAL, &v.row.id.to_string()).await?;
    let attach = reply_str(&req, "op") == "attach";
    if attach
        && reply_str(&req, "session").is_empty()
        && reply_str(&req, "dir").is_empty()
        && !v.spec.editor_path.is_empty()
    {
        req["dir"] = json!(v.spec.editor_path);
    }
    let (reply, rest) = ask(&mut conn, &req, Duration::from_secs(30)).await?;
    let actor = s.principal.actor(&s.ip);
    match reply_str(&req, "op") {
        "attach" => {
            let details =
                json!({ "session": reply.get("session"), "asked": reply_str(&req, "session") });
            envs::record_access(
                &app.store,
                &actor,
                &v,
                "environment.terminal_attach",
                details,
            )
            .await;
        }
        "close" => {
            let details = json!({ "session": reply_str(&req, "session") });
            envs::record_access(
                &app.store,
                &actor,
                &v,
                "environment.terminal_close",
                details,
            )
            .await;
        }
        _ => {}
    }
    Ok((v, conn, rest, reply))
}

async fn list_terminals(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Json<Vec<Value>>, ApiError> {
    let (_, _, _, reply) = terminal(&app, &s, &id, json!({ "op": "list" })).await?;
    let sessions = reply
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    Ok(Json(
        sessions
            .iter()
            .map(|t| {
                json!({
                    "id": t.get("id").cloned().unwrap_or_default(),
                    "title": t.get("title").cloned().unwrap_or(json!("")),
                    "created_at": t.get("created").cloned().unwrap_or_default(),
                    "clients": t.get("clients").cloned().unwrap_or(json!(0)),
                    "cols": t.get("cols").cloned().unwrap_or(json!(0)),
                    "rows": t.get("rows").cloned().unwrap_or(json!(0)),
                })
            })
            .collect(),
    ))
}

async fn close_terminal(
    State(app): State<Shared>,
    s: Signed,
    Path((id, session)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let (_, _, _, reply) =
        terminal(&app, &s, &id, json!({ "op": "close", "session": session })).await?;
    let err = reply_str(&reply, "err");
    if !err.is_empty() {
        return Err(ApiError::new(StatusCode::NOT_FOUND, err));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Attaches a socket to a terminal session:
/// ?session=&dir=&cols=&rows=, a new session with no session, in dir if
/// given. The first message is the attach's reply, as text; after it every
/// message is binary, one frame each, its type byte then its payload, both
/// ways: the stream's length prefix is the message's own.
async fn terminal_socket(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
    RawQuery(q): RawQuery,
    parts: Parts,
) -> Result<Response, ApiError> {
    let mut req = json!({ "op": "attach" });
    for (k, v) in url::form_urlencoded::parse(q.unwrap_or_default().as_bytes()) {
        match k.as_ref() {
            "session" | "dir" if !v.is_empty() => req[k.as_ref()] = json!(v),
            "cols" | "rows" => {
                let n = v.parse::<i64>().unwrap_or(0).clamp(0, 1000);
                if n > 0 {
                    req[k.as_ref()] = json!(n);
                }
            }
            _ => {}
        }
    }
    same_origin(&parts)?;
    let (_, conn, rest, reply) = terminal(&app, &s, &id, req).await?;
    let ws = upgrade(&app, parts, &[]).await?;
    Ok(ws.on_upgrade(move |ws| bridge_terminal(ws, conn, rest, reply)))
}

async fn bridge_terminal(mut ws: WebSocket, mut conn: Conn, mut rbuf: BytesMut, reply: Value) {
    if ws
        .send(Message::Text(reply.to_string().into()))
        .await
        .is_err()
        || !reply_str(&reply, "err").is_empty()
    {
        close(&mut ws, 1000, "").await;
        return;
    }
    let mut keep = KeepAlive::new();
    loop {
        // Frames already read go first.
        while rbuf.len() >= 5 {
            let typ = rbuf[0];
            let n = u32::from_be_bytes([rbuf[1], rbuf[2], rbuf[3], rbuf[4]]) as usize;
            if n > MAX_TERMINAL_FRAME {
                return;
            }
            if rbuf.len() < 5 + n {
                break;
            }
            rbuf.advance(5);
            let mut msg = Vec::with_capacity(n + 1);
            msg.push(typ);
            msg.extend_from_slice(&rbuf.split_to(n));
            if ws.send(Message::Binary(msg.into())).await.is_err() {
                return;
            }
            if typ == b'x' {
                close(&mut ws, 1000, "the shell exited").await;
                return;
            }
        }
        tokio::select! {
            r = conn.read_buf(&mut rbuf) => {
                if !matches!(r, Ok(n) if n > 0) {
                    return;
                }
            }
            msg = ws.recv() => match heard(&mut ws, msg, &mut keep, true).await {
                Said::Binary(m) if !m.is_empty() && matches!(m[0], b'i' | b'r' | b'p') => {
                    let payload = &m[1..];
                    if payload.len() > MAX_TERMINAL_FRAME {
                        return;
                    }
                    let mut frame = Vec::with_capacity(5 + payload.len());
                    frame.push(m[0]);
                    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
                    frame.extend_from_slice(payload);
                    if conn.write_all(&frame).await.is_err() {
                        return;
                    }
                }
                Said::End => return,
                _ => {}
            },
            alive = keep.due() => {
                if !alive || ws.send(Message::Ping(bytes::Bytes::new())).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// Carries a connection to an environment's Code service over a socket:
/// each binary message one of the service's frames. The client API's
/// ?root= is the folder it starts in, the environment's editor folder when
/// not given.
async fn code_socket(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
    RawQuery(q): RawQuery,
    parts: Parts,
) -> Result<Response, ApiError> {
    same_origin(&parts)?;
    let (v, worker) = running(&app, &s, &id).await?;
    let asked = if client_api(&parts) {
        url::form_urlencoded::parse(q.unwrap_or_default().as_bytes())
            .find(|(k, _)| k == "root")
            .map(|(_, v)| v.into_owned())
            .filter(|r| !r.is_empty())
    } else {
        None
    };
    let root = asked.unwrap_or_else(|| v.spec.editor_path.clone());
    envs::record_access(
        &app.store,
        &s.principal.actor(&s.ip),
        &v,
        "environment.open_code",
        json!({}),
    )
    .await;
    let mut conn = open(&app, worker, tunnel::KIND_CODE, &v.row.id.to_string()).await?;
    let req = if root.is_empty() {
        json!({})
    } else {
        json!({ "root": root })
    };
    send_line(&mut conn, &req)
        .await
        .map_err(|_| unreachable_error())?;
    let ws = upgrade(&app, parts, &[]).await?;
    Ok(ws.on_upgrade(move |ws| bridge_code(ws, conn)))
}

async fn bridge_code(mut ws: WebSocket, mut conn: Conn) {
    let mut keep = KeepAlive::new();
    let mut rbuf = BytesMut::new();
    let mut received = false;
    'relay: loop {
        while rbuf.len() >= 4 {
            let n = u32::from_be_bytes([rbuf[0], rbuf[1], rbuf[2], rbuf[3]]) as usize;
            if n > MAX_CODE_FRAME {
                break 'relay;
            }
            if rbuf.len() < 4 + n {
                rbuf.reserve(4 + n - rbuf.len());
                break;
            }
            rbuf.advance(4);
            received = true;
            if ws
                .send(Message::Binary(rbuf.split_to(n).freeze()))
                .await
                .is_err()
            {
                break 'relay;
            }
        }
        tokio::select! {
            r = conn.read_buf(&mut rbuf) => {
                if !matches!(r, Ok(n) if n > 0) {
                    break 'relay;
                }
            }
            msg = ws.recv() => match heard(&mut ws, msg, &mut keep, true).await {
                Said::Binary(m) => {
                    if m.len() > MAX_CODE_FRAME {
                        break 'relay;
                    }
                    let mut frame = Vec::with_capacity(4 + m.len());
                    frame.extend_from_slice(&(m.len() as u32).to_be_bytes());
                    frame.extend_from_slice(&m);
                    if conn.write_all(&frame).await.is_err() {
                        break 'relay;
                    }
                }
                Said::End => break 'relay,
                Said::Other => {}
            },
            alive = keep.due() => {
                if !alive || ws.send(Message::Ping(bytes::Bytes::new())).await.is_err() {
                    break 'relay;
                }
            }
        }
    }
    if received {
        close(&mut ws, 1000, "").await;
    } else {
        close(
            &mut ws,
            1013,
            "the environment's Code service is not available",
        )
        .await;
    }
}

/// An environment the asker may reach whose desktop can be served, and its
/// worker.
async fn desktop_target(app: &Shared, s: &Signed, id: &str) -> Result<(View, Uuid), ApiError> {
    let (v, worker) = running(app, s, id).await?;
    if v.spec.display == hangar_api::Display::None {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "the environment is headless: it has no desktop",
        ));
    }
    Ok((v, worker))
}

/// Carries a VNC viewer's connection to an environment's desktop: every
/// binary message RFB bytes as they are, both ways. A connection the
/// desktop ends before sending anything had no desktop to serve.
async fn desktop_socket(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
    parts: Parts,
) -> Result<Response, ApiError> {
    same_origin(&parts)?;
    let web_ui = !client_api(&parts);
    let (v, worker) = desktop_target(&app, &s, &id).await?;
    envs::record_access(
        &app.store,
        &s.principal.actor(&s.ip),
        &v,
        "environment.open_desktop",
        json!({}),
    )
    .await;
    let mut conn = open(&app, worker, tunnel::KIND_DESKTOP, &v.row.id.to_string()).await?;
    send_line(&mut conn, &json!({ "op": "vnc" }))
        .await
        .map_err(|_| unreachable_error())?;
    // noVNC may offer the "binary" subprotocol.
    let ws = upgrade(&app, parts, if web_ui { &["binary"] } else { &[] }).await?;
    Ok(ws.on_upgrade(move |ws| bridge_desktop(ws, conn, web_ui)))
}

async fn bridge_desktop(mut ws: WebSocket, mut conn: Conn, answer_pings: bool) {
    let mut keep = KeepAlive::new();
    let mut buf = vec![0u8; 64 << 10];
    let mut sent = 0usize;
    loop {
        tokio::select! {
            r = conn.read(&mut buf) => match r {
                Ok(n) if n > 0 => {
                    sent += n;
                    if ws.send(Message::Binary(bytes::Bytes::copy_from_slice(&buf[..n]))).await.is_err() {
                        break;
                    }
                }
                _ => break,
            },
            msg = ws.recv() => match heard(&mut ws, msg, &mut keep, answer_pings).await {
                Said::Binary(m) => {
                    if conn.write_all(&m).await.is_err() {
                        break;
                    }
                }
                Said::End => break,
                Said::Other => {}
            },
            alive = keep.due(), if answer_pings => {
                if !alive || ws.send(Message::Ping(bytes::Bytes::new())).await.is_err() {
                    break;
                }
            }
        }
    }
    if sent == 0 {
        close(&mut ws, 1013, "the environment's desktop is not running").await;
    } else {
        close(&mut ws, 1000, "").await;
    }
}

async fn resize_desktop(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
    Body(b): Body<Value>,
) -> Result<StatusCode, ApiError> {
    let size = |k: &str| {
        b.get(k)
            .and_then(Value::as_i64)
            .ok_or_else(|| ApiError::invalid(format!("{k} is an integer")))
    };
    let (width, height) = (size("width")?, size("height")?);
    let (v, worker) = desktop_target(&app, &s, &id).await?;
    let mut conn = open(&app, worker, tunnel::KIND_DESKTOP, &v.row.id.to_string()).await?;
    let req = json!({ "op": "resize", "width": width, "height": height });
    let (reply, _) = ask(&mut conn, &req, Duration::from_secs(30)).await?;
    let err = reply_str(&reply, "err");
    if !err.is_empty() {
        return Err(ApiError::new(StatusCode::CONFLICT, err));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// Asks a running environment's agent about its processes.
async fn processes(
    app: &Shared,
    s: &Signed,
    id: &str,
    req: Value,
) -> Result<(View, Value), ApiError> {
    let (v, worker) = running(app, s, id).await?;
    let mut conn = open(app, worker, tunnel::KIND_PROCESSES, &v.row.id.to_string()).await?;
    let (reply, _) = ask(&mut conn, &req, Duration::from_secs(15)).await?;
    Ok((v, reply))
}

async fn list_processes(
    State(app): State<Shared>,
    s: Signed,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (_, reply) = processes(&app, &s, &id, json!({ "op": "list" })).await?;
    let err = reply_str(&reply, "error");
    if !err.is_empty() {
        return Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err));
    }
    let list: Vec<Value> = reply
        .get("processes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            let f = |k: &str| p.get(k).cloned().unwrap_or(json!(0));
            let t = |k: &str| p.get(k).cloned().unwrap_or(json!(""));
            let mut out = json!({
                "pid": f("pid"),
                "ppid": f("ppid"),
                "user": t("user"),
                "name": t("name"),
                "command": t("command"),
                "state": t("state"),
                "cpu_percent": f("cpu_percent"),
                "rss_bytes": f("rss_bytes"),
                "threads": f("threads"),
                "started": t("started"),
            });
            if p.get("protected").and_then(Value::as_bool) == Some(true) {
                out["protected"] = json!(true);
            }
            out
        })
        .collect();
    Ok(Json(json!({
        "cpus": reply.get("cpus").cloned().unwrap_or(json!(0)),
        "memory_bytes": reply.get("memory_bytes").cloned().unwrap_or(json!(0)),
        "processes": list,
    })))
}

async fn signal_process(
    State(app): State<Shared>,
    s: Signed,
    Path((id, pid)): Path<(String, i64)>,
    parts: Parts,
    Body(b): Body<Value>,
) -> Result<StatusCode, ApiError> {
    let signal = reply_str(&b, "signal").to_string();
    if signal != "TERM" && signal != "KILL" {
        return Err(ApiError::invalid("the signal is TERM or KILL"));
    }
    let (v, reply) = processes(
        &app,
        &s,
        &id,
        json!({ "op": "kill", "pid": pid, "signal": signal }),
    )
    .await?;
    let err = reply_str(&reply, "error").to_string();
    envs::record_access(
        &app.store,
        &s.principal.actor(&s.ip),
        &v,
        "environment.signal_process",
        json!({ "pid": pid, "signal": signal, "error": err }),
    )
    .await;
    if !err.is_empty() {
        // The web UI's API says such a process cannot be signalled; the
        // client API, that it cannot be signalled as things stand.
        let status = if client_api(&parts) {
            StatusCode::CONFLICT
        } else {
            StatusCode::BAD_REQUEST
        };
        return Err(ApiError::new(status, err));
    }
    Ok(StatusCode::NO_CONTENT)
}

fn offset(q: Option<String>) -> i64 {
    url::form_urlencoded::parse(q.unwrap_or_default().as_bytes())
        .find(|(k, _)| k == "offset")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(-1)
}

/// Asks a worker for part of one of its logs: an environment's, or with
/// none its own.
async fn read_log(
    app: &Shared,
    worker: Uuid,
    env: &str,
    log: &str,
    offset: i64,
) -> Result<Json<Value>, ApiError> {
    let mut conn = open(app, worker, tunnel::KIND_LOGS, env).await?;
    let req = json!({ "environment": env, "log": log, "offset": offset, "max": 0 });
    let (reply, _) = ask(&mut conn, &req, Duration::from_secs(15)).await?;
    let err = reply_str(&reply, "error");
    if !err.is_empty() {
        return Err(ApiError::new(StatusCode::SERVICE_UNAVAILABLE, err));
    }
    use base64::Engine;
    let data = base64::engine::general_purpose::STANDARD
        .decode(reply_str(&reply, "data"))
        .unwrap_or_default();
    let start = reply.get("start").and_then(Value::as_i64).unwrap_or(0);
    let mut out = json!({
        "start": start,
        "next": start + data.len() as i64,
        "size": reply.get("size").and_then(Value::as_i64).unwrap_or(0),
        "text": String::from_utf8_lossy(&data),
    });
    if reply.get("missing").and_then(Value::as_bool) == Some(true) {
        out["missing"] = json!(true);
    }
    Ok(Json(out))
}

async fn environment_log(
    State(app): State<Shared>,
    s: Signed,
    Path((id, log)): Path<(String, String)>,
    RawQuery(q): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let v = envs::get(&app.store, &s.principal, env_id(&id)?)
        .await?
        .ok_or_else(|| ApiError::not_found("environment"))?;
    let Some(worker) = v.row.worker_id else {
        return Err(ApiError::new(StatusCode::CONFLICT, NOT_PLACED).code(code::NOT_PLACED));
    };
    read_log(&app, worker, &v.row.id.to_string(), &log, offset(q)).await
}

async fn worker_log(
    State(app): State<Shared>,
    Path(id): Path<String>,
    RawQuery(q): RawQuery,
) -> Result<Json<Value>, ApiError> {
    let id = Uuid::parse_str(&id).map_err(|_| ApiError::not_found("worker"))?;
    if crate::workers::get(&app.store, id).await?.is_none() {
        return Err(ApiError::not_found("worker"));
    }
    read_log(&app, id, "", "worker", offset(q)).await
}
