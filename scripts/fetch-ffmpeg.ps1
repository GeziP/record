<#
.SYNOPSIS
  下载开发用的 ffmpeg（LGPL 构建，不含 libx264 等 GPL 组件）到 vendor\ffmpeg\。
  核对 SHA-256 之后才解包；只解包，不安装。

.PARAMETER Zip
  已经下载好的 zip。GitHub 很慢或者离线时先用浏览器下载，再用它指定；哈希照样核对。

.PARAMETER Force
  vendor\ffmpeg 已经存在也重新下载。
#>
[CmdletBinding()]
param(
    [string]$Zip,
    [switch]$Force
)
$ErrorActionPreference = 'Stop'

# 固定版本。BtbN 的 autobuild 标签会被定期清理：下载报 404 时，到
# https://github.com/BtbN/FFmpeg-Builds/releases 找一个较新的 win64-lgpl 资源，更新下面三行
# （哈希要自己下载后计算并核对来源，不要照抄别人给的）。
$Tag = 'autobuild-2026-10-13-13-05'
$Asset = 'ffmpeg-n9.0.2-27-g7e93014c5b-win64-lgpl-9.0.zip'
$Sha256 = '1ad3d8acc778b8fadf43e8b43d2c6f6db90d7e20494669bbb32eb03ea8f8eeeb'
$Url = "https://github.com/BtbN/FFmpeg-Builds/releases/download/$Tag/$Asset"

$root = Split-Path -Parent $PSScriptRoot
$dest = Join-Path $root 'vendor\ffmpeg'
$exe = Join-Path $dest 'bin\ffmpeg.exe'
if ((Test-Path $exe) -and -not $Force) {
    Write-Host "已经有了：$exe"
    & $exe -hide_banner -version | Select-Object -First 1
    return
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ('record-ffmpeg-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    if ($Zip) {
        $zipPath = (Resolve-Path $Zip).Path
    }
    else {
        $zipPath = Join-Path $tmp $Asset
        Write-Host "下载 $Url"
        try {
            $ProgressPreference = 'SilentlyContinue'
            Invoke-WebRequest -Uri $Url -OutFile $zipPath -UseBasicParsing
        }
        catch {
            throw ("下载失败：{0}`n如果是 404，说明 BtbN 已经清理了这个标签，见脚本顶部的说明；网络慢的话，用浏览器下载后用 -Zip 指定。" -f $_.Exception.Message)
        }
    }

    $actual = (Get-FileHash $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($actual -ne $Sha256) {
        throw "SHA-256 不符：期望 $Sha256，实际 $actual。文件不可信，没有解包。"
    }
    Write-Host 'SHA-256 核对通过，解包……'

    $unpacked = Join-Path $tmp 'unpacked'
    Expand-Archive -Path $zipPath -DestinationPath $unpacked
    $inner = Get-ChildItem $unpacked -Directory | Select-Object -First 1
    if (-not $inner -or -not (Test-Path (Join-Path $inner.FullName 'bin\ffmpeg.exe'))) {
        throw '压缩包里没有 bin\ffmpeg.exe，结构和预期不一致，没有安装。'
    }
    if (Test-Path $dest) { Remove-Item $dest -Recurse -Force }
    New-Item -ItemType Directory -Path (Split-Path $dest) -Force | Out-Null
    Move-Item $inner.FullName $dest
    Write-Host "已解包到 $dest"
    & $exe -hide_banner -version | Select-Object -First 1
}
finally {
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
