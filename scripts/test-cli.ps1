[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot

Push-Location -LiteralPath $repoRoot
try {
    cargo fmt --all -- --check
    if ($LASTEXITCODE -ne 0) { throw 'cargo fmt check failed' }

    cargo clippy --workspace --all-targets -- -D warnings
    if ($LASTEXITCODE -ne 0) { throw 'cargo clippy failed' }

    cargo test --workspace
    if ($LASTEXITCODE -ne 0) { throw 'cargo test failed' }

    cargo run -p wispdisk-cli -- /add /rem /letter:R /size:64MiB /dry-run
    if ($LASTEXITCODE -ne 0) { throw 'add dry-run failed' }

    cargo run -p wispdisk-cli -- /del /letter R /dry-run
    if ($LASTEXITCODE -ne 0) { throw 'delete dry-run failed' }
}
finally {
    Pop-Location
}

