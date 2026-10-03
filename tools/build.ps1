<#
.SYNOPSIS
    Builds the capture core and the interface, and stages both into dist\app\.

.DESCRIPTION
    The two executables belong in one folder: the interface looks for
    irminsul.exe next to itself and does not go hunting through a build tree, so
    the folder this script produces is a runnable application rather than a pile
    of build output.

        dist\app\RyukinLedger.exe        界面
        dist\app\irminsul.exe            抓包内核
        ...                              以及所选打包模式带上的其余文件

    Packaging modes (-Mode)
    -----------------------
      FrameworkDependent  (默认) 最小。目标机器必须装 .NET 9 Desktop Runtime，
                                  否则双击会提示去下载。
      SelfContained               把 .NET 运行时一起放进文件夹，约 150 MB。
                                  拷到没装运行时的机器上也能跑。
      SingleFile                  单个自包含 exe。注意它是自解压的：首次启动会
                                  解包到 %TEMP%，所以启动更慢，对杀软和
                                  SmartScreen 的暴露面也更大。

    裁剪（PublishTrimmed）和 Native AOT 不在选项里，因为 WPF 不支持它们 ——
    这不是本项目的取舍，是微软明确列出的限制。

    -ReadyToRun 是正交的附加项：预先编译，启动更快，代价是文件更大。它需要
    运行时标识，脚本会自动带上 -r。

    The script also reports the mandatory integrity label of what it produced.
    A label other than Medium means the folder is sandboxed or synchronised in a
    way that will make the executables run at Low integrity -- in which case they
    cannot write to %LOCALAPPDATA%, and Windows will refuse to start them on the
    double-click path.  tools\reset-integrity-label.ps1 explains the mechanism
    and repairs it.

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\tools\build.ps1

.EXAMPLE
    # 自带运行时，拷到别的机器也能跑
    powershell -ExecutionPolicy Bypass -File .\tools\build.ps1 -Mode SelfContained

.EXAMPLE
    # 单文件，且从干净的 dist\app 开始
    powershell -ExecutionPolicy Bypass -File .\tools\build.ps1 -Mode SingleFile -Clean

.EXAMPLE
    # 依赖已在 cargo 缓存里，没有网络
    powershell -ExecutionPolicy Bypass -File .\tools\build.ps1 -Offline
#>
[CmdletBinding()]
param(
    # 打包形态。见上面的说明。
    [ValidateSet('FrameworkDependent', 'SelfContained', 'SingleFile')]
    [string] $Mode = 'FrameworkDependent',

    [string] $Configuration = 'Release',

    # 运行时标识。SelfContained / SingleFile / -ReadyToRun 需要它。
    [string] $RuntimeIdentifier = 'win-x64',

    # 预先编译（ReadyToRun）：启动更快，文件更大。WPF 支持它；裁剪和 AOT 不支持。
    [switch] $ReadyToRun,

    # Where the runnable application is assembled.  Defaults to dist\app.
    [string] $Destination,

    # Remove the destination folder first.
    [switch] $Clean,

    # 只构建，不打印「接下来怎么跑」那几行。tools\publish.ps1 用它：那里要连做
    # 三次构建，三段一模一样的运行提示只会把最后的汇总冲掉。
    [switch] $Quiet,

    # Pass --offline to cargo, for building without network access.
    [switch] $Offline,

    [switch] $SkipCore,
    [switch] $SkipApp
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
if (-not $Destination) {
    $Destination = Join-Path $root 'dist\app'
}

$appProject = Join-Path $root 'src\RyukinLedger.App\RyukinLedger.App.csproj'
$coreDir = Join-Path $root 'src\irminsul'
$coreExe = Join-Path $coreDir "target\$($Configuration.ToLowerInvariant())\irminsul.exe"

function Write-Step([string] $Text) {
    Write-Host ''
    Write-Host "=== $Text" -ForegroundColor Cyan
}

function Get-LabelText([string] $File) {
    $line = & icacls $File 2>$null | Select-String -Pattern 'Mandatory Label'
    if (-not $line) { return '(无标签 -> 默认 Medium)' }
    return ($line.ToString().Trim() -replace '\s+', ' ')
}

$started = Get-Date

Write-Step '准备'
Write-Host "  仓库   $root"
Write-Host "  输出   $Destination"
Write-Host "  配置   $Configuration"
Write-Host "  模式   $Mode$(if ($ReadyToRun) { ' + ReadyToRun' })"
if ($Mode -ne 'FrameworkDependent' -or $ReadyToRun) {
    Write-Host "  平台   $RuntimeIdentifier"
}

if ($Clean -and (Test-Path -LiteralPath $Destination)) {
    Write-Host '  清理旧输出…'
    Remove-Item -LiteralPath $Destination -Recurse -Force
}

New-Item -ItemType Directory -Path $Destination -Force | Out-Null

# ---------------------------------------------------------------------------
# 界面
# ---------------------------------------------------------------------------
if (-not $SkipApp) {
    Write-Step '构建界面'
    if (-not (Test-Path -LiteralPath $appProject)) {
        Write-Host "  找不到 $appProject" -ForegroundColor Red
        exit 2
    }

    # 项目文件把四个开关钉成了「框架依赖 + 不单文件 + 不裁剪 + 不 R2R」；
    # 命令行上的 -p: 优先级更高，所以这里按模式覆盖它们。
    $publishArgs = @($appProject, '-c', $Configuration, '-o', $Destination, '--nologo')

    if ($Mode -ne 'FrameworkDependent' -or $ReadyToRun) {
        $publishArgs += @('-r', $RuntimeIdentifier)
    }

    switch ($Mode) {
        'FrameworkDependent' {
            $publishArgs += @('-p:SelfContained=false', '-p:PublishSingleFile=false')
        }
        'SelfContained' {
            $publishArgs += @('-p:SelfContained=true', '-p:PublishSingleFile=false')
        }
        'SingleFile' {
            # WPF 的原生 DLL 也要塞进单文件，运行时再解包出来。
            $publishArgs += @(
                '-p:SelfContained=true',
                '-p:PublishSingleFile=true',
                '-p:IncludeNativeLibrariesForSelfExtract=true'
            )
        }
    }

    if ($ReadyToRun) {
        $publishArgs += '-p:PublishReadyToRun=true'
    }

    Write-Host "  dotnet publish -Mode $Mode"
    & dotnet publish @publishArgs
    if ($LASTEXITCODE -ne 0) {
        Write-Host '  dotnet publish 失败。' -ForegroundColor Red
        exit $LASTEXITCODE
    }
} else {
    Write-Step '跳过界面'
}

# ---------------------------------------------------------------------------
# 抓包内核
# ---------------------------------------------------------------------------
if (-not $SkipCore) {
    Write-Step '构建抓包内核'

    if (-not (Test-Path -LiteralPath $coreDir)) {
        Write-Host "  找不到 $coreDir" -ForegroundColor Red
        exit 2
    }

    $cargoArgs = @('build')
    if ($Configuration -eq 'Release') { $cargoArgs += '--release' }
    if ($Offline) { $cargoArgs += '--offline' }

    Push-Location $coreDir
    try {
        Write-Host "  cargo $($cargoArgs -join ' ')"
        & cargo @cargoArgs
        $cargoExit = $LASTEXITCODE
    } finally {
        Pop-Location
    }

    if ($cargoExit -ne 0) {
        Write-Host '  cargo build 失败。' -ForegroundColor Red
        if (-not $Offline) {
            Write-Host '  如果只是没有网络，且依赖已经在 cargo 缓存里，试试加 -Offline。' -ForegroundColor Yellow
        }
        exit $cargoExit
    }

    if (-not (Test-Path -LiteralPath $coreExe)) {
        Write-Host "  构建结束但找不到 $coreExe" -ForegroundColor Red
        exit 2
    }

    Copy-Item -LiteralPath $coreExe -Destination $Destination -Force
} else {
    Write-Step '跳过抓包内核'
}

# ---------------------------------------------------------------------------
# 第三方声明
# ---------------------------------------------------------------------------
# 内核是从 konkers/irminsul 改出来的，它的依赖也都是 MIT。MIT 要求版权声明和
# 许可声明跟着软件一起分发，所以这份文件必须和 exe 放在一起，而不是只留在仓库里。
$notices = Join-Path $root 'THIRD-PARTY-NOTICES.md'
if (Test-Path -LiteralPath $notices) {
    Copy-Item -LiteralPath $notices -Destination $Destination -Force
    Copy-Item -LiteralPath (Join-Path $coreDir 'LICENSE') -Destination $Destination -Force
}

# ---------------------------------------------------------------------------
# 结果
# ---------------------------------------------------------------------------
Write-Step '结果'

# SingleFile 只有一个 exe：dll / deps.json / runtimeconfig.json 都打包在里面了。
$expected = @('RyukinLedger.exe')
if ($Mode -ne 'SingleFile') {
    $expected += @('RyukinLedger.dll', 'RyukinLedger.deps.json', 'RyukinLedger.runtimeconfig.json')
}

if (-not $SkipCore) { $expected += 'irminsul.exe' }

$missing = @()
$lowLabelled = @()

foreach ($name in $expected) {
    $file = Join-Path $Destination $name
    if (-not (Test-Path -LiteralPath $file)) {
        $missing += $name
        continue
    }

    $label = Get-LabelText $file
    if ($label -notmatch 'Medium' -and $label -notmatch '无标签') {
        $lowLabelled += "$name ($label)"
    }

    Write-Host ("  {0,-30} {1,12:N0} 字节   {2}" -f $name, (Get-Item -LiteralPath $file).Length, $label)
}

if ($missing.Count -gt 0) {
    Write-Host ''
    Write-Host "缺少文件：$($missing -join ', ')" -ForegroundColor Red
    exit 2
}

# A stale copy of the core in the output folder is silent and poisonous: the
# interface runs whatever irminsul.exe sits next to it, so an old build looks
# exactly like a new one until the log fails to show the change that was just
# made.  That has already cost one debugging round, so say it out loud.
$deployedCore = Join-Path $Destination 'irminsul.exe'
if ($SkipCore) {
    if (-not (Test-Path -LiteralPath $deployedCore)) {
        Write-Host ''
        Write-Host '注意：输出目录里没有 irminsul.exe（-SkipCore），界面启动内核会直接失败。' -ForegroundColor Red
    } else {
        $newestSource = Get-ChildItem (Join-Path $coreDir 'src') -Recurse -File -ErrorAction SilentlyContinue |
            Sort-Object LastWriteTime -Descending |
            Select-Object -First 1

        if ($newestSource -and (Get-Item -LiteralPath $deployedCore).LastWriteTime -lt $newestSource.LastWriteTime) {
            Write-Host ''
            Write-Host '注意：输出目录里的 irminsul.exe 比内核源码旧。' -ForegroundColor Red
            Write-Host ("      内核  {0}" -f (Get-Item -LiteralPath $deployedCore).LastWriteTime) -ForegroundColor Red
            Write-Host ("      源码  {0}  ({1})" -f $newestSource.LastWriteTime, $newestSource.Name) -ForegroundColor Red
            Write-Host '      -SkipCore 不会重建内核，跑起来仍然是旧行为。去掉 -SkipCore 重跑。' -ForegroundColor Yellow
        }
    }
}

$totalMb = (Get-ChildItem -LiteralPath $Destination -Recurse -File | Measure-Object -Property Length -Sum).Sum / 1MB

Write-Host ''
Write-Host ("完成，用时 {0:N1} 秒。输出目录共 {1:N1} MB。" -f ((Get-Date) - $started).TotalSeconds, $totalMb) -ForegroundColor Green

if (-not $Quiet) {
    if ($Mode -eq 'FrameworkDependent') {
        Write-Host '目标机器需要 .NET 9 Desktop Runtime（当前模式不自带运行时）。' -ForegroundColor DarkGray
    } elseif ($Mode -eq 'SingleFile') {
        Write-Host '单文件 exe 首次启动会解包到 %TEMP%，启动较慢，对杀软的暴露面也更大。' -ForegroundColor DarkGray
        Write-Host '如果 SmartScreen 变敏感了，换回 -Mode SelfContained 试试。' -ForegroundColor DarkGray
    }

    Write-Host ''
    Write-Host '运行：' -ForegroundColor Yellow
    Write-Host "  $(Join-Path $Destination 'RyukinLedger.exe')" -ForegroundColor White
    Write-Host ''
    Write-Host '  先只看界面（不弹 UAC）：加 --no-core' -ForegroundColor DarkGray
    Write-Host '  临时指定内核：        --core <irminsul.exe 路径>' -ForegroundColor DarkGray
}

if ($lowLabelled.Count -gt 0) {
    Write-Host ''
    Write-Host '注意：下面这些文件带的不是 Medium 标签。' -ForegroundColor Red
    $lowLabelled | ForEach-Object { Write-Host "  $_" -ForegroundColor Red }
    Write-Host '  从这种文件启动的进程会运行在 Low 完整性级别上，' -ForegroundColor Red
    Write-Host '  结果是写不进 %LOCALAPPDATA%，并且双击会被 Windows 拒绝启动。' -ForegroundColor Red
    Write-Host '  用管理员 PowerShell 跑 tools\reset-integrity-label.ps1 把标签改回来。' -ForegroundColor Yellow
}

Write-Host ''

# 显式退出码。没有这一行的话，$LASTEXITCODE 会停在最后一个原生命令（icacls）的
# 结果上——调用者（tools\publish.ps1）读到的是那个，而不是「构建成功了」。
exit 0
