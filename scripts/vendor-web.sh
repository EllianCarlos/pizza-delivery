#!/bin/sh
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
dest="$root/crates/pizza-delivery/web-assets"
rm -rf "$dest"
mkdir -p "$dest"
# The whole dist: hashed bundle, index, the rewritten sw.js and the .br/.gz variants.
cp "$root"/web/dist/* "$dest/"
for name in "$root"/web/public/*; do
  [ -e "$dest/$(basename "$name")" ] || cp "$name" "$dest/"
done
