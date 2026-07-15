#!/bin/sh
# roster installer — downloads the prebuilt binary for this platform from
# GitHub releases, verifies its checksum, and installs it.
#
#   curl -fsSL https://roster-dev.vercel.app/install.sh | sh
#
# That host is a rewrite in front of this file on main
# (website/next.config.mjs). It is a convenience, not the source: the URL it
# fetches works directly and is the fallback if the site is ever down —
#
#   curl -fsSL https://raw.githubusercontent.com/zidankazi/roster/main/install.sh | sh
#
# Options via environment variables:
#   ROSTER_VERSION  tag to install (default: the latest release, e.g. v0.2.0)
#   ROSTER_BINDIR   install directory (default: ~/.local/bin)

set -eu

REPO="zidankazi/roster"
BINDIR="${ROSTER_BINDIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*" >&2; }
fail() { say "install failed: $*"; exit 1; }

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"

os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Darwin) os="apple-darwin" ;;
  Linux) os="unknown-linux-gnu" ;;
  *) fail "unsupported OS: $os (prebuilt binaries cover macOS and Linux; try cargo install --git https://github.com/$REPO roster)" ;;
esac
case "$arch" in
  arm64 | aarch64) arch="aarch64" ;;
  x86_64 | amd64) arch="x86_64" ;;
  *) fail "unsupported architecture: $arch" ;;
esac
target="$arch-$os"

version="${ROSTER_VERSION:-}"
if [ -z "$version" ]; then
  # The latest-release URL redirects to .../tag/<version>.
  version=$(curl -fsSLI -o /dev/null -w '%{url_effective}' \
    "https://github.com/$REPO/releases/latest" | sed 's|.*/tag/||')
  [ -n "$version" ] || fail "could not determine the latest release"
fi

name="roster-$version-$target"
url="https://github.com/$REPO/releases/download/$version/$name.tar.gz"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

say "downloading roster $version for $target…"
curl -fsSL "$url" -o "$tmp/$name.tar.gz" || fail "download failed: $url"
curl -fsSL "$url.sha256" -o "$tmp/$name.tar.gz.sha256" || fail "checksum download failed"

expected=$(awk '{print $1}' "$tmp/$name.tar.gz.sha256")
if command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$tmp/$name.tar.gz" | awk '{print $1}')
else
  actual=$(sha256sum "$tmp/$name.tar.gz" | awk '{print $1}')
fi
[ "$expected" = "$actual" ] || fail "checksum mismatch (expected $expected, got $actual)"

tar xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$BINDIR"
install -m 755 "$tmp/$name/roster" "$BINDIR/roster"

say "installed $("$BINDIR/roster" --version) to $BINDIR/roster"
case ":$PATH:" in
  *":$BINDIR:"*) ;;
  *) say "note: $BINDIR is not on your PATH — add it, e.g.:"
     say "  export PATH=\"$BINDIR:\$PATH\"" ;;
esac
