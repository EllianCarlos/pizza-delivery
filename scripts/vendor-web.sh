#!/bin/sh
set -eu
root=$(cd "$(dirname "$0")/.." && pwd)
dest="$root/crates/pizza-delivery/web-assets"
rm -rf "$dest"
mkdir -p "$dest"
for name in index.html app.js app.css; do
  cp "$root/web/dist/$name" "$dest/$name"
done
cp "$root"/web/public/* "$dest/"
