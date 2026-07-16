//! Bidirectional byte bridging between a local reader/writer pair (helper
//! stdio, or a spawned git service's pipes) and an iroh stream pair.

use std::io;

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

/// Copy local -> remote until EOF, then half-close the remote stream so the
/// peer sees a clean FIN.
async fn copy_to_remote(
    mut from: impl AsyncRead + Unpin,
    mut send: noq::SendStream,
    token: &CancellationToken,
) -> io::Result<u64> {
    tokio::select! {
        res = tokio::io::copy(&mut from, &mut send) => {
            let n = res?;
            send.finish()?;
            Ok(n)
        }
        _ = token.cancelled() => {
            send.reset(0u8.into()).ok();
            Err(io::Error::other("cancelled"))
        }
    }
}

/// Copy remote -> local until FIN, then shut the local writer down so a piped
/// child process sees EOF on its stdin.
async fn copy_to_local(
    mut recv: noq::RecvStream,
    mut to: impl AsyncWrite + Unpin,
    token: &CancellationToken,
) -> io::Result<u64> {
    tokio::select! {
        res = async {
            let n = tokio::io::copy(&mut recv, &mut to).await?;
            to.shutdown().await?;
            io::Result::Ok(n)
        } => res,
        _ = token.cancelled() => {
            recv.stop(0u8.into()).ok();
            Err(io::Error::other("cancelled"))
        }
    }
}

/// Forward both directions until each side reaches EOF. An error on one
/// direction cancels the other; a clean EOF does not (half-close is a normal
/// part of the git protocols).
pub async fn forward_bidi(
    from_local: impl AsyncRead + Unpin,
    to_local: impl AsyncWrite + Unpin,
    from_remote: noq::RecvStream,
    to_remote: noq::SendStream,
) -> io::Result<()> {
    let token = CancellationToken::new();
    let (to_remote_res, to_local_res) = tokio::join!(
        async {
            let res = copy_to_remote(from_local, to_remote, &token).await;
            if res.is_err() {
                token.cancel();
            }
            res
        },
        async {
            let res = copy_to_local(from_remote, to_local, &token).await;
            if res.is_err() {
                token.cancel();
            }
            res
        }
    );
    to_remote_res?;
    to_local_res?;
    Ok(())
}
