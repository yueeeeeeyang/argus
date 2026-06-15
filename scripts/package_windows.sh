#!/usr/bin/env bash
# 文件职责：在类 Unix 环境中交叉构建 Windows Argus 包。
# 创建日期：2026-06-15
# 修改日期：2026-06-15
# 作者：Argus 开发团队
# 主要功能：使用 Rust Windows target 构建 argus.exe，并输出可分发 zip 包。

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="${WINDOWS_TARGET:-x86_64-pc-windows-msvc}"
DIST_DIR="$ROOT_DIR/dist/windows/Argus"
ZIP_PATH="$ROOT_DIR/dist/windows/Argus-${TARGET}.zip"

# Windows MSVC 目标在非 Windows 主机上需要额外的 C 头文件、链接器和 SDK。这里先给出清晰
# 提示，避免在 ring / GPUI 等依赖编译到一半时才暴露晦涩的工具链错误。
HOST_TRIPLE="$(rustc -vV | awk '/^host:/ { print $2 }')"
if [[ "$TARGET" == *"-windows-msvc" && "$HOST_TRIPLE" != *"-windows-msvc" && "${ARGUS_ALLOW_CROSS_WINDOWS:-0}" != "1" ]]; then
  cat >&2 <<EOF
当前主机为 ${HOST_TRIPLE}，不能直接打包 ${TARGET}。

推荐方式：
  1. 在 Windows 本机运行：powershell -ExecutionPolicy Bypass -File scripts/package_windows.ps1
  2. 或在当前机器配置完整 Windows 交叉编译工具链后，设置 ARGUS_ALLOW_CROSS_WINDOWS=1 再运行本脚本。

EOF
  exit 1
fi

if ! rustup target list --installed | grep -qx "$TARGET"; then
  cat >&2 <<EOF
缺少 Rust 目标：$TARGET
请先执行：rustup target add $TARGET
EOF
  exit 1
fi

if [[ ! -f "$ROOT_DIR/resources/windows/app.ico" ]]; then
  "$ROOT_DIR/scripts/generate_icons.sh" "$ROOT_DIR/assets/icons/app-icon.png"
fi

cd "$ROOT_DIR"
cargo build --release --target "$TARGET"

rm -rf "$DIST_DIR"
mkdir -p "$DIST_DIR"
cp "$ROOT_DIR/target/$TARGET/release/argus.exe" "$DIST_DIR/argus.exe"
cp "$ROOT_DIR/resources/windows/app.ico" "$DIST_DIR/Argus.ico"

python3 - "$DIST_DIR" "$ZIP_PATH" <<'PY'
import sys
import zipfile
from pathlib import Path

source = Path(sys.argv[1])
output = Path(sys.argv[2])
output.parent.mkdir(parents=True, exist_ok=True)
if output.exists():
    output.unlink()

with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED) as archive:
    for path in source.rglob("*"):
        if path.is_file():
            archive.write(path, path.relative_to(source.parent))
PY

echo "Windows 应用已打包：$DIST_DIR"
echo "Windows 压缩包已生成：$ZIP_PATH"
