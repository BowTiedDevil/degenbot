#!/usr/bin/env bash
# Canonical degenbot Rust-extension build. One home for the workspace manifest,
# the binding crate, the scoped hotpath rustflag, and the feature sets, so
# `just dev`, `just build-rust-extension`, and scripts/coverage.sh cannot drift
# apart (they historically did: the coverage arm claimed the dev feature set but
# built the defaults). Honors CARGO_TARGET_DIR — the justfile `target_dir`.
#
# Modes:
#   develop    editable install of the dev-feature extension  (`just dev`)
#   build      release-equivalent extension build, no install (`just build-rust-extension`)
#   coverage   instrumented editable install, dev features    (scripts/coverage.sh)
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

manifest="rust/Cargo.toml"
crate="degenbot_rs"

# tokio's full RuntimeMetrics getters sit behind the unstable API. The cfg is
# declared once in `.cargo/config.toml` (`[target.'cfg(all())'] rustflags`), so
# every cargo/uv build — including uv's PEP 517 project build — gets it, and the
# `hotpath` feature sets (dev, coverage) no longer rely on an ad-hoc rustflag.
# The prefix below is kept only for modes that set `RUSTFLAGS` (which overrides
# the config): coverage's `-C instrument-coverage`.
hotpath="--cfg tokio_unstable"

mode="${1:-}"
if [ "$#" -gt 0 ]; then shift; fi

case "$mode" in
    develop)
        RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$hotpath" \
            uv run --no-sync maturin develop --features dev-features "$@"
        ;;
    build)
        cargo build --locked --manifest-path "$manifest" -p "$crate" --features extension-module "$@"
        ;;
    coverage)
        RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$hotpath -C instrument-coverage" \
            uv run --no-sync maturin develop --features dev-features "$@"
        ;;
    rustflags)
        # Print the scoped hotpath rustflag for callers (the justfile check /
        # hotpath recipes) so it is defined in exactly one place.
        printf '%s' "$hotpath"
        ;;
    *)
        printf 'usage: %s {develop|build|coverage|rustflags}\n' "$(basename "$0")" >&2
        exit 2
        ;;
esac
