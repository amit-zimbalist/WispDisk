[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$VMName,

    [Parameter(Mandatory = $true)]
    [string]$BaselineName,

    [Parameter(Mandatory = $true)]
    [string]$ExpectedBaselineId,

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
        CompletedAtUtc        = [DateTime]::UtcNow.ToString('o')
        Error                 = $_.Exception.ToString()
        FullyQualifiedErrorId = $_.FullyQualifiedErrorId
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $OutputPath -Encoding utf8
    exit 1
}

$vm = Get-VM -Name $VMName
$baselines = @(Get-VMSnapshot -VM $vm | Where-Object { $_.Name -ceq $BaselineName })
if ($baselines.Count -ne 1) {
    throw "Expected exactly one '$BaselineName' checkpoint on '$VMName'; found $($baselines.Count)."
}

$baseline = $baselines[0]
if ($baseline.Id.Guid -ne $ExpectedBaselineId) {
    throw "Baseline ID mismatch: expected '$ExpectedBaselineId', found '$($baseline.Id.Guid)'."
}

if ($vm.State.ToString() -ne 'Off') {
    Stop-VM -VM $vm -Confirm:$false
    $deadline = [DateTime]::UtcNow.AddSeconds(60)
    do {
        Start-Sleep -Seconds 2
        $vm = Get-VM -Name $VMName
    } while ($vm.State.ToString() -ne 'Off' -and [DateTime]::UtcNow -lt $deadline)
    if ($vm.State.ToString() -ne 'Off') {
        throw "Guest did not shut down cleanly within 60 seconds; current state is '$($vm.State)'."
    }
}

Restore-VMSnapshot -VMSnapshot $baseline -Confirm:$false
$vm = Get-VM -Name $VMName
if ($vm.State.ToString() -ne 'Off') {
    throw "Expected the restored clean checkpoint to leave the guest off; state is '$($vm.State)'."
}

Start-VM -VM $vm | Out-Null
$deadline = [DateTime]::UtcNow.AddSeconds(60)
do {
    Start-Sleep -Seconds 2
    $vm = Get-VM -Name $VMName
} while ($vm.State.ToString() -ne 'Running' -and [DateTime]::UtcNow -lt $deadline)
if ($vm.State.ToString() -ne 'Running') {
    throw "Guest did not reach Running state within 60 seconds; current state is '$($vm.State)'."
}

[ordered]@{
    CompletedAtUtc = [DateTime]::UtcNow.ToString('o')
    VMName         = $vm.Name
    VMId           = $vm.Id.Guid
    State          = $vm.State.ToString()
    BaselineName   = $baseline.Name
    BaselineId     = $baseline.Id.Guid
} | ConvertTo-Json | Set-Content -LiteralPath $OutputPath -Encoding utf8
