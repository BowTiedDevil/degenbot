#!/usr/bin/env bash

# Classify the active Rust toolchain against the repo's contract and print the
# tier on stdout: `strict` on the reference channel rust-toolchain.toml pins,
# `portable` on any toolchain meeting the workspace MSRV. Fails closed when the
# toolchain cannot certify even the portable contract.
#
# Tiers, not a hard pin: build/test verdicts are stable across toolchains that
# meet the MSRV, but the lint and format SURFACES drift with the toolchain (a
# newer clippy carries lints the reference never had), so strict lint/format
# verification is the reference toolchain's job and CI runs it. A gate must
# never fail a compliant contributor on their toolchain's vintage.

set -euo pipefail

cd "$(dirname "$0")/.."

channel="$(awk -F'"' '/^channel = /{print $2}' rust-toolchain.toml)"
msrv="$(awk -F'"' '/^rust-version = /{print $2}' rust/Cargo.toml | head -n1)"

if ! command -v rustc >/dev/null 2>&1; then
    echo "toolchain-tier: no rustc on PATH; the workspace needs >= $msrv." >&2
    exit 1
fi

actual="$(rustc --version | awk '{print $2}')"
origin="$(command -v rustc)"

if [ "$(printf '%s\n%s\n' "$msrv" "$actual" | sort -V | head -n1)" != "$msrv" ]; then
    echo "toolchain-tier: rustc $actual at $origin is below the workspace MSRV $msrv (rust/Cargo.toml)." >&2
    echo "The gates certify nothing on it. Install $msrv or newer." >&2
    exit 1
fi

if [[ "$channel" =~ ^[0-9] ]]; then
    if [[ "$actual" == "$channel"* ]]; then
        echo "strict"
        exit 0
    fi
elif rustup show active-toolchain 2>/dev/null | grep -q "$channel"; then
    echo "strict"
    exit 0
fi

echo "portable"
echo "toolchain-tier: rustc $actual at $origin meets the MSRV ($msrv) but is not the reference '$channel'." >&2
echo "  Certifying the PORTABLE contract: build, tests, architecture gates, and the" >&2
echo "  workspace lint policy's named hard denies (rust/Cargo.toml [lints])." >&2
echo "  The strict layer (clippy --deny warnings, exact fmt) is the reference toolchain's verdict; CI runs it." >&2
