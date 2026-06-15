#!/usr/bin/env bash
# 文件职责：构建并打包 macOS Argus.app。
# 创建日期：2026-06-15
# 修改日期：2026-06-15
# 作者：Argus 开发团队
# 主要功能：编译 release 二进制、生成应用图标、组装 .app，并执行本地 ad-hoc 签名。

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_NAME="Argus"
APP_DIR="$ROOT_DIR/dist/macos/${APP_NAME}.app"
CONTENTS_DIR="$APP_DIR/Contents"
MACOS_DIR="$CONTENTS_DIR/MacOS"
RESOURCES_DIR="$CONTENTS_DIR/Resources"

"$ROOT_DIR/scripts/generate_icons.sh" "$ROOT_DIR/assets/icons/app-icon.png"

cd "$ROOT_DIR"
cargo build --release

rm -rf "$APP_DIR"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"
cp "$ROOT_DIR/target/release/argus" "$MACOS_DIR/argus"
cp "$ROOT_DIR/resources/macos/Info.plist" "$CONTENTS_DIR/Info.plist"
cp "$ROOT_DIR/resources/macos/AppIcon.icns" "$RESOURCES_DIR/AppIcon.icns"
chmod +x "$MACOS_DIR/argus"

# 本地 ad-hoc 签名方便直接双击运行；失败时保留未签名 app，便于定位证书或权限问题。
if command -v codesign >/dev/null 2>&1; then
  if ! codesign --force --deep --sign - "$APP_DIR" >/dev/null 2>&1; then
    echo "警告：ad-hoc 签名失败，已保留未签名应用包。" >&2
  fi
fi

echo "macOS 应用已打包：$APP_DIR"
