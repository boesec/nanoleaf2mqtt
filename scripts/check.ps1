# Runs the same checks as the gate: format, lints, tests. Stops at the first failure.
# Cargo writes progress to stderr; the exit code decides, not stderr output.
$ErrorActionPreference = 'Continue'
Set-Location (Join-Path $PSScriptRoot '..')

function Invoke-Step {
    param([string]$Name, [scriptblock]$Command)
    Write-Host "==> $Name"
    & $Command
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: $Name"
        exit $LASTEXITCODE
    }
}

Invoke-Step 'cargo fmt --all --check' { cargo fmt --all --check }
Invoke-Step 'cargo clippy' { cargo clippy --all-targets --all-features -- -D warnings }
Invoke-Step 'cargo test' { cargo test --all-features }
