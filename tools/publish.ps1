<#
.SYNOPSIS
    Builds every packaging mode and turns each into a zip ready to hand out.

.DESCRIPTION
    build.ps1 makes one mode into one folder.  This script runs it once per mode,
    stages each result in a folder named after the release, compresses it, and
    hashes it:

        dist\release\RyukinLedger-0.1.0-win-x64-FrameworkDependent.zip
        dist\release\RyukinLedger-0.1.0-win-x64-SelfContained.zip
        dist\release\RyukinLedger-0.1.0-win-x64-SingleFile.zip
        dist\release\SHA256SUMS.txt

    Each zip contains a single top-level folder of the same name, so extracting
    it does not spray loose files into whatever directory it was unpacked in.

    The version comes from Directory.Build.props at the repository root -- the
    one place it is set.  The core's own version (src\irminsul\Cargo.toml) is
    printed alongside it, because the two mean different things and a release
    record is more useful with both.

    Nothing here talks to the network except the .NET restore that a
    SelfContained or SingleFile publish needs the first time.

.EXAMPLE
    pwsh -File .\tools\publish.ps1

.EXAMPLE
    # 只打最小那种，且不重复构建内核之外的其它形态
    pwsh -File .\tools\publish.ps1 -Modes FrameworkDependent

.EXAMPLE
    # 临时改版本号，不动 Directory.Build.props
    pwsh -File .\tools\publish.ps1 -Version 0.2.0
#>
[CmdletBinding()]
param(
    # 发行版本号。默认读仓库根目录 Directory.Build.props 里的 <Version>。
    [string] $Version,

    # 压缩包放哪。默认 dist\release。
    [string] $Output,

    [ValidateSet('FrameworkDependent', 'SelfContained', 'SingleFile')]
    [string[]] $Modes = @('FrameworkDependent', 'SelfContained', 'SingleFile'),

    [string] $RuntimeIdentifier = 'win-x64',

    # 不把 assets\ 放进压缩包。
    [switch] $NoAssets,

    # 压缩之后保留没压缩的目录（占空间，但方便直接跑一下试试）。
    [switch] $KeepFolders,

    # 透传给 build.ps1：给 cargo 加 --offline。
    [switch] $Offline
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$buildScript = Join-Path $PSScriptRoot 'build.ps1'
$propsPath = Join-Path $root 'Directory.Build.props'
$cargoToml = Join-Path $root 'src\irminsul\Cargo.toml'

if (-not $Output) {
    $Output = Join-Path $root 'dist\release'
}

function Write-Step([string] $Text) {
    Write-Host ''
    Write-Host "=== $Text" -ForegroundColor Cyan
}

function Read-ProductVersion([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path)) {
        throw "找不到 $Path —— 产品版本号应该在那里。"
    }

    [xml] $xml = Get-Content -LiteralPath $Path -Raw
    $node = $xml.SelectSingleNode('/Project/PropertyGroup/Version')
    if (-not $node -or [string]::IsNullOrWhiteSpace($node.InnerText)) {
        throw "$Path 里没有 <Version>。"
    }

    return $node.InnerText.Trim()
}

function Read-CoreVersion([string] $Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return '(找不到 Cargo.toml)' }

    $match = Select-String -LiteralPath $Path -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
    if (-not $match) { return '(Cargo.toml 里没有 version)' }

    return $match.Matches[0].Groups[1].Value
}

# ---------------------------------------------------------------------------

$started = Get-Date

if (-not $Version) {
    $Version = Read-ProductVersion $propsPath
}

# 版本号会被用作文件名，所以先挡住会破坏路径的字符，而不是等压缩到一半才炸。
if ($Version -notmatch '^[0-9A-Za-z][0-9A-Za-z.\-+]*$') {
    Write-Host "版本号 '$Version' 不能用作文件名。" -ForegroundColor Red
    exit 2
}

$coreVersion = Read-CoreVersion $cargoToml

Write-Step '准备'
Write-Host "  仓库     $root"
Write-Host "  输出     $Output"
Write-Host "  版本     $Version    (Directory.Build.props)"
Write-Host "  内核     $coreVersion  (src\irminsul\Cargo.toml)"
Write-Host "  平台     $RuntimeIdentifier"
Write-Host "  形态     $($Modes -join ', ')"
if ($NoAssets) { Write-Host '  素材     不打包' -ForegroundColor Yellow }

New-Item -ItemType Directory -Path $Output -Force | Out-Null

$results = @()

foreach ($mode in $Modes) {
    Write-Step "打包 $mode"

    $name = "RyukinLedger-$Version-$RuntimeIdentifier-$mode"
    $stage = Join-Path $Output $name
    $zip = Join-Path $Output "$name.zip"

    $buildArgs = @{
        Mode              = $mode
        Destination       = $stage
        RuntimeIdentifier = $RuntimeIdentifier
        Clean             = $true
        Quiet             = $true
    }
    if ($Offline) { $buildArgs['Offline'] = $true }

    # 进程内调用：build.ps1 里的 exit 只会结束它自己，不会把这里带走。
    $modeStarted = Get-Date
    & $buildScript @buildArgs

    if ($LASTEXITCODE -ne 0) {
        Write-Host ''
        Write-Host "  build.ps1 失败（$mode），退出码 $LASTEXITCODE。" -ForegroundColor Red
        exit $LASTEXITCODE
    }

    # 光看退出码不够：一个漏拷贝的目录也可能返回 0。缺件就不该打出包来。
    $required = @('RyukinLedger.exe', 'irminsul.exe')
    $absent = $required | Where-Object { -not (Test-Path -LiteralPath (Join-Path $stage $_)) }
    if ($absent) {
        Write-Host ''
        Write-Host "  产出里缺少：$($absent -join ', ')。没有打包。" -ForegroundColor Red
        exit 2
    }

    if ($NoAssets) {
        $assets = Join-Path $stage 'assets'
        if (Test-Path -LiteralPath $assets) {
            Remove-Item -LiteralPath $assets -Recurse -Force
            Write-Host '  已剔除 assets\'
        }
    }

    if (Test-Path -LiteralPath $zip) { Remove-Item -LiteralPath $zip -Force }

    Write-Host '  压缩…'
    # includeBaseDirectory: 解压出来是一层同名文件夹，而不是散落一地。
    [System.IO.Compression.ZipFile]::CreateFromDirectory(
        $stage,
        $zip,
        [System.IO.Compression.CompressionLevel]::Optimal,
        $true)

    $file = Get-Item -LiteralPath $zip
    $hash = (Get-FileHash -LiteralPath $zip -Algorithm SHA256).Hash

    $results += [pscustomobject]@{
        Mode    = $mode
        Name    = $file.Name
        Bytes   = $file.Length
        Sha256  = $hash
        Seconds = ((Get-Date) - $modeStarted).TotalSeconds
    }

    Write-Host ("  {0}  ({1:N1} MB, {2:N0} 秒)" -f $file.Name, ($file.Length / 1MB), $results[-1].Seconds) -ForegroundColor Green

    if (-not $KeepFolders) {
        Remove-Item -LiteralPath $stage -Recurse -Force
    }
}

# ---------------------------------------------------------------------------
# 校验和
# ---------------------------------------------------------------------------
# 给每个包一行 `<hash>  <文件名>`，就是 sha256sum -c 认的格式，所以别人拿到
# 压缩包之后不用先学会这个脚本怎么用。
$sumsPath = Join-Path $Output 'SHA256SUMS.txt'
$results | ForEach-Object { "$($_.Sha256.ToLowerInvariant())  $($_.Name)" } |
    Set-Content -LiteralPath $sumsPath -Encoding ascii

Write-Step '结果'

foreach ($result in $results) {
    Write-Host ("  {0,-52} {1,8:N1} MB" -f $result.Name, ($result.Bytes / 1MB))
    Write-Host ("  {0,-52} {1}" -f '', $result.Sha256.ToLowerInvariant()) -ForegroundColor DarkGray
}

$totalMb = ($results | Measure-Object -Property Bytes -Sum).Sum / 1MB

Write-Host ''
Write-Host ("完成，用时 {0:N1} 秒。{1} 个压缩包，共 {2:N1} MB。" -f ((Get-Date) - $started).TotalSeconds, $results.Count, $totalMb) -ForegroundColor Green
Write-Host ''
Write-Host "  $Output" -ForegroundColor White
Write-Host "  $sumsPath" -ForegroundColor DarkGray

Write-Host ''
exit 0
