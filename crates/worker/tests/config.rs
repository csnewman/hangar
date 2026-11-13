use hangar_api::Resources;
use hangar_vm::log::Log;
use hangar_worker::config::{Config, DEFAULT_CREDENTIAL_FILE, Size};

#[test]
fn sizes() {
    for (input, want) in [
        ("0", 0u64),
        ("1024", 1024),
        ("4GiB", 4 << 30),
        ("512 MiB", 512 << 20),
        ("1TiB", 1 << 40),
    ] {
        assert_eq!(Size::parse(input), Ok(Size(want)), "{input}");
    }
    for bad in ["", "4GB", "-1", "lots"] {
        assert!(Size::parse(bad).is_err(), "{bad:?} parsed");
    }
    assert_eq!(Size(4 << 30).mib(), 4096);
}

const MINIMAL: &str =
    "server:\n  url: http://hangar.example/\nnode:\n  name: w1\nruntime: simulated\n";

/// The defaults: the credential's place, a swap of one, and the URL
/// without its trailing slash.
#[test]
fn defaults() {
    let c = Config::parse(MINIMAL).unwrap();
    assert_eq!(c.server.url, "http://hangar.example");
    assert_eq!(c.server.credential_file, DEFAULT_CREDENTIAL_FILE);
    assert_eq!(c.vm.swap, Some(1.0));
    assert_eq!(c.node.name, "w1");

    let c = Config::parse("server:\n  url: http://x\nruntime: simulated\n").unwrap();
    assert!(
        !c.node.name.is_empty(),
        "the hostname names an unnamed node"
    );
}

/// A setting the worker does not know is refused, as a misspelt one would
/// otherwise be ignored; so is what the worker cannot run with.
#[test]
fn refusals() {
    for (yaml, why) in [
        (
            "server:\n  url: http://x\n  urll: y\nruntime: simulated\n",
            "urll",
        ),
        ("runtime: simulated\n", "server.url"),
        ("server:\n  url: http://x\n", "runtime"),
        (
            "server:\n  url: http://x\nruntime: simulated\novercommit:\n  memory: 0.5\n",
            "overcommit",
        ),
        (
            "server:\n  url: http://x\nruntime: simulated\nvm:\n  swap: -1\n",
            "vm.swap",
        ),
        ("server:\n  url: http://x\nruntime: krun\n", "vm.kernel"),
        (
            "server:\n  url: http://x\nruntime: krun\nvm:\n  kernel: /k\n",
            "storage.environments",
        ),
        (
            "server:\n  url: http://x\nruntime: simulated\nreserved:\n  memory: 4GB\n",
            "invalid size",
        ),
    ] {
        match Config::parse(yaml) {
            Ok(_) => panic!("accepted:\n{yaml}"),
            Err(e) => assert!(e.contains(why), "{e:?} does not say {why:?}"),
        }
    }
}

/// Every worker configuration in the repository reads.
#[test]
fn the_repository_configs() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for f in [
        "dev/worker-mac.yaml",
        "dev/worker-lima.yaml",
        "dev/worker-lima-krun.yaml",
        "deploy/worker.yaml",
    ] {
        let c = Config::load(&root.join(f)).unwrap_or_else(|e| panic!("{f}: {e}"));
        assert!(!c.server.url.is_empty() && !c.runtime.is_empty(), "{f}");
    }
    let mac = Config::load(&root.join("dev/worker-mac.yaml")).unwrap();
    assert_eq!(
        mac.node.labels.get("runtime").map(String::as_str),
        Some("krun")
    );
    assert_eq!(mac.reserved.memory, Size(8 << 30));
    assert!(mac.vm.gpu.venus && mac.vm.gpu.venus_restore);
}

/// A worker offers its machine, less what is reserved, times its
/// overcommit; a ratio of 0 is 1.
#[test]
fn offer() {
    let total = Resources {
        cpus: 8,
        memory_mib: 16384,
    };
    let cfg = |extra: &str| Config::parse(&format!("{MINIMAL}{extra}")).unwrap();
    let log = Log::default();
    assert_eq!(hangar_worker::offer(&cfg(""), total, 0, &log), total);
    assert_eq!(
        hangar_worker::offer(
            &cfg("reserved:\n  cpus: 2\n  memory: 4GiB\novercommit:\n  cpus: 4\n"),
            total,
            0,
            &log
        ),
        Resources {
            cpus: 24,
            memory_mib: 12288,
        }
    );
    assert_eq!(
        hangar_worker::offer(&cfg("reserved:\n  cpus: 20\n"), total, 0, &log).cpus,
        0
    );
}
