//! The control channel, as the host speaks it, and the pieces of init that
//! are plain arithmetic.

use std::time::{Duration, Instant};

use hangar_agent::control::serve;
use hangar_agent::init::{machine_id, swap_header, valid_machine_id};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread")]
async fn hello_then_requests() {
    let (host, guest) = tokio::io::duplex(1 << 20);
    tokio::spawn(serve(guest));
    let (rd, mut wr) = tokio::io::split(host);
    let mut lines = BufReader::new(rd).lines();
    let mut next = async || -> Value {
        let l = timeout(WAIT, lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str(&l).unwrap()
    };
    let hello = next().await;
    assert_eq!(
        (hello["kind"].as_str(), hello["version"].as_i64()),
        (Some("hello"), Some(1))
    );
    assert!(hello["page_size"].as_i64().unwrap() >= 4096);

    let send = |v: Value| format!("{v}\n");
    let reqs = [
        json!({"id": 1, "kind": "exec", "cmd": ["sh", "-c", "sleep 1; echo slow"]}),
        json!({"id": 2, "kind": "ping"}),
        json!({"id": 3, "kind": "exec", "cmd": ["sh", "-c", "echo out; echo err >&2; exit 4"]}),
        json!({"id": 4, "kind": "exec", "cmd": []}),
        json!({"id": 5, "kind": "dance"}),
        json!({"id": 6, "kind": "exec", "cmd": ["no-such-command-here"]}),
        // What it leaves running holds its output; the answer comes anyway.
        json!({"id": 7, "kind": "exec", "cmd": ["sh", "-c", "echo started; sleep 30 &"]}),
        json!({"id": 8, "kind": "info"}),
    ];
    let start = Instant::now();
    for r in reqs {
        wr.write_all(send(r).as_bytes()).await.unwrap();
    }
    wr.write_all(b"not json\n").await.unwrap();
    let mut got = std::collections::HashMap::new();
    let mut order = vec![];
    while got.len() < 8 {
        let r = next().await;
        let id = r["id"].as_u64().unwrap();
        order.push(id);
        got.insert(id, r);
    }
    // The ping is not held up behind the slow command.
    let pos = |id| order.iter().position(|&i| i == id).unwrap();
    assert!(pos(2) < pos(1), "answered in order {order:?}");
    assert_eq!(got[&1]["stdout"], "slow\n");
    assert_eq!(got[&2], json!({"id": 2, "code": 0}));
    assert_eq!(
        (got[&3]["stdout"].as_str(), got[&3]["stderr"].as_str()),
        (Some("out\n"), Some("err\n"))
    );
    assert_eq!(got[&3]["code"], 4);
    assert_eq!(got[&4]["err"], "exec with no command");
    assert!(got[&5]["err"].as_str().unwrap().contains("dance"));
    assert!(
        !got[&6]["err"].as_str().unwrap().is_empty(),
        "{:?}",
        got[&6]
    );
    assert_eq!(
        (got[&7]["stdout"].as_str(), got[&7]["code"].as_i64()),
        (Some("started\n"), Some(0))
    );
    assert!(got[&8]["info"].as_str().unwrap().contains(" up "));
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "took {:?}",
        start.elapsed()
    );
}

#[test]
fn swap_headers() {
    let h = swap_header(64 << 20, 4096, [7; 16]).unwrap();
    assert_eq!(h.len(), 4096);
    assert_eq!(&h[4086..], b"SWAPSPACE2");
    assert_eq!(
        u32::from_ne_bytes(h[1024..1028].try_into().unwrap()),
        1,
        "version"
    );
    assert_eq!(
        u32::from_ne_bytes(h[1028..1032].try_into().unwrap()),
        (64 << 20) / 4096 - 1,
        "last page"
    );
    assert_eq!(&h[1036..1052], &[7; 16], "uuid");
    assert_eq!(&h[1052..1063], b"hangar-swap");
    let h = swap_header(1 << 30, 16384, [0; 16]).unwrap();
    assert_eq!(&h[16374..], b"SWAPSPACE2", "a 16 KiB page");
    assert!(swap_header(4096, 4096, [0; 16]).is_none(), "one page");
}

#[test]
fn machine_ids() {
    let id = machine_id([0xff; 16]);
    assert!(valid_machine_id(&id), "{id}");
    assert_eq!(&id[12..13], "4", "version 4");
    assert_eq!(&id[16..17], "b", "the variant");
    assert!(!valid_machine_id("uninitialized"));
    assert!(!valid_machine_id(&id.to_uppercase()));
}
