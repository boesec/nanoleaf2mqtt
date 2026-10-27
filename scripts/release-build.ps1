# Builds the release archives on Windows, the same way as the release workflow: static musl
# binaries for x86_64, aarch64 and armv7 Linux with cargo-zigbuild, each packed by
# scripts/package-release.sh, plus SHA256SUMS. For checking a release before it is tagged, and
# as the way to build one when GitHub Actions is not available.
#
# Needs once:
#   winget install zig.zig
#   cargo install cargo-zigbuild
#   rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf
# and Git for Windows (its bash and GNU tar pack the archives).
#
# Writes target/dist/<version>/ and prints the paths of the archives and of SHA256SUMS.
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

$Targets = @(
    'x86_64-unknown-linux-musl',
    'aarch64-unknown-linux-musl',
    'armv7-unknown-linux-musleabihf'
)

# A shell started before Zig was installed does not see winget's PATH change yet.
if (-not (Get-Command zig -ErrorAction SilentlyContinue)) {
    $env:Path = [Environment]::GetEnvironmentVariable('Path', 'Machine') + ';' +
        [Environment]::GetEnvironmentVariable('Path', 'User')
}
if (-not (Get-Command zig -ErrorAction SilentlyContinue)) {
    throw 'zig not found: winget install zig.zig'
}
if (-not (Get-Command cargo-zigbuild -ErrorAction SilentlyContinue)) {
    throw 'cargo-zigbuild not found: cargo install cargo-zigbuild'
}

# Git for Windows' bash, not the WSL launcher that may come first on PATH.
$Git = Get-Command git -ErrorAction SilentlyContinue
if (-not $Git) {
    throw 'git not found: Git for Windows is needed for its bash'
}
$Bash = Join-Path (Split-Path (Split-Path $Git.Source)) 'bin/bash.exe'
if (-not (Test-Path $Bash)) {
    throw "Git for Windows' bash not found at $Bash"
}

$Installed = rustup target list --installed
$Missing = $Targets | Where-Object { $Installed -notcontains $_ }
if ($Missing) {
    throw "rust targets missing: rustup target add $($Missing -join ' ')"
}

$Version = (cargo metadata --no-deps --format-version 1 --locked | ConvertFrom-Json).packages |
    Where-Object { $_.name -eq 'nanoleaf2mqtt' } |
    Select-Object -ExpandProperty version
if (-not $Version) {
    throw 'version not found in Cargo.toml'
}

# Relative paths with forward slashes work for cargo and for bash alike.
$OutDir = "target/dist/$Version"
if (Test-Path $OutDir) {
    Remove-Item -Recurse -Force $OutDir
}

# Cargo writes progress to stderr; the exit code decides, not stderr output.
$ErrorActionPreference = 'Continue'
foreach ($Target in $Targets) {
    Write-Host "==> cargo zigbuild --target $Target"
    cargo zigbuild --release --locked --target $Target
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: cargo zigbuild --target $Target"
        exit $LASTEXITCODE
    }
    Write-Host "==> packing $Target"
    & $Bash scripts/package-release.sh $Version $Target "target/$Target/release/nanoleaf2mqtt" $OutDir |
        Out-Null
    if ($LASTEXITCODE -ne 0) {
        Write-Host "FAILED: packing $Target"
        exit $LASTEXITCODE
    }
}
$ErrorActionPreference = 'Stop'

# The format of sha256sum, so `sha256sum -c SHA256SUMS` checks it on Linux.
$Archives = Get-ChildItem -Path $OutDir -Filter '*.tar.gz' | Sort-Object Name
$Lines = foreach ($Archive in $Archives) {
    $Hash = (Get-FileHash -Algorithm SHA256 -Path $Archive.FullName).Hash.ToLowerInvariant()
    "$Hash  $($Archive.Name)"
}
$Sums = Join-Path (Resolve-Path $OutDir) 'SHA256SUMS'
[IO.File]::WriteAllText($Sums, (($Lines -join "`n") + "`n"))

$Archives | ForEach-Object { Write-Output $_.FullName }
Write-Output $Sums
