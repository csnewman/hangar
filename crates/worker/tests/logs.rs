use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use hangar_worker::logs::{
    self, COMBINED_SIZE, Combiner, ENVIRONMENT_SIZE, Reply, Request, Rings, read_file,
};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hangar-worker-logs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn req(env: &str, offset: i64, max: i64) -> Request {
    Request {
        environment: env.into(),
        log: String::new(),
        offset,
        max,
    }
}

fn text(r: &Reply) -> String {
    String::from_utf8_lossy(&r.data).into_owned()
}

#[test]
fn files() {
    let path = scratch("file").join("console.log");
    let r = read_file(&path, &req("", 0, 0)).unwrap();
    assert!(r.missing, "a log not yet written: {r:?}");
    std::fs::write(&path, "one\ntwo\nthree\n").unwrap();

    // The end, and on from where a reader got to.
    let r = read_file(&path, &req("", -1, 6)).unwrap();
    assert_eq!((text(&r).as_str(), r.start, r.size), ("three\n", 8, 14));
    OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"four\n")
        .unwrap();
    let r = read_file(&path, &req("", r.start + r.data.len() as i64, 0)).unwrap();
    assert_eq!(text(&r), "four\n");
    // A reader past the end -- the machine started again with a new log --
    // is given its end.
    let r = read_file(&path, &req("", 1000, 5)).unwrap();
    assert_eq!(text(&r), "four\n");
}

/// A reply is one line of JSON as the Go server reads it: the data in
/// base64, missing and error only when they are so.
#[test]
fn replies() {
    let r = Reply {
        start: 1,
        size: 4,
        data: b"abc".to_vec(),
        ..Reply::default()
    };
    assert_eq!(
        serde_json::to_string(&r).unwrap(),
        r#"{"start":1,"size":4,"data":"YWJj"}"#
    );
    let back: Reply =
        serde_json::from_str(r#"{"start":0,"size":0,"data":null,"missing":true}"#).unwrap();
    assert!(back.missing && back.data.is_empty());
    let q: Request =
        serde_json::from_str(r#"{"environment":"e1","log":"console","offset":-1,"max":100}"#)
            .unwrap();
    assert_eq!(
        (q.environment.as_str(), q.log.as_str(), q.offset, q.max),
        ("e1", "console", -1, 100)
    );
}

/// The worker's lines are kept whole and per environment: a line's
/// environment is its environment= field.
#[test]
fn rings() {
    let rings = Rings::new();
    let put = |line: &str| {
        let l = format!("time=x level=INFO {line}\n");
        rings.write(logs::environment(&l), l.as_bytes());
        l
    };
    let whole = [
        put("msg=\"worker ready\" name=w1"),
        put("msg=booting environment=e1"),
        put("msg=\"opening a stream\" environment=e2 err=gone"),
    ]
    .concat();

    assert_eq!(text(&rings.read(&req("", 0, 0))), whole);
    let e1 = text(&rings.read(&req("e1", 0, 0)));
    assert!(e1.contains("booting") && !e1.contains("worker ready") && !e1.contains("opening"));
    assert!(text(&rings.read(&req("e2", 0, 0))).contains("opening a stream"));
    assert!(rings.read(&req("e3", 0, 0)).missing);

    // The oldest go once a ring is full, whole lines at a time.
    for _ in 0..ENVIRONMENT_SIZE / 10 {
        put("msg=filling environment=e1");
    }
    let r = rings.read(&req("e1", 0, 0));
    assert!(r.start > 0 && r.data.starts_with(b"time=") && r.size - r.start == r.data.len() as i64);

    rings.forget("e1");
    assert!(rings.read(&req("e1", 0, 0)).missing);
    let mut envs = rings.environments();
    envs.sort();
    assert_eq!(envs, ["e2"]);
}

/// An environment's logs combine as the worker sees them written, each
/// line marked with its log: what the files held before it was followed is
/// left out, a line waits for its end, and a log that starts over, or is
/// put in another file's place, is read again from its start.
#[test]
fn combined() {
    let dir = scratch("combined");
    let rings = Arc::new(Rings::new());
    let worker = |line: &str| {
        rings.write(
            "e1",
            format!("time=x level=INFO msg={line} environment=e1\n").as_bytes(),
        )
    };
    let file = {
        let dir = dir.clone();
        move |name: &str| dir.join(format!("{name}.log"))
    };
    let write = |name: &str, text: &str, truncate: bool| {
        let mut o = OpenOptions::new();
        o.create(true).write(true);
        if truncate {
            o.truncate(true);
        } else {
            o.append(true);
        }
        o.open(file(name))
            .unwrap()
            .write_all(text.as_bytes())
            .unwrap();
    };
    let files = {
        let file = file.clone();
        Arc::new(move |env: &str, name: &str| (env == "e1").then(|| file(name)))
    };
    let c = Combiner::new(rings.clone(), files);
    let read = || {
        text(&c.read_combined(&Request {
            environment: "e1".into(),
            log: logs::COMBINED.into(),
            offset: 0,
            max: COMBINED_SIZE as i64,
        }))
    };

    assert!(c.read_combined(&req("e1", 0, 0)).missing);
    write(logs::CONSOLE, "an earlier boot\n", false);
    worker("starting");
    c.poll();
    write(logs::CONSOLE, "Linux version\r\nlogin: ", false);
    write(logs::MONITOR, "monitor up\n", false);
    c.poll();
    worker("started");
    write(logs::CONSOLE, "dev\n", false);
    c.poll();

    let got = read();
    for want in [
        "worker  │ time=",
        "msg=starting",
        "console │ Linux version\n",
        "monitor │ monitor up\n",
        "msg=started",
        "console │ login: dev\n",
    ] {
        assert!(got.contains(want), "no {want:?} in:\n{got}");
    }
    assert!(!got.contains("an earlier boot"), "{got}");
    assert!(got.find("Linux version") < got.find("msg=started"), "{got}");

    // The machine boots again, its console starting over.
    write(logs::CONSOLE, "booting again\n", true);
    c.poll();
    assert!(read().ends_with("console │ booting again\n"), "{}", read());

    // And again, its monitor's log a new file already longer than the last.
    std::fs::remove_file(file(logs::MONITOR)).unwrap();
    write(logs::MONITOR, "monitor up again, with more to say\n", false);
    c.poll();
    assert!(
        read().ends_with("monitor │ monitor up again, with more to say\n"),
        "{}",
        read()
    );

    // Forgotten, its combined log goes too.
    rings.forget("e1");
    c.poll();
    assert!(c.read_combined(&req("e1", 0, 0)).missing);
}

#[test]
fn environments_of_lines() {
    assert_eq!(
        logs::environment("time=x level=INFO msg=a environment=e1 err=x"),
        "e1"
    );
    assert_eq!(
        logs::environment("time=x level=INFO msg=a environment=e1"),
        "e1"
    );
    assert_eq!(
        logs::environment("time=x level=INFO msg=\"no environment\""),
        ""
    );
}
