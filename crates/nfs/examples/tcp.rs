//! Serves a directory of file sets over TCP, for a Linux NFS client to
//! mount where there is no vsock: a container's kernel, say, running
//! pjdfstest against the server.
//!
//!     tcp ROOT ADDR all|root|none
//!
//! Every directory in ROOT is a set, squashed as the last argument says;
//! squashed callers act as 1000:1000. Run as root, the server acts as its
//! callers. Mount it with
//! `mount -t nfs4 -o vers=4.2,proto=tcp,port=PORT 127.0.0.1:/ DIR`.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;

use hangar_nfs::{Config, Server, Squash, Stream, View};

struct Dir {
    root: PathBuf,
    squash: Squash,
}

impl View for Dir {
    fn sets(&self) -> Vec<String> {
        std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|e| e.file_name().into_string().ok())
            .collect()
    }

    fn hidden(&self, _: &str, _: &[u8]) -> bool {
        false
    }

    fn squash(&self, _: &str) -> Squash {
        self.squash
    }
}

struct Tcp(TcpStream);

impl Read for Tcp {
    fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
        self.0.read(b)
    }
}

impl Write for Tcp {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.0.write(b)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Stream for Tcp {
    fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Tcp)
    }

    fn shutdown(&self) {
        let _ = self.0.shutdown(Shutdown::Both);
    }
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [root, addr, squash] = args.as_slice() else {
        eprintln!("usage: tcp ROOT ADDR all|root|none");
        std::process::exit(2);
    };
    let squash = match squash.as_str() {
        "all" => Squash::All,
        "root" => Squash::Root,
        "none" => Squash::None,
        s => {
            eprintln!("tcp: {s}: all, root or none");
            std::process::exit(2);
        }
    };
    let root = PathBuf::from(root);
    let view = Arc::new(Dir {
        root: root.clone(),
        squash,
    });
    let server = Server::new(Config {
        root: &root,
        view,
        uid: 1000,
        gid: 1000,
    })?;
    let ln = TcpListener::bind(addr)?;
    for conn in ln.incoming() {
        let (conn, server) = (conn?, server.clone());
        std::thread::spawn(move || {
            if let Err(e) = server.serve(Tcp(conn)) {
                eprintln!("tcp: {e}");
            }
        });
    }
    Ok(())
}
