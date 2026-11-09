<#
.SYNOPSIS
    Uninstall horon-agent on Windows.
.DESCRIPTION
    Removes the per-user launcher at %LOCALAPPDATA%\horon-agent\bin\horon-agent.exe,
    installed build binaries, and the horon-agent launcher directory from the user PATH.
    By default user data under %USERPROFILE%\.horon is kept.

    One-liner uninstall:
      irm https://raw.githubusercontent.com/horon-ai/agent/main/scripts/install/uninstall.ps1 | iex
.PARAMETER InstallDir
    Override the launcher directory (default: $env:LOCALAPPDATA\horon-agent\bin)
.PARAMETER Purge
    Also delete user data in $env:HORON_HOME or %USERPROFILE%\.horon.
.PARAMETER DryRun
    Print what would be removed without deleting anything.
.PARAMETER Yes
    Skip the confirmation prompt.
#>
param(
    [string]$InstallDir,
    [switch]$Purge,
    [switch]$DryRun,
    [switch]$Yes
)

$ErrorActionPreference = 'Stop'

function Write-Info($msg) { Write-Host $msg -ForegroundColor Blue }
function Write-Err($msg) { throw "error: $msg" }
function Write-Warn($msg) { Write-Host "warning: $msg" -ForegroundColor Yellow }

function Get-HoronAgentLocalAppDataDir {
    if ($env:LOCALAPPDATA) { return $env:LOCALAPPDATA }

    $localAppData = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
    if ($localAppData) { return $localAppData }

    if ($env:USERPROFILE) { return (Join-Path $env:USERPROFILE "AppData\Local") }
    return (Join-Path ([Environment]::GetFolderPath("UserProfile")) "AppData\Local")
}

function Get-DefaultHoronAgentInstallDir {
    return (Join-Path (Get-HoronAgentLocalAppDataDir) "horon-agent\bin")
}

function Get-HoronAgentRoamingAppDataDir {
    if ($env:APPDATA) { return $env:APPDATA }

    $appData = [Environment]::GetFolderPath([Environment+SpecialFolder]::ApplicationData)
    if ($appData) { return $appData }

    if ($env:USERPROFILE) { return (Join-Path $env:USERPROFILE "AppData\Roaming") }
    return (Join-Path ([Environment]::GetFolderPath("UserProfile")) "AppData\Roaming")
}

function Get-HoronAgentStartupShortcutPath {
    return (Join-Path (Get-HoronAgentRoamingAppDataDir) "Microsoft\Windows\Start Menu\Programs\Startup\horon-agent-hotkey.lnk")
}

function Get-HoronAgentHotkeyArtifactPaths([string]$UserDataDir) {
    $hotkeyDir = Join-Path $UserDataDir "hotkey"
    return @(
        (Join-Path $hotkeyDir "horon-agent-hotkey.ps1"),
        (Join-Path $hotkeyDir "horon-agent-hotkey-launcher.vbs"),
        (Join-Path $hotkeyDir "horon-agent-hotkey-shortcut.ps1")
    )
}

function Clear-HoronAgentHotkeySetupState([string]$UserDataDir) {
    $setupHintsPath = Join-Path $UserDataDir "setup_hints.json"
    if (-not (Test-Path -LiteralPath $setupHintsPath)) { return }

    try {
        $state = Get-Content -LiteralPath $setupHintsPath -Raw | ConvertFrom-Json -ErrorAction Stop
        foreach ($property in @(
            @{ Name = "hotkey_configured"; Value = $false },
            @{ Name = "hotkey_dismissed"; Value = $true }
        )) {
            if ($state.PSObject.Properties.Name -contains $property.Name) {
                $state.($property.Name) = $property.Value
            } else {
                $state | Add-Member -NotePropertyName $property.Name -NotePropertyValue $property.Value
            }
        }
        $state | ConvertTo-Json | Set-Content -LiteralPath $setupHintsPath -Encoding UTF8
    } catch {
        Write-Warn "Could not update hotkey setup state in $setupHintsPath"
    }
}

function ConvertTo-HoronAgentPathKey([string]$PathValue) {
    if (-not $PathValue) { return "" }
    $clean = [Environment]::ExpandEnvironmentVariables($PathValue.Trim().Trim('"'))
    if (-not $clean) { return "" }
    try { $clean = [System.IO.Path]::GetFullPath($clean) } catch {}
    $clean = $clean.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
    return $clean.ToUpperInvariant()
}

function Test-HoronAgentSafePurgePath([string]$PathValue) {
    $pathKey = ConvertTo-HoronAgentPathKey $PathValue
    if (-not $pathKey) { return $false }

    try {
        $fullPath = [System.IO.Path]::GetFullPath([Environment]::ExpandEnvironmentVariables($PathValue.Trim().Trim('"')))
        $rootKey = ConvertTo-HoronAgentPathKey ([System.IO.Path]::GetPathRoot($fullPath))
        $leafName = [System.IO.Path]::GetFileName($fullPath.TrimEnd(
            [System.IO.Path]::DirectorySeparatorChar,
            [System.IO.Path]::AltDirectorySeparatorChar
        ))
    } catch {
        return $false
    }

    if ($pathKey -eq $rootKey -or $leafName -notmatch '(?i)^\.?horon-agent(?:[-_ ].*)?$') {
        return $false
    }

    $separator = [string][System.IO.Path]::DirectorySeparatorChar
    foreach ($protectedPath in @(
        $env:USERPROFILE,
        $env:HOME,
        $env:LOCALAPPDATA,
        $env:APPDATA,
        [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
    )) {
        $protectedKey = ConvertTo-HoronAgentPathKey $protectedPath
        if (-not $protectedKey) { continue }
        if ($pathKey -eq $protectedKey -or $protectedKey.StartsWith($pathKey + $separator, [System.StringComparison]::OrdinalIgnoreCase)) {
            return $false
        }
    }

    return $true
}

function Test-HoronAgentManagedExecutablePath([string]$ExecutablePath, [string]$LauncherPath, [string]$BuildsDir) {
    $executableKey = ConvertTo-HoronAgentPathKey $ExecutablePath
    $launcherKey = ConvertTo-HoronAgentPathKey $LauncherPath
    $buildsKey = ConvertTo-HoronAgentPathKey $BuildsDir
    if (-not $executableKey) { return $false }
    if ($launcherKey -and $executableKey -eq $launcherKey) { return $true }

    # A live upgrade may rename the loaded stable launcher before replacing it.
    # Treat only that tightly-scoped backup pattern in the launcher directory as
    # managed so uninstall can stop and remove it without touching other tools.
    $launcherDirKey = ConvertTo-HoronAgentPathKey (Split-Path -Parent $LauncherPath)
    $executableDirKey = ConvertTo-HoronAgentPathKey (Split-Path -Parent $ExecutablePath)
    $executableName = Split-Path -Leaf $ExecutablePath
    if ($launcherDirKey -and $executableDirKey -eq $launcherDirKey -and $executableName -like '.horon-agent-launcher-old-*.exe') {
        return $true
    }

    $separator = [string][System.IO.Path]::DirectorySeparatorChar
    return [bool]($buildsKey -and $executableKey.StartsWith($buildsKey + $separator, [System.StringComparison]::OrdinalIgnoreCase))
}

function Split-HoronAgentPathList([string]$PathValue) {
    if (-not $PathValue) { return @() }
    $entries = @()
    foreach ($entry in ($PathValue -split ';')) {
        $clean = $entry.Trim().Trim('"')
        if ($clean) { $entries += $clean }
    }
    return $entries
}

function Join-HoronAgentPathList([string[]]$Entries) {
    if (-not $Entries -or $Entries.Count -eq 0) { return "" }
    return ($Entries -join ';')
}

function Get-HoronAgentManagedPathKeys([string]$InstallDir) {
    $keys = New-Object 'System.Collections.Generic.HashSet[string]' ([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($candidate in @($InstallDir, (Get-DefaultHoronAgentInstallDir))) {
        $key = ConvertTo-HoronAgentPathKey $candidate
        if ($key) { [void]$keys.Add($key) }
    }
    return $keys
}

function Resolve-HoronAgentPathRemoval {
    param(
        [Parameter(Mandatory = $true)][string]$InstallDir,
        [AllowNull()][string]$CurrentPath
    )

    $managedKeys = Get-HoronAgentManagedPathKeys -InstallDir $InstallDir
    $nextEntries = @()
    $removedManaged = 0

    foreach ($entry in (Split-HoronAgentPathList $CurrentPath)) {
        $key = ConvertTo-HoronAgentPathKey $entry
        if (-not $key) { continue }
        if ($managedKeys.Contains($key)) {
            $removedManaged += 1
            continue
        }
        $nextEntries += $entry
    }

    $nextPath = Join-HoronAgentPathList $nextEntries
    return [pscustomobject]@{
        Path = $nextPath
        Changed = ($nextPath -ne ([string]$CurrentPath))
        RemovedManagedEntries = $removedManaged
        InstallDir = $InstallDir
    }
}

function Send-HoronAgentEnvironmentChangedBroadcast {
    if ($env:HORON_AGENT_DISABLE_ENV_BROADCAST -eq "1") { return $false }
    if (-not ("HoronAgent.EnvironmentBroadcast" -as [type])) {
        Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
namespace HoronAgent {
    public static class EnvironmentBroadcast {
        [DllImport("user32.dll", SetLastError = true, CharSet = CharSet.Auto)]
        public static extern IntPtr SendMessageTimeout(
            IntPtr hWnd,
            UInt32 Msg,
            UIntPtr wParam,
            string lParam,
            UInt32 fuFlags,
            UInt32 uTimeout,
            out UIntPtr lpdwResult);
    }
}
"@
    }
    $result = [UIntPtr]::Zero
    [HoronAgent.EnvironmentBroadcast]::SendMessageTimeout([IntPtr]0xffff, 0x001A, [UIntPtr]::Zero, "Environment", 0x0002, 5000, [ref]$result) | Out-Null
    return $true
}

function Remove-HoronAgentUserPath {
    param(
        [Parameter(Mandatory = $true)][string]$InstallDir,
        [AllowNull()][string]$CurrentPath,
        [scriptblock]$SetUserPathAction,
        [scriptblock]$BroadcastAction,
        [bool]$Broadcast = $true
    )

    if (-not $PSBoundParameters.ContainsKey('CurrentPath')) {
        $CurrentPath = [Environment]::GetEnvironmentVariable("Path", "User")
    }

    $update = Resolve-HoronAgentPathRemoval -InstallDir $InstallDir -CurrentPath $CurrentPath
    $broadcasted = $false
    if ($update.Changed) {
        if ($SetUserPathAction) {
            & $SetUserPathAction $update.Path
        } else {
            [Environment]::SetEnvironmentVariable("Path", $update.Path, "User")
        }

        if ($Broadcast) {
            if ($BroadcastAction) { & $BroadcastAction | Out-Null } else { Send-HoronAgentEnvironmentChangedBroadcast | Out-Null }
            $broadcasted = $true
        }
    }
    $update | Add-Member -NotePropertyName Broadcasted -NotePropertyValue $broadcasted
    return $update
}


function Invoke-HoronAgentUninstall {
    param(
        [string]$InstallDir,
        [switch]$Purge,
        [switch]$DryRun,
        [switch]$Yes
    )

if (-not $InstallDir) { $InstallDir = Get-DefaultHoronAgentInstallDir }

$localHoronAgentRoot = Join-Path (Get-HoronAgentLocalAppDataDir) "horon-agent"
$launcherPath = Join-Path $InstallDir "horon-agent.exe"
$buildsDir = Join-Path $localHoronAgentRoot "builds"
$userDataDir = if ($env:HORON_HOME) {
    $env:HORON_HOME
} elseif ($env:USERPROFILE) {
    Join-Path $env:USERPROFILE ".horon"
} else {
    Join-Path ([Environment]::GetFolderPath("UserProfile")) ".horon"
}
$startupShortcutPath = Get-HoronAgentStartupShortcutPath
$hotkeyArtifactPaths = @(Get-HoronAgentHotkeyArtifactPaths -UserDataDir $userDataDir)
$launcherBackupPaths = if (Test-Path -LiteralPath $InstallDir) {
    @(Get-ChildItem -LiteralPath $InstallDir -Filter '.horon-agent-launcher-old-*.exe' -File -Force -ErrorAction SilentlyContinue |
        ForEach-Object { $_.FullName })
} else {
    @()
}
if ($Purge -and -not (Test-HoronAgentSafePurgePath $userDataDir)) {
    Write-Err "Refusing to purge unsafe HORON_HOME path '$userDataDir'. Use a dedicated .horon or horon-agent-* directory."
}

$targets = @()
if (Test-Path -LiteralPath $launcherPath) { $targets += "$launcherPath (launcher)" }
foreach ($path in $launcherBackupPaths) { $targets += "$path (previous live-upgrade launcher)" }
if (Test-Path -LiteralPath $buildsDir) { $targets += "$buildsDir (installed binaries)" }
if (Test-Path -LiteralPath $startupShortcutPath) { $targets += "$startupShortcutPath (launch-hotkey startup shortcut)" }
foreach ($path in $hotkeyArtifactPaths) {
    if (Test-Path -LiteralPath $path) { $targets += "$path (launch-hotkey artifact)" }
}
if ($Purge -and (Test-Path -LiteralPath $userDataDir)) { $targets += "$userDataDir (user data)" }

$userPathPreview = Resolve-HoronAgentPathRemoval -InstallDir $InstallDir -CurrentPath ([Environment]::GetEnvironmentVariable("Path", "User"))
if ($userPathPreview.RemovedManagedEntries -gt 0) {
    $targets += "$InstallDir (user PATH entry)"
}

if ($targets.Count -eq 0) {
    Write-Info "Nothing to uninstall: no horon-agent installation found."
    return 0
}

Write-Info "The following will be removed:"
foreach ($target in $targets) { Write-Host "  - $target" }
if (-not $Purge) {
    Write-Warn "User data in $userDataDir is kept. Run with -Purge for a full wipe."
}

if ($DryRun) {
    Write-Info "Dry run: nothing was deleted."
    return 0
}

if (-not $Yes) {
    $reply = Read-Host "Proceed? [y/N]"
    if ($reply -notin @("y", "Y", "yes", "YES")) {
        Write-Info "Aborted."
        return 1
    }
}

try {
    $managedProcessIds = @(Get-CimInstance Win32_Process -Filter "Name = 'horon-agent.exe'" -ErrorAction SilentlyContinue |
        Where-Object { Test-HoronAgentManagedExecutablePath -ExecutablePath $_.ExecutablePath -LauncherPath $launcherPath -BuildsDir $buildsDir } |
        ForEach-Object { $_.ProcessId })
    foreach ($processId in $managedProcessIds) {
        $process = Get-Process -Id $processId -ErrorAction SilentlyContinue
        Stop-Process -Id $processId -Force -ErrorAction SilentlyContinue
        if ($process) {
            try { [void]$process.WaitForExit(10000) } catch {}
        }
    }
} catch {}

if (Test-Path -LiteralPath $startupShortcutPath) {
    Remove-Item -LiteralPath $startupShortcutPath -Force
    Write-Info "Removed $startupShortcutPath"
}

foreach ($path in $hotkeyArtifactPaths) {
    if (Test-Path -LiteralPath $path) {
        Remove-Item -LiteralPath $path -Force
        Write-Info "Removed $path"
    }
}
if (-not $Purge) {
    $hotkeyDir = Join-Path $userDataDir "hotkey"
    if (Test-Path -LiteralPath $hotkeyDir) {
        Remove-Item -LiteralPath $hotkeyDir -Force -ErrorAction SilentlyContinue
    }
    Clear-HoronAgentHotkeySetupState -UserDataDir $userDataDir
}

# Alias launchers (horon.exe, hrn.exe) are copies of the launcher; remove only
# byte-identical ones so an unrelated program with the same name survives.
foreach ($alias in @("horon.exe", "hrn.exe")) {
    $aliasPath = Join-Path $InstallDir $alias
    if ((Test-Path -LiteralPath $aliasPath) -and (Test-Path -LiteralPath $launcherPath) -and
        ((Get-FileHash -LiteralPath $aliasPath).Hash -eq (Get-FileHash -LiteralPath $launcherPath).Hash)) {
        Remove-Item -LiteralPath $aliasPath -Force
        Write-Info "Removed $aliasPath"
    }
}
if (Test-Path -LiteralPath $launcherPath) {
    Remove-Item -LiteralPath $launcherPath -Force
    Write-Info "Removed $launcherPath"
}

foreach ($path in $launcherBackupPaths) {
    if (Test-Path -LiteralPath $path) {
        Remove-Item -LiteralPath $path -Force
        Write-Info "Removed $path"
    }
}

if (Test-Path -LiteralPath $InstallDir) {
    try { Remove-Item -LiteralPath $InstallDir -Force -ErrorAction SilentlyContinue } catch {}
}

if ($Purge) {
    foreach ($path in @($localHoronAgentRoot, $userDataDir)) {
        if ($path -and (Test-Path -LiteralPath $path)) {
            Remove-Item -LiteralPath $path -Recurse -Force
            Write-Info "Removed $path"
        }
    }
} elseif (Test-Path -LiteralPath $buildsDir) {
    Remove-Item -LiteralPath $buildsDir -Recurse -Force
    Write-Info "Removed $buildsDir"
}

$pathUpdate = Remove-HoronAgentUserPath -InstallDir $InstallDir
if ($pathUpdate.Changed) {
    Write-Info "Removed $($pathUpdate.RemovedManagedEntries) horon-agent entr$(if ($pathUpdate.RemovedManagedEntries -eq 1) { 'y' } else { 'ies' }) from user PATH"
}

Write-Info "horon-agent uninstalled."
Write-Info "Reinstall with: irm https://horon-agent.sh/install.ps1 | iex"


    return 0
}

if ($env:HORON_AGENT_UNINSTALL_PS1_IMPORT_ONLY -ne "1") {
    $exitCode = Invoke-HoronAgentUninstall -InstallDir $InstallDir -Purge:$Purge -DryRun:$DryRun -Yes:$Yes
    if ($null -ne $exitCode -and [int]$exitCode -ne 0) {
        if ($MyInvocation.MyCommand.Path) { exit ([int]$exitCode) }
        $global:LASTEXITCODE = [int]$exitCode
    }
}
