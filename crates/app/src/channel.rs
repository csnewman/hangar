//! Which build of the app this is, chosen when it is compiled
//! (`HANGAR_CHANNEL`): a developer's own build, the experimental release, or
//! the stable one. Each is an app of its own, with its own name, settings,
//! places and tokens, so one installed beside another, or beside the
//! Electron app, never reads or writes what the other keeps.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// Built from a checkout (`dev/app.sh`): the default.
    Dev,
    /// The experimental release, the `hangar-experimental` cask.
    Experimental,
    /// The release that replaces the Electron app.
    Stable,
}

impl Channel {
    /// The channel this build was compiled for.
    pub fn current() -> Channel {
        match option_env!("HANGAR_CHANNEL") {
            Some("stable") => Channel::Stable,
            Some("experimental") => Channel::Experimental,
            _ => Channel::Dev,
        }
    }

    /// The app's name: its menu's, and its folders' and Keychain service's.
    pub fn name(self) -> &'static str {
        match self {
            Channel::Dev => "Hangar Dev",
            Channel::Experimental => "Hangar Experimental",
            Channel::Stable => "Hangar",
        }
    }
}
