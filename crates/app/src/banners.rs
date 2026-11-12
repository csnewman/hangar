//! What needs deciding about an environment, as the web's page says it in
//! banners: a newer build of its image, an upgrade to keep or roll back, a
//! change waiting for its next start, and how it differs from its template.
//! The environment's tab shows them as IntelliJ shows an editor's
//! notifications, a strip under its toolbar; its settings' General page
//! shows them with its image.

use crate::api::Environment;
use crate::envsettings::changeable;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Warn,
}

/// What an action on a banner does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Does {
    /// Changes its image: upgrade (forced past what the comparison found),
    /// rollback, keep or cancel.
    Image { change: &'static str, force: bool },
    /// Gives it its template's settings, asked first.
    Reset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Act {
    pub label: &'static str,
    pub does: Does,
    /// Whether it can be done now; why not, when not.
    pub ready: bool,
    pub why: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    /// Which banner, and about what: one put away stays away until what it
    /// says changes.
    pub key: String,
    pub kind: Kind,
    pub tone: Tone,
    pub text: String,
    /// Files it is about, listed on asking.
    pub files: Vec<String>,
    pub acts: Vec<Act>,
    /// Whether it may be put away.
    pub dismissible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// About its image: a newer build, an upgrade, a change waiting.
    Image,
    /// How it differs from its template.
    Template,
}

/// A digest as the web shows a build: twelve characters of its hash.
pub fn build(digest: &str) -> String {
    digest
        .strip_prefix("sha256:")
        .unwrap_or(digest)
        .chars()
        .take(12)
        .collect()
}

/// A setting an environment takes from its template, in words.
fn setting(name: &str) -> &str {
    match name {
        "cpus" => "vCPUs",
        "gpu" => "GPU",
        "dax" => "image mapping",
        "repos" => "repositories",
        "trusted_folders" => "trusted folders",
        "web_names" => "web server names",
        "name" => "accepted names",
        other => other,
    }
}

/// What needs deciding about an environment, most pressing first.
pub fn banners(env: &Environment) -> Vec<Banner> {
    let d = &env.details;
    let stopped = changeable(env);
    let stop_first = (!stopped).then_some("Stop the environment first");
    let image = d.image.clone().unwrap_or_else(|| "its image".into());
    let mut out = vec![];
    if let Some(change) = &d.image_change {
        let text = if change == "upgrade" {
            format!(
                "Its next start upgrades {image}{}, keeping a copy of its writable disk to roll back to.",
                env.image_update
                    .as_ref()
                    .map(|u| format!(" to {}", build(&u.digest)))
                    .unwrap_or_default()
            )
        } else {
            format!(
                "Its next start rolls back to {} and the writable disk it had then, losing what has been written to it since the upgrade.",
                d.image_rollback
                    .as_ref()
                    .map(|r| build(&r.digest))
                    .unwrap_or_else(|| "an earlier build".into())
            )
        };
        out.push(Banner {
            key: format!("change:{change}"),
            kind: Kind::Image,
            tone: Tone::Info,
            text,
            files: vec![],
            acts: vec![Act {
                label: "Cancel",
                does: Does::Image {
                    change: "cancel",
                    force: false,
                },
                ready: true,
                why: None,
            }],
            dismissible: false,
        });
    } else {
        if let Some(r) = &d.image_rollback {
            out.push(Banner {
                key: format!("rollback:{}", r.digest),
                kind: Kind::Image,
                tone: Tone::Info,
                text: format!(
                    "Upgraded from {} on {}. Its writable disk from before ({}) is kept, to roll back to.",
                    build(&r.digest),
                    r.at.format("%-d %b"),
                    crate::docker::bytes(r.size_bytes as f64)
                ),
                files: vec![],
                acts: vec![
                    Act {
                        label: "Roll Back",
                        does: Does::Image {
                            change: "rollback",
                            force: false,
                        },
                        ready: stopped,
                        why: stop_first,
                    },
                    Act {
                        label: "Keep Upgrade",
                        does: Does::Image {
                            change: "keep",
                            force: false,
                        },
                        ready: true,
                        why: None,
                    },
                ],
                dismissible: false,
            });
        }
        if let Some(u) = &env.image_update {
            let count = u.conflicts.len() as i64 + u.more_conflicts;
            let risky = u.checked && u.error.is_none() && (count > 0 || u.packages);
            let mut text = format!(
                "A newer build of {image} is ready ({}; this has {}).",
                build(&u.digest),
                d.image_digest
                    .as_deref()
                    .map(build)
                    .unwrap_or_else(|| "an earlier build".into())
            );
            if !u.checked {
                text.push_str(if stopped {
                    " Checking what upgrading would leave hidden…"
                } else {
                    " Stop the environment to check what upgrading would leave hidden."
                });
            } else if let Some(e) = &u.error {
                text.push_str(&format!(
                    " Its changes could not be compared with the newer build: {e}"
                ));
            } else if !risky {
                text.push_str(" None of its changes hide a file the newer build changes.");
            } else {
                if u.packages {
                    text.push_str(
                        " Packages have been installed in it, whose database would hide the newer one.",
                    );
                }
                if count > 0 {
                    text.push_str(&format!(
                        " {count} file{} it has changed would hide the newer build's.",
                        if count == 1 { "" } else { "s" }
                    ));
                }
            }
            let mut files = if risky { u.conflicts.clone() } else { vec![] };
            if risky && u.more_conflicts > 0 {
                files.push(format!("and {} more", u.more_conflicts));
            }
            let force = risky || u.error.is_some();
            out.push(Banner {
                key: format!("image:{}", u.digest),
                kind: Kind::Image,
                tone: Tone::Warn,
                text,
                files,
                acts: vec![Act {
                    label: if force {
                        "Upgrade Anyway"
                    } else {
                        "Upgrade at Next Start"
                    },
                    does: Does::Image {
                        change: "upgrade",
                        force,
                    },
                    ready: stopped && u.checked,
                    why: stop_first.or((!u.checked).then_some("Its files are not compared yet")),
                }],
                dismissible: true,
            });
        }
    }
    if !d.template_changes.is_empty() && d.template_id.is_some() {
        let names: Vec<&str> = d.template_changes.iter().map(|c| setting(c)).collect();
        out.push(Banner {
            key: format!("template:{}", names.join(",")),
            kind: Kind::Template,
            tone: Tone::Info,
            text: format!(
                "Changed from its template, {}: {}.",
                env.template,
                names.join(", ")
            ),
            files: vec![],
            acts: vec![Act {
                label: "Reset to Template",
                does: Does::Reset,
                ready: stopped,
                why: stop_first,
            }],
            dismissible: true,
        });
    }
    out
}
