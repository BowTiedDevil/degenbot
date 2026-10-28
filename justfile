# Justfile for degenbot development
# https://github.com/casey/just

# Cargo target directory — per-environment, like UV_PROJECT_ENVIRONMENT and
# PYTHONPYCACHEPREFIX. The repo is bind-mounted between host and container, so a
# shared target dir makes the two sides fight over cargo fingerprints and
# artifacts; point each environment at its own dir (container: devcontainer.json;
# host: .envrc). Defaults to rust/target for CI and a bare checkout.
target_dir := env_var_or_default("CARGO_TARGET_DIR", "rust/target")

# The project-venv command prefix. `--no-sync` skips uv's project build so
# `uv run` does not re-link the extension before every recipe; a plain `uv sync`
# now builds the same dev-features extension (pyproject.toml
# `config-settings-package`), so this is a speed choice, not a correctness
# guard. Defined once so recipes never hand-write the flag.
# Recipe argv reaches the body as real positional parameters, so "$@"
# forwards each argument exactly as the caller typed it (a `{{ args }}` text
# splice re-splits quoted words in bash and silently runs the wrong thing).
set positional-arguments := true

py_run := "uv run --no-sync"

# Default recipe - show available commands
default:
    @just --list


# Print the current environment's virtualenv path (host or container, whichever
# is active — uv resolves it from UV_PROJECT_ENVIRONMENT or a local .venv).
venv:
    @{{ py_run }} python -c 'import sys; print(sys.prefix)'


# Run a command against the project venv without hand-writing `--no-sync`
# (ad-hoc use; recipes should use `{{ py_run }}` directly, which preserves
# argument quoting).
py *args:
    # argv is forwarded word-for-word through "$@"; a text splice of the raw
    # args would re-parse the joined text in bash (`just py python -c 'x=f(1)'`
    # used to die at the paren).
    {{ py_run }} "$@"

# Print the project's single source-of-truth version (the [workspace.package]
# literal in rust/Cargo.toml, inherited by every crate + bridged into the wheel
# by maturin — ADR-009).
version:
    #!/usr/bin/env python3
    import json, subprocess
    meta = subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1",
         "--manifest-path", "rust/Cargo.toml", "--no-deps"]
    )
    pkgs = json.loads(meta)["packages"]
    print(next(p["version"] for p in pkgs if p["name"] == "degenbot_rs"))

# Bump every crate to a new SEMVER version in one atomic edit (ADR-009 lockstep).
# cargo-edit updates the [workspace.package] literal, every inherited [package]
# version, every internal [workspace.dependencies] requirement, and Cargo.lock —
# version drift that felled the 0.6.0-alpha.6 crates.io publish is impossible
# here. Pass the crates.io SEMVER form (0.6.0-alpha.7), not the PEP440 tag form
# (0.6.0a7). Requires cargo-edit: cargo install cargo-edit
bump-version version:
    cargo set-version --workspace {{ version }} --manifest-path rust/Cargo.toml

#
# Release-checklist step (schema-carrying releases): bump the Rust schema
# ritual in lockstep — `RUST_SCHEMA_VERSION` in `degenbot-db/src/schema.rs` →
# append the matching `MigrationStep` to `RUST_MIGRATIONS` in
# `degenbot-db/src/migrate.rs` → extend the fixture matrix (one DB per released
# revision). Verify the publish chain with `just publish-dry-run`.

# ========== Rust Development ==========

# Local Cargo and maturin development builds intentionally use the workspace
# [profile.dev]: opt-level = 1, no LTO or stripping, and line-tables-only debug
# info. This favors representative optimization while keeping rebuilds cheaper
# than release. Release builds keep thin LTO, stripping, and the intentional
# per-package codegen-unit policy documented in rust/Cargo.toml.

# Build the native Tauri AppImage for the GUI proof of concept. The app-local
# npm script supplies the AppImage extraction and linuxdeploy NO_STRIP settings
# required by modern Fedora ELF libraries.
tauri-appimage:
    cd apps/tauri-poc && npm run appimage

# Print the active Rust toolchain and the repository policy. The root
# rust-toolchain.toml pins development and release builds to Rust 1.98.1;
# the workspace MSRV is Rust 1.97, checked by `just check-msrv` (CI's msrv job).
toolchain:
    @echo "Pinned Rust policy: 1.98.1 (workspace MSRV: 1.97)"
    @rustc --version
    @cargo --version

# Classify the active toolchain against the workspace contract and enforce its
# floor. Tiers, not a hard pin: any toolchain meeting the MSRV certifies the
# portable contract (build, tests, architecture gates, clippy's deny-by-default
# lints), while the strict lint/format layer is the reference channel's verdict
# and runs in CI — a compliant contributor's toolchain vintage must never fail a
# gate. Classification lives in scripts/toolchain_tier.sh (one owner).
check-toolchain:
    #!/usr/bin/env bash
    set -euo pipefail
    tier="$(scripts/toolchain_tier.sh)"
    echo "toolchain: $tier tier certified"

# Resolves the reth-vs-anvil ambiguity behind tests reporting an anvil
# conclusion from a reth response. Ask the node; never infer from PATH.
# Measured shape matrix and sim-gate implications: /skill:node-identity
node-identity:
    @curl -s -X POST -H 'content-type: application/json' \
        --data '{"jsonrpc":"2.0","id":1,"method":"web3_clientVersion","params":[]}' \
        "${TEST_ETHEREUM_FULL_NODE_HTTP_URI:?set TEST_ETHEREUM_FULL_NODE_HTTP_URI}"

# Run the standalone-Rust-consumer smoke (ADR-005 standalone claim). Proves a
# `cargo add degenbot` consumer reaches BotState/DexIdentity/calc math with no
# Python in the build graph. `examples/standalone_consumer.rs` panic!s on any
# check failure, so this is the standalone-consumer gate. The example is a
# `cargo add degenbot` showcase binary AND a CI-runnable assertion.
test-standalone:
    cargo run --locked --manifest-path rust/Cargo.toml -p degenbot --example standalone_consumer
    # The WS-ingestion crate's headless boot (no Python, no net).
    cargo run --locked --manifest-path rust/Cargo.toml -p degenbot-ingestion --example headless_boot

# ========== Tests ==========
#
# "Run the tests" is no longer a language choice: Python is a driver shell over
# the Rust core, so the default gate runs BOTH the native Rust suite and the
# full pytest suite (which itself drives the core through the PyO3 seam, golden
# on-chain-oracle replay, and the relocated seam suites) under one entrypoint.
# CI and the pre-push hook still address the language tracks directly
# (`test-rust` / `test-python`) so the python-version matrix and job
# partitioning keep working. Deliberately excluded from `test` (run on demand):
# toolchain-gated Tier-3 harness rebuilds (`test-tier3` / `verify-tier3-*`) and
# net-gated suites (`test-fork`, `record-golden`, `verify-deployments`).

# Default gate: workspace lint (fails fast) + the retirement-tripwire ratchet +
# standalone smoke + cargo workspace + full pytest. The ratchet is a ~1s source scan
# (ADR-087 D4), so it rides here as well as at pre-push: a tripwire introduced while
# iterating fails at `just test`, not only at the push that follows.
test: lint-rust-check lint-tripwire-ratchet test-rust-nextest test-python check-consumer-probes

# Run the pre-push gates before `git push`. prek.toml's
# `stages = ["pre-push"]` entries are the single owner of the gate list (the
# push-gate manifest, ADR-074); this recipe only delegates to them, so it can
# never drift into a shorter list that reports green while the push is blocked
# by a gate the recipe never ran. prek is a global uv tool (see
# `just setup-git-hooks`), installed on demand here.
#
# live-invariant gate (ADR-087 D2): watches the live `stages = ["pre-push"]` entries in prek.toml; not tripwire precedent.
pre-push:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v prek >/dev/null 2>&1 || uv tool install prek
    prek run --stage pre-push

# Run only the Rust track (standalone smoke + cargo workspace). CI's rust-test
# job and the pre-push hook call this subunit directly; humans use `just test`.
test-rust: check-toolchain test-standalone
    #!/usr/bin/env bash
    set -euo pipefail

    # The venv python's LIBDIR for LD_LIBRARY_PATH (dep-crate test binaries
    # link libpython). BEST-EFFORT BY CONTRACT, unlike the rest of this recipe
    # (which fails closed): this is the CI rust job's entrypoint and that job
    # has no `uv`; an omitted probe just lets rust-lld autoprobe a LIBDIR.
    # Tolerated ONLY here -- `test-rust-nextest` and the coverage recipe are
    # uv-required by design and stay strict.
    python_libdir=""
    if command -v uv >/dev/null 2>&1; then
        python_libdir="$({{ py_run }} python -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))' || true)"
    else
        echo "note: uv not found; skipping the python-libdir probe (best-effort)" >&2
    fi
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    # vendored deployments.json (degenbot-uniswap) must match the canonical
    # Python-tree registry file byte-for-byte: a crate can only embed
    # in-tarball files, so the embed uses the in-crate mirror
    cmp -s src/degenbot/registry/deployments.json rust/crates/foundation/degenbot-uniswap/src/deployments.json || { echo 'ERROR: deployments.json vendor drift (canonical vs degenbot-uniswap mirror)' >&2; exit 1; }
    # The fixture-corpus and lab features ride every workspace lane (ADR-084
    # D3): the corpora and the simulation harness stay EXECUTED here, never
    # silently skipped behind required-features, and the assertion-free green
    # exit of a zero-match filter can not hide a lost corpus.
    cargo test --locked --manifest-path rust/Cargo.toml --workspace --features degenbot-pools/capture-fixtures,degenbot-bot/executor-ab-probe,degenbot-simulation/test-harness

# Reproduce CI's test-binary build exactly (ci.yml sets these two globally):
# a crate-private fn's LAYOUT can depend on profile flags -- the ADR-071 D3
# entry-row identity gate failed in CI under exactly these while passing
# locally (2026-10-14). Any "passes here, fails in CI" suspicion gets
# reproduced with this recipe before anything else is theorized.
test-rust-ci:
    CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 just test-rust


# The fleet budget-algebra (pure plan/budget tiering) and the
# binding-parity gates (pinned-vs-serial outcome-corpus identity). NAMED so a
# fleet-tier regression is readable in any check log, CI's or the hook's.
test-fleet-parity:
    cd rust && cargo test --locked -p degenbot-workers -- plan budget
    # executor-ab-probe: the outcome corpus lives in a feature-gated island
    # (ADR-084 D3) — without the feature this filter would match zero tests
    # and exit green over a silently skipped corpus.
    cd rust && cargo test --locked -p degenbot-bot --features executor-ab-probe -- the_outcome_corpus lane_death


# Two-sided golden replay gate (offline, seconds): profit under/over-shoot and
# nondeterminism across reps, on the single-block anchor + the 156-row heavy
# corpus. Blocks merge in CI; named here so the pre-push gate runs it too.
test-golden-replay:
    cd rust && cargo run --locked -q -p degenbot-pools --features capture-fixtures --example concentrated_liquidity_solve_replay -- crates/foundation/degenbot-pools/tests/fixtures/concentrated_liquidity_capture_offline.jsonl
    cd rust && cargo run --locked -q -p degenbot-pools --features capture-fixtures --example concentrated_liquidity_solve_replay -- crates/foundation/degenbot-pools/tests/fixtures/heavy_concentrated_liquidity_solve_captures.jsonl


# Run the cargo workspace suite under cargo-nextest: per-process test
# isolation, cross-binary parallelism, and per-test durations as JUnit XML at
# <cargo target dir>/nextest/ci/nextest-ci.junit.xml. `--no-fail-fast` so one failing
# test never aborts the whole run.
# Requires cargo-nextest: cargo install --locked cargo-nextest
# Extra args forward to `cargo nextest run` (e.g. `just test-rust-nextest -E 'test(foo) or test(bar)'`
# or `just test-rust-nextest -p degenbot-substrate`). They cross the recipe
# boundary as real argv (`"$@"` under `set positional-arguments`), so quoted
# multi-word filters keep their grouping and parens are never bash-parsed.
# `-p`/`--package` must ALSO drop the trailing `--workspace`: nextest gives
# --workspace precedence and silently runs every workspace member when both
# selections are present, so the documented `-p <crate>` narrowing would never
# narrow.
#
# Why the whole-workspace default over `-p <crate>`: resolver v3 unifies
# features for `--workspace`, so its artifacts are the one warm, canonical set
# in `rust/target`. A `-p <crate>` selection unifies features differently
# (core crates lose the `pyo3` feature the binding layer enables; dep features
# like tokio's shrink), so cargo stores a second rlib set under different
# metadata hashes — alternating between the two rebuilds shared dependencies on
# every shared edit (measured: `-p degenbot-simulation` recompiled 6 just-built
# crates in ~1m; a leaf crate pays nothing). The workspace run also executes
# every crate's suite, catching cross-crate fallout (signature changes
# rippling into dependents, e.g. examples/settlement_bot) that a scoped run
# never sees.
test-rust-nextest *args: check-toolchain
    #!/usr/bin/env bash
    set -euo pipefail
    python_libdir="$({{ py_run }} python -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
    export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    # argv rides "$@" — each flag and its value reach cargo intact (`-p <crate>`,
    # `-E 'test(a) or test(b)'`). `-p`/`--package` must ALSO drop the trailing
    # `--workspace`: nextest gives --workspace precedence and silently runs
    # every workspace member when both selections are present.
    selection="--workspace"
    for arg in "$@"; do
        case "$arg" in
            -p|--package|--package=*) selection="" ;;
        esac
    done
    # Fixture-corpus and lab features ride every lane (ADR-084 D3): the golden
    # corpora and the simulation harness are executed here, never silently
    # skipped behind required-features.
    cargo nextest run --locked --manifest-path rust/Cargo.toml $selection --profile ci --no-fail-fast --features degenbot-pools/capture-fixtures,degenbot-bot/executor-ab-probe,degenbot-simulation/test-harness "$@"

# Run the fork/RPC-gated suites — the legs every offline gate (test, pre-push,
# CI) deliberately deselects:
#   - the pytest live-RPC tiers (`online_rpc` / `base`, minus `slow`);
#   - the Rust live revm fork-truth legs: the `#[ignore]`d `live_*` tests in
#     degenbot-simulation, gated on the chain-1 endpoint env family
#     `DEGENBOT_RPC_HTTP_CHAINID_1` (exported here from the probe target).
# Fails CLOSED unless the machine DECLARES an endpoint (process env or the
# gitignored tests.env, resolved through tests/conftest.py: env > tests.env >
# table default — the same layers the pytest legs dial, so the probe can never
# green-light one machine while the tests hit another) AND that endpoint answers
# eth_chainId (probe, don't assume). The conftest table default is not a lane
# declaration: it serves the offline suite's imports. The devcontainer's reth
# satisfies both (containerEnv sets http://host.containers.internal:8545/, WS
# :8546); any reachable archive endpoint works.
# On demand only: minutes of live-RPC work, never in `test` / pre-push / CI.
test-fork: dev
    #!/usr/bin/env bash
    set -euo pipefail
    # Resolve the probe target through tests/conftest.py's declared layers so
    # this gate dials exactly what the pytest legs dial.
    rpc_uri="$({{ py_run }} python -c 'from tests.conftest import declared_rpc_uri; print(declared_rpc_uri("TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI") or "")')"
    if [ -z "$rpc_uri" ]; then
        echo "ERROR: no RPC declared — set TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI (process env or tests.env) to a reachable Ethereum endpoint" >&2
        echo "       (the devcontainer sets http://host.containers.internal:8545/ for its reth)" >&2
        exit 1
    fi
    chain_id=""
    chain_id="$(curl -sS -m 10 -X POST -H 'Content-Type: application/json' \
        --data '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' "$rpc_uri")" \
        || chain_id=""
    if [[ ! $chain_id == *'"result"'* ]]; then
        echo "ERROR: no reachable Ethereum RPC at TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI=$rpc_uri" >&2
        echo "       (eth_chainId response: ${chain_id:-<none>}) — start the node or re-point the URI" >&2
        exit 1
    fi
    echo "▶ fork lane RPC: $rpc_uri (chain $({{ py_run }} python -c 'import json,sys; print(int(json.load(sys.stdin)["result"], 16))' <<<"$chain_id"))"
    echo
    echo "▶ pytest fork legs: -m '(online_rpc or base) and not slow'"
    pytest_rc=0
    {{ py_run }} pytest -m '(online_rpc or base) and not slow' -q --no-header || pytest_rc=$?
    echo
    echo "▶ Rust live revm fork legs: degenbot-simulation live_* (run-ignored only)"
    rust_rc=0
    DEGENBOT_RPC_HTTP_CHAINID_1="$rpc_uri" cargo nextest run --locked --manifest-path rust/Cargo.toml \
        -p degenbot-simulation -E 'test(live_)' --run-ignored only --no-fail-fast || rust_rc=$?
    echo
    # Both legs always run: a failing first leg must not starve the lane of
    # the second leg's evidence. The lane still fails closed on any leg.
    if [ "$pytest_rc" -ne 0 ] || [ "$rust_rc" -ne 0 ]; then
        echo "✗ fork lane FAILED (pytest exit ${pytest_rc}, rust exit ${rust_rc})" >&2
        exit $(( pytest_rc != 0 ? pytest_rc : rust_rc ))
    fi
    echo "✓ fork lane complete."

# Rank the slowest tests from the last `just test-rust-nextest` JUnit report.
test-timing top='30':
    #!/usr/bin/env bash
    set -euo pipefail
    {{ py_run }} python - <<'EOF'
    import statistics, xml.etree.ElementTree as ET
    top = int("{{top}}")
    tree = ET.parse("{{ target_dir }}/nextest/ci/nextest-ci.junit.xml")
    cases = sorted(
        ((float(tc.get("time")), f"{tc.get('classname')}. {tc.get('name')}") for tc in tree.iter("testcase")),
        reverse=True,
    )
    serial = sum(d for d, _ in cases)
    slow = [c for c in cases if c[0] >= 1.0]
    print(f"tests: {len(cases)}  serial sum: {serial:.1f}s  median: {statistics.median(d for d, _ in cases)*1000:.1f}ms")
    print(f"tests >= 1s: {len(slow)}  combined: {sum(d for d, _ in slow):.1f}s ({100*sum(d for d, _ in slow)/serial:.0f}% of serial time)")
    for d, name in cases[:top]:
        print(f"  {d:8.3f}s  {name}")
    EOF

# The replay bench (GLOSSARY “replay bench”, ADR-068 D6): the ONE
# command reproducing the updater per-chunk baseline — the real chunk loops
# over the committed seed cassettes through the replay transport (zero
# network), reporting RPC round trips + response bytes, SQL statement count,
# stage wall times, the write-lock hold, and chunks/sec. The table this
# prints is the survey doc's Baseline section (docs/updater-rpc-sql-survey.md);
# the Perf A–E tasks re-rank against it. Uses the bench profile (release-like).
bench-updaters:
    cargo bench --locked --manifest-path rust/Cargo.toml -p degenbot --features degenbot/sql-ledger --bench updater_replay_bench

# The wave-2 EVM-oracle capture generator's gate: regenerate the
# wave-2 cassettes + SQL goldens into a temp home and exit 1 on any byte
# drift against the committed artifacts. The example only builds under the
# `sql-ledger` passthrough (its [[example]] required-features, same shape as
# the updater_replay_bench [[bench]] entry above) — this recipe is the one
# command that carries the feature.
check-evm-captures:
    cargo run --locked --manifest-path rust/Cargo.toml -p degenbot --features degenbot/sql-ledger --example generate_evm_oracle_captures -- --check tests/fixtures/cassettes/wave2 tests/fixtures/sql_goldens/wave2

# The wave-3 aave-side adversarial capture generator's gate: the
# same drift gate as check-evm-captures over the wave-3 fixture homes.
check-aave-captures:
    cargo run --locked --manifest-path rust/Cargo.toml -p degenbot --features degenbot/sql-ledger --example generate_aave_adversarial_captures -- --check tests/fixtures/cassettes/wave3 tests/fixtures/sql_goldens/wave3

# crates.io publish oracle (crates-io-publishing-prep handoff §2, gate G1):
# verification-builds every publishable workspace member in dependency order.
# ~20-40 min cold. CI's PR gate (check-publish) runs the clean-tree form.
publish-dry-run *args:
    #!/usr/bin/env bash
    set -euo pipefail
    cd rust
    cargo publish --locked --workspace --dry-run --allow-dirty {{ args }}

# Which publishable members are live on crates.io at the workspace version, and
# which are not — the audit the release workflow's crates.io leg runs before every
# upload round. The oracle is the crates.io sparse index (what `cargo publish`
# gates on), not `cargo search`, whose backend lags publication. Prints pending
# members; pass `--live` or `--classify` for the other views.
crates-io-pending *args:
    scripts/crates_io_pending.sh "$@"

# Authoritative CI/pre-push Clippy gate. It deliberately checks default
# features without `--all-features`, so test-only and mutually exclusive build
# variants cannot hide a default regression; it runs the architecture subset
# first, matching CI exactly, and never edits the tree.
# Run the non-mutating Rust Clippy check (CI-identical gate).
# Default Rust lint lane, tier-aware (scripts/toolchain_tier.sh): on the
# reference toolchain this is CI-identical (`--deny warnings`); on any other
# supported toolchain it runs clippy's deny-by-default floor only, because the
# lint surface drifts with the toolchain and CI is the strict authority.
# `--all-features` stays off; the one named feature carried here is
# `degenbot-simulation/test-harness`, which gates a lab `src` module (not a
# build variant) out of the shipped crate root, so the lab keeps its
# `--deny warnings` coverage. The feature-OFF production shape — every
# dependent compiling without the harness — is `check-rust-default`'s claim.
lint-rust-check: check-toolchain check-no-inner-allow check-engine-impl-blocks check-cli-shell-purity check-config-derived-statics check-capability-catalog check-dependency-charter
    #!/usr/bin/env bash
    set -euo pipefail
    tier="$(scripts/toolchain_tier.sh)"
    if [ "$tier" = "strict" ]; then
        cargo clippy --locked --workspace --all-targets --manifest-path rust/Cargo.toml --features degenbot-simulation/test-harness -- --deny warnings
    else
        cargo clippy --locked --workspace --all-targets --manifest-path rust/Cargo.toml --features degenbot-simulation/test-harness
    fi

# Default Rust lint lane: a CI-identical, non-mutating alias of
# `lint-rust-check`, so a green run here is exactly what CI will accept.
# Run the default non-mutating Rust Clippy check (alias of `lint-rust-check`).
lint-rust: lint-rust-check

# Check every workspace member with its declared default features. This is the
# independent default-feature check; it is not an all-features build.
check-rust-default:
    cargo check --locked --workspace --all-targets --manifest-path rust/Cargo.toml

# Check the pure-Rust umbrella consumer surface, including its standalone
# examples, without selecting the PyO3 binding crate.
check-rust-consumer:
    cargo check --locked -p degenbot --all-targets --manifest-path rust/Cargo.toml

# Check the binding crate with its intentionally broad default domain surface,
# but without the extension-module link mode.
check-rust-binding-default:
    cargo check --locked -p degenbot_rs --all-targets --manifest-path rust/Cargo.toml

# Check the dev-wheel feature set. Keep this explicit: these profiling,
# telemetry, allocator, and allocator-selection features are development-only
# and are not part of the release-wheel or standalone-default matrix.
check-rust-dev-features:
    RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$(scripts/build_extension.sh rustflags)" cargo check --locked -p degenbot_rs --all-targets --manifest-path rust/Cargo.toml --features dev-features

# Check the release-equivalent extension feature set in the release profile.
# The release maturin command uses `pyo3/extension-module`; this package feature
# forwards to the same PyO3 feature while retaining the binding crate defaults.
check-rust-extension-release:
    cargo check --locked --release -p degenbot_rs --lib --manifest-path rust/Cargo.toml --features extension-module

# Exhaustive all-features compilation is an explicit diagnostic, not the
# default gate. It intentionally includes test-only and mutually exclusive
# variants and must never be used to validate default or release behavior.
check-rust-all-features:
    RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$(scripts/build_extension.sh rustflags)" cargo check --locked --workspace --all-targets --all-features --manifest-path rust/Cargo.toml

# Hotpath test lane. Tokio's full RuntimeMetrics getters sit behind the
# unstable API and there is no global rustflag, so any direct hotpath
# Cargo/maturin invocation must set the cfg explicitly. Extra args forward to
# `cargo test` (e.g. `just test-hotpath -- frame_pipeline`).
# Canonical form; do not hand-type the RUSTFLAGS.
test-hotpath *args:
    RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }$(scripts/build_extension.sh rustflags)" cargo test --locked --manifest-path rust/Cargo.toml -p degenbot-bot --features hotpath {{ args }}

# Forbid file-level inner "#![allow]" - clippy's allow_attributes catches only the
# outer #[allow] form; this closes the historical inner-attribute loophole it
# misses. One reasoned outer #[allow(..., reason = "...")] remains permitted for
# the legitimate cross-target conditional suppressions #[expect] cannot express.
# The MSRV compile check CI runs (the `msrv` job): the whole workspace at the
# `rust-version` floor, all targets. Local name for the job's raw command.
check-msrv:
    #!/usr/bin/env bash
    set -euo pipefail
    # Fail CLOSED with a clear reason when `cargo +toolchain` cannot work
    # (cargo not proxied by rustup). Never fall back to the active toolchain:
    # compiling at 1.98.1 and calling it the MSRV would report a false green.
    if ! cargo +1.97.0 --version >/dev/null 2>&1; then
        echo "ERROR: the MSRV check needs rustup with the 1.97.0 toolchain (rustup toolchain install 1.97.0);" >&2
        echo "this cargo does not accept +toolchain directives. CI's msrv job runs this check." >&2
        exit 1
    fi
    cargo +1.97.0 check --manifest-path rust/Cargo.toml --workspace --all-targets --locked


# live-invariant gate (ADR-087 D2): watches the live `rust/crates` sources for inner `#![allow]` attributes; not tripwire precedent.
check-no-inner-allow:
    # C7: the gate body lives as a cargo test on the umbrella crate
    # (rust/crates/facade/degenbot/tests/architecture_gates.rs).
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test architecture_gates -- no_inner_allow_attributes --exact --nocapture

# Check Rust formatting (read-only; fails on drift). Run `just format` to fix.
fmt-check: check-toolchain
    #!/usr/bin/env bash
    set -euo pipefail
    cargo fmt --manifest-path rust/Cargo.toml --all -- --check || {
        echo "fmt-check: the diff above is this rustfmt's verdict. The reference formatter" >&2
        echo "  is the pinned channel's rustfmt (rust-toolchain.toml). If 'just format'" >&2
        echo "  under it produces no diff, this is a formatter-version artifact." >&2
        exit 1
    }

# Enforce the no-pyo3-in-core invariant. Pure Rust core crates must
# not depend on pyo3 under their default features. Add new core crates here.
#
# live-invariant gate (ADR-087 D2): watches the default-feature dependency set of every live core crate; not tripwire precedent.
check-no-pyo3-in-cores:
    # C7: the gate body lives as a cargo test on the umbrella crate
    # (rust/crates/facade/degenbot/tests/architecture_gates.rs).
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test architecture_gates -- core_crates_are_pyo3_free_under_default_features --exact --nocapture

# live-invariant gate (ADR-087 D2): watches the live `degenbot-cli-core` crate's dependency set, which must stay clap- and indicatif-free; not tripwire precedent.
check-cli-core-purity:
    # C7: the gate body lives as a cargo test on the umbrella crate
    # (rust/crates/facade/degenbot/tests/architecture_gates.rs).
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test architecture_gates -- cli_core_is_clap_and_indicatif_free --exact --nocapture

# Enforce the ADR-051 D2 dependency charter for the argv facade: `degenbot-cli`
# may name workspace members (the clap-free semantics crate + the sink crates)
# plus a small, explicit external allowlist - argv spelling (clap), progress
# rendering (indicatif), the SIGINT runtime (tokio) and the sink stack (tracing
# / tracing-subscriber). Anything else is a layering regression: the facade
# maps argv into `degenbot-cli-core` and must never reach a domain-engine crate
# on its own. Mirrors check-cli-core-purity / check-no-pyo3-in-cores.
#
# live-invariant gate (ADR-087 D2): watches the external dependency allowlist of the live `degenbot-cli` facade; not tripwire precedent.
check-cli-shell-purity:
    # C7: the gate body lives as a cargo test on the umbrella crate
    # (rust/crates/facade/degenbot/tests/architecture_gates.rs).
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test architecture_gates -- cli_shell_names_only_allowlisted_externals --exact --nocapture


# Structural gate: the engine
# seam deepens until `EngineStages` is the ONE external driver surface and
# every `impl ArbitrageEngine` block outside `arb_engine/mod.rs` is dissolved.
# Wired into the Rust hygiene set
# (`lint-rust-check` + the `rust-engine-impl-blocks` pre-commit hook + CI).
# The `{` anchor keeps the prose mention of `impl ArbitrageEngine` in
# mod.rs's module-tree comment from being counted as a block.
#
# Two assertions:
#   1. exactly ONE `impl ArbitrageEngine` block, in arb_engine/mod.rs.
#   2. `ArbitrageEngine` appears in degenbot-python/src exactly ONCE - the
#      pyclass `name = "ArbitrageEngine",` compat string (the deliberate
#      Python-API name exemption). Any other hit is a seam regression.
#
# live-invariant gate (ADR-087 D2): watches the one live `impl ArbitrageEngine` block in `arb_engine/mod.rs` and the pyclass compat string; not tripwire precedent.
check-engine-impl-blocks:
    # C7: the gate body lives as a cargo test on the umbrella crate
    # (rust/crates/facade/degenbot/tests/architecture_gates.rs).
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test architecture_gates -- one_engine_impl_block --exact --nocapture


# Structural ratchet for the config-holder test seam: every
# process-once static (`OnceLock`/`LazyLock`/`OnceCell`/`Once`) whose
# initialization path reads `degenbot_config::holder::config()`/`config_arc()`
# (or the substrate `stance::config()` delegate) must be classified in
# rust/crates/facade/degenbot/tests/config_derived_statics_census.txt as
# per-read | republishable | process-lifetime. The call-site audit is a
# snapshot in a result doc; this gate makes it durable, so the next
# holder-derived once-static cannot land silently stale-able. Wired into
# `lint-rust-check` (hence `just lint` / CI / the pre-push `rust-clippy` hook)
# plus the pre-push `rust-config-derived-statics` hook.
#
# live-invariant gate (ADR-087 D2): watches every live config-derived process-once static against `tests/config_derived_statics_census.txt`; not tripwire precedent.
check-config-derived-statics:
    # C7: the gate body lives as a cargo test on the umbrella crate
    # (rust/crates/facade/degenbot/tests/architecture_gates.rs).
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test architecture_gates -- config_derived_statics_are_classified --exact --nocapture

# ADR-084 D1 capability-catalog ratchet: docs/capabilities.md must name every
# publishable crate at least once, resolve every home path against the module
# tree, land every umbrella alias on a catalogged crate, and keep the console
# rows on consumer (v). A new capability without a catalog row fails here.
# Wired into `lint-rust-check` (hence `just lint` / CI).
check-capability-catalog:
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test architecture_gates -- capability_catalog_covers_every_public_crate --exact --nocapture

# ADR-084 D3 dependency-charter gate: every deny rule in
# rust/crates/facade/degenbot/tests/dependency_charter.txt asserted over
# `cargo tree -p X -e normal` at single-package, default-feature resolution.
# A new C-build or pyo3 leak into a seam crate's published dependency set
# fails here. Wired into `lint-rust-check`.
#
# live-invariant gate (ADR-087 D2): watches the resolved dependency tree of every live crate against the ADR-084 D3 deny rules; not tripwire precedent.
check-dependency-charter:
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test architecture_gates -- dependency_charter_is_respected --exact --nocapture

# ADR-084 D1/C5 consumer probes: each named catalog consumer completes a real
# task through its own surface. (ii) the pools-only Rust consumer solves a
# path through the solve door; (iii) the Python driver solves through the FFI
# seam; (v) the console operator runs the offline `config show --resolved`
# task. Wired into `just test`; `check-rust-consumer` only checks the umbrella
# and never runs pools' examples, so it cannot satisfy C5.
check-consumer-probes:
    cargo run --locked --manifest-path rust/Cargo.toml -p degenbot-pools --features capture-fixtures --example solve_path_probe
    {{py_run}} pytest tests/standalone_parity/test_solve_surface_probe.py -q -p no:cacheprovider
    {{py_run}} degenbot config show --resolved > /dev/null

# Build Rust extension module in the release-equivalent feature set. This uses
# the workspace development profile (opt-level = 1, no LTO or stripping) for
# local validation; the feature set is the same as the release maturin
# invocation.
build-rust-extension:
    scripts/build_extension.sh build

# ========== Build-Artifact Housekeeping ==========

# Reclaim disk space under the Cargo target roots. The sweep covers BOTH
# environment roots when they exist — the devcontainer's in-tree rust/target
# and the host's out-of-tree ~/.cache/degenbot-target (bind-mounted into the
# container; see devcontainer.json) — so one invocation reclaims both sides
# from either side. Set GC_TARGET_DIR to narrow the sweep to exactly one root.
# Preview first; both modes report every cache family and exact target/reclaimable
# byte counts:
#   DRY_RUN=1 just gc-target     # report candidates; delete nothing
#   AGE=3 just gc-target         # remove artifacts older than three days
#   AGE=0 DRY_RUN=1 just gc-target
#
# Normal Cargo caches age-sweep deps/examples/build/.fingerprint, always drop
# incremental state, and dedupe old large test binaries. Separate rebuildable
# families are age-swept too: coverage reports/builds, LLVM coverage, Criterion,
# wheels, and rustdoc output. Every root's maturin/ cache is always protected.
# The family policy is this script.
gc-target:
    scripts/gc-target.sh

# ========== Python Development ==========

# Install the locked Python environment without building the project a second
# time. `dev` is the sole editable-extension install path and selects the
# canonical Cargo `dev-features` alias explicitly.
bootstrap:
    uv sync --locked --dev --no-install-project
    just dev

# Build and install the Python extension in development mode. Maturin consumes
# pyproject's `profile = "dev"` setting, which matches rust/Cargo.toml's
# opt-level = 1 development profile (not an unoptimized debug build). Run
# `just bootstrap` first; `--no-sync` keeps this build from silently replacing
# the explicit development feature set with ordinary package defaults.
dev:
    scripts/build_extension.sh develop


# `uv.lock` must match `pyproject.toml` -- the uv mirror of the rust side's
# `cargo metadata --locked` check (the `test-rust` job's first gate). Runs in
# the CI `python` job; run it locally after editing dependencies.
check-uv-lock:
    uv lock --check

# Run only the Python track (full pytest). CI's python-test matrix job and the
# pre-push hook call this subunit directly; humans use `just test`. Under the
# default offline marker filter (`-m "not slow and not base and not online_rpc and not stub_drift"`)
# this covers the PyO3 seam, golden on-chain-oracle replay, AND the relocated
# seam suites. A focused parity-only run is `just py pytest -m onchain_oracle`.
#
# The `dev` prerequisite builds + installs the extension (incremental `maturin
# develop`) so the suite always exercises the current Rust sources. This replaces
# the retired runtime build-receipt/staleness gate: freshness is guaranteed by
# building the thing we test, not by inspecting whether a build happened.
test-python: dev
    {{ py_run }} pytest -x -q --no-header

# Spray tripwire over the Python suite (scripts/pytest_noise_gate.sh). Runs
# pytest with the whole child-process output captured to a log file, then scans
# the capture for the two signatures of the historical "exception spray" class:
# `--- Logging error ---` blocks and raw `Reconnection attempt` lines. Those
# printed to the real stdout/stderr AFTER the pytest session ended and left the
# exit code at 0, which is why post-session output is invisible to pytest
# plugins and the scan has to live at the process/recipe level.
#
# The suite's own exit code is captured separately from the signature scan and
# propagated unchanged, so a genuine test failure is never masked by a clean
# signature scan; a spray signature instead yields its own distinct nonzero
# exit. The single positional argument is the pytest keyword filter, quoted as
# one argument so `just`'s unquoted variadic splicing cannot shred it; the
# default is empty (full suite). Trailing tokens forward verbatim:
#   just test-python-noise-gate                  # full suite
#   just test-python-noise-gate 'not readme'     # skip the README sybil items
#   just test-python-noise-gate '' tests/golden  # full suite plus a path arg
#
# Negative probe (the red path, demonstrable without re-running the suite):
#   printf '%s\n' 'Reconnection attempt' > /tmp/spray.log
#   bash scripts/pytest_noise_gate.sh scan /tmp/spray.log   # exits 3, prints it
#
# CI: armed as the python job's "Run Tests" step in .github/workflows/ci.yml
# (it replaced the bare `just test-python` call; that recipe is untouched). The
# pre-push hook in prek.toml also arms it (`python-test` -> this recipe), so a
# local push runs the same tripwire; the bare `just test-python` recipe stays
# for callers that only want the suite's exit code.
test-python-noise-gate kw="" *args: dev
    #!/usr/bin/env bash
    set -euo pipefail
    # Usage: just test-python-noise-gate [KW] [PYTEST_ARGS ...]. KW is the -k
    # expression (quoted as one word here); everything after it rides "$@" so
    # extra pytest args and multiple test targets reach pytest exactly as typed
    # (a raw text splice re-split those words and silently ran the wrong tests).
    kw="${1-}"; [ $# -eq 0 ] || shift
    if [ -n "$kw" ]; then
        exec bash scripts/pytest_noise_gate.sh run -k "$kw" "$@"
    fi
    exec bash scripts/pytest_noise_gate.sh run "$@"

# Flake probe for the ordering-sensitive suites (session/operator-channel
# state machines, where inter-test order leaks surface): run both suites
# three consecutive times with the cache provider off; ANY failing run fails
# the recipe. Each invocation is `timeout`-bounded as a process-level backstop
# beside pytest-timeout's in-process guard, so the probe completes or fails —
# never hangs.
# Mirror of the CI python job's NON-floor legs: the suite with `anvil` NOT
# reachable (CI installs Foundry only on the 3.12 floor leg). Guarded tests
# must skip cleanly -- an unguarded spawn fails at `AnvilFork(...)`, exactly
# the regression class this caught once (2026-10-14). The simulation + its
# fail-closed reachability check live in scripts/no_anvil_run.sh. This full
# suite is the manual mirror of those CI legs; the pre-push gate runs the
# spawn-surface slice below instead (~1s vs ~9min: without the binary the
# anvil-dependent tests skip instantly).
test-python-no-anvil kw="" *args: dev
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{ kw }}" ]; then
        exec bash scripts/no_anvil_run.sh -k "{{ kw }}" {{ args }}
    fi
    exec bash scripts/no_anvil_run.sh {{ args }}


# The no-anvil dimension's pre-push slice: the same simulation scoped to the
# SPAWN SURFACE -- every test file mentioning AnvilFork / standalone_anvil /
# anvil_factory, discovered from the tree at invocation time. Self-maintaining:
# a new spawn site joins automatically, because spawning means mentioning one
# of those tokens. An unguarded spawn anywhere in the surface fails at
# construction; guarded tests skip. Discovery failing closed matters: an empty
# selection would make this gate vacuous.
test-python-no-anvil-spawn-surface kw="" *args: dev
    #!/usr/bin/env bash
    set -euo pipefail
    mapfile -t files < <(rg -l "AnvilFork|standalone_anvil|anvil_factory" tests/ --glob '**/test_*.py')
    if [ "${#files[@]}" -eq 0 ]; then
        echo "ERROR: spawn-surface discovery found nothing -- the pattern or the tree moved" >&2
        exit 1
    fi
    if [ -n "{{ kw }}" ]; then
        exec bash scripts/no_anvil_run.sh -k "{{ kw }}" {{ args }} "${files[@]}"
    fi
    exec bash scripts/no_anvil_run.sh {{ args }} "${files[@]}"


# Repeat-run gate for the two ordering-sensitive suites
# (tests/arbitrage/test_arbitrage_session.py, tests/operator/test_operator_channel.py):
# three consecutive runs under `-p no:cacheprovider`, failing on any failing
# run. A single green pass can hide cross-run ordering or state coupling;
# repetition is the detector.
test-flake-probe:
    #!/usr/bin/env bash
    set -euo pipefail
    for i in 1 2 3; do
        echo "▶ flake-probe run ${i}/3"
        timeout 600 {{ py_run }} pytest -q --no-header -p no:cacheprovider \
            tests/arbitrage/test_arbitrage_session.py tests/operator/test_operator_channel.py
    done
    echo "✓ flake-probe: 3/3 green"

# Re-populate golden files for on-chain-oracle parity tests. Requires a working
# fork (tests.env RPC or local node). Pass a nodeid to refresh a single test:
#   just record-golden -- tests/uniswap/v3/test_uniswap_v3_liquidity_pool.py::test_cached_calculations
# Single-process (-n0): parametrized parity tests share one golden file per
# test function and accumulate keys across params; xdist parallelism would race
# the shared file (last-writer-wins, losing keys). Replay is read-only and safe
# under xdist, but record accumulates writes.
record-golden *args:
    {{ py_run }} pytest --golden-mode record -m onchain_oracle -q --no-header -n0 {{ args }}

# Live chain-data drift gate (explicit only — never in test/pre-push/CI lanes):
# re-records every py_oracle corpus twice and requires byte-identity with disk,
# per chain, against live archive endpoints. Minutes of paced live RPC and the
# endpoints rate-limit, so this runs on demand, not in any gate battery:
#   just live-drift                # all three chains
#   just live-drift -- -k ethereum # one chain
live-drift *args:
    {{ py_run }} pytest -m live_drift -q --no-header tests/golden/test_live_drift.py {{ args }}


# Record the README examples' chain surface into the offline replay cassette
# (`tests/fixtures/chain_data/1/readme_examples.json`). Drives the real
# `Bot.build_pool` / token-utility surface through a head-pinned recording proxy
# in front of the archive node, so regeneration is byte-identical (the head is
# pinned at `scripts/record_readme_cassette.py`'s `PIN`, and every "latest" tag
# is rewritten to it). The README suite then replays offline via the
# `Bot(provider=...)` seam (conftest.py). Requires a reachable archive node.
record-readme-cassette:
    {{ py_run }} python scripts/record_readme_cassette.py


# Drift gate for the README cassette: re-record and require byte-identity with
# the committed file. Needs a reachable archive node (not a CI gate — CI replays
# the committed cassette offline).
check-readme-cassette:
    {{ py_run }} python scripts/record_readme_cassette.py --check

# Verify every shipped deployment address is actually deployed on-chain (cast).
# Tier 1 (bytecode presence) by default; escalate via the env var:
#   DEGENBOT_VERIFY_DEPLOYMENTS=2 just verify-deployments   # +selector fingerprint
#   DEGENBOT_VERIFY_DEPLOYMENTS=3 ETHERSCAN_API_KEY=... just verify-deployments  # +Etherscan source
#   DEGENBOT_VERIFY_DEPLOYMENTS=4 just verify-deployments   # +init_code_hash reproduces pool address
# Requires a reachable RPC per chain (tests.env / env vars). Deselected from the
# default `test-python` run (online_rpc marker) — run on demand only.
verify-deployments *args:
    DEGENBOT_VERIFY_DEPLOYMENTS=${DEGENBOT_VERIFY_DEPLOYMENTS:-1} {{ py_run }} pytest -m online_rpc -q --no-header -p no:randomly {{ args }} tests/registry/test_deployment_onchain_verification.py

# Re-populate the golden deployment-verification capture (tiers 1/2/4 facts for
# every factory row on a reachable chain) consumed by the hermetic replay
# tests tests/registry/test_deployment_golden_verification.py (default suite,
# fully offline). Requires a reachable RPC per chain (tests.env / env vars);
# unreachable chains contribute no rows. Tier 4 is asserted live while
# recording, so a capture is only committed when every recorded row verified.
record-deployment-golden *args:
    DEGENBOT_VERIFY_DEPLOYMENTS=4 {{ py_run }} pytest --golden-mode record -m online_rpc -q --no-header -p no:randomly -n0 {{ args }} tests/registry/test_deployment_onchain_verification.py

# ========== Tier-3 On-Chain Oracles ==========
#
# `just test-tier3 [family]` — build a family's pinned canonical-reference
# harness (real solc/forge toolchain), republish its artifacts under
# `tier3-oracle/artifacts/`, and run that family's byte-exact
# Rust-vs-real-EVM test. No family (default `all`) runs EVERY family, in the
# order listed below.
#
# Family notes (harness sources under `tier3-oracle/src*/`):
#   step      SwapMath.computeSwapStep (V3 + V4) vs the real canonical core
#             libraries run as EVM bytecode in revm. V3 via direct
#             solc 0.7.6 (v3-core pragmas <0.8 + foundry can't resolve solc
#             <0.8 in this env — documented toolchain deviation; the script
#             caches solc 0.7.6 in the svm dir) + V4 via forge 0.8.26.
#             Asserts each Rust output field === the on-chain output.
#   swap      V3 `Pool.swap` end-to-end. solc 0.7.6 harness, drives
#             `v3_simulate_swap` against real UniswapV3Pool bytecode in revm.
#   v2        V2 `Pair.swap` (family 1/3 of the Tier-3 cutover). solc 0.5.16
#             harness; `IntHopState::swap` (V2 getAmountOut) byte-exact via the
#             K-invariant boundary.
#   v4        V4 `PoolManager.swap` end-to-end. solc 0.8.26 harness
#             (PoolManager singleton + unlocker + mock tokens);
#             `v4_simulate_swap` through the unlock/settle dance, with
#             amount0/amount1 byte-exact to the on-chain BalanceDelta.
#   path5000  path-5000 V4 CL-hop clamp regression: prove the CL-hop
#             input clamp turns the 20.7M-gas EMPTY-HALT into a clean
#             byte-exact fill under the executor's 5M ceiling. Rebuilds the
#             shared v4 harness and runs the pair from the umbrella `degenbot`
#             crate.
#   curve     Curve stableswap `get_dy` (family 2/3 of the Tier-3 cutover). solc
#             0.8.26 harness — a faithful Solidity port of the STANDARD
#             stableswap `get_dy` (Curve's canonical source is Vyper, absent
#             here); the `simulate_swap` standard path is byte-exact to the
#             on-chain `getDy`.
#   balancer  Balancer weighted/stable (family 3/3 of the Tier-3 cutover). solc
#             0.7.6 harness over the CANONICAL balancer-v2-monorepo math cores
#             (FixedPoint/LogExpMath/WeightedMath/StableMath, pinned commit
#             f8b6f44); the `simulate_swap` weighted + stable
#             (invariant_version==1) paths are byte-exact.
#   pancake   PancakeSwap V3 `PancakeV3Pool.swap`. solc 0.7.6 harness over the
#             Etherscan-verified deployed source (pool 0x1445F32D1A74872bA41f3D8cF4022E9996120b31,
#             vendored under `lib/pancake-src/`); byte-exact math AND the
#             9-field `Swap` event variant decodes only via the PancakeSwap
#             decoder (not the Uniswap one).
#   pancake2  PancakeSwap V2 pair swap (the fork-fee sub-slice of the V2
#             family — the source of `tier3_v2_pair_swap_vs_revm.rs`'s
#             deferral). solc 0.5.16 harness over the REAL Ethereum-mainnet
#             `PancakePair` (hardcoded 0.25% fee = the engine's
#             `PANCAKESWAP_V2` preset, 3-tuple timestamped reserves);
#             `IntHopState::swap` byte-exact at the fork fee via the
#             K-invariant boundary.
#
# The same tests ALSO run in the default `just test-rust` (they load the
# COMMITTED bytecode from `tier3-oracle/artifacts/`, toolchain-free), so this
# recipe's unique role is regenerate + publish the artifacts (after a
# harness-source edit; `rebuild-tier3-artifacts` republishes without running
# them) and re-run the family. Recompiling dozens of revm harnesses is slow —
# run a single family, or `all`, accordingly.
test-tier3 family='all':
    #!/usr/bin/env bash
    set -euo pipefail

    run_family() {
        local harness pkg test
        case "$1" in
            step)     harness=build-tier3-harnesses.sh;           pkg=degenbot-math; test=tier3_compute_swap_step_vs_revm ;;
            swap)     harness=build-tier3-v3-swap-harness.sh;     pkg=degenbot-simulation; test=tier3_v3_pool_swap_vs_revm ;;
            v2)       harness=build-tier3-v2-swap-harness.sh;     pkg=degenbot-pools; test=tier3_v2_pair_swap_vs_revm ;;
            v4)       harness=build-tier3-v4-swap-harness.sh;     pkg=degenbot-simulation; test=tier3_v4_pool_swap_vs_revm ;;
            path5000) harness=build-tier3-v4-swap-harness.sh;     pkg=degenbot; test=tier3_path5000_v4_clamp ;;
            curve)    harness=build-tier3-curve-swap-harness.sh;  pkg=degenbot-pools; test=tier3_curve_swap_vs_revm ;;
            balancer) harness=build-tier3-balancer-swap-harness.sh; pkg=degenbot-pools; test=tier3_balancer_swap_vs_revm ;;
            pancake)  harness=build-tier3-pancake-v3-swap-harness.sh; pkg=degenbot-simulation; test=tier3_pancake_v3_swap_vs_revm ;;
            pancake2) harness=build-tier3-pancake-v2-swap-harness.sh; pkg=degenbot-pools; test=tier3_pancake_v2_swap_vs_revm ;;
            *) echo "unknown tier-3 family '$1' (families: step swap v2 v4 path5000 curve balancer pancake pancake2 | all)" >&2; exit 2 ;;
        esac
        tier3-oracle/"$harness"
        python_libdir="$({{ py_run }} python -c 'import sysconfig; print(sysconfig.get_config_var("LIBDIR"))')"
        export LD_LIBRARY_PATH="${python_libdir}${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
        cargo test --locked --manifest-path rust/Cargo.toml -p "$pkg" --test "$test"
    }

    if [ "{{ family }}" = "all" ]; then
        for f in step swap v2 v4 path5000 curve balancer pancake pancake2; do
            run_family "$f"
        done
    else
        run_family "{{ family }}"
    fi

# Validate the committed tier-3 harness bytecode: recompile EVERY harness with
# the real solc/forge toolchain (into a throwaway dir, PUBLISH=0 — committed
# artifacts are never mutated) and byte-compare the creation bytecode against
# what the Rust tests load from `tier3-oracle/artifacts/`. This is the
# authoritative compile-vs-use check (covers a harness-source OR pinned
# vendored-lib edit without a rebuild); the toolchain-free complement
# `tier3_harness_artifacts.rs` runs in the default suite. Requires the
# toolchain (bootstrap-libs + svm solc). Wired into the CI `tier3-oracle` job.
verify-tier3-artifacts:
    tier3-oracle/verify-tier3-artifacts.sh

# Validate the committed Vyper executor artifact (tier-3b): recompile
# executor/contracts/cmd_executor.vy with the pinned vyper 0.5.0a3 (into a
# throwaway dir, PUBLISH=0) and byte-compare against what the Rust tier-3b tests
# load from `tier3-oracle/artifacts/executor/`. This is the authoritative
# compile-vs-use check for the vyper artifact; the toolchain-free complement
# `tier3_executor_artifacts.rs` runs in the default suite. Requires the
# toolchain: the in-repo `executor/` uv project (vyper ==0.5.0a3). Wired into
# the CI `tier3-oracle` job (not the default cargo-test path).
verify-tier3-executor-artifact:
    tier3-oracle/verify-tier3-executor-artifact.sh

# Rebuild + publish every Tier-3 harness artifact (bytecode + source-hash
# manifest) from the current sources, without running the test suites. Run this
# after editing a `tier3-oracle/src*/**/*.sol` harness (or bumping a pinned
# vendored lib), then commit the updated `tier3-oracle/artifacts/`.
rebuild-tier3-artifacts:
    #!/usr/bin/env bash
    set -euo pipefail
    tier3-oracle/build-tier3-harnesses.sh
    tier3-oracle/build-tier3-v2-swap-harness.sh
    tier3-oracle/build-tier3-v3-swap-harness.sh
    tier3-oracle/build-tier3-v4-swap-harness.sh
    tier3-oracle/build-tier3-curve-swap-harness.sh
    tier3-oracle/build-tier3-balancer-swap-harness.sh
    tier3-oracle/build-tier3-pancake-v3-swap-harness.sh
    tier3-oracle/build-tier3-pancake-v2-swap-harness.sh

# ========== Code Coverage ==========
#
# `scripts/coverage.sh` runs three arms over LLVM instrumentation, all writing
# HTML (for humans) + `coverage.json` (llvm-cov export format, agents) under
# `rust/target/coverage/`:
#
#   just coverage-rust        # cargo workspace suite (cargo-llvm-cov)
#   just coverage-pyo3        # pytest driving an instrumented degenbot._ffi cdylib
#   just coverage             # both, merged into one report + coverage.lcov
#
# The merged report is also the highest-fidelity LCOV available (cargo suite
# ∪ pytest-driven binding coverage).
#
# Slow (full instrumented rebuilds, ~10-20 min cold) — on-demand, not wired
# into pre-push/CI. Scoped runs keep the rebuild narrow:
#   COV_PACKAGES="-p degenbot-config" just coverage-rust
# Args pass to the pytest arm: just coverage-pyo3 tests/abi -q

coverage-rust *args:
    scripts/coverage.sh rust {{ args }}

coverage-pyo3 *args:
    scripts/coverage.sh pyo3 {{ args }}

coverage *args:
    scripts/coverage.sh all {{ args }}

# ========== Code Quality ==========

# Lint Markdown files
lint-markdown:
    npx --yes markdownlint-cli2 --fix "**/*.md" "!**/node_modules/**" "!**/.venv/**" "!tier3-oracle/lib/**" "!logs/**" "!.scratch/**"

# Lint Python files
lint-python:
    uvx ruff check --fix src/ examples/
    # ty must run under the project environment (it resolves third-party imports
    # and the degenbot stubs from the venv), so `uvx ty` cannot work; `--with ty`
    # gives the latest ty without pinning it into the venv.
    {{ py_run }} --with ty ty check --fix --no-progress src/

# Lint Python (check-only; non-mutating). Mirrors the ruff+ty gate CI runs,
# minus `--fix`, so a pre-commit run cannot dirty staged files. Stricter than
# CI's `lint-python`: fails on any issue `--fix` would have auto-applied.
lint-comment-hygiene:
    scripts/hooks/comment-hygiene.sh


# ADR-087 D4 — the retirement-tripwire ratchet. Scans the test/gate surface (rust
# test files, tests/, justfile, prek.toml, scripts/, docs/) for the signatures of a
# tripwire: an assertion that a deleted name is still absent, a grep for a retired
# identifier, a collection named for the revival asserted empty, or a prose "do not
# revive / re-land / rebuild" ban. Deleting code is the complete retirement (D1), so
# one hit fails the gate.
#
# It flags and does not classify: delete the flagged artifact, or label it with the
# D2 marker naming the PRESENT code it watches. The marker is the only exemption —
# there is no waiver file, and adding one would recreate the garrison.
#
# The gate body is scripts/hooks/tripwire_ratchet.py; its negative probe (a synthetic
# tripwire fixture, observed failing) lives in tests/test_tripwire_ratchet.py, which
# runs in the default pytest selection.
#
# live-invariant gate (ADR-087 D2): watches the present test/gate surface for the tripwire signatures ADR-087 D1 bans; not tripwire precedent.
lint-tripwire-ratchet:
    {{ py_run }} python scripts/hooks/tripwire_ratchet.py

lint-python-check:
    uvx ruff check src/ examples/
    {{ py_run }} --with ty ty check --no-progress src/

# Dead-code detector (off the gate — output is a triage list). Each hit
# needs an `rg` call before deletion: vulture is static and can't see
# FFI-seam callers (Rust core) or framework dispatch (pydantic validators,
# SQLAlchemy TypeDecorator signatures, `__exit__`/Protocol params). 80%
# confidence is the operating point; the 60% tier is mostly framework-
# dispatched methods (validators, properties on models, enum members).
# Complements ruff: ruff's F401 rule exempts `if TYPE_CHECKING:` imports
# and there is no ruff unreachable-code rule, so vulture catches both.
dead-code:
    uvx vulture src/degenbot vulture_whitelist.py --min-confidence 80

# Deeper dead-code sweep — catches unused functions/methods/classes too
# (the 80% tier only catches unused variables, imports, unreachable code).
# Output is much noisier (~hundreds of findings, mostly framework-dispatched
# pydantic validators, @property on models, enum members). Use periodically
# for intentional dead-code audits; not a routine gate. Generate whitelist
# candidates with: vulture src/degenbot --min-confidence 60 --make-whitelist
dead-code-deep:
    uvx vulture src/degenbot --min-confidence 60 --make-whitelist

# Check Python formatting (read-only; fails on drift). Run `just format` to fix.
fmt-check-python:
    uvx ruff format --check src/

# Lint commit messages across a range (default: everything not yet pushed).
# Examples: just lint-commits              # @{push}..HEAD
#           just lint-commits HEAD~5..HEAD # explicit range
# just lint-commits main..HEAD   # branch commits
lint-commits range="@{push}..HEAD":
    #!/usr/bin/env bash
    set -euo pipefail
    range="{{ range }}"
    if [[ "$range" == *".."* ]]; then
      from="${range%%..*}"
      to="${range##*..}"
      [ -z "$to" ] && to=HEAD
    else
      from="$range"
      to=HEAD
    fi
    npx --yes @commitlint/cli --from "$from" --to "$to"

# Run all linters (Rust + Python + Markdown)
lint: fmt-check fmt-check-python lint-rust lint-python lint-markdown

# Format all code
format: check-toolchain
    uvx ruff format src/
    cargo fmt --manifest-path rust/Cargo.toml --all

# ========== Dependency Updates ==========

# Upgrade Python and Rust dependencies (incl. semver-major bumps) — the
# repo-local replacement for dependabot's pip + cargo ecosystems.
#
# Python — two passes, mirroring Cargo's split of requirements from lockfile:
#   1. `scripts/bump_python_deps.py` rewrites the version *requirements* in
#      pyproject.toml (main deps + pinned dependency-group entries) to the
#      latest stable on PyPI, across semver major boundaries (e.g.
#      `pydantic ~= 2.13` -> `~= 2.14`) — the `cargo upgrade` analog. Plain
#      `uv sync --upgrade`/`uv lock --upgrade` cannot do this: re-resolving
#      only advances within the existing ranges. Unpinned dev-group entries
#      are already open-ended, and the script honors the [tool.uv]
#      `exclude-newer` horizon so the new pins remain resolvable by uv.
#   2. `uv lock --upgrade` re-resolves uv.lock inside the new ranges — direct
#      + transitive, all groups — the `cargo update` analog; `uv sync` then
#      refreshes the venv.
#
# Rust — two passes, because Cargo splits the job:
#   1. `cargo upgrade --incompatible` rewrites the version *requirements* in
#      every member Cargo.toml to the latest published, including across
#      semver major boundaries (e.g. revm 41 -> 42). `cargo update` alone
#      cannot do this — it only refreshes Cargo.lock within the existing
#      `^x.y.z` range, so a major release on crates.io is invisible to it.
#   2. `cargo update` then refreshes Cargo.lock to satisfy the new
#      requirements. `cargo upgrade` already rewrites the lock too, but the
#      explicit pass also pulls compatible patch bumps it left at the floor.
#
# Requires the `cargo-edit` subcommand (`cargo upgrade`); install with
#   `cargo install --locked cargo-edit`
update-deps:
    #!/usr/bin/env bash
    set -euo pipefail
    {{ py_run }} python scripts/bump_python_deps.py
    uv lock --upgrade
    # --no-install-project: skip uv's redundant project build (a plain `uv sync`
    # now builds the same dev-features extension); `just dev` rebuilds it
    # explicitly.
    uv sync --no-install-project
    just dev
    cargo upgrade --manifest-path rust/Cargo.toml --incompatible
    cargo update --manifest-path rust/Cargo.toml

# ========== Repository Setup ==========

# Refresh the committed DEGENBOT_* key-inventory snapshot the
# degenbot-config schema-completeness test compares against. The exclusion glob keeps the snapshot self-contamination
# (regeneration rewriting its own source) out of the sweep.
env-inventory:
    #!/usr/bin/env bash
    set -euo pipefail
    target="rust/crates/foundation/degenbot-config/tests/degenbot_env_inventory.txt"
    rg -o 'DEGENBOT_[A-Z_]+' rust/crates --no-filename \
        -g '!**/degenbot_env_inventory.txt' \
        -g '!**/degenbot-config/tests/**' | sort -u > "$target"
    echo "✓ inventory snapshot refreshed: $target ($(wc -l < "$target") keys)"
    echo '  then: cd rust && REGEN_CONFIG_DOCS=1 cargo test -p degenbot-config'

# Install prek git hooks and configure template.
# Run this once after cloning. Commit MESSAGE lint runs at commit time (low
# friction, since .commitlintrc.yml is relaxed: free-form scope, 100-col) so a
# bad message is caught the moment it is written — not at push, when amending
# days-old commits needs a deep rebase. Code linters (clippy/ty) run at pre-push
# + CI: a code-lint failure is fixable with a follow-up commit. Hooks are
# declared in prek.toml. For manual message range checks: just lint-commits.
setup-git-hooks:
    #!/usr/bin/env bash
    set -euo pipefail
    # prek installs into git's effective hooks dir (default .git/hooks). Clear
    # any stale custom hooksPath from the old .githooks setup so it isn't used.
    git config --unset core.hooksPath 2>/dev/null || true
    git config commit.template .commit-template
    # prek is installed as a global uv tool (~/.local/bin/prek) so the hooks
    # it generates do NOT pin a throwaway venv path — they fall back to `prek`
    # on PATH, which survives venv recreations and is reproducible across
    # host/container installs. Install on demand if missing (host first run).
    command -v prek >/dev/null 2>&1 || uv tool install prek
    prek install
    echo "✓ prek hooks installed:"
    echo "    pre-commit : Markdown lint + PLC0415 noqa guard (staged files)"
    echo "                + instant checks (Rust/Python fmt, Rust no-pyo3)"
    echo "    commit-msg : commitlint against .commitlintrc.yml (relaxed rules)"
    echo "    pre-push   : object-DB GC + commitlint push-range + Rust/Python lint"
    echo "                 (clippy/ty) + build & test suite (rust build/test,"
    echo "                 python build/test)"
    echo "    Bypass: git push --no-verify (CI still runs)."
    echo "✓ commit template configured."


# ========== Settlement-Bot Parity Gate ==========
#
# The executable successor to the parity ledger
# (docs/architecture/rust-settlement-bot-parity.md): the fixture boot gate
# (Rust integration test + Python PyO3 probe against the shared oracle
# tests/standalone_parity/fixtures/settlement_bot_boot.json) plus the
# recorded dual-driver decision diff. CI-safe and offline (no RPC); the live
# anvil arm is opt-in via DEGENBOT_DUAL_DRIVER_GATE=1 + DEGENBOT_FORK_RPC
# (tests/standalone_parity/dual_driver_gate.py --live).
test-settlement-parity:
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot-settlement-bot-example --test boot_gate
    {{ py_run }} pytest tests/standalone_parity/test_settlement_bot_boot_gate.py tests/standalone_parity/test_settlement_bot_dual_driver_gate.py -q
    {{ py_run }} python tests/standalone_parity/dual_driver_gate.py --recorded

# ========== No-Python Console Gate (ADR-051 D10) ==========
#
# The Rust-owned console (`degenbot`) must build and run with NO Python in the
# path. This recipe builds the `degenbot-cli` binary, then runs the fixed argv
# smoke set (--help / --version / `database inspect` over committed
# Alembic-stamped fixtures copied to a temp dir / `exchange activate`+`deactivate`
# idempotence on a fresh DB copy) and diffs the machine-checkable stdout against
# the checked-in oracle
# (.github/workflows/cli-no-python-expected.txt). Bash + sed + diff only — no
# Python, no uv. Mirrors the `cli-no-python` CI job.
#
# Seeded-divergence proof (the comparator must be able to fail), selectable by
# DEGENBOT_CLI_GATE_SEED (both must exit 0, i.e. the injected divergence WAS
# caught):
#   DEGENBOT_CLI_GATE_SEED=expected just ci-no-python-cli-gate
#       mutate one expected line in a temp oracle copy; the diff must trip.
#   DEGENBOT_CLI_GATE_SEED=live just ci-no-python-cli-gate
#       point `database inspect` at a Rust-owned DB copy (real binary, real
#       different output); the diff must trip.
# Authoring aid only: DEGENBOT_CLI_GATE_DUMP_ACTUAL=1 prints the normalized
# capture so the oracle can be regenerated deliberately.
ci-no-python-cli-gate:
    cargo build --locked -p degenbot-cli --manifest-path rust/Cargo.toml
    bash .github/workflows/cli-no-python-gate.sh

# Cancelling-async-rust: a `timeout(..)` deadline belongs on the OPERATION
# boundary, never on the future that carries the value. RFD 400's select-loops
# case study is the canonical trap —
# `timeout(Duration, tx.send(value)).await` reads as a deadline on a send but
# is a deadline on a FUTURE, so a fired timeout drops it mid-await and the moved
# value is lost with no way to tell cancellation from delivery. The same shape
# over `AsyncWriteExt::write_all` silently swallows partial progress: bytes the
# peer accepted vanish from the caller's view, so a retry can duplicate a
# partial frame. This gate flags a production `timeout(` whose second argument
# mentions `.send(` or `.write_all(`. Mirrors check-no-pyo3-in-cores: a
# permanent, mechanical sweep gate (the one legitimate production site is named
# in the gate's allowlist with its reason, not excused by a looser pattern).
#
# live-invariant gate (ADR-087 D2): watches live production `timeout(..)` calls that wrap a data-carrying send or a non-resumable write; not tripwire precedent.
check-cancel-safety:
    # C7: the gate body lives as a cargo test on the umbrella crate
    # (rust/crates/facade/degenbot/tests/architecture_gates.rs).
    cargo test --locked --manifest-path rust/Cargo.toml -p degenbot --test architecture_gates -- no_timeout_wrapped_value_sends_or_write_all --exact --nocapture

# ========== Stub Generation + Verification (ADR-066) ==========
#
# `just gen-stubs` regenerates the committed `src/degenbot/_ffi/` stub tree
# from the seam's built cdylib (ADR-066 D1/D2): it (1) builds `degenbot_rs`
# with the named `experimental-inspect` feature — a TEMPORARY toggle per
# ADR-066 D1; release wheels never select it, and this build exists solely to
# be introspected — (2) runs the `degenbot-stubgen` generator over that
# cdylib, and (3) replaces the committed stub tree with the output.
#
# Regeneration is deterministic: the same cdylib produces a byte-identical
# tree, so the second of two consecutive runs must be diff-empty (the drift
# gate below asserts this continuously). The introspected build lands in its
# own target dir (`<target_dir>/stubgen-inspect`), so it never churns the
# canonical cache: the installed development extension and `just dev`'s build
# plan are untouched, and no follow-up `just dev` is needed.
#
# `just gen-stubs --check` (used by tests/ffi/test_ffi_stub_drift.py)
# generates into a temp dir and FAILS on any diff against the committed tree
# without writing; `REGEN_STUBS=1` opts the check into writing, mirroring
# `REGEN_CONFIG_DOCS=1 cargo test -p degenbot-config` (the REGEN pattern of
# docs/rust-config-keys.md). No hand edits over generated output, ever:
# generator gaps live only in tests/ffi/stubtest_allowlist.txt (ADR-066 D3).
#
# live-invariant gate (ADR-087 D2): watches the committed `src/degenbot/_ffi/` stub tree against this generator's output; not tripwire precedent.
gen-stubs check="":
    #!/usr/bin/env bash
    set -euo pipefail
    # just does not pass recipe arguments as positional script arguments
    # (recipe argv is forwarded as `"$@"`), but the flag is read from `{{ check }}` so
    # its one-token contract is explicit.
    check=0
    case "{{ check }}" in
        "") ;;
        --check) check=1 ;;
        *) echo "usage: just gen-stubs [--check]" >&2; exit 2 ;;
    esac
    # The generator parses introspection chunks statically (object-file
    # symbols), so the introspected build is never loaded or installed. Build it
    # into its own target dir: `experimental-inspect` is a distinct feature set
    # from `dev-features`, so sharing the canonical dir would churn its
    # fingerprints. The first run pays a from-scratch workspace compile; later
    # runs are incremental.
    inspect_dir="{{ target_dir }}/stubgen-inspect"
    CARGO_TARGET_DIR="$inspect_dir" cargo build --locked --manifest-path rust/Cargo.toml -p degenbot_rs --features experimental-inspect
    CARGO_TARGET_DIR="$inspect_dir" cargo build --locked --manifest-path rust/Cargo.toml -p degenbot-stubgen
    out="$(mktemp -d)"
    trap 'rm -rf "$out"' EXIT
    "$inspect_dir/debug/degenbot-stubgen" \
        "$inspect_dir/debug/libdegenbot_rs.so" _ffi "$out"
    # Pipeline steps: ruff-format then ruff-check-fix the generated tree
    # BEFORE the diff/copy branch. The generator's raw signatures (long
    # single-line defs, `X |None`, unsorted imports) are not format- or
    # lint-clean, and `just fmt-check-python` / `just lint-python-check` fail
    # on them. Because the steps sit above the branch, `gen-stubs --check`
    # also applies them — the drift gate's contract (regeneration reproduces
    # the committed tree byte-for-byte) holds only if the committed tree is
    # pipeline output inclusive of formatting and lint fixing. `--config`
    # pins the repo's pyproject: the temp out dir is outside the repo, so
    # ruff's upward config discovery would otherwise fall back to defaults
    # and the tree would not be reproducible. The generated-stub rules the
    # check pass cannot fix (Rust doc-comment prose) are exempted under the
    # bare `"*.pyi"` per-file-ignores key, which matches the temp dir too.
    uvx ruff format --config pyproject.toml "$out"
    uvx ruff check --fix --config pyproject.toml "$out"
    if [ "$check" = 1 ] && [ -z "${REGEN_STUBS:-}" ]; then
        if diff -ru src/degenbot/_ffi "$out"; then
            echo "✓ committed stubs match generator output"
        else
            echo "ERROR: src/degenbot/_ffi has drifted from generator output; run \`just gen-stubs\`" >&2
            exit 1
        fi
    else
        rm -rf src/degenbot/_ffi
        cp -R "$out" src/degenbot/_ffi
        echo "✓ committed stubs regenerated (src/degenbot/_ffi)"
    fi

# ========== Stub-to-Runtime Drift Gate (ADR-053 stubs, ADR-066 D3 cutover) ==========
#
# mypy.stubtest is now GENERATOR VERIFICATION (ADR-066 D3): it introspects the
# INSTALLED degenbot._ffi extension and compares it against the GENERATED
# src/degenbot/_ffi/*.pyi stubs in BOTH directions, across every class member,
# with no curated table. Its allowlist (tests/ffi/stubtest_allowlist.txt)
# carries only generator gaps — the enumerated PyO3-introspection limitations,
# never hand-edits over generated output. The regenerate-and-diff gate against
# the generator is tests/ffi/test_ffi_stub_drift.py (via `just gen-stubs
# --check`). What stubtest cannot see (Python-side surface in driver modules)
# stays in tests/ffi/test_ffi_registration_surface.py.
#
# mypy.stubtest replaces the bespoke drift gate's R1/R3/R4 mechanics (and R2,
# verified below): it introspects the INSTALLED degenbot._ffi extension and
# compares it against the generated src/degenbot/_ffi/*.pyi stubs in
# BOTH directions, across every class member, with no curated table. The
# allowlist (tests/ffi/stubtest_allowlist.txt) carries only PyO3-
# introspection noise and stub-only type exemptions, each group annotated with
# the drift rule it serves. What stubtest cannot see (Python-side surface in
# driver modules) stays in tests/ffi/test_ffi_registration_surface.py.
lint-stubtest:
    #!/usr/bin/env bash
    set -euo pipefail
    # --ignore-disjoint-bases: PEP 800 disjoint-base flags on every #[pyclass]
    # are PyO3-synthetic runtime semantics, not stub drift. Staleness of the
    # introspected .so is owned by the build step (the test tracks depend on
    # `dev`) — stubtest simply checks whatever is installed.
    {{ py_run }} stubtest --ignore-disjoint-bases \
        --allowlist tests/ffi/stubtest_allowlist.txt degenbot._ffi

# Dedicated regenerate-and-diff stub gate (ADR-066 D2/D3): the compile-coupled
# face of the stub-drift ratchet. `just gen-stubs --check` cargo-builds
# degenbot_rs with the temporary `experimental-inspect` feature + degenbot-stubgen
# before introspecting the cdylib, so it is NOT in the default pytest selection
# (the `stub_drift` marker excludes it) — it runs once as the CI `stub-drift`
# job, and here for an on-demand local run. Run it with exclusive use of the
# build graph: a concurrent `just dev` / `cargo build` holds the debug lock and
# can relink the introspected `.so`.
lint-stub-drift:
    just gen-stubs --check

# The solve-entry A/B readout (the unification harness): per-path median/p95
# for the all-CL intake vs the direct entry over a capture corpus, parity-gated
# (exits 1 on divergence). DR_AB_ITERS / DR_AB_MEMO shape the run.
entry-ab corpus="heavy_concentrated_liquidity_solve_captures.jsonl":
    cargo run --locked --manifest-path rust/Cargo.toml -p degenbot-pools --features capture-fixtures --example entry_ab_readout -- rust/crates/foundation/degenbot-pools/tests/fixtures/{{corpus}}


