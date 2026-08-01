param(
    [Parameter(Mandatory = $true)]
    [string] $Civ6Directory,

    [Parameter(Mandatory = $true)]
    [string] $RelayIp
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path -LiteralPath $Civ6Directory -PathType Container)) {
    throw "Civ VI directory does not exist: $Civ6Directory"
}

$configPath = Join-Path $Civ6Directory "injciv6-config.txt"
Set-Content -LiteralPath $configPath -Value $RelayIp -Encoding ascii -NoNewline
Write-Host "Wrote Civ VI discovery target to $configPath"
Write-Host "Run injciv6 once after Civ VI is running; do not inject the same process twice."
