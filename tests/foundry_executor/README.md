# Executor contract test suite

The canonical test lane for the Vyper executor contracts in `contracts/vyper/`.
It compiles those sources with the root project's pinned Vyper, hands
forge-ready artifacts to the Solidity suites, and runs them through the justfile
(never `forge test` directly — the artifacts and the generated interfaces are
build outputs). `PORTING.md` is the migration record: it names the retired
Python module behind each Solidity file and preserves the measured baselines.

The suites split by the resource they need. The ordinary tree is every suite
that needs neither FFI nor a node; the trusted production-calldata suite needs
`--ffi` and the Rust encoder; the PoolManager parity suite needs a mainnet
archive fork; the access-list benchmark needs a node and is on demand only.

```bash
just test-executor                                    # full lane: tree + production calldata + parity (492 tests)
just test-executor-offline                            # tree + production calldata, no node (459 tests)
just test-executor-tree                               # ordinary tree only (427 tests)
just test-executor-tree --match-path 'test/V2V3.t.sol' # one file
just test-executor-tree --match-test testV2V3 -vvvv   # one test, traced
just test-executor-production-calldata                # trusted FFI lane (32 tests)
just test-executor-pool-manager-parity                # fake-vs-real PoolManager fork lane (33 tests)
just test-executor-access-list                        # optional RPC access-list lane (2 tests)
just test-executor-evidence-snapshot                  # regenerate .gas-snapshot-evidence
just test-executor-evidence-snapshot-check            # drift gate on that file
just test-executor-artifacts                          # artifacts + interfaces only
just py python tests/foundry_executor/build_artifacts.py --check-release  # compile identity
```

Every lane recipe runs `test-executor-artifacts` first, which compiles the Vyper
sources with `--check-release`: the lane is red when the executor bytecode it
deploys is not the tracked release bytecode
(`rust/crates/foundation/degenbot-executor/releases/v2`). Each lane recipe also
captures Forge's summary and `scripts/forge_lane_summary.sh` requires at least
one executed test and zero skipped ones. That guard is load-bearing: a
specialized suite `vm.skip`s itself when its lane switch is absent, and Forge
exits 0 for both a skipped contract and a filter that matched nothing, so the
exit code alone would report green over no coverage.

`just test-executor` probes the archive endpoint before any lane runs and fails
closed when the machine declares none. `just test-executor-offline` is the lane
CI runs and the executor dependency of the default `just test`; the node-backed
parity suite is its only omission, and it is never part of the default gate.

### Trusted FFI lane (production calldata)

`test/CmdExecutorThreeHopProductionCalldata.t.sol` executes the bytes the
production Rust composer emits, so it needs two things the ordinary tree must
never provide: `--ffi` and the `encode_three_hop` Cargo binary. Its own recipe
builds that binary (`just build-executor-three-hop-encoder`) and runs only that
suite:

```bash
just test-executor-production-calldata                          # encoder + 32 tests
just test-executor-production-calldata --match-test testV2V4V4  # one row
just test-executor-production-snapshot                               # regenerate .gas-snapshot-production
just test-executor-production-snapshot-check                         # drift gate on that file
```

### Fork lane (fake-vs-real PoolManager parity)

`test/PoolManagerParity.t.sol` is the parity tier for the fake Vyper
`PoolManager`: it deploys the fake beside the DEPLOYED mainnet manager
(`0x000000000004444c5dc75cB358380D2e3dE08A90`, fork block 22,000,000) and asserts
the same call on both, reading each manager's own transient slots and pool state
through `exttload`/`extsload` instead of a model of them. `ExttloadComparator`
supplies the inside-callback real-vs-fake delta, lock and nonzero-delta-count
reads. Every negative case pins BOTH managers' identities — the deployed
manager's exact custom error and the fake's exact `Error(string)` — so a parity
case never passes on a shared "both reverted".

The lane is on demand for the same reason the FFI lane is: it needs a resource the
offline gate must never dial. `DEGENBOT_POOL_MANAGER_PARITY_FORK` is set only by
`just test-executor-pool-manager-parity`. The ordinary tree excludes the suite
instead of letting it skip, so `just test-executor-tree` runs 427 tests and never
dials a node.

The recipe resolves `TEST_ETHEREUM_ARCHIVE_NODE_HTTP_URI` through the declared
layers `test-fork` probes (process env > `tests.env`), probes `eth_chainId`
(chain 1) and `web3_clientVersion` BEFORE Forge starts, and fails closed on a
missing or unreachable endpoint. The suite repeats the check that matters for its
own claims: `REAL_PM` must have code on the fork, because a parity assertion
against an empty account certifies nothing.

```bash
just test-executor-pool-manager-parity                            # 33 tests
just test-executor-pool-manager-parity --match-test testTakeNativeTransfersEth -vvvv
```

### Gas evidence (per-variant comparisons)

The comparison figures are committed evidence rather than printed output:
direct-vs-calc, the three three-pool approaches, bribe overhead, ERC6909
MINT-vs-TAKE, the four V3/V4 three-hop approaches and the PM-bank MINT/TAKE
table. Each measured ARM is its own `testGasEvidence…` test, run from the same
pristine fixture as its siblings, so the committed snapshot key names the
variant:

```bash
just test-executor-evidence-snapshot        # write .gas-snapshot-evidence (21 labels)
just test-executor-evidence-snapshot-check  # fail if any figure or label drifted
```

The comparison tests (`testGasComparison…`, `testMintVsTake…`,
`testBribeAddress…`) stay and keep their cross-variant assertions — the snapshot
cannot see two figures in one test, so it carries the arms and the comparisons
carry the relations. The file is written only by `forge snapshot`; a plain
`forge test` never rewrites it, which is what makes `--check` a drift gate
against the committed figures rather than against the last local run.

The other two matrices keep their own files: `.gas-snapshot` (optimized 27-row
matrix) and `.gas-snapshot-production` (production-calldata 27-row matrix).

### Optional access-list lane (EIP-2930)

`test/AccessListBenchmark.t.sol` estimates ONE real mainnet call object twice —
bare, then with exactly the access list `eth_createAccessList` returned — at one
pinned block, and reports both figures:

```bash
just test-executor-access-list   # 2 tests against the declared archive node
```

The lane is isolated the same way the fork lane is: `DEGENBOT_ACCESS_LIST_RPC`
is set only by that recipe, the endpoint is resolved through `tests/conftest.py`'s
declared layers and probed (`eth_chainId` must be 1, `web3_clientVersion` must
answer) BEFORE Forge runs, and a missing or unreachable endpoint fails closed.
Neither canonical gate includes this suite; the summary guard refuses a run that
skipped it.

The measured subject is a canonical `SwapRouter02` swap, not the executor's own
stream, and the suite's contract doc says why: a Foundry test's contracts exist
only in Forge's in-process EVM, and Forge's local EVM never applies EIP-2930
intrinsic gas — so no local run can produce a with/without-access-list pair at
all. Both figures come back from the node on the run; nothing is hardcoded.

The 27 permutation rows carry their own committed gas snapshot,
`tests/foundry_executor/.gas-snapshot-production` — one stable label per row,
generated and drift-checked by `just test-executor-production-snapshot[-check]`
(encoder build, lane switch, `--ffi`, and `--match-test 'testV'` so the five
`testEncoder…` refusal cases never enter the file). It is a separate file from
`.gas-snapshot` because
the production streams are not the optimized matrix's bytes: V2-V2-V2 is
self-funded and V3-V2-V4 runs its forward-flash/final-repayment arm (both one
transfer wider), and V4-V2-V4 nets differently. The check fails closed even if
the suite would skip — a missing lane switch surfaces as a missing snapshot
entry, never a hollow pass.

The suite takes its lane switch from `DEGENBOT_THREE_HOP_PRODUCTION_FFI` (set
only by that recipe) and `vm.skip`s the whole contract otherwise; the ordinary
tree excludes the suite, so it never shells out. The encoder path resolves the
Cargo target dir the way the justfile does
(`CARGO_TARGET_DIR`, else `rust/target`) and reads
`<target-dir>/debug/encode_three_hop` from the repository root. A valid-hex
stdout is hex-decoded by the cheatcode, so the encoder's one hex line arrives in
the test as the raw command stream to execute verbatim.

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

### Canonical Uniswap bytecode (one-off compiles)

The same "does the toolchain need to be in the loop?" logic applies to Solidity:
`v2-core` is `=0.5.16`, `v3-core` is `=0.7.6` and **forge's solc manager cannot
resolve pre-0.8 versions in this environment** — the repository's own harness
scripts already say so and fetch binaries directly. The v4-core `PoolManager`
adds a second reason not to hand it to forge: it pins `pragma solidity 0.8.26`,
which the lane's own solc (0.8.28) cannot compile, and it needs EIP-1153
transient storage. Nothing is lost: `build_solidity_artifacts.py` fetches the
pinned binaries into the same svm cache the harness scripts use and drives them
through `py-solc-x`'s standard-JSON interface, emitting the same artifact shape
this lane deploys. Its stamp records a per-artifact hash next to the source
digest, so a stale or corrupted artifact (or an old/malformed stamp) reads as
cold and forces one deterministic recompile. `test/RealLibraries.t.sol` then
runs the executor against canonical `UniswapV2Pair` (behind the real
`UniswapV2Factory`), a canonical `UniswapV3Pool` (created by the real
`UniswapV3Factory`, with real tick/position liquidity and the executor's V3
auto-pay path), and the canonical v4-core `PoolManager` (solc 0.8.26, EVM
`cancun`, built from `tier3-oracle/lib/v4-core`).

The V4 half of that file is also the lane's v4 router: the harness implements
`unlockCallback` itself and reads the manager's own transient storage
(`exttload`/`extsload` slot identities from v4-core's `Lock`,
`NonzeroDeltaCount`, `CurrencyDelta` and `CurrencyReserves`), so unlock,
`modifyLiquidity` delta accounting, `take`, `sync`/`settle`, ERC6909
`mint`/`burn` and one executor-driven swap are asserted against the manager's
state rather than against a parallel model of it. The compile's identity
decisions — source tree, solc, EVM fork, optimizer, remappings — are stated in
the builder's docstring and in `PORTING.md`.

## Layout

- `build_artifacts.py` — pinned vyper in-process (`vyper.compile_code`) → `artifacts/<Contract>.json` (+ manifest with input-tree, source, bytecode, and whole-artifact hashes), plus the generated revert vocabulary `src/generated/Errors.sol`: custom-error selectors computed from the built `CmdExecutor` ABI, with the compiler revert-label vocabulary taken from the tracked release `cmd_executor.error_map.json` and cross-checked against the fresh compile on every rebuild (an unknown label or a vocabulary divergence fails the build loudly).
- `build_solidity_artifacts.py` — pinned solc 0.5.16 / 0.7.6 / 0.8.26 binaries, driven through `py-solc-x` → the same artifact shape for canonical `UniswapV2Factory`/`UniswapV2Pair`/`UniswapV3Factory`/`UniswapV3Pool`, the v4-core `PoolManager` (solc 0.8.26, EVM `cancun`, from `tier3-oracle/lib/v4-core`), and the committed solc-0.7.6 `SolReference` (the original Solidity V3-math libraries, byte-identical to its committed predecessor modulo its metadata hash, for the differential library ports). Its module docstring carries the V4 compile's source/compiler/fork/optimizer/remapping decisions.
- `gen_interfaces.py` — ABI → `src/generated/I<Contract>.sol` + tuple structs in `Types.sol`.
- `src/Vm.sol` — minimal cheatcode interface (no forge-std dependency), carrying the declarations the FFI (`vm.ffi`, `vm.tryFfi` with its `FfiResult` exit-code/stdout/stderr struct, and `vm.skip` as the specialized-lane control), RPC/fork (`vm.rpc`, `createFork`/`selectFork`), JSON (`parseJson*`), and hex (`toString`) lanes need — and nothing speculative beyond those consumers.
- `src/TestBase.sol` — artifact deployment, `execute` helpers, assertions, log/transfer accounting, balance snapshots with conservation diffs (`_snapshotBalances` + `_assert*Conserved`/`_assertExecutorProfit`), exact revert assertions over the generated `ExecutorErrors` vocabulary, and the `ExecutorFixture` that mirrors the retired fixture graph (deployment, funding, and receipt/transfer accounting).
- `src/ConservationOps.sol` — the composed tracked-account conservation check (the port of `test_conservation.py`'s `_verify_conservation`): tokens[0] = WETH combined with NATIVE in the mode-1 profit domain, every other tracked token summed to zero, optional executor-profit assertion run first.
- `src/Cmd.sol` — the command-stream encoder: the port of `conftest_shared.py`'s `enc_*` helpers and the `AddressTable` sentinel resolution (including the raw `0x30` V3 form and `V4_BATCH_OPEN_WETH`).

- `test/EventTopicFidelity.t.sol` — the fake-vs-canonical topic0 tier. `test/ArtifactIdentity.t.sol` — the in-suite artifact-identity tier (the build step's `--check-release` re-asserted from the files on disk). `test/AccessListBenchmark.t.sol` — the optional RPC access-list lane; `.gas-snapshot-evidence` next to this file is its offline sibling: the per-variant gas evidence.
- `src/Setup.sol` — pool setup: the port of `_setup_v2_pair`, `_setup_v3`, `_setup_v4_swap`, `v2_get_amount_out`.
- `test/*.t.sol` — ported test files (each names the Python file it ports), plus `test/HarnessPrimitives.t.sol`, the named tests for the shared substrate primitives themselves (encoding bytes/lengths, the generated error vocabulary, conservation diffs, exact-revert negative controls, and the artifact additions).
- `foundry.toml` — the lane's forge project config. Its `fs_permissions` grants read on `./artifacts` (the lane's build output) and on the committed release directory under `rust/crates/foundation/degenbot-executor/releases/v2`, which `test/ReleaseMetadata.t.sol` reads as its authority: the release ABI, method-identifier table, v2 manifest and pinned Vyper source are read where they are committed, never copied into the lane.

## Coverage (migration table)

The left column names the retired Python module each Solidity file was ported
from; `PORTING.md` carries the per-case evidence and dispositions. Rows whose
left column is "—" are additions made directly in this lane.

| Retired Python module | Solidity file | Python tests | Ported tests |
| --- | --- | --- | --- |
| `test_initialize_and_opcodes.py` | `InitializeAndOpcodes.t.sol` | 5 | 5 |
| `test_no_fe_prefix.py` | `NoFePrefix.t.sol` | 2 | 2 |
| `test_stream_truncation.py` | `StreamTruncation.t.sol` | 2 | 2 |
| `test_profit_guard_m1_m2.py` | `ProfitGuard.t.sol` | 2 | 2 |
| `test_token_sentinels.py` | `TokenSentinels.t.sol` | 5 (parametrized) | 5 |
| `test_preprocess_addr_count.py` | `PreprocessAddrCount.t.sol` | 2 | 4 (288-byte accept and 289-byte ABI rejection added) |
| `test_cmd_executor_edge_cases.py` | `EdgeCases.t.sol` | 2 | 2 |
| `test_execution_receipt.py` | `ExecutionReceipt.t.sol` | 12 (parametrized) | 13 (12 source + 1 added receipt boundary: a physical-balance-capped bribe reports the ACTUAL paid amount — `bribe_paid` = the seeded physical balance, drained to zero, with the retained profit left as ERC6909 claims) |
| `test_events.py` | `Events.t.sol` | 6 | 11 (6 source + 5 documented event-surface additions on the withdrawal exit paths: V4 take counted exactly once with no ERC6909 topic, ERC6909 mint logged exactly once, the burn/take/xfer-balance exit surface, the native-ETH exit's single-ERC6909 surface, SEND_ETH_ALL's zero-event surface). The topic0 identities those counts rely on are checked against REAL canonical emissions in `EventTopicFidelity.t.sol` (below) |
| `test_withdraw_reentrancy.py` | `WithdrawReentrancy.t.sol` | 2 | 3 (withdraw owner-only added) |
| `test_donation_profit_inflation.py` | `DonationProfitInflation.t.sol` | 1 (+1 skipped) | 1 |
| `test_sentinel_failclosed.py` | `SentinelFailClosed.t.sol` | 21 (11 helper meta-tests) | 21 |
| `test_v3_libraries.py` | `V3Libraries.t.sol` | 53 | 53 |
| `test_v3_pool.py` | `V3Pool.t.sol` | 2 | 2 |
| `test_v3_debug.py` | `V3Debug.t.sol` | 1 | 1 |
| `test_v3_library_fuzz.py` | `V3LibraryFuzz.t.sol` | 24 (10 differential + 14 parity-helper meta) | 24 |
| `test_v2_fee_denominator_fuzz.py` | `V2FeeDenominatorFuzz.t.sol` | 7 | 7 |
| `test_v2_configurable_fee.py` | `V2ConfigurableFee.t.sol` | 7 | 7 |
| `test_v2_fee_bounds.py` | `V2FeeBounds.t.sol` | 4 | 7 (the four source cases now pin the identity that actually fires — the pair's `INSUFFICIENT_LIQUIDITY` for the compact command, the empty-data excess-balance underflow for the three calc cases on the source's unfunded pair — plus three funded cases that reach the fee math: fee=0 EXECUTES as a fee-free swap, fee=10000 computes a zero output the pair refuses with its exact `INSUFFICIENT_OUTPUT_AMOUNT`, fee=20000 hits the checked multiplication with the empty-data revert and delivers nothing) |
| `test_v2_swap_calc_excess.py` | `V2SwapCalcExcess.t.sol` | 7 | 7 |
| `test_v2_swap_compact_fee.py` | `V2SwapCompactFee.t.sol` | 4 | 4 |
| `test_cmd_executor_v2_swap_direct.py` | `CmdExecutorV2SwapDirect.t.sol` | 8 | 10 (incl. the named V2-V4-V4 DIRECT-vs-CALC comparison and the two per-arm `testGasEvidenceV2V4V4Calc`/`…Direct` cases whose labels are the committed `.gas-snapshot-evidence` entries) |
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
| `test_erc6909.py` | `ERC6909.t.sol` | 5 | 7 (view surface, V4_MINT_COMPACT entry, two-phase V4_BURN_COMPACT settlement with the exact WETH take, the mint-vs-take comparison as one self-contained state-snapshot-isolated case — disposition 11 — and the two arms measured separately as `testGasEvidenceErc6909Take`/`…Mint` for their committed snapshot labels; all executions keep the source's `config = 2`) |
| `test_erc6909_composability.py` | `ERC6909Composability.t.sol` | 6 (7 expanded) | 8 (7 source + 1 added direct case: V4_BURN funding a residual -1 WETH PM debit on a V4→V2→V2 borrow — the no-burn negative control pins the PM's exact `CurrencyNotSettled:<weth>:-1e18` and the paying run settles it with zero ERC20 transfers and exact balance deltas; bribe receipt pinned from the simulated `(gross_profit, bribe_paid)`; warmup-wei accounting exact on every ERC6909 WETH expectation) |
| `test_pm_as_bank.py` | `PMAsBank.t.sol` | 11 | 19 (four V2-ending paths × MINT/TAKE with exact profit pins, V2-V2-V3 as MINT-impossible — pinned to the executor's exact `InsufficientMintDelta(0, 1)` — plus TAKE-physical with the source's 5-transfer count, the ordering-dependent `_gas_results` summary replaced by one self-contained `testMintVsTakeGasComparison` that runs every path's MINT and TAKE from the same pristine state — disposition 6 — and eight `testGasEvidencePmBank<Path><Mode>` tests that give each path × mode its own committed snapshot label) |
| `test_cmd_executor_bribe_commands.py` | `CmdExecutorBribeCommands.t.sol` | 5 | 8 (5 source + 2 per-variant `testGasEvidenceBribe…` cases for the committed baseline/bribed labels + 1 added boundary: zero gross profit pays zero regardless of bips — receipt `(0, 0)` exact for bips {1, 5000, 10000} with recipient untouched; coinbase cases pin `vm.coinbase` to a dedicated funded address and assert its exact delta — the lane's forge EVM credits the beneficiary nothing at gasprice 0/basefee 0; auto-withdraw pinned to the wei; the named `BribeGasOverhead` case is snapshot-isolated so both runs share byte-identical state) |
| `test_cmd_executor_bribe_transfer.py` | `CmdExecutorBribeTransfer.t.sol` | 4 | 4 (two-transaction native-ETH ERC6909 funding: TX1 WETH→native swap + V4_MINT_COMPACT at the source's config = 2, TX2 V4_BURN_COMPACT + V4_TAKE_DELTA; every bribe pinned to the wei via the source's `profit = combined delta + bribe` identity, TX2's recipient table entry kept at index ≥ 1 because config idx 0 is the coinbase recipient) |
| `test_v2_swap_calc_overflow.py` | `V2SwapCalcOverflow.t.sol` | 1 (permanently skipped) | 2 (executable checked-mul boundary + normal-path control; skip replaced) |
| `test_cmd_executor_three_hop_optimized.py` | `CmdExecutorThreeHopOptimized.t.sol` | 27 | 27 (one test per permutation, so the test name is the row label: `testV2V2V2` … `testV4V4V4`; exact transfer count per row — V4V4V4=1, V2V4V4/V3V4V4/V4V2V4/V4V3V4/V4V4V2/V4V4V3=3, all others=4 — plus tracked-account conservation through `ConservationOps`; `testV4V4V4` captures the net profit as an ERC6909 claim under the source's `profit_mode = 2`; per-row gas lives in the committed `.gas-snapshot`, generated by `just test-executor-snapshot` and drift-gated by `just test-executor-snapshot-check` — disposition 11) |
| `test_cmd_executor_three_hop_production_calldata.py` | `CmdExecutorThreeHopProductionCalldata.t.sol` | 6 (32 collected) | 32 (27 permutations, one test per row so the test name is the row label, plus the five encoder CLI refusals; the streams come from the Rust `encode_three_hop` composer through `vm.ffi` and are executed verbatim — nothing is rebuilt with `Cmd.sol`; the source's production transfer counts are kept exactly — V4V4V4=1, V2V4V4/V3V4V4/V4V3V4/V4V4V2/V4V4V3=3, V2V2V2/V3V2V4=5, all others=4 — with tracked-account conservation over the deduplicated set (the source snapshots into a `(token, account)`-keyed mapping, so a repeated PoolManager collapses to one entry) and `profit_mode = 2` on the V4V4V4 MINT-capture row; the five refusals pin exit code 1, empty stdout and the exact stderr text through `vm.tryFfi`; trusted FFI lane only; per-row gas lives in the committed `.gas-snapshot-production`, generated by `just test-executor-production-snapshot` and drift-gated by `just test-executor-production-snapshot-check` — disposition 11) |
| `test_release_metadata.py` | `ReleaseMetadata.t.sol` | 19 (22 expanded) | 23 (22 source cases across four contracts — declared values, getter shape, manifest agreement, declared grammar — plus the added executor-wide ABI-shape case: the committed ABI's complete function/selector set including both `execute` overloads, parameter and return types, constructor/fallback shape, and the 10 error selectors round-tripped through the generated vocabulary; every selector cross-checked against the release's `method_identifiers.json`; the capability commitment recomputed as `keccak256(scheme ++ abi.encode(uint32[] ids))` and cross-checked against the source `CAPABILITY_*` constants, the manifest IDs and its commitment; all 31 declared opcodes sampled — raw V3 `0x30`, direction-baked `0x33`/`0x34`, `V4_BATCH_OPEN_WETH` `0x43` — with opcode identity and declared byte width; config fields tile bits 0-255 with the packed values pinned; manifest/source/ABI drift fails with the opcode, field, capability index or signature named) |
| — (gap filled, not a port) | `AbiOverloads.t.sol` | 0 | 3 |
| `test_fake_abi_fidelity.py` | `FakeABIFidelity.t.sol` | 6 (9 collected) | 9 (one named test per collected case: three V2 callback variants, two V3 variants, and the four V4 oversized-`bytes` cases; every callback case pins the intentional `ECHO_VERIFIED` payload exactly, with a corrupted-digest `ECHO_BAD_DIGEST` control proving the marker is only reachable after verification) |
| `test_fake_pool_manager_parity.py` | `PoolManagerParity.t.sol` (fork lane) | 30 defs / 29 collected | 33 (29 ported + 4 added: the shadowed `test_double_unlock_reverts` intention as a real nested-unlock `AlreadyUnlocked`, the empty `test_swap_zero_amount_reverts` as an executed `SwapAmountCannotBeZero`, the guard-only clear case as a real exact-positive-delta clear, and the hook-caller dynamic-fee update the retired suite could not impersonate; every negative case pins both managers' own revert identity; on-demand lane, excluded by the ordinary tree) |
| — (canonical Uniswap bytecode) | `RealLibraries.t.sol` | 0 | 8 (2 V2/V3 + 6 canonical v4-core `PoolManager`: unlock round trip and nested-unlock refusal, `modifyLiquidity` delta accounting via `exttload`, take + `sync`/`settle` reserves snapshot, ERC6909 mint and burn, and one executor-driven swap read back from the manager's `Swap` event) |
| — (harness substrate primitives, QXIGKC) | `HarnessPrimitives.t.sol` | 0 | 22 |

| — (fake-vs-canonical event topics, Q3UOC6) | `EventTopicFidelity.t.sol` | 0 | 9 (V2 `Sync`/`Swap`, V3 `Swap`, V4 `Initialize`/`ModifyLiquidity`/`Donate` and the ERC6909 `Approval`/`OperatorSet` surface compared emission-to-emission against the canonical artifacts through one shared unlock callback, plus the fake's V4 `Swap` and mint `Transfer` pinned to the canonical declarations) |

| — (in-suite artifact identity, Q3UOC6) | `ArtifactIdentity.t.sol` | 0 | 3 (every artifact hashes to its manifest entry; the compiled executor hashes to the committed release `.hex` files; the deployed executor is the artifact runtime plus the documented immutable section) |

| — (optional RPC access-list lane) | `AccessListBenchmark.t.sol` | 0 | 2 (node identity; the same mainnet call estimated bare and with the access list `eth_createAccessList` returned, at one pinned block — on demand only) |

Not ported: the Rust-artifact cross-checks.

## Porting rules this lane encodes

- **The test contract is the owner EOA.** Every owner action is a plain call, so
  `msg.sender` is the owner at the top level and the executor inside callbacks —
  the same call graph an owner EOA produces. `vm.prank` is used
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
  keep read-only execution semantics: a failed simulation leaves no state behind.
- **Release identity is read from the committed tree, not the lane's build
  output.** `test/ReleaseMetadata.t.sol` reads the release ABI, method-identifier
  table, v2 manifest and pinned Vyper source from
  `rust/crates/foundation/degenbot-executor/releases/v2` (read granted in
  `foundry.toml`). The lane's `--check-release` step already proves its fresh
  compile is the released bytecode, so comparing the lane to itself would be
  vacuous; reading the committed files keeps the assertions about what a
  deployment declares.
- **Reverts are pinned, not merely expected.** Wherever the contract exposes a
  revert identity, the port asserts the exact revert data — selector and
  arguments through the generated `ExecutorErrors` vocabulary — instead of a
  bare `assertFalse(ok)`. Reverts that carry no identity (Vyper bounds checks,
  ABI decoder rejections) are pinned as the zero-length payload; the empty
  execution section and the hook sentinels are deliberate empty-data cases,
  and the native hook sentinel's downstream `ManagerLocked` Error(string) is
  asserted as an identified acceptance, distinct from any executor guard.
