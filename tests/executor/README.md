# Executor contract tests

Ape + Foundry tests for the Vyper sources in `contracts/vyper/`, using the root
uv environment and the repository's archive-node configuration.

## Run

```bash
just test-executor                                               # full suite
just test-executor tests/executor/test_cmd_executor_compact.py    # one file
just test-executor tests/executor/ -k 'sentinel'                  # name filter
just test-executor tests/executor/test_initialize_and_opcodes.py -n0
just test-executor-offline                                  # standalone Anvil, no archive
```

`just test-executor` resolves `TEST_ETHEREUM_FULL_NODE_HTTP_URI` through the
same declared layers as the other root recipes and fails closed when no archive
endpoint answers. `ape-config.yaml` supplies the upstream to Anvil;
`ape-pytest.ini` owns the suite's pytest and warning policy.

Use `-n0` for a single-process run. Under xdist, `conftest.py` assigns each
worker a separate Foundry host so accounts, nonces, and fork state do not race.

## Layout

- `rust/crates/foundation/degenbot-executor/releases/v2/contract.vy` — command-stream executor under active
  development.
- `contracts/vyper/fake_*.vy` — ERC-20, WETH, V2 pair, V3 pool, and V4
  PoolManager test doubles.
- `contracts/vyper/sol_reference/` — Solidity V3 math reference sources and the
  committed artifact used by the cross-implementation fuzz tests.
- `tests/executor/conftest_shared.py` — command encoding, pool setup, revert
  identity, and shared fixtures.
- `tests/executor/cmd_stream.py` — command-stream parsing and transfer-count
  assertions.

## Three-hop coverage

Three execution layers deliberately answer different questions:

- `test_cmd_executor_three_hop_optimized.py` proves all 27 contract-mechanics
  orderings with the test encoder.
- `test_cmd_executor_three_hop_production_calldata.py` encodes every ordering
  through `degenbot_executor::composers::encode_cmd_3_hop` and executes those
  exact production bytes, checking transfer counts and token conservation.
  Its pure-Rust helper is built only through
  `just build-executor-three-hop-encoder`.
- `degenbot-simulation/tests/harness_declarative.rs` executes all 27 through
  EVM2 with the real executor bytecode and exact predicted-profit deltas. Its
  `full_three_hop_matrix_executes_production_solver_amounts` test also solves
  every row through the production mixed-path solver, then executes the
  solver-selected input, output, and consumed-input vectors.

Run both Ape matrices serially with:

```bash
just test-executor tests/executor/test_cmd_executor_three_hop_optimized.py -q -n0
just test-executor tests/executor/test_cmd_executor_three_hop_production_calldata.py -q -n0
```

Run the EVM2 matrix and its completeness guard with:

```bash
just test-rust-nextest -p degenbot-simulation -E 'test(full_matrix_executes_with_expected_profit) or test(matrix_covers_full_reachable_grammar)'
```

Run the production-solver EVM2 matrix with:

```bash
just test-rust-nextest -p degenbot-simulation -E 'test(full_three_hop_matrix_executes_production_solver_amounts)'
```

The Rust `command_stream_byte_identity_is_pinned` test separately pins every
permutation under 12 amount/option configurations.

### Fixed-path bot probe

`scripts/executor_three_hop_matrix.json` names a mainnet WETH/USDC/WBTC
triangle with V2, V3, and V4 pools on every edge. The companion Python tool
injects both token-cycle directions (54 directed paths) and aggregates their
exact engine path IDs into 27 protocol rows.

Start an operator-only, zero-floor, injected-executor dry run (set the owner,
injected address, and runtime artifact for the release under test):

```bash
export DEGENBOT_INJECT_EXECUTOR_CODE=1
export EXECUTOR_OWNER_ADDRESS=0x...
export INJECTED_EXECUTOR_ADDRESS=0x...
export EXECUTOR_RUNTIME=/path/to/cmd_executor_runtime.txt
export DEGENBOT_MIN_PROFIT_WEI=0
export DEGENBOT_MIN_PROFIT_MARGIN_BPS=0
./run_bot.sh foreground -- --skip-discovery --operator-socket /tmp/degenbot-27.sock
```

From another shell, inject and then render the fail-closed report:

```bash
just py python scripts/executor_three_hop_matrix.py inject \
  --socket /tmp/degenbot-27.sock \
  --output logs/executor_solver_sim_27_registrations.json
just py python scripts/executor_three_hop_matrix.py report \
  --log logs/bot_run.log \
  --registrations logs/executor_solver_sim_27_registrations.json \
  --output logs/executor_solver_sim_27_report.json
```

Registration is not simulation coverage: live pools may have no profitable
input in either direction. `report` marks those rows `missing` and exits
nonzero. Valid receipts emit `[sim-ok]`; failures and exceptions remain
separate. Any `Submitted path` record fails the report. Never add `--live`:
dry-run skips signing and executor-code injection independently gates
submission.

The fakes enforce the behavior the executor depends on: V2's K invariant,
V3's balance-delta input check and reentrancy guard, V4's delta settlement,
ERC-6909 accounting, and callback variants. They isolate executor routing and
settlement logic from real pool pricing. They do not replace mainnet-fork or
Tier-3 execution tests.

## Rebuild reference artifacts

After editing the Solidity reference sources, run
`contracts/scripts/build-sol-reference-artifact.sh` and commit the regenerated
artifact. The routine Ape compile excludes `sol_reference/`; the fuzz tests
consume its committed solc 0.7.6 artifact directly.

The production Vyper artifact has a separate authoritative compile-vs-use gate:

```bash
just verify-tier3-executor-artifact
```
