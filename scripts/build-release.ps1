# Build the release exe with no build-machine paths inside it.
# Rust keeps source paths in a release binary (panic locations, the PDB path); they would
# show the builder's user name. This maps them to neutral prefixes.
# Needs VCPKG_ROOT set (see README). Output: target\release\arkimage.exe
$ErrorActionPreference = "Stop"
$repo = Split-Path -Parent $PSScriptRoot
$cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $env:USERPROFILE ".cargo" }
$rustupHome = if ($env:RUSTUP_HOME) { $env:RUSTUP_HOME } else { Join-Path $env:USERPROFILE ".rustup" }
$maps = @(
  "--remap-path-prefix=$repo=arkimage",
  "--remap-path-prefix=$(Join-Path $cargoHome 'registry\src')=crates",
  "--remap-path-prefix=$(Join-Path $cargoHome 'git\checkouts')=git",
  "--remap-path-prefix=$rustupHome=rustup",
  "-Clink-arg=/PDBALTPATH:arkimage.pdb"
)
if ($env:VCPKG_ROOT) { $maps += "--remap-path-prefix=$($env:VCPKG_ROOT)=vcpkg" }
$env:RUSTFLAGS = ($maps -join " ")
# cargo reports progress on stderr: judge it by its exit code, not by stderr
$ErrorActionPreference = "Continue"
Push-Location $repo
try { cargo build --release 2>&1 | ForEach-Object { "$_" } } finally { Pop-Location }
if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
