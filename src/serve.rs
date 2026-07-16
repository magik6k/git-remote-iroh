//! The serving side: accept iroh connections and wire them up to
//! `git upload-pack` / `git receive-pack` on this repository.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use iroh::endpoint::presets;
use iroh::Endpoint;
use iroh_tickets::endpoint::EndpointTicket;
use tokio::time::timeout;

use crate::bridge::forward_bidi;
use crate::proto::{self, Header, Service, Status, SECRET_LEN};
use crate::ticket::{Identity, RemoteUrl};

const ONLINE_TIMEOUT: Duration = Duration::from_secs(5);

pub struct ServeOpts {
    pub git_dir: PathBuf,
    pub identity: Identity,
    pub writable: bool,
    /// Exit after the first successful session.
    pub once: bool,
}

#[derive(Clone)]
struct SessionCfg {
    git_dir: PathBuf,
    secret: [u8; SECRET_LEN],
    writable: bool,
}

/// Bind an endpoint, report the dialable URL through `on_ready`, then accept
/// and handle sessions until Ctrl-C (or, with `once`, until one succeeds).
pub async fn serve(opts: ServeOpts, on_ready: impl FnOnce(&str)) -> Result<()> {
    let cfg = SessionCfg {
        git_dir: opts.git_dir,
        secret: opts.identity.secret,
        writable: opts.writable,
    };
    let endpoint = Endpoint::builder(presets::N0)
        .secret_key(opts.identity.key.clone())
        .alpns(vec![proto::ALPN.to_vec()])
        .bind()
        .await
        .context("failed to bind iroh endpoint")?;
    if timeout(ONLINE_TIMEOUT, endpoint.online()).await.is_err() {
        eprintln!("warning: could not reach a relay server; only direct connections will work");
    }
    let url = RemoteUrl {
        ticket: EndpointTicket::new(endpoint.addr()),
        secret: cfg.secret,
    };
    on_ready(&url.to_string());

    let mut result = Ok(());
    loop {
        let incoming = tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                if opts.once {
                    result = Err(anyhow::anyhow!("interrupted"));
                }
                break;
            }
            inc = endpoint.accept() => match inc {
                Some(inc) => inc,
                None => break,
            },
        };
        if opts.once {
            let res = tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    result = Err(anyhow::anyhow!("interrupted"));
                    break;
                }
                res = handle_incoming(incoming, cfg.clone()) => res,
            };
            match res {
                Ok(()) => break,
                // Keep waiting: a failed attempt (wrong secret, aborted
                // fetch) should not consume the offer.
                Err(e) => eprintln!("session failed: {e:#}"),
            }
        } else {
            let cfg = cfg.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_incoming(incoming, cfg).await {
                    eprintln!("session failed: {e:#}");
                }
            });
        }
    }
    endpoint.close().await;
    result
}

async fn handle_incoming(incoming: iroh::endpoint::Incoming, cfg: SessionCfg) -> Result<()> {
    let connection = incoming.await.context("failed to accept connection")?;
    let remote = short_id(&connection.remote_id().to_string());
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .context("failed to accept stream")?;
    let header = timeout(Duration::from_secs(10), Header::read(&mut recv))
        .await
        .context("timed out waiting for the client header")??;

    if !proto::secrets_match(&header.secret, &cfg.secret) {
        reject(send, &connection, Status::BadAuth).await;
        bail!("rejected {remote}: wrong secret");
    }
    if header.service == Service::ReceivePack && !cfg.writable {
        reject(send, &connection, Status::Denied).await;
        bail!("rejected push from {remote}: serving read-only (use --writable)");
    }

    let verb = match header.service {
        Service::UploadPack => "fetch",
        Service::ReceivePack => "push",
    };
    eprintln!("{verb} session from {remote} ...");

    let mut cmd = tokio::process::Command::new("git");
    cmd.arg(header.service.git_subcommand())
        .arg(&cfg.git_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    if let Some(gp) = &header.git_protocol {
        cmd.env("GIT_PROTOCOL", gp);
    }
    let mut child = cmd.spawn().context("failed to spawn git")?;
    Status::Ok.write(&mut send).await?;

    let child_stdout = child.stdout.take().expect("stdout piped");
    let child_stdin = child.stdin.take().expect("stdin piped");
    // The bridge often ends with a benign error: once the peer has everything
    // it closes the connection rather than half-closing its send side. The
    // child's exit status is what decides whether the session worked.
    let bridge_res = forward_bidi(child_stdout, child_stdin, recv, send).await;
    let status = child.wait().await?;
    if !status.success() {
        bail!(
            "git {} exited with {status}",
            header.service.git_subcommand()
        );
    }
    if let Err(e) = bridge_res {
        tracing::debug!("bridge ended: {e:#}");
    }
    // Wait for the peer to close the connection so everything we sent is
    // known to be delivered before a --once serve tears the endpoint down.
    let _ = timeout(Duration::from_secs(30), connection.closed()).await;
    eprintln!("{verb} session from {remote} done");
    Ok(())
}

fn short_id(id: &str) -> String {
    id.chars().take(10).collect()
}

/// Send a rejection status and give it a moment to reach the peer.
async fn reject(
    mut send: noq::SendStream,
    connection: &iroh::endpoint::Connection,
    status: Status,
) {
    if status.write(&mut send).await.is_ok() {
        send.finish().ok();
        let _ = timeout(Duration::from_secs(3), connection.closed()).await;
    }
}
