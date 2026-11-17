# Foundry lane for the Vyper executor contracts

A Foundry port of the Ape suite in `tests/executor/`. It exists to answer two
questions with running code rather than an opinion: what does this suite look
like outside Ape, and what does Foundry make possible that the Ape lane does not.

Run it through the justfile (never `forge test` directly — the artifacts and the
generated interfaces are build outputs):

```bash
just test-foundry-executor                                   # whole lane
just test-foundry-executor --match-path 'test/V2V3.t.sol'    # one file
just test-foundry-executor --match-test testV2V3 -vvvv       # one test, traced
just foundry-executor-artifacts                              # artifacts + interfaces only
just py python tests/foundry_executor/build_artifacts.py --check-release  # compile identity
```

`just test-foundry-executor` runs the same build steps with `--check-release` on
the Vyper compile: the lane is red if the executor bytecode it deploys is not the
tracked release bytecode (`rust/crates/foundation/degenbot-executor/releases/v2`).

## How compilation works (and why it is not `forge build` on the Vyper tree)

Forge's Vyper import resolver understands `from <dir> import <module>.vy` and
stops there. This tree imports *names* out of `.vyi` interface files
(`from .interfaces.UniswapV4 import IPoolManager`), which the resolver cannot
resolve — the whole workspace root (including `// SPDX`-style relative imports
from outside the project root) fails to build. Verified failure modes:

| Import form | forge's Vyper resolver |
| --- | --- |
| `from ethereum.ercs import IERC20` | resolves (stdlib is compiled in) |
| `from .sub import Mod` where `sub/Mod.vy` | resolves |
| `from .sub import IThing` where `sub/Thing.vyi` | **fails** |
| `from .sub.Thing import x` | **fails** |
| `from .interfaces.UniswapV4 import IPoolManager` (`UniswapV4/IPoolManager.vyi`) | **fails** |

So the lane compiles the sources with `vyper` itself — a direct repository
dependency (`vyper == 0.5.0b2` in `pyproject.toml`), imported by
`build_artifacts.py` and called in-process rather than resolved through PATH or
spawned as a subprocess:

```python
vyper.compile_code(path.read_text(encoding="utf-8"), contract_path=path,
                   output_formats=["abi", "bytecode", "bytecode_runtime"])
```

`contract_path` is what performs the resolution forge cannot: vyper anchors the
tree's `from .interfaces.X import I` imports on the source file's own
directory. `build_artifacts.py` splits that output into
`artifacts/<Contract>.json` and caches on a digest of the whole
`contracts/vyper` `.vy`/`.vyi` tree (the top-level sources are not the whole
input — they import `.vyi` interfaces transitively); on a cache hit every
generated artifact is byte-verified against the hash recorded at generation,
so corruption anywhere in an artifact — ABI included — forces a rebuild;
`gen_interfaces.py` turns the ABIs into `src/generated/I*.sol`. Two consequences, both good: the
taken bytecode is the *same* pinned-vyper output the release artifacts carry
(verified byte-for-byte by `--check-release`), and an ABI change becomes a
compile error in this lane instead of a stale hand-written declaration.

### Canonical Uniswap bytecode (pre-0.8 solc, one-off)

The same "does the toolchain need to be in the loop?" logic applies to Solidity:
`v2-core` is `=0.5.16` and `v3-core` is `=0.7.6`, and **forge's solc manager
cannot resolve pre-0.8 versions in this environment** — the repository's own
harness scripts already say so and fetch binaries directly. Nothing is lost:
`build_solidity_artifacts.py` fetches the pinned binaries into the same svm
cache the harness scripts use and drives them through `py-solc-x`'s
standard-JSON interface, emitting the same artifact shape this lane deploys.
Its stamp records a per-artifact hash next to the source digest, so a stale or
corrupted artifact (or an old/malformed stamp) reads as cold and forces one
deterministic recompile. `test/RealLibraries.t.sol`
then runs the executor against canonical `UniswapV2Pair` (behind the real
`UniswapV2Factory`) and a canonical `UniswapV3Pool` (created by the real
`UniswapV3Factory`, with real tick/position liquidity and the executor's V3
auto-pay path).

## Layout

- `build_artifacts.py` — pinned vyper in-process (`vyper.compile_code`) → `artifacts/<Contract>.json` (+ manifest with input-tree, source, bytecode, and whole-artifact hashes), plus the generated revert vocabulary `src/generated/Errors.sol`: custom-error selectors computed from the built `CmdExecutor` ABI, with the compiler revert-label vocabulary taken from the tracked release `cmd_executor.error_map.json` and cross-checked against the fresh compile on every rebuild (an unknown label or a vocabulary divergence fails the build loudly).
- `build_solidity_artifacts.py` — pinned solc 0.5.16 / 0.7.6 binaries, driven through `py-solc-x` → the same artifact shape for canonical `UniswapV2Factory`/`UniswapV2Pair`/`UniswapV3Factory`/`UniswapV3Pool` and the committed solc-0.7.6 `SolReference` (the original Solidity V3-math libraries, byte-identical to the committed Ape-lane artifact modulo its metadata hash, for the differential library ports).
- `gen_interfaces.py` — ABI → `src/generated/I<Contract>.sol` + tuple structs in `Types.sol`.
- `src/Vm.sol` — minimal cheatcode interface (no forge-std dependency), carrying the declarations the planned FFI (`vm.ffi`), RPC/fork (`vm.rpc`, `createFork`/`selectFork`), JSON (`parseJson*`), and hex (`toString`) lanes need — and nothing speculative beyond those consumers.
- `src/TestBase.sol` — artifact deployment, `execute` helpers, assertions, log/transfer accounting, balance snapshots with conservation diffs (`_snapshotBalances` + `_assert*Conserved`/`_assertExecutorProfit`), exact revert assertions over the generated `ExecutorErrors` vocabulary, and the `ExecutorFixture` that mirrors `tests/executor/conftest.py`.
- `src/ConservationOps.sol` — the composed tracked-account conservation check (the port of `test_conservation.py`'s `_verify_conservation`): tokens[0] = WETH combined with NATIVE in the mode-1 profit domain, every other tracked token summed to zero, optional executor-profit assertion run first.
- `src/Cmd.sol` — the command-stream encoder: the port of `conftest_shared.py`'s `enc_*` helpers and the `AddressTable` sentinel resolution (including the raw `0x30` V3 form and `V4_BATCH_OPEN_WETH`).
- `src/Setup.sol` — pool setup: the port of `_setup_v2_pair`, `_setup_v3`, `_setup_v4_swap`, `v2_get_amount_out`.
- `test/*.t.sol` — ported test files (each names the Python file it ports), plus `test/HarnessPrimitives.t.sol`, the named tests for the shared substrate primitives themselves (encoding bytes/lengths, the generated error vocabulary, conservation diffs, exact-revert negative controls, and the artifact additions).

## Ported coverage

| Python file | Solidity file | Python tests | Ported tests |
| --- | --- | --- | --- |
| `test_initialize_and_opcodes.py` | `InitializeAndOpcodes.t.sol` | 5 | 5 |
| `test_no_fe_prefix.py` | `NoFePrefix.t.sol` | 2 | 2 |
| `test_stream_truncation.py` | `StreamTruncation.t.sol` | 2 | 2 |
| `test_profit_guard_m1_m2.py` | `ProfitGuard.t.sol` | 2 | 2 |
| `test_token_sentinels.py` | `TokenSentinels.t.sol` | 5 (parametrized) | 5 |
| `test_preprocess_addr_count.py` | `PreprocessAddrCount.t.sol` | 2 | 4 (288-byte accept and 289-byte ABI rejection added) |
| `test_cmd_executor_edge_cases.py` | `EdgeCases.t.sol` | 2 | 2 |
| `test_execution_receipt.py` | `ExecutionReceipt.t.sol` | 12 (parametrized) | 12 |
| `test_events.py` | `Events.t.sol` | 6 | 11 (6 source + 5 documented event-surface additions on the withdrawal exit paths: V4 take counted exactly once with no ERC6909 topic, ERC6909 mint logged exactly once, the burn/take/xfer-balance exit surface, the native-ETH exit's single-ERC6909 surface, SEND_ETH_ALL's zero-event surface) |
| `test_withdraw_reentrancy.py` | `WithdrawReentrancy.t.sol` | 2 | 3 (withdraw owner-only added) |
| `test_donation_profit_inflation.py` | `DonationProfitInflation.t.sol` | 1 (+1 skipped) | 1 |
| `test_sentinel_failclosed.py` | `SentinelFailClosed.t.sol` | 21 (11 helper meta-tests) | 21 |
| `test_v3_libraries.py` | `V3Libraries.t.sol` | 53 | 53 |
| `test_v3_pool.py` | `V3Pool.t.sol` | 2 | 2 |
| `test_v3_debug.py` | `V3Debug.t.sol` | 1 | 1 |
| `test_v3_library_fuzz.py` | `V3LibraryFuzz.t.sol` | 24 (10 differential + 14 parity-helper meta) | 24 |
| `test_v2_fee_denominator_fuzz.py` | `V2FeeDenominatorFuzz.t.sol` | 7 | 7 |
| `test_v2_configurable_fee.py` | `V2ConfigurableFee.t.sol` | 7 | 7 |
| `test_v2_fee_bounds.py` | `V2FeeBounds.t.sol` | 4 | 4 |
| `test_v2_swap_calc_excess.py` | `V2SwapCalcExcess.t.sol` | 7 | 7 |
| `test_v2_swap_compact_fee.py` | `V2SwapCompactFee.t.sol` | 4 | 4 |
| `test_cmd_executor_v2_swap_direct.py` | `CmdExecutorV2SwapDirect.t.sol` | 8 | 8 (incl. the named V2-V4-V4 DIRECT-vs-CALC gas case) |
| `test_cmd_executor_three_pool_v2.py` | `CmdExecutorThreePoolV2.t.sol` | 9 | 9 (incl. per-approach named gas evidence) |
| `test_cmd_executor_v2v2_v3v3.py` | `CmdExecutorV2V2V3V3.t.sol` | 2 | 2 |
| `test_cmd_executor_v3_three_hop.py` | `CmdExecutorV3ThreeHop.t.sol` | 14 | 14 (incl. the four named `ApproachGas` evidence cases; forward direct custody pins the exact empty-data auto-pay underflow revert) |
| `test_cmd_executor_v2v3.py` | `V2V3.t.sol` | 2 | 2 |
| `test_cmd_executor_compact.py` | `CmdExecutorCompact.t.sol` | 5 | 5 (all executions keep the source's `config = 2`; the take-compact USDC and withdraw-all zero-WETH pins are exact) |
| `test_cmd_executor_dynamic.py` | `CmdExecutorDynamic.t.sol` | 6 | 6 (TAKE_DELTA / SETTLE_DELTA / SETTLE_ALL / SWAP_DYNAMIC + both mixed sequences, default config) |
| `test_cmd_executor_v4v2.py` | `CmdExecutorV4V2.t.sol` | 3 | 3 (explicit-callback V2 pay, V2-first outer callback, direct-custody V2_SWAP_CALC) |
| `test_cmd_executor_v4v3.py` | `CmdExecutorV4V3.t.sol` | 3 | 3 (explicit V3 forward_data pay+sync+transfer+settle, V4→V3 auto-pay, V3-first auto-pay) |
| `test_cmd_executor_v4v4.py` | `CmdExecutorV4V4.t.sol` | 7 | 7 (module WBTC/WETH PM fixture override preserved; three V4_BATCH explicit/dynamic auto-settle cases; WETH→USDC→WBTC→WETH three-hop with its class-local WETH/USDC PM + executor) |
| `test_cmd_executor_v4v4_wrap_unwrap.py` | `CmdExecutorV4V4WrapUnwrap.t.sol` | 4 | 4 (unwrap/wrap scenarios, exact and _ALL; source USDC-profit assertions exact) |
| `test_cmd_executor_inline_wrap_unwrap.py` | `CmdExecutorInlineWrapUnwrap.t.sol` | 8 | 8 (A-D scenarios × exact/_ALL under `config = 2`; USDC-profit assertions exact) |
| `test_withdrawal.py` | `Withdrawal.t.sol` | 9 | 9 (ERC6909→physical USDC in two/one tx, ERC6909 native ETH, ERC20_XFER_BALANCE and specific-amount ERC20_TRANSFER, SEND_ETH/SEND_ETH_ALL, both round trips; config = 2 preserved on every execution, the ERC20-only withdrawal tx at the default config = 1) |
| `test_conservation.py` | `Conservation.t.sol` | 4 | 7 (4 source + 3 documented direct-helper meta-tests; every live failure pinned to its exact Error(string) reason via an external-call negative control, and the tracked-recipient case pins the positive conservation verdict before the profit mismatch) |
| `test_v3_v4_v3_residual_regression.py` | `V3V4V3ResidualRegression.t.sol` | 2 | 3 (2 source + 1 added: OLD static encoder's `config = 0` revert pinned exactly as `InvalidProfitMode(0)`; the added valid-config case runs the SAME old static stream and pins the PM's exact `CurrencyNotSettled:<wbtc>: +<surplus>` residual; NEW dynamic+delta+settle-all succeeds with positive profit) |
| `test_v2_swap_calc_overflow.py` | `V2SwapCalcOverflow.t.sol` | 1 (permanently skipped) | 2 (executable checked-mul boundary + normal-path control; skip replaced) |
| — (gap filled, not a port) | `AbiOverloads.t.sol` | 0 | 3 |
| — (canonical Uniswap bytecode) | `RealLibraries.t.sol` | 0 | 2 |
| — (harness substrate primitives, QXIGKC) | `HarnessPrimitives.t.sol` | 0 | 22 |

Not ported: the remaining swap-family files (`bribe_commands`,
`bribe_transfer`), the three-hop matrices
(`three_hop_optimized`, `three_hop_production_calldata`),
`erc6909`, `erc6909_composability`, `pm_as_bank`,
`release_metadata`, the fake-parity tiers (`fake_pool_manager_parity`,
`fake_abi_fidelity`), and the Rust-artifact cross-checks.

## Porting rules this lane encodes

- **The test contract is the owner EOA.** Every owner action is a plain call, so
  `msg.sender` is the owner at the top level and the executor inside callbacks —
  the same call graph Ape's `sender=owner_account` produces. `vm.prank` is used
  only for single-level negative cases (non-owner callers); it must not be used
  around executor calls, because it does not propagate into inner calls.
- **Default arguments are overloads, not runtime defaults.** Vyper emits one
  selector per defaulted-argument permutation — `config: uint256 = 1` yields
  both `execute(bytes)` (`0x09c5eabe`) and `execute(bytes,uint256)`
  (`0xab5898e8`), confirmed by `vyper -f method_identifiers`. A Solidity caller
  must name one, so `test/AbiOverloads.t.sol` pins both selectors, proves the two
  entry points return the same receipt, and proves the omitted-argument form is
  NOT `config = 0` (mode 0 is rejected, so a port that substitutes 0 fails loudly
  rather than silently skipping the profit check).
- **Vyper's ETH-receipt surface is `__default__`, not `receive`.** The executor,
  all five fakes, and the harnesses declare `__default__` (ABI entry
  `__default__()` = `0x89402a72`); the executor's version is what the donation
  test gates. The Solidity *test harness* needs `receive()` only because it
  stands in for an EOA that receives ETH (WETH.withdraw payouts, bribes).
- **`execute.call` becomes `_executeSimulated`** (snapshot → call → revert) to
  keep Ape's read-only `eth_call` semantics.
- **Reverts are pinned, not merely expected.** Wherever the contract exposes a
  revert identity, the port asserts the exact revert data — selector and
  arguments through the generated `ExecutorErrors` vocabulary — instead of a
  bare `assertFalse(ok)`. Reverts that carry no identity (Vyper bounds checks,
  ABI decoder rejections) are pinned as the zero-length payload; the empty
  execution section and the hook sentinels are deliberate empty-data cases,
  and the native hook sentinel's downstream `ManagerLocked` Error(string) is
  asserted as an identified acceptance, distinct from any executor guard.
