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
    $BuildMessages = & cargo build --release --bin argus --message-format=json-render-diagnostics
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo 构建失败，停止打包并保留已有安装包。"
    }
    $BuildEvents = @($BuildMessages | ForEach-Object { $_ | ConvertFrom-Json })
    $Finished = @($BuildEvents | Where-Object { $_.reason -eq "build-finished" })
    $Executables = @($BuildEvents | Where-Object {
        $_.reason -eq "compiler-artifact" -and $_.target.name -eq "argus" -and $_.executable
    } | ForEach-Object { $_.executable } | Select-Object -Unique)
    if ($Finished.Count -ne 1 -or -not $Finished[0].success -or $Executables.Count -ne 1) {
        throw "本次构建未成功生成唯一的 Argus 可执行文件。"
    }
    $Executable = $Executables[0]
    if (-not (Test-Path -PathType Leaf $Executable)) {
        throw "构建产物不存在：$Executable"
    }

    if (Test-Path $DistDir) {
        Remove-Item $DistDir -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $DistDir | Out-Null
    Copy-Item $Executable (Join-Path $DistDir "argus.exe")
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
