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

$pendingId = $null
$oldBaselineId = $null
$oldBaselineRemoved = $false

trap {
    $recovery = $null
    if ($pendingId) {
        $vmForRecovery = Get-VM -Name $VMName -ErrorAction SilentlyContinue
        $pendingForRecovery = if ($vmForRecovery) {
            Get-VMSnapshot -VM $vmForRecovery -ErrorAction SilentlyContinue |
                Where-Object { $_.Id.Guid -eq $pendingId } |
                Select-Object -First 1
        }

        $namedBaseline = if ($vmForRecovery) {
            Get-VMSnapshot -VM $vmForRecovery -Name $BaselineName -ErrorAction SilentlyContinue
        }

        if ($oldBaselineRemoved -and $pendingForRecovery -and -not $namedBaseline) {
            try {
                Rename-VMSnapshot -VMSnapshot $pendingForRecovery -NewName $BaselineName -ErrorAction Stop
                $recovery = 'Renamed the verified pending checkpoint to the baseline name.'
            } catch {
                $recovery = "Automatic recovery failed: $($_.Exception.Message)"
            }
        }
    }

    [ordered]@{
        CompletedAtUtc        = [DateTime]::UtcNow.ToString('o')
        Error                 = $_.Exception.ToString()
        FullyQualifiedErrorId = $_.FullyQualifiedErrorId
        OldBaselineId         = $oldBaselineId
        OldBaselineRemoved    = $oldBaselineRemoved
        PendingId             = $pendingId
        Recovery              = $recovery
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $OutputPath -Encoding utf8
    exit 1
}

$vm = Get-VM -Name $VMName
$oldBaselines = @(Get-VMSnapshot -VM $vm | Where-Object { $_.Name -ceq $BaselineName })
if ($oldBaselines.Count -ne 1) {
    throw "Expected exactly one '$BaselineName' checkpoint on '$VMName'; found $($oldBaselines.Count)."
}

$oldBaseline = $oldBaselines[0]
$oldBaselineId = $oldBaseline.Id.Guid
$oldCreationTime = $oldBaseline.CreationTime.ToString('o')
$originalState = $vm.State.ToString()

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

$pendingName = "$BaselineName.pending.$([Guid]::NewGuid().ToString('N'))"
$pending = Checkpoint-VM -VM $vm -SnapshotName $pendingName -Passthru
$pendingId = $pending.Id.Guid

$verifiedPending = Get-VMSnapshot -VM $vm |
    Where-Object { $_.Id.Guid -eq $pendingId -and $_.Name -ceq $pendingName } |
    Select-Object -First 1
if (-not $verifiedPending) {
    throw 'The replacement checkpoint could not be verified before removing the old baseline.'
}

Remove-VMSnapshot -VMSnapshot $oldBaseline -Confirm:$false
$oldBaselineRemoved = $true

$verifiedPending = Get-VMSnapshot -VM $vm |
    Where-Object { $_.Id.Guid -eq $pendingId } |
    Select-Object -First 1
if (-not $verifiedPending) {
    throw 'The replacement checkpoint disappeared after removal of the old baseline.'
}

Rename-VMSnapshot -VMSnapshot $verifiedPending -NewName $BaselineName
$newBaseline = Get-VMSnapshot -VM $vm -Name $BaselineName
if ($newBaseline.Id.Guid -ne $pendingId) {
    throw 'The renamed baseline identity does not match the verified replacement checkpoint.'
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
    CompletedAtUtc         = [DateTime]::UtcNow.ToString('o')
    VMName                 = $vm.Name
    VMId                   = $vm.Id.Guid
    OriginalState          = $originalState
    FinalState             = $vm.State.ToString()
    BaselineName           = $newBaseline.Name
    PreviousBaselineId     = $oldBaselineId
    PreviousCreationTime   = $oldCreationTime
    ReplacementBaselineId  = $newBaseline.Id.Guid
    ReplacementCreationTime = $newBaseline.CreationTime.ToString('o')
    CheckpointType         = $vm.CheckpointType.ToString()
    AutomaticCheckpointsEnabled = $vm.AutomaticCheckpointsEnabled
} | ConvertTo-Json | Set-Content -LiteralPath $OutputPath -Encoding utf8
