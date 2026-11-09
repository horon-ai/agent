<#
.SYNOPSIS
    Install horon-agent on Windows.
.DESCRIPTION
    Downloads the latest horon-agent release and installs it to %LOCALAPPDATA%\horon-agent\bin.

    One-liner install:
      irm https://horon-agent.sh/install.ps1 | iex

    Or download and run (allows parameters):
      & ([scriptblock]::Create((irm https://horon-agent.sh/install.ps1)))
.PARAMETER InstallDir
    Override the installation directory (default: $env:LOCALAPPDATA\horon-agent\bin)
.PARAMETER Version
    Override the version tag to install. Required when using a local artifact path.
.PARAMETER ArtifactExePath
    Use a local horon-agent.exe artifact instead of downloading from GitHub.
.PARAMETER ArtifactTgzPath
    Use a local horon-agent .tar.gz artifact instead of downloading from GitHub.
.PARAMETER ReleaseAssetsDir
    Directory holding the same release's SHA256SUMS. Required with a local
    artifact; nothing is downloaded then.
.PARAMETER BuildFromSource
    If no prebuilt release asset is available, explicitly allow a source build.
    Source builds require Git, Rust, and the Visual Studio C++ Build Tools.
.PARAMETER ConfigureAlacritty
    Install Alacritty through winget when it is not already available.
.PARAMETER SkipAlacrittySetup
    Deprecated compatibility switch. Alacritty setup is opt-in by default.
#>
param(
    [string]$InstallDir,
    [string]$Version,
    [string]$ArtifactExePath,
    [string]$ArtifactTgzPath,
    [string]$ReleaseAssetsDir,
    [switch]$BuildFromSource,
    [switch]$ConfigureAlacritty,
    [switch]$SkipAlacrittySetup
)

$ErrorActionPreference = 'Stop'

if ($PSVersionTable.PSVersion.Major -lt 5) {
    Write-Host "error: PowerShell 5.1 or later is required" -ForegroundColor Red
    exit 1
}

$Repo = "horon-ai/agent"

if (-not $InstallDir) {
    $localAppData = if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData) }
    if (-not $localAppData -and $env:USERPROFILE) { $localAppData = Join-Path $env:USERPROFILE "AppData\Local" }
    $InstallDir = Join-Path $localAppData "horon-agent\bin"
}

$HoronAgentHome = if ($env:HORON_HOME) {
    $env:HORON_HOME
} elseif ($env:USERPROFILE) {
    Join-Path $env:USERPROFILE ".horon"
} else {
    Join-Path ([Environment]::GetFolderPath("UserProfile")) ".horon"
}

function Write-Info($msg) { Write-Host $msg -ForegroundColor Blue }
function Write-Err($msg) { throw "error: $msg" }
function Write-Warn($msg) { Write-Host "warning: $msg" -ForegroundColor Yellow }

function ConvertFrom-HoronAgentWebContent($Content) {
    if ($null -eq $Content) { return "" }

    # Windows PowerShell 5.1 returns Byte[] for some text responses when the
    # server uses application/octet-stream (including horon-agent.sh metadata and
    # GitHub release checksum manifests). Casting Byte[] directly to [string]
    # produces a space-separated list of decimal bytes instead of the text.
    if ($Content -is [byte[]]) {
        return [System.Text.Encoding]::UTF8.GetString($Content)
    }

    return [string]$Content
}

function Test-HoronAgentReleaseTag([string]$Tag) {
    return [bool]($Tag -match '^v[0-9]+\.[0-9]+\.[0-9]+(?:[+.-][A-Za-z0-9.-]+)?$')
}

function Resolve-HoronAgentReleaseTagFromUri([string]$Uri) {
    if (-not $Uri) { return $null }
    if ($Uri -match '/releases/tag/([^/?#]+)') {
        return [Uri]::UnescapeDataString($Matches[1])
    }
    return $null
}

function Get-LatestHoronAgentReleaseTag {
    # Avoid api.github.com here. Its unauthenticated limit is only 60 requests
    # per public IP per hour, so installs are unreliable behind shared NAT/VPNs.
    try {
        $response = Invoke-WebRequest -UseBasicParsing -Method Head -Uri "https://github.com/$Repo/releases/latest"
        $baseResponse = $response.BaseResponse
        $resolvedUri = $null

        if ($baseResponse) {
            $responseUriProperty = $baseResponse.PSObject.Properties['ResponseUri']
            if ($responseUriProperty -and $responseUriProperty.Value) {
                $resolvedUri = [string]$responseUriProperty.Value
            }

            if (-not $resolvedUri) {
                $requestMessageProperty = $baseResponse.PSObject.Properties['RequestMessage']
                if ($requestMessageProperty -and $requestMessageProperty.Value) {
                    $resolvedUri = [string]$requestMessageProperty.Value.RequestUri
                }
            }
        }

        $tag = Resolve-HoronAgentReleaseTagFromUri $resolvedUri
        if ($tag) { return $tag }
    } catch {
        Write-Err "Failed to determine latest version: $_"
    }

    Write-Err "Failed to determine latest version"
}

function Get-HoronAgentSha256FromManifest {
    param(
        [Parameter(Mandatory = $true)][string]$ManifestText,
        [Parameter(Mandatory = $true)][string]$AssetName
    )

    $found = @()
    foreach ($line in ($ManifestText -split "`r?`n")) {
        if ($line -match '^\s*([0-9a-fA-F]{64})\s+\*?(.+?)\s*$') {
            # A manifest row may name the asset by path (`nested/path/asset.exe`,
            # and `./asset.exe` from a directory walk); the artifact selected for
            # download is matched by its file name.
            $candidateName = [System.IO.Path]::GetFileName($Matches[2])
            if ($candidateName -eq $AssetName) {
                $found += $Matches[1].ToLowerInvariant()
            }
        }
    }

    if ($found.Count -eq 1) { return $found[0] }
    return $null
}

function Assert-HoronAgentFileChecksum([string]$FilePath, [string]$ExpectedSha256, [string]$AssetName) {
    try {
        $actual = (Get-FileHash -LiteralPath $FilePath -Algorithm SHA256).Hash.ToLowerInvariant()
    } catch {
        Write-Err "Could not calculate SHA256 for ${AssetName}: $_"
    }

    if ($actual -ne $ExpectedSha256) {
        Remove-Item -LiteralPath $FilePath -Force -ErrorAction SilentlyContinue
        Write-Err "SHA256 verification failed for $AssetName (expected $ExpectedSha256, got $actual)"
    }

    Write-Info "Verified SHA256: $AssetName"
    return $actual
}

function Get-HoronAgentLocalAppDataDir {
    if ($env:LOCALAPPDATA) {
        return $env:LOCALAPPDATA
    }

    $localAppData = [Environment]::GetFolderPath([Environment+SpecialFolder]::LocalApplicationData)
    if ($localAppData) {
        return $localAppData
    }

    if ($env:USERPROFILE) {
        return (Join-Path $env:USERPROFILE "AppData\Local")
    }

    return (Join-Path ([Environment]::GetFolderPath("UserProfile")) "AppData\Local")
}

function Get-DefaultHoronAgentInstallDir {
    return (Join-Path (Get-HoronAgentLocalAppDataDir) "horon-agent\bin")
}

function ConvertTo-HoronAgentPathKey([string]$PathValue) {
    if (-not $PathValue) {
        return ""
    }

    $clean = [Environment]::ExpandEnvironmentVariables($PathValue.Trim().Trim('"'))
    if (-not $clean) {
        return ""
    }

    try {
        $clean = [System.IO.Path]::GetFullPath($clean)
    } catch {
    }


    $clean = $clean.TrimEnd([System.IO.Path]::DirectorySeparatorChar, [System.IO.Path]::AltDirectorySeparatorChar)
    return $clean.ToUpperInvariant()
}

function Split-HoronAgentPathList([string]$PathValue) {
    if (-not $PathValue) {
        return @()
    }

    $entries = @()
    foreach ($entry in ($PathValue -split ';')) {
        $clean = $entry.Trim().Trim('"')
        if ($clean) {
            $entries += $clean
        }
    }
    return $entries
}

function Join-HoronAgentPathList([string[]]$Entries) {
    if (-not $Entries -or $Entries.Count -eq 0) {
        return ""
    }

    return ($Entries -join ';')
}

function Get-HoronAgentManagedPathKeys([string]$InstallDir) {
    $keys = New-Object 'System.Collections.Generic.HashSet[string]' ([System.StringComparer]::OrdinalIgnoreCase)
    foreach ($candidate in @($InstallDir, (Get-DefaultHoronAgentInstallDir))) {
        $key = ConvertTo-HoronAgentPathKey $candidate
        if ($key) {
            [void]$keys.Add($key)
        }
    }
    return $keys
}

function Resolve-HoronAgentPathUpdate {
    param(
        [Parameter(Mandatory = $true)][string]$InstallDir,
        [AllowNull()][string]$CurrentPath,
        [switch]$RemoveOnly
    )

    $managedKeys = Get-HoronAgentManagedPathKeys -InstallDir $InstallDir
    $nextEntries = @()
    $removedManaged = 0

    foreach ($entry in (Split-HoronAgentPathList $CurrentPath)) {
        $key = ConvertTo-HoronAgentPathKey $entry
        if (-not $key) {
            continue
        }

        if ($managedKeys.Contains($key)) {
            $removedManaged += 1
            continue
        }

        $nextEntries += $entry
    }

    if (-not $RemoveOnly) {
        $nextEntries = @($InstallDir) + $nextEntries
    }

    $nextPath = Join-HoronAgentPathList $nextEntries
    $changed = ($nextPath -ne ([string]$CurrentPath))

    return [pscustomobject]@{
        Path = $nextPath
        Changed = $changed
        RemovedManagedEntries = $removedManaged
        RemovedDuplicateEntries = 0
        InstallDir = $InstallDir
    }
}

function Send-HoronAgentEnvironmentChangedBroadcast {
    if ($env:HORON_AGENT_DISABLE_ENV_BROADCAST -eq "1") {
        return $false
    }

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

function Set-HoronAgentUserPath {
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

    $update = Resolve-HoronAgentPathUpdate -InstallDir $InstallDir -CurrentPath $CurrentPath
    $broadcasted = $false

    if ($update.Changed) {
        if ($SetUserPathAction) {
            & $SetUserPathAction $update.Path
        } else {
            [Environment]::SetEnvironmentVariable("Path", $update.Path, "User")
        }

        if ($Broadcast) {
            if ($BroadcastAction) {
                & $BroadcastAction | Out-Null
            } else {
                Send-HoronAgentEnvironmentChangedBroadcast | Out-Null
            }
            $broadcasted = $true
        }
    }

    $update | Add-Member -NotePropertyName Broadcasted -NotePropertyValue $broadcasted
    return $update
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

    $update = Resolve-HoronAgentPathUpdate -InstallDir $InstallDir -CurrentPath $CurrentPath -RemoveOnly
    $broadcasted = $false

    if ($update.Changed) {
        if ($SetUserPathAction) {
            & $SetUserPathAction $update.Path
        } else {
            [Environment]::SetEnvironmentVariable("Path", $update.Path, "User")
        }

        if ($Broadcast) {
            if ($BroadcastAction) {
                & $BroadcastAction | Out-Null
            } else {
                Send-HoronAgentEnvironmentChangedBroadcast | Out-Null
            }
            $broadcasted = $true
        }
    }

    $update | Add-Member -NotePropertyName Broadcasted -NotePropertyValue $broadcasted
    return $update
}

function Set-HoronAgentProcessPath([string]$InstallDir) {
    $update = Resolve-HoronAgentPathUpdate -InstallDir $InstallDir -CurrentPath $env:Path
    $env:Path = $update.Path
    return $update
}

function Remove-HoronAgentStaleLauncherBackups {
    param(
        [Parameter(Mandatory = $true)][string]$LauncherDir
    )

    Get-ChildItem -LiteralPath $LauncherDir -Filter '.horon-agent-launcher-old-*.exe' -File -Force -ErrorAction SilentlyContinue |
        Remove-Item -Force -ErrorAction SilentlyContinue
}

function Install-HoronAgentLauncher {
    param(
        [Parameter(Mandatory = $true)][string]$SourcePath,
        [Parameter(Mandatory = $true)][string]$LauncherPath
    )

    $launcherDir = Split-Path -Parent $LauncherPath
    New-Item -ItemType Directory -Path $launcherDir -Force | Out-Null

    $operationId = [guid]::NewGuid().ToString('N')
    $tempLauncher = Join-Path $launcherDir (".horon-agent-launcher-{0}.tmp.exe" -f $operationId)
    $oldLauncher = Join-Path $launcherDir (".horon-agent-launcher-old-{0}.exe" -f $operationId)
    $movedExistingLauncher = $false
    try {
        Copy-Item -Path $SourcePath -Destination $tempLauncher -Force
        if (Test-Path -LiteralPath $LauncherPath) {
            # Windows will not overwrite a loaded executable, but it does allow
            # the directory entry to be renamed while the process keeps running
            # from its existing file handle. Move the old launcher aside first,
            # then atomically put the new binary at the stable PATH location.
            Move-Item -LiteralPath $LauncherPath -Destination $oldLauncher
            $movedExistingLauncher = $true
        }

        try {
            Move-Item -LiteralPath $tempLauncher -Destination $LauncherPath
        } catch {
            if ($movedExistingLauncher -and -not (Test-Path -LiteralPath $LauncherPath)) {
                Move-Item -LiteralPath $oldLauncher -Destination $LauncherPath
                $movedExistingLauncher = $false
            }
            throw
        }

        if ($movedExistingLauncher) {
            # Removal succeeds immediately for an idle launcher. If an older
            # horon-agent process still has the renamed executable loaded, Windows
            # keeps it until that process exits and the next install cleans it.
            Remove-Item -LiteralPath $oldLauncher -Force -ErrorAction SilentlyContinue
        }

        # Only prune backups after the stable path contains the new launcher.
        # Doing this before replacement could delete another concurrent
        # installer's rollback file during its short rename window.
        Remove-HoronAgentStaleLauncherBackups -LauncherDir $launcherDir
    } finally {
        Remove-Item -LiteralPath $tempLauncher -Force -ErrorAction SilentlyContinue
    }

    return $LauncherPath
}

function Resolve-OptionalPath([string]$PathValue) {
    if (-not $PathValue) {
        return $null
    }

    try {
        return (Resolve-Path -LiteralPath $PathValue -ErrorAction Stop).Path
    } catch {
        Write-Err "Provided path does not exist: $PathValue"
    }
}

function Stop-ProcessTree([int]$ProcessId) {
    try {
        Get-CimInstance Win32_Process -ErrorAction SilentlyContinue |
            Where-Object { $_.ParentProcessId -eq $ProcessId } |
            ForEach-Object { Stop-ProcessTree -ProcessId $_.ProcessId }
    } catch {}

    try {
        Stop-Process -Id $ProcessId -Force -ErrorAction SilentlyContinue
    } catch {}
}

function Invoke-ProcessWithTimeout {
    param(
        [Parameter(Mandatory = $true)][string]$FilePath,
        [string[]]$ArgumentList = @(),
        [Parameter(Mandatory = $true)][int]$TimeoutSeconds,
        [Parameter(Mandatory = $true)][string]$FriendlyName,
        [switch]$CaptureOutput
    )

    $startParams = @{
        FilePath = $FilePath
        ArgumentList = $ArgumentList
        PassThru = $true
        NoNewWindow = $true
    }

    $stdoutPath = $null
    $stderrPath = $null
    if ($CaptureOutput) {
        $stdoutPath = Join-Path $env:TEMP ("horon-agent-{0}-{1}-stdout.log" -f $FriendlyName, [guid]::NewGuid().ToString('N'))
        $stderrPath = Join-Path $env:TEMP ("horon-agent-{0}-{1}-stderr.log" -f $FriendlyName, [guid]::NewGuid().ToString('N'))
        $startParams.RedirectStandardOutput = $stdoutPath
        $startParams.RedirectStandardError = $stderrPath
    }

    $process = Start-Process @startParams
    # Wait-Process did not gain -Timeout until newer PowerShell releases. Use
    # the underlying .NET Process API so the documented PowerShell 5.1 minimum
    # is real rather than only passing on PowerShell 7.
    $timedOut = -not $process.WaitForExit($TimeoutSeconds * 1000)
    if ($timedOut) {
        Stop-ProcessTree -ProcessId $process.Id
        return [pscustomobject]@{
            TimedOut = $true
            ExitCode = $null
            StdoutPath = $stdoutPath
            StderrPath = $stderrPath
        }
    }

    # Ensure redirected streams have finished flushing before callers inspect
    # their files, then refresh ExitCode from the completed process.
    $process.WaitForExit()
    $process.Refresh()
    return [pscustomobject]@{
        TimedOut = $false
        ExitCode = $process.ExitCode
        StdoutPath = $stdoutPath
        StderrPath = $stderrPath
    }
}

function Write-LogTail([string]$Path, [string]$Label) {
    if (-not $Path -or -not (Test-Path $Path)) {
        return
    }

    $lines = Get-Content -Path $Path -Tail 40 -ErrorAction SilentlyContinue
    if ($lines -and $lines.Count -gt 0) {
        Write-Warn "$Label (last 40 lines):"
        $lines | ForEach-Object { Write-Host $_ }
    }
}

function Test-CommandExists([string]$CommandName) {
    return [bool](Get-Command $CommandName -ErrorAction SilentlyContinue)
}

function Test-AlacrittyInstalled {
    return [bool](Find-AlacrittyPath)
}

function Find-AlacrittyPath {
    $candidates = @(
        "C:\Program Files\Alacritty\alacritty.exe",
        "C:\Program Files (x86)\Alacritty\alacritty.exe"
    )

    if ($env:LOCALAPPDATA) {
        $candidates += (Join-Path $env:LOCALAPPDATA "Microsoft\WinGet\Links\alacritty.exe")
    }

    foreach ($candidate in $candidates) {
        if ($candidate -and (Test-Path $candidate)) {
            return $candidate
        }
    }

    try {
        $command = Get-Command alacritty -ErrorAction Stop
        if ($command -and $command.Source) {
            return $command.Source
        }
    } catch {}

    return $null
}

function Install-Alacritty {
    if (Test-AlacrittyInstalled) {
        Write-Info "Alacritty is already installed"
        return $true
    }

    if (-not (Test-CommandExists "winget")) {
        Write-Warn "winget was not found, so Alacritty could not be installed automatically"
        Write-Warn "Install App Installer / winget from Microsoft, then run: winget install -e --id Alacritty.Alacritty"
        return $false
    }

    Write-Info "Installing Alacritty..."
    $wingetArgs = @(
        "install",
        "-e",
        "--id", "Alacritty.Alacritty",
        "--accept-source-agreements",
        "--accept-package-agreements",
        "--disable-interactivity"
    )

    $wingetResult = Invoke-ProcessWithTimeout -FilePath "winget" -ArgumentList $wingetArgs -TimeoutSeconds 180 -FriendlyName "winget-install"
    if ($wingetResult.TimedOut) {
        Write-Warn "Alacritty install timed out after 180 seconds; skipping automatic setup"
        return $false
    }

    if ($wingetResult.ExitCode -ne 0) {
        Write-Warn "Alacritty install failed (winget exit code: $($wingetResult.ExitCode))"
        return $false
    }

    $alacrittyPath = Find-AlacrittyPath
    if (-not $alacrittyPath) {
        Write-Warn "Alacritty install finished, but alacritty.exe was not found on PATH yet"
        return $false
    }

    Write-Info "Alacritty installed: $alacrittyPath"
    return $true
}

function ConvertFrom-HoronAgentVersionOutput([string]$Output) {
    if (-not $Output) {
        return $null
    }

    # A genuinely fresh profile may print a startup notice before the version.
    # When output is captured by PowerShell, terminal control
    # sequences can also leave the final `horon-agent v...` on the same logical line.
    if ($Output -match '(?i)\bhoron-agent\s+v?([0-9][0-9A-Za-z.+-]*)') {
        return "v$($Matches[1])"
    }

    return $null
}

function Get-HoronAgentVersionFromBinary([string]$BinaryPath) {
    if (-not $BinaryPath -or -not (Test-Path -LiteralPath $BinaryPath)) {
        return $null
    }

    $previousErrorActionPreference = $ErrorActionPreference
    try {
        # A fresh profile may emit a startup notice on stderr. Under Windows
        # PowerShell with ErrorActionPreference=Stop, native stderr is
        # promoted to a terminating NativeCommandError even when the process
        # succeeds. Capture both streams without letting that notice abort the
        # version probe.
        $ErrorActionPreference = 'Continue'
        $output = (& $BinaryPath --version 2>&1 | Out-String).Trim()
        $exitCode = $LASTEXITCODE
        if ($exitCode -ne 0) {
            return $null
        }
        return (ConvertFrom-HoronAgentVersionOutput $output)
    } catch {
        return $null
    } finally {
        $ErrorActionPreference = $previousErrorActionPreference
    }
}

function Assert-HoronAgentBinaryCandidate {
    param(
        [Parameter(Mandatory = $true)][string]$BinaryPath,
        [Parameter(Mandatory = $true)][string]$ExpectedVersion
    )

    if ($env:HORON_AGENT_INSTALL_SKIP_BINARY_VALIDATION -eq "1") {
        return $null
    }

    $reportedVersion = Get-HoronAgentVersionFromBinary $BinaryPath
    if (-not $reportedVersion) {
        Write-Err "Downloaded horon-agent binary could not run '--version'. It may be corrupt, quarantined by antivirus, or built for the wrong architecture."
    }

    $expectedNumber = $ExpectedVersion.TrimStart('v')
    if ($reportedVersion.TrimStart('v') -ne $expectedNumber) {
        Write-Err "Downloaded binary reports $reportedVersion, but the installer requested $ExpectedVersion"
    }

    Write-Info "Validated horon-agent binary: $reportedVersion"
    return $reportedVersion
}

function Test-HoronAgentMsvcBuildToolsAvailable {
    if (Get-Command link.exe -ErrorAction SilentlyContinue) {
        return $true
    }

    $programFilesX86 = [Environment]::GetFolderPath([Environment+SpecialFolder]::ProgramFilesX86)
    if (-not $programFilesX86) {
        return $false
    }

    $vswhere = Join-Path $programFilesX86 "Microsoft Visual Studio\Installer\vswhere.exe"
    if (-not (Test-Path -LiteralPath $vswhere)) {
        return $false
    }

    try {
        $linkPath = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\link.exe' 2>$null | Select-Object -First 1
        return [bool]$linkPath
    } catch {
        return $false
    }
}

function Assert-HoronAgentSourceBuildPrerequisites {
    if (-not (Get-Command git -ErrorAction SilentlyContinue)) {
        Write-Err "Git is required for -BuildFromSource. Install it with: winget install -e --id Git.Git"
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -or -not (Get-Command rustc -ErrorAction SilentlyContinue)) {
        Write-Err "Rust is required for -BuildFromSource. Install it from https://rustup.rs, then open a new PowerShell window."
    }

    $rustHost = ""
    try {
        $rustHost = (& rustc -vV 2>$null | Select-String '^host:' | Select-Object -First 1).ToString()
    } catch {}

    if ($rustHost -match 'pc-windows-msvc' -and -not (Test-HoronAgentMsvcBuildToolsAvailable)) {
        Write-Err "The MSVC linker (link.exe) was not found. Install Visual Studio 2022 Build Tools with the 'Desktop development with C++' workload, then open a new PowerShell window before using -BuildFromSource."
    }
}

function Resolve-HoronAgentWindowsArtifact([string[]]$ArchitectureCandidates) {
    $sawX64 = $false

    foreach ($arch in @($ArchitectureCandidates)) {
        if (-not $arch) { continue }
        switch -Regex ($arch.Trim()) {
            '^(Arm64|ARM64|AARCH64|aarch64)$' { return "horon-agent-windows-aarch64" }
            '^(X64|AMD64|x86_64)$' { $sawX64 = $true }
        }
    }

    if ($sawX64) { return "horon-agent-windows-x86_64" }
    return $null
}

function Get-HoronAgentWindowsArtifact {
    $candidates = @()

    try {
        $runtimeArch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
        if ($runtimeArch) { $candidates += [string]$runtimeArch }
    } catch {}

    foreach ($envArch in @($env:PROCESSOR_ARCHITECTURE, $env:PROCESSOR_ARCHITEW6432)) {
        if ($envArch) { $candidates += [string]$envArch }
    }

    $artifact = Resolve-HoronAgentWindowsArtifact $candidates
    if ($artifact) { return $artifact }

    $displayArch = if ($candidates.Count -gt 0) { $candidates -join ", " } else { "<unknown>" }
    Write-Err "Unsupported architecture: $displayArch (supported: x86_64, ARM64)"
}

function Expand-HoronAgentBootstrap([string]$Archive, [string]$Member, [string]$Destination) {
    # Stream only the fixed executable member. Never extract archive paths to disk.
    $start = New-Object System.Diagnostics.ProcessStartInfo
    $start.FileName = "tar"
    $start.Arguments = '-xzOf "' + $Archive.Replace('"', '\"') + '" "' + $Member + '"'
    $start.UseShellExecute = $false
    $start.RedirectStandardOutput = $true
    $start.RedirectStandardError = $true
    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $start
    $output = [System.IO.File]::Create($Destination)
    try {
        if (-not $process.Start()) { Write-Err "Could not start archive reader" }
        $copy = $process.StandardOutput.BaseStream.CopyToAsync($output)
        $errors = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit(60000)) {
            $process.Kill()
            $process.WaitForExit()
            Write-Err "Archive reader timed out"
        }
        $copy.GetAwaiter().GetResult()
        $message = $errors.GetAwaiter().GetResult()
        if ($process.ExitCode -ne 0) { Write-Err "Archive reader failed: $message" }
    } finally {
        $output.Dispose()
        $process.Dispose()
    }
}

function Invoke-HoronAgentInstall {
$Artifact = Get-HoronAgentWindowsArtifact

$ResolvedArtifactExePath = Resolve-OptionalPath $ArtifactExePath
$ResolvedArtifactTgzPath = Resolve-OptionalPath $ArtifactTgzPath

if ($ResolvedArtifactExePath -and $ResolvedArtifactTgzPath) {
    Write-Err "Provide only one of -ArtifactExePath or -ArtifactTgzPath"
}

if (-not $Version) {
    if ($ResolvedArtifactExePath -or $ResolvedArtifactTgzPath -or $ReleaseAssetsDir) {
        Write-Err "-Version is required when using local release assets"
    } else {
        Write-Info "Fetching latest release..."
        $Version = Get-LatestHoronAgentReleaseTag
    }
}

if (-not (Test-HoronAgentReleaseTag $Version)) { Write-Err "Invalid release version" }
if (($ResolvedArtifactExePath -or $ResolvedArtifactTgzPath) -and -not $ReleaseAssetsDir) {
    Write-Err "-ReleaseAssetsDir is required with local artifacts and must contain the release SHA256SUMS"
}

$VersionNum = $Version.TrimStart('v')
$DownloadBase = "https://github.com/$Repo/releases/download/$Version"

$BuildsDir = Join-Path (Get-HoronAgentLocalAppDataDir) "horon-agent\builds"
$LauncherPath = Join-Path $InstallDir "horon-agent.exe"

$Existing = ""
if (Test-Path $LauncherPath) {
    try { $Existing = & $LauncherPath --version 2>$null | Select-Object -First 1 } catch {}
}

if ($Existing) {
    if ($Existing -match [regex]::Escape($VersionNum)) {
        Write-Info "horon-agent $Version is already installed - reinstalling"
    } else {
        Write-Info "Updating horon-agent $Existing -> $Version"
    }
} else {
    Write-Info "Installing horon-agent $Version"
}
Write-Info "  launcher: $LauncherPath"

$TempDir = Join-Path $env:TEMP "horon-agent-install-$(Get-Random)"
New-Item -ItemType Directory -Path $TempDir -Force | Out-Null

try {
$AssetsDir = Join-Path $TempDir "assets"
New-Item -ItemType Directory -Path $AssetsDir | Out-Null
$checksumName = "SHA256SUMS"
$checksumPath = Join-Path $AssetsDir $checksumName
if ($ReleaseAssetsDir) {
    $source = Join-Path $ReleaseAssetsDir $checksumName
    $item = Get-Item -LiteralPath $source -ErrorAction Stop
    if ($item.PSIsContainer -or ($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint)) {
        Write-Err "Release asset is not a regular file: $checksumName"
    }
    Copy-Item -LiteralPath $source -Destination $checksumPath
} else {
    Invoke-WebRequest -UseBasicParsing -Uri "$DownloadBase/$checksumName" -OutFile $checksumPath
}
$Checksums = [System.IO.File]::ReadAllText($checksumPath)
$DownloadMode = ""
$DownloadPath = Join-Path $TempDir "horon-agent.download"

if ($ResolvedArtifactExePath) {
    Write-Info "Using local artifact exe: $ResolvedArtifactExePath"
    Copy-Item -Path $ResolvedArtifactExePath -Destination $DownloadPath -Force
    $DownloadMode = "bin"
} elseif ($ResolvedArtifactTgzPath) {
    Write-Info "Using local artifact archive: $ResolvedArtifactTgzPath"
    Copy-Item -Path $ResolvedArtifactTgzPath -Destination $DownloadPath -Force
    $DownloadMode = "tar"
} else {
    foreach ($candidate in @(
        @{ Name = "$Artifact.exe"; Mode = "bin" },
        @{ Name = "$Artifact.tar.gz"; Mode = "tar" }
    )) {
        try {
            Write-Info "Downloading $($candidate.Name) from $DownloadBase..."
            Invoke-WebRequest -UseBasicParsing -Uri "$DownloadBase/$($candidate.Name)" -OutFile $DownloadPath
            $DownloadMode = $candidate.Mode
            break
        } catch {}
    }
}

if ($DownloadMode) {
    $downloadedAssetName = if ($DownloadMode -eq "bin") { "$Artifact.exe" } else { "$Artifact.tar.gz" }
    $expectedSha256 = Get-HoronAgentSha256FromManifest -ManifestText $Checksums -AssetName $downloadedAssetName
    if (-not $expectedSha256) { Write-Err "SHA256SUMS must list $downloadedAssetName exactly once" }
    Assert-HoronAgentFileChecksum -FilePath $DownloadPath -ExpectedSha256 $expectedSha256 -AssetName $downloadedAssetName | Out-Null
    Copy-Item -LiteralPath $DownloadPath -Destination (Join-Path $AssetsDir $downloadedAssetName)
}

$DestBin = Join-Path $TempDir "horon-agent.exe"
$SourceBinary = $null

if ($DownloadMode -eq "tar") {
    Write-Info "Extracting..."
    Expand-HoronAgentBootstrap -Archive $DownloadPath -Member "$Artifact.exe" -Destination $DestBin
} elseif ($DownloadMode -eq "bin") {
    Move-Item -Path $DownloadPath -Destination $DestBin -Force
} else {
    if (-not $BuildFromSource) {
        $releaseUrl = "https://github.com/$Repo/releases/tag/$Version"
        Write-Err "No prebuilt $Artifact asset was found in $Version. Check $releaseUrl or rerun the downloaded script with -BuildFromSource. The installer will not start a long source build automatically."
    }

    Write-Info "No prebuilt asset found for $Artifact in $Version; -BuildFromSource was requested"
    Assert-HoronAgentSourceBuildPrerequisites

    $SrcDir = Join-Path $TempDir "horon-agent-src"
    Write-Info "Cloning $Repo at $Version..."
    $gitCloneResult = Invoke-ProcessWithTimeout -FilePath "git" -ArgumentList @(
        "clone",
        "--depth", "1",
        "--branch", $Version,
        "https://github.com/$Repo.git",
        $SrcDir
    ) -TimeoutSeconds 600 -FriendlyName "git-clone" -CaptureOutput
    if ($gitCloneResult.TimedOut) {
        Write-LogTail -Path $gitCloneResult.StdoutPath -Label "git stdout"
        Write-LogTail -Path $gitCloneResult.StderrPath -Label "git stderr"
        Write-Err "git clone timed out after 600 seconds"
    }
    if ($gitCloneResult.ExitCode -ne 0) {
        Write-LogTail -Path $gitCloneResult.StdoutPath -Label "git stdout"
        Write-LogTail -Path $gitCloneResult.StderrPath -Label "git stderr"
        Write-Err "Failed to clone $Repo at $Version (exit code: $($gitCloneResult.ExitCode))"
    }

    Write-Info "Building horon-agent from source (this can take several minutes)..."
    # .cargo/config.toml sets a POSIX shell rustc wrapper for local builds.
    # Windows cannot run it, so clear it for this build.
    $env:CARGO_BUILD_RUSTC_WRAPPER = ""
    $cargoResult = Invoke-ProcessWithTimeout -FilePath "cargo" -ArgumentList @(
        "build", "--release", "--locked", "-p", "horon-agent", "--bin", "horon-agent",
        "--manifest-path", (Join-Path $SrcDir "Cargo.toml")
    ) -TimeoutSeconds 1800 -FriendlyName "cargo-build" -CaptureOutput
    if ($cargoResult.TimedOut) {
        Write-LogTail -Path $cargoResult.StdoutPath -Label "cargo stdout"
        Write-LogTail -Path $cargoResult.StderrPath -Label "cargo stderr"
        Write-Err "cargo build timed out after 1800 seconds"
    }
    if ($cargoResult.ExitCode -ne 0) {
        Write-LogTail -Path $cargoResult.StdoutPath -Label "cargo stdout"
        Write-LogTail -Path $cargoResult.StderrPath -Label "cargo stderr"
        Write-Err "cargo build failed (exit code: $($cargoResult.ExitCode))"
    }

    $BuiltBin = Join-Path $SrcDir "target\release\horon-agent.exe"
    if (-not (Test-Path $BuiltBin)) { Write-Err "Built binary not found at $BuiltBin" }
    Copy-Item -Path $BuiltBin -Destination $DestBin -Force
    $SourceBinary = $DestBin
}

Assert-HoronAgentBinaryCandidate -BinaryPath $DestBin -ExpectedVersion $Version | Out-Null

$oldHome = $env:HORON_HOME
$oldInstallDir = $env:HORON_AGENT_INSTALL_DIR
try {
    # Windows keeps build channels under %LOCALAPPDATA%\horon-agent\builds, which
    # is the layout the existing installs, docs and verification use. Setting
    # HORON_HOME here would move them to ~/.horon/builds instead.
    $env:HORON_HOME = $null
    $env:HORON_AGENT_INSTALL_DIR = $InstallDir
    $arguments = @("install-release", "--assets", $AssetsDir, "--agent-version", $VersionNum)
    if ($SourceBinary) { $arguments += @("--source-binary", $SourceBinary) }
    & $DestBin @arguments
    if ($LASTEXITCODE -ne 0) { Write-Err "Release installation failed" }
} finally {
    $env:HORON_HOME = $oldHome
    $env:HORON_AGENT_INSTALL_DIR = $oldInstallDir
}
} finally {
    Get-Item -LiteralPath $TempDir -ErrorAction SilentlyContinue |
        Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
}

# Gracefully reload any running background server onto the freshly installed
# binary (issue #291). `server reload` only reloads a genuinely-older daemon,
# hands its live sessions to the new process, and is a no-op when nothing is
# running, so it is safe to call unconditionally. Best-effort: never fail the
# install over it.
if ($env:HORON_AGENT_SKIP_SERVER_RELOAD -ne "1") {
    try {
        & $LauncherPath server reload 2>$null | Out-Null
    } catch {
    }
}

$userPathUpdate = Set-HoronAgentUserPath -InstallDir $InstallDir
if ($userPathUpdate.Changed) {
    Write-Info "Updated user PATH with $InstallDir"
    if ($userPathUpdate.RemovedManagedEntries -gt 0) {
        Write-Info "  removed $($userPathUpdate.RemovedManagedEntries) stale horon-agent PATH entr$(if ($userPathUpdate.RemovedManagedEntries -eq 1) { 'y' } else { 'ies' })"
    }
} else {
    Write-Info "User PATH already contains $InstallDir"
}

Set-HoronAgentProcessPath -InstallDir $InstallDir | Out-Null

$shouldSetupAlacritty = [bool]($ConfigureAlacritty -and -not $SkipAlacrittySetup)

if ($ConfigureAlacritty -and $SkipAlacrittySetup) {
    Write-Warn "Both -ConfigureAlacritty and -SkipAlacrittySetup were provided; skipping Alacritty setup"
}

if ($shouldSetupAlacritty) {
    Install-Alacritty | Out-Null
} else {
    Write-Info "Optional Alacritty setup not requested"
}

$alacrittyPath = Find-AlacrittyPath

Write-Host ""
Write-Info "horon-agent $Version installed successfully!"
Write-Host ""

if ($alacrittyPath) {
    Write-Info "Alacritty ready: $alacrittyPath"
}

if (Get-Command horon-agent -ErrorAction SilentlyContinue) {
    Write-Info "Run 'horon-agent' to get started."
} else {
    Write-Host "  Open a new terminal window, then run:"
    Write-Host ""
    Write-Host "    horon-agent" -ForegroundColor Green
}
}

if ($env:HORON_AGENT_INSTALL_PS1_IMPORT_ONLY -ne "1") {
    Invoke-HoronAgentInstall
}
