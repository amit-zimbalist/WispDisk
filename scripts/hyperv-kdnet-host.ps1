[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$GuestAuditPath,

    [Parameter(Mandatory = $true)]
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'
$outputDirectory = Split-Path -Parent $OutputPath
if ($outputDirectory) {
    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
}

trap {
    [ordered]@{
        ConfiguredAtUtc      = [DateTime]::UtcNow.ToString('o')
        Error                = $_.Exception.ToString()
        FullyQualifiedErrorId = $_.FullyQualifiedErrorId
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $OutputPath -Encoding utf8
    exit 1
}

$audit = Get-Content -LiteralPath $GuestAuditPath -Raw | ConvertFrom-Json
$settings = @($audit.Guest.BcdDebugSettings)

function Get-KdnetSetting {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Name
    )

    $line = $settings | Where-Object { $_ -match "^$([regex]::Escape($Name))\s+(.+)$" } | Select-Object -First 1
    if (-not $line) {
        throw "KDNET setting '$Name' was not found in the guest audit."
    }
    return ([regex]::Match($line, "^$([regex]::Escape($Name))\s+(.+)$")).Groups[1].Value.Trim()
}

$hostIPv4 = Get-KdnetSetting -Name 'hostip'
$portText = Get-KdnetSetting -Name 'port'
$key = Get-KdnetSetting -Name 'key'
$port = 0
if (-not [int]::TryParse($portText, [ref]$port) -or $port -lt 49152 -or $port -gt 65535) {
    if ($portText -ne '5364') {
        throw "KDNET port '$portText' is invalid."
    }
    $port = 5364
}

$hostAddress = Get-NetIPAddress -AddressFamily IPv4 -IPAddress $hostIPv4 -ErrorAction SilentlyContinue
if (-not $hostAddress) {
    throw "Configured KDNET host address '$hostIPv4' is not assigned to this host."
}

$kdPath = 'C:\Program Files (x86)\Windows Kits\10\Debuggers\x64\kd.exe'
if (-not (Test-Path -LiteralPath $kdPath -PathType Leaf)) {
    throw "Kernel debugger not found at '$kdPath'."
}

$ruleName = "WispDisk KDNET UDP $port"
$rule = Get-NetFirewallRule -DisplayName $ruleName -ErrorAction SilentlyContinue
if (-not $rule) {
    $ruleParameters = @{
        DisplayName = $ruleName
        Group       = 'WispDisk Driver Testing'
        Direction   = 'Inbound'
        Action      = 'Allow'
        Enabled     = 'True'
        Profile     = 'Any'
        Protocol    = 'UDP'
        LocalPort   = $port
        Program     = $kdPath
    }
    $rule = New-NetFirewallRule @ruleParameters
} else {
    $rule | Set-NetFirewallRule -Enabled True -Action Allow -Profile Any | Out-Null
}

[ordered]@{
    ConfiguredAtUtc = [DateTime]::UtcNow.ToString('o')
    HostIPv4        = $hostIPv4
    Port            = $port
    Key             = $key
    DebuggerPath    = $kdPath
    FirewallRule    = $ruleName
} | ConvertTo-Json | Set-Content -LiteralPath $OutputPath -Encoding utf8
