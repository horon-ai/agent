param(
    [Parameter(Mandatory=$true)]
    [ValidateNotNullOrEmpty()]
    [string]$Name,

    # The executable to start. `cargo_with_timeout.ps1` passes 'cargo'; the
    # Windows targeted validation step passes a compiled libtest binary it
    # already built (#413).
    [Parameter(Mandatory=$true)]
    [ValidateNotNullOrEmpty()]
    [string]$FilePath,

    [string[]]$Arguments = @(),

    [ValidateRange(1, 86400)]
    [int]$TimeoutSeconds = 300,

    # Cargo runs a lib test executable with the working directory at the
    # package root. A caller that starts the executable itself has to say so.
    [ValidateNotNullOrEmpty()]
    [string]$WorkingDirectory,

    # `Start-Process` writes a redirected stream to the given file. A cargo
    # `--message-format=json` run needs this: its artifact report must be read
    # back, and the console cannot be captured through a PowerShell pipeline.
    [ValidateNotNullOrEmpty()]
    [string]$StandardOutput,

    [ValidateNotNullOrEmpty()]
    [string]$StandardError
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

function Stop-ProcessTree {
    param(
        [Parameter(Mandatory=$true)]
        [System.Diagnostics.Process]$Process
    )

    if ($Process.HasExited) {
        return
    }

    $taskkill = Get-Command taskkill.exe -ErrorAction SilentlyContinue
    if ($taskkill) {
        & $taskkill.Source /PID $Process.Id /T /F | ForEach-Object { Write-Host $_ }
        return
    }

    Stop-Process -Id $Process.Id -Force -ErrorAction SilentlyContinue
}

function Write-RedirectedTail {
    param(
        [Parameter(Mandatory=$true)]
        [string]$Path
    )

    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }

    $tail = Get-Content -LiteralPath $Path -Tail 120
    if ($tail) {
        Write-Host "--- tail of $Path"
        $tail | ForEach-Object { Write-Host $_ }
        Write-Host "--- end of $Path"
    }
}

if ($WorkingDirectory -and -not (Test-Path -LiteralPath $WorkingDirectory -PathType Container)) {
    throw "$Name cannot start: working directory $WorkingDirectory does not exist"
}

$startProcess = @{
    FilePath               = $FilePath
    ArgumentList           = $Arguments
    NoNewWindow            = $true
    PassThru               = $true
}
if ($WorkingDirectory) {
    $startProcess.WorkingDirectory = $WorkingDirectory
}
if ($StandardOutput) {
    $startProcess.RedirectStandardOutput = $StandardOutput
}
if ($StandardError) {
    $startProcess.RedirectStandardError = $StandardError
}

$timeoutMilliseconds = $TimeoutSeconds * 1000

Write-Host "::group::$Name"
try {
    $commandLine = "$FilePath $($Arguments -join ' ')".Trim()
    Write-Host $commandLine
    $process = Start-Process @startProcess

    if (-not $process.WaitForExit($timeoutMilliseconds)) {
        Stop-ProcessTree -Process $process
        throw "$Name timed out after $TimeoutSeconds seconds"
    }

    # With a redirected stream, `Start-Process` copies it on a background
    # reader, and `WaitForExit(milliseconds)` does not wait for that copy to
    # finish. The parameterless overload does, so the file is complete before
    # the caller reads it.
    if ($StandardOutput -or $StandardError) {
        $process.WaitForExit()
    }

    $exitCode = $process.ExitCode
    if ($exitCode -ne 0) {
        if ($StandardOutput) {
            Write-RedirectedTail -Path $StandardOutput
        }
        if ($StandardError) {
            Write-RedirectedTail -Path $StandardError
        }
        throw "$Name failed with exit code $exitCode"
    }
} finally {
    Write-Host '::endgroup::'
}
