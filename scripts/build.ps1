[CmdletBinding()]
param(
    [ValidateSet('Debug', 'Release')]
    [string]$Configuration = 'Debug',

    [ValidateSet('x64', 'ARM64')]
    [string]$Platform = 'x64',

    [switch]$SkipDriver
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$driverSolution = Join-Path $repoRoot 'driver\WispDisk.sln'
$artifactRoot = Join-Path $repoRoot 'artifacts'
$packageDirectory = Join-Path $artifactRoot "package\$Configuration\$Platform"
$rustTarget = if ($Platform -eq 'ARM64') { 'aarch64-pc-windows-msvc' } else { $null }

if ($rustTarget) {
    $installedRustTargets = @(& rustup target list --installed)
    if ($LASTEXITCODE -ne 0) { throw 'Unable to query installed Rust targets.' }
    if ($installedRustTargets -notcontains $rustTarget) {
        throw "Rust target $rustTarget is not installed. Run: rustup target add $rustTarget"
    }
}

if (-not $SkipDriver) {
    $vswhere = 'C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe'
    if (-not (Test-Path -LiteralPath $vswhere)) {
        throw 'Visual Studio Installer vswhere.exe was not found.'
    }

    $visualStudio = & $vswhere -latest -products * -requires Microsoft.Component.MSBuild -property installationPath
    if (-not $visualStudio) {
        throw 'A Visual Studio installation containing MSBuild was not found.'
    }

    $driverVisualStudio = & $vswhere -latest -products * -requires Microsoft.Component.MSBuild Component.Microsoft.Windows.DriverKit -property installationPath
    if (-not $driverVisualStudio) {
        throw 'Visual Studio is missing Component.Microsoft.Windows.DriverKit. Add the Windows Driver Kit component even if the WDK headers and libraries are already installed.'
    }

    if ($env:Driver_SpectreMitigation -ne 'false') {
        $spectreComponent = if ($Platform -eq 'ARM64') {
            'Microsoft.VisualStudio.Component.VC.Runtimes.ARM64.Spectre'
        } else {
            'Microsoft.VisualStudio.Component.VC.Runtimes.x86.x64.Spectre'
        }
        $spectreVisualStudio = & $vswhere -latest -products * -requires $spectreComponent -property installationPath
        if (-not $spectreVisualStudio) {
            $toolsetVersionFile = Join-Path $driverVisualStudio 'VC\Auxiliary\Build\Microsoft.VCToolsVersion.default.txt'
            if (-not (Test-Path -LiteralPath $toolsetVersionFile -PathType Leaf)) {
                throw 'Visual Studio does not report the Spectre component and its active C++ toolset could not be resolved.'
            }
            $toolsetVersion = (Get-Content -LiteralPath $toolsetVersionFile -Raw).Trim()
            $spectreLibraryDirectory = Join-Path $driverVisualStudio "VC\Tools\MSVC\$toolsetVersion\lib\spectre\$Platform"
            $spectreLibraries = @(Get-ChildItem -LiteralPath $spectreLibraryDirectory -Filter '*.lib' -File -ErrorAction SilentlyContinue)
            if ($spectreLibraries.Count -eq 0) {
                throw "Visual Studio is missing $spectreComponent, which the WDK requires for mitigated $Platform driver builds."
            }
        }
    }

    $visualStudio = $driverVisualStudio
    $msbuild = Join-Path $visualStudio 'MSBuild\Current\Bin\amd64\MSBuild.exe'
    if (-not (Test-Path -LiteralPath $msbuild)) {
        $msbuild = Join-Path $visualStudio 'MSBuild\Current\Bin\MSBuild.exe'
    }
    & $msbuild $driverSolution /m /t:Build "/p:Configuration=$Configuration" "/p:Platform=$Platform" /p:RunCodeAnalysis=true
    if ($LASTEXITCODE -ne 0) { throw 'driver build failed' }

    New-Item -ItemType Directory -Force -Path $packageDirectory | Out-Null
    $driverOutput = Join-Path $artifactRoot "driver\$Configuration\$Platform"
    $builtPackageFiles = @{
        'WispDisk.sys' = Join-Path $driverOutput 'WispDisk.sys'
        'WispDisk.inf' = Join-Path $driverOutput 'WispDisk.inf'
        'WispDisk.cat' = Join-Path $driverOutput 'WispDisk\wispdisk.cat'
    }
    foreach ($fileName in $builtPackageFiles.Keys) {
        $builtFile = $builtPackageFiles[$fileName]
        if (-not (Test-Path -LiteralPath $builtFile -PathType Leaf)) {
            throw "driver build did not produce $builtFile"
        }
        Copy-Item -LiteralPath $builtFile -Destination (Join-Path $packageDirectory $fileName) -Force
    }

    $env:WISPDISK_DRIVER_PACKAGE_DIR = $packageDirectory
}

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
    if ($LASTEXITCODE -ne 0) { throw 'Rust build failed' }
}
finally {
    Pop-Location
}
