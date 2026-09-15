#!/usr/bin/env bash
#
# cli-no-python-gate.sh — the console has no Python in its build or run path
# (ADR-051 D10, ergo EA6DY7).
#
# The gate runs a fixed argv set against the built \`degenbot\` binary and diffs
# the machine-checkable stdout against the checked-in oracle
# (cli-no-python-expected.txt). Bash + sed + diff only: no Python, no uv, no
# interpreter of any kind. The CI job that calls this deliberately does NOT
# provision Python (no actions/setup-python, no astral-sh/setup-uv).
#
# What is exercised (ADR-051 D2/D5, ADR-052 D1/D4):
#   * \`--help\` — the whole clap argv vocabulary (every group + leaf).
#   * \`--version\` — the shared build receipt (ADR-009 lockstep; the receipt is
#     normalized so a build-number advance is not oracle churn).
#   * \`database inspect\` over committed Alembic-stamped fixtures copied into a
#     temp dir (never the repo fixture: a write arm auto-heals in place, D1) —
#     ADR-052 D6 classifies by marker presence only, so both the head and the
#     stale fixture report \`legacy_alembic\`.
#   * \`exchange activate\` / \`exchange deactivate\` idempotence on a fresh DB
#     copy — first run flips, second run reports the already-in-state arm.
# Fresh copies live under a \`mktemp -d\` workdir; the committed fixtures are
# never mutated.
#
# ## Seeded-divergence proof (the comparator has teeth)
#
# Mirrors rust/examples/settlement_bot/tests/boot_gate.rs: a gate nobody can
# fail is not a gate. Two seeds, selected by DEGENBOT_CLI_GATE_SEED:
#
#   expected — mutate ONE expected line in a temp copy of the oracle
#              (\`legacy_alembic\` -> \`rust_owned\`). The run PASSES only if the
#              diff is non-empty AND names the mutated token — i.e. the
#              comparator caught the injected divergence.
#   live     — point the \`database inspect\` arm at a Rust-owned DB instead of
#              the head fixture (a real binary printing a real different schema
#              state). The run PASSES only if the diff catches it.
#
# The oracle is never rewritten. Authoring aid only:
# DEGENBOT_CLI_GATE_DUMP_ACTUAL=1 prints the normalized capture and exits 0, so
# the oracle can be regenerated deliberately and reviewed in a diff.
#
# Exit codes: 0 gate passed (or the requested seed was correctly detected);
# 1 gate failed; 2 usage/environment error.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

bin="${DEGENBOT_CLI_BIN:-rust/target/debug/degenbot}"
oracle=".github/workflows/cli-no-python-expected.txt"
head_fixture="rust/crates/degenbot-db/tests/fixtures/alembic_revs/2606a6c7f5ee.db"
stale_fixture="rust/crates/degenbot-db/tests/fixtures/alembic_revs/e0aaad8ad486.db"

if [ ! -x "$bin" ]; then
    echo "cli-no-python gate: binary not found or not executable: $bin" >&2
    echo "  build it first: cargo build -p degenbot-cli --manifest-path rust/Cargo.toml" >&2
    exit 2
fi
for fixture in "$head_fixture" "$stale_fixture"; do
    if [ ! -f "$fixture" ]; then
        echo "cli-no-python gate: Alembic fixture missing: $fixture" >&2
        exit 2
    fi
done
if [ ! -f "$oracle" ]; then
    echo "cli-no-python gate: oracle missing: $oracle" >&2
    exit 2
fi

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

# Fresh copies: \`inspect\` is read-only, but the exchange write arms auto-heal
# the Alembic fixture in place (ADR-052 D1) and would otherwise dirty the
# committed fixture.
cp "$head_fixture" "$workdir/head.db"
cp "$stale_fixture" "$workdir/stale.db"
cp "$head_fixture" "$workdir/exchange.db"

# The inspect arm's fixture. In the live seed this points at a Rust-owned copy,
# so the real binary prints a real different schema state.
inspect_fixture="$workdir/head.db"
if [ "${DEGENBOT_CLI_GATE_SEED:-}" = "live" ]; then
    inspect_fixture="$workdir/live.db"
fi

# Hermetic env: env -i clears any DEGENBOT_* leak from the host, HOME points at
# the scratch dir so no real ~/.config/degenbot is read, and NO_COLOR/TERM keep
# clap's output plain. stderr (the tracing sink) is dropped — the gate compares
# the operator stdout contract only.
run_cli() {
    env -i "PATH=$PATH" "HOME=$workdir" NO_COLOR=1 TERM=dumb CLICOLOR=0 \
        "$bin" "$@" 2>/dev/null
}

# In the live seed, materialize a Rust-owned DB (a real different schema state)
# for the inspect arm to diverge on: flip a head-fixture copy via cutover.
if [ "${DEGENBOT_CLI_GATE_SEED:-}" = "live" ]; then
    cp "$head_fixture" "$workdir/live.db"
    run_cli database cutover --force --database "$workdir/live.db" >/dev/null
fi

# One oracle section: the rendered command label, the stdout, and the exit code.
capture() {
    local label="$1"; shift
    printf '$ %s\n' "$label"
    local out code
    out="$(run_cli "$@")" && code=0 || code=$?
    printf '%s\n' "$out"
    printf 'exit %s\n' "$code"
    printf '\n'
}

actual_raw="$workdir/actual.raw"
{
    capture "degenbot --help" --help
    capture "degenbot --version" --version
    capture "degenbot database inspect --database <head.db>" \
        database inspect --database "$inspect_fixture"
    capture "degenbot database inspect --database <stale.db>" \
        database inspect --database "$workdir/stale.db"
    capture "degenbot exchange activate --chain base --name aerodrome_v2 (1st, fresh DB copy)" \
        exchange activate --chain base --name aerodrome_v2 --database "$workdir/exchange.db"
    capture "degenbot exchange activate --chain base --name aerodrome_v2 (2nd, idempotent)" \
        exchange activate --chain base --name aerodrome_v2 --database "$workdir/exchange.db"
    capture "degenbot exchange deactivate --chain base --name aerodrome_v2 (1st)" \
        exchange deactivate --chain base --name aerodrome_v2 --database "$workdir/exchange.db"
    capture "degenbot exchange deactivate --chain base --name aerodrome_v2 (2nd, idempotent)" \
        exchange deactivate --chain base --name aerodrome_v2 --database "$workdir/exchange.db"
} > "$actual_raw"

# Normalize the two volatile fields so the oracle is stable across machines and
# builds: the scratch path and the version/build receipt (ADR-009 makes the
# version single-sourced; the receipt advances with every source edit).
actual="$workdir/actual.txt"
sed -E \
    -e "s|$workdir|<tmp>|g" \
    -e 's/^(degenbot )[^ ]+( \(build ).*(\))$/\1<semver>\2<receipt>\3/' \
    "$actual_raw" > "$actual"

if [ "${DEGENBOT_CLI_GATE_DUMP_ACTUAL:-}" = "1" ]; then
    cat "$actual"
    exit 0
fi

expected="$oracle"
if [ "${DEGENBOT_CLI_GATE_SEED:-}" = "expected" ]; then
    # One injected divergence in a temp copy: flip the head-inspect label.
    expected="$workdir/mutated-expected.txt"
    awk '/^Schema state: legacy_alembic\.$/ && !seen { \
             print "Schema state: rust_owned."; seen = 1; next \
         } { print }' \
        "$oracle" > "$expected"
fi

diff_file="$workdir/diff.txt"
matched=1
if ! diff -u "$expected" "$actual" > "$diff_file" 2>&1; then
    matched=0
fi

seed="${DEGENBOT_CLI_GATE_SEED:-}"
case "$seed" in
    "")
        if [ "$matched" -eq 1 ]; then
            echo "cli-no-python gate: OK — stdout matches $oracle"
            exit 0
        fi
        echo "cli-no-python gate: FAIL — stdout diverged from $oracle" >&2
        cat "$diff_file" >&2
        exit 1
        ;;
    expected)
        if [ "$matched" -eq 0 ] && grep -q 'legacy_alembic' "$diff_file"; then
            echo "cli-no-python gate: seeded-divergence (expected mutation) detected — comparator has teeth"
            exit 0
        fi
        echo "cli-no-python gate: seeded-divergence (expected mutation) NOT detected — comparator has no teeth" >&2
        cat "$diff_file" >&2
        exit 1
        ;;
    live)
        if [ "$matched" -eq 0 ] && grep -q 'rust_owned' "$diff_file"; then
            echo "cli-no-python gate: seeded-divergence (live Rust-owned state) detected — comparator has teeth"
            exit 0
        fi
        echo "cli-no-python gate: seeded-divergence (live Rust-owned state) NOT detected" >&2
        cat "$diff_file" >&2
        exit 1
        ;;
    *)
        echo "cli-no-python gate: unknown DEGENBOT_CLI_GATE_SEED='$seed' (want 'expected' or 'live')" >&2
        exit 2
        ;;
esac
