<#
.SYNOPSIS
    Packages one release archive for crate-plugin-kit (Windows).

.DESCRIPTION
        scripts/package-release.ps1 -Target <triple> [-OutDir dist]

    writes

        dist/crate-plugin-kit-<target>.zip

    The archive holds exactly two entries, both at the top level: the binary
    (`plugin-asset.exe`) and `LICENSE`. The layout is an interface, not a preference --
    `cargo binstall crate-plugin-kit` unpacks the archive and looks for the binary at its
    root, which is why `[package.metadata.binstall] bin-dir` in Cargo.toml has to spell
    that out; and MIT requires the licence notice to travel with a copy of the binary
    anyway.

    The name is an interface too, and a subtle one: `crate-plugin-kit-<target>` carries
    the **crate** name rather than the binary's (`plugin-asset`) because that is what
    binstall's `{ name }` means. The binary name only has to match `{ bin }` inside the
    archive.

    This script is what CI runs, so a release can be reproduced locally with one command.

.PARAMETER Target
    The target triple to build for, such as x86_64-pc-windows-msvc.

.PARAMETER OutDir
    Where the archive goes. Defaults to `dist`.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Target,

    [string]$OutDir = 'dist'
)

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot

# `--locked`: a release is built from the committed lockfile, so the same tag yields the
# same dependency set no matter who builds it.
cargo build --release --locked --manifest-path "$root/Cargo.toml" --target $Target
if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed (exit code $LASTEXITCODE)"
}

$binary = Join-Path $root "target/$Target/release/plugin-asset.exe"
if (-not (Test-Path $binary)) {
    throw "no executable at $binary"
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$archive = Join-Path $OutDir "crate-plugin-kit-$Target.zip"

# Staged in a temporary directory, then compressed with a wildcard so the entries sit at
# the archive root instead of inside a directory named after the staging path.
$staging = Join-Path ([System.IO.Path]::GetTempPath()) "kit-package-$([guid]::NewGuid())"
New-Item -ItemType Directory -Force -Path $staging | Out-Null
try {
    Copy-Item $binary (Join-Path $staging 'plugin-asset.exe')
    Copy-Item (Join-Path $root 'LICENSE') (Join-Path $staging 'LICENSE')

    Compress-Archive -Path (Join-Path $staging '*') -DestinationPath $archive -Force
}
finally {
    Remove-Item $staging -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Output $archive
