# Builds the Linux binary and installs or updates it on a systemd host over SSH, in one command:
#   scripts/deploy.ps1 -Target root@<host>
# Copies binary, unit and packaging/install.sh to a temporary directory on the host, runs the
# install script there (creates the service user on the first run, replaces binary and unit,
# restarts, waits for the start line in the journal) and removes the temporary directory.
# The SSH user must be root or able to run the install script as root.
param(
    # SSH destination, for example root@<host>.
    [Parameter(Mandatory = $true)]
    [string]$Target,
    # Deploy the binary of the last build-linux.ps1 run instead of building.
    [switch]$SkipBuild
)
$ErrorActionPreference = 'Stop'
$Root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

if ($SkipBuild) {
    $Binary = Join-Path $Root 'target/x86_64-unknown-linux-gnu/release/nanoleaf2mqtt'
} else {
    $Binary = & (Join-Path $PSScriptRoot 'build-linux.ps1') | Select-Object -Last 1
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}
if (-not (Test-Path $Binary)) {
    throw "binary missing: $Binary"
}
$Unit = Join-Path $Root 'packaging/systemd/nanoleaf2mqtt.service'
$Install = Join-Path $Root 'packaging/install.sh'

# Native tools write to stderr; their exit codes decide.
$ErrorActionPreference = 'Continue'
$Ssh = @('-o', 'BatchMode=yes')

Write-Host "==> preparing $Target"
$Remote = (& ssh @Ssh $Target 'mktemp -d /var/tmp/nanoleaf2mqtt-deploy.XXXXXX' | Select-Object -Last 1)
if ($LASTEXITCODE -ne 0 -or -not $Remote) {
    Write-Host "FAILED: cannot reach $Target"
    exit 1
}
$Remote = $Remote.Trim()

Write-Host "==> copying to ${Target}:$Remote"
& scp @Ssh -q $Binary $Unit $Install "${Target}:$Remote/"
if ($LASTEXITCODE -ne 0) {
    & ssh @Ssh $Target "rm -rf '$Remote'"
    Write-Host 'FAILED: copy'
    exit 1
}

& ssh @Ssh $Target "bash '$Remote/install.sh' '$Remote/nanoleaf2mqtt' '$Remote/nanoleaf2mqtt.service'; rc=`$?; rm -rf '$Remote'; exit `$rc"
if ($LASTEXITCODE -ne 0) {
    Write-Host 'FAILED: install'
    exit $LASTEXITCODE
}
Write-Host "==> deployed to $Target"
