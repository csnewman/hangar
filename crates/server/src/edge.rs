//! What faces the internet directly, each part only when configured: the
//! zone's DNS server, HTTPS, and a redirect from HTTP to it.
//!
//! The redirect serves an environment's own hosts rather than redirecting
//! them, since plain HTTP to one of them is for the environment's port 80.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{ConnectInfo, Request};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rustls::server::ResolvesServerCert;
use tokio::sync::mpsc;

use crate::config::{Certs, Tls};
use crate::http::Shared;
use crate::publichost::Public;

/// How long a TLS handshake may take.
const HANDSHAKE_WITHIN: Duration = Duration::from_secs(10);

/// Starts each configured part, saying on failed what stops one.
pub async fn start(
    app: &Shared,
    router: &Router,
    failed: mpsc::Sender<String>,
) -> Result<(), String> {
    let edge = &app.cfg.edge;
    let Some(public) = app.cfg.public.clone() else {
        return Ok(());
    };
    if edge.dns.is_none() && edge.tls.is_none() && edge.redirect.is_none() {
        return Ok(());
    }
    if let Some(dns) = &edge.dns {
        let zone = Arc::new(crate::zone::Zone::new(
            public.hostname(),
            dns.clone(),
            app.store.clone(),
        )?);
        let failed = failed.clone();
        tokio::spawn(async move {
            if let Err(e) = zone.serve().await {
                let _ = failed.send(format!("serving DNS: {e}")).await;
            }
        });
    }
    tokio::spawn(crate::zone::prune(app.store.clone()));

    if let Some(tls) = &edge.tls {
        let resolver = resolver(app, &public, tls)?;
        let listener = crate::listen::tcp(&tls.listen)
            .await
            .map_err(|e| format!("listening on {}: {e}", tls.listen))?;
        eprintln!("hangar-server: serving HTTPS on {}", tls.listen);
        let acceptor =
            tokio_rustls::TlsAcceptor::from(Arc::new(crate::certs::server_config(resolver)));
        let router = router.clone();
        let failed = failed.clone();
        tokio::spawn(async move {
            let e = serve_tls(listener, acceptor, router).await;
            let _ = failed.send(format!("serving HTTPS: {e}")).await;
        });
    }

    if let Some(listen) = &edge.redirect {
        let listener = crate::listen::tcp(listen)
            .await
            .map_err(|e| format!("listening on {listen}: {e}"))?;
        eprintln!("hangar-server: redirecting HTTP to HTTPS on {listen}");
        let redirecting = router
            .clone()
            .layer(axum::middleware::from_fn_with_state(public, redirect));
        tokio::spawn(async move {
            let served = axum::serve(
                listener,
                redirecting.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await;
            let e = served.err().map(|e| e.to_string()).unwrap_or_default();
            let _ = failed.send(format!("redirecting HTTP: {e}")).await;
        });
    }
    Ok(())
}

fn resolver(
    app: &Shared,
    public: &Public,
    tls: &Tls,
) -> Result<Arc<dyn ResolvesServerCert>, String> {
    match &tls.certs {
        Certs::Files { cert, key } => {
            let files = crate::certs::Files::load(cert, key)?;
            let mut hosts = vec![
                public.host.clone(),
                public.name("env000000"),
                public.name("app-env000000"),
            ];
            hosts.extend(crate::registry::host(app));
            files.warn_uncovered(&hosts);
            Ok(Arc::new(files))
        }
        Certs::Acme { email, ca, roots } => {
            let acme = Arc::new(crate::certs::Acme::new(
                public.hostname(),
                email.clone(),
                ca.clone(),
                roots.clone(),
                app.store.clone(),
            ));
            tokio::spawn(acme.clone().run());
            Ok(acme)
        }
    }
}

/// Serves HTTPS on listener: HTTP/2 or HTTP/1.1 as the client's ALPN asks,
/// each request marked as having come over HTTPS.
async fn serve_tls(
    listener: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    router: Router,
) -> String {
    loop {
        let (tcp, peer) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("hangar-server: accepting an HTTPS connection: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let (acceptor, router) = (acceptor.clone(), router.clone());
        tokio::spawn(async move {
            let Ok(Ok(tls)) = tokio::time::timeout(HANDSHAKE_WITHIN, acceptor.accept(tcp)).await
            else {
                return;
            };
            let svc = hyper::service::service_fn(move |mut req: Request<hyper::body::Incoming>| {
                req.extensions_mut().insert(ConnectInfo(peer));
                req.headers_mut()
                    .insert("x-forwarded-proto", HeaderValue::from_static("https"));
                // HTTP/2 names the host in the request's authority, not a
                // Host header, which is what everything else reads.
                if !req.headers().contains_key(header::HOST)
                    && let Some(v) = req
                        .uri()
                        .authority()
                        .and_then(|a| HeaderValue::from_str(a.as_str()).ok())
                {
                    req.headers_mut().insert(header::HOST, v);
                }
                let mut r = router.clone();
                tower_service::Service::call(&mut r, req.map(axum::body::Body::new))
            });
            let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection_with_upgrades(TokioIo::new(tls), svc)
                .await;
        });
    }
}

/// Sends a request to Hangar's HTTPS origin, on the host it asked for,
/// unless it is for one of an environment's hosts.
async fn redirect(
    axum::extract::State(public): axum::extract::State<Public>,
    req: Request,
    next: axum::middleware::Next,
) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if crate::ports::owns(&public, &host).is_some() {
        return next.run(req).await;
    }
    let mut name = crate::publichost::hostname(&host).to_string();
    if name.is_empty() {
        name = public.hostname().to_string();
    }
    if name.contains(':') {
        name = format!("[{name}]");
    }
    let port = match public.host.strip_prefix('[') {
        Some(rest) => rest.split_once("]:").map(|(_, p)| p),
        None => public.host.rsplit_once(':').map(|(_, p)| p),
    };
    let authority = match port {
        Some(p) => format!("{name}:{p}"),
        None => name,
    };
    let path = req
        .uri()
        .path_and_query()
        .map(|p| p.as_str())
        .unwrap_or("/");
    let to = format!("{}://{authority}{path}", public.scheme);
    match HeaderValue::from_str(&to) {
        Ok(v) => (StatusCode::PERMANENT_REDIRECT, [(header::LOCATION, v)]).into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}
