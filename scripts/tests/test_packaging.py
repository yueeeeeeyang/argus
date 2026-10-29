"""用隔离的构建工具替身验证打包入口，防止失败后发布旧产物。"""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


SCRIPTS = Path(__file__).resolve().parents[1]


class PackagingTests(unittest.TestCase):
    def run_package(self, script, exit_code):
        """构造旧默认产物和新自定义目录产物，执行真实打包脚本并检查最终结果。"""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            for name in (script, "read_build_artifact.py"):
                shutil.copy(SCRIPTS / name, root / "scripts" / name)
            generator = root / "scripts/generate_icons.sh"
            generator.write_text("#!/bin/sh\nexit 0\n")
            generator.chmod(0o755)
            for name in ("resources/windows/app.ico", "resources/macos/AppIcon.icns", "resources/macos/Info.plist"):
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"test resource")
            target = "x86_64-pc-windows-msvc"
            is_windows = script != "package_macos.sh"
            is_cross_build = script == "build_windows_x64.sh"
            old_binary = root / (f"target/{target}/release/argus.exe" if is_windows else "target/release/argus")
            old_binary.parent.mkdir(parents=True)
            old_binary.write_bytes(b"stale executable")
            artifact = root / "custom target/release/argus"
            artifact.parent.mkdir(parents=True)
            artifact.write_bytes(b"fresh executable")
            package = root / ("dist/windows/Argus" if is_windows else "dist/macos/Argus.app")
            package.mkdir(parents=True)
            sentinel = package / "existing-package"
            sentinel.write_bytes(b"preserve on failure")
            binaries = root / "tools"
            binaries.mkdir()
            cargo = binaries / "cargo"
            cargo.write_text(
                "#!/usr/bin/env python3\nimport json, os, sys\n"
                "if sys.argv[1:] == ['xwin', '--version']: print('cargo-xwin test'); sys.exit(0)\n"
                "print(json.dumps({'reason':'compiler-artifact','target':{'name':'argus'},'executable':os.environ['FAKE_ARTIFACT']}))\n"
                "print(json.dumps({'reason':'build-finished','success':int(os.environ['FAKE_BUILD_EXIT']) == 0}))\n"
                "sys.exit(int(os.environ['FAKE_BUILD_EXIT']))\n"
            )
            cargo.chmod(0o755)
            for name, body in {
                "codesign": "exit 0",
                "rustc": f"echo 'host: {target}'",
                "rustup": f"echo '{target}'",
                "llvm-ar": "exit 0",
                "file": "echo 'PE32+ executable x86-64'",
            }.items():
                command = binaries / name
                command.write_text(f"#!/bin/sh\n{body}\n")
                command.chmod(0o755)
            environment = os.environ | {
                "PATH": str(binaries) + os.pathsep + os.environ["PATH"],
                "FAKE_ARTIFACT": str(artifact),
                "FAKE_BUILD_EXIT": str(exit_code),
                "CARGO_TARGET_DIR": str(root / "custom target"),
            }
            result = subprocess.run(["bash", str(root / "scripts" / script)], env=environment, text=True, capture_output=True, timeout=20)
            if exit_code:
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(sentinel.is_file(), result.stdout + result.stderr)
                self.assertNotIn("应用已打包", result.stdout)
                self.assertNotIn("可执行文件已生成", result.stdout)
            else:
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                if is_cross_build:
                    self.assertIn(str(artifact), result.stdout)
                else:
                    output = package / ("argus.exe" if is_windows else "Contents/MacOS/argus")
                    self.assertEqual(output.read_bytes(), b"fresh executable")

    def test_bash_packages_use_successful_build_artifact_from_custom_directory(self):
        for script in ("package_macos.sh", "package_windows.sh", "build_windows_x64.sh"):
            with self.subTest(script=script):
                self.run_package(script, 0)

    def test_bash_packages_preserve_existing_package_when_build_fails(self):
        for script in ("package_macos.sh", "package_windows.sh", "build_windows_x64.sh"):
            with self.subTest(script=script):
                self.run_package(script, 1)

    def test_artifact_reader_rejects_incomplete_or_malformed_build(self):
        for messages in ("not json", json.dumps({"reason": "build-finished", "success": True}), json.dumps({"reason": "build-finished", "success": False})):
            with self.subTest(messages=messages):
                result = subprocess.run(["python3", str(SCRIPTS / "read_build_artifact.py")], input=messages, text=True, capture_output=True, timeout=5)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
