[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
$isAdministrator = $principal.IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator
)

$vms = @(Get-VM | ForEach-Object {
    $vm = $_
    $firmware = if ($vm.Generation -eq 2) {
        Get-VMFirmware -VM $vm
    } else {
        $null
    }
    $snapshots = @(Get-VMSnapshot -VM $vm -ErrorAction SilentlyContinue | ForEach-Object {
        [ordered]@{
            Name                 = $_.Name
            Id                   = $_.Id.Guid
            CreationTime         = $_.CreationTime.ToString('o')
            ParentCheckpointName = $_.ParentCheckpointName
            CheckpointType       = $_.CheckpointType.ToString()
        }
    })

    $networkAdapters = @(Get-VMNetworkAdapter -VM $vm | ForEach-Object {
        [ordered]@{
            Name       = $_.Name
            SwitchName = $_.SwitchName
            MacAddress = $_.MacAddress
            Status     = $_.Status.ToString()
            IPAddresses = @($_.IPAddresses)
        }
    })

    $integrationServices = @(Get-VMIntegrationService -VM $vm | ForEach-Object {
        [ordered]@{
            Name                     = $_.Name
            Enabled                  = $_.Enabled
            PrimaryStatusDescription = $_.PrimaryStatusDescription
        }
    })

    [ordered]@{
        Name                        = $vm.Name
        Id                          = $vm.Id.Guid
        State                       = $vm.State.ToString()
        Generation                  = $vm.Generation
        Version                     = $vm.Version.ToString()
        CheckpointType              = $vm.CheckpointType.ToString()
        AutomaticCheckpointsEnabled = $vm.AutomaticCheckpointsEnabled
        SecureBootEnabled           = if ($firmware) { $firmware.SecureBoot } else { $null }
        Snapshots                   = $snapshots
        NetworkAdapters             = $networkAdapters
        IntegrationServices         = $integrationServices
    }
})

$switches = @(Get-VMSwitch | ForEach-Object {
    [ordered]@{
        Name                    = $_.Name
        Id                      = $_.Id.Guid
        SwitchType              = $_.SwitchType.ToString()
        NetAdapterInterfaceGuid = $_.NetAdapterInterfaceGuid.Guid
    }
})

$inventory = [ordered]@{
    ComputerName    = $env:COMPUTERNAME
    UserName        = $identity.Name
    IsAdministrator = $isAdministrator
    CapturedAtUtc   = [DateTime]::UtcNow.ToString('o')
    VMs             = $vms
    Switches        = $switches
}

$outputDirectory = Split-Path -Parent $OutputPath
if ($outputDirectory) {
    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
}

$inventory | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $OutputPath -Encoding utf8
