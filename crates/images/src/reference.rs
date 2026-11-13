//! Image references, as Docker reads them: `ubuntu` is
//! `docker.io/library/ubuntu:latest`.

use std::fmt;

/// An image reference: where its repository is, and the tag or digest it
/// names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reference {
    /// The registry's host, with its port if it has one: docker.io for
    /// Docker Hub.
    pub domain: String,
    /// The repository's path in the registry: library/ubuntu.
    pub path: String,
    pub tag: Option<String>,
    /// A manifest's digest, sha256:<hex>.
    pub digest: Option<String>,
}

impl Reference {
    /// Reads a reference as Docker normalises one: a name with no registry
    /// is Docker Hub's, in library/ when it has no path of its own, and one
    /// with neither tag nor digest is :latest. A first component with a dot,
    /// a colon or a capital, or localhost, is a registry's host.
    pub fn parse(s: &str) -> Result<Reference, String> {
        let bad = |why: &str| Err(format!("{s}: {why}"));
        let (rest, digest) = match s.split_once('@') {
            Some((r, d)) => (r, Some(d.to_string())),
            None => (s, None),
        };
        if let Some(d) = &digest {
            let ok = d
                .strip_prefix("sha256:")
                .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()));
            if !ok {
                return bad("a digest that is not sha256:<64 hex digits>");
            }
        }
        // A tag follows the last colon after the last slash.
        let last_slash = rest.rfind('/').map(|i| i + 1).unwrap_or(0);
        let (name, tag) = match rest[last_slash..].rfind(':') {
            Some(i) => (
                &rest[..last_slash + i],
                Some(rest[last_slash + i + 1..].to_string()),
            ),
            None => (rest, None),
        };
        if let Some(t) = &tag {
            let ok = !t.is_empty()
                && t.len() <= 128
                && t.bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
                && t.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b));
            if !ok {
                return bad("a tag that is not one");
            }
        }
        let (mut domain, mut path) = match name.split_once('/') {
            Some((first, more))
                if first.contains(['.', ':'])
                    || first == "localhost"
                    || first.to_lowercase() != first =>
            {
                (first.to_string(), more.to_string())
            }
            _ => ("docker.io".to_string(), name.to_string()),
        };
        if domain == "index.docker.io" {
            domain = "docker.io".into();
        }
        if domain == "docker.io" && !path.contains('/') {
            path = format!("library/{path}");
        }
        let component_ok = |c: &str| {
            !c.is_empty()
                && c.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
                && c.bytes().next().is_some_and(|b| b.is_ascii_alphanumeric())
                && c.bytes().last().is_some_and(|b| b.is_ascii_alphanumeric())
        };
        if path.is_empty() || !path.split('/').all(component_ok) {
            return bad("a repository name that is not lowercase letters, digits and separators");
        }
        let tag = match (&tag, &digest) {
            // A digest names the manifest; a tag beside it says nothing more.
            (_, Some(_)) => None,
            (None, None) => Some("latest".into()),
            (t, None) => t.clone(),
        };
        Ok(Reference {
            domain,
            path,
            tag,
            digest,
        })
    }

    /// The same repository, at a digest.
    pub fn at(&self, digest: &str) -> Reference {
        Reference {
            tag: None,
            digest: Some(digest.into()),
            ..self.clone()
        }
    }

    /// What the registry API names it by: the digest, else the tag.
    pub fn version(&self) -> &str {
        self.digest
            .as_deref()
            .or(self.tag.as_deref())
            .unwrap_or("latest")
    }

    /// The host the registry's API is reached on: Docker Hub's is
    /// registry-1.docker.io.
    pub fn api_host(&self) -> &str {
        if self.domain == "docker.io" {
            "registry-1.docker.io"
        } else {
            &self.domain
        }
    }

    /// Whether the registry is on this machine, which is spoken to over
    /// plain HTTP, as one run for testing images before they are published
    /// is.
    pub fn is_local(&self) -> bool {
        let host = match self.domain.rsplit_once(':') {
            Some((h, p)) if p.bytes().all(|b| b.is_ascii_digit()) => h,
            _ => &self.domain,
        };
        matches!(host, "localhost" | "127.0.0.1" | "[::1]" | "::1")
    }
}

impl fmt::Display for Reference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.domain, self.path)?;
        if let Some(t) = &self.tag {
            write!(f, ":{t}")?;
        }
        if let Some(d) = &self.digest {
            write!(f, "@{d}")?;
        }
        Ok(())
    }
}
