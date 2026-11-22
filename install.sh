#!/bin/sh
# iptop installer: downloads the latest static release for this machine from
# GitHub, checks its SHA-256 and installs it.
#
#   curl -fsSL https://raw.githubusercontent.com/Michuu/iptop/main/install.sh | sh
#
# Environment:
#   IPTOP_VERSION=v0.1.3   install this version instead of the latest
#   IPTOP_PREFIX=DIR       install into DIR (default /usr/local/bin)
#   IPTOP_SETCAP=1         also grant cap_net_raw,cap_net_admin so iptop runs without sudo
#                          (lets every local user capture traffic; off by default)
set -eu

REPO="Michuu/iptop"
PREFIX="${IPTOP_PREFIX:-/usr/local/bin}"

say() { printf '%s\n' "iptop: $*"; }
die() { printf '%s\n' "iptop: error: $*" >&2; exit 1; }

[ "$(uname -s)" = "Linux" ] || die "only Linux is supported (iptop uses AF_PACKET), this is $(uname -s)"

# The binaries are static, so only the kernel's CPU type matters (a 32-bit
# Raspberry Pi OS on a 64-bit kernel reports aarch64 and runs the aarch64 build).
machine="${IPTOP_ARCH:-$(uname -m)}"
case "$machine" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    armv7* | armv8l) arch=armv7 ;;
    armv6*) arch=armv6 ;;
    riscv64) arch=riscv64 ;;
    *) die "no prebuilt binary for $machine; build from source: https://github.com/$REPO" ;;
esac

if command -v curl >/dev/null 2>&1; then
    fetch() { curl -fsSL "$1" -o "$2"; }
    fetch_stdout() { curl -fsSL "$1"; }
elif command -v wget >/dev/null 2>&1; then
    fetch() { wget -qO "$2" "$1"; }
    fetch_stdout() { wget -qO- "$1"; }
else
    die "need curl or wget"
fi

version="${IPTOP_VERSION:-}"
if [ -z "$version" ]; then
    version=$(fetch_stdout "https://api.github.com/repos/$REPO/releases/latest" |
        sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$version" ] || die "could not find the latest release (GitHub API rate limit?); set IPTOP_VERSION=vX.Y.Z"
fi

name="iptop-$version-$arch-linux"
base="https://github.com/$REPO/releases/download/$version"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading $name"
fetch "$base/$name.tar.gz" "$tmp/$name.tar.gz" || die "download failed: $base/$name.tar.gz"
fetch "$base/SHA256SUMS" "$tmp/SHA256SUMS" || die "download failed: $base/SHA256SUMS"

want=$(grep " $name.tar.gz\$" "$tmp/SHA256SUMS" | cut -d' ' -f1)
if command -v sha256sum >/dev/null 2>&1; then
    got=$(sha256sum "$tmp/$name.tar.gz" | cut -d' ' -f1)
elif command -v shasum >/dev/null 2>&1; then
    got=$(shasum -a 256 "$tmp/$name.tar.gz" | cut -d' ' -f1)
else
    got=""
    say "warning: no sha256sum/shasum, skipping the checksum"
fi
if [ -n "$got" ]; then
    [ -n "$want" ] && [ "$got" = "$want" ] || die "checksum mismatch for $name.tar.gz"
    say "checksum OK"
fi

tar -xzf "$tmp/$name.tar.gz" -C "$tmp"
"$tmp/$name/iptop" --version >/dev/null 2>&1 || die "the downloaded binary does not run on this machine"

# Use sudo only if the target directory (or, if it doesn't exist yet, its
# nearest existing parent) is not writable.
dir="$PREFIX"
while [ ! -e "$dir" ]; do dir=$(dirname "$dir"); done
sudo=""
if [ ! -w "$dir" ] && [ "$(id -u)" -ne 0 ]; then
    command -v sudo >/dev/null 2>&1 || die "$PREFIX is not writable and sudo is missing; set IPTOP_PREFIX"
    sudo="sudo"
    say "installing into $PREFIX (needs sudo)"
fi
$sudo mkdir -p "$PREFIX"
$sudo install -m 755 "$tmp/$name/iptop" "$PREFIX/iptop"
say "installed $("$PREFIX/iptop" --version) to $PREFIX/iptop"

if [ "${IPTOP_SETCAP:-0}" = "1" ]; then
    if command -v setcap >/dev/null 2>&1; then
        $sudo setcap cap_net_raw,cap_net_admin=eip "$PREFIX/iptop" && say "capabilities set: run iptop without sudo"
    else
        say "setcap not found (package libcap2-bin / libcap); run iptop with sudo"
    fi
else
    say "run it with: sudo iptop"
fi
say "optional GeoIP/ASN: put GeoLite2-City.mmdb and GeoLite2-ASN.mmdb in ~/.local/share/iptop/"
