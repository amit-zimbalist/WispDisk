[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$VMName,

    [Parameter(Mandatory = $true)]
    [string]$HostIPv4,

    [string]$GuestUserName,

    [Parameter(Mandatory = $true)]
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'
$outputDirectory = Split-Path -Parent $OutputPath
if ($outputDirectory) {
    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
}

$credentialParameters = @{
    Message = "Enter an administrator account for Hyper-V guest '$VMName'"
}
if ($GuestUserName) {
    $credentialParameters.UserName = $GuestUserName
}
$credential = Get-Credential @credentialParameters
if (-not $credential) {
    throw 'Guest credentials were not supplied.'
}

$session = $null
try {
    $session = New-PSSession -VMName $VMName -Credential $credential
    $guest = Invoke-Command -Session $session -ArgumentList $HostIPv4 -ScriptBlock {
        param([string]$DebuggerHostIPv4)

        $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
        $principal = [Security.Principal.WindowsPrincipal]::new($identity)
        $isAdministrator = $principal.IsInRole(
            [Security.Principal.WindowsBuiltInRole]::Administrator
        )

        $os = Get-CimInstance -ClassName Win32_OperatingSystem
        $computerSystem = Get-CimInstance -ClassName Win32_ComputerSystem
        $systemDrive = Get-CimInstance -ClassName Win32_LogicalDisk -Filter "DeviceID='$($env:SystemDrive)'"
        $crashControl = Get-ItemProperty -LiteralPath 'HKLM:\SYSTEM\CurrentControlSet\Control\CrashControl'

        $bitLocker = $null
        if (Get-Command Get-BitLockerVolume -ErrorAction SilentlyContinue) {
            $volume = Get-BitLockerVolume -MountPoint $env:SystemDrive -ErrorAction SilentlyContinue
            if ($volume) {
                $bitLocker = [ordered]@{
                    MountPoint       = $volume.MountPoint
                    VolumeStatus     = $volume.VolumeStatus.ToString()
                    ProtectionStatus = $volume.ProtectionStatus.ToString()
                    LockStatus       = $volume.LockStatus.ToString()
                }
            }
        }

        $secureBoot = $null
        try {
            $secureBoot = Confirm-SecureBootUEFI
        } catch {
            $secureBoot = "Unavailable: $($_.Exception.Message)"
        }

        $pingSucceeded = Test-Connection -ComputerName $DebuggerHostIPv4 -Count 2 -Quiet

        [ordered]@{
            ComputerName            = $env:COMPUTERNAME
            UserName                = $identity.Name
            IsAdministrator         = $isAdministrator
            OS                      = [ordered]@{
                Caption     = $os.Caption
                Version     = $os.Version
                BuildNumber = $os.BuildNumber
                OSArchitecture = $os.OSArchitecture
            }
            SecureBootEnabled       = $secureBoot
            BitLocker               = $bitLocker
            HostIPv4                = $DebuggerHostIPv4
            HostPingSucceeded       = $pingSucceeded
            AutomaticManagedPagefile = $computerSystem.AutomaticManagedPagefile
            PageFileUsage           = @(Get-CimInstance -ClassName Win32_PageFileUsage | Select-Object Name, AllocatedBaseSize, CurrentUsage, PeakUsage)
            SystemDrive             = [ordered]@{
                DeviceID  = $systemDrive.DeviceID
                SizeBytes = [uint64]$systemDrive.Size
                FreeBytes = [uint64]$systemDrive.FreeSpace
            }
            CrashControl            = [ordered]@{
                CrashDumpEnabled     = $crashControl.CrashDumpEnabled
                DumpFile             = $crashControl.DumpFile
                MinidumpDir          = $crashControl.MinidumpDir
                LogEvent             = $crashControl.LogEvent
                AutoReboot           = $crashControl.AutoReboot
                Overwrite            = $crashControl.Overwrite
                AlwaysKeepMemoryDump = $crashControl.AlwaysKeepMemoryDump
            }
            MemoryDumpExists        = Test-Path -LiteralPath "$env:SystemRoot\MEMORY.DMP"
            MinidumpDirectoryExists = Test-Path -LiteralPath "$env:SystemRoot\Minidump"
            KDNETPresent            = Test-Path -LiteralPath 'C:\KDNET\kdnet.exe'
            NetworkAdapters         = @(Get-NetAdapter | Select-Object Name, InterfaceDescription, Status, MacAddress, LinkSpeed)
            IPv4Addresses           = @(Get-NetIPAddress -AddressFamily IPv4 | ForEach-Object {
                [ordered]@{
                    InterfaceAlias = $_.InterfaceAlias
                    IPAddress      = $_.IPAddress
                    PrefixLength   = $_.PrefixLength
                    AddressState   = $_.AddressState.ToString()
                }
            })
            BcdCurrent              = @(& bcdedit.exe /enum '{current}' 2>&1 | ForEach-Object { $_.ToString() })
            BcdDebugSettings        = @(& bcdedit.exe /dbgsettings 2>&1 | ForEach-Object { $_.ToString() })
            VerifierActive          = @(& verifier.exe /query 2>&1 | ForEach-Object { $_.ToString() })
            VerifierSettings        = @(& verifier.exe /querysettings 2>&1 | ForEach-Object { $_.ToString() })
        }
    }

    $result = [ordered]@{
        VMName        = $VMName
        CapturedAtUtc = [DateTime]::UtcNow.ToString('o')
        Guest         = $guest
    }

    $result | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $OutputPath -Encoding utf8
} catch {
    $failure = [ordered]@{
        VMName        = $VMName
        CapturedAtUtc = [DateTime]::UtcNow.ToString('o')
        Error         = $_.Exception.ToString()
        FullyQualifiedErrorId = $_.FullyQualifiedErrorId
    }
    $failure | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $OutputPath -Encoding utf8
    exit 1
} finally {
    if ($session) {
        Remove-PSSession -Session $session
    }
}
