# Executor migration manifest: Ape (`tests/executor/`) → Foundry (`tests/foundry_executor/`)

Checklist manifest for ergo epic N24TXM. Published by **636OFE**; every later port
task updates its rows (Status column) instead of re-deriving scope. This is a
living checklist, not a retirement tripwire.

## Baseline evidence (frozen by 636OFE, 2026-11-17)

Tree at capture: `HEAD` = `adc5501d4cbf9641e65a1ebe8f4a131edae50fe5` (the required
baseline is `HEAD`, so ancestry holds trivially). Baseline logs are retained under
`/tmp`, outside tracked artifacts:

| Lane | Command | Result | Exit | Log |
|---|---|---|---|---|
| Offline Ape (executor-scoped) | `just test-executor-offline tests/executor/ -q` | 378 passed, 3 skipped (381 collected), 0 failed — 62.5s | 0 | `/tmp/ape-offline-baseline.log` |
| Mainnet-fork-only Ape | `just test-executor-offline tests/executor/ -m mainnet_fork -q` | 24 passed, 5 failed (29 collected) — 23.2s | 1 | `/tmp/ape-fork-baseline.log` |
| Foundry (existing port) | `just test-foundry-executor` | 48 passed, 0 failed, 0 skipped across 14 suites | 0 | `/tmp/foundry-baseline.log` |

The offline skips are the explicit dispositions 4, 5 and 6 below (donation
PM-native-take smoke, V2 overflow guard, ordering-dependent PM-bank gas summary).
`test_gas_comparison_table` is xdist-order dependent: it skips whenever its worker
collects/runs it before the same-module classes that call `_record`, because its
input is in-memory state, not a persisted artifact.

Fork-lane failures are recorded baseline facts, not regressions introduced by this
task; all five live in `test_fake_pool_manager_parity.py` and are LEZT6O port scope.
All five fail on the same setup assertion — `get_code(REAL_PM_ADDRESS)` returns `0x`
on the local Foundry provider (`chain_id=31337`) — i.e. the fork never brought real
PoolManager bytecode up at the expected height. These are environment/fork-setup
failures, not characterized contract-behavior failures: LEZT6O must validate the
archive endpoint and fork height and fail closed when the fork is not real, rather
than treating any of these as a contract-behavior red.

- `TestTake::test_take_native_transfers_eth`
- `TestSyncSettle::test_settle_native_inside_unlock`
- `TestExttloadTransientSlots::test_exttload_native_delta_matches`
- `TestExttloadTransientSlots::test_exttload_lock_and_count_slots_inside_unlock`
- `TestExttloadTransientSlots::test_exttload_cleared_outside_unlock`

**Card-command caveat.** The PM card's literal offline command
`just test-executor-offline -q` hands `-q` to the recipe's first positional
(`target`), so ape runs with no test path and collects the entire repository suite
under the ape pytest plugin: 4216 passed, 221 failed, 133 skipped, 68 errors, exit 1,
21m32s (log `/tmp/ape-whole-repo-literal-card-cmd.log`). That is not the executor
baseline. The executor-scoped invocation passes the target explicitly, as in the
table above.

## Count reconciliation

Task bodies N24TXM/636OFE cite "45 modules"; the tree at baseline has **47**
`test_*.py` modules, while N24TXM's "371 Python test functions" matches exactly.
All 47 modules are assigned below — coverage is a superset of the stale count, so no
module is unassigned.

- Test definitions: 371 `def test_*`, one of which is the shadowed duplicate in
  `test_fake_pool_manager_parity.py`.
- Collected definitions: 370 (only the second `test_double_unlock_reverts` is
  collected; AST scan found no other same-class shadowing).
- Offline collected cases: 381 = 341 non-parity collected defs + 40 parametrize
  expansions (`test_execution_receipt` +5, `test_erc6909_composability` +1,
  `test_fake_abi_fidelity` +3, `test_cmd_executor_three_hop_production_calldata` +26,
  `test_release_metadata` +3, `test_token_sentinels` +2). Hypothesis property tests
  count as one case each; their examples are internal to the case.
- Fork-only collected cases: 29 (29 collected from the parity module's 30 defs).
- Sum of the table's Cases column: 410 = 381 offline + 29 fork.

## Explicit dispositions (duplicates, empty, skips, helpers, gas)

| # | Item | Decision | Owner |
|---|---|---|---|
| 1 | Duplicate `TestUnlock.test_double_unlock_reverts` (`test_fake_pool_manager_parity.py:171` and `:179`; Python class semantics collect only the second) | Port the collected second definition's assertions AND add a real callback-driven nested-unlock test asserting `AlreadyUnlocked` for the shadowed first definition's intended behavior — the first definition is shadowed in Ape collection, but its behavior is a port requirement, not dead code to drop | LEZT6O |
| 2 | Empty `TestSwap.test_swap_zero_amount_reverts` (docstring + comment, no assertion) | Replace: port as a real `SwapAmountCannotBeZero` revert assertion in the fork lane | LEZT6O |
| 3 | Empty `TestModifyLiquidity.test_update_dynamic_lp_fee_works_outside_unlock` (no assertion; Ape could not impersonate the hook) | Replace, not drop: Foundry `vm.prank` can impersonate the hook address, so port as a real outside-unlock assertion | LEZT6O |
| 4 | Permanent skip `test_pm_native_take_still_delivers_eth` (`test_donation_profit_inflation.py`) | Retire the permanent skip; PM native-take delivery is covered by the withdrawal native-ETH legs (`test_withdrawal.py` → `Withdrawal.t.sol`), while `DonationProfitInflation.t.sol` stays focused on `__default__` gating | LVVJOU + AL53BE |
| 5 | Permanent skip `test_normal_v2_swap_calc_unaffected` (`test_v2_swap_calc_overflow.py`) | Replace the skip with an executable boundary test in `V2SwapCalcOverflow.t.sol`: drive the checked-mul path via an unbounded test token (or direct storage seeding of the pair reserves) and assert the clean checked-mul revert, plus a normal-path control proving no regression | AVYD7W |
| 6 | Ordering-dependent `test_gas_comparison_table` (`test_pm_as_bank.py`) | Not ported as-is: print-only summary (no assertion beyond the skip) fed by in-memory `_record` state, hence xdist-order dependent. Replace with deterministic Forge gas evidence for the V2-V2-V2 / V2-V3-V2 / V3-V2-V2 / V3-V3-V2 MINT-vs-TAKE paths | Q3UOC6 |
| 7 | Unused helpers `run_gas_benchmark` / `compute_access_list` (`conftest_shared.py`) | Delete the helpers (zero call sites in the suite); Q3UOC6 adds the isolated `vm.rpc eth_createAccessList` benchmark lane as their Foundry replacement | Q3UOC6 |
| 8 | Stale `cmd_stream.py` | Delete, no port: zero importers; its parser duplicates `conftest_shared.py` encoders | KW5RDF |
| 9 | `.gas-results` (repo root) | Written by `record_gas` (sole caller `test_cmd_executor_three_hop_optimized.py`), unlinked by `conftest.py` at session start, read by nothing. Retire the write path with the Q3UOC6 gas-evidence replacement; delete the file at the KW5RDF cutover | Q3UOC6 + KW5RDF |
| 10 | Intentional `config = 0` revert in `test_v3_v4_v3_residual_regression.py` | Preserve: port as an exact-revert characterization. The revert under `config = 0` is the current intended contract behavior (overproduction guard), **not** an omitted-default bug | 72TJMU |
| 11 | `pytest.mark.gas` surfaces (`pytestmark` on `test_cmd_executor_three_hop_optimized.py` and `test_cmd_executor_three_hop_production_calldata.py`; ad-hoc `*_gas` tests in `test_cmd_executor_v3_three_hop.py`, `test_cmd_executor_three_pool_v2.py`, `test_cmd_executor_v2_swap_direct.py`, `test_erc6909.py`) | Port as Forge gas evidence (forge gas snapshots / per-path gas constants), not silent drops | 3VFCP5, 5SPJYJ, 72TJMU, AVYD7W, EMCYDR, Q3UOC6 |

## Module rows (one row per `test_*.py` module — all 47, none unassigned)

Defs = `def test_*` definitions; Cases = collected cases after pytest parametrize
expansion. Owning tasks are the port epic's ergo IDs.

| Module | Defs | Cases | Owner | Forge destination | Status | Notes |
|---|---|---|---|---|---|---|
| `test_cmd_executor_bribe_commands.py` | 5 | 5 | GTRKFA | test/CmdExecutorBribeCommands.t.sol | to create |  |
| `test_cmd_executor_bribe_transfer.py` | 4 | 4 | GTRKFA | test/CmdExecutorBribeTransfer.t.sol | to create |  |
| `test_cmd_executor_compact.py` | 5 | 5 | JBBFCQ | test/CmdExecutorCompact.t.sol | to create |  |
| `test_cmd_executor_dynamic.py` | 6 | 6 | JBBFCQ | test/CmdExecutorDynamic.t.sol | to create |  |
| `test_cmd_executor_edge_cases.py` | 2 | 2 | LVVJOU | test/EdgeCases.t.sol | ported (green) | Success-only port; both sources assert success paths, no broad revert assertions to harden. |
| `test_cmd_executor_inline_wrap_unwrap.py` | 8 | 8 | JBBFCQ | test/CmdExecutorInlineWrapUnwrap.t.sol | to create |  |
| `test_cmd_executor_three_hop_optimized.py` | 27 | 27 | 3VFCP5 | test/CmdExecutorThreeHopOptimized.t.sol | to create | `pytestmark = pytest.mark.gas`; sole `record_gas` caller (dispositions 9/11). |
| `test_cmd_executor_three_hop_production_calldata.py` | 6 | 32 | 5SPJYJ | test/CmdExecutorThreeHopProductionCalldata.t.sol | to create | `pytestmark = pytest.mark.gas`; 27-combo PERMUTATIONS expansion; consumes the Rust `encode_three_hop` [[bin]] artifact (5SPJYJ FFI lane). |
| `test_cmd_executor_three_pool_v2.py` | 9 | 9 | AVYD7W | test/CmdExecutorThreePoolV2.t.sol | to create | 3 `test_approach_*_gas` tests inside (disposition 11). |
| `test_cmd_executor_v2_swap_direct.py` | 8 | 8 | AVYD7W | test/CmdExecutorV2SwapDirect.t.sol | to create | `test_gas_comparison_v2_v4_v4` inside (disposition 11). |
| `test_cmd_executor_v2v2_v3v3.py` | 2 | 2 | 72TJMU | test/CmdExecutorV2V2V3V3.t.sol | to create |  |
| `test_cmd_executor_v2v3.py` | 2 | 2 | LVVJOU | test/V2V3.t.sol | ported (green) | Success-only port; no broad revert assertions to harden. |
| `test_cmd_executor_v3_three_hop.py` | 14 | 14 | 72TJMU | test/CmdExecutorV3ThreeHop.t.sol | to create | 4 ad-hoc `*_gas` tests inside (disposition 11). |
| `test_cmd_executor_v4v2.py` | 3 | 3 | JBBFCQ | test/CmdExecutorV4V2.t.sol | to create |  |
| `test_cmd_executor_v4v3.py` | 3 | 3 | JBBFCQ | test/CmdExecutorV4V3.t.sol | to create |  |
| `test_cmd_executor_v4v4.py` | 7 | 7 | JBBFCQ | test/CmdExecutorV4V4.t.sol | to create |  |
| `test_cmd_executor_v4v4_wrap_unwrap.py` | 4 | 4 | JBBFCQ | test/CmdExecutorV4V4WrapUnwrap.t.sol | to create |  |
| `test_conservation.py` | 4 | 4 | AL53BE | test/Conservation.t.sol | to create |  |
| `test_donation_profit_inflation.py` | 2 | 2 | LVVJOU | test/DonationProfitInflation.t.sol | ported (green) | 2 defs; `test_pm_native_take_still_delivers_eth` is a permanent `pytest.skip` (disposition 4) — PM native-take delivery stays covered by the withdrawal native-ETH legs. Hardened: the donation pins the exact `Unauthorized(caller)` the `__default__` gate raises. |
| `test_erc6909.py` | 5 | 5 | EMCYDR | test/ERC6909.t.sol | to create | `test_mint_vs_take_gas` inside (disposition 11). |
| `test_erc6909_composability.py` | 6 | 7 | EMCYDR | test/ERC6909Composability.t.sol | to create |  |
| `test_events.py` | 6 | 6 | AL53BE | test/Events.t.sol | ported (green) |  |
| `test_execution_receipt.py` | 7 | 12 | LVVJOU | test/ExecutionReceipt.t.sol | ported (green) | Hardened: `InvalidProfitMode(mode)` argument pins for all four invalid modes, exact `InsufficientProfit(actual, expected)` pins (decrease, shortfall, bribe floor), `Unauthorized(caller)` sweep pin. |
| `test_fake_abi_fidelity.py` | 6 | 9 | QKQOIZ | test/FakeABIFidelity.t.sol | to create |  |
| `test_fake_pool_manager_parity.py` | 30 | 29 | LEZT6O | test/PoolManagerParity.t.sol (fork lane) | to create (fork lane) | 30 defs / 29 collected (duplicate `test_double_unlock_reverts`; first definition shadowed). `pytestmark mainnet_fork`. 5 fork failures recorded as baseline facts. The `ExttloadComparator` artifact (+ `IExttloadComparator`) is built by the lane's pinned-vyper pipeline since QXIGKC. |
| `test_initialize_and_opcodes.py` | 5 | 5 | LVVJOU | test/InitializeAndOpcodes.t.sol | ported (green) | Hardened: exact `Unauthorized(caller)` and `InvalidMsgValue(value)` pins. |
| `test_no_fe_prefix.py` | 2 | 2 | LVVJOU | test/NoFePrefix.t.sol | ported (green) | Hardened to exact pins: `InvalidCommand(0xFE)` for the prefixed stream, the empty-data revert for bare `0xFF`. |
| `test_pm_as_bank.py` | 11 | 11 | EMCYDR | test/PMAsBank.t.sol | to create | 11 defs; `test_gas_comparison_table` is the ordering-dependent gas summary (disposition 6). |
| `test_preprocess_addr_count.py` | 2 | 2 | LVVJOU | test/PreprocessAddrCount.t.sol | ported (green) | Hardened: empty-data pin on the 294-byte ABI rejection; exact 288-byte accept and 289-byte ABI rejection cases added (both empty-data at the decoder). |
| `test_profit_guard_m1_m2.py` | 2 | 2 | LVVJOU | test/ProfitGuard.t.sol | ported (green) | Hardened: exact `BipsTooHigh(10001)` pin via a non-degenerate stream (a bare separator reverts in the dispatch loop before any config handling). |
| `test_release_metadata.py` | 19 | 22 | I5R3LB | test/ReleaseMetadata.t.sol | to create |  |
| `test_sentinel_failclosed.py` | 21 | 21 | LVVJOU | test/SentinelFailClosed.t.sol | ported (green) | Includes 11 helper meta-tests (`expect_revert`/`expect_accepted` negative controls). Preserves the intentional downstream `ManagerLocked` acceptance (native hook sentinel) and the empty-data hook-sentinel semantics; exercises both `execute(bytes)` and `execute(bytes,uint256)`. |
| `test_stream_truncation.py` | 2 | 2 | LVVJOU | test/StreamTruncation.t.sol | ported (green) | Hardened: the maximal stream's revert is pinned as the exact Error(string) `ManagerLocked` payload. |
| `test_token_sentinels.py` | 3 | 5 | LVVJOU | test/TokenSentinels.t.sol | ported (green) | Hardened: the unassigned reserved index pins the empty-data fail-closed revert. |
| `test_v2_configurable_fee.py` | 7 | 7 | AVYD7W | test/V2ConfigurableFee.t.sol | to create |  |
| `test_v2_fee_bounds.py` | 4 | 4 | AVYD7W | test/V2FeeBounds.t.sol | to create |  |
| `test_v2_fee_denominator_fuzz.py` | 7 | 7 | XYAZ7L | test/V2FeeDenominatorFuzz.t.sol | to create | 7 Hypothesis pure-math properties (examples internal to each case). |
| `test_v2_swap_calc_excess.py` | 7 | 7 | AVYD7W | test/V2SwapCalcExcess.t.sol | to create |  |
| `test_v2_swap_calc_overflow.py` | 1 | 1 | AVYD7W | test/V2SwapCalcOverflow.t.sol | to create (replaces skip; disposition 5) | 1 def; permanently `pytest.skip`s in Ape — ported as the executable checked-mul boundary test of disposition 5. |
| `test_v2_swap_compact_fee.py` | 4 | 4 | AVYD7W | test/V2SwapCompactFee.t.sol | to create |  |
| `test_v3_debug.py` | 1 | 1 | XYAZ7L | test/V3Debug.t.sol | to create |  |
| `test_v3_libraries.py` | 53 | 53 | XYAZ7L | test/V3Libraries.t.sol | to create |  |
| `test_v3_library_fuzz.py` | 24 | 24 | XYAZ7L | test/V3LibraryFuzz.t.sol | to create | 10 Hypothesis differential tests (200-500 examples each) + 14 parity-helper meta-tests. The committed solc-0.7.6 `SolReference` artifact (+ `ISolReference`) is built by the lane since QXIGKC. |
| `test_v3_pool.py` | 2 | 2 | XYAZ7L | test/V3Pool.t.sol | to create |  |
| `test_v3_v4_v3_residual_regression.py` | 2 | 2 | 72TJMU | test/V3V4V3ResidualRegression.t.sol | to create | Intentional `config = 0` revert — disposition 10. |
| `test_withdraw_reentrancy.py` | 2 | 2 | LVVJOU | test/WithdrawReentrancy.t.sol | ported (green) | Hardened: the reentry pins the exact `Unauthorized(caller)` the owner gate raises at reentry; withdraw owner-only coverage added. |
| `test_withdrawal.py` | 9 | 9 | AL53BE | test/Withdrawal.t.sol | to create |  |

## Support / config rows (one row per named file)

| File | Role | Owner | Disposition |
|---|---|---|---|
| `conftest.py` | Fixtures (`owner_account`, `usdc`, `weth`, `wbtc`, `v4_pm`, `executor`, `run_executor`), xdist per-worker foundry port pinning, hypothesis deadline profile, `.gas-results` session unlink | QXIGKC | Fixture semantics ported into `Setup.sol`/`TestBase.sol` deployment + funding helpers (landed in the QXIGKC substrate; see the primitives table below); per-worker port pinning and the session-unlink side effect retire with the Ape runner |
| `conftest_shared.py` | Stream encoders (`enc_preamble`, `make_config`, `enc_*`), snapshot/revert helpers, log counting support, `record_gas`, unused `run_gas_benchmark`/`compute_access_list` | QXIGKC (+Q3UOC6 for gas helpers) | Encoders → `Cmd.sol` builders **landed** (incl. the raw 0x30 form and V4_BATCH_OPEN_WETH); snapshot/revert helpers → `TestBase.sol` conservation + exact-revert primitives **landed**; `record_gas` → Q3UOC6 gas evidence; unused helpers deleted (disposition 7) |
| `verify.py` | Raw log topic0 ERC20/ERC6909 transfer counting for transfer-count assertions | QXIGKC | Ported as `TestBase.sol` log-counting helpers (`_countTransfers`, `_countTopic`, plus `_countTransfersInvolving` for tracked-address accounting) — **landed** |
| `cmd_stream.py` | Standalone command-stream parser | KW5RDF | Stale, zero importers — delete at cutover, no port (disposition 8) |
| `ape-config.yaml` (repo root) | Ape project config | KW5RDF | Delete at the hard cutover together with the Ape dependency surface |
| `ape-pytest.ini` (repo root) | Ape pytest config (markers, xdist, timeout) | KW5RDF | Delete at the hard cutover; marker semantics (fork lane, gas) are superseded by the Foundry lane split (`test-foundry-executor` / fork recipes) |
| `.gas-results` (repo root) | Runtime gas output of `record_gas` | Q3UOC6 + KW5RDF | Unread at baseline — retire the writer, delete the file (disposition 9) |

## Existing Foundry substrate rows (not from an Ape module)

| Forge file | Tests | Owner | Role |
|---|---|---|---|
| `test/AbiOverloads.t.sol` | 3 | QXIGKC | Pins the `execute(bytes)` / `execute(bytes,uint256)` selector pair Vyper emits for the defaulted `config` argument; both entry points must agree |
| `test/RealLibraries.t.sol` | 2 | QKQOIZ | Canonical `UniswapV2Pair` (solc 0.5.16) and `UniswapV3Pool` (solc 0.7.6) bytecode fidelity behind the fake-pool suites |
| `src/Cmd.sol`, `src/TestBase.sol`, `src/Setup.sol`, `src/Vm.sol`, `src/generated/*` + `build_artifacts.py`, `build_solidity_artifacts.py`, `gen_interfaces.py`, `foundry.toml` | — | QXIGKC | Harness/artifact substrate. QXIGKC landed the shared primitives below; canonical V4 PoolManager compilation is QKQOIZ scope, not here |

### Substrate primitives landed (QXIGKC)

Each primitive has a named test in `test/HarnessPrimitives.t.sol` and at least
one downstream port task. The revert-selector table is **generated**
(`src/generated/Errors.sol`, from the executor artifact ABI + the tracked
release `cmd_executor.error_map.json` vocabulary, cross-checked against the
fresh compile on every rebuild) — no port may hand-write a second selector
table.

| Primitive | Named test(s) | Downstream task |
|---|---|---|
| Raw `V3_SWAP_COMPACT` 0x30 encoder (`Cmd.v3SwapCompactRaw`, 17+N bytes, in-band direction flag) | `testV3SwapCompactRawBytesAndLength`, `testV3SwapCompactRawDirectionFlagOnly` | XYAZ7L (`test_v3_debug.py`, `v3_debug` streams); 72TJMU (`v3_three_hop` nested-callback streams) |
| `V4_BATCH_OPEN_WETH` 0x43 encoder (`Cmd.v4BatchOpenWeth`, shared 0x42 body) | `testV4BatchOpenWethOpAndLayout` | EMCYDR (`test_erc6909_composability.py` batch x ERC6909 capture) |
| `makeConfig` field-width facts (mode 0 rejected loudly at the boundary) | `testMakeConfigFieldWidths`, `testMakeConfigRejectsBadMode` | GTRKFA, JBBFCQ (bribe/compact config ports) |
| Generated `ExecutorErrors` vocabulary (`src/generated/Errors.sol`) | `testGeneratedSelectorsMatchCanonicalSignatures`, `testNameOfCoversGeneratedVocabulary` | LVVJOU (`SentinelFailClosed` exact-revert ports), 72TJMU (disposition 10 exact-revert characterization) |
| Exact revert selector+argument assertions (`_assertRevertData`, `_assertRevertSelector`, `_executorRevert`) with success/wrong-selector/wrong-argument/unrelated-error negative controls | `testExecutorInvalidCommandExactSelectorAndArgument` + the `testAssertRevertData*` matrix | LVVJOU (harness meta-tests of `test_sentinel_failclosed.py`'s `TestRevertHelpers`) |
| Balance snapshots & conservation diffs (`_snapshotBalances`, `_tokenDeltaSum`, `_assertTokenConserved`, `_assertWethEthConserved`, `_assertExecutorProfit`) | `testTokenConservationPassesWithinTrackedSet`, `testTokenConservationFailsOnLeakToUntrackedAddress`, `testWethEthConservationAndProfitDelta` | AL53BE (`test/Conservation.t.sol` port), 3VFCP5/5SPJYJ (three-hop profit checks) |
| Tracked transfer/log accounting (`_countTransfersInvolving` alongside the existing topic counters; only the ERC20/ERC6909 Transfer topics count — Swap logs and the ERC6909 id topic never do) | `testCountTransfersInvolvingTracksBothSides`, `testCountTransfersInvolvingIgnoresSwapAndTokenIdTopics` | EMCYDR (`ERC6909` transfer-count ports), AL53BE (`test_withdrawal.py` round trips) |
| `Vm.sol` lane declarations: `ffi` (5SPJYJ), `rpc`/`createFork`/`selectFork`/`activeFork` (LEZT6O fork, Q3UOC6 `eth_createAccessList`), `parseJson`/`parseJsonString`/`parseJsonUint` (I5R3LB), `expectRevert(bytes4)` (LVVJOU), `toString(bytes32)`/`toString(bytes)` (hex diagnostics) | `testParseJsonTypedGetters`, `testToStringHexForms` (the offline-safe lanes; FFI/RPC/fork declarations are exercised by their consuming ports, which set `--ffi` or a live endpoint) | 5SPJYJ, LEZT6O, Q3UOC6, I5R3LB, LVVJOU |
| `ExttloadComparator.vy` compiled into the artifact substrate (+ `IExttloadComparator`) | `testExttloadComparatorArtifactSlotMath` | LEZT6O (fork-lane exttload parity comparisons) |
| Committed solc-0.7.6 `SolReference` built into the lane's artifact shape (+ `ISolReference`) | `testSolReferenceMatchesVyperHarness` (differential vs `TestHarness`) | XYAZ7L (`test_v3_libraries.py`, `test_v3_library_fuzz.py`) |
| `assertEq(string,string)` in `TestBase` | used throughout `HarnessPrimitives.t.sol` | all remaining ports |

The 0x33/0x34 direction-specific encoders were already in place
(`Cmd.v3SwapCompact`); the source helper `make_bribe_config` is subsumed by
`Cmd.makeConfig` — its zero-mode packing has no ported call site, and a zero
mode is rejected loudly at the executor boundary (`testMakeConfigRejectsBadMode`).

## Function inventory (source test functions per module)

Every `def test_*` in the tree, in definition order. `Class.` prefixes are the Python
collection path; a bare name is module-level. The shadowed duplicate is marked.

- `test_cmd_executor_bribe_commands.py` (5): `TestBribeCoinbase.test_bribe_coinbase_first_command`, `TestBribeCoinbase.test_bribe_coinbase_zero_bips`, `TestBribeAddress.test_bribe_address_first_command`, `TestBribeAddress.test_bribe_address_gas_overhead`, `TestBribeWETHAutoWithdraw.test_bribe_auto_withdraws_weth`
- `test_cmd_executor_bribe_transfer.py` (4): `TestBribeTransferBalances.test_bribe_address_sends_to_recipient`, `TestBribeTransferBalances.test_bribe_coinbase_sends_eth`, `TestBribeTransferBalances.test_no_bribe_sends_nothing`, `TestBribeTransferBalances.test_bribe_fractional_precision`
- `test_cmd_executor_compact.py` (5): `TestV4SwapCompact.test_v4_swap_compact_weth_usdc`, `TestErc20XferBalance.test_xfer_balance_after_v4_take`, `TestV4TakeCompact.test_v4_take_compact`, `TestWethDepositAll.test_deposit_all_eth`, `TestWethWithdrawAll.test_withdraw_all_weth`
- `test_cmd_executor_dynamic.py` (6): `TestV4TakeDelta.test_v4_take_delta_weth_profit`, `TestV4SettleDelta.test_v4_settle_delta_weth`, `TestV4SettleAll.test_v4_settle_all_v4v4_cross_currency`, `TestV4SwapDynamic.test_v4_swap_dynamic_v4v4`, `TestMixedExplicitDynamic.test_mixed_v4_take_and_take_delta`, `TestMixedExplicitDynamic.test_settle_then_settle_all`
- `test_cmd_executor_edge_cases.py` (2): `TestV4NativeEthSettlement.test_v4_native_eth_input`, `TestV2DirectSwapNoCallback.test_v2_direct_swap_no_callback`
- `test_cmd_executor_inline_wrap_unwrap.py` (8): `TestV4ToV2InlineWrap.test_v4_eth_to_v2_weth_with_inline_wrap`, `TestV4ToV2InlineWrap.test_v4_eth_to_v2_weth_with_inline_wrap_all`, `TestV4ToV3InlineWrap.test_v4_eth_to_v3_weth_with_inline_wrap`, `TestV4ToV3InlineWrap.test_v4_eth_to_v3_weth_with_inline_wrap_all`, `TestV2ToV4InlineUnwrap.test_v2_weth_to_v4_eth_with_inline_unwrap`, `TestV2ToV4InlineUnwrap.test_v2_weth_to_v4_eth_with_inline_unwrap_all`, `TestV3ToV4InlineUnwrap.test_v3_weth_to_v4_eth_with_inline_unwrap`, `TestV3ToV4InlineUnwrap.test_v3_weth_to_v4_eth_with_inline_unwrap_all`
- `test_cmd_executor_three_hop_optimized.py` (27): `TestV2V2V2.test_v2_v2_v2`, `TestV2V2V3.test_v2_v2_v3`, `TestV2V2V4.test_v2_v2_v4`, `TestV2V3V2.test_v2_v3_v2`, `TestV2V3V3.test_v2_v3_v3`, `TestV2V3V4.test_v2_v3_v4`, `TestV2V4V2.test_v2_v4_v2`, `TestV2V4V3.test_v2_v4_v3`, `TestV2V4V4.test_v2_v4_v4`, `TestV3V2V2.test_v3_v2_v2`, `TestV3V2V3.test_v3_v2_v3`, `TestV3V2V4.test_v3_v2_v4`, `TestV3V3V2.test_v3_v3_v2`, `TestV3V3V3.test_v3_v3_v3`, `TestV3V3V4.test_v3_v3_v4`, `TestV3V4V2.test_v3_v4_v2`, `TestV3V4V3.test_v3_v4_v3`, `TestV3V4V4.test_v3_v4_v4`, `TestV4V2V2.test_v4_v2_v2`, `TestV4V2V3.test_v4_v2_v3`, `TestV4V2V4.test_v4_v2_v4`, `TestV4V3V2.test_v4_v3_v2`, `TestV4V3V3.test_v4_v3_v3`, `TestV4V3V4.test_v4_v3_v4`, `TestV4V4V2.test_v4_v4_v2`, `TestV4V4V3.test_v4_v4_v3`, `TestV4V4V4.test_v4_v4_v4`
- `test_cmd_executor_three_hop_production_calldata.py` (6): `test_encoder_rejects_a_missing_request_argument`, `test_encoder_rejects_malformed_json`, `test_encoder_rejects_a_hop_family_it_cannot_decode`, `test_encoder_requires_tick_spacing_for_v4_hops`, `test_encoder_rejects_a_v2_fee_above_u16`, `test_production_calldata_executes_three_hop_permutation`
- `test_cmd_executor_three_pool_v2.py` (9): `TestApproach1AllV2SwapCompact.test_three_pool_nested_callback`, `TestApproach2FlashPlusSwapCalc.test_three_pool_flash_plus_calc`, `TestApproach3AllSwapCalc.test_three_pool_all_calc`, `TestGasComparison.test_approach_1_gas`, `TestGasComparison.test_approach_2_gas`, `TestGasComparison.test_approach_3_gas`, `TestEconomicEquivalence.test_approach_1_profit`, `TestEconomicEquivalence.test_approach_2_profit`, `TestEconomicEquivalence.test_approach_3_profit`
- `test_cmd_executor_v2_swap_direct.py` (8): `TestV2SwapDirectBasic.test_v2_swap_direct_simple`, `TestV2SwapDirectBasic.test_v2_swap_direct_v4_take_prefund`, `TestV2SwapDirectVsCalc.test_v2_v2_v2_direct`, `TestV2SwapDirectThreeHop.test_v2_v4_v4_direct`, `TestV2SwapDirectThreeHop.test_v4_v2_v4_direct`, `TestV2SwapDirectThreeHop.test_v4_v2_v2_direct`, `TestV2SwapDirectThreeHop.test_v2_v4_v2_direct`, `TestV2SwapDirectGasComparison.test_gas_comparison_v2_v4_v4`
- `test_cmd_executor_v2v2_v3v3.py` (2): `TestV3V3.test_v3_v3_nested_callback_double_autopay`, `TestV2V2.test_v2_v2_nested_callback`
- `test_cmd_executor_v2v3.py` (2): `TestV2ToV3.test_v2_v3_weth_usdc_to_weth`, `TestV3ToV2.test_v3_v2_weth_usdc_to_weth`
- `test_cmd_executor_v3_three_hop.py` (14): `TestV3V3V3NestedCallbacks.test_v3_v3_v3_nested_callbacks`, `TestV3V3V3InnerAutoPay.test_v3_v3_v3_inner_auto_pay`, `TestV3V3V3MiddleAutoPay.test_v3_v3_v3_middle_auto_pay`, `TestV3V3V3DoubleAutoPay.test_v3_v3_v3_double_auto_pay`, `TestV3V3V3ReverseDirectCustody.test_v3_v3_v3_reverse_direct_custody`, `TestV3V3V3ForwardDirectCustody.test_v3_v3_v3_forward_direct_custody_fails`, `TestV4V3V3Delta.test_v4_v3_v3_with_take_then_compact`, `TestV4V3V3Delta.test_v4_v3_v3_delta_pre_funded`, `TestV3V3V4.test_v3_v3_v4`, `TestPancakeV3Callback.test_v3_v3_v3_pancake_callback`, `TestGasComparison.test_nested_callbacks_gas`, `TestGasComparison.test_double_auto_pay_gas`, `TestGasComparison.test_v4_v3_v3_gas`, `TestGasComparison.test_v3_v3_v4_gas`
- `test_cmd_executor_v4v2.py` (3): `TestV4ToV2.test_v4_v2_usdc_to_weth`, `TestV2ToV4.test_v2_v4_weth_to_usdc`, `TestV4ToV2SwapCalc.test_v4_v2_swap_calc`
- `test_cmd_executor_v4v3.py` (3): `TestV4ToV3.test_v4_v3_usdc_to_weth`, `TestV4ToV3AutoPay.test_v4_v3_auto_pay_usdc_to_weth`, `TestV3ToV4AutoPay.test_v3_v4_auto_pay_weth_to_usdc`
- `test_cmd_executor_v4v4.py` (7): `TestV4V4SameCurrency.test_v4_v4_all_weth_pairs`, `TestV4Batch.test_v4_batch_same_currency`, `TestV4Batch.test_v4_batch_cross_currency`, `TestV4Batch.test_v4_batch_usdc_intermediate`, `TestV4V4DifferentCurrency.test_v4_v4_weth_wbtc_eth`, `TestV4V4V4ThreeHop.test_v4_v4_v4_three_hop`, `TestV4V4DifferentCurrency2.test_v4_v4_usdc_intermediate_weth_eth`
- `test_cmd_executor_v4v4_wrap_unwrap.py` (4): `TestV4WethToV4Eth.test_v4_weth_to_v4_eth_with_inline_unwrap`, `TestV4WethToV4Eth.test_v4_weth_to_v4_eth_with_inline_unwrap_all`, `TestV4EthToV4Weth.test_v4_eth_to_v4_weth_with_inline_wrap`, `TestV4EthToV4Weth.test_v4_eth_to_v4_weth_with_inline_wrap_all`
- `test_conservation.py` (4): `TestConservationCatchesLeakedTokens.test_v4_take_to_untracked_address`, `TestConservationCatchesExtraTransfer.test_extra_erc20_transfer_to_untracked_address`, `TestConservationPassesButProfitCheckFails.test_v4_take_to_tracked_pool_profit_check_fails`, `TestConservationCatchesStrayTransferInCallback.test_stray_weth_transfer_after_v2_swap`
- `test_donation_profit_inflation.py` (2): `TestDonationProfitInflation.test_plain_eth_donation_reverts`, `TestDonationProfitInflation.test_pm_native_take_still_delivers_eth`
- `test_erc6909.py` (5): `TestERC6909ViewFunctions.test_default_balances_are_zero`, `TestERC6909ViewFunctions.test_approve_and_set_operator`, `TestERC6909Mint.test_mint_creates_erc6909_balance`, `TestERC6909Burn.test_burn_settles_from_erc6909`, `TestERC6909GasSavings.test_mint_vs_take_gas`
- `test_erc6909_composability.py` (6): `TestERC6909MintProfitV4V4V4.test_mint_profit_zero_transfer`, `TestERC6909MultiTxComposability.test_mint_then_burn_across_transactions`, `TestERC6909MultiTxComposability.test_burn_settles_weth_debit`, `TestERC6909MintProfitMixedPaths.test_v2_v4_v4_mint_profit`, `TestERC6909MintProfitMixedPaths.test_v3_v4_v4_mint_profit`, `TestETHFundedSettlement.test_v4_v4_v4_with_weth_deposit`
- `test_events.py` (6): `test_erc20_transfer_event`, `test_erc20_mint_event`, `test_erc20_burn_event`, `test_weth_deposit_event`, `test_weth_withdraw_event`, `test_v2_swap_events`
- `test_execution_receipt.py` (7): `test_execute_returns_zero_receipt`, `test_physical_receipt_and_bribe_accounting`, `test_invalid_profit_modes_fail_loudly`, `test_default_config_rejects_physical_balance_decrease`, `test_sweep_is_separate_from_execution`, `test_sweep_is_owner_only`, `test_min_profit_rejects_shortfall`
- `test_fake_abi_fidelity.py` (6): `TestFakeV4PoolManagerAbi.test_unlock_round_trips_oversized_data`, `TestFakeV4PoolManagerAbi.test_swap_accepts_oversized_hook_data`, `TestFakeV4PoolManagerAbi.test_modify_liquidity_accepts_oversized_hook_data`, `TestFakeV4PoolManagerAbi.test_donate_accepts_oversized_hook_data`, `TestFakeV2PairAbi.test_swap_delivers_oversized_callback_data`, `TestFakeV3PoolAbi.test_swap_delivers_oversized_callback_data`
- `test_fake_pool_manager_parity.py` (30): `TestExttloadTransientSlots.test_exttload_native_delta_matches`, `TestExttloadTransientSlots.test_exttload_lock_and_count_slots_inside_unlock`, `TestExttloadTransientSlots.test_exttload_cleared_outside_unlock`, `TestExttloadTransientSlots.test_exttload_slot_computation_consistency`, `TestUnlock.test_double_unlock_reverts`, `TestUnlock.test_double_unlock_reverts`, `TestUnlock.test_operations_locked_outside_unlock`, `TestSyncSettle.test_sync_native_outside_unlock`, `TestSyncSettle.test_settle_native_inside_unlock`, `TestSyncSettle.test_settle_for_delegates_delta`, `TestInitialize.test_initialize_basic`, `TestInitialize.test_initialize_currency_order_violation`, `TestInitialize.test_initialize_same_currencies`, `TestInitialize.test_initialize_tick_spacing_too_large`, `TestInitialize.test_initialize_works_outside_unlock`, `TestClear.test_clear_exact_positive_delta`, `TestERC6909.test_mint_credits_erc6909_and_debits_delta`, `TestERC6909.test_burn_debits_erc6909_and_credits_delta`, `TestERC6909.test_erc6909_transfer`, `TestERC6909.test_erc6909_approve_and_allowance`, `TestERC6909.test_erc6909_set_operator`, `TestSwap.test_swap_exact_input_sign_convention`, `TestSwap.test_swap_zero_amount_reverts`, `TestModifyLiquidity.test_modify_liquidity_requires_unlock`, `TestModifyLiquidity.test_modify_liquidity_requires_initialized_pool`, `TestDonate.test_donate_requires_unlock`, `TestUpdateDynamicLPFee.test_update_dynamic_lp_fee_only_hook`, `TestUpdateDynamicLPFee.test_update_dynamic_lp_fee_works_outside_unlock`, `TestTake.test_take_native_transfers_eth`, `TestTake.test_take_requires_unlock`
- `test_initialize_and_opcodes.py` (5): `TestL1SendEthDocstrings.test_send_eth_opcode`, `TestL1SendEthDocstrings.test_send_eth_all_opcode`, `TestL2Initialize.test_initialize_rejects_non_owner`, `TestL2Initialize.test_initialize_rejects_wrong_value`, `TestL2Initialize.test_initialize_one_wei_succeeds_and_strands_no_eth`
- `test_no_fe_prefix.py` (2): `TestM4NoFePrefix.test_fe_prefixed_stream_reverts`, `TestM4NoFePrefix.test_ff_only_reverts_empty_execution`
- `test_pm_as_bank.py` (11): `TestPMBankV2V2V2.test_mint`, `TestPMBankV2V2V2.test_take`, `TestPMBankV3V2V2.test_mint`, `TestPMBankV3V2V2.test_take`, `TestPMBankV2V3V2.test_mint`, `TestPMBankV2V3V2.test_take`, `TestPMBankV2V2V3.test_mint_impossible`, `TestPMBankV2V2V3.test_take_physical`, `TestPMBankV3V3V2.test_mint`, `TestPMBankV3V3V2.test_take`, `test_gas_comparison_table`
- `test_preprocess_addr_count.py` (2): `TestL4PreprocessAddrCount.test_overflow_set_addresses_revert`, `TestL4PreprocessAddrCount.test_max_set_addresses_accepted`
- `test_profit_guard_m1_m2.py` (2): `test_bribe_too_high_bips_reverts`, `test_zero_min_profit_is_legal_for_no_prefund_execution`
- `test_release_metadata.py` (19): `TestDeclaredValues.test_release_id`, `TestDeclaredValues.test_protocol_major`, `TestDeclaredValues.test_call_encoding_version`, `TestDeclaredValues.test_capabilities_hash`, `TestDeclaredValues.test_release_id_is_a_nul_padded_ascii_tag`, `TestGetterShape.test_getter_is_a_zero_input_view_function`, `TestGetterShape.test_capabilities_hash_getter_is_a_zero_input_view_bytes32`, `TestManifestAgreement.test_release_tag`, `TestManifestAgreement.test_schema_version`, `TestManifestAgreement.test_protocol_major`, `TestManifestAgreement.test_call_encoding_version`, `TestManifestAgreement.test_capability_ids`, `TestManifestAgreement.test_capability_scheme`, `TestManifestAgreement.test_capability_commitment_matches_the_source_declaration`, `TestManifestAgreement.test_manifest_commitment_is_reproducible_from_its_ids`, `TestManifestAgreement.test_execute_abi_is_the_declared_one`, `TestManifestAgreement.test_manifest_agrees_with_the_deployed_contract`, `TestDeclaredGrammar.test_declared_opcode_sizes_match_the_encoding_helpers`, `TestDeclaredGrammar.test_declared_config_layout_matches_the_packer`
- `test_sentinel_failclosed.py` (21): `TestM5V4TakeDeltaGuard.test_take_delta_zero_delta_no_op_not_opaque_revert`, `TestM6HookSentinelFailClosed.test_v4_swap_compact_pm_hook_sentinel_reverts`, `TestM6HookSentinelFailClosed.test_v4_swap_compact_weth_hook_sentinel_reverts`, `TestM6HookSentinelFailClosed.test_v4_swap_dynamic_self_hook_sentinel_reverts`, `TestM6HookSentinelFailClosed.test_v4_swap_compact_native_hook_sentinel_allowed`, `TestM7V4SettleDeltaSentinelFailClosed.test_settle_delta_pm_sentinel_reverts`, `TestM7V4SettleDeltaSentinelFailClosed.test_settle_delta_self_sentinel_reverts`, `TestM3BribeRecipientBounds.test_bribe_recipient_idx_32_reverts`, `TestM3BribeRecipientBounds.test_bribe_recipient_idx_sentinel_reverts`, `TestM3BribeRecipientBounds.test_bribe_recipient_idx_31_accepted`, `TestRevertHelpers.test_expect_revert_fails_on_success`, `TestRevertHelpers.test_expect_revert_fails_on_silent_reverted_receipt`, `TestRevertHelpers.test_expect_revert_propagates_unrelated_exception`, `TestRevertHelpers.test_expect_revert_fails_on_wrong_error`, `TestRevertHelpers.test_expect_revert_fails_on_wrong_opcode`, `TestRevertHelpers.test_expect_revert_fails_on_wrong_message`, `TestRevertHelpers.test_expect_accepted_fails_on_guard_revert`, `TestRevertHelpers.test_expect_accepted_fails_on_unidentified_revert`, `TestRevertHelpers.test_expect_accepted_propagates_unrelated_exception`, `TestRevertHelpers.test_expect_accepted_accepts_success`, `TestRevertHelpers.test_expect_accepted_accepts_identified_downstream`
- `test_stream_truncation.py` (2): `TestStreamTruncationBound.test_max_length_stream_of_single_byte_commands_fits_loop_cap`, `TestStreamTruncationBound.test_loop_cap_equals_byte_bound`
- `test_token_sentinels.py` (3): `test_token_sentinels_resolve_without_table_entries`, `test_settle_all_includes_stablecoin_sentinels`, `test_unassigned_reserved_index_reverts`
- `test_v2_configurable_fee.py` (7): `TestUniswapV2Fee.test_v2_swap_calc_uniswap_fee`, `TestPancakeSwapV2Fee.test_v2_swap_calc_pancake_fee`, `TestLowFees.test_v2_swap_calc_fee_10`, `TestLowFees.test_v2_swap_calc_fee_50`, `TestLowFees.test_v2_swap_calc_fee_99`, `TestFeeAffectsOutput.test_lower_fee_produces_more_output`, `TestPancakeCallback.test_pancake_callback_with_fee_25`
- `test_v2_fee_bounds.py` (4): `TestFeeBoundsV2SwapCalc.test_fee_exactly_10000_reverts`, `TestFeeBoundsV2SwapCalc.test_fee_above_10000_reverts`, `TestFeeBoundsV2SwapCalc.test_fee_zero_reverts`, `TestFeeBoundsV2SwapCompact.test_fee_above_10000_reverts`
- `test_v2_fee_denominator_fuzz.py` (7): `TestUniswapV2Fee30.test_get_amount_out_identical`, `TestUniswapV2Fee30.test_get_amount_in_identical`, `TestUniswapV2Fee30.test_k_check_equivalent`, `TestPancakeSwapFee25.test_get_amount_out_old_gives_more`, `TestPancakeSwapFee25.test_get_amount_in_old_gives_less`, `TestPancakeSwapFee25.test_new_k_pass_implies_old_k_pass`, `TestSubOnePercentFees.test_old_charges_zero_fee_sub10bps`
- `test_v2_swap_calc_excess.py` (7): `TestV2DirectExcess.test_v2_direct_excess_balance`, `TestV2DirectExcess.test_v2_direct_excess_balance_reverts_without_deposit`, `TestV2DirectExcess.test_v2_direct_no_callback_fires`, `TestV4ToV2DirectCustody.test_v4_v2_direct_custody_take`, `TestV4ToV2DirectCustody.test_v4_v2_direct_custody_take_delta`, `TestV2ToV2DirectCustody.test_v2_v2_direct_custody`, `TestExcessBalanceWithMultipleDeposits.test_multiple_deposits_create_excess`
- `test_v2_swap_calc_overflow.py` (1): `TestL6V2SwapCalcOverflow.test_normal_v2_swap_calc_unaffected`
- `test_v2_swap_compact_fee.py` (4): `TestUniswapV2Fee.test_v2_swap_compact_uniswap_fee`, `TestPancakeSwapV2Fee.test_v2_swap_compact_pancake_fee`, `TestFeeAffectsAutoPay.test_lower_fee_pays_less_input`, `TestMixedFeeV2V2.test_mixed_fee_v2v2_path`
- `test_v3_debug.py` (1): `test_v3c_zfo`
- `test_v3_libraries.py` (53): `TestFullMath.test_mul_div_simple`, `TestFullMath.test_mul_div_simple2`, `TestFullMath.test_mul_div_exact`, `TestFullMath.test_mul_div_truncation`, `TestFullMath.test_mul_div_phantom_overflow_u128`, `TestFullMath.test_mul_div_phantom_overflow_pow2`, `TestFullMath.test_mul_div_phantom_overflow_near_max`, `TestFullMath.test_mul_div_phantom_overflow_u255`, `TestFullMath.test_mul_div_one_max_one`, `TestFullMath.test_mul_div_denominator_one`, `TestFullMath.test_mul_div_zero_numerator`, `TestFullMath.test_mul_div_zero_denominator_reverts`, `TestFullMath.test_mul_div_rounding_up_simple`, `TestFullMath.test_mul_div_rounding_up_exact`, `TestFullMath.test_mul_div_rounding_up_truncates`, `TestUnsafeMath.test_div_rounding_up_exact`, `TestUnsafeMath.test_div_rounding_up_truncates`, `TestUnsafeMath.test_div_rounding_up_one_over`, `TestUnsafeMath.test_div_rounding_up_zero`, `TestUnsafeMath.test_div_rounding_up_large`, `TestSafeCast.test_to_uint160_small`, `TestSafeCast.test_to_uint160_max`, `TestSafeCast.test_to_uint160_overflow_reverts`, `TestSafeCast.test_to_int128_positive`, `TestSafeCast.test_to_int128_negative`, `TestSafeCast.test_to_int128_max`, `TestSafeCast.test_to_int128_min`, `TestSafeCast.test_to_int128_overflow_reverts`, `TestSafeCast.test_to_int256_small`, `TestSafeCast.test_to_int256_positive`, `TestSafeCast.test_to_int256_max`, `TestSafeCast.test_to_int256_overflow_reverts`, `TestSqrtPriceMath.test_get_next_sqrt_price_from_input_zfo`, `TestSqrtPriceMath.test_get_next_sqrt_price_from_input_not_zfo`, `TestSqrtPriceMath.test_get_next_sqrt_price_from_input_zero_amount`, `TestSqrtPriceMath.test_get_next_sqrt_price_from_output_zfo`, `TestSqrtPriceMath.test_get_next_sqrt_price_from_output_not_zfo`, `TestSqrtPriceMath.test_get_amount0_delta_round_up`, `TestSqrtPriceMath.test_get_amount0_delta_round_down`, `TestSqrtPriceMath.test_get_amount1_delta_round_up`, `TestSqrtPriceMath.test_get_amount1_delta_round_down`, `TestSqrtPriceMath.test_get_amount0_delta_reversed_inputs`, `TestSqrtPriceMath.test_get_amount1_delta_reversed_inputs`, `TestTickMath.test_get_sqrt_ratio_at_tick_zero`, `TestTickMath.test_get_sqrt_ratio_at_tick_one`, `TestTickMath.test_get_sqrt_ratio_at_tick_100`, `TestTickMath.test_get_sqrt_ratio_at_tick_100000`, `TestTickMath.test_get_tick_at_sqrt_ratio_q96`, `TestTickMath.test_get_tick_at_sqrt_ratio_min`, `TestTickMath.test_round_trip_tick_100`, `TestTickMath.test_round_trip_tick_1000`, `TestTickMath.test_round_trip_tick_negative`, `TestTickMath.test_get_sqrt_ratio_at_tick_reverts_past_max`
- `test_v3_library_fuzz.py` (24): `TestFullMathFuzz.test_mul_div`, `TestFullMathFuzz.test_mul_div_rounding_up`, `TestSqrtPriceMathFuzz.test_get_next_sqrt_price_from_input`, `TestSqrtPriceMathFuzz.test_get_next_sqrt_price_from_output`, `TestSqrtPriceMathFuzz.test_get_amount0_delta`, `TestSqrtPriceMathFuzz.test_get_amount1_delta`, `TestTickMathFuzz.test_get_sqrt_ratio_at_tick`, `TestTickMathFuzz.test_get_tick_at_sqrt_ratio`, `TestTickMathFuzz.test_round_trip`, `TestSwapMathFuzz.test_compute_swap_step`, `TestRevertParityHelpers.test_equal_results_are_parity`, `TestRevertParityHelpers.test_result_mismatch_fails`, `TestRevertParityHelpers.test_result_vs_revert_fails`, `TestRevertParityHelpers.test_revert_vs_result_fails`, `TestRevertParityHelpers.test_matching_revert_messages_are_parity`, `TestRevertParityHelpers.test_bare_reverts_are_parity`, `TestRevertParityHelpers.test_annotated_revert_matches_bare_revert`, `TestRevertParityHelpers.test_different_revert_messages_fail`, `TestRevertParityHelpers.test_matching_custom_errors_are_parity`, `TestRevertParityHelpers.test_different_custom_error_fields_fail`, `TestRevertParityHelpers.test_different_custom_error_names_fail`, `TestRevertParityHelpers.test_unrelated_exception_on_vyper_side_propagates`, `TestRevertParityHelpers.test_unrelated_exception_on_solidity_side_propagates`, `TestRevertParityHelpers.test_missing_method_propagates`
- `test_v3_pool.py` (2): `TestV3PoolBasic.test_initialize_and_liquidity`, `TestV3PoolBasic.test_swap_weth_for_usdc`
- `test_v3_v4_v3_residual_regression.py` (2): `TestV3V4V3ResidualRegression.test_old_static_encoder_reverts_on_v4_overproduction`, `TestV3V4V3ResidualRegression.test_new_dynamic_encoder_succeeds_on_v4_overproduction`
- `test_withdraw_reentrancy.py` (2): `TestWithdrawReentrancyGuard.test_withdraw_reentry_reverts`, `TestWithdrawReentrancyGuard.test_withdraw_honest_path_unaffected`
- `test_withdrawal.py` (9): `TestWithdrawERC6909USDC.test_withdraw_erc6909_usdc_two_tx`, `TestWithdrawERC6909USDC.test_withdraw_erc6909_usdc_single_tx`, `TestWithdrawERC6909NativeETH.test_withdraw_erc6909_native_eth`, `TestWithdrawERC20.test_withdraw_erc20_xfer_balance`, `TestWithdrawERC20.test_withdraw_erc20_transfer_specific_amount`, `TestWithdrawETH.test_send_eth_specific_amount`, `TestWithdrawETH.test_send_eth_all`, `TestFullRoundTrip.test_full_round_trip_usdc_via_erc6909`, `TestFullRoundTrip.test_full_round_trip_native_eth_via_erc6909`

## Row-update protocol

Each port task flips its rows' Status to "ported (green)" only after its scoped gate
passes, appends any new disposition decisions discovered mid-port, and keeps this
manifest the single migration checklist. Final acceptance (LSUERW) requires every
module row retired, the 410-case census reconciled against the Foundry lanes (381
offline + 29 fork collected cases at baseline), and all five known-red parity cases
ported and PASSING against a validated archive fork with real PoolManager bytecode
at the expected height — the reth archive node is part of the standing environment
contract, and LEZT6O's fork lane must be green. Fail-closed behavior applies when an
endpoint is invalid or unavailable (probe/diagnose and stop loudly), not as a
substitute for passing the parity cases at final acceptance.
