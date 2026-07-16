#!/bin/sh
# Install the latest git-remote-iroh release binary.
#
#   curl -fsSL https://raw.githubusercontent.com/magik6k/git-remote-iroh/main/install.sh | sh
#
# Installs to ~/.local/bin by default; override with GIT_REMOTE_IROH_INSTALL_DIR.
set -eu

REPO="magik6k/git-remote-iroh"

err() {
    echo "error: $*" >&2
    exit 1
}

os=$(uname -s)
arch=$(uname -m)
case "$os" in
    Linux)
        case "$arch" in
            x86_64) target="x86_64-unknown-linux-musl" ;;
            aarch64 | arm64) target="aarch64-unknown-linux-musl" ;;
            *) err "unsupported architecture: $arch (try: cargo install --git https://github.com/$REPO)" ;;
        esac
        ;;
    Darwin)
        case "$arch" in
            x86_64) target="x86_64-apple-darwin" ;;
            arm64) target="aarch64-apple-darwin" ;;
            *) err "unsupported architecture: $arch (try: cargo install --git https://github.com/$REPO)" ;;
        esac
        ;;
    *)
        err "unsupported OS: $os (try: cargo install --git https://github.com/$REPO)"
        ;;
esac

tag=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
    grep -o '"tag_name": *"[^"]*"' | head -n1 | cut -d'"' -f4)
[ -n "$tag" ] || err "could not determine the latest release"

name="git-remote-iroh-$tag-$target"
url="https://github.com/$REPO/releases/download/$tag/$name.tar.gz"

bin_dir="${GIT_REMOTE_IROH_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$bin_dir"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading $url"
curl -fsSL "$url" | tar xz -C "$tmp"
install -m 755 "$tmp/$name/git-remote-iroh" "$bin_dir/git-remote-iroh"
echo "Installed $bin_dir/git-remote-iroh ($tag)"

case ":$PATH:" in
    *":$bin_dir:"*) ;;
    *) echo "note: $bin_dir is not on your PATH; add it so git can find the helper" ;;
esac
