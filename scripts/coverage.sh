#!/usr/bin/env bash
# LLVM coverage pipeline: three arms over one toolchain convention.
#
#   scripts/coverage.sh rust    # cargo-llvm-cov over the cargo workspace suite
#   scripts/coverage.sh pyo3    # pytest driving an instrumented degenbot._ffi cdylib
#   scripts/coverage.sh all     # both, then a merged report over the union
#
# Outputs land under rust/target/coverage/; every arm writes `html/` (human
# reading) and `coverage.json` (llvm-cov export format, agent consumption).
# The combined arm also writes `coverage.lcov` + `combined.profdata` so the
# CRAP tooling can score pytest-driven coverage:
#   cd rust && cargo crap --workspace --lcov target/coverage/coverage.lcov
#
# Env overrides:
#   COV_PACKAGES   cargo-style package selectors; empty = whole workspace.
#                  Setting this drops COV_FEATURES (scoped members can't
#                  resolve workspace-member features) unless set explicitly.
#   COV_FEATURES   features for the instrumented build (default
#                  degenbot-bot/otel — same rationale as CRAP_FEATURES)
#   COV_ROOT       output root (default rust/target/coverage)
#   COV_PYTEST_ARGS extra pytest args for the pyo3 arm (always runs the
#                  pyproject-default deselection; pass e.g. 'tests/abi -q')
#   LLVM_COV/LLVM_PROFDATA  set explicitly for non-rustup hosts; defaults to
#                  PATH lookup
#
# Toolchain expectations: rustc LLVM version must match llvm-cov/llvm-profdata
# (mismatch fails the profdata merge, not the build). No rustup needed: system
# llvm-tools (dnf install llvm) work when versions match.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COV_ROOT="${COV_ROOT:-$ROOT/rust/target/coverage}"
mode="${1:?usage: scripts/coverage.sh rust|pyo3|all}"
shift || true

export PATH="$HOME/.cargo/bin:$PATH"

if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
    echo "ERROR: cargo-llvm-cov not found. Install: cargo install --locked cargo-llvm-cov" >&2
    exit 1
fi
# LLVM tool discovery mirrors crap-coverage in the justfile: cargo-llvm-cov
# locates rustup's llvm-tools itself when present; on a system-rustc host point
# LLVM_COV/LLVM_PROFDATA at the distro LLVM explicitly.
if [[ -z "${LLVM_COV:-}" && -z "${LLVM_PROFDATA:-}" ]] \
    && command -v llvm-cov >/dev/null 2>&1 && command -v llvm-profdata >/dev/null 2>&1; then
    export LLVM_COV="$(command -v llvm-cov)"
    export LLVM_PROFDATA="$(command -v llvm-profdata)"
fi
if [[ -z "${LLVM_COV:-}" && -z "${LLVM_PROFDATA:-}" ]]; then
    echo "ERROR: no LLVM coverage tools found (rustup llvm-tools-preview or distro llvm-cov/llvm-profdata)." >&2
    exit 1
fi

# degenbot-python's test harnesses link libpython; the pytest run needs it too.
python_libdir="$(uv run --no-sync python -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"

# The cargo target dir is where cargo-llvm-cov parks its profraws + test
# binaries (<target>/llvm-cov-target). Resolve it the way cargo does.
target_dir="$(cargo metadata --manifest-path rust/Cargo.toml --no-deps --format-version 1 \
    | python3 -c 'import sys, json; print(json.load(sys.stdin)["target_directory"])')"
LV_TARGET="$target_dir/llvm-cov-target"

cov_pkg_features() {
    read -r -a pkg_args <<< "${COV_PACKAGES:-}"
    scope=(--workspace)
    [ "${#pkg_args[@]}" -gt 0 ] && scope=()
    features=()
    if [ -n "${COV_FEATURES:-degenbot-bot/otel}" ] \
        && { [ "${#pkg_args[@]}" -eq 0 ] || [ -n "${COV_FEATURES:-}" ]; }; then
        features=(--features "${COV_FEATURES:-degenbot-bot/otel}")
    fi
}

# Generates the rust-arm reports from a completed --no-report run (no test
# re-run). --json is cargo-llvm-cov's JSON, which is llvm-cov export text.
report_rust() {
    cov_pkg_features
    mkdir -p "$COV_ROOT/rust"
    cargo llvm-cov report "${scope[@]}" "${features[@]}" "${pkg_args[@]}" \
        --json --output-path "$COV_ROOT/rust/coverage.json" "$@"
    cargo llvm-cov report "${scope[@]}" "${features[@]}" "${pkg_args[@]}" \
        --html --output-dir "$COV_ROOT/rust/html" "$@"
}

# One agent JSON (llvm export, per-file summaries) + one HTML report from a
# merged profdata over the given objects.
emit_report() { # $1 name, $2.. objects
    local name="$1"; shift
    local profdata="$COV_ROOT/$name.profdata"
    llvm-cov show --instr-profile="$profdata" "$@" \
        -format=html --show-line-counts-or-regions \
        -output-dir "$COV_ROOT/$name/html" >/dev/null
    llvm-cov export --instr-profile="$profdata" --summary-only "$@" \
        >"$COV_ROOT/$name/coverage.json"
    # lcov only exists for the combined arm (it feeds `just crap` with the
    # pytest-driven coverage the cargo suite alone can't see).
    if [ "$name" = combined ]; then
        llvm-cov export --instr-profile="$profdata" -format=lcov "$@" \
            >"$COV_ROOT/coverage.lcov"
    fi
}

stack_objects() { # one path per cargo test binary (inode dedupe: deps/ + debug/ are hardlink twins)
    find "$LV_TARGET/debug" "$LV_TARGET/debug/deps" -maxdepth 1 -type f -perm -u\+x ! -name '*.so' \
        -printf '%i %p\n' 2>/dev/null | sort -n -k1,1 -k2 | awk '!seen[$1]++ { print $2 }'
}

run_rust_arm() {
    # cargo-llvm-cov must run from the workspace root (locate-project), while
    # repo-root-relative paths above stay anchored; the subshell contains cd.
    (
    cd "$ROOT/rust"
    cov_pkg_features
    # Delegating the run to cargo-llvm-cov (build + per-test profraws) keeps
    # binary enumeration exact; reports are emitted separately so the JSON and
    # HTML come from the same instrumented run.
    cargo llvm-cov "${scope[@]}" "${features[@]}" "${pkg_args[@]}" \
        --no-fail-fast --no-report "$@"
    report_rust
    )
}

run_pyo3_arm() {
    local so="$ROOT/src/degenbot/_ffi.abi3.so"
    local backup="$COV_ROOT/_ffi.abi3.so.pre-coverage"
    local raw="$COV_ROOT/pyo3-profraw"
    local so_build="$COV_ROOT/pyo3-build/debug"

    mkdir -p "$COV_ROOT" "$raw" "$COV_ROOT/pyo3"
    [ -f "$so" ] || { echo "ERROR: $so not found — install the extension first (just dev)" >&2; exit 1; }
    cp -f "$so" "$backup"
    restore_abi3() {
        cp -f "$backup" "$so"
        rm -f "$backup"
    }
    trap restore_abi3 EXIT INT TERM

    # Instrumented cdylib build with the same feature set the dev install
    # uses (pyproject [tool.maturin] features), into an isolated target dir so
    # the normal cargo/maturin caches stay untouched. RUSTFLAGS must repeat
    # .cargo/config.toml's --cfg tokio_unstable: RUSTFLAGS replaces config
    # rustflags entirely.
    RUSTFLAGS="--cfg tokio_unstable -C instrument-coverage" \
    CARGO_TARGET_DIR="$COV_ROOT/pyo3-build" \
        uv run --no-sync maturin develop

    LLVM_PROFILE_FILE="$raw/pyo3-%p-%m.profraw" \
        uv run --no-sync pytest -q --no-header ${COV_PYTEST_ARGS:-} "$@"

    [ -n "$(find "$raw" -name '*.profraw' -print -quit)" ] \
        || { echo "ERROR: pytest wrote no profraw files — the instrumented cdylib may not have been loaded" >&2; exit 1; }

    local cdylib="$so_build/libdegenbot_rs.so"
    [ -f "$cdylib" ] || { echo "ERROR: instrumented cdylib missing at $cdylib" >&2; exit 1; }
    llvm-profdata merge -sparse "$raw"/*.profraw -o "$COV_ROOT/pyo3.profdata"
    emit_report pyo3 --object "$cdylib"

    restore_abi3
    trap - EXIT INT TERM
}

run_combined() {
    # The rust-arm test binaries + the instrumented cdylib were built from the
    # same sources; a single profdata over both object sets is the union of
    # suite- and pytest-driven coverage.
    local profdata="$COV_ROOT/combined.profdata"
    mkdir -p "$COV_ROOT/combined"
    readarray -t raws < <(
        { find "$LV_TARGET" -maxdepth 2 -name '*.profraw' -print 2>/dev/null; \
          find "$COV_ROOT/pyo3-profraw" -name '*.profraw' -print 2>/dev/null; } | sort -u
    )
    [ "${#raws[@]}" -gt 0 ] || { echo "ERROR: no profraw files found — run 'scripts/coverage.sh all' first" >&2; exit 1; }
    llvm-profdata merge -sparse "${raws[@]}" -o "$profdata"

    readarray -t objects < <(
        { stack_objects; echo "$COV_ROOT/pyo3-build/debug/libdegenbot_rs.so"; } | sort -n
    )
    local cdylib_in_list=0
    for obj in "${objects[@]}"; do
        [[ "$obj" == *libdegenbot_rs.so ]] && cdylib_in_list=1
    done
    [ "$cdylib_in_list" -eq 1 ] || { echo "ERROR: instrumented cdylib missing — run the pyo3 arm first" >&2; exit 1; }

    emit_report combined --object "${objects[@]}"
}

case "$mode" in
    rust)   run_rust_arm "$@" ;;
    pyo3)   run_pyo3_arm "$@" ;;
    all)    run_rust_arm; run_pyo3_arm "$@"; run_combined ;;
    *)      echo "unknown mode '$mode' (rust|pyo3|all)" >&2; exit 1 ;;
esac

echo
echo "✓ coverage '$mode' complete — reports under ${COV_ROOT#"$ROOT"/}:"
if [ "$mode" = all ]; then
    echo "    ${COV_ROOT#"$ROOT"/}/combined/html/index.html | combined/coverage.json | coverage.lcov"
fi
