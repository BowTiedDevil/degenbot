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
  Its pure-Rust helper is the `degenbot-fixture-tools`
  `encode_three_hop` binary, built only through
  `just build-executor-three-hop-encoder` (which also stages it at
  `rust/target/debug/examples/encode_three_hop`, the path this suite reads).
- `degenbot-simulation/tests/harness_declarative.rs` executes all 27 through
  EVM2 with the real executor bytecode and exact predicted-profit deltas. Its
  `full_three_hop_matrix_executes_production_solver_amounts` test also solves
  every row through the production mixed-path solver, then executes the
  solver-selected input, output, and consumed-input vectors.
- The same rows run under each of the three capital postures the live probe
  offers, through the SAME fixture interface (`FixtureCapital` /
  `ProbeCapital` / `CapitalOverridePlan`). `production` applies no override and
  executes against exactly the holdings the fixture seeded;
  `full_three_hop_self_fund_matrix_executes_from_declared_capital` installs the
  entry each row's built Plan declares (`encode_cmd_stream_capital`) and clears
  the rest; `full_three_hop_zero_balance_matrix_executes_without_token_funding`
  clears every touched asset. No runner entry mints a universal WETH or
  intermediate-token buffer, and each `ChainResult` carries the applied plan plus
  exact initial/final balances for every touched native, ERC-20, and
  PoolManager-claim asset.

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

Run the self-funded and zero-balance posture matrices (plus the exact-balance
snapshot assertions) with:

```bash
just test-rust-nextest -p degenbot-simulation -E 'test(full_three_hop_self_fund_matrix) or test(full_three_hop_zero_balance_matrix) or test(capital)'
```

Fixture capital is DECLARED, never granted: a row that needs a holding seeds it
explicitly (`Harness::fund`) or asks for the posture that declares it. Pool
liquidity, WETH native backing, and V2 pair approvals are fixture setup (how the
fake pools can pay and pull), never executor capital. A raw payload's bytes
carry no Plan to read a requirement from, so a raw `SelfFund` run must name its
`CapitalRequirement`s (`FixtureCapital::self_fund_declared`) — the fixture
refuses to guess an amount from the bytes or the hop list.

The Rust `command_stream_byte_identity_is_pinned` test separately pins every
permutation under 12 amount/option configurations. The self-funded 3-hop
producers that were consolidated onto the shared mechanics primitives keep their
own scoped honesty gate:

```bash
just test-rust-nextest -p degenbot-executor -E 'test(mechanics_honesty)'
```

### Fixed-path bot probe

`scripts/executor_three_hop_matrix.json` names a mainnet WETH/USDC/WBTC
triangle with V2, V3, and V4 pools on every edge. The companion Python tool
injects the forward token cycle and records exact engine path IDs for all 27
protocol rows.

### Bankroll matrix: one command

`just executor-bankroll-matrix` is the canonical way to run both capital
postures. It owns the whole session — one fresh `run_bot.sh start`, ONE
injection of the registration set, both probes against that same set, every
artifact archived, and `run_bot.sh stop` in its `finally` (also on inject/probe
failure, injected failure, and SIGINT/SIGTERM). It refuses a nonempty output
directory and an already-running bot rather than taking over somebody else's
session, and it never reads, accepts, or prints key material.

```bash
just executor-bankroll-matrix -- \
  --owner 0x... \
  --injected-address 0x... \
  --runtime /path/to/cmd_executor_runtime_bytecode.txt \
  --output-dir /path/to/run
```

`--owner`, `--injected-address`, `--runtime`, and `--output-dir` are required.
Optional: `--capital both|self_fund|zero_balance` (default `both`),
`--fixed-input WEI` (default `1000000000000000`), `--chain-id` (default `1`),
`--release-id` (default `cmd_executor/2`), and `--ready-timeout SECONDS`
(default `120`). The posture picks its own expectation: `self_fund` must execute
every row (`--expect executed`), `zero_balance` must refuse every row on capital
policy (`--expect insolvent`), so a selection of one posture still states the
right policy for it.

`--runtime` must be the release's deployed runtime — the bytecode with its
immutable tail already baked in — and `--release-id` must name a release whose
tail layout the runner knows. That is `cmd_executor/2` today: the runner reads
the last seven 32-byte words of the artifact, takes `OWNER_ADDR` from word 0
(low 20 bytes of the word), and compares it with `--owner`, ignoring hex case.
Any other release ID is refused rather than guessed. `execute()` admits the
runtime's own `OWNER_ADDR` alone, so a mismatched `--owner` would revert every
self-funded row; the run is refused before the launcher, the endpoint, or the
output directory is touched.

The tracked artifact bakes the production release owner, and the check above
compares `--owner` with the artifact's own word 0 — so it passes when both are
that owner, while the session still signs with its own key. The composition then
refuses with `OwnerNotSigner`, because the runtime admits a sender the session
cannot be. Bake the probe runtime for the signer instead of using the
production-owner artifact blindly:

```bash
uv run python contracts/recompile.py \
  --owner <session signer> \
  --output-dir /path/to/probe-runtime
```

The bake keeps the accepted code+CBOR and rebinds immutable word 0 only, and it
refuses a custom owner without an explicit output directory — the release
deployment always keeps the release owner. The command, its guards, and the
artifact report are in
[`contracts/README.md`](../../contracts/README.md#baking-a-signer-compatible-probe-runtime-custom-owner).

The run's directory is the evidence envelope:

| Artifact | Content |
| --- | --- |
| `metadata.json` | chain ID, pre-launch block, release ID, runtime and manifest SHA-256, source commit and dirty flag, fixed input, selected capitals, owner/injected addresses, artifact names — written BEFORE the first probe and embedded in every report |
| `run_metadata.json` | the run's outcome, every command's real return code, each posture's exit code and report summary, and the cleanup verdict |
| `registrations.json` | the ONE injected registration set both probes used |
| `report_self_fund.json`, `report_zero_balance.json` | one fail-closed matrix report per selected posture |
| `matrix_inject.log`, `matrix_probe_<capital>.log`, `launcher.log` | the commands' own output, with any RPC URI redacted |
| `bot_run.log` | the bot's own launcher log, archived after `stop` |

Exit codes: `0` — every selected report complete and policy-matched; `1` — a
failed launch/injection/probe, an incomplete report, a policy mismatch, or a
session that left a bot, pidfile, or socket behind; `2` — refused before launch
(unusable argument, a runtime artifact that is missing, not hex, too short for
its immutable tail, or owned by another address, an unsupported `--release-id`,
nonzero high bytes in the owner word, a nonempty output directory, a bot
already running, an unreachable or wrong-chain endpoint); `130` — stopped by
SIGINT/SIGTERM, which is the operator's own outcome and still cleans up.

An operator stop (`^C` / `SIGTERM`) stops the bot, archives what exists, and
records `outcome: "stopped"`: the postures that never ran stay named as
skipped, so an early stop never reads as full coverage.

### Advanced/debug: the manual two-shell sequence

The commands below are the underlying flow, for debugging the launcher or a
single command in isolation. Prefer the one-command recipe above.

Start an operator-only, zero-floor, injected-executor dry run (set the owner,
injected address, and runtime artifact for the release under test):

```bash
export DEGENBOT_INJECT_EXECUTOR_CODE=1
export EXECUTOR_OWNER_ADDRESS=0x...
export INJECTED_EXECUTOR_ADDRESS=0x...
export EXECUTOR_RUNTIME=/path/to/cmd_executor_runtime_bytecode.txt
export DEGENBOT_MIN_PROFIT_WEI=0
export DEGENBOT_MIN_PROFIT_MARGIN_BPS=0
./run_bot.sh foreground -- --skip-discovery --sparse-registration --operator-socket /tmp/degenbot-27.sock
```

From another shell, inject and then render the fail-closed probe report:

```bash
just py python scripts/executor_three_hop_matrix.py inject \
  --socket /tmp/degenbot-27.sock \
  --output logs/executor_solver_sim_27_registrations.json
just py python scripts/executor_three_hop_matrix.py probe \
  --socket /tmp/degenbot-27.sock \
  --registrations logs/executor_solver_sim_27_registrations.json \
  --output logs/executor_solver_sim_27_report.json \
  --expect executed
just py python scripts/executor_three_hop_matrix.py report \
  --log logs/bot_run.log \
  --registrations logs/executor_solver_sim_27_registrations.json \
  --output logs/executor_solver_sim_27_report.json
```

Registration is not simulation coverage: live pools may have no profitable
input. `probe` supplies a small fixed input, quotes through the canonical V3/V4
quoters, and runs each path through the session's dedicated execution-probe
executor. Never add `--live`: the probe executor has no submit lane at all (no
signer, nonce lease, relay, or broadcast), and executor-code injection is what
makes the injected runtime simulable. `--sparse-registration` avoids stale
tracked tick fixtures; it does not relax simulation checks.

#### The probe report is fail-closed

`probe` writes schema version 2 of the named output file. Each answer is the
session's typed `ProbeReport`, projected onto the wire by
`execution_probe_response`; each completed row keeps that response verbatim in
`response` beside the normalized fields — the probed `request_block`, the
`capital` posture, the simulate verdict's own typed label, the raw failure
`bucket` and `revert` bytes, the typed `zero_capital` deficit
(`asset`/`required`/`available`), the encoder's `planning` summary (target,
native value, calldata length) when it composed a row, and the `receipt`
(gross/net profit, gas used) when a receipt-bearing verdict ran.

Every row is classified into exactly one `category`:

| Category | Meaning |
| --- | --- |
| `executed_success` | a committed `profitable` simulation with a complete receipt (gross profit, net profit, gas used) |
| `economic_reject` | a receipt that lost money: a `gas-unprofitable` simulation with a complete receipt, or the executor's terminal `InsufficientProfit` revert, proven by its raw bytes — the selector `0x4e88422a` plus exactly its two ABI words, in hex |
| `planning_refused` | no bytecode was ever simulated: the encoder's `encode-failed` grammar gap, or the ledger's typed `insolvent-zero-capital` refusal with a complete deficit |
| `unexpected_revert` | a failure the report cannot account for: an unknown bucket or selector, an empty or truncated revert, or an insolvency label with no typed deficit |
| `assembly_missing` | the row never reached the simulation (an assembly skip) |
| `exception` | a transport failure, a refused operator command, a receipt-bearing label with no integer receipt behind it, any response that does not state the exact structural submission evidence (`possible: false`, `observed: false`, and the executor-shape sentence), or a response that does not name its own request |

Execution coverage is `executed_success` plus `economic_reject` — both have an
EVM outcome, and a gas-unprofitable receipt is an executed economic reject, not a
winning arbitrage. Nothing else is coverage, and neither a bucket label nor a
`profitable` spelling promotes a row on its own: the terminal economic refusal is
read from the exact revert bytes, and a receipt-bearing outcome needs the
receipt's own integer numbers behind it.

`--expect` is required and states what the run must prove:

- `--expect executed`: every row must be `executed_success` or `economic_reject`.
- `--expect insolvent`: every row must be `planning_refused` with raw bucket
  `insolvent-zero-capital` and a complete typed deficit. Any execution, any
  other refusal, and any unexplained revert is unexpected.

Each row records `expected` and `matches_policy`; the summary reports `expected`,
`completed`, `missing`, `matched`, `unexpected`, `complete`, `interrupted`, and a
count per category. The registration set is validated before the first probe: a
run covers exactly 27 or exactly 54 directed rows, with unique non-negative engine
path IDs, unique `(permutation, direction)` identities, an explicit `direction` on
every row, and exactly the identities the matrix contains. Duplicates, negative
path IDs, undirected rows, and incomplete sets are refused, and a refused set
writes no report.

A row also audits the answer's identity instead of trusting the socket to have
correlated it: the response's `path_id` must be the engine path the probe asked
for, its `capital` exactly the requested posture, and its `current_block` a
non-negative integer. A response that fails any of the three is an `exception`
row — a profitable verdict for somebody else's path is not coverage — and the raw
response stays in the row beside the verdict.

The report exists before the first probe — every expected row present as
`pending` — and is replaced atomically (temp file + `replace`) after every
response, so an interrupted or killed run leaves a parseable incomplete report
whose remaining rows are still pending, never a green fragment over the rows that
happened to finish. `KeyboardInterrupt` writes the interrupted report and exits
non-zero.

Exit codes: `0` — every expected row completed and matched the declared policy;
`1` — an interrupted or incomplete run, or any unexpected row (a correct
`--expect executed` or `--expect insolvent` run needs no `|| true`); `2` — the
registration set was refused before a single probe was issued. The legacy log
`report` command keeps its own behavior: it fails on missing coverage and on any
`Submitted path` record.

Non-submission is structural: every probe row and the report itself carry
`{"possible": false, "observed": false, "evidence": "dedicated ProbeExecutor has
no submit lane"}`. That is a fact about the executor's construction, not evidence
read off a log — the log `report` command's `broadcast_observed` scan stays
separate, and an absent log line proves nothing by itself.

Pass `--capital zero_balance` to `probe` (the flag replaced `--zero-balance`) to
use ordinary in-path-flash encoding with every asset the probed paths TOUCH
cleared at the executor — for the fixed WETH/USDC/WBTC triangle that is native
ETH, WETH, USDC, WBTC, and their PoolManager ERC-6909 claims — while owner gas
funding and injected code remain. The other two modes are `--capital self_fund`
(the default: the entry capital each built Plan declares, installed, with the
self-funded encoding and every other touched holding cleared) and `--capital
production` (the account's real balances, no capital override at all). The
cleared/installed set is the paths', never a fixed token list: a path naming an
asset outside the pinned mainnet layouts (`CapitalLayouts::verified`: WETH9
`balanceOf`@3, USDC `balances`@9, WBTC `balances`@0, canonical PoolManager
ERC-6909 `balanceOf`@4) is REFUSED before any EVM state exists rather than
probed with its balance left in place. Fixture tokens are the `StubToken` stub
(`balanceOf`@3) and the fake PoolManager's claim mapping (@6), so
`Harness::capital_layouts` declares those slots explicitly — the pinned set is
never applied to a lab contract, and there is no operator or FFI surface that
declares a layout. A losing WETH cycle is then a typed
capital-policy refusal — `bucket: "insolvent-zero-capital"` beside the deficit's
`asset`/`required`/`available` — never a validator panic and never an
`encode-failed` label. The deterministic
`full_three_hop_zero_balance_matrix_executes_without_token_funding` test proves
all 27 profitable shapes with every touched native, token, and PoolManager-claim
balance cleared to exact zero, and
`full_three_hop_self_fund_matrix_executes_from_declared_capital` proves the same
rows from exactly the entry capital their built Plans declare.

`probe` also states the amount vector it ran: `--fixed-input WEI` (default
`1000000000000000`) is sent as the request's `optimal_input` and recorded in the
report and in every row, and `--metadata-file PATH` embeds a JSON object of run
facts (chain, block, release, artifact hashes, source state) into the report
BEFORE its first probe — so an interrupted report still says what it ran under.
Both fail closed: a non-positive or non-integer fixed input, a metadata file
that is missing, unparseable, not a JSON object, empty, or that names a
different `fixed_input` refuses the run (exit `2`) without writing a report.

The five fakes follow the oracle harnesses' build contract. One table,
`tier3-oracle/harness-artifacts.tsv`, binds every committed artifact under
`tier3-oracle/artifacts/` (the ten oracle harnesses and the five fakes) to its
git-tracked source, the compiler output path it is staged from, its build
script, and its publish group. After editing the fake Uniswap contracts, run
`just rebuild-tier3-executor-stubs`: it compiles the stubs into
`OUT_DIR=tier3-oracle/out`, then republishes `tier3-oracle/artifacts/harness`
plus their source hashes in `tier3-oracle/artifacts/manifest.json` — commit
both. In particular, the fake PoolManager's synced-reserve settlement mirrors
real V4: a pre-swap token payment is credited by `settle()` before the swap
creates its debt.

The fakes enforce the behavior the executor depends on: V2's K invariant,
V3's balance-delta input check and reentrancy guard, V4's delta settlement,
ERC-6909 accounting, and callback variants. The V3 check is direction-faithful:
the pool transfers the output, snapshots the *input* token's balance, calls back,
and requires the callback to have paid the swap's own `amountSpecified` of that
token, so a reverse-direction swap gets no free pass.
`TestV3SwapDriver`'s `underpayment` hook withholds exactly that many input units,
which is how `harness_poolv3.rs` proves both directions from the negative side: a
one-unit shortfall reverts `IIA` and unwinds to the pre-swap balances. They
isolate executor routing and settlement logic from real pool pricing. They do not
replace mainnet-fork or Tier-3 execution tests.

## Rebuild reference artifacts

After editing the Solidity reference sources, run
`contracts/scripts/build-sol-reference-artifact.sh` and commit the regenerated
artifact. The routine Ape compile excludes `sol_reference/`; the fuzz tests
consume its committed solc 0.7.6 artifact directly.

The production Vyper artifact has a separate authoritative compile-vs-use gate:

```bash
just verify-tier3-executor-artifact
```

## Verify the fake artifacts against a fresh compile

`tier3-oracle/harness-artifacts.tsv` is the single map behind the whole artifact
pipeline: build staging, publish selection, the source hashes in
`tier3-oracle/artifacts/manifest.json`, and the compile-vs-use check. Adding a
harness there is the only edit its build, publish, and verification paths need.

`just verify-tier3-artifacts` (CI's `tier3-oracle` job) reads that map, runs each
unique build script once into a throwaway `OUT_DIR` with `PUBLISH=0`, and
byte-compares every committed artifact's creation code AND deployed runtime
against the fresh compile. A stub edit without a republish fails there, as does a
missing artifact or a missing/non-hex bytecode field. Its toolchain-free
complement is `tier3_harness_artifacts.rs`, which hashes the tracked sources
against `artifacts/manifest.json` in the default suite.

To inspect a scratch fake-fixture build without touching tracked files:

```bash
OUT_DIR="$(mktemp -d)" PUBLISH=0 tier3-oracle/build-tier3-executor-stubs.sh
python3 tier3-oracle/harness-artifacts.py verify --out-dir "$OUT_DIR" --group executor
```

`PUBLISH=0` writes the five `harness/*.json` entries into `OUT_DIR` and stops;
`PUBLISH=1` (the default, and what `just rebuild-tier3-executor-stubs` runs)
copies those same entries through `publish-artifacts.sh executor` and refreshes
the manifest. An oracle-only publish — `publish-artifacts.sh` with no argument,
which is what each oracle harness build calls — republishes the ten oracle
harnesses and never requires the fake outputs. A caller that relied on the stub
build's old unconditional republish keeps that behavior from the default
`PUBLISH=1`; only an explicit `PUBLISH=0` leaves tracked artifacts alone.
