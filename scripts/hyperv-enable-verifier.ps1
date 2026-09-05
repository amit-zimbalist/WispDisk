[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$VMName,

    [Parameter(Mandatory = $true)]
    [string]$GuestUserName,

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
    $guestResult = Invoke-Command -Session $session -ScriptBlock {
        function Invoke-VerifierCommand {
            param([string[]]$Arguments)
            $lines = @(& verifier.exe @Arguments 2>&1 | ForEach-Object { $_.ToString() })
            [ordered]@{
                Arguments = @($Arguments)
                ExitCode  = $LASTEXITCODE
                Output    = $lines
            }
        }

        $driver = Get-CimInstance -ClassName Win32_SystemDriver -Filter "Name='WispDisk'"
        if (-not $driver) {
            throw 'The installed WispDisk driver service was not found.'
        }

        $reset = Invoke-VerifierCommand -Arguments @('/reset')
        if ([int]$reset.ExitCode -notin @(0, 2)) {
            throw "verifier /reset failed with exit code $($reset.ExitCode)."
        }
        $standard = Invoke-VerifierCommand -Arguments @('/standard','/driver','WispDisk.sys')
        if ([int]$standard.ExitCode -notin @(0, 2)) {
            throw "verifier /standard failed with exit code $($standard.ExitCode)."
        }
        $bootMode = Invoke-VerifierCommand -Arguments @('/bootmode','oneboot')
        if ([int]$bootMode.ExitCode -notin @(0, 2)) {
            throw "verifier /bootmode oneboot failed with exit code $($bootMode.ExitCode)."
        }
        $querySettings = Invoke-VerifierCommand -Arguments @('/querysettings')
        if ($querySettings.ExitCode -ne 0) {
            throw "verifier /querysettings failed with exit code $($querySettings.ExitCode)."
        }
        $queryText = $querySettings.Output -join "`n"
        if ($queryText -notmatch '(?i)WispDisk\.sys') {
            throw 'WispDisk.sys is absent from verifier /querysettings.'
        }
        if ($queryText -notmatch '(?i)Verifier Flags:\s+0x001209bb') {
            throw 'Verifier did not report the expected Standard flag set 0x001209bb.'
        }

        [ordered]@{
            ComputerName = $env:COMPUTERNAME
            ConfiguredAtUtc = [DateTime]::UtcNow.ToString('o')
            Driver = [ordered]@{
                Name      = $driver.Name
                State     = $driver.State
                StartMode = $driver.StartMode
                PathName  = $driver.PathName
            }
            Reset         = $reset
            Standard      = $standard
            BootMode      = $bootMode
            QuerySettings = $querySettings
        }
    }

    [ordered]@{
        VMName = $VMName
        Guest  = $guestResult
    } | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $OutputPath -Encoding utf8

    Invoke-Command -Session $session -ScriptBlock {
        shutdown.exe /r /t 0 | Out-Null
    } -ErrorAction SilentlyContinue
} catch {
    [ordered]@{
        AttemptedAtUtc         = [DateTime]::UtcNow.ToString('o')
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
