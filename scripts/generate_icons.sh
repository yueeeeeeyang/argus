#!/usr/bin/env bash
# 文件职责：从源 PNG 生成 Argus 打包所需图标资源。
# 创建日期：2026-06-15
# 修改日期：2026-06-15
# 作者：Argus 开发团队
# 主要功能：生成 macOS AppIcon.icns 和 Windows app.ico，供平台打包脚本复用。

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ $# -gt 0 ]]; then
  SOURCE_ICON="$1"
else
  SOURCE_ICON="$ROOT_DIR/assets/icons/app-icon.png"
fi

MACOS_ICON="$ROOT_DIR/resources/macos/AppIcon.icns"
WINDOWS_ICON="$ROOT_DIR/resources/windows/app.ico"
ICON_WORK_DIR="$ROOT_DIR/target/package/icons"
ICONSET_DIR="$ICON_WORK_DIR/AppIcon.iconset"
ICO_PNG_DIR="$ICON_WORK_DIR/ico-png"

if [[ ! -f "$SOURCE_ICON" ]]; then
  echo "源图标不存在：$SOURCE_ICON" >&2
  exit 1
fi

if ! command -v sips >/dev/null 2>&1; then
  echo "缺少 sips，无法从 PNG 生成多尺寸图标。" >&2
  exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "缺少 python3，无法生成 macOS .icns 和 Windows .ico。" >&2
  exit 1
fi

rm -rf "$ICONSET_DIR" "$ICO_PNG_DIR"
mkdir -p "$ICONSET_DIR" "$ICO_PNG_DIR" "$(dirname "$MACOS_ICON")" "$(dirname "$WINDOWS_ICON")"

# macOS iconset 要求固定命名；每个尺寸都从原图重新采样，避免连续缩放造成模糊。
sips -s format png -z 16 16 "$SOURCE_ICON" --out "$ICONSET_DIR/icon_16x16.png" >/dev/null
sips -s format png -z 32 32 "$SOURCE_ICON" --out "$ICONSET_DIR/icon_16x16@2x.png" >/dev/null
sips -s format png -z 32 32 "$SOURCE_ICON" --out "$ICONSET_DIR/icon_32x32.png" >/dev/null
sips -s format png -z 64 64 "$SOURCE_ICON" --out "$ICONSET_DIR/icon_32x32@2x.png" >/dev/null
sips -s format png -z 128 128 "$SOURCE_ICON" --out "$ICONSET_DIR/icon_128x128.png" >/dev/null
sips -s format png -z 256 256 "$SOURCE_ICON" --out "$ICONSET_DIR/icon_128x128@2x.png" >/dev/null
sips -s format png -z 256 256 "$SOURCE_ICON" --out "$ICONSET_DIR/icon_256x256.png" >/dev/null
sips -s format png -z 512 512 "$SOURCE_ICON" --out "$ICONSET_DIR/icon_256x256@2x.png" >/dev/null
sips -s format png -z 512 512 "$SOURCE_ICON" --out "$ICONSET_DIR/icon_512x512.png" >/dev/null
sips -s format png -z 1024 1024 "$SOURCE_ICON" --out "$ICONSET_DIR/icon_512x512@2x.png" >/dev/null

# sips 可能继承 com.apple.provenance 等扩展属性，某些 macOS 版本的 iconutil 会因此判定 iconset 无效。
if command -v xattr >/dev/null 2>&1; then
  xattr -cr "$ICONSET_DIR" "$ICO_PNG_DIR" >/dev/null 2>&1 || true
fi

# iconutil 对部分 RGB PNG 容错较差；统一转换为带 alpha 的 RGBA PNG。
python3 - "$ICONSET_DIR" <<'PY'
import binascii
import struct
import sys
import zlib
from pathlib import Path

PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"

def read_chunks(data):
    """按 PNG chunk 顺序迭代二进制数据，返回 chunk 类型与负载内容。"""
    offset = len(PNG_SIGNATURE)
    while offset < len(data):
        length = struct.unpack(">I", data[offset:offset + 4])[0]
        kind = data[offset + 4:offset + 8]
        body = data[offset + 8:offset + 8 + length]
        yield kind, body
        offset += 12 + length

def paeth(left, up, up_left):
    """实现 PNG Paeth 滤波预测器，用于还原被滤波的 RGB 行数据。"""
    value = left + up - up_left
    left_distance = abs(value - left)
    up_distance = abs(value - up)
    up_left_distance = abs(value - up_left)
    if left_distance <= up_distance and left_distance <= up_left_distance:
        return left
    if up_distance <= up_left_distance:
        return up
    return up_left

def unfilter_row(filter_type, row, previous, bytes_per_pixel):
    """根据 PNG filter 类型还原单行像素，参数包含当前行、上一行和单像素字节数。"""
    row = bytearray(row)
    for index in range(len(row)):
        left = row[index - bytes_per_pixel] if index >= bytes_per_pixel else 0
        up = previous[index] if previous else 0
        up_left = previous[index - bytes_per_pixel] if previous and index >= bytes_per_pixel else 0
        if filter_type == 1:
            row[index] = (row[index] + left) & 0xFF
        elif filter_type == 2:
            row[index] = (row[index] + up) & 0xFF
        elif filter_type == 3:
            row[index] = (row[index] + ((left + up) // 2)) & 0xFF
        elif filter_type == 4:
            row[index] = (row[index] + paeth(left, up, up_left)) & 0xFF
        elif filter_type != 0:
            raise ValueError(f"不支持的 PNG filter：{filter_type}")
    return bytes(row)

def png_chunk(kind, body):
    """按 PNG 规范封装 chunk，自动计算长度和 CRC 校验值。"""
    return (
        struct.pack(">I", len(body))
        + kind
        + body
        + struct.pack(">I", binascii.crc32(kind + body) & 0xFFFFFFFF)
    )

def convert_rgb_png_to_rgba(path):
    """将 sips 生成的 8-bit RGB PNG 转换为 RGBA PNG，提升 ICNS/ICO 兼容性。"""
    data = path.read_bytes()
    if not data.startswith(PNG_SIGNATURE):
        raise ValueError(f"{path} 不是 PNG 文件")

    ihdr = None
    idat_parts = []
    for kind, body in read_chunks(data):
        if kind == b"IHDR":
            ihdr = body
        elif kind == b"IDAT":
            idat_parts.append(body)

    width, height, bit_depth, color_type, compression, filter_method, interlace = struct.unpack(">IIBBBBB", ihdr)
    if color_type == 6:
        return
    if bit_depth != 8 or color_type != 2 or compression != 0 or filter_method != 0 or interlace != 0:
        raise ValueError(f"{path} 不是可转换的 8-bit RGB PNG")

    raw = zlib.decompress(b"".join(idat_parts))
    bytes_per_pixel = 3
    input_stride = width * bytes_per_pixel
    offset = 0
    previous = None
    rgba_rows = bytearray()
    for _ in range(height):
        filter_type = raw[offset]
        offset += 1
        row = unfilter_row(filter_type, raw[offset:offset + input_stride], previous, bytes_per_pixel)
        offset += input_stride
        rgba_rows.append(0)
        for pixel in range(0, len(row), 3):
            rgba_rows.extend(row[pixel:pixel + 3])
            rgba_rows.append(255)
        previous = row

    rgba_ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, compression, filter_method, interlace)
    path.write_bytes(
        PNG_SIGNATURE
        + png_chunk(b"IHDR", rgba_ihdr)
        + png_chunk(b"IDAT", zlib.compress(bytes(rgba_rows), 9))
        + png_chunk(b"IEND", b"")
    )

for png_path in Path(sys.argv[1]).glob("*.png"):
    convert_rgb_png_to_rgba(png_path)
PY

# 直接写入 ICNS 容器，避免 iconutil 在不同 macOS 环境下对 iconset 校验差异导致打包失败。
python3 - "$MACOS_ICON" \
  "icp4=$ICONSET_DIR/icon_16x16.png" \
  "icp5=$ICONSET_DIR/icon_32x32.png" \
  "icp6=$ICONSET_DIR/icon_32x32@2x.png" \
  "ic07=$ICONSET_DIR/icon_128x128.png" \
  "ic08=$ICONSET_DIR/icon_256x256.png" \
  "ic09=$ICONSET_DIR/icon_512x512.png" \
  "ic10=$ICONSET_DIR/icon_512x512@2x.png" <<'PY'
import struct
import sys
from pathlib import Path

entries = []
for item in sys.argv[2:]:
    kind, path = item.split("=", 1)
    data = Path(path).read_bytes()
    entries.append(kind.encode("ascii") + struct.pack(">I", len(data) + 8) + data)

payload = b"".join(entries)
Path(sys.argv[1]).write_bytes(b"icns" + struct.pack(">I", len(payload) + 8) + payload)
PY

# ICO 使用 PNG 帧，Windows Vista 及以上可直接读取 PNG 压缩图标，文件更小且质量稳定。
ICO_SIZES=(16 24 32 48 64 128 256)
ICO_FILES=()
for size in "${ICO_SIZES[@]}"; do
  output="$ICO_PNG_DIR/icon_${size}.png"
  sips -s format png -z "$size" "$size" "$SOURCE_ICON" --out "$output" >/dev/null
  ICO_FILES+=("$output")
done

python3 - "$WINDOWS_ICON" "${ICO_FILES[@]}" <<'PY'
import struct
import sys
from pathlib import Path

output = Path(sys.argv[1])
images = []
for name in sys.argv[2:]:
    path = Path(name)
    size = int(path.stem.split("_")[-1])
    data = path.read_bytes()
    images.append((size, data))

header_size = 6 + 16 * len(images)
offset = header_size
directory = bytearray()
payload = bytearray()

for size, data in images:
    width = 0 if size >= 256 else size
    height = 0 if size >= 256 else size
    directory.extend(struct.pack("<BBBBHHII", width, height, 0, 0, 1, 32, len(data), offset))
    payload.extend(data)
    offset += len(data)

output.write_bytes(struct.pack("<HHH", 0, 1, len(images)) + directory + payload)
PY

echo "已生成 macOS 图标：$MACOS_ICON"
echo "已生成 Windows 图标：$WINDOWS_ICON"
