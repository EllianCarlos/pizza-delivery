#!/bin/sh
set -eu

repo="EllianCarlos/pizza-delivery"
version="${PIZZA_DELIVERY_VERSION:-latest}"
prefix="${PIZZA_DELIVERY_PREFIX:-$HOME/.local}"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target="x86_64-unknown-linux-gnu" ;;
  Linux-aarch64 | Linux-arm64) target="aarch64-unknown-linux-gnu" ;;
  Darwin-x86_64) target="x86_64-apple-darwin" ;;
  Darwin-arm64) target="aarch64-apple-darwin" ;;
  *) echo "unsupported platform: $(uname -s) $(uname -m)" >&2; exit 1 ;;
esac

if [ "$version" = "latest" ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/$version"
fi

name="pizza-delivery-$target"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -fsSL "$base/$name.tar.gz" -o "$tmp/$name.tar.gz"
curl -fsSL "$base/$name.tar.gz.sha256" -o "$tmp/$name.tar.gz.sha256"

expected="$(cut -d' ' -f1 "$tmp/$name.tar.gz.sha256")"
if command -v sha256sum >/dev/null 2>&1; then
  actual="$(sha256sum "$tmp/$name.tar.gz" | cut -d' ' -f1)"
else
  actual="$(shasum -a 256 "$tmp/$name.tar.gz" | cut -d' ' -f1)"
fi
if [ "$expected" != "$actual" ]; then
  echo "checksum mismatch for $name.tar.gz" >&2
  exit 1
fi

tar xzf "$tmp/$name.tar.gz" -C "$tmp"
mkdir -p "$prefix/bin" \
  "$prefix/share/bash-completion/completions" \
  "$prefix/share/zsh/site-functions" \
  "$prefix/share/fish/vendor_completions.d"
install -m 755 "$tmp/$name/pizza-delivery" "$prefix/bin/pizza-delivery"
install -m 644 "$tmp/$name/completions/pizza-delivery.bash" "$prefix/share/bash-completion/completions/pizza-delivery"
install -m 644 "$tmp/$name/completions/pizza-delivery.zsh" "$prefix/share/zsh/site-functions/_pizza-delivery"
install -m 644 "$tmp/$name/completions/pizza-delivery.fish" "$prefix/share/fish/vendor_completions.d/pizza-delivery.fish"

echo "installed pizza-delivery to $prefix/bin"
