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

- `contracts/vyper/cmd_executor.vy` — command-stream executor under active
  development.
- `contracts/vyper/fake_*.vy` — ERC-20, WETH, V2 pair, V3 pool, and V4
  PoolManager test doubles.
- `contracts/vyper/sol_reference/` — Solidity V3 math reference sources and the
  committed artifact used by the cross-implementation fuzz tests.
- `tests/executor/conftest_shared.py` — command encoding, pool setup, revert
  identity, and shared fixtures.
- `tests/executor/cmd_stream.py` — command-stream parsing and transfer-count
  assertions.

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
