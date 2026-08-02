[CmdletBinding()]
param(
    [string]$InstallRoot = (Split-Path -Parent $PSScriptRoot),
    [string]$ClientExe,
    [string]$Civ6Exe,
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"
$rulePrefix = "Civ6 LAN Bridge"

function Assert-Administrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw "This installer must run from an elevated PowerShell session."
    }
}

function Remove-BridgeRules {
    Get-NetFirewallRule -DisplayName "$rulePrefix*" -ErrorAction SilentlyContinue |
        Remove-NetFirewallRule -ErrorAction SilentlyContinue
}

Assert-Administrator

if ($Uninstall) {
    Remove-BridgeRules
    Write-Host "Removed Civ6 LAN Bridge firewall rules."
    exit 0
}

Remove-BridgeRules

if (-not $ClientExe) {
    $ClientExe = Get-ChildItem -LiteralPath $InstallRoot -File -Filter "*.exe" |
        Where-Object { $_.Name -notmatch "^(unins|uninstall|WebView2Loader)" } |
        Select-Object -First 1 -ExpandProperty FullName
}

if (-not (Test-Path -LiteralPath $ClientExe -PathType Leaf)) {
    throw "Client executable was not found: $ClientExe"
}

# The relay socket is scoped to the installed bridge process. Private and
# Domain profiles are the default; Public is deliberately not opened.
New-NetFirewallRule `
    -DisplayName "$rulePrefix Relay Inbound" `
    -Direction Inbound -Action Allow -Enabled True `
    -Profile Domain,Private -Protocol UDP -LocalPort 32000 `
    -Program $ClientExe | Out-Null

New-NetFirewallRule `
    -DisplayName "$rulePrefix Relay Outbound" `
    -Direction Outbound -Action Allow -Enabled True `
    -Profile Domain,Private -Protocol UDP -RemotePort 32000 `
    -Program $ClientExe | Out-Null

if (-not $Civ6Exe) {
    $candidates = @(
        "$env:ProgramFiles\Steam\steamapps\common\Sid Meier's Civilization VI\Base\Binaries\Win64Steam\CivilizationVI.exe",
        "${env:ProgramFiles(x86)}\Steam\steamapps\common\Sid Meier's Civilization VI\Base\Binaries\Win64Steam\CivilizationVI.exe"
    )
    $Civ6Exe = $candidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
}

if ($Civ6Exe -and (Test-Path -LiteralPath $Civ6Exe -PathType Leaf)) {
    # These rules cover Civ6's local UDP sockets. The WFP callout still owns
    # the broadcast rewrite; firewall rules do not replace interception.
    New-NetFirewallRule `
        -DisplayName "$rulePrefix Civ6 Discovery Inbound" `
        -Direction Inbound -Action Allow -Enabled True `
        -Profile Domain,Private -Protocol UDP -LocalPort "62900-62999" `
        -Program $Civ6Exe | Out-Null
    New-NetFirewallRule `
        -DisplayName "$rulePrefix Civ6 Discovery Outbound" `
        -Direction Outbound -Action Allow -Enabled True `
        -Profile Domain,Private -Protocol UDP -RemotePort "62900-62999" `
        -Program $Civ6Exe | Out-Null
    New-NetFirewallRule `
        -DisplayName "$rulePrefix Civ6 Gameplay Inbound" `
        -Direction Inbound -Action Allow -Enabled True `
        -Profile Domain,Private -Protocol UDP -LocalPort 62056 `
        -Program $Civ6Exe | Out-Null
    New-NetFirewallRule `
        -DisplayName "$rulePrefix Civ6 Gameplay Outbound" `
        -Direction Outbound -Action Allow -Enabled True `
        -Profile Domain,Private -Protocol UDP -RemotePort 62056 `
        -Program $Civ6Exe | Out-Null
    Write-Host "Added relay and Civ6 UDP rules for $Civ6Exe."
} else {
    Write-Warning "Civ6.exe was not found; relay rules were added, but Civ6-specific rules were skipped. Re-run with -Civ6Exe after installing the game."
}
