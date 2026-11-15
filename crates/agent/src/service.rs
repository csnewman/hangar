//! What each of the agent's services says of itself in the first answer it
//! gives a connection, so a client built at another time knows what it may
//! ask: the version of the service's protocol, the agent's build, and what
//! the service does beyond what its protocol promises, by name. A client
//! takes a service that says none of this as one at protocol 1 with no
//! features, and asks nothing of it that those do not promise.

use serde::{Deserialize, Serialize};

/// This build, as the agent says it: HANGAR_VERSION when it was compiled,
/// as images are built, else the crate's version.
pub const BUILD: &str = match option_env!("HANGAR_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct Service {
    pub protocol: u32,
    pub build: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

impl Service {
    fn new(protocol: u32, features: &[&str]) -> Service {
        Service {
            protocol,
            build: BUILD.to_string(),
            features: features.iter().map(|f| f.to_string()).collect(),
        }
    }
}

/// The Code service (`code/`), whose hello answers with it. ping: it
/// answers "ping" at once, for the browser to time the way there and back.
pub fn code() -> Service {
    Service::new(1, &["ping"])
}

/// The terminal sessions (`terminal.rs`), in each reply.
pub fn terminal() -> Service {
    Service::new(1, &[])
}

/// The processes (`procs.rs`), in each reply.
pub fn processes() -> Service {
    Service::new(1, &[])
}

/// The profile session (`profile/`), in a hello message it sends first.
pub fn profile() -> Service {
    Service::new(1, &[])
}
