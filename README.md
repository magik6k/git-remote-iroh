# git-remote-iroh

Fetch and push git repositories directly between machines — no hosting
service, no daemon, no port forwarding, no VPN.

```sh
# machine A
$ git push iroh:// main
    git clone iroh://endpoint2rgc…/8f00ba4…      # <- prints this, waits

# machine B, anywhere in the world
$ git clone iroh://endpoint2rgc…/8f00ba4…        # A's push completes
```

git-remote-iroh is a [git remote helper] that tunnels **git's native smart
protocol** over [iroh] — QUIC connections with NAT hole-punching, relay
fallback, and end-to-end encryption. Because the real `git upload-pack` and
`git receive-pack` run on each end, transfers get full want/have negotiation
and server-built thin packs: fetching and pushing over `iroh://` moves
exactly the same bytes as over `ssh://`, between any two machines that can
each reach the internet (or each other).

[git remote helper]: https://git-scm.com/docs/gitremote-helpers
[iroh]: https://www.iroh.computer/

## Install

Prebuilt binary (Linux, Apple-silicon macOS; installs to `~/.local/bin`):

```sh
curl -fsSL https://raw.githubusercontent.com/magik6k/git-remote-iroh/main/install.sh | sh
```

With cargo, from source (any platform, rust ≥ 1.91):

```sh
cargo install --git https://github.com/magik6k/git-remote-iroh
```

Or grab a binary from the [releases page]. However it gets there — once
`git-remote-iroh` is on `PATH`, git handles `iroh://` URLs by itself.

[releases page]: https://github.com/magik6k/git-remote-iroh/releases

## Usage

### One-shot transfer

`git push iroh://` (with a bare URL) offers the repository for a single
fetch. It prints a URL, blocks until the other side has pulled, and then
reports the push as done:

```sh
# machine A
$ git push iroh:// main

# machine B — either of:
$ git clone iroh://endpoint…/8f00ba4… myrepo
$ git pull iroh://endpoint…/8f00ba4… main
```

Nothing is stored anywhere in between: the two machines talk directly
(through an iroh relay only when hole-punching fails, still encrypted
end to end).

### Serve a repository as a regular remote

```sh
# machine A
$ git-remote-iroh serve --writable
iroh://endpoint2rgc…/8f00ba4…

# machine B
$ git remote add laptop iroh://endpoint2rgc…/8f00ba4…
$ git fetch laptop
$ git push laptop main
```

The repository's identity lives in `.git/iroh/`, so the URL stays the same
across serve sessions — set it up as a named remote once and sync whenever
`serve` is running on the other side.

### Commands and options

| Command | Description |
| --- | --- |
| `git-remote-iroh serve` | Serve the current repository until Ctrl-C. |
| `serve --writable` | Also accept pushes (`git receive-pack`). |
| `serve --once` | Exit after the first successful session. |
| `serve --ephemeral` | Throwaway identity; the URL only works for this session. |
| `git-remote-iroh id` | Print the repo's stable short URL (dials via iroh discovery while `serve` runs). |

## Security model

- **The URL is a bearer capability.** It contains the endpoint's public key
  and a random 128-bit secret; anyone who has the full URL can fetch the
  repository (and push, with `--writable`). Share it like a password.
- Connections are authenticated and end-to-end encrypted by iroh (TLS over
  QUIC, keyed by the endpoint's ed25519 identity). Relays forward opaque
  ciphertext and cannot read repository data.
- The identity key and secret are stored with owner-only permissions in
  `.git/iroh/`. Delete that directory (or use `--ephemeral`) to rotate the
  URL.

## Notes

- Pushing into a repository with a working tree is refused for the
  checked-out branch unless that repo sets
  `git config receive.denyCurrentBranch updateInstead` (`serve` prints a
  hint). Pushing to other branches, or into a bare repo, just works.
- Offer-mode `git push iroh://` reporting `ok` means one peer fetched the
  offered refs — a way to hand a repo over, not durable storage.
- NAT traversal uses iroh's default relay and discovery infrastructure
  (run by [number 0]); on a shared LAN everything also works fully offline.

[number 0]: https://n0.computer/

## How it works

`serve` binds an iroh endpoint and prints `iroh://<ticket>/<secret>`. When
git sees an `iroh://` URL it invokes this helper, which advertises the
`connect` capability, dials the ticket, authenticates with the secret, and
then simply shovels bytes between git and the `git upload-pack` /
`git receive-pack` spawned on the serving side. Offer mode
(`git push iroh://`) is sugar that serves the repository for exactly one
fetch. The stream-bridging approach follows n0's [dumbpipe].

[dumbpipe]: https://github.com/n0-computer/dumbpipe

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
