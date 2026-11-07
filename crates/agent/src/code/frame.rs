//! A connection carries frames: a four-byte big-endian length, then that
//! many bytes. The first byte of a frame is its channel.

use tokio::io::{AsyncRead, AsyncReadExt};

/// JSON requests, responses and events.
pub const JSON: u8 = b'r';

/// The largest frame read; a larger one ends the connection.
const MAX_FRAME: usize = 64 << 20;

/// The next frame, None once the connection ends or sends a frame too large.
pub async fn read<R: AsyncRead + Unpin>(r: &mut R) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).await.ok()?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME {
        return None;
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await.ok()?;
    Some(buf)
}

/// A frame of `channel`, its payload `parts` one after another, with its
/// length in front.
pub fn encode(channel: u8, parts: &[&[u8]]) -> Vec<u8> {
    let len = 1 + parts.iter().map(|p| p.len()).sum::<usize>();
    let mut out = Vec::with_capacity(4 + len);
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.push(channel);
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}
