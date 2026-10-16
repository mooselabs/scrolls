#!/bin/sh
set -eu
# Private install. Never modifies PATH or a shared Dolt installation.
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
os=$(uname -s | tr '[:upper:]' '[:lower:]')
arch=$(uname -m)
case "$arch" in x86_64) arch=amd64;; aarch64|arm64) arch=arm64;; *) echo "Unsupported architecture: $arch" >&2; exit 1;; esac
# This release is qualified on Linux amd64; other targets require a recorded digest.
case "$os-$arch" in linux-amd64) checksum=1f7ad8c2622995789420a3fb0f2d16b4aa7430000a825dd91d5938f36480cbf6;; *) echo 'Only linux-amd64 is qualified in v1' >&2; exit 1;; esac
mkdir -p "$root/.tools"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
curl -fL --retry 3 "https://github.com/dolthub/dolt/releases/download/v2.2.0/dolt-$os-$arch.tar.gz" -o "$tmp/dolt.tar.gz"
printf '%s  %s\n' "$checksum" "$tmp/dolt.tar.gz" | sha256sum -c -
tar -xzf "$tmp/dolt.tar.gz" -C "$tmp"
install -m 755 "$tmp/dolt-$os-$arch/bin/dolt" "$root/.tools/dolt"
"$root/.tools/dolt" version
