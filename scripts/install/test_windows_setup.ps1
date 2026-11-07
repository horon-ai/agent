param()

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$repoRoot = Split-Path -Parent (Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path))
$installScript = Join-Path $repoRoot 'scripts\install\install.ps1'
$uninstallScript = Join-Path $repoRoot 'scripts\install\uninstall.ps1'
$testRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("horon-agent-windows-setup-eval-{0}" -f ([guid]::NewGuid().ToString('N')))

$envNames = @(
    'LOCALAPPDATA',
    'APPDATA',
    'USERPROFILE',
    'HORON_AGENT_HOME',
    'TEMP',
    'TMP',
    'PATH',
    'HORON_AGENT_INSTALL_PS1_IMPORT_ONLY',
    'HORON_AGENT_UNINSTALL_PS1_IMPORT_ONLY',
    'HORON_AGENT_DISABLE_ENV_BROADCAST',
    'HORON_AGENT_WINDOWS_SETUP_SKIP_EXTERNALS'
)
$originalEnv = @{}
foreach ($name in $envNames) {
    $originalEnv[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}

function Restore-TestEnvironment {
    foreach ($name in $script:envNames) {
        $value = $script:originalEnv[$name]
        if ($null -eq $value) {
            [Environment]::SetEnvironmentVariable($name, $null, 'Process')
        } else {
            [Environment]::SetEnvironmentVariable($name, $value, 'Process')
        }
    }
}

function Assert-True($Condition, [string]$Message) {
    if (-not $Condition) { throw $Message }
}

function Assert-False($Condition, [string]$Message) {
    if ($Condition) { throw $Message }
}

function Assert-Equal($Expected, $Actual, [string]$Message) {
    if ($Expected -ne $Actual) {
        throw "$Message`nExpected: $Expected`nActual:   $Actual"
    }
}

function Assert-Contains([string]$Haystack, [string]$Needle, [string]$Message) {
    if (-not $Haystack.Contains($Needle)) {
        throw "$Message`nMissing: $Needle"
    }
}

function Assert-NotContains([string]$Haystack, [string]$Needle, [string]$Message) {
    if ($Haystack.Contains($Needle)) {
        throw "$Message`nUnexpected: $Needle"
    }
}

function Assert-PathExists([string]$Path, [string]$Message) {
    Assert-True (Test-Path -LiteralPath $Path) $Message
}

function Assert-PathMissing([string]$Path, [string]$Message) {
    Assert-False (Test-Path -LiteralPath $Path) $Message
}

function Assert-PathCount([string]$PathValue, [string]$Entry, [int]$ExpectedCount, [string]$Message) {
    $entryKey = ConvertTo-HoronAgentPathKey $Entry
    $count = 0
    foreach ($candidate in (Split-HoronAgentPathList $PathValue)) {
        if ((ConvertTo-HoronAgentPathKey $candidate) -eq $entryKey) { $count += 1 }
    }
    Assert-Equal $ExpectedCount $count $Message
}

function Invoke-Case([string]$Name, [scriptblock]$Body) {
    Write-Host "CASE $Name"
    & $Body
    $script:passedCases += 1
}

function New-IsolatedWindowsProfile([string]$Name) {
    $root = Join-Path $script:testRoot $Name
    $local = Join-Path $root 'Local App Data'
    $profile = Join-Path $root 'User Profile'
    $appData = Join-Path $profile 'AppData\Roaming'
    # The uninstaller's purge guard only accepts a dedicated `.horon-agent` or
    # `horon-agent-*` directory, and `-Purge` targets this configured home, so the
    # isolated home must satisfy that contract.
    $horonAgentHome = Join-Path $root 'horon-agent home'
    $temp = Join-Path $root 'Temp'
    foreach ($dir in @($local, $profile, $appData, $horonAgentHome, $temp)) {
        New-Item -ItemType Directory -Path $dir -Force | Out-Null
    }

    $env:LOCALAPPDATA = $local
    $env:USERPROFILE = $profile
    $env:APPDATA = $appData
    $env:HORON_AGENT_HOME = $horonAgentHome
    $env:TEMP = $temp
    $env:TMP = $temp

    return [pscustomobject]@{
        Root = $root
        LocalAppData = $local
        UserProfile = $profile
        AppData = $appData
        HoronAgentHome = $horonAgentHome
        Temp = $temp
        InstallDir = Join-Path $local 'horon-agent\bin'
        LauncherPath = Join-Path $local 'horon-agent\bin\horon-agent.exe'
        BuildsDir = Join-Path $local 'horon-agent\builds'
        # Uninstaller targets left behind by earlier installers (launch hotkeys).
        SetupHintsPath = Join-Path $horonAgentHome 'setup_hints.json'
        HotkeyDir = Join-Path $horonAgentHome 'hotkey'
        StartupShortcutPath = Join-Path $appData 'Microsoft\Windows\Start Menu\Programs\Startup\horon-agent-hotkey.lnk'
    }
}

function Set-InstallScriptProfileGlobals($Profile) {
    $script:HoronAgentHome = $Profile.HoronAgentHome
}

function Get-CimInstance { @() }

$passedCases = 0
$coveredScenarios = [ordered]@{
    path_persistence_deduplication = $false
    wm_settingchange = $false
    uninstall_cleanup = $false
    rollback_failure = $false
}
New-Item -ItemType Directory -Path $testRoot -Force | Out-Null

try {
    $env:HORON_AGENT_INSTALL_PS1_IMPORT_ONLY = '1'
    . $installScript -SkipAlacrittySetup

    Invoke-Case 'release_lookup_uses_only_the_canonical_github_release' {
        Assert-Equal 'v1.2.3' (Resolve-HoronAgentReleaseTagFromUri 'https://github.com/horon-ai/agent/releases/tag/v1.2.3') 'release redirect parser should extract the stable tag'
        Assert-Equal 'v1.2.3-rc.1' (Resolve-HoronAgentReleaseTagFromUri 'https://github.com/horon-ai/agent/releases/tag/v1.2.3-rc.1?source=latest') 'release redirect parser should stop before query parameters'
        Assert-Equal $true (Test-HoronAgentReleaseTag 'v1.2.3') 'stable release tags should validate'
        Assert-Equal $false (Test-HoronAgentReleaseTag 'latest') 'unversioned release labels should not validate'
        $scriptText = Get-Content -LiteralPath $installScript -Raw
        Assert-NotContains $scriptText 'api.github.com/repos/$Repo/releases/latest' 'installer should not use the rate-limited unauthenticated GitHub API'
        Assert-NotContains $scriptText 'horon-agent.sh/releases' 'installer should not consult a static release-metadata host for release bytes'

        # The canonical HEAD redirect is the only release lookup; every asset URL
        # is derived from the tag it resolves.
        $script:releaseLookupRequests = @()
        function Invoke-WebRequest {
            param(
                [switch]$UseBasicParsing,
                [string]$Method,
                [Parameter(Mandatory = $true)][string]$Uri,
                [string]$OutFile
            )
            $script:releaseLookupRequests += "$Method $Uri"
            if ($Uri -eq 'https://github.com/horon-ai/agent/releases/latest') {
                return [pscustomobject]@{
                    BaseResponse = [pscustomobject]@{
                        ResponseUri = 'https://github.com/horon-ai/agent/releases/tag/v1.2.3'
                    }
                }
            }
            throw "unexpected URI: $Uri"
        }
        try {
            Assert-Equal 'v1.2.3' (Get-LatestHoronAgentReleaseTag) 'the GitHub latest-release redirect should resolve the version tag'
            Assert-Equal 'Head https://github.com/horon-ai/agent/releases/latest' $script:releaseLookupRequests[0] 'the release lookup should be one HEAD request to the canonical repository'
            Assert-Equal 1 $script:releaseLookupRequests.Count 'no second release host should be consulted'
        } finally {
            Remove-Item Function:\Invoke-WebRequest -ErrorAction SilentlyContinue
        }
    }

    Invoke-Case 'path_persistence_and_deduplication' {
        $profile = New-IsolatedWindowsProfile 'path-dedupe'
        $installVariant = ($profile.InstallDir.ToUpperInvariant() + '\')
        $currentPath = "C:\Tools;$installVariant;$($profile.InstallDir);C:\Tools\;C:\Other"
        $pathUpdate = Resolve-HoronAgentPathUpdate -InstallDir $profile.InstallDir -CurrentPath $currentPath
        Assert-Equal "$($profile.InstallDir);C:\Tools;C:\Tools\;C:\Other" $pathUpdate.Path 'install PATH update should prepend the canonical launcher dir without rewriting unrelated entries'
        Assert-PathCount $pathUpdate.Path $profile.InstallDir 1 'updated PATH should contain exactly one horon-agent launcher dir'
        Assert-Equal 2 $pathUpdate.RemovedManagedEntries 'PATH update should remove both stale horon-agent launcher entries before re-adding one'
        Assert-Equal 0 $pathUpdate.RemovedDuplicateEntries 'PATH update should preserve unrelated duplicate entries'

        $script:setCalls = 0
        $script:broadcastCalls = 0
        $script:appliedPath = $null
        $setPathAction = { param($value) $script:setCalls += 1; $script:appliedPath = $value }
        $broadcastAction = { $script:broadcastCalls += 1 }
        $first = Set-HoronAgentUserPath -InstallDir $profile.InstallDir -CurrentPath 'C:\Tools' -SetUserPathAction $setPathAction -BroadcastAction $broadcastAction
        Assert-Equal 1 $script:setCalls 'user PATH setter should be called once when PATH changes'
        Assert-Equal 1 $script:broadcastCalls 'environment broadcast should be called once when PATH changes'
        Assert-Equal $true $first.Broadcasted 'changed PATH update should report a broadcast'
        $second = Set-HoronAgentUserPath -InstallDir $profile.InstallDir -CurrentPath $script:appliedPath -SetUserPathAction $setPathAction -BroadcastAction $broadcastAction
        Assert-Equal 1 $script:setCalls 'user PATH setter should not be called when PATH is already correct'
        Assert-Equal 1 $script:broadcastCalls 'environment broadcast should not be called when PATH is unchanged'
        Assert-Equal $false $second.Broadcasted 'unchanged PATH update should not report a broadcast'
        $script:coveredScenarios.path_persistence_deduplication = $true
    }

    Invoke-Case 'wm_settingchange_broadcast_contract' {
        $text = Get-Content -LiteralPath $installScript -Raw
        Assert-Contains $text '0x001A' 'installer should broadcast WM_SETTINGCHANGE'
        Assert-Contains $text '"Environment"' 'installer should broadcast the Environment lParam'
        Assert-Contains $text 'SendMessageTimeout([IntPtr]0xffff' 'installer should broadcast to HWND_BROADCAST'
        $env:HORON_AGENT_DISABLE_ENV_BROADCAST = '1'
        Assert-Equal $false (Send-HoronAgentEnvironmentChangedBroadcast) 'broadcast helper should honor the deterministic no-mutate opt-out'
        $env:HORON_AGENT_DISABLE_ENV_BROADCAST = $null
        $script:coveredScenarios.wm_settingchange = $true
    }

    Invoke-Case 'launcher_rollback_failure_preserves_existing_install' {
        $profile = New-IsolatedWindowsProfile 'rollback-failure'
        New-Item -ItemType Directory -Path $profile.InstallDir -Force | Out-Null
        Set-Content -Path $profile.LauncherPath -Value 'known-good' -NoNewline
        $missingSource = Join-Path $profile.Root 'missing-source.exe'
        $threw = $false
        try {
            Install-HoronAgentLauncher -SourcePath $missingSource -LauncherPath $profile.LauncherPath | Out-Null
        } catch {
            $threw = $true
        }
        Assert-Equal $true $threw 'launcher install should surface copy failures'
        Assert-Equal 'known-good' (Get-Content -LiteralPath $profile.LauncherPath -Raw) 'failed launcher install should preserve the existing launcher'
        $tempLaunchers = @(Get-ChildItem -LiteralPath $profile.InstallDir -Filter '.horon-agent-launcher-*.tmp.exe' -Force -ErrorAction SilentlyContinue)
        Assert-Equal 0 $tempLaunchers.Count 'failed launcher install should not leave temporary launcher files behind'
        $script:coveredScenarios.rollback_failure = $true
    }

    $env:HORON_AGENT_UNINSTALL_PS1_IMPORT_ONLY = '1'
    . $uninstallScript

    function Get-CimInstance { @() }
    $script:uninstallUserPath = $null
    $script:uninstallSetCalls = 0
    $script:uninstallBroadcasts = 0
    function Remove-HoronAgentUserPath {
        param(
            [Parameter(Mandatory = $true)][string]$InstallDir,
            [AllowNull()][string]$CurrentPath,
            [scriptblock]$SetUserPathAction,
            [scriptblock]$BroadcastAction,
            [bool]$Broadcast = $true
        )
        $update = Resolve-HoronAgentPathRemoval -InstallDir $InstallDir -CurrentPath $script:uninstallUserPath
        if ($update.Changed) {
            $script:uninstallUserPath = $update.Path
            $script:uninstallSetCalls += 1
            if ($Broadcast) { $script:uninstallBroadcasts += 1 }
        }
        $update | Add-Member -NotePropertyName Broadcasted -NotePropertyValue ([bool]($update.Changed -and $Broadcast))
        return $update
    }

    Invoke-Case 'uninstall_guards_purge_paths_and_process_scope' {
        $profile = New-IsolatedWindowsProfile 'uninstall-safety'
        Assert-Equal $true (Test-HoronAgentSafePurgePath $profile.HoronAgentHome) 'dedicated horon-agent data directories should be purgeable'
        Assert-Equal $false (Test-HoronAgentSafePurgePath $profile.UserProfile) 'the user profile must never be accepted as a purge target'
        Assert-Equal $false (Test-HoronAgentSafePurgePath $profile.Root) 'a parent workspace must never be accepted as a purge target'
        Assert-Equal $true (Test-HoronAgentManagedExecutablePath -ExecutablePath $profile.LauncherPath -LauncherPath $profile.LauncherPath -BuildsDir $profile.BuildsDir) 'the installed launcher should be recognized as managed'
        Assert-Equal $true (Test-HoronAgentManagedExecutablePath -ExecutablePath (Join-Path $profile.InstallDir '.horon-agent-launcher-old-a1b2c3.exe') -LauncherPath $profile.LauncherPath -BuildsDir $profile.BuildsDir) 'a renamed live-upgrade launcher should be recognized as managed'
        Assert-Equal $false (Test-HoronAgentManagedExecutablePath -ExecutablePath (Join-Path $profile.InstallDir 'other-tool.exe') -LauncherPath $profile.LauncherPath -BuildsDir $profile.BuildsDir) 'unrelated executables beside the launcher must not be terminated'
        Assert-Equal $true (Test-HoronAgentManagedExecutablePath -ExecutablePath (Join-Path $profile.BuildsDir 'stable\horon-agent.exe') -LauncherPath $profile.LauncherPath -BuildsDir $profile.BuildsDir) 'installed version binaries should be recognized as managed'
        Assert-Equal $false (Test-HoronAgentManagedExecutablePath -ExecutablePath (Join-Path $profile.Root 'development\horon-agent.exe') -LauncherPath $profile.LauncherPath -BuildsDir $profile.BuildsDir) 'unrelated development binaries must not be terminated'
    }

    Invoke-Case 'uninstall_cleanup_removes_binaries_path_and_keeps_user_data' {
        $profile = New-IsolatedWindowsProfile 'uninstall-cleanup'
        New-Item -ItemType Directory -Path $profile.InstallDir -Force | Out-Null
        New-Item -ItemType Directory -Path (Join-Path $profile.BuildsDir 'stable') -Force | Out-Null
        New-Item -ItemType Directory -Path $profile.HoronAgentHome -Force | Out-Null
        New-Item -ItemType Directory -Path $profile.HotkeyDir -Force | Out-Null
        New-Item -ItemType Directory -Path (Split-Path -Parent $profile.StartupShortcutPath) -Force | Out-Null
        Set-Content -Path $profile.LauncherPath -Value 'installed launcher' -NoNewline
        $oldLauncherPath = Join-Path $profile.InstallDir '.horon-agent-launcher-old-a1b2c3.exe'
        Set-Content -Path $oldLauncherPath -Value 'previous running launcher' -NoNewline
        Set-Content -Path (Join-Path $profile.BuildsDir 'stable\horon-agent.exe') -Value 'stable build' -NoNewline
        Set-Content -Path (Join-Path $profile.HoronAgentHome 'config.toml') -Value 'kept = true' -NoNewline
        Set-Content -Path (Join-Path $profile.HotkeyDir 'horon-agent-hotkey.ps1') -Value 'legacy listener' -NoNewline
        Set-Content -Path $profile.StartupShortcutPath -Value 'startup shortcut' -NoNewline
        @{ hotkey_configured = $true; hotkey_dismissed = $false } | ConvertTo-Json | Set-Content -Path $profile.SetupHintsPath -Encoding UTF8
        $installVariant = ($profile.InstallDir.ToUpperInvariant() + '\')
        $script:uninstallUserPath = "$($profile.InstallDir);C:\Keep;$installVariant"
        $script:uninstallSetCalls = 0
        $script:uninstallBroadcasts = 0

        $exitCode = Invoke-HoronAgentUninstall -InstallDir $profile.InstallDir -Yes
        Assert-Equal 0 $exitCode 'uninstall should complete successfully in the isolated profile'
        Assert-PathMissing $profile.LauncherPath 'uninstall should remove the launcher'
        Assert-PathMissing $oldLauncherPath 'uninstall should remove renamed live-upgrade launchers'
        Assert-PathMissing $profile.InstallDir 'uninstall should remove the empty launcher directory'
        Assert-PathMissing $profile.BuildsDir 'uninstall should remove installed build binaries'
        Assert-PathMissing $profile.StartupShortcutPath 'uninstall should remove the launch-hotkey Startup shortcut'
        Assert-PathMissing (Join-Path $profile.HotkeyDir 'horon-agent-hotkey.ps1') 'uninstall should remove legacy launch-hotkey artifacts'
        Assert-PathExists (Join-Path $profile.HoronAgentHome 'config.toml') 'uninstall without -Purge should keep user data'
        $setupHints = Get-Content -LiteralPath $profile.SetupHintsPath -Raw | ConvertFrom-Json
        Assert-Equal $false $setupHints.hotkey_configured 'uninstall should clear the persisted hotkey-configured state'
        Assert-Equal $true $setupHints.hotkey_dismissed 'uninstall should keep the removed hotkey prompt dismissed'
        Assert-Equal 'C:\Keep' $script:uninstallUserPath 'uninstall should remove all horon-agent-managed PATH variants and keep unrelated entries'
        Assert-Equal 1 $script:uninstallSetCalls 'uninstall should write mocked user PATH once when cleanup changes it'
        Assert-Equal 1 $script:uninstallBroadcasts 'uninstall should broadcast once after PATH cleanup'
        $script:coveredScenarios.uninstall_cleanup = $true
    }

    $missingScenarios = @($coveredScenarios.GetEnumerator() | Where-Object { -not $_.Value } | ForEach-Object { $_.Key })
    Assert-Equal 0 $missingScenarios.Count ("missing scenario coverage: {0}" -f ($missingScenarios -join ', '))
    Write-Host "Scenario checklist: $($coveredScenarios.Count)/$($coveredScenarios.Count) requested Windows setup scenarios covered." -ForegroundColor Green
    Write-Host "All $passedCases Windows setup evaluation cases passed." -ForegroundColor Green
} finally {
    Restore-TestEnvironment
    Remove-Item -LiteralPath $testRoot -Recurse -Force -ErrorAction SilentlyContinue
}
