# Package Distribution

This directory contains package manifests for various package managers.

## Releasing

1. Tag a new version: `git tag v0.2.0 && git push --tags`
2. GitHub Actions will automatically build binaries for all platforms
3. The release workflow creates artifacts with updated package manifests

## AUR (Arch Linux)

Two packages are provided:

- `trx-bin` - Prebuilt binary package (PKGBUILD-bin)
- `trx-git` - Build from source package (PKGBUILD-git)

To publish to AUR:

```bash
# Clone your AUR repo for trx-bin
git clone ssh://aur@aur.archlinux.org/trx-bin.git aur-trx-bin
cd aur-trx-bin

# Copy updated files after release
cp ../dist/aur/PKGBUILD-bin PKGBUILD
cp ../dist/aur/.SRCINFO .

# Commit and push
git add PKGBUILD .SRCINFO
git commit -m "Update to version X.Y.Z"
git push
```

## Homebrew

The formula supports macOS (Intel/ARM) and Linux (x86_64/ARM64).

To create a tap:

```bash
# Create a new tap repository: homebrew-tap
# Add the formula to Formula/trx.rb
# Users install with: brew install byteowlz/tap/trx
```

Or submit to homebrew-core for wider distribution.

## Scoop (Windows)

The manifest supports Windows x64 and ARM64.

To create a bucket:

```bash
# Create a new bucket repository: scoop-bucket
# Add trx.json to the bucket directory
# Users install with: scoop bucket add byteowlz https://github.com/byteowlz/scoop-bucket && scoop install trx
```

## Manual Installation

Download the appropriate binary from the GitHub releases page:

- **Linux x86_64**: `trx-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz`
- **Linux ARM64**: `trx-vX.Y.Z-aarch64-unknown-linux-gnu.tar.gz`
- **Linux musl x86_64**: `trx-vX.Y.Z-x86_64-unknown-linux-musl.tar.gz`
- **macOS Intel**: `trx-vX.Y.Z-x86_64-apple-darwin.tar.gz`
- **macOS Apple Silicon**: `trx-vX.Y.Z-aarch64-apple-darwin.tar.gz`
- **Windows x64**: `trx-vX.Y.Z-x86_64-pc-windows-msvc.zip`
- **Windows ARM64**: `trx-vX.Y.Z-aarch64-pc-windows-msvc.zip`

Extract and place `trx` (and `trx-tui`) in your PATH.
