# 文件职责：在 Windows 本机打包 Argus 应用。
# 创建日期：2026-06-15
# 修改日期：2026-06-15
# 作者：Argus 开发团队
# 主要功能：编译 release 版本、复制带图标资源的 argus.exe，并生成 zip 分发包。

$ErrorActionPreference = "Stop"

$RootDir = Resolve-Path (Join-Path $PSScriptRoot "..")
$DistDir = Join-Path $RootDir "dist\windows\Argus"
$ZipPath = Join-Path $RootDir "dist\windows\Argus-windows.zip"
$IconPath = Join-Path $RootDir "resources\windows\app.ico"

if (-not (Test-Path $IconPath)) {
    throw "缺少 Windows 图标资源：$IconPath。请先在 macOS 上运行 scripts/generate_icons.sh，或手动提供 app.ico。"
}

Push-Location $RootDir
try {
    cargo build --release

    if (Test-Path $DistDir) {
        Remove-Item $DistDir -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
    Copy-Item (Join-Path $RootDir "target\release\argus.exe") (Join-Path $DistDir "argus.exe")
    Copy-Item $IconPath (Join-Path $DistDir "Argus.ico")

    if (Test-Path $ZipPath) {
        Remove-Item $ZipPath -Force
    }
    Compress-Archive -Path $DistDir -DestinationPath $ZipPath
    Write-Host "Windows 应用已打包：$DistDir"
    Write-Host "Windows 压缩包已生成：$ZipPath"
}
finally {
    Pop-Location
}
