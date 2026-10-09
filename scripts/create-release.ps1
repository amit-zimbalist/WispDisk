#Requires -Version 5.1

[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [ValidatePattern('^[0-9A-Za-z][0-9A-Za-z._+-]*$')]
    [string] $Version
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot

Push-Location $repoRoot
try {
    Write-Host "Building and signing WispDisk $Version"
    & cargo make build
    if ($LASTEXITCODE -ne 0) {
        throw "The release build failed with exit code $LASTEXITCODE"
    }

    & (Join-Path $PSScriptRoot 'package-release.ps1') -Version $Version
}
finally {
    Pop-Location
}
