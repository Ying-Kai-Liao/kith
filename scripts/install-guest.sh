#!/bin/sh
# Installs the client-only kith on a Linux VM (a cloud bot's computer) from a GitHub Release. No git, Rust, root or sudo:
#
#   curl -fsSL https://raw.githubusercontent.com/Ying-Kai-Liao/kith/main/scripts/install-guest.sh | sh
#
# Puts one static binary at ~/.local/bin/kith; then `kith join <link>`. Run it again to upgrade. Needs sh, tar and curl (or wget),
# and minisign or OpenSSL 3 to check the release's signature.
# The release's SHA256SUMS is signed in CI (SHA256SUMS.minisig) with the key whose public half is KITH_PUBKEY below (the
# same as packaging/minisign.pub), so a release whose files and checksums were both swapped is refused.
#
# Environment:
#   KITH_VERSION          a release to install (1.2.3 or v1.2.3); default the latest
#   KITH_GUEST_PREFIX     where to install (bin/kith goes under it); default ~/.local. Given explicitly, it also becomes the bot's
#                         home: <prefix>/.kith holds device.key and guest.json, and kith finds it from its own path in any
#                         shell (for a VM that only keeps one folder, e.g. /workspace). KITH_GUEST_HOME still wins.
#   KITH_REPO             GitHub repo to download from; default Ying-Kai-Liao/kith
#   KITH_ALLOW_UNSIGNED   1 installs a release with no signature (or no tool to check it) on the checksum alone
#   KITH_INSTALL_TARBALL  a local kith-guest-<version>-linux-<arch>.tar.gz to install instead of downloading (tests,
#                         offline); verified against KITH_INSTALL_SHA256 or the .sha256 file beside it, if there is one
set -eu

REPO=${KITH_REPO:-Ying-Kai-Liao/kith}
PREFIX=${KITH_GUEST_PREFIX:-$HOME/.local}
KITH_PUBKEY=RWQhm3bB3gz4f+YdOeWnGIzSXYL09VomfCiTphj+KFfDr1HQE2l4Ufdi

say() { printf '%s\n' "$*"; }
die() { printf 'install-guest: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

[ "$(uname -s)" = Linux ] || die "this installs the Linux client; on a Mac use https://raw.githubusercontent.com/$REPO/main/scripts/install.sh"
case $(uname -m) in
  x86_64|amd64) ARCH=x86_64 ;;
  aarch64|arm64) ARCH=aarch64 ;;
  *) die "unsupported CPU: $(uname -m)" ;;
esac
have tar || die "tar is needed"

if have sha256sum; then sha256_of() { sha256sum "$1" | cut -d' ' -f1; }
elif have shasum; then sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
else die "sha256sum (or shasum) is needed to verify the download"; fi

# fetch <url> <file>: curl, else wget.
if have curl; then fetch() { curl -fsSL -o "$2" "$1"; }
elif have wget; then fetch() { wget -q -O "$2" "$1"; }
else die "curl (or wget) is needed"; fi

WORK=$(mktemp -d "${TMPDIR:-/tmp}/kith-install.XXXXXX")
trap 'rm -rf "$WORK"' EXIT

# b64bytes <base64> <skip> <count> <out>: bytes skip+1..skip+count of the decoded base64.
b64bytes() { printf '%s' "$1" | base64 -d | tail -c +$(($2 + 1)) | head -c "$3" > "$4"; }

# verify_sig <file> <file.minisig>: checks a minisign signature against KITH_PUBKEY, with minisign or else OpenSSL 3
# (a minisign signature is Ed25519 over the file's BLAKE2b-512, or over the file itself for the legacy "Ed" kind).
# Returns 1 for a bad signature and 2 when there is no tool to check it.
verify_sig() {
  if have minisign; then
    minisign -Vq -P "$KITH_PUBKEY" -m "$1" -x "$2" >/dev/null 2>&1 || return 1
    return 0
  fi
  have openssl && openssl version 2>/dev/null | grep -q '^OpenSSL [3-9]' || return 2
  d=$WORK/sig
  mkdir -p "$d"
  sig=$(sed -n 2p "$2")
  b64bytes "$sig" 0 2 "$d/alg"
  b64bytes "$sig" 2 8 "$d/keyid.sig"
  b64bytes "$sig" 10 64 "$d/sig"
  b64bytes "$KITH_PUBKEY" 2 8 "$d/keyid.pub"
  b64bytes "$KITH_PUBKEY" 10 32 "$d/pk"
  cmp -s "$d/keyid.sig" "$d/keyid.pub" || return 1
  # The Ed25519 key as a DER SubjectPublicKeyInfo: the fixed 12-byte prefix, then the 32 key bytes.
  { printf '\060\052\060\005\006\003\053\145\160\003\041\000'; cat "$d/pk"; } > "$d/pk.der"
  case $(cat "$d/alg") in
    ED) openssl dgst -blake2b512 -binary "$1" > "$d/msg" ;;
    Ed) cp "$1" "$d/msg" ;;
    *) return 1 ;;
  esac
  openssl pkeyutl -verify -pubin -keyform DER -inkey "$d/pk.der" -rawin -in "$d/msg" -sigfile "$d/sig" >/dev/null 2>&1
}

if [ -n "${KITH_INSTALL_TARBALL:-}" ]; then
  TARBALL=$KITH_INSTALL_TARBALL
  [ -f "$TARBALL" ] || die "$TARBALL not found"
  WANT=${KITH_INSTALL_SHA256:-}
  [ -n "$WANT" ] || { [ -f "$TARBALL.sha256" ] && WANT=$(cut -d' ' -f1 < "$TARBALL.sha256"); } || true
else
  VERSION=${KITH_VERSION:-}
  if [ -z "$VERSION" ]; then
    # /releases/latest redirects to /releases/tag/vX.Y.Z.
    if have curl; then
      url=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest") || die "could not look up the latest release of $REPO"
    else
      url=$(wget -q --max-redirect=0 -S -O /dev/null "https://github.com/$REPO/releases/latest" 2>&1 | sed -n 's/^ *[Ll]ocation: *//p' | tr -d '\r' | head -1) || url=
    fi
    VERSION=${url##*/}
    case $VERSION in v[0-9]*) ;; *) die "could not find the latest release of $REPO (none yet?); set KITH_VERSION" ;; esac
  fi
  VERSION=${VERSION#v}
  ASSET=kith-guest-$VERSION-linux-$ARCH.tar.gz
  BASE=https://github.com/$REPO/releases/download/v$VERSION
  say "downloading kith $VERSION (linux $ARCH)..."
  TARBALL=$WORK/$ASSET
  fetch "$BASE/$ASSET" "$TARBALL" || die "download failed: $BASE/$ASSET"
  fetch "$BASE/SHA256SUMS" "$WORK/SHA256SUMS" || die "download failed: $BASE/SHA256SUMS"
  unsigned() {
    [ "${KITH_ALLOW_UNSIGNED:-}" = 1 ] || die "$1; not installing (KITH_ALLOW_UNSIGNED=1 installs it on the checksum alone)"
    say "warning: $1; installing on the checksum alone (KITH_ALLOW_UNSIGNED=1)"
  }
  if fetch "$BASE/SHA256SUMS.minisig" "$WORK/SHA256SUMS.minisig"; then
    rc=0; verify_sig "$WORK/SHA256SUMS" "$WORK/SHA256SUMS.minisig" || rc=$?
    case $rc in
      0) say "signature ok" ;;
      2) unsigned "minisign or OpenSSL 3 is needed to check the release signature" ;;
      *) die "SHA256SUMS of v$VERSION is not signed by the kith release key; not installing" ;;
    esac
  else
    unsigned "release v$VERSION has no signature ($BASE/SHA256SUMS.minisig)"
  fi
  WANT=$(awk -v f="$ASSET" '$2 == f || $2 == "*" f { print $1 }' "$WORK/SHA256SUMS")
  [ -n "$WANT" ] || die "no checksum for $ASSET in SHA256SUMS"
fi

if [ -n "${WANT:-}" ]; then
  [ "$(sha256_of "$TARBALL")" = "$WANT" ] || die "checksum mismatch for $TARBALL; not installing"
  say "checksum ok"
fi

mkdir -p "$WORK/x"
tar -C "$WORK/x" -xzf "$TARBALL"
SRC=$(echo "$WORK"/x/kith-guest-*)
[ -f "$SRC/bin/kith" ] || die "$TARBALL is not a kith client release"

# A new file moved into place by rename, so a running kith is never half-written.
mkdir -p "$PREFIX/bin"
cp "$SRC/bin/kith" "$PREFIX/bin/.kith.new.$$" && chmod 755 "$PREFIX/bin/.kith.new.$$" && mv -f "$PREFIX/bin/.kith.new.$$" "$PREFIX/bin/kith"
# An explicit prefix is the bot's home too: the folder next to bin/ that kith looks for. Chosen once here, so an upgrade
# keeps it; an existing bot at ~/.config/kith is left where it is, and the default prefix changes nothing.
if [ -n "${KITH_GUEST_PREFIX:-}" ] && [ ! -e "$PREFIX/.kith" ] && [ ! -f "${XDG_CONFIG_HOME:-$HOME/.config}/kith/device.key" ]; then
  (umask 077; mkdir -p "$PREFIX/.kith")
fi
say "installed kith to $PREFIX/bin/kith"
case ":$PATH:" in
  *":$PREFIX/bin:"*) ;;
  *) say "add it to your PATH:  export PATH=\"$PREFIX/bin:\$PATH\"" ;;
esac
say "next: kith join <link>"
