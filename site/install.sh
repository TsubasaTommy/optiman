#!/bin/sh
# optimize-image-folder installer for macOS.
#   curl -fsSL https://tsubasatommy.github.io/optimize-image-folder/install.sh | sh
# Options after `sh -s --` are passed to `optimize-image-folder install`, e.g.
#   curl -fsSL .../install.sh | sh -s -- --folder ~/Pictures --new-only --original trash
set -eu

REPO="TsubasaTommy/optimize-image-folder"
ASSET="optimize-image-folder-macos-universal.tar.gz"
BASE="https://github.com/$REPO/releases/latest/download"
DEST="${OIF_INSTALL_DIR:-$HOME/.local/bin}"

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
if [ -x "$DEST/optimize-image-folder" ]; then
  "$DEST/optimize-image-folder" stop >/dev/null 2>&1 || true
fi
install -m 755 "$tmp/optimize-image-folder" "$DEST/optimize-image-folder"
xattr -d com.apple.quarantine "$DEST/optimize-image-folder" 2>/dev/null || true
echo "インストールしました: $DEST/optimize-image-folder"

case ":$PATH:" in
  *":$DEST:"*) ;;
  *)
    echo ""
    echo "ヒント: $DEST が PATH に含まれていません。次を実行すると optimize-image-folder コマンドが使えます:"
    echo "  echo 'export PATH=\"$DEST:\$PATH\"' >> ~/.zshrc && exec zsh"
    ;;
esac

echo ""
"$DEST/optimize-image-folder" install "$@"
