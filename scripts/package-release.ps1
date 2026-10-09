#Requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^[0-9A-Za-z][0-9A-Za-z._+-]*$')]
    [string] $Version,

    [ValidateSet('Release', 'Debug')]
    [string] $Configuration = 'Release'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$artifactsRoot = Join-Path $repoRoot 'artifacts'
$releaseRoot = Join-Path $artifactsRoot 'release'
$certificatePath = Join-Path $artifactsRoot 'signing/WispDiskTest.cer'

function Assert-File {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [string] $Description
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Missing ${Description}: $Path"
    }
}

function Get-UpperFileHash {
    param(
        [Parameter(Mandatory)]
        [string] $Path,

        [Parameter(Mandatory)]
        [ValidateSet('SHA1', 'SHA256')]
        [string] $Algorithm
    )

    (Get-FileHash -LiteralPath $Path -Algorithm $Algorithm).Hash.ToUpperInvariant()
}

function Assert-ManifestFile {
    param(
        [Parameter(Mandatory)]
        [object] $Manifest,

        [Parameter(Mandatory)]
        [string] $Role,

        [Parameter(Mandatory)]
        [string] $Path
    )

    Assert-File -Path $Path -Description $Role
    $entries = @($Manifest.Files | Where-Object { $_.Role -eq $Role })
    if ($entries.Count -ne 1) {
        throw "Expected exactly one '$Role' entry in the build manifest; found $($entries.Count)"
    }

    $actualHash = Get-UpperFileHash -Path $Path -Algorithm SHA256
    if ($actualHash -ne $entries[0].SHA256.ToUpperInvariant()) {
        throw "The $Role file does not match its build manifest: $Path"
    }
}

Assert-File -Path $certificatePath -Description 'public signing certificate'
$publicCertificate = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($certificatePath)
try {
    if ($publicCertificate.HasPrivateKey) {
        throw "Refusing to package a certificate that contains a private key: $certificatePath"
    }
}
finally {
    $publicCertificate.Dispose()
}
New-Item -ItemType Directory -Force -Path $releaseRoot | Out-Null

$certificateThumbprint = Get-UpperFileHash -Path $certificatePath -Algorithm SHA1
$archives = [System.Collections.Generic.List[System.IO.FileInfo]]::new()
$architectures = @(
    @{ Name = 'x64'; Platform = 'x64' },
    @{ Name = 'arm64'; Platform = 'ARM64' }
)

foreach ($architecture in $architectures) {
    $platform = $architecture.Platform
    $manifestPath = Join-Path $artifactsRoot "signing/$Configuration-$platform-manifest.json"
    Assert-File -Path $manifestPath -Description "$platform build manifest"
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json

    if ($manifest.SchemaVersion -lt 3) {
        throw "The $platform manifest predates CLI symbol packaging; rebuild before packaging"
    }
    if ($manifest.Configuration -ne $Configuration -or $manifest.Platform -ne $platform) {
        throw "Build manifest identity does not match $Configuration/$platform"
    }
    if ($manifest.DriverTestCertificateThumbprint -ne $certificateThumbprint -or
        $manifest.ExecutableCertificateThumbprint -ne $certificateThumbprint) {
        throw "The $platform artifacts were not all signed by the packaged public certificate"
    }

    $binRoot = Join-Path $artifactsRoot "bin/$Configuration/$platform"
    $packageRoot = Join-Path $artifactsRoot "package/$Configuration/$platform"
    $driverRoot = Join-Path $artifactsRoot "driver/$Configuration/$platform"
    $filesByRole = [ordered]@{
        Driver               = Join-Path $packageRoot 'WispDisk.sys'
        DriverInf            = Join-Path $packageRoot 'WispDisk.inf'
        DriverCatalog        = Join-Path $packageRoot 'WispDisk.cat'
        DriverSymbols        = Join-Path $driverRoot 'WispDisk.pdb'
        SignedCli            = Join-Path $binRoot 'wispdisk.exe'
        CliSymbols           = Join-Path $binRoot 'wispdisk.pdb'
        PublicTestCertificate = $certificatePath
    }
    foreach ($entry in $filesByRole.GetEnumerator()) {
        Assert-ManifestFile -Manifest $manifest -Role $entry.Key -Path $entry.Value
    }

    $archiveBase = "wispdisk-$Version-windows-$($architecture.Name)"
    $stagingRoot = Join-Path $releaseRoot '.staging'
    $stagingDirectory = Join-Path $stagingRoot $archiveBase
    $driverDirectory = Join-Path $stagingDirectory 'driver'
    $archivePath = Join-Path $releaseRoot "$archiveBase.zip"

    if (Test-Path -LiteralPath $stagingDirectory) {
        Remove-Item -LiteralPath $stagingDirectory -Recurse -Force
    }
    if (Test-Path -LiteralPath $archivePath) {
        Remove-Item -LiteralPath $archivePath -Force
    }
    New-Item -ItemType Directory -Force -Path $driverDirectory | Out-Null

    Copy-Item -LiteralPath $filesByRole.SignedCli -Destination (Join-Path $stagingDirectory 'wispdisk.exe')
    Copy-Item -LiteralPath $filesByRole.CliSymbols -Destination (Join-Path $stagingDirectory 'wispdisk.pdb')
    Copy-Item -LiteralPath $filesByRole.PublicTestCertificate -Destination (Join-Path $stagingDirectory 'WispDiskTest.cer')
    Copy-Item -LiteralPath $manifestPath -Destination (Join-Path $stagingDirectory 'build-manifest.json')
    Copy-Item -LiteralPath $filesByRole.Driver -Destination (Join-Path $driverDirectory 'WispDisk.sys')
    Copy-Item -LiteralPath $filesByRole.DriverInf -Destination (Join-Path $driverDirectory 'WispDisk.inf')
    Copy-Item -LiteralPath $filesByRole.DriverCatalog -Destination (Join-Path $driverDirectory 'WispDisk.cat')
    Copy-Item -LiteralPath $filesByRole.DriverSymbols -Destination (Join-Path $driverDirectory 'WispDisk.pdb')

    Compress-Archive -Path (Join-Path $stagingDirectory '*') -DestinationPath $archivePath -CompressionLevel Optimal
    $archives.Add((Get-Item -LiteralPath $archivePath))
    Remove-Item -LiteralPath $stagingDirectory -Recurse -Force
}

$checksumLines = foreach ($archive in $archives | Sort-Object Name) {
    $hash = (Get-UpperFileHash -Path $archive.FullName -Algorithm SHA256).ToLowerInvariant()
    "$hash *$($archive.Name)"
}
$checksumPath = Join-Path $releaseRoot 'SHA256SUMS.txt'
[System.IO.File]::WriteAllText(
    $checksumPath,
    (($checksumLines -join "`n") + "`n"),
    [System.Text.Encoding]::ASCII
)

Write-Host 'Release assets:'
$archives | Sort-Object Name | ForEach-Object { Write-Host "  $($_.FullName)" }
Write-Host "  $checksumPath"
