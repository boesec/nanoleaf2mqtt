# Runs the checks for Linux from Windows: clippy for the Linux target (covers the code behind
# cfg(unix)), then cross-compiles the test binaries and runs them on a Linux host over SSH:
#   scripts/linux-test.ps1 -Target root@<host>
# Needs the same tools as build-linux.ps1. On the host only bash, tar and coreutils are needed,
# no Rust toolchain. Everything copied there lives in a temporary directory under /var/tmp
# (on disk, not in a memory-backed /tmp) and is removed afterwards. The log is also written to
# target/linux-test/linux-test.log.
param(
    # SSH destination, for example root@<host>.
    [Parameter(Mandatory = $true)]
    [string]$Target,
    # Skip clippy for the Linux target.
    [switch]$SkipClippy
)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
Set-Location $Root

$Triple = 'x86_64-unknown-linux-gnu.2.36'

if (-not (Get-Command zig -ErrorAction SilentlyContinue)) {
    $env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
        [Environment]::GetEnvironmentVariable('Path', 'User')
}
foreach ($tool in 'zig', 'cargo-zigbuild') {
    if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) {
        throw "$tool not found, see scripts/build-linux.ps1"
    }
}

# Test binaries without debug info: a few hundred MB instead of several GB to copy.
$env:CARGO_PROFILE_DEV_DEBUG = '0'
$env:CARGO_PROFILE_DEV_STRIP = 'debuginfo'

# Cargo writes progress to stderr; the exit code decides, not stderr output.
$ErrorActionPreference = 'Continue'

if (-not $SkipClippy) {
    Write-Host "==> cargo clippy --target $Triple"
    cargo-zigbuild clippy --all-targets --all-features --target $Triple -- -D warnings
    if ($LASTEXITCODE -ne 0) {
        Write-Host 'FAILED: clippy'
        exit $LASTEXITCODE
    }
}

Write-Host '==> building the test binaries'
$messages = cargo-zigbuild test --all-features --target $Triple --no-run --message-format=json-render-diagnostics
if ($LASTEXITCODE -ne 0) {
    Write-Host 'FAILED: building the test binaries'
    exit $LASTEXITCODE
}
$artifacts = $messages |
    Where-Object { $_.StartsWith('{') } |
    ForEach-Object { $_ | ConvertFrom-Json } |
    Where-Object { $_.reason -eq 'compiler-artifact' -and $_.executable -and $_.package_id -match 'nanoleaf2mqtt' }
$tests = @($artifacts | Where-Object { $_.profile.test })
$service = @($artifacts | Where-Object { -not $_.profile.test -and $_.target.kind -contains 'bin' })
if ($tests.Count -eq 0 -or $service.Count -ne 1) {
    Write-Host 'FAILED: test binaries or service binary not found in the cargo output'
    exit 1
}

# Tests read CONTRACT.md and src/ below CARGO_MANIFEST_DIR and start the service binary from
# CARGO_BIN_EXE_nanoleaf2mqtt. Both are Windows paths fixed at compile time; the bundle carries
# them, and linux-test.sh recreates them on the Linux side.
$ManifestDir = Split-Path -Parent $tests[0].manifest_path
$Work = Join-Path $Root 'target/linux-test'
$Bundle = Join-Path $Work 'bundle'
if (Test-Path $Work) { Remove-Item -Recurse -Force $Work }
New-Item -ItemType Directory -Force (Join-Path $Bundle 'tests'), (Join-Path $Bundle 'bin'), (Join-Path $Bundle 'source') | Out-Null
foreach ($test in $tests) {
    Copy-Item $test.executable (Join-Path $Bundle 'tests')
}
Copy-Item $service[0].executable (Join-Path $Bundle 'bin/nanoleaf2mqtt')
Copy-Item (Join-Path $ManifestDir 'CONTRACT.md') (Join-Path $Bundle 'source')
Copy-Item -Recurse (Join-Path $ManifestDir 'src') (Join-Path $Bundle 'source')
Copy-Item (Join-Path $PSScriptRoot 'linux-test.sh') $Bundle
$utf8 = New-Object System.Text.UTF8Encoding($false)
[IO.File]::WriteAllText((Join-Path $Bundle 'manifest-dir.txt'), $ManifestDir, $utf8)
[IO.File]::WriteAllText((Join-Path $Bundle 'bin-exe.txt'), $service[0].executable, $utf8)

$Archive = Join-Path $Work 'bundle.tar.gz'
tar -czf $Archive -C $Work bundle
if ($LASTEXITCODE -ne 0) {
    Write-Host 'FAILED: packing the bundle'
    exit 1
}

$Ssh = @('-o', 'BatchMode=yes')
Write-Host "==> copying $($tests.Count) test binaries to $Target"
$Remote = (& ssh @Ssh $Target 'mktemp -d /var/tmp/nanoleaf2mqtt-test.XXXXXX' | Select-Object -Last 1)
if ($LASTEXITCODE -ne 0 -or -not $Remote) {
    Write-Host "FAILED: cannot reach $Target"
    exit 1
}
$Remote = $Remote.Trim()
& scp @Ssh -q $Archive "${Target}:$Remote/"
if ($LASTEXITCODE -ne 0) {
    & ssh @Ssh $Target "rm -rf '$Remote'"
    Write-Host 'FAILED: copy'
    exit 1
}

Write-Host "==> running the tests on $Target"
$Log = Join-Path $Work 'linux-test.log'
& ssh @Ssh $Target "cd '$Remote' && tar -xzf bundle.tar.gz && rm bundle.tar.gz && bash bundle/linux-test.sh '$Remote/bundle'; rc=`$?; rm -rf '$Remote'; exit `$rc" |
    Tee-Object -FilePath $Log
$rc = $LASTEXITCODE
if ($rc -ne 0) {
    Write-Host "FAILED: tests on Linux (log: $Log)"
    exit $rc
}
Write-Host "==> all tests passed on Linux (log: $Log)"
