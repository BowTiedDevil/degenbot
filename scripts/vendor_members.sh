#!/usr/bin/env bash
# Print `--exclude <name>` for every vendored workspace member (rust/vendor/).
#
# The vendored evm2 family (ADR-096) is upstream's code, shipped through the
# feature sets our members request: workspace-wide lanes that select members as
# primary packages must not select it. Its in-crate test modules are upstream's
# (test harnesses, and a revm differential check, that never run here), and its
# feature matrix carries nightly-only switches that cannot compile on the
# pinned stable toolchain. The family still compiles as a dependency of every
# consumer — which is exactly what ships.
#
# The derivation lives in scripts/vendor_members.py, which is linted, formatted
# and unit-tested; this file owns the cargo invocation and the pipe.
set -euo pipefail

here="$(dirname "${BASH_SOURCE[0]}")"
cargo metadata --format-version 1 --no-deps --manifest-path "$here/../rust/Cargo.toml" |
    python3 "$here/vendor_members.py"
