[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$VMName,

    [Parameter(Mandatory = $true)]
    [string]$GuestUserName,

    [Parameter(Mandatory = $true)]
    [string]$GuestRunDirectory,

    [Parameter(Mandatory = $true)]
    [string]$OutputPath
)

$ErrorActionPreference = 'Stop'
$outputDirectory = Split-Path -Parent $OutputPath
if ($outputDirectory) {
    New-Item -ItemType Directory -Path $outputDirectory -Force | Out-Null
}

$credential = Get-Credential -UserName $GuestUserName -Message "Enter the administrator password for Hyper-V guest '$VMName'"
if (-not $credential) {
    throw 'Guest credentials were not supplied.'
}

$session = $null
try {
    $session = New-PSSession -VMName $VMName -Credential $credential
    $guestResult = Invoke-Command -Session $session -ArgumentList $GuestRunDirectory -ScriptBlock {
        param([string]$RunDirectory)

        $startedAt = Get-Date
        $stage = 'preflight'
        $driveLetter = $null
        $diskIdentity = $null
        $result = [ordered]@{
            Success            = $false
            StartedAt          = $startedAt.ToUniversalTime().ToString('o')
            CompletedAt        = $null
            Stage              = $stage
            DriveLetter        = $null
            Add                = $null
            Volume             = $null
            Disk               = $null
            DataHashBeforeRead = $null
            DataHashAfterRead  = $null
            Delete             = $null
            DeviceBefore       = @()
            DeviceAfterAdd     = @()
            DeviceAfterDelete  = @()
            SystemEvents       = @()
            SetupApiExcerpt    = @()
            Error              = $null
        }

        function Invoke-WispDiskCli {
            param([string[]]$Arguments)
            $lines = @(& (Join-Path $RunDirectory 'wispdisk.exe') @Arguments 2>&1 |
                ForEach-Object { $_.ToString() })
            [ordered]@{
                Arguments = @($Arguments)
                ExitCode  = $LASTEXITCODE
                Output    = $lines
            }
        }

        function Get-WispDiskDevices {
            @(Get-PnpDevice -PresentOnly -ErrorAction SilentlyContinue |
                Where-Object {
                    $_.InstanceId -like 'ROOT\WISPDISK*' -or
                    $_.FriendlyName -like '*WispDisk*' -or
                    $_.InstanceId -like 'SCSI\DISK&VEN_WISPDISK*'
                } |
                ForEach-Object {
                    [ordered]@{
                        Class        = $_.Class
                        FriendlyName = $_.FriendlyName
                        InstanceId   = $_.InstanceId
                        Status       = $_.Status
                    }
                })
        }

        try {
            $verifier = @(& verifier.exe /querysettings 2>&1 | ForEach-Object { $_.ToString() })
            if (($verifier -join "`n") -notmatch 'Verifier Flags:\s+0x00000000') {
                throw 'Driver Verifier is not off before the baseline test.'
            }

            $cliPath = Join-Path $RunDirectory 'wispdisk.exe'
            if (-not (Test-Path -LiteralPath $cliPath -PathType Leaf)) {
                throw "CLI is missing: $cliPath"
            }

            $result.DeviceBefore = Get-WispDiskDevices
            $usedLetters = @(Get-Volume -ErrorAction SilentlyContinue |
                Where-Object DriveLetter |
                ForEach-Object { $_.DriveLetter.ToString().ToUpperInvariant() })
            $driveLetter = @('R','S','T','U','V','W','X','Y','Z') |
                Where-Object { $usedLetters -notcontains $_ } |
                Select-Object -First 1
            if (-not $driveLetter) {
                throw 'No conservative test drive letter is free.'
            }
            $result.DriveLetter = $driveLetter

            $stage = 'add'
            $result.Stage = $stage
            $add = Invoke-WispDiskCli -Arguments @('/add','/hdd','/letter',$driveLetter,'/size','64MiB')
            $result.Add = $add
            if ($add.ExitCode -ne 0) {
                throw "wispdisk.exe /add failed with exit code $($add.ExitCode)."
            }

            $stage = 'mount-wait'
            $result.Stage = $stage
            $deadline = (Get-Date).AddSeconds(30)
            $volume = $null
            do {
                Start-Sleep -Milliseconds 500
                $volume = Get-Volume -DriveLetter $driveLetter -ErrorAction SilentlyContinue
            } while (-not $volume -and (Get-Date) -lt $deadline)
            if (-not $volume) {
                throw "Drive $driveLetter`: did not appear within 30 seconds."
            }

            $partition = Get-Partition -DriveLetter $driveLetter
            $disk = $partition | Get-Disk
            $diskIdentity = $disk.UniqueId
            $result.Volume = [ordered]@{
                DriveLetter     = $volume.DriveLetter.ToString()
                FileSystem      = $volume.FileSystem
                FileSystemLabel = $volume.FileSystemLabel
                HealthStatus    = $volume.HealthStatus.ToString()
                Size            = [uint64]$volume.Size
            }
            $result.Disk = [ordered]@{
                Number       = $disk.Number
                FriendlyName = $disk.FriendlyName
                SerialNumber = $disk.SerialNumber
                UniqueId     = $disk.UniqueId
                BusType      = $disk.BusType.ToString()
                HealthStatus = $disk.HealthStatus.ToString()
                Size         = [uint64]$disk.Size
            }
            $result.DeviceAfterAdd = Get-WispDiskDevices

            if ($volume.HealthStatus.ToString() -ne 'Healthy') {
                throw "Mounted volume health is '$($volume.HealthStatus)'."
            }
            if ($disk.FriendlyName -notlike '*WispDisk*') {
                throw "Mounted drive is not attributed to WispDisk: '$($disk.FriendlyName)'."
            }

            $stage = 'data-io'
            $result.Stage = $stage
            $testPath = "$driveLetter`:\baseline.bin"
            $bytes = New-Object byte[] (1024 * 1024)
            for ($index = 0; $index -lt $bytes.Length; $index++) {
                $bytes[$index] = [byte](($index * 31 + 7) % 251)
            }
            $stream = [IO.FileStream]::new(
                $testPath,
                [IO.FileMode]::CreateNew,
                [IO.FileAccess]::Write,
                [IO.FileShare]::None
            )
            try {
                $stream.Write($bytes, 0, $bytes.Length)
                $stream.Flush($true)
            } finally {
                $stream.Dispose()
            }
            $result.DataHashBeforeRead = (Get-FileHash -LiteralPath $testPath -Algorithm SHA256).Hash
            [GC]::Collect()
            $result.DataHashAfterRead = (Get-FileHash -LiteralPath $testPath -Algorithm SHA256).Hash
            if ($result.DataHashBeforeRead -ne $result.DataHashAfterRead) {
                throw 'Baseline write/read SHA-256 mismatch.'
            }

            $stage = 'delete'
            $result.Stage = $stage
            $delete = Invoke-WispDiskCli -Arguments @('/del','/letter',$driveLetter)
            $result.Delete = $delete
            if ($delete.ExitCode -ne 0) {
                throw "wispdisk.exe /del failed with exit code $($delete.ExitCode)."
            }

            $stage = 'removal-wait'
            $result.Stage = $stage
            $deadline = (Get-Date).AddSeconds(30)
            do {
                Start-Sleep -Milliseconds 500
                $remainingVolume = Get-Volume -DriveLetter $driveLetter -ErrorAction SilentlyContinue
                $remainingDisk = Get-Disk -ErrorAction SilentlyContinue |
                    Where-Object { $_.UniqueId -eq $diskIdentity }
            } while (($remainingVolume -or $remainingDisk) -and (Get-Date) -lt $deadline)
            if ($remainingVolume -or $remainingDisk) {
                throw 'The baseline WispDisk volume or disk remained after deletion.'
            }

            $result.DeviceAfterDelete = Get-WispDiskDevices
            $result.Success = $true
            $result.Stage = 'complete'
        } catch {
            $result.Error = $_.Exception.ToString()
            $result.Stage = $stage
        } finally {
            $result.CompletedAt = [DateTime]::UtcNow.ToString('o')
            $result.SystemEvents = @(Get-WinEvent -FilterHashtable @{LogName='System'; StartTime=$startedAt} -ErrorAction SilentlyContinue |
                Select-Object -First 200 |
                ForEach-Object {
                    [ordered]@{
                        TimeCreated  = $_.TimeCreated.ToUniversalTime().ToString('o')
                        ProviderName = $_.ProviderName
                        Id           = $_.Id
                        Level        = $_.LevelDisplayName
                        Message      = $_.Message
                    }
                })
            $setupApiPath = Join-Path $env:SystemRoot 'inf\setupapi.dev.log'
            if (Test-Path -LiteralPath $setupApiPath) {
                $result.SetupApiExcerpt = @(Get-Content -LiteralPath $setupApiPath -Tail 600 |
                    Select-String -Pattern 'WispDisk|ROOT\\WISPDISK' -Context 3,8 |
                    ForEach-Object { $_.ToString() })
            }
        }

        [pscustomobject]$result
    }

    $hostResult = [ordered]@{
        VMName = $VMName
        Guest  = $guestResult
    }
    $hostResult | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $OutputPath -Encoding utf8
    if (-not $guestResult.Success) {
        exit 1
    }
} catch {
    [ordered]@{
        VMName                = $VMName
        Error                 = $_.Exception.ToString()
        FullyQualifiedErrorId = $_.FullyQualifiedErrorId
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $OutputPath -Encoding utf8
    exit 1
} finally {
    if ($session) {
        Remove-PSSession -Session $session
    }
}
