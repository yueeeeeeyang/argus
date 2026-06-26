#!/usr/bin/env bash
# 文件职责：一键构建 Argus 当前平台升级包、生成 manifest、签名并可选上传到升级服务器。
# 创建日期：2026-06-26
# 修改日期：2026-06-26
# 作者：Argus 开发团队
# 主要功能：固定 Ed25519 验签公钥，输出客户端配置和升级包信息，减少手工发布出错概率。

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="$ROOT_DIR/dist/updates"
SERVER_URL=""
UPLOAD_TARGET=""
VERSION=""
RELEASE_NOTES=""
RELEASE_NOTES_FILE=""
PUBLISHED_AT="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
SKIP_BUILD="0"
ASSET_PATH=""
ASSET_URL=""
TARGET_OS=""
TARGET_ARCH=""

usage() {
  cat <<'EOF'
用法：
  scripts/generate_upgrade_package.sh --server-url <升级服务器URL> [选项]

常用选项：
  --server-url URL        升级服务器根地址，例如 https://updates.example.com/argus/
  --upload TARGET         可选，生成后用 rsync 上传，例如 deploy@host:/var/www/argus/
  --version VERSION       可选，默认读取 Cargo.toml package.version
  --notes TEXT            可选，升级日志文本
  --notes-file FILE       可选，从文件读取升级日志
  --out-dir DIR           可选，默认 dist/updates
  --asset FILE            可选，跳过构建，直接使用已有升级资产
  --asset-url URL         可选，manifest 中写入的资产 URL，默认使用 <os>-<arch>/<文件名>
  --os OS                 可选，覆盖平台标识：macos/windows/linux
  --arch ARCH             可选，覆盖架构标识：aarch64/x86_64/arm
  --skip-build            可选，跳过构建，必须配合 --asset
  -h, --help              显示帮助

必需环境变量：
  ARGUS_UPDATE_SIGNING_KEY_BASE64   32 字节 Ed25519 seed 的 Base64

可选环境变量：
  ARGUS_UPDATE_PUBLIC_KEY_BASE64    固定客户端验签公钥；设置后脚本会校验它和私钥匹配

生成新发布 seed 示例：
  openssl rand -base64 32

查看 seed 对应公钥：
  ARGUS_UPDATE_SIGNING_KEY_BASE64=... cargo run --quiet --bin argus_update_manifest_tool -- pub-key
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --server-url)
      SERVER_URL="${2:-}"
      shift 2
      ;;
    --upload)
      UPLOAD_TARGET="${2:-}"
      shift 2
      ;;
    --version)
      VERSION="${2:-}"
      shift 2
      ;;
    --notes)
      RELEASE_NOTES="${2:-}"
      shift 2
      ;;
    --notes-file)
      RELEASE_NOTES_FILE="${2:-}"
      shift 2
      ;;
    --out-dir)
      OUT_DIR="${2:-}"
      shift 2
      ;;
    --asset)
      ASSET_PATH="${2:-}"
      SKIP_BUILD="1"
      shift 2
      ;;
    --asset-url)
      ASSET_URL="${2:-}"
      shift 2
      ;;
    --os)
      TARGET_OS="${2:-}"
      shift 2
      ;;
    --arch)
      TARGET_ARCH="${2:-}"
      shift 2
      ;;
    --skip-build)
      SKIP_BUILD="1"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "未知参数：$1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if [[ -z "${ARGUS_UPDATE_SIGNING_KEY_BASE64:-}" ]]; then
  echo "错误：请先设置 ARGUS_UPDATE_SIGNING_KEY_BASE64。" >&2
  usage >&2
  exit 1
fi

detect_os() {
  case "$(uname -s)" in
    Darwin) echo "macos" ;;
    Linux) echo "linux" ;;
    MINGW*|MSYS*|CYGWIN*) echo "windows" ;;
    *) echo "unknown" ;;
  esac
}

detect_arch() {
  case "$(uname -m)" in
    arm64|aarch64) echo "aarch64" ;;
    x86_64|amd64) echo "x86_64" ;;
    arm*) echo "arm" ;;
    *) uname -m ;;
  esac
}

read_cargo_version() {
  python3 - "$ROOT_DIR/Cargo.toml" <<'PY'
import re
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text(encoding="utf-8")
match = re.search(r'(?m)^version\s*=\s*"([^"]+)"', text)
if not match:
    raise SystemExit("无法从 Cargo.toml 读取 package.version")
print(match.group(1))
PY
}

TARGET_OS="${TARGET_OS:-$(detect_os)}"
TARGET_ARCH="${TARGET_ARCH:-$(detect_arch)}"
VERSION="${VERSION:-$(read_cargo_version)}"
if [[ -n "$RELEASE_NOTES_FILE" ]]; then
  RELEASE_NOTES="$(cat "$RELEASE_NOTES_FILE")"
fi
if [[ -z "$RELEASE_NOTES" ]]; then
  RELEASE_NOTES="Argus ${VERSION} 发布"
fi
if [[ "$TARGET_OS" == "unknown" ]]; then
  echo "错误：无法识别当前平台，请用 --os 指定 macos/windows/linux。" >&2
  exit 1
fi
if [[ "$SKIP_BUILD" == "1" && -z "$ASSET_PATH" ]]; then
  echo "错误：--skip-build 需要配合 --asset 指定已有升级资产。" >&2
  exit 1
fi

STAGE_DIR="$OUT_DIR/$VERSION"
PLATFORM_DIR="${TARGET_OS}-${TARGET_ARCH}"
ASSET_DIR="$STAGE_DIR/$PLATFORM_DIR"
mkdir -p "$ASSET_DIR"

build_current_platform_asset() {
  case "$TARGET_OS" in
    macos)
      "$ROOT_DIR/scripts/package_macos.sh"
      local app_dir="$ROOT_DIR/dist/macos/Argus.app"
      local zip_path="$ASSET_DIR/Argus.app.zip"
      if [[ ! -d "$app_dir" ]]; then
        echo "错误：未找到 macOS 应用包：$app_dir" >&2
        exit 1
      fi
      /usr/bin/ditto -c -k --keepParent "$app_dir" "$zip_path"
      echo "$zip_path"
      ;;
    linux)
      (cd "$ROOT_DIR" && cargo build --release)
      local binary_path="$ASSET_DIR/argus"
      cp "$ROOT_DIR/target/release/argus" "$binary_path"
      chmod +x "$binary_path"
      echo "$binary_path"
      ;;
    windows)
      (cd "$ROOT_DIR" && cargo build --release)
      local exe_path="$ASSET_DIR/argus.exe"
      cp "$ROOT_DIR/target/release/argus.exe" "$exe_path"
      echo "$exe_path"
      ;;
    *)
      echo "错误：不支持的平台：$TARGET_OS" >&2
      exit 1
      ;;
  esac
}

if [[ -n "$ASSET_PATH" ]]; then
  if [[ ! -f "$ASSET_PATH" ]]; then
    echo "错误：升级资产不存在：$ASSET_PATH" >&2
    exit 1
  fi
  ASSET_NAME="$(basename "$ASSET_PATH")"
  FINAL_ASSET_PATH="$ASSET_DIR/$ASSET_NAME"
  cp "$ASSET_PATH" "$FINAL_ASSET_PATH"
else
  FINAL_ASSET_PATH="$(build_current_platform_asset)"
  ASSET_NAME="$(basename "$FINAL_ASSET_PATH")"
fi

RELATIVE_ASSET_URL="${ASSET_URL:-${PLATFORM_DIR}/${ASSET_NAME}}"
MANIFEST_PATH="$STAGE_DIR/manifest-v1.json"
SIGNATURE_PATH="$STAGE_DIR/manifest-v1.json.sig"
ASSET_META="$(python3 - "$FINAL_ASSET_PATH" <<'PY'
import hashlib
import os
import sys
from pathlib import Path

path = Path(sys.argv[1])
digest = hashlib.sha256()
with path.open("rb") as handle:
    for chunk in iter(lambda: handle.read(1024 * 1024), b""):
        digest.update(chunk)
print(f"{digest.hexdigest()} {os.path.getsize(path)}")
PY
)"
ASSET_SHA256="${ASSET_META%% *}"
ASSET_SIZE="${ASSET_META##* }"

python3 - "$MANIFEST_PATH" "$VERSION" "$RELEASE_NOTES" "$PUBLISHED_AT" "$TARGET_OS" "$TARGET_ARCH" "$RELATIVE_ASSET_URL" "$ASSET_SHA256" "$ASSET_SIZE" <<'PY'
import json
import sys
from pathlib import Path

manifest_path = Path(sys.argv[1])
manifest = {
    "version": sys.argv[2],
    "release_notes": sys.argv[3],
    "published_at": sys.argv[4],
    "assets": [
        {
            "os": sys.argv[5],
            "arch": sys.argv[6],
            "url": sys.argv[7],
            "sha256": sys.argv[8],
            "size_bytes": int(sys.argv[9]),
        }
    ],
}
manifest_path.parent.mkdir(parents=True, exist_ok=True)
manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
PY

SIGN_OUTPUT="$(
  cd "$ROOT_DIR"
  cargo run --quiet --bin argus_update_manifest_tool -- sign "$MANIFEST_PATH" "$SIGNATURE_PATH"
)"
PUBLIC_KEY_BASE64="$(printf "%s\n" "$SIGN_OUTPUT" | awk -F= '/^ARGUS_UPDATE_PUBLIC_KEY_BASE64=/ {print $2; exit}')"

if [[ -n "$UPLOAD_TARGET" ]]; then
  if ! command -v rsync >/dev/null 2>&1; then
    echo "错误：--upload 需要系统安装 rsync。" >&2
    exit 1
  fi
  rsync -av "$STAGE_DIR"/ "$UPLOAD_TARGET"
fi

cat <<EOF

升级包生成完成
版本：$VERSION
平台：$TARGET_OS/$TARGET_ARCH
发布时间：$PUBLISHED_AT
输出目录：$STAGE_DIR

资产：$FINAL_ASSET_PATH
资产 URL：$RELATIVE_ASSET_URL
SHA-256：$ASSET_SHA256
大小：$ASSET_SIZE bytes

Manifest：$MANIFEST_PATH
签名：$SIGNATURE_PATH
固定验签公钥：
ARGUS_UPDATE_PUBLIC_KEY_BASE64=$PUBLIC_KEY_BASE64
EOF

if [[ -n "$SERVER_URL" ]]; then
  cat <<EOF

客户端升级配置：
[upgrade]
enabled = true
server_url = "$SERVER_URL"
public_key_base64 = "$PUBLIC_KEY_BASE64"
EOF
fi

if [[ -n "$UPLOAD_TARGET" ]]; then
  echo
  echo "已上传到：$UPLOAD_TARGET"
else
  echo
  echo "未上传；如需上传请追加：--upload deploy@host:/var/www/argus/"
fi
