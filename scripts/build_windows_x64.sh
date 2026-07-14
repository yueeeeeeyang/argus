#!/usr/bin/env bash
# 文件职责：在 macOS 或 Linux 主机上交叉编译 Windows x64 版 Argus 可执行文件。
# 创建日期：2026-07-14
# 修改日期：2026-07-14
# 作者：Argus 开发团队
# 主要功能：检查 Rust 与 cargo-xwin 工具链，使用 Windows MSVC 目标生成 64 位 argus.exe。

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WINDOWS_TARGET="x86_64-pc-windows-msvc"
WINDOWS_ICON="$ROOT_DIR/resources/windows/app.ico"
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT_DIR/target}"

# Cargo 将相对形式的 CARGO_TARGET_DIR 解释为相对于当前工作目录。脚本会先切换到项目根目录，
# 因此这里提前转为绝对路径，确保后续产物校验与 Cargo 的实际输出位置保持一致。
if [[ "$TARGET_DIR" != /* ]]; then
  TARGET_DIR="$ROOT_DIR/$TARGET_DIR"
fi

WINDOWS_EXECUTABLE="$TARGET_DIR/$WINDOWS_TARGET/release/argus.exe"

# 检查指定命令是否可用。
# 参数说明：第一个参数为命令名，第二个参数为缺失时显示的安装提示。
# 返回值：命令存在时返回 0；命令缺失时输出错误并退出脚本。
require_command() {
  local command_name="$1"
  local install_hint="$2"

  if ! command -v "$command_name" >/dev/null 2>&1; then
    echo "缺少命令：$command_name" >&2
    echo "$install_hint" >&2
    exit 1
  fi
}

# 校验当前系统是否适合使用 cargo-xwin 进行交叉编译。
# 参数说明：无。
# 返回值：macOS/Linux 返回 0；其他系统输出替代构建方式并退出脚本。
validate_host_platform() {
  local host_system
  host_system="$(uname -s)"

  case "$host_system" in
    Darwin | Linux)
      ;;
    *)
      echo "当前系统 ${host_system} 不支持此交叉编译脚本。" >&2
      echo "Windows 主机请运行 scripts/package_windows.ps1。" >&2
      exit 1
      ;;
  esac
}

# 校验固定的 Windows x64 Rust 标准库是否已安装。
# 参数说明：无。
# 返回值：目标已安装时返回 0；缺失时输出安装命令并退出脚本。
validate_rust_target() {
  if ! rustup target list --installed | grep -qx "$WINDOWS_TARGET"; then
    echo "缺少 Rust 目标：$WINDOWS_TARGET" >&2
    echo "请先执行：rustup target add $WINDOWS_TARGET" >&2
    exit 1
  fi
}

# 校验 cargo-xwin 子命令是否可用。cargo-xwin 会提供 Windows SDK、CRT 与链接器配置，
# 使 macOS/Linux 无需安装 Visual Studio 即可构建 MSVC ABI 的 Windows 可执行文件。
# 参数说明：无。
# 返回值：cargo-xwin 可用时返回 0；缺失时输出安装命令并退出脚本。
validate_cargo_xwin() {
  if ! cargo xwin --version >/dev/null 2>&1; then
    echo "缺少 cargo-xwin，无法配置 Windows SDK 与 MSVC 链接环境。" >&2
    echo "请先执行：cargo install cargo-xwin --locked" >&2
    exit 1
  fi
}

# 校验 MSVC 静态库构建所需的 LLVM 归档工具。cargo-xwin 会自动发现 Homebrew LLVM，
# 也会复用 rustup 的 llvm-tools-preview 组件，并为 llvm-ar 创建 llvm-lib 兼容入口。
# 参数说明：无。
# 返回值：找到 llvm-ar 时返回 0；缺失时按当前平台输出安装方式并退出脚本。
validate_llvm_archiver() {
  local rust_llvm_ar
  local host_system
  rust_llvm_ar="$(dirname "$(rustc --print target-libdir)")/bin/llvm-ar"
  host_system="$(uname -s)"

  if command -v llvm-ar >/dev/null 2>&1 || [[ -x "$rust_llvm_ar" ]]; then
    return
  fi

  if [[ "$host_system" == "Darwin" ]]; then
    if [[ -x "/opt/homebrew/opt/llvm/bin/llvm-ar" || -x "/usr/local/opt/llvm/bin/llvm-ar" ]]; then
      return
    fi

    echo "缺少 LLVM 归档工具 llvm-ar/llvm-lib。" >&2
    echo "请先执行：brew install llvm" >&2
  else
    echo "缺少 LLVM 归档工具 llvm-ar/llvm-lib。" >&2
    echo "请安装系统 LLVM 工具包，或执行：rustup component add llvm-tools-preview" >&2
  fi
  exit 1
}

# 确保 Windows 资源编译所需的应用图标存在。
# 参数说明：无。
# 返回值：图标存在或成功生成时返回 0；图标生成失败时由生成脚本终止执行。
ensure_windows_icon() {
  if [[ ! -f "$WINDOWS_ICON" ]]; then
    "$ROOT_DIR/scripts/generate_icons.sh" "$ROOT_DIR/assets/icons/app-icon.png"
  fi
}

# 使用 cargo-xwin 编译 release 版 Windows x64 主程序，并校验目标文件确实生成。
# 参数说明：调用本脚本时附加的参数会原样传给 cargo，例如可传入 --locked。
# 返回值：构建且产物校验成功时返回 0；Cargo 失败或产物缺失时退出脚本。
build_windows_executable() {
  cd "$ROOT_DIR"
  cargo xwin build --release --target "$WINDOWS_TARGET" --bin argus "$@"

  if [[ ! -f "$WINDOWS_EXECUTABLE" ]]; then
    echo "交叉编译命令已结束，但未找到目标文件：$WINDOWS_EXECUTABLE" >&2
    exit 1
  fi
}

# 使用系统 file 命令核验产物格式，防止工具链或目标目录配置错误时误报成功。
# 参数说明：无。
# 返回值：产物为 PE32+ x86-64 或系统未提供 file 时返回 0；架构不符时退出脚本。
validate_windows_executable() {
  local executable_description

  if ! command -v file >/dev/null 2>&1; then
    return
  fi

  executable_description="$(file "$WINDOWS_EXECUTABLE")"
  if [[ "$executable_description" != *"PE32+"* || "$executable_description" != *"x86-64"* ]]; then
    echo "目标文件不是预期的 Windows x64 可执行文件：$executable_description" >&2
    exit 1
  fi
}

# 按依赖顺序执行环境检查与交叉编译，尽早给出可操作的错误提示。
require_command "uname" "请安装可提供 uname 的基础系统工具。"
require_command "cargo" "请先安装 Rust 工具链：https://rustup.rs/"
require_command "rustup" "请使用 rustup 管理 Rust 工具链：https://rustup.rs/"
validate_host_platform
validate_rust_target
validate_cargo_xwin
validate_llvm_archiver
ensure_windows_icon
build_windows_executable "$@"
validate_windows_executable

echo "Windows x64 可执行文件已生成：$WINDOWS_EXECUTABLE"
