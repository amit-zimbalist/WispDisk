[CmdletBinding()]
param(
    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Debug',

    [ValidateSet('x64', 'ARM64')]
    [string]$Platform = 'x64'
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$packageDirectory = Join-Path $repoRoot "artifacts\package\$Configuration\$Platform"
$signingDirectory = Join-Path $repoRoot 'artifacts\signing'
$manifestPath = Join-Path $signingDirectory "$Configuration-$Platform-manifest.json"
$certificatePath = Join-Path $signingDirectory 'WispDiskTest.cer'
$subject = 'CN=WispDisk Driver Test Certificate'
$rustTarget = if ($Platform -eq 'ARM64') { 'aarch64-pc-windows-msvc' } else { $null }

if ($rustTarget) {
    $installedRustTargets = @(& rustup target list --installed)
    if ($LASTEXITCODE -ne 0) { throw 'Unable to query installed Rust targets.' }
    if ($installedRustTargets -notcontains $rustTarget) {
        throw "Rust target $rustTarget is not installed. Run: rustup target add $rustTarget"
    }
}

New-Item -ItemType Directory -Path $signingDirectory -Force | Out-Null

$requiredPackageFiles = @('WispDisk.sys', 'WispDisk.inf', 'WispDisk.cat')
foreach ($fileName in $requiredPackageFiles) {
    $path = Join-Path $packageDirectory $fileName
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Required package file is missing: $path"
    }
}

$certificate = Get-ChildItem -Path Cert:\CurrentUser\My |
    Where-Object {
        $_.Subject -eq $subject -and
        $_.HasPrivateKey -and
        $_.NotAfter -gt (Get-Date).AddMonths(6)
    } |
    Sort-Object NotAfter -Descending |
    Select-Object -First 1

if (-not $certificate) {
    $certificateParameters = @{
        Type              = 'CodeSigningCert'
        Subject           = $subject
        FriendlyName      = 'WispDisk disposable-VM test signing'
        CertStoreLocation = 'Cert:\CurrentUser\My'
        KeyAlgorithm      = 'RSA'
        KeyLength         = 3072
        HashAlgorithm     = 'SHA256'
        KeyExportPolicy   = 'NonExportable'
        NotAfter          = (Get-Date).AddYears(3)
    }
    $certificate = New-SelfSignedCertificate @certificateParameters
}

Export-Certificate -Cert $certificate -FilePath $certificatePath -Force | Out-Null

foreach ($store in @('Cert:\CurrentUser\Root', 'Cert:\CurrentUser\TrustedPublisher')) {
    $trusted = Get-ChildItem -Path $store | Where-Object { $_.Thumbprint -eq $certificate.Thumbprint }
    if (-not $trusted) {
        Import-Certificate -FilePath $certificatePath -CertStoreLocation $store | Out-Null
    }
}

$signtool = Get-ChildItem -Path 'C:\Program Files (x86)\Windows Kits\10\bin\*\x64\signtool.exe' |
    Sort-Object FullName -Descending |
    Select-Object -First 1 -ExpandProperty FullName
$inf2cat = Get-ChildItem -Path 'C:\Program Files (x86)\Windows Kits\10\bin\*\x86\inf2cat.exe' |
    Sort-Object FullName -Descending |
    Select-Object -First 1 -ExpandProperty FullName

if (-not $signtool -or -not (Test-Path -LiteralPath $signtool -PathType Leaf)) {
    throw 'signtool.exe was not found in the installed Windows Kits.'
}
if (-not $inf2cat -or -not (Test-Path -LiteralPath $inf2cat -PathType Leaf)) {
    throw 'inf2cat.exe was not found in the installed Windows Kits.'
}

$sysPath = Join-Path $packageDirectory 'WispDisk.sys'
$infPath = Join-Path $packageDirectory 'WispDisk.inf'
$catPath = Join-Path $packageDirectory 'WispDisk.cat'
$inf2CatOs = if ($Platform -eq 'ARM64') { '10_RS3_ARM64' } else { '10_X64' }

& $signtool sign /v /fd SHA256 /s My /sha1 $certificate.Thumbprint $sysPath
if ($LASTEXITCODE -ne 0) { throw 'Embedded SYS signing failed.' }

& $inf2cat "/driver:$packageDirectory" "/os:$inf2CatOs" /uselocaltime
if ($LASTEXITCODE -ne 0) { throw 'Catalog regeneration failed after SYS signing.' }

& $signtool sign /v /fd SHA256 /s My /sha1 $certificate.Thumbprint $catPath
if ($LASTEXITCODE -ne 0) { throw 'Catalog signing failed.' }

& $signtool verify /v /pa $sysPath
if ($LASTEXITCODE -ne 0) { throw 'Embedded SYS signature verification failed.' }
& $signtool verify /v /pa $catPath
if ($LASTEXITCODE -ne 0) { throw 'Catalog signature verification failed.' }
& $signtool verify /v /pa /c $catPath $sysPath
if ($LASTEXITCODE -ne 0) { throw 'Catalog membership verification failed for WispDisk.sys.' }
& $signtool verify /v /pa /c $catPath $infPath
if ($LASTEXITCODE -ne 0) { throw 'Catalog membership verification failed for WispDisk.inf.' }

$previousPackageDirectory = $env:WISPDISK_DRIVER_PACKAGE_DIR
$env:WISPDISK_DRIVER_PACKAGE_DIR = $packageDirectory
try {
    Push-Location -LiteralPath $repoRoot
    try {
        $cargoArguments = @('build', '--workspace')
        if ($Configuration -eq 'Release') {
            $cargoArguments += '--release'
        }
        if ($rustTarget) {
            $cargoArguments += @('--target', $rustTarget)
        }
        & cargo @cargoArguments
        if ($LASTEXITCODE -ne 0) { throw 'Rust rebuild with signed embedded package failed.' }
    } finally {
        Pop-Location
    }
} finally {
    $env:WISPDISK_DRIVER_PACKAGE_DIR = $previousPackageDirectory
}

$profile = if ($Configuration -eq 'Release') { 'release' } else { 'debug' }
$cliDirectory = if ($rustTarget) {
    Join-Path $repoRoot "target\$rustTarget\$profile"
} else {
    Join-Path $repoRoot "target\$profile"
}
$cliPath = Join-Path $cliDirectory 'wispdisk.exe'
$pdbPath = Join-Path $repoRoot "artifacts\driver\$Configuration\$Platform\WispDisk.pdb"
$manifestFiles = @($sysPath, $infPath, $catPath, $pdbPath, $cliPath, $certificatePath)
foreach ($path in $manifestFiles) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Manifest input is missing: $path"
    }
}

$manifest = [ordered]@{
    CreatedAtUtc        = [DateTime]::UtcNow.ToString('o')
    Configuration       = $Configuration
    Platform            = $Platform
    CertificateSubject  = $certificate.Subject
    CertificateThumbprint = $certificate.Thumbprint
    CertificateNotAfter = $certificate.NotAfter.ToUniversalTime().ToString('o')
    PrivateKeyExportable = $false
    Files               = @($manifestFiles | ForEach-Object {
        $item = Get-Item -LiteralPath $_
        [ordered]@{
            Path       = $item.FullName
            Length     = $item.Length
            SHA256     = (Get-FileHash -LiteralPath $item.FullName -Algorithm SHA256).Hash
        }
    })
}

$manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $manifestPath -Encoding utf8
Write-Output $manifestPath
