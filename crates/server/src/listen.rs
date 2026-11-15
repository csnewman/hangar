//! Listening on an address as configured: `:port` is every interface's,
//! IPv6 and IPv4 alike, or IPv4's alone on a machine without IPv6.

use std::io;

/// Every interface's address at port, as configured by `:port`.
pub const ANY: &str = "[::]";

/// The IPv4 address of `[::]:port`, for a machine that cannot bind IPv6.
fn v4(addr: &str) -> Option<String> {
    addr.strip_prefix("[::]:")
        .map(|port| format!("0.0.0.0:{port}"))
}

pub async fn tcp(addr: &str) -> io::Result<tokio::net::TcpListener> {
    match tokio::net::TcpListener::bind(addr).await {
        Err(e) if v4(addr).is_some() && e.kind() != io::ErrorKind::AddrInUse => {
            tokio::net::TcpListener::bind(v4(addr).unwrap_or_default()).await
        }
        r => r,
    }
}

pub async fn udp(addr: &str) -> io::Result<tokio::net::UdpSocket> {
    match tokio::net::UdpSocket::bind(addr).await {
        Err(e) if v4(addr).is_some() && e.kind() != io::ErrorKind::AddrInUse => {
            tokio::net::UdpSocket::bind(v4(addr).unwrap_or_default()).await
        }
        r => r,
    }
}
