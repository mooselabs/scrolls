#!/bin/sh
set -eu
# Private, pinned test dependency. Does not replace a shared bd installation.
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
case "$(uname -s)-$(uname -m)" in Linux-x86_64) ;; *) echo 'Only Linux x86_64 is qualified' >&2; exit 1;; esac
mkdir -p "$root/.tools"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
curl -fL --retry 3 https://github.com/gastownhall/beads/releases/download/v1.3.0/beads_1.3.0_linux_amd64.tar.gz -o "$tmp/beads.tar.gz"
printf '%s  %s\n' 2f92b904ecf35b607e44dc5c39229173af69c54f1183e8d709f1773540cdcf3b "$tmp/beads.tar.gz" | sha256sum -c -
tar -xzf "$tmp/beads.tar.gz" -C "$tmp"
install -m 755 "$tmp/bd" "$root/.tools/bd"
"$root/.tools/bd" version
