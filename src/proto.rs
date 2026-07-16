//! Wire protocol between the helper (client) and `serve` (server), spoken on
//! a single iroh bidirectional stream before handing the bytes over to git.
//!
//! Client -> server:
//!   magic (4) | service (1) | secret (16) | gp_len (1) | git_protocol (gp_len)
//! Server -> client:
//!   status (1)
//! Everything after that is raw `git upload-pack` / `git receive-pack` traffic.

use anyhow::{bail, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// ALPN for the git-remote-iroh protocol.
pub const ALPN: &[u8] = b"git-remote-iroh/0";

/// Magic bytes opening every stream; doubles as the protocol version gate.
pub const MAGIC: [u8; 4] = *b"GRI0";

pub const SECRET_LEN: usize = 16;

/// Longest GIT_PROTOCOL value we are willing to forward into the environment.
const MAX_GIT_PROTOCOL_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Service {
    UploadPack,
    ReceivePack,
}

impl Service {
    /// Parse the service name git passes to the helper's `connect` command.
    pub fn from_helper_name(name: &str) -> Option<Self> {
        match name {
            "git-upload-pack" => Some(Self::UploadPack),
            "git-receive-pack" => Some(Self::ReceivePack),
            _ => None,
        }
    }

    pub fn git_subcommand(self) -> &'static str {
        match self {
            Self::UploadPack => "upload-pack",
            Self::ReceivePack => "receive-pack",
        }
    }

    fn to_byte(self) -> u8 {
        match self {
            Self::UploadPack => 1,
            Self::ReceivePack => 2,
        }
    }

    fn from_byte(b: u8) -> Result<Self> {
        match b {
            1 => Ok(Self::UploadPack),
            2 => Ok(Self::ReceivePack),
            _ => bail!("unknown service {b}"),
        }
    }
}

#[derive(Debug)]
pub struct Header {
    pub service: Service,
    pub secret: [u8; SECRET_LEN],
    /// Value of GIT_PROTOCOL on the client, forwarded to the spawned service
    /// so protocol v2 works end to end.
    pub git_protocol: Option<String>,
}

impl Header {
    pub async fn write(&self, w: &mut (impl AsyncWrite + Unpin)) -> Result<()> {
        let gp = self.git_protocol.as_deref().unwrap_or("");
        let mut buf = Vec::with_capacity(MAGIC.len() + 1 + SECRET_LEN + 1 + gp.len());
        buf.extend_from_slice(&MAGIC);
        buf.push(self.service.to_byte());
        buf.extend_from_slice(&self.secret);
        buf.push(gp.len() as u8);
        buf.extend_from_slice(gp.as_bytes());
        w.write_all(&buf).await?;
        Ok(())
    }

    pub async fn read(r: &mut (impl AsyncRead + Unpin)) -> Result<Self> {
        let mut magic = [0u8; MAGIC.len()];
        r.read_exact(&mut magic).await?;
        if magic != MAGIC {
            bail!("bad magic; peer is not a git-remote-iroh client");
        }
        let service = Service::from_byte(r.read_u8().await?)?;
        let mut secret = [0u8; SECRET_LEN];
        r.read_exact(&mut secret).await?;
        let gp_len = r.read_u8().await? as usize;
        if gp_len > MAX_GIT_PROTOCOL_LEN {
            bail!("GIT_PROTOCOL value too long");
        }
        let git_protocol = if gp_len == 0 {
            None
        } else {
            let mut buf = vec![0u8; gp_len];
            r.read_exact(&mut buf).await?;
            let s = String::from_utf8(buf)?;
            if !valid_git_protocol(&s) {
                bail!("invalid GIT_PROTOCOL value");
            }
            Some(s)
        };
        Ok(Self {
            service,
            secret,
            git_protocol,
        })
    }
}

/// A GIT_PROTOCOL value we accept into the environment: short printable ASCII.
pub fn valid_git_protocol(s: &str) -> bool {
    s.len() <= MAX_GIT_PROTOCOL_LEN && s.bytes().all(|b| (0x20..0x7f).contains(&b))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    BadAuth,
    Denied,
}

impl Status {
    pub async fn write(self, w: &mut (impl AsyncWrite + Unpin)) -> Result<()> {
        let b = match self {
            Self::Ok => 0u8,
            Self::BadAuth => 1,
            Self::Denied => 2,
        };
        w.write_all(&[b]).await?;
        Ok(())
    }

    pub async fn read(r: &mut (impl AsyncRead + Unpin)) -> Result<Self> {
        match r.read_u8().await? {
            0 => Ok(Self::Ok),
            1 => Ok(Self::BadAuth),
            2 => Ok(Self::Denied),
            b => bail!("unknown status {b}"),
        }
    }
}

/// Constant-time comparison of two secrets.
pub fn secrets_match(a: &[u8; SECRET_LEN], b: &[u8; SECRET_LEN]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn header_roundtrip() {
        let header = Header {
            service: Service::ReceivePack,
            secret: [7u8; SECRET_LEN],
            git_protocol: Some("version=2".to_string()),
        };
        let mut buf = Vec::new();
        header.write(&mut buf).await.unwrap();
        let parsed = Header::read(&mut buf.as_slice()).await.unwrap();
        assert_eq!(parsed.service, Service::ReceivePack);
        assert_eq!(parsed.secret, [7u8; SECRET_LEN]);
        assert_eq!(parsed.git_protocol.as_deref(), Some("version=2"));
    }

    #[test]
    fn secret_compare() {
        assert!(secrets_match(&[1; SECRET_LEN], &[1; SECRET_LEN]));
        let mut other = [1u8; SECRET_LEN];
        other[SECRET_LEN - 1] = 2;
        assert!(!secrets_match(&[1; SECRET_LEN], &other));
    }
}
