#!/bin/sh
# Optiman installer for macOS.
#   curl -fsSL https://tsubasatommy.github.io/optiman/install.sh | sh
# Options after `sh -s --` are passed to `optiman install`, e.g.
#   curl -fsSL .../install.sh | sh -s -- --folder ~/Pictures --new-only --original trash
set -eu

REPO="TsubasaTommy/optiman"
ASSET="optiman-macos-universal.tar.gz"
BASE="https://github.com/$REPO/releases/latest/download"
DEST="${OPTIMAN_INSTALL_DIR:-$HOME/.local/bin}"

if [ "$(uname -s)" != "Darwin" ]; then
  echo "このスクリプトは macOS 用です。Windows は install.ps1 を使ってください。" >&2
  exit 1
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "ダウンロード中: $ASSET"
curl -fsSL "$BASE/$ASSET" -o "$tmp/$ASSET"
curl -fsSL "$BASE/SHA256SUMS.txt" -o "$tmp/SHA256SUMS.txt"

expected=$(awk -v f="$ASSET" '$2 == f || $2 == "*"f { print $1 }' "$tmp/SHA256SUMS.txt")
actual=$(shasum -a 256 "$tmp/$ASSET" | awk '{ print $1 }')
if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
  echo "チェックサムが一致しません (期待値: ${expected:-なし}, 実際: $actual)" >&2
  exit 1
fi

mkdir -p "$DEST"
tar -xzf "$tmp/$ASSET" -C "$tmp"
if [ -x "$DEST/optiman" ]; then
  "$DEST/optiman" stop >/dev/null 2>&1 || true
fi
# Up to v0.2.0 the command was called optimize-image-folder. `optiman install` below takes over its
# settings and launchd agent, so the old binary only needs to stop and go.
if [ -x "$DEST/optimize-image-folder" ]; then
  "$DEST/optimize-image-folder" stop >/dev/null 2>&1 || true
  rm -f "$DEST/optimize-image-folder"
fi
install -m 755 "$tmp/optiman" "$DEST/optiman"
xattr -d com.apple.quarantine "$DEST/optiman" 2>/dev/null || true
echo "インストールしました: $DEST/optiman"

case ":$PATH:" in
  *":$DEST:"*) ;;
  *)
    echo ""
    echo "ヒント: $DEST が PATH に含まれていません。次を実行すると optiman コマンドが使えます:"
    echo "  echo 'export PATH=\"$DEST:\$PATH\"' >> ~/.zshrc && exec zsh"
    ;;
esac

echo ""
"$DEST/optiman" install "$@"
