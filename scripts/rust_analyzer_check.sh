#!/usr/bin/env bash

# rust-analyzer's `cargo check` for this workspace: the `just check-rust-default`
# shape, minus the vendored evm2 family as primary packages.
#
# rust-analyzer checks every workspace member with `--all-targets`, which selects
# rust/vendor as primary packages and compiles upstream's in-crate test modules
# against dev-deps the rewritten manifests deliberately do not declare
# (rust/vendor/README.md) — so the editor floods with diagnostics in vendored
# code that no gate ever sees. The family still compiles as a dependency of
# every consumer, which is exactly what ships; only the primary-package
# selection goes away.
#
# The excludes derive from cargo metadata via scripts/vendor_members.sh, the
# same contract the workspace-wide lanes use, so this cannot drift from the tree.
#
# Wired as rust-analyzer.check.overrideCommand (.vscode/settings.json), which
# requires JSON diagnostics on stdout.

set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

exec cargo check --locked --workspace --all-targets --message-format=json \
    $(bash "$script_dir/vendor_members.sh") \
    --manifest-path "$script_dir/../rust/Cargo.toml" "$@"
