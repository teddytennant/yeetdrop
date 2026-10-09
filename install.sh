#!/bin/sh
# yeetdrop installer: fetch the release binary, then run `yeetdrop setup`.
#   curl -fsSL https://raw.githubusercontent.com/teddytennant/yeetdrop/main/install.sh | sh
# Env: YEETDROP_VERSION=v0.1.0  pin a release   YEETDROP_BIN=dir  install location
set -eu

REPO=teddytennant/yeetdrop
BIN=${YEETDROP_BIN:-$HOME/.local/bin}

[ "$(uname -s)" = Linux ] || { echo "yeetdrop only runs on Linux" >&2; exit 1; }
case "$(uname -m)" in
  x86_64|amd64)  arch=x86_64 ;;
  aarch64|arm64) arch=aarch64 ;;
  *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
esac

file="yeetdrop-$arch-linux.tar.gz"
if [ -n "${YEETDROP_VERSION:-}" ]; then
  base="https://github.com/$REPO/releases/download/$YEETDROP_VERSION"
else
  base="https://github.com/$REPO/releases/latest/download"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "downloading $file"
curl -fsSL "$base/$file" -o "$tmp/$file"
curl -fsSL "$base/$file.sha256" -o "$tmp/$file.sha256"
(cd "$tmp" && sha256sum -c "$file.sha256" >/dev/null) || { echo "checksum mismatch, not installing" >&2; exit 1; }

tar -xzf "$tmp/$file" -C "$tmp" yeetdrop
mkdir -p "$BIN"
install -m755 "$tmp/yeetdrop" "$BIN/yeetdrop"
echo "installed $BIN/yeetdrop"

case ":$PATH:" in
  *":$BIN:"*) ;;
  *) echo "note: $BIN is not on your PATH" ;;
esac

# stdin is the script when piped, so ask the terminal directly.
if ( : </dev/tty ) 2>/dev/null; then
  exec "$BIN/yeetdrop" setup </dev/tty
fi
echo "run: $BIN/yeetdrop setup"
