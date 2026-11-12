//! Terminal sessions, through the manager's connections as the worker makes
//! them, with a shell that says hello and echoes what it is sent, so nothing
//! depends on anyone's prompt.

use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use hangar_agent::terminal::{
    self, FRAME_EXIT, FRAME_INPUT, FRAME_OUTPUT, FRAME_PREFER, FRAME_RESIZE, FRAME_SIZE, Manager,
    Reply, Request,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(10);

fn manager() -> Arc<Manager> {
    Manager::new(Box::new(|_: &Request| {
        let mut c = Command::new("/bin/sh");
        c.arg("-c")
            .arg(r#"echo hello; while read l; do echo "got:$l"; done"#);
        Ok(c)
    }))
}

/// One connection to a manager.
struct Conn {
    rd: BufReader<ReadHalf<DuplexStream>>,
    wr: WriteHalf<DuplexStream>,
    out: String,
}

async fn dial(m: &Arc<Manager>, req: Request) -> (Conn, Reply) {
    let (a, b) = tokio::io::duplex(1 << 20);
    let m = m.clone();
    tokio::spawn(async move { m.serve(b).await });
    let (rd, mut wr) = tokio::io::split(a);
    let mut rd = BufReader::new(rd);
    let mut line = serde_json::to_vec(&req).unwrap();
    line.push(b'\n');
    wr.write_all(&line).await.unwrap();
    let mut reply = String::new();
    timeout(WAIT, rd.read_line(&mut reply))
        .await
        .expect("a reply")
        .unwrap();
    let reply = serde_json::from_str(&reply).unwrap();
    (
        Conn {
            rd,
            wr,
            out: String::new(),
        },
        reply,
    )
}

fn attach(session: &str, cols: u16, rows: u16) -> Request {
    Request {
        op: "attach".into(),
        session: session.into(),
        cols,
        rows,
        ..Request::default()
    }
}

fn op(op: &str, session: &str) -> Request {
    Request {
        op: op.into(),
        session: session.into(),
        ..Request::default()
    }
}

impl Conn {
    /// Reads output until what was seen contains `want`, saying whether the
    /// shell exited first.
    async fn until(&mut self, want: &str) -> bool {
        while !self.out.contains(want) {
            let frame = timeout(WAIT, terminal::read_frame(&mut self.rd)).await;
            let (typ, p) = match frame {
                Ok(Ok(f)) => f,
                other => panic!("waiting for {want:?}, saw {:?}: {other:?}", self.out),
            };
            match typ {
                FRAME_OUTPUT => self.out.push_str(&String::from_utf8_lossy(&p)),
                FRAME_EXIT => return true,
                _ => {}
            }
        }
        false
    }

    /// Reads frames until a size, keeping any output.
    async fn size(&mut self) -> (u16, u16, usize, bool) {
        loop {
            let frame = timeout(WAIT, terminal::read_frame(&mut self.rd)).await;
            let (typ, p) = match frame {
                Ok(Ok(f)) => f,
                other => panic!("waiting for a size: {other:?}"),
            };
            match typ {
                FRAME_OUTPUT => self.out.push_str(&String::from_utf8_lossy(&p)),
                FRAME_SIZE => return terminal::parse_size(&p).unwrap(),
                _ => {}
            }
        }
    }

    async fn sized(&mut self, typ: u8, cols: u16, rows: u16) {
        terminal::write_frame(&mut self.wr, typ, &terminal::resize_payload(cols, rows))
            .await
            .unwrap();
    }

    async fn send(&mut self, s: &str) {
        terminal::write_frame(&mut self.wr, FRAME_INPUT, s.as_bytes())
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn sessions_outlive_connections() {
    let m = manager();

    let (mut first, r) = dial(&m, attach("", 100, 30)).await;
    assert!(r.err.is_empty(), "attach: {r:?}");
    let id = r.session.expect("a session").id;
    first.until("hello").await;
    first.send("one\n").await;
    first.until("got:one").await;

    // A second connection -- another tab -- gets the history, then the same
    // live output as the first, and is told how much of what follows is
    // the replay.
    let (mut second, r) = dial(&m, attach(&id, 0, 0)).await;
    assert_eq!(r.session.as_ref().map(|s| s.clients), Some(2), "{r:?}");
    assert_eq!(r.history, "hello\r\none\r\ngot:one\r\n".len());
    second.until("got:one").await;
    second.send("two\n").await;
    first.until("got:two").await;
    second.until("got:two").await;

    // The first goes away -- a refresh -- and the session carries on.
    drop(first);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_, r) = dial(&m, op("list", "")).await;
    assert_eq!(r.sessions.len(), 1, "{r:?}");
    assert_eq!(
        (r.sessions[0].id.as_str(), r.sessions[0].clients),
        (id.as_str(), 1)
    );

    let (mut third, r) = dial(&m, attach(&id, 0, 0)).await;
    assert!(r.err.is_empty(), "attaching again: {r:?}");
    third.until("got:two").await;

    // Closing hangs up on the shell, and every connection is told it ended.
    let (_, r) = dial(&m, op("close", &id)).await;
    assert!(r.err.is_empty(), "close: {r:?}");
    assert!(
        second.until("never printed").await,
        "no exit frame after close"
    );

    let (_, r) = dial(&m, attach(&id, 0, 0)).await;
    assert_eq!(r.err, "no such session");
    let (_, r) = dial(&m, op("close", "nope")).await;
    assert_eq!(r.err, "no such session");
}

/// The size is the window of whoever is using the terminal: the first to
/// attach, then whoever types or asks for it. Another window opening, or
/// changing, takes nothing from them; it is told the size to draw at.
#[tokio::test]
async fn the_size_is_whoever_is_using_it() {
    let m = manager();

    let (mut a, r) = dial(&m, attach("", 100, 30)).await;
    let s = r.session.clone().expect("a session");
    assert!(
        r.owner && (s.cols, s.rows) == (100, 30),
        "first attach: {r:?}"
    );
    assert_eq!(a.size().await, (100, 30, 1, true));

    let (mut b, r) = dial(&m, attach(&s.id, 60, 20)).await;
    let bs = r.session.clone().unwrap();
    assert!(
        !r.owner && (bs.cols, bs.rows) == (100, 30),
        "a second window took the size: {r:?}"
    );
    assert_eq!(b.size().await, (100, 30, 2, false));
    assert_eq!(a.size().await, (100, 30, 2, true));

    // The other window changing changes nothing; the holder's does.
    b.sized(FRAME_PREFER, 70, 25).await;
    a.sized(FRAME_PREFER, 110, 35).await;
    assert_eq!(a.size().await, (110, 35, 2, true));
    assert_eq!(b.size().await, (110, 35, 2, false));

    // Input takes nothing: every window's terminal answers queries of
    // itself. Asking takes the size, as a window does when typed into.
    b.send("x\n").await;
    b.until("got:x").await;
    b.sized(FRAME_RESIZE, 70, 25).await;
    assert_eq!(b.size().await, (70, 25, 2, true));
    assert_eq!(a.size().await, (70, 25, 2, false));

    // Asking for it takes it back.
    a.sized(FRAME_RESIZE, 120, 40).await;
    assert_eq!(a.size().await, (120, 40, 2, true));
    assert_eq!(b.size().await, (120, 40, 2, false));

    // The holder leaving leaves the size as it was, held by nobody.
    drop(a);
    assert_eq!(b.size().await, (120, 40, 1, false));
    let (_, r) = dial(&m, op("list", "")).await;
    assert_eq!(
        (r.sessions[0].cols, r.sessions[0].rows),
        (120, 40),
        "the terminal's size: {r:?}"
    );

    dial(&m, op("close", &s.id)).await;
}

#[tokio::test]
async fn frames() {
    let mut b = Vec::new();
    terminal::write_frame(&mut b, FRAME_RESIZE, &terminal::resize_payload(132, 43))
        .await
        .unwrap();
    let (typ, p) = terminal::read_frame(&mut b.as_slice()).await.unwrap();
    assert_eq!(typ, FRAME_RESIZE);
    assert_eq!(terminal::parse_resize(&p), Some((132, 43)));
    assert_eq!(
        terminal::parse_size(&terminal::size_payload(80, 24, 3, true)),
        Some((80, 24, 3, true))
    );
    let big = vec![0; terminal::MAX_FRAME + 1];
    assert!(
        terminal::write_frame(&mut Vec::new(), FRAME_INPUT, &big)
            .await
            .is_err()
    );
}
