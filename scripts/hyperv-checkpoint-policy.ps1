[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$VMName,

    [Parameter(Mandatory = $true)]
    [string]$BaselineName,

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
        ConfiguredAtUtc       = [DateTime]::UtcNow.ToString('o')
        Error                 = $_.Exception.ToString()
        FullyQualifiedErrorId = $_.FullyQualifiedErrorId
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $OutputPath -Encoding utf8
    exit 1
}

$vm = Get-VM -Name $VMName
$baseline = @(Get-VMSnapshot -VM $vm | Where-Object { $_.Name -ceq $BaselineName })
if ($baseline.Count -ne 1) {
    throw "Expected exactly one '$BaselineName' checkpoint on '$VMName'; found $($baseline.Count)."
}

if ($vm.CheckpointType.ToString() -ne 'Standard') {
    Set-VM -VM $vm -CheckpointType Standard
}

if ($vm.AutomaticCheckpointsEnabled) {
    Set-VM -VM $vm -AutomaticCheckpointsEnabled $false
}

$vm = Get-VM -Name $VMName
$baseline = Get-VMSnapshot -VM $vm -Name $BaselineName

[ordered]@{
    ConfiguredAtUtc             = [DateTime]::UtcNow.ToString('o')
    VMName                      = $vm.Name
    VMId                        = $vm.Id.Guid
    State                       = $vm.State.ToString()
    CheckpointType              = $vm.CheckpointType.ToString()
    AutomaticCheckpointsEnabled = $vm.AutomaticCheckpointsEnabled
    BaselineName                = $baseline.Name
    BaselineId                  = $baseline.Id.Guid
    BaselineCreationTime        = $baseline.CreationTime.ToString('o')
    BaselineParentName          = $baseline.ParentCheckpointName
} | ConvertTo-Json | Set-Content -LiteralPath $OutputPath -Encoding utf8
