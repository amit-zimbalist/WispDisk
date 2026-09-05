[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$VMName,

    [Parameter(Mandatory = $true)]
    [string]$GuestUserName,

    [Parameter(Mandatory = $true)]
    [string]$GuestRunDirectory,

    [Parameter(Mandatory = $true)]
    [string]$HostEvidenceDirectory,

    [Parameter(Mandatory = $true)]
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Path $HostEvidenceDirectory -Force | Out-Null

$credential = Get-Credential -UserName $GuestUserName -Message "Enter the administrator password for Hyper-V guest '$VMName'"
if (-not $credential) {
    throw 'Guest credentials were not supplied.'
}

$session = $null
try {
    $session = New-PSSession -VMName $VMName -Credential $credential
    $guestEvidenceDirectory = Join-Path $GuestRunDirectory 'failure-evidence'
    $guestResult = Invoke-Command -Session $session -ArgumentList $guestEvidenceDirectory -ScriptBlock {
        param([string]$EvidenceDirectory)

        if (-not (Test-Path -LiteralPath $EvidenceDirectory)) {
            New-Item -ItemType Directory -Path $EvidenceDirectory | Out-Null
        }

        $verifierActive = @(& verifier.exe /query 2>&1 | ForEach-Object { $_.ToString() })
        $verifierSettings = @(& verifier.exe /querysettings 2>&1 | ForEach-Object { $_.ToString() })
        $verifierActive | Set-Content -LiteralPath (Join-Path $EvidenceDirectory 'verifier-query.txt') -Encoding utf8
        $verifierSettings | Set-Content -LiteralPath (Join-Path $EvidenceDirectory 'verifier-querysettings.txt') -Encoding utf8

        $pnpDevices = @(Get-PnpDevice -ErrorAction SilentlyContinue |
            Where-Object {
                $_.InstanceId -like 'ROOT\WISPDISK*' -or
                $_.FriendlyName -like '*WispDisk*' -or
                $_.InstanceId -like 'SCSI\DISK&VEN_WISPDISK*'
            } |
            ForEach-Object {
                [ordered]@{
                    Present      = $_.Present
                    Class        = $_.Class
                    FriendlyName = $_.FriendlyName
                    InstanceId   = $_.InstanceId
                    Status       = $_.Status
                    Problem      = if ($null -ne $_.Problem) { $_.Problem.ToString() } else { $null }
                }
            })

        $disks = @(Get-Disk -ErrorAction SilentlyContinue |
            Where-Object { $_.FriendlyName -like '*WispDisk*' } |
            ForEach-Object {
                [ordered]@{
                    Number            = $_.Number
                    FriendlyName      = $_.FriendlyName
                    SerialNumber      = $_.SerialNumber
                    UniqueId          = $_.UniqueId
                    BusType           = $_.BusType.ToString()
                    PartitionStyle    = $_.PartitionStyle.ToString()
                    OperationalStatus = @($_.OperationalStatus | ForEach-Object { $_.ToString() })
                    HealthStatus      = $_.HealthStatus.ToString()
                    Size              = [uint64]$_.Size
                    IsOffline         = $_.IsOffline
                    IsReadOnly        = $_.IsReadOnly
                }
            })

        $volumes = @(Get-Volume -ErrorAction SilentlyContinue |
            ForEach-Object {
                [ordered]@{
                    DriveLetter     = if ($_.DriveLetter) { $_.DriveLetter.ToString() } else { $null }
                    FileSystemLabel = $_.FileSystemLabel
                    FileSystem      = $_.FileSystem
                    DriveType       = $_.DriveType.ToString()
                    HealthStatus    = $_.HealthStatus.ToString()
                    Size            = [uint64]$_.Size
                    Path            = $_.Path
                }
            })

        $partitions = @(Get-Partition -ErrorAction SilentlyContinue |
            ForEach-Object {
                [ordered]@{
                    DiskNumber      = $_.DiskNumber
                    PartitionNumber = $_.PartitionNumber
                    DriveLetter     = if ($_.DriveLetter) { $_.DriveLetter.ToString() } else { $null }
                    Type            = $_.Type
                    Size            = [uint64]$_.Size
                    AccessPaths     = @($_.AccessPaths)
                }
            })

        $driver = Get-CimInstance -ClassName Win32_SystemDriver -Filter "Name='WispDisk'" -ErrorAction SilentlyContinue
        if (-not $driver) {
            $driver = Get-CimInstance -ClassName Win32_SystemDriver -Filter "Name='WispDiskVmp'" -ErrorAction SilentlyContinue
        }

        $dumpFiles = @()
        $memoryDumpPath = Join-Path $env:SystemRoot 'MEMORY.DMP'
        if (Test-Path -LiteralPath $memoryDumpPath -PathType Leaf) {
            $item = Get-Item -LiteralPath $memoryDumpPath
            $destination = Join-Path $EvidenceDirectory 'MEMORY.DMP'
            Copy-Item -LiteralPath $memoryDumpPath -Destination $destination -Force
            $dumpFiles += [ordered]@{
                Name          = 'MEMORY.DMP'
                Length        = $item.Length
                LastWriteTime = $item.LastWriteTimeUtc.ToString('o')
            }
        }
        $minidumpDirectory = Join-Path $env:SystemRoot 'Minidump'
        if (Test-Path -LiteralPath $minidumpDirectory) {
            Get-ChildItem -LiteralPath $minidumpDirectory -File -ErrorAction SilentlyContinue | ForEach-Object {
                Copy-Item -LiteralPath $_.FullName -Destination $EvidenceDirectory -Force
                $dumpFiles += [ordered]@{
                    Name          = $_.Name
                    Length        = $_.Length
                    LastWriteTime = $_.LastWriteTimeUtc.ToString('o')
                }
            }
        }

        $setupApiPath = Join-Path $env:SystemRoot 'inf\setupapi.dev.log'
        if (Test-Path -LiteralPath $setupApiPath -PathType Leaf) {
            Copy-Item -LiteralPath $setupApiPath -Destination (Join-Path $EvidenceDirectory 'setupapi.dev.log') -Force
        }
        & wevtutil.exe epl System (Join-Path $EvidenceDirectory 'System.evtx') /ow:true
        if ($LASTEXITCODE -ne 0) { throw 'System event-log export failed.' }
        & wevtutil.exe epl Application (Join-Path $EvidenceDirectory 'Application.evtx') /ow:true
        if ($LASTEXITCODE -ne 0) { throw 'Application event-log export failed.' }

        $state = [ordered]@{
            CapturedAtUtc    = [DateTime]::UtcNow.ToString('o')
            ComputerName     = $env:COMPUTERNAME
            VerifierActive   = $verifierActive
            VerifierSettings = $verifierSettings
            Driver           = if ($driver) {
                [ordered]@{
                    Name      = $driver.Name
                    State     = $driver.State
                    StartMode = $driver.StartMode
                    PathName  = $driver.PathName
                }
            } else { $null }
            PnpDevices       = $pnpDevices
            Disks            = $disks
            Volumes          = $volumes
            Partitions       = $partitions
            DumpFiles        = $dumpFiles
        }
        $state | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $EvidenceDirectory 'current-state.json') -Encoding utf8
        $state
    }

    Copy-Item -FromSession $session -Path (Join-Path $guestEvidenceDirectory '*') -Destination $HostEvidenceDirectory -Recurse -Force
    [ordered]@{
        CollectedAtUtc       = [DateTime]::UtcNow.ToString('o')
        VMName               = $VMName
        GuestEvidencePath    = $guestEvidenceDirectory
        HostEvidencePath     = $HostEvidenceDirectory
        Guest                = $guestResult
    } | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $OutputPath -Encoding utf8
} catch {
    [ordered]@{
        CollectedAtUtc        = [DateTime]::UtcNow.ToString('o')
        VMName                = $VMName
        Error                 = $_.Exception.ToString()
        FullyQualifiedErrorId = $_.FullyQualifiedErrorId
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $OutputPath -Encoding utf8
    exit 1
} finally {
    if ($session) {
        Remove-PSSession -Session $session -ErrorAction SilentlyContinue
    }
}
