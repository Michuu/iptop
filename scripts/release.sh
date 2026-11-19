#!/bin/sh
# Build static release tarballs into dist/.
# Needs: rustup targets below, cargo-zigbuild (cargo install cargo-zigbuild) and zig (pip install ziglang).
set -eu
cd "$(dirname "$0")/.."
VERSION="v$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
rm -rf dist && mkdir -p dist
# target triple : release name
for pair in \
    x86_64-unknown-linux-musl:x86_64-linux \
    aarch64-unknown-linux-musl:aarch64-linux \
    armv7-unknown-linux-musleabihf:armv7-linux
do
    triple=${pair%%:*}; name=${pair##*:}
    cargo zigbuild --release --target "$triple"
    d="iptop-$VERSION-$name"
    mkdir -p "dist/$d"
    cp "target/$triple/release/iptop" README.md LICENSE "dist/$d/"
    tar -C dist -czf "dist/$d.tar.gz" "$d"
    rm -rf "dist/$d"
done
(cd dist && sha256sum *.tar.gz > SHA256SUMS)
ls -la dist
