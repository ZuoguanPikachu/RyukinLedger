<#
.SYNOPSIS
    Resets the mandatory integrity label of the project folder back to Medium.

.DESCRIPTION
    Every file in this project currently carries "Mandatory Label\Low".  That
    label is inherited from the project root, and Windows lowers a new process
    to the integrity level of its executable image.  The consequences are:

      * anything started from a file in here runs at Low integrity;
      * a Low integrity process obeys "no write up", so it cannot write to
        %LOCALAPPDATA% (Medium) at all -- it gets Access Denied even though the
        ACL grants full control;
      * the same Low, unsigned executable gets refused by SmartScreen on the
        shell (double-click) launch path.

    Setting the label of the tree back to Medium fixes all three, because new
    files then inherit Medium from their parent directory.

    IMPORTANT -- read this before running
    -------------------------------------
    The coding agent runs under a sandbox whose own token is at Low integrity.
    While the project folder is Low, the agent can write to it (write-equal).
    The moment the folder becomes Medium, the agent's writes become *write up*
    and are denied: it will no longer be able to edit sources or build.

    So run this only together with switching DSH for this project to a
    non-sandboxed (full access) mode.  Otherwise you trade one problem for
    another.

    This script needs elevation because *raising* an object's integrity level
    requires SeRelabelPrivilege.

.EXAMPLE
    # from an ELEVATED PowerShell, in the project root:
    powershell -ExecutionPolicy Bypass -File .\tools\reset-integrity-label.ps1

.EXAMPLE
    # dry run: report only, change nothing
    powershell -ExecutionPolicy Bypass -File .\tools\reset-integrity-label.ps1 -ReportOnly
#>
[CmdletBinding()]
param(
    # Directory to reset.  Defaults to the project root (the parent of tools\).
    [string] $Path = (Split-Path -Parent $PSScriptRoot),

    # Also reset every existing file and subdirectory.  Without this, only the
    # root's inherit-only ACE changes, so files that already exist keep Low.
    [bool] $Recurse = $true,

    # Print what would happen and exit without touching anything.
    [switch] $ReportOnly
)

$ErrorActionPreference = 'Stop'

function Get-LabelOf([string] $P) {
    $line = & icacls $P 2>$null | Select-String -Pattern 'Mandatory Label'
    if (-not $line) { return '(none -> Medium by default)' }
    return ($line.ToString().Trim() -replace '\s+', ' ')
}

$resolved = (Resolve-Path -LiteralPath $Path).Path

Write-Host ''
Write-Host '================ 当前状态 ================' -ForegroundColor Cyan
Write-Host "目标目录 : $resolved"
Write-Host ("目录标签 : {0}" -f (Get-LabelOf $resolved))

$isAdmin = ([Security.Principal.WindowsPrincipal] [Security.Principal.WindowsIdentity]::GetCurrent()
    ).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

Write-Host ("提权状态 : {0}" -f $(if ($isAdmin) { '是' } else { '否' }))

if ($ReportOnly) {
    Write-Host ''
    Write-Host '(-ReportOnly：未做任何修改)' -ForegroundColor Yellow
    exit 0
}

if (-not $isAdmin) {
    Write-Host ''
    Write-Host '需要管理员权限：把对象的完整性级别「提高」需要 SeRelabelPrivilege。' -ForegroundColor Red
    Write-Host ''
    Write-Host '请这样执行（会弹 UAC）：' -ForegroundColor Yellow
    Write-Host ('  Start-Process powershell -Verb RunAs -ArgumentList ''-ExecutionPolicy'',''Bypass'',''-File'',''{0}''' -f $PSCommandPath) -ForegroundColor White
    Write-Host ''
    Write-Host '在动手之前，请先确认你已经把 DSH 对这个项目切成非沙箱（full access）模式，' -ForegroundColor Red
    Write-Host '否则改完之后编码代理会因为 no-write-up 而无法再写入这个目录。' -ForegroundColor Red
    exit 1
}

Write-Host ''
Write-Host '================ 执行 ================' -ForegroundColor Cyan

# (OI)(CI)Medium: apply the label to this container and let children inherit it.
$icaclsArgs = @($resolved, '/setintegritylevel', '(OI)(CI)Medium')
if ($Recurse) { $icaclsArgs += @('/T', '/C') }

Write-Host ("icacls {0}" -f ($icaclsArgs -join ' '))
Write-Host ''

& icacls @icaclsArgs
$exit = $LASTEXITCODE

Write-Host ''
Write-Host '================ 结果 ================' -ForegroundColor Cyan
Write-Host ("目录标签 : {0}" -f (Get-LabelOf $resolved))

Write-Host ''
if ($exit -eq 0) {
    Write-Host '完成。' -ForegroundColor Green
} else {
    Write-Host "icacls 退出码 $exit，可能有部分文件没改成功。" -ForegroundColor Yellow
}
Write-Host ''
