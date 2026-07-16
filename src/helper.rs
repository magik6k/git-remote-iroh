//! The git remote helper protocol (spoken with git over stdin/stdout).
//!
//! Two modes:
//! - connect mode (`iroh://<ticket>/<secret>`): advertise the `connect`
//!   capability and bridge git's native packfile protocol to the remote
//!   `serve` over an iroh stream. Fetch and push both go through here.
//! - offer mode (bare `iroh://`): advertise `push`, and implement it by
//!   serving this repository for a single fetch, printing the URL for the
//!   other side. "Pushing" completes when the peer has pulled.

use anyhow::{bail, Context, Result};
use iroh::endpoint::presets;
use iroh::Endpoint;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

use crate::bridge::forward_bidi;
use crate::proto::{self, Header, Service, Status};
use crate::serve::{serve, ServeOpts};
use crate::ticket::{self, Identity, RemoteUrl};

pub async fn run(_remote: &str, url: &str) -> Result<()> {
    match RemoteUrl::parse(url)? {
        Some(remote_url) => connect_mode(remote_url).await,
        None => offer_mode().await,
    }
}

/// Read one helper command line. Reads byte by byte on purpose: after a
/// `connect` command the stream switches to raw git protocol traffic, so we
/// must never buffer past the newline.
async fn read_command(stdin: &mut (impl AsyncRead + Unpin)) -> Result<Option<String>> {
    let mut line = Vec::new();
    loop {
        let mut b = [0u8; 1];
        if stdin.read(&mut b).await? == 0 {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(String::from_utf8(line)?))
            };
        }
        if b[0] == b'\n' {
            return Ok(Some(String::from_utf8(line)?));
        }
        line.push(b[0]);
    }
}

async fn connect_mode(url: RemoteUrl) -> Result<()> {
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    while let Some(cmd) = read_command(&mut stdin).await? {
        match cmd.as_str() {
            "capabilities" => {
                stdout.write_all(b"connect\n\n").await?;
                stdout.flush().await?;
            }
            "" => continue,
            c => {
                let Some(service_name) = c.strip_prefix("connect ") else {
                    bail!("unsupported helper command: {c}");
                };
                let Some(service) = Service::from_helper_name(service_name) else {
                    bail!("unsupported service: {service_name}");
                };
                return connect_and_bridge(&url, service, stdin, stdout).await;
            }
        }
    }
    Ok(())
}

async fn connect_and_bridge(
    url: &RemoteUrl,
    service: Service,
    stdin: tokio::io::Stdin,
    mut stdout: tokio::io::Stdout,
) -> Result<()> {
    let endpoint = Endpoint::builder(presets::N0)
        .bind()
        .await
        .context("failed to bind iroh endpoint")?;
    let res = async {
        let connection = endpoint
            .connect(url.ticket.endpoint_addr().clone(), proto::ALPN)
            .await
            .context(
                "could not reach the remote peer \
                 (is `git-remote-iroh serve` or `git push iroh://` still running there?)",
            )?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .context("failed to open stream")?;
        let header = Header {
            service,
            secret: url.secret,
            git_protocol: std::env::var("GIT_PROTOCOL")
                .ok()
                .filter(|s| proto::valid_git_protocol(s)),
        };
        header.write(&mut send).await?;
        let status = Status::read(&mut recv)
            .await
            .context("remote closed the connection before replying")?;
        match status {
            Status::Ok => {}
            Status::BadAuth => bail!("remote rejected the secret in this URL"),
            Status::Denied => {
                bail!("remote does not accept pushes; run `git-remote-iroh serve --writable` there")
            }
        }
        // Tell git the connection is established; from here on stdin/stdout
        // carry the native git protocol.
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
        forward_bidi(stdin, stdout, recv, send).await?;
        Ok(())
    }
    .await;
    endpoint.close().await;
    res
}

async fn offer_mode() -> Result<()> {
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    while let Some(cmd) = read_command(&mut stdin).await? {
        match cmd.as_str() {
            "capabilities" => {
                stdout.write_all(b"push\n\n").await?;
                stdout.flush().await?;
            }
            // We are not a real remote; nothing exists "there" yet.
            "list for-push" => {
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
            "" => continue,
            c if c.starts_with("push ") => {
                let mut specs = vec![c["push ".len()..].to_string()];
                // A push batch is terminated by a blank line.
                while let Some(line) = read_command(&mut stdin).await? {
                    if line.is_empty() {
                        break;
                    }
                    let Some(spec) = line.strip_prefix("push ") else {
                        bail!("unexpected command in push batch: {line}");
                    };
                    specs.push(spec.to_string());
                }
                offer_and_wait().await?;
                for spec in &specs {
                    let dst = spec.rsplit_once(':').map(|(_, dst)| dst).unwrap_or(spec);
                    stdout.write_all(format!("ok {dst}\n").as_bytes()).await?;
                }
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
            other => bail!("unsupported helper command: {other}"),
        }
    }
    Ok(())
}

/// Serve the current repository until one peer completes a fetch.
async fn offer_and_wait() -> Result<()> {
    let git_dir = ticket::git_dir()?;
    let identity = Identity::load_or_create(&git_dir)?;
    serve(
        ServeOpts {
            git_dir,
            identity,
            writable: false,
            once: true,
        },
        |url| {
            eprintln!();
            eprintln!("Offering this repository for one fetch over iroh.");
            eprintln!("On the other machine, run one of:");
            eprintln!();
            eprintln!("    git clone {url}");
            eprintln!("    git pull {url} <branch>");
            eprintln!();
            eprintln!("Waiting for the peer (Ctrl-C to abort) ...");
        },
    )
    .await
}
