//! iroh:// URL parsing/formatting and per-repository identity persistence.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{bail, Context, Result};
use iroh::SecretKey;
use iroh_tickets::endpoint::EndpointTicket;

use crate::proto::SECRET_LEN;

pub const URL_SCHEME: &str = "iroh://";

/// A parsed iroh:// remote URL: where to dial, plus the bearer secret.
#[derive(Debug, Clone)]
pub struct RemoteUrl {
    pub ticket: EndpointTicket,
    pub secret: [u8; SECRET_LEN],
}

impl RemoteUrl {
    /// Parse an iroh:// URL. A bare `iroh://` (no ticket) selects offer mode
    /// and parses to `None`.
    pub fn parse(url: &str) -> Result<Option<Self>> {
        let Some(rest) = url.strip_prefix(URL_SCHEME) else {
            bail!("not an iroh:// url: {url}");
        };
        let rest = rest.trim_matches('/');
        if rest.is_empty() {
            return Ok(None);
        }
        let Some((ticket_str, secret_str)) = rest.split_once('/') else {
            bail!("bad iroh:// url; expected iroh://<ticket>/<secret>");
        };
        let ticket = EndpointTicket::from_str(ticket_str)
            .map_err(|e| anyhow::anyhow!("invalid ticket in url: {e}"))?;
        let secret_bytes = data_encoding::HEXLOWER_PERMISSIVE
            .decode(secret_str.trim_matches('/').as_bytes())
            .context("invalid secret in url")?;
        let secret: [u8; SECRET_LEN] = secret_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("secret in url has the wrong length"))?;
        Ok(Some(Self { ticket, secret }))
    }
}

impl fmt::Display for RemoteUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{URL_SCHEME}{}/{}",
            self.ticket,
            data_encoding::HEXLOWER.encode(&self.secret)
        )
    }
}

/// The serving side's stable identity: the iroh endpoint key that peers dial,
/// and the bearer secret that authorizes them.
pub struct Identity {
    pub key: SecretKey,
    pub secret: [u8; SECRET_LEN],
}

impl Identity {
    pub fn ephemeral() -> Self {
        Self {
            key: SecretKey::generate(),
            secret: rand::random(),
        }
    }

    /// Load the identity stored under `<git-dir>/iroh/`, creating it on first
    /// use. Reusing the identity keeps the repository's URL stable across
    /// serve sessions.
    pub fn load_or_create(git_dir: &Path) -> Result<Self> {
        let dir = git_dir.join("iroh");
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("cannot create {}", dir.display()))?;

        let key_path = dir.join("key");
        let key = match read_trimmed(&key_path)? {
            Some(hex) => SecretKey::from_str(&hex)
                .map_err(|e| anyhow::anyhow!("corrupt {}: {e}", key_path.display()))?,
            None => {
                let key = SecretKey::generate();
                write_private(&key_path, &data_encoding::HEXLOWER.encode(&key.to_bytes()))?;
                key
            }
        };

        let token_path = dir.join("token");
        let secret = match read_trimmed(&token_path)? {
            Some(hex) => {
                let bytes = data_encoding::HEXLOWER_PERMISSIVE
                    .decode(hex.as_bytes())
                    .with_context(|| format!("corrupt {}", token_path.display()))?;
                bytes
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("corrupt {}", token_path.display()))?
            }
            None => {
                let secret: [u8; SECRET_LEN] = rand::random();
                write_private(&token_path, &data_encoding::HEXLOWER.encode(&secret))?;
                secret
            }
        };

        Ok(Self { key, secret })
    }
}

fn read_trimmed(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s.trim().to_string())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

fn write_private(path: &Path, contents: &str) -> Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    use std::io::Write;
    file.write_all(contents.as_bytes())
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

/// Absolute path of the current repository's git dir. Respects GIT_DIR, which
/// git sets when it invokes the helper.
pub fn git_dir() -> Result<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .context("failed to run git")?;
    if !out.status.success() {
        bail!("not inside a git repository");
    }
    Ok(PathBuf::from(String::from_utf8(out.stdout)?.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use iroh::EndpointAddr;

    #[test]
    fn url_roundtrip() {
        let key = SecretKey::generate();
        let ticket = EndpointTicket::new(EndpointAddr::new(key.public()));
        let url = RemoteUrl {
            ticket,
            secret: [42u8; SECRET_LEN],
        };
        let s = url.to_string();
        assert!(s.starts_with(URL_SCHEME));
        let parsed = RemoteUrl::parse(&s).unwrap().expect("not offer mode");
        assert_eq!(parsed.secret, url.secret);
        assert_eq!(parsed.ticket.endpoint_addr(), url.ticket.endpoint_addr());
    }

    #[test]
    fn bare_url_is_offer_mode() {
        assert!(RemoteUrl::parse("iroh://").unwrap().is_none());
        assert!(RemoteUrl::parse("iroh:///").unwrap().is_none());
    }
}
