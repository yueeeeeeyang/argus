#!/usr/bin/env bash
# 文件职责：重新生成来源树使用的 Material Icon Theme 子集（SVG 源 + 64px PNG）。
# 创建日期：2026-10-28
# 修改日期：2026-10-28
# 作者：Argus 开发团队
# 主要功能：按固定版本从 jsDelivr 下载选定图标，并生成运行时使用的透明 PNG。

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="$ROOT_DIR/assets/icons/material"
VERSION="5.38.1"
BASE_URL="https://cdn.jsdelivr.net/npm/material-icon-theme@$VERSION/icons"

if ! command -v rsvg-convert >/dev/null 2>&1; then
  echo "缺少 rsvg-convert，请先安装 librsvg（macOS: brew install librsvg）。" >&2
  exit 1
fi

IDS=(
  file folder-base folder-base-open
  log zip settings xml json yaml toml document markdown console jar table database lock
  changelog docker makefile git license readme maven go-mod
)

mkdir -p "$OUT_DIR"
for id in "${IDS[@]}"; do
  curl -fsSL --retry 3 -m 60 -o "$OUT_DIR/$id.svg" "$BASE_URL/$id.svg"
  rsvg-convert -w 64 -h 64 "$OUT_DIR/$id.svg" -o "$OUT_DIR/$id.png"
done

curl -fsSL -m 60 -o "$OUT_DIR/LICENSE" \
  "https://raw.githubusercontent.com/material-extensions/vscode-material-icon-theme/main/LICENSE"

echo "Material Icon Theme $VERSION：已生成 ${#IDS[@]} 个图标到 $OUT_DIR"
