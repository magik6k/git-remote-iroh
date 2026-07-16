//! git-remote-iroh: p2p git fetch/push over iroh.
//!
//! Invoked by git as a remote helper for iroh:// URLs, or manually with the
//! `serve` / `id` subcommands.

mod bridge;
mod helper;
mod proto;
mod serve;
mod ticket;

use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use iroh::EndpointAddr;
use iroh_tickets::endpoint::EndpointTicket;
use tracing_subscriber::EnvFilter;

use crate::serve::ServeOpts;
use crate::ticket::{Identity, RemoteUrl};

#[derive(Parser)]
#[command(name = "git-remote-iroh", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Serve the current repository to iroh:// peers until Ctrl-C.
    Serve {
        /// Also accept pushes (git receive-pack).
        #[arg(long)]
        writable: bool,
        /// Exit after the first successful session.
        #[arg(long)]
        once: bool,
        /// Use a throwaway identity: the printed URL only works for this
        /// session, instead of being stable across serves of this repo.
        #[arg(long)]
        ephemeral: bool,
    },
    /// Print this repository's stable short iroh:// URL. Peers can dial it
    /// through discovery whenever `serve` is running here.
    Id,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .with_writer(std::io::stderr)
        .init();

    // git invokes remote helpers as `git-remote-iroh <remote> <url>`.
    let args: Vec<String> = std::env::args().collect();
    let res = if args.len() == 3 && args[2].starts_with(ticket::URL_SCHEME) {
        helper::run(&args[1], &args[2]).await
    } else {
        run_cli().await
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_cli() -> Result<()> {
    match Cli::parse().command {
        Command::Serve {
            writable,
            once,
            ephemeral,
        } => {
            let git_dir = ticket::git_dir()?;
            let identity = if ephemeral {
                Identity::ephemeral()
            } else {
                Identity::load_or_create(&git_dir)?
            };
            if writable {
                warn_if_push_will_bounce();
            }
            serve::serve(
                ServeOpts {
                    git_dir,
                    identity,
                    writable,
                    once,
                },
                |url| {
                    // The URL goes to stdout for scripts; hints go to stderr.
                    println!("{url}");
                    eprintln!();
                    eprintln!("Serving this repository over iroh. On another machine:");
                    eprintln!();
                    eprintln!("    git clone {url}");
                    eprintln!("    git remote add <name> {url}");
                    eprintln!();
                    eprintln!("Waiting for peers (Ctrl-C to stop) ...");
                },
            )
            .await
        }
        Command::Id => {
            let git_dir = ticket::git_dir()?;
            let identity = Identity::load_or_create(&git_dir)?;
            let url = RemoteUrl {
                ticket: EndpointTicket::new(EndpointAddr::new(identity.key.public())),
                secret: identity.secret,
            };
            println!("{url}");
            Ok(())
        }
    }
}

/// Pushing into a non-bare repo bounces off the checked-out branch unless
/// receive.denyCurrentBranch is configured; warn up front.
fn warn_if_push_will_bounce() {
    let bare = git_stdout(&["rev-parse", "--is-bare-repository"]);
    if bare.as_deref() != Some("false") {
        return;
    }
    let deny = git_stdout(&["config", "receive.denyCurrentBranch"]).unwrap_or_default();
    if deny != "updateInstead" && deny != "ignore" && deny != "warn" {
        eprintln!(
            "hint: this repository has a working tree; pushes to the checked-out branch\n\
             hint: will be rejected. To accept them and update the working tree, run:\n\
             hint:     git config receive.denyCurrentBranch updateInstead"
        );
    }
}

fn git_stdout(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}
