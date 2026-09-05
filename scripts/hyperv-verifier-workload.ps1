[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$VMName,

    [Parameter(Mandatory = $true)]
    [string]$GuestUserName,

    [Parameter(Mandatory = $true)]
    [string]$GuestRunDirectory,

    [switch]$ReplaceExistingWorkloadData,

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
    $guestResult = Invoke-Command -Session $session -ArgumentList $GuestRunDirectory,$ReplaceExistingWorkloadData.IsPresent -ScriptBlock {
        param([string]$RunDirectory, [bool]$ReplaceWorkloadData)

        $ErrorActionPreference = 'Stop'
        $startedAt = Get-Date
        $stage = 'preflight'
        $seed = 1592606758
        $result = [ordered]@{
            Success         = $false
            Stage           = $stage
            StartedAtUtc    = $startedAt.ToUniversalTime().ToString('o')
            CompletedAtUtc  = $null
            Seed            = $seed
            SourceFileCount = 0
            SourceBytes     = 0
            SourceManifestSHA256 = $null
            Drives          = @()
            VerifierBefore  = @()
            VerifierAfter   = @()
            SystemEvents    = @()
            SetupApiExcerpt = @()
            Error           = $null
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

        function Get-FileManifest {
            param([string]$Root)
            $resolvedRoot = [IO.Path]::GetFullPath($Root).TrimEnd('\')
            @(Get-ChildItem -LiteralPath $resolvedRoot -File -Recurse |
                ForEach-Object {
                    [pscustomobject][ordered]@{
                        RelativePath = $_.FullName.Substring($resolvedRoot.Length).TrimStart('\')
                        Length       = $_.Length
                        SHA256       = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
                    }
                } |
                Sort-Object RelativePath)
        }

        function Assert-ManifestsEqual {
            param(
                [object[]]$Expected,
                [object[]]$Actual,
                [string]$Description
            )
            if ($Expected.Count -ne $Actual.Count) {
                throw "$Description file count mismatch: expected $($Expected.Count), got $($Actual.Count)."
            }
            for ($index = 0; $index -lt $Expected.Count; $index++) {
                if ($Expected[$index].RelativePath -cne $Actual[$index].RelativePath -or
                    [uint64]$Expected[$index].Length -ne [uint64]$Actual[$index].Length -or
                    $Expected[$index].SHA256 -cne $Actual[$index].SHA256) {
                    throw "$Description mismatch at '$($Expected[$index].RelativePath)'."
                }
            }
        }

        function Get-TextSHA256 {
            param([string]$Text)
            $sha = [Security.Cryptography.SHA256]::Create()
            try {
                $bytes = [Text.Encoding]::UTF8.GetBytes($Text)
                return (($sha.ComputeHash($bytes) | ForEach-Object { $_.ToString('X2') }) -join '')
            } finally {
                $sha.Dispose()
            }
        }

        function Flush-TestVolume {
            param([string]$DriveLetter)
            if (-not ('WispDiskTest.NativeVolume' -as [type])) {
                Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;

namespace WispDiskTest {
    public static class NativeVolume {
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern SafeFileHandle CreateFile(
            string name, uint access, uint share, IntPtr security,
            uint creation, uint flags, IntPtr template);

        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool FlushFileBuffers(SafeFileHandle handle);

        public static void Flush(string driveLetter) {
            const uint GENERIC_READ = 0x80000000;
            const uint GENERIC_WRITE = 0x40000000;
            const uint FILE_SHARE_READ = 0x00000001;
            const uint FILE_SHARE_WRITE = 0x00000002;
            const uint OPEN_EXISTING = 3;
            string path = @"\\.\" + driveLetter.TrimEnd(':') + ":";
            using (SafeFileHandle handle = CreateFile(
                path, GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE, IntPtr.Zero,
                OPEN_EXISTING, 0, IntPtr.Zero)) {
                if (handle.IsInvalid) throw new Win32Exception(Marshal.GetLastWin32Error());
                if (!FlushFileBuffers(handle)) throw new Win32Exception(Marshal.GetLastWin32Error());
            }
        }
    }
}
'@
            }
            [WispDiskTest.NativeVolume]::Flush($DriveLetter)
        }

        function Get-WispDiskDeviceSnapshot {
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
            $result.VerifierBefore = @(& verifier.exe /query 2>&1 | ForEach-Object { $_.ToString() })
            $verifierText = $result.VerifierBefore -join "`n"
            if ($verifierText -notmatch '(?i)Verifier Flags:\s+0x001209bb' -or
                $verifierText -notmatch '(?i)MODULE:\s+WispDisk\.sys') {
                throw 'WispDisk.sys is not actively verified with Standard flags.'
            }
            $existingWispDisks = @(Get-Disk -ErrorAction SilentlyContinue |
                Where-Object { $_.FriendlyName -like '*WispDisk*' })
            if ($existingWispDisks.Count -ne 0) {
                throw "Expected a clean adapter with zero WispDisk disks; found $($existingWispDisks.Count)."
            }

            $sourceDirectory = Join-Path $RunDirectory 'workload-source'
            if (Test-Path -LiteralPath $sourceDirectory) {
                if (-not $ReplaceWorkloadData) {
                    throw "Source directory already exists: $sourceDirectory"
                }
                $resolvedRunDirectory = [IO.Path]::GetFullPath($RunDirectory).TrimEnd('\')
                $resolvedSourceDirectory = [IO.Path]::GetFullPath($sourceDirectory)
                $expectedPrefix = $resolvedRunDirectory + '\'
                if (-not $resolvedSourceDirectory.StartsWith($expectedPrefix, [StringComparison]::OrdinalIgnoreCase)) {
                    throw "Refusing to clean a source directory outside the current run: $resolvedSourceDirectory"
                }
                Remove-Item -LiteralPath $resolvedSourceDirectory -Recurse -Force
                $oldManifest = Join-Path $RunDirectory 'workload-source-manifest.json'
                if (Test-Path -LiteralPath $oldManifest) {
                    Remove-Item -LiteralPath $oldManifest -Force
                }
            }
            New-Item -ItemType Directory -Path $sourceDirectory | Out-Null

            $stage = 'generate-source'
            $result.Stage = $stage
            $random = [Random]::new($seed)
            for ($directoryIndex = 0; $directoryIndex -lt 10; $directoryIndex++) {
                $nested = Join-Path $sourceDirectory ("nested\d{0:D2}\leaf" -f $directoryIndex)
                New-Item -ItemType Directory -Path $nested -Force | Out-Null
                for ($fileIndex = 0; $fileIndex -lt 20; $fileIndex++) {
                    $length = ($directoryIndex * 379 + $fileIndex * 211) % 4097
                    $bytes = New-Object byte[] $length
                    $random.NextBytes($bytes)
                    [IO.File]::WriteAllBytes((Join-Path $nested ("small-{0:D2}.bin" -f $fileIndex)), $bytes)
                }
                [IO.File]::WriteAllBytes((Join-Path $nested 'empty.bin'), [byte[]]@())
            }
            for ($fileIndex = 0; $fileIndex -lt 10; $fileIndex++) {
                $bytes = New-Object byte[] (256 * 1024)
                $random.NextBytes($bytes)
                [IO.File]::WriteAllBytes((Join-Path $sourceDirectory ("medium-{0:D2}.bin" -f $fileIndex)), $bytes)
            }
            for ($fileIndex = 0; $fileIndex -lt 4; $fileIndex++) {
                $bytes = New-Object byte[] (8 * 1024 * 1024)
                $random.NextBytes($bytes)
                [IO.File]::WriteAllBytes((Join-Path $sourceDirectory ("large-{0:D2}.bin" -f $fileIndex)), $bytes)
            }

            $sourceManifest = Get-FileManifest -Root $sourceDirectory
            $manifestJson = $sourceManifest | ConvertTo-Json -Depth 4
            $manifestPath = Join-Path $RunDirectory 'workload-source-manifest.json'
            $manifestJson | Set-Content -LiteralPath $manifestPath -Encoding utf8
            $result.SourceFileCount = $sourceManifest.Count
            $result.SourceBytes = [uint64](($sourceManifest | Measure-Object -Property Length -Sum).Sum)
            $result.SourceManifestSHA256 = Get-TextSHA256 -Text $manifestJson

            $usedLetters = @(Get-Volume -ErrorAction SilentlyContinue |
                Where-Object DriveLetter |
                ForEach-Object { $_.DriveLetter.ToString().ToUpperInvariant() })
            $letters = @('R','S','T','U','V','W','X','Y','Z') |
                Where-Object { $usedLetters -notcontains $_ } |
                Select-Object -First 2
            if ($letters.Count -ne 2) {
                throw 'Two distinct conservative test drive letters are not free.'
            }

            $specifications = @(
                [ordered]@{ Letter=$letters[0]; Media='hdd'; Size='128MiB' },
                [ordered]@{ Letter=$letters[1]; Media='rem'; Size='128MiB' }
            )

            foreach ($specification in $specifications) {
                $stage = "add-$($specification.Letter)"
                $result.Stage = $stage
                $add = Invoke-WispDiskCli -Arguments @('/add',"/$($specification.Media)",'/letter',$specification.Letter,'/size',$specification.Size)
                $driveResult = [ordered]@{
                    Letter          = $specification.Letter
                    Media           = $specification.Media
                    RequestedSize   = $specification.Size
                    DeviceId        = $null
                    DiskUniqueId     = $null
                    DiskFriendlyName = $null
                    DiskSize         = $null
                    Add             = $add
                    ManifestAfterCopySHA256  = $null
                    ManifestAfterFlushSHA256 = $null
                    RoundTripManifestSHA256  = $null
                    MutationsCompleted       = $false
                    Delete          = $null
                    Removed         = $false
                    DevicesAfterAdd = @()
                }
                $result.Drives += [pscustomobject]$driveResult
                if ($add.ExitCode -ne 0) {
                    throw "Add failed for $($specification.Letter): with exit code $($add.ExitCode)."
                }
                $deviceMatch = [regex]::Match(($add.Output -join ' '), 'device id\s+(\d+)', 'IgnoreCase')
                if (-not $deviceMatch.Success) {
                    throw "CLI did not return a stable device ID for $($specification.Letter):."
                }

                $deadline = (Get-Date).AddSeconds(30)
                $volume = $null
                do {
                    Start-Sleep -Milliseconds 500
                    $volume = Get-Volume -DriveLetter $specification.Letter -ErrorAction SilentlyContinue
                } while (-not $volume -and (Get-Date) -lt $deadline)
                if (-not $volume) {
                    throw "$($specification.Letter): did not mount within 30 seconds."
                }
                $disk = Get-Partition -DriveLetter $specification.Letter | Get-Disk
                if ($disk.FriendlyName -notlike '*WispDisk*') {
                    throw "$($specification.Letter): is not backed by a WispDisk device."
                }

                $currentDrive = $result.Drives[-1]
                $currentDrive.DeviceId = [uint32]$deviceMatch.Groups[1].Value
                $currentDrive.DiskUniqueId = $disk.UniqueId
                $currentDrive.DiskFriendlyName = $disk.FriendlyName
                $currentDrive.DiskSize = [uint64]$disk.Size
                $currentDrive.DevicesAfterAdd = Get-WispDiskDeviceSnapshot
            }

            foreach ($drive in $result.Drives) {
                $letter = $drive.Letter
                $root = "$letter`:"
                $retained = Join-Path $root 'retained'
                $mutations = Join-Path $root 'mutations'
                $roundTrip = Join-Path $RunDirectory "roundtrip-$letter"

                $stage = "copy-$letter"
                $result.Stage = $stage
                New-Item -ItemType Directory -Path $retained,$mutations | Out-Null
                Copy-Item -Path (Join-Path $sourceDirectory '*') -Destination $retained -Recurse -Force
                $afterCopy = Get-FileManifest -Root $retained
                Assert-ManifestsEqual -Expected $sourceManifest -Actual $afterCopy -Description "$letter`: copy"
                $drive.ManifestAfterCopySHA256 = Get-TextSHA256 -Text ($afterCopy | ConvertTo-Json -Depth 4)

                $stage = "mutations-$letter"
                $result.Stage = $stage
                $mutationA = Join-Path $mutations 'create.bin'
                $mutationB = Join-Path $mutations 'renamed.bin'
                [IO.File]::WriteAllBytes($mutationA, ([Text.Encoding]::UTF8.GetBytes('initial mutation payload')))
                Rename-Item -LiteralPath $mutationA -NewName 'renamed.bin'
                [IO.File]::WriteAllBytes($mutationB, ([Text.Encoding]::UTF8.GetBytes('overwritten mutation payload')))
                $truncate = [IO.File]::Open($mutationB, [IO.FileMode]::Open, [IO.FileAccess]::Write, [IO.FileShare]::None)
                try {
                    $truncate.SetLength(7)
                    $truncate.Flush($true)
                } finally {
                    $truncate.Dispose()
                }
                $deletePath = Join-Path $mutations 'delete-me.bin'
                [IO.File]::WriteAllBytes($deletePath, ([byte[]](1,2,3,4,5)))
                Remove-Item -LiteralPath $deletePath -Force
                if (Test-Path -LiteralPath $deletePath) {
                    throw "$letter`: mutation delete did not remove its target."
                }
                $drive.MutationsCompleted = $true

                $stage = "flush-reread-$letter"
                $result.Stage = $stage
                Flush-TestVolume -DriveLetter $letter
                $afterFlush = Get-FileManifest -Root $retained
                Assert-ManifestsEqual -Expected $sourceManifest -Actual $afterFlush -Description "$letter`: flush/reread"
                $drive.ManifestAfterFlushSHA256 = Get-TextSHA256 -Text ($afterFlush | ConvertTo-Json -Depth 4)

                $stage = "roundtrip-$letter"
                $result.Stage = $stage
                New-Item -ItemType Directory -Path $roundTrip | Out-Null
                Copy-Item -Path (Join-Path $retained '*') -Destination $roundTrip -Recurse -Force
                $roundTripManifest = Get-FileManifest -Root $roundTrip
                Assert-ManifestsEqual -Expected $sourceManifest -Actual $roundTripManifest -Description "$letter`: round trip"
                $drive.RoundTripManifestSHA256 = Get-TextSHA256 -Text ($roundTripManifest | ConvertTo-Json -Depth 4)

                $stage = "clear-data-$letter"
                $result.Stage = $stage
                Remove-Item -LiteralPath $retained,$mutations -Recurse -Force
                if ((Test-Path -LiteralPath $retained) -or (Test-Path -LiteralPath $mutations)) {
                    throw "$letter`: test data remained after cleanup."
                }
            }

            foreach ($drive in $result.Drives) {
                $stage = "delete-$($drive.Letter)"
                $result.Stage = $stage
                $delete = Invoke-WispDiskCli -Arguments @('/del','/letter',$drive.Letter)
                $drive.Delete = $delete
                if ($delete.ExitCode -ne 0) {
                    throw "Delete failed for $($drive.Letter): with exit code $($delete.ExitCode)."
                }
                if (($delete.Output -join ' ') -notmatch "device id\s+$($drive.DeviceId)(\D|$)") {
                    throw "Delete for $($drive.Letter): did not report expected device ID $($drive.DeviceId)."
                }

                $deadline = (Get-Date).AddSeconds(30)
                do {
                    Start-Sleep -Milliseconds 500
                    $volume = Get-Volume -DriveLetter $drive.Letter -ErrorAction SilentlyContinue
                    $disk = Get-Disk -ErrorAction SilentlyContinue |
                        Where-Object { $_.UniqueId -eq $drive.DiskUniqueId }
                } while (($volume -or $disk) -and (Get-Date) -lt $deadline)
                if ($volume -or $disk) {
                    throw "$($drive.Letter): volume or disk remained after deletion."
                }
                $drive.Removed = $true
            }

            $result.VerifierAfter = @(& verifier.exe /query 2>&1 | ForEach-Object { $_.ToString() })
            $afterVerifierText = $result.VerifierAfter -join "`n"
            if ($afterVerifierText -notmatch '(?i)MODULE:\s+WispDisk\.sys') {
                throw 'WispDisk.sys disappeared from active Verifier state during the workload.'
            }

            $result.Success = $true
            $result.Stage = 'complete'
        } catch {
            $result.Error = $_.Exception.ToString()
            $result.Stage = $stage
        } finally {
            $result.CompletedAtUtc = [DateTime]::UtcNow.ToString('o')
            $result.SystemEvents = @(Get-WinEvent -FilterHashtable @{LogName='System'; StartTime=$startedAt} -ErrorAction SilentlyContinue |
                Select-Object -First 300 |
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
                $result.SetupApiExcerpt = @(Get-Content -LiteralPath $setupApiPath -Tail 1000 |
                    Select-String -Pattern 'WispDisk|ROOT\\WISPDISK' -Context 3,8 |
                    ForEach-Object { $_.ToString() })
            }
        }

        [pscustomobject]$result
    }

    [ordered]@{
        VMName = $VMName
        Guest  = $guestResult
    } | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $OutputPath -Encoding utf8

    $guestManifestPath = Join-Path $GuestRunDirectory 'workload-source-manifest.json'
    if (Invoke-Command -Session $session -ArgumentList $guestManifestPath -ScriptBlock { param($Path) Test-Path -LiteralPath $Path }) {
        Copy-Item -FromSession $session -LiteralPath $guestManifestPath -Destination $outputDirectory -Force
    }

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
        Remove-PSSession -Session $session -ErrorAction SilentlyContinue
    }
}
