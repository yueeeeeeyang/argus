#!/usr/bin/env python3
"""从 Cargo 的本次成功构建事件中取得 Argus 可执行文件，支持自定义目标目录。"""

import json
from pathlib import Path
import sys


def main():
    """缺少成功结束事件或唯一可执行产物时失败，避免打包旧 target 目录文件。"""
    executables = set()
    succeeded = False
    try:
        for line in sys.stdin:
            message = json.loads(line)
            if message.get("reason") == "build-finished":
                succeeded = message.get("success") is True
            if (
                message.get("reason") == "compiler-artifact"
                and message.get("target", {}).get("name") == "argus"
                and message.get("executable")
            ):
                executables.add(message["executable"])
        if not succeeded or len(executables) != 1:
            raise ValueError("本次构建未成功生成唯一的 Argus 可执行文件")
        executable = Path(executables.pop())
        if not executable.is_file():
            raise ValueError(f"构建产物不存在：{executable}")
        print(executable)
    except (ValueError, OSError) as error:
        print(f"读取构建产物失败：{error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
