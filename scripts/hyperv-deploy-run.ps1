[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$VMName,

    [Parameter(Mandatory = $true)]
    [string]$GuestUserName,

    [Parameter(Mandatory = $true)]
    [string]$RunDirectory,

    [switch]$ReplaceExistingRunDirectory,

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
    $manifestPath = Join-Path $RunDirectory 'host-manifest.json'
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    $guestDirectory = "C:\WispDiskTest\$($manifest.RunId)"

    $session = New-PSSession -VMName $VMName -Credential $credential
    Invoke-Command -Session $session -ArgumentList $guestDirectory,$ReplaceExistingRunDirectory.IsPresent -ScriptBlock {
        param([string]$Destination, [bool]$ReplaceExisting)
        $deploymentRoot = [IO.Path]::GetFullPath('C:\WispDiskTest')
        $resolvedDestination = [IO.Path]::GetFullPath($Destination)
        $expectedPrefix = $deploymentRoot.TrimEnd('\') + '\'
        if (-not $resolvedDestination.StartsWith($expectedPrefix, [StringComparison]::OrdinalIgnoreCase)) {
            throw "Refusing to manage a deployment path outside '$deploymentRoot': $resolvedDestination"
        }
        if (Test-Path -LiteralPath $Destination) {
            if (-not $ReplaceExisting) {
                throw "Guest run directory already exists: $Destination"
            }
            Remove-Item -LiteralPath $resolvedDestination -Recurse -Force
        }
        New-Item -ItemType Directory -Path $resolvedDestination -Force | Out-Null
    }

    $copyNames = @($manifest.Files | ForEach-Object { $_.Name }) + @('host-manifest.json')
    foreach ($copyName in $copyNames) {
        $copyPath = Join-Path $RunDirectory $copyName
        if (-not (Test-Path -LiteralPath $copyPath -PathType Leaf)) {
            throw "Staged deployment file is missing: $copyPath"
        }
        [IO.File]::SetAttributes($copyPath, [IO.FileAttributes]::Archive)
        Copy-Item -LiteralPath $copyPath -Destination $guestDirectory -ToSession $session
    }

    $guestResult = Invoke-Command -Session $session -ArgumentList $guestDirectory -ScriptBlock {
        param([string]$DeploymentDirectory)

        $manifest = Get-Content -LiteralPath (Join-Path $DeploymentDirectory 'host-manifest.json') -Raw |
            ConvertFrom-Json
        $hashes = @()
        foreach ($expected in $manifest.Files) {
            $path = Join-Path $DeploymentDirectory $expected.Name
            if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
                throw "Deployed file is missing: $path"
            }
            $actual = Get-FileHash -LiteralPath $path -Algorithm SHA256
            if ($actual.Hash -ne $expected.SHA256) {
                throw "SHA-256 mismatch for '$($expected.Name)': expected $($expected.SHA256), got $($actual.Hash)."
            }
            $hashes += [ordered]@{
                Name   = $expected.Name
                SHA256 = $actual.Hash
                Match  = $true
            }
        }

        $certificatePath = Join-Path $DeploymentDirectory 'WispDiskTest.cer'
        $certificate = [Security.Cryptography.X509Certificates.X509Certificate2]::new($certificatePath)
        function Test-LocalMachineCertificate {
            param(
                [Parameter(Mandatory = $true)]
                [string]$StoreName,

                [Parameter(Mandatory = $true)]
                [string]$Thumbprint
            )

            $store = [Security.Cryptography.X509Certificates.X509Store]::new(
                $StoreName,
                [Security.Cryptography.X509Certificates.StoreLocation]::LocalMachine
            )
            try {
                $store.Open([Security.Cryptography.X509Certificates.OpenFlags]::ReadOnly)
                $matches = $store.Certificates.Find(
                    [Security.Cryptography.X509Certificates.X509FindType]::FindByThumbprint,
                    $Thumbprint,
                    $false
                )
                return $matches.Count -gt 0
            } finally {
                $store.Close()
            }
        }

        foreach ($storeName in @('Root', 'TrustedPublisher')) {
            $trusted = Test-LocalMachineCertificate -StoreName $storeName -Thumbprint $certificate.Thumbprint
            if (-not $trusted) {
                $certutilOutput = @(& certutil.exe -f -addstore $storeName $certificatePath 2>&1 |
                    ForEach-Object { $_.ToString() })
                if ($LASTEXITCODE -ne 0) {
                    throw "certutil failed to add the public certificate to LocalMachine\\$storeName`: $($certutilOutput -join ' ')"
                }
                $trusted = Test-LocalMachineCertificate -StoreName $storeName -Thumbprint $certificate.Thumbprint
                if (-not $trusted) {
                    throw "The public certificate was not found in LocalMachine\\$storeName after certutil reported success."
                }
            }
        }

        $sysSignature = Get-AuthenticodeSignature -LiteralPath (Join-Path $DeploymentDirectory 'WispDisk.sys')
        $catSignature = Get-AuthenticodeSignature -LiteralPath (Join-Path $DeploymentDirectory 'WispDisk.cat')
        if ($sysSignature.Status.ToString() -ne 'Valid') {
            throw "Guest rejected the WispDisk.sys signature: $($sysSignature.StatusMessage)"
        }
        if ($catSignature.Status.ToString() -ne 'Valid') {
            throw "Guest rejected the WispDisk.cat signature: $($catSignature.StatusMessage)"
        }

        $dumpDirectory = Join-Path $env:SystemRoot 'Minidump'
        New-Item -ItemType Directory -Path $dumpDirectory -Force | Out-Null
        $probePath = Join-Path $dumpDirectory '.wispdisk-write-probe'
        [IO.File]::WriteAllText($probePath, 'probe')
        Remove-Item -LiteralPath $probePath -Force

        [ordered]@{
            ComputerName        = $env:COMPUTERNAME
            DeploymentDirectory = $DeploymentDirectory
            Hashes              = $hashes
            CertificateThumbprint = $certificate.Thumbprint
            SystemSignatureStatus = $sysSignature.Status.ToString()
            CatalogSignatureStatus = $catSignature.Status.ToString()
            BcdCurrent          = @(& bcdedit.exe /enum '{current}' 2>&1 | ForEach-Object { $_.ToString() })
            BcdDebugSettings    = @(& bcdedit.exe /dbgsettings 2>&1 | ForEach-Object { $_.ToString() })
            VerifierSettings    = @(& verifier.exe /querysettings 2>&1 | ForEach-Object { $_.ToString() })
            MemoryDumpPathAccessible = Test-Path -LiteralPath $env:SystemRoot
            MinidumpPathAccessible   = Test-Path -LiteralPath $dumpDirectory
        }
    }

    [ordered]@{
        DeployedAtUtc = [DateTime]::UtcNow.ToString('o')
        VMName        = $VMName
        RunId         = $manifest.RunId
        Guest         = $guestResult
    } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $OutputPath -Encoding utf8
} catch {
    [ordered]@{
        DeployedAtUtc         = [DateTime]::UtcNow.ToString('o')
        VMName               = $VMName
        Error                = $_.Exception.ToString()
        FullyQualifiedErrorId = $_.FullyQualifiedErrorId
    } | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $OutputPath -Encoding utf8
    exit 1
} finally {
    if ($session) {
        Remove-PSSession -Session $session
    }
}
