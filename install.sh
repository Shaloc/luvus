#!/bin/sh
# Luvus fork installer. Downloads a checksummed release from Shaloc/luvus.
#
#   curl -fsSL https://raw.githubusercontent.com/Shaloc/luvus/main/install.sh | sh
#
# Overrides:
#   LUVUS_VERSION=fork-1.0.99-<commit>   install a specific fork release tag
#   LUVUS_INSTALL_DIR=...  where to put the binary (default: ~/.local/bin)
set -eu

REPO="Shaloc/luvus"
BIN="luvus"

err() { printf 'error: %s\n' "$1" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

# ── pick a downloader ──
if have curl; then DL="curl -fsSL --connect-timeout 10 --max-time 60"; DLO="curl -fsSL --connect-timeout 10 --max-time 60 -o"
elif have wget; then DL="wget --timeout=60 --tries=1 -qO-"; DLO="wget --timeout=60 --tries=1 -qO"
else err "need curl or wget"; fi

# ── detect target triple ──
os=$(uname -s)
arch=$(uname -m)
case "$os" in
  Darwin)
    case "$arch" in
      arm64|aarch64) target="aarch64-apple-darwin" ;;
      *) err "unsupported macOS arch: $arch" ;;
    esac ;;
  Linux)
    case "$arch" in
      x86_64) target="x86_64-unknown-linux-musl" ;;
      *) err "unsupported Linux arch: $arch" ;;
    esac ;;
  *) err "no fork binary for OS: $os; build from source" ;;
esac

# ── resolve version ──
if [ -n "${LUVUS_VERSION:-}" ]; then
  tag="$LUVUS_VERSION"
else
  metadata=$($DL "https://api.github.com/repos/$REPO/releases/latest") || metadata=""
  tag=$(printf '%s\n' "$metadata" | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)
  # Anonymous API requests can be rate-limited while public release redirects
  # still work. Accept only a tag redirect belonging to this exact fork.
  if [ -z "$tag" ] && have curl; then
    latest=$(curl -fsSL --connect-timeout 10 --max-time 30 -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") || latest=""
    case "$latest" in "https://github.com/$REPO/releases/tag/"*) tag=${latest##*/} ;; esac
  fi
  [ -n "$tag" ] || err "could not find the latest fork release (set LUVUS_VERSION to a fork release tag)"
fi
case "$tag" in
  *[!a-zA-Z0-9._-]*|'' ) err "invalid release tag" ;;
  fork-*) ;;
  *) err "expected a fork release tag, got: $tag" ;;
esac

asset="$BIN-$target.tar.gz"
url="https://github.com/$REPO/releases/download/$tag/$asset"
printf 'Installing %s %s (%s)...\n' "$BIN" "$tag" "$target"

# ── download + extract ──
tmp=$(mktemp -d "${TMPDIR:-/tmp}/luvus-download.XXXXXX")
stage=""
cleanup() {
  # These paths are private directories created by mktemp, never user input.
  rm -rf "$tmp"
  if [ -n "$stage" ] && [ -f "$stage/$BIN.new" ]; then
    rm -f "$stage/$BIN.new"
  fi
}
trap cleanup EXIT
$DLO "$tmp/$asset" "$url" || err "download failed: $url"
$DLO "$tmp/$asset.sha256" "$url.sha256" || err "checksum download failed"
# Verify only this archive, not arbitrary filenames from the checksum file.
expected=$(awk 'NR == 1 {print $1}' "$tmp/$asset.sha256")
case "$expected" in *[!a-fA-F0-9]*|'') err "invalid SHA-256 checksum" ;; esac
[ "${#expected}" -eq 64 ] || err "invalid SHA-256 checksum length"
if have sha256sum; then actual=$(sha256sum "$tmp/$asset" | awk '{print $1}')
elif have shasum; then actual=$(shasum -a 256 "$tmp/$asset" | awk '{print $1}')
else err "need sha256sum or shasum"; fi
[ "$actual" = "$expected" ] || err "SHA-256 mismatch; installation unchanged"
# Release archives contain one versioned directory. Extract only its binary;
# no archive path is allowed to become an installation destination.
member=$(tar -tzf "$tmp/$asset" | awk '/^luvus-[a-zA-Z0-9._-]+\/luvus$/ {print}')
[ -n "$member" ] || err "archive did not contain '$BIN'"
case "$member" in *'
'*) err "archive contains multiple binaries" ;; esac
tar -xOzf "$tmp/$asset" "$member" > "$tmp/$BIN" || err "extract failed"
[ -s "$tmp/$BIN" ] || err "archive contained an empty binary"
chmod 755 "$tmp/$BIN"

# ── choose an install dir on PATH ──
if [ -n "${LUVUS_INSTALL_DIR:-}" ]; then
  dir="$LUVUS_INSTALL_DIR"
else
  dir="$HOME/.local/bin"
fi
mkdir -p "$dir"
[ -w "$dir" ] || err "cannot write to $dir (set LUVUS_INSTALL_DIR to a writable dir)"
[ ! -L "$dir/$BIN" ] || err "refusing to replace a symlink: $dir/$BIN"
if [ -e "$dir/$BIN" ]; then
  [ -f "$dir/$BIN" ] || err "not a regular file: $dir/$BIN"
fi
stage=$(mktemp -d "$dir/.luvus-update.XXXXXX")
cp "$tmp/$BIN" "$stage/$BIN.new"
chmod 755 "$stage/$BIN.new"
version=$("$stage/$BIN.new" --version --remote-session-protocol) || err "downloaded binary cannot run"
printf '%s\n' "$version"
case "$version" in
  "luvus "*" remote-session=2 transport=9"|"luvus "*" remote-session=2 transport=10") ;;
  *) err "downloaded binary is not a compatible modified remote-session build" ;;
esac
if [ -f "$dir/$BIN" ]; then
  cp -p "$dir/$BIN" "$stage/$BIN.previous"
  printf 'Previous binary saved to %s/%s.previous\n' "$stage" "$BIN"
fi
# Same-filesystem rename keeps running servers on their existing executable.
mv -f "$stage/$BIN.new" "$dir/$BIN"

printf '\nInstalled to %s/%s. No servers were restarted.\n' "$dir" "$BIN"
case ":$PATH:" in
  *":$dir:"*) printf 'Run: %s\n' "$BIN" ;;
  *) printf 'Add to PATH:  export PATH="%s:$PATH"\n' "$dir" ;;
esac
