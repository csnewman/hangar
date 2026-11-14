//! Names the parts of Hangar served on hosts of their own, each
//! environment's web servers and the container registry, after Hangar's
//! public host (internal/publichost).
//!
//! A name is a label joined to the public host in one of two styles: as a
//! subdomain (env<short>.hangar.example.com, the default), which needs the
//! zone under the public host and a certificate for *.<host>; or as a
//! prefix (env<short>-hangar.example.com), for networks where a machine has
//! one name and every <anything>-<name> beside it already resolves to it
//! under a wildcard certificate for the domain. Either way the names are in
//! Hangar's own site.

/// How a name is joined to the public host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    Subdomain,
    Prefix,
}

impl Style {
    /// A style as configured; empty is Subdomain.
    pub fn parse(s: &str) -> Result<Style, String> {
        match s.trim().to_lowercase().as_str() {
            "" | "subdomain" => Ok(Style::Subdomain),
            "prefix" => Ok(Style::Prefix),
            _ => Err(format!(
                "the host style is \"subdomain\" or \"prefix\", not {s:?}"
            )),
        }
    }

    fn join(self) -> char {
        match self {
            Style::Subdomain => '.',
            Style::Prefix => '-',
        }
    }
}

/// Hangar's origin as browsers reach it, and the style its other hosts are
/// named in.
#[derive(Debug, Clone)]
pub struct Public {
    pub scheme: String,
    /// Lower case, with the port when the origin has one.
    pub host: String,
    pub style: Style,
}

impl Public {
    /// Reads Hangar's public URL, of which only the origin is used.
    pub fn parse(raw: &str, style: Style) -> Result<Public, String> {
        let bad = || {
            format!(
                "the public URL must be an http or https origin, such as https://hangar.example.com; got {raw:?}"
            )
        };
        let (scheme, rest) = raw.split_once("://").ok_or_else(bad)?;
        let host = rest
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default()
            .to_lowercase();
        if !matches!(scheme, "http" | "https") || host.is_empty() {
            return Err(bad());
        }
        let p = Public {
            scheme: scheme.to_string(),
            host,
            style,
        };
        if style == Style::Prefix {
            let name = p.hostname();
            if name.parse::<std::net::IpAddr>().is_ok() || !name.contains('.') {
                return Err(format!(
                    "prefix host names are siblings of the public host in its domain, so it must be a name in one, such as hangar.example.com; got {name:?}"
                ));
            }
        }
        Ok(p)
    }

    /// Whether people reach Hangar over HTTPS.
    pub fn secure(&self) -> bool {
        self.scheme == "https"
    }

    /// The public host without its port.
    pub fn hostname(&self) -> &str {
        hostname(&self.host)
    }

    /// Hangar's own origin.
    pub fn origin(&self) -> String {
        format!("{}://{}", self.scheme, self.host)
    }

    /// The host for label, with the public host's port if it has one.
    pub fn name(&self, label: &str) -> String {
        format!("{}{}{}", label.to_lowercase(), self.style.join(), self.host)
    }

    /// The label host is named for, if it is one of the names: it ends in
    /// the public host, port included, joined in this style.
    pub fn label(&self, host: &str) -> Option<String> {
        let host = host.to_lowercase();
        let suffix = format!("{}{}", self.style.join(), self.host);
        let label = host.strip_suffix(&suffix)?;
        (!label.is_empty() && !label.contains('.')).then(|| label.to_string())
    }
}

/// A host without its port, or an IPv6 address without its brackets.
pub fn hostname(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    host.rsplit_once(':').map_or(host, |(name, _)| name)
}
