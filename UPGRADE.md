<!--
文件职责：说明 Argus 自动升级功能的客户端配置、服务器文件布局和发布清单格式。
创建日期：2026-06-16
修改日期：2026-06-16
作者：Argus 开发团队
主要功能：为客户端接入、升级服务器部署、manifest 编写、签名和本地验证提供操作说明。
-->

# Argus 自动升级功能说明

## 1. 功能概述

Argus 当前升级模型为“按当前安装单元覆盖升级”。Windows/Linux 裸二进制继续使用 `self-replace` 替换当前可执行文件；macOS 正式分发使用 `Argus.app.zip`，客户端解包后替换整个 `Argus.app` bundle，再通过 LaunchServices 启动新版本。macOS 使用 `cargo run` 或裸二进制运行时不执行自动安装，避免把 `.app.zip` 误写入可执行文件。

客户端启动后读取 `~/.argus/settings.toml` 中的升级配置；若启用自动检查且配置了升级服务器和验签公钥，客户端会在后台请求服务器上的 `manifest-v1.json` 和 `manifest-v1.json.sig`。

客户端只会在以下条件全部满足时提示或安装新版本：

- `manifest-v1.json.sig` 是对 `manifest-v1.json` 原文字节的 Ed25519 签名。
- 设置页“升级”中配置的 `public_key_base64` 可以验证该签名。
- manifest 中的 `version` 高于当前 `CARGO_PKG_VERSION`。
- manifest 中存在匹配当前 `os` 和 `arch` 的资产。
- 下载的升级资产大小等于 `size_bytes`。
- 下载的升级资产 SHA-256 等于 `sha256`。

用户在弹窗中点击“立即升级”后，客户端会下载并校验升级资产。Windows/Linux 裸二进制模式会保存到 `~/.argus/updates/<version>/argus(.exe).download` 并调用 `self-replace`；macOS `.app` 模式会保存 `Argus.app.zip.download`，解包出 `Argus.app`，把当前 `.app` 整体替换为新版 bundle，然后启动新版本并退出旧进程。

## 2. 客户端配置

配置文件位置：

```text
~/.argus/settings.toml
```

升级配置段示例：

```toml
[upgrade]
enabled = true
server_url = "https://updates.example.com/argus/"
public_key_base64 = "Ed25519 公钥 Base64"
skipped_version = "0.2.0"
last_check_at = "2026-06-16T12:00:00Z"
```

字段说明：

| 字段 | 类型 | 是否必填 | 说明 |
| --- | --- | --- | --- |
| `enabled` | bool | 否 | 是否在启动后自动检查升级；默认 `false`。 |
| `server_url` | string | 是 | 升级服务器根地址；为空时不发起自动检查。 |
| `public_key_base64` | string | 是 | 32 字节 Ed25519 验签公钥的 Base64 文本；为空时不信任任何升级清单。 |
| `skipped_version` | string | 否 | 用户选择“跳过此版本”后写入；自动检查会忽略该版本，手动检查仍可显示。 |
| `last_check_at` | string | 否 | 最近一次检查时间，由客户端写入，仅用于设置页展示和诊断。 |

设置页中的“关于”分组展示当前版本、当前平台和“检查更新”按钮；“升级”分组用于配置自动检查开关、升级服务器地址和 `public_key_base64` 验签公钥。

## 3. 服务器文件布局

服务器只需要托管静态文件。客户端会在 `server_url` 下读取固定文件名：

```text
https://updates.example.com/argus/
├── manifest-v1.json
├── manifest-v1.json.sig
├── macos-aarch64/
│   └── Argus.app.zip
├── macos-x86_64/
│   └── Argus.app.zip
├── linux-x86_64/
│   └── argus
└── windows-x86_64/
    └── argus.exe
```

客户端请求规则：

- `server_url + manifest-v1.json`
- `server_url + manifest-v1.json.sig`
- manifest 中每个资产的 `url`

资产 `url` 可以是绝对地址，也可以是相对 `server_url` 的路径。例如 `macos-aarch64/Argus.app.zip` 会被解析为 `https://updates.example.com/argus/macos-aarch64/Argus.app.zip`。

macOS 资产必须是 zip 包，zip 内应包含一个顶层 `Argus.app` 目录；客户端会校验其中存在 `Contents/MacOS/argus`。Windows/Linux 资产仍为单个可执行文件。

## 4. manifest-v1.json 格式

完整示例：

```json
{
  "version": "0.2.0",
  "release_notes": "新增自动升级功能\n优化大文件日志读取体验\n修复设置页输入框焦点问题",
  "published_at": "2026-06-16T12:00:00Z",
  "assets": [
    {
      "os": "macos",
      "arch": "aarch64",
      "url": "macos-aarch64/Argus.app.zip",
      "sha256": "8d6f2a0c1a0f0f6d2f4a8a7f2d3b0f1e9c4a5f7b8c9d0e1f2a3b4c5d6e7f8091",
      "size_bytes": 28674656
    },
    {
      "os": "macos",
      "arch": "x86_64",
      "url": "macos-x86_64/Argus.app.zip",
      "sha256": "0f4e2d9c8b7a695847362514f0e1d2c3b4a5968778695a4b3c2d1e0f9a8b7c6d",
      "size_bytes": 30193280
    },
    {
      "os": "linux",
      "arch": "x86_64",
      "url": "linux-x86_64/argus",
      "sha256": "9d8c7b6a594837261504f3e2d1c0bbaa99887766554433221100ffeeddccbbaa",
      "size_bytes": 20444960
    },
    {
      "os": "windows",
      "arch": "x86_64",
      "url": "windows-x86_64/argus.exe",
      "sha256": "4f8d2a7b9c1e0f6d3b5a79887766554433221100ffeeddccbbaa998877665544",
      "size_bytes": 21544960
    }
  ]
}
```

字段说明：

| 字段 | 类型 | 说明 |
| --- | --- | --- |
| `version` | string | 远端版本号，支持标准 SemVer 和 `v0.2.0` 写法；必须高于当前版本才会提示。 |
| `release_notes` | string | 升级日志，客户端弹窗按换行展示。 |
| `published_at` | string | 发布时间，推荐 RFC3339。 |
| `assets` | array | 平台资产列表。 |
| `assets[].os` | string | 目标系统；当前客户端会匹配 `macos`、`windows`、`linux`。 |
| `assets[].arch` | string | 目标架构；当前客户端会匹配 `aarch64`、`x86_64`、`arm`。 |
| `assets[].url` | string | 升级资产下载地址；macOS 为 `Argus.app.zip`，Windows/Linux 为 binary；可为绝对 URL 或相对路径。 |
| `assets[].sha256` | string | 升级资产 SHA-256 十六进制值；macOS 计算 zip 文件，Windows/Linux 计算 binary。 |
| `assets[].size_bytes` | number | 升级资产字节数；macOS 填 zip 文件大小，Windows/Linux 填 binary 文件大小。 |

注意：`manifest-v1.json.sig` 必须签名 `manifest-v1.json` 的原始字节。修改 manifest 的空格、换行或字段顺序后，都必须重新签名。

## 5. 生成 SHA-256 和文件大小

macOS：

```bash
/usr/bin/ditto -c -k --keepParent Argus.app macos-aarch64/Argus.app.zip
shasum -a 256 macos-aarch64/Argus.app.zip
stat -f%z macos-aarch64/Argus.app.zip
```

Linux：

```bash
shasum -a 256 linux-x86_64/argus
stat -c%s linux-x86_64/argus
```

Windows PowerShell：

```powershell
Get-FileHash .\windows-x86_64\argus.exe -Algorithm SHA256
(Get-Item .\windows-x86_64\argus.exe).Length
```

## 6. manifest 签名

客户端使用 Ed25519 校验 `manifest-v1.json.sig`。签名文件内容是 Base64 文本，表示 64 字节 Ed25519 签名。

客户端不再内置发布公钥。需要把发布私钥对应的 32 字节 Ed25519 公钥 Base64 配置到设置页“升级”分组的“验签公钥”，或写入 `~/.argus/settings.toml` 的 `[upgrade].public_key_base64` 字段。设置页输入框的占位提示为 `ARGUS_UPDATE_PUBLIC_KEY_BASE64`，用于提示这里需要填写同名发布公钥值。

签名伪流程：

```text
manifest_bytes = read("manifest-v1.json")
signature = ed25519_sign(private_key, manifest_bytes)
write_text("manifest-v1.json.sig", base64(signature))
```

使用 Python 和 PyNaCl 生成演示签名：

```python
from base64 import b64encode
from pathlib import Path
from nacl.signing import SigningKey

private_key_seed = bytes.fromhex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f")
signing_key = SigningKey(private_key_seed)
public_key_base64 = b64encode(bytes(signing_key.verify_key)).decode("ascii")

manifest = Path("manifest-v1.json").read_bytes()
signature = signing_key.sign(manifest).signature
Path("manifest-v1.json.sig").write_text(b64encode(signature).decode("ascii"), encoding="utf-8")

print("ARGUS_UPDATE_PUBLIC_KEY_BASE64 =", public_key_base64)
```

说明：上面的私钥仅用于本地 demo。生产环境必须使用安全生成和保管的私钥。

## 7. 本地 demo

假设已经构建出新版 macOS `.app`：

```bash
cargo build --release
mkdir -p /tmp/argus-updates/macos-aarch64
cp -R target/release/bundle/osx/Argus.app /tmp/argus-updates/Argus.app
/usr/bin/ditto -c -k --keepParent /tmp/argus-updates/Argus.app /tmp/argus-updates/macos-aarch64/Argus.app.zip
```

生成 SHA-256 和大小后写入 `/tmp/argus-updates/manifest-v1.json`。示例：

```json
{
  "version": "0.2.0",
  "release_notes": "本地 demo 升级\n验证 manifest 签名和 .app bundle 覆盖流程",
  "published_at": "2026-06-16T12:00:00Z",
  "assets": [
    {
      "os": "macos",
      "arch": "aarch64",
      "url": "macos-aarch64/Argus.app.zip",
      "sha256": "<替换为 shasum -a 256 Argus.app.zip 输出>",
      "size_bytes": <替换为 stat -f%z Argus.app.zip 输出>
    }
  ]
}
```

使用上文 Python 脚本生成 `/tmp/argus-updates/manifest-v1.json.sig`，然后启动静态服务器：

```bash
cd /tmp/argus-updates
python3 -m http.server 8765
```

客户端配置：

```toml
[upgrade]
enabled = true
server_url = "http://127.0.0.1:8765/"
public_key_base64 = "填入脚本输出的 ARGUS_UPDATE_PUBLIC_KEY_BASE64"
```

启动旧版 Argus 后，在设置页“升级”中确认服务器地址和验签公钥，再在“关于”中点击“检查更新”。若签名、公钥、版本号、平台、哈希和大小均正确，客户端会弹出新版本提示。

## 8. 发布检查清单

发布前按以下顺序检查：

1. `Cargo.toml` 中的版本号已更新。
2. macOS 的 `Argus.app` 和其它平台 binary 都来自对应版本的 release build。
3. 每个平台资产都已计算正确的 SHA-256 和字节数。
4. `manifest-v1.json` 中 `version` 高于旧客户端版本。
5. `manifest-v1.json.sig` 是对最终 manifest 文件重新签名后的结果。
6. 设置页或配置文件中的 `public_key_base64` 与发布私钥匹配。
7. 使用本地或预发服务器完成一次旧版到新版升级验证。

## 9. 常见问题

### macOS 为什么下载 zip，而不是直接下载 `.app`？

`.app` 是目录 bundle，HTTP 静态服务器无法把目录作为单个文件传输。发布时用 zip 保存整个 `Argus.app`，客户端校验 zip 的 SHA-256 和大小后解包，再把当前 `Argus.app` 整体替换为新版。

### 为什么 manifest 也要签名，升级资产还要 SHA-256？

签名保护 manifest 不被篡改，SHA-256 确保下载的 zip 或 binary 和 manifest 声明完全一致。两者同时通过才会安装。

### 用户点击“跳过此版本”后怎么办？

客户端会把版本号写入 `skipped_version`。启动后的自动检查不会再弹出该版本；设置页手动检查仍会忽略跳过状态并显示可用版本。

### 替换失败会怎样？

客户端会保留旧进程运行并展示失败弹窗。下载文件仍保存在 `~/.argus/updates/<version>/`，便于排查。
