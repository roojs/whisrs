#!/usr/bin/env bash
# Build a .deb from source using Debian packaging in debian/.
#
# Usage:
#   ./scripts/build-deb.sh
#   ./scripts/build-deb.sh --minimal
#
# Requires a recent Rust toolchain (same as CI: stable via rustup) plus:
#   sudo apt install debhelper libasound2-dev libxkbcommon-dev pkg-config libclang-dev cmake
#
# Pre-built .deb packages are also published on GitHub Releases (see release.yml).

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if ! command -v cargo &>/dev/null; then
    echo "cargo not found in PATH — install Rust stable (e.g. rustup) before building." >&2
    exit 1
fi

echo "Using $(cargo --version) and $(rustc --version)"

FEATURES=""
if [ "${1:-}" = "--minimal" ]; then
    FEATURES="--no-default-features --features tray,overlay"
fi

# -d: allow building when apt's cargo/rustc are older than the lockfile requires.
CARGO=cargo FEATURES="$FEATURES" dpkg-buildpackage -b -us -uc -d

echo
echo "Built package(s) in $(dirname "$ROOT"):"
ls -1 ../*.deb 2>/dev/null || true
