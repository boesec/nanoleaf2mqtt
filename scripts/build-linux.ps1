# Cross-compiles the release binary for x86_64 Linux on Windows, with cargo-zigbuild and Zig as
# C compiler and linker (SQLite and ring contain C code).
#
# Needs once:
#   winget install zig.zig
#   cargo install cargo-zigbuild
#   rustup target add x86_64-unknown-linux-gnu
#
# The ".2.36" suffix links against glibc 2.36 symbols, so the binary runs on Debian 12 and later.
# Prints the path of the binary as the last line.
$ErrorActionPreference = 'Stop'
Set-Location (Join-Path $PSScriptRoot '..')

$Target = 'x86_64-unknown-linux-gnu'
$GlibcFloor = '2.36'

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

# Cargo writes progress to stderr; the exit code decides, not stderr output.
$ErrorActionPreference = 'Continue'
cargo zigbuild --release --locked --target "$Target.$GlibcFloor"
if ($LASTEXITCODE -ne 0) {
    Write-Host 'FAILED: cargo zigbuild'
    exit $LASTEXITCODE
}

$Binary = Join-Path (Get-Location) "target/$Target/release/nanoleaf2mqtt"
if (-not (Test-Path $Binary)) {
    Write-Host "FAILED: $Binary missing"
    exit 1
}
Write-Output $Binary
