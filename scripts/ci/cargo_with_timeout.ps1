param(
    [Parameter(Mandatory=$true)]
    [ValidateNotNullOrEmpty()]
    [string]$Name,

    [Parameter(Mandatory=$true)]
    [ValidateNotNullOrEmpty()]
    [string[]]$CargoArgs,

    [ValidateRange(1, 86400)]
    [int]$TimeoutSeconds = 300,

    # A cargo run whose stdout is a report to be read back (`--message-format=json`)
    # asks for the file here: `Start-Process` writes it directly, and a
    # PowerShell pipeline cannot capture a child process's console output.
    [ValidateNotNullOrEmpty()]
    [string]$StandardOutput
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

# The runner is generic so a step can apply the same timeout and failure
# reporting to a compiled test executable it starts itself (#413).
$runner = Join-Path $PSScriptRoot 'run_with_timeout.ps1'
$runnerArgs = @{
    Name           = $Name
    FilePath       = 'cargo'
    Arguments      = $CargoArgs
    TimeoutSeconds = $TimeoutSeconds
}
if ($StandardOutput) {
    $runnerArgs.StandardOutput = $StandardOutput
}

& $runner @runnerArgs
