# Rust workspace architecture map

This map is the contributor's short guide to the Rust workspace. The
[workspace manifest](Cargo.toml), crate manifests, and
[architecture-gate tests](crates/degenbot/tests/architecture_gates.rs) are
authoritative when this document and the code disagree. The workspace currently
contains **32 crates under `crates/` plus one external-consumer example**.

For the product-level rule, read [AGENTS.md](../AGENTS.md): **Rust is the engine;
Python is a driver shell, not a co-implementation.** The original split is
recorded in [ADR-005](../docs/adr/ADR-005-polars-inspired-three-layer-architecture.md),
and the console topology in
[ADR-051](../docs/adr/ADR-051-rust-owned-console.md).

## First-class consumers and entry points

There are two equally first-class consumers of one Rust core:

1. A **pure-Rust MEV bot** depends on the publishable `degenbot` umbrella. It
   does not need Python or PyO3 in its dependency graph.
2. A **Python-driven MEV bot** calls the same core through the thin,
   non-publishable `degenbot_rs` PyO3 extension.

The Rust-owned console is a shared no-Python shell used directly by Rust
operators and as the argv passthrough for Python.

| Surface | Package and path | Owns | Publication and build entry |
| --- | --- | --- | --- |
| Pure-Rust product facade | `degenbot` — `crates/degenbot/` | Stable umbrella API: re-exports the PyO3-free product crates. It contains no PyO3 dependency. | Publishable and a workspace `default-member`; `cargo build --manifest-path rust/Cargo.toml` builds it. |
| Python driver shell | `degenbot_rs` — `crates/degenbot-python/` | PyO3 classes, conversions, module registration, GIL-safe calls, and selection of the Python-facing domain surface. It translates calls; it does not own MEV business logic. | `publish = false`; select it explicitly with `-p degenbot_rs`. Wheels are built by maturin/`uv`; use `extension-module` for the extension link. |
| No-Python console shell | `degenbot-cli` — `crates/degenbot-cli/` | The single clap argv tree, `degenbot` binary, rendering/progress, signal policy, and console sinks. | Publishable and a workspace `default-member`; built by the default Cargo command. |
| Console semantics | `degenbot-cli-core` — `crates/degenbot-cli-core/` | Typed `Command` values, command execution, reports, prompts, cancellation, and command error-to-exit mapping. It is shared by the Rust binary and Python passthrough. | Publishable; it is a library, not the binary entry point. |
| External Rust consumer example | `degenbot-settlement-bot-example` — `examples/settlement_bot/` | A complete settlement-arbitrage driver. Its MEV/domain capabilities enter through the `degenbot` umbrella; it directly names `degenbot-eventhub` for operator event types as the current reachability exception. Format codecs, async runtime, and tracing remain driver-local. | `publish = false`; excluded from `default-members`, so select it explicitly. |

Two smoke programs protect the consumer boundaries:

- `crates/degenbot/examples/standalone_consumer.rs` is the Tier-0 proof that
  the public umbrella can construct state, register a pool, and calculate a
  swap without Python. Run it through `just test-standalone`.
- `examples/settlement_bot/` is the executable external-consumer example. Its
  compile failure is the signal that a product capability is missing from the
  umbrella; adding a direct internal dependency to bypass a missing umbrella
  capability defeats the check. The existing direct `degenbot-eventhub` edge is
  an explicit reachability exception, not a pattern for new domain dependencies.

## Crate ownership map

The groups below describe responsibility, not a promise of a perfectly uniform
dependency depth. All arrows in the dependency graph point from a consumer to
the crate that provides the capability.

### Product facade and shells

| Crate | Home for |
| --- | --- |
| `degenbot` | The public Rust product surface. A new capability required by an external bot belongs in an underlying core and must be re-exported here. |
| `degenbot_rs` | Python/PyO3 adaptation only. Python-facing conveniences may live here when they are presentation, extraction, or conversion rather than product behavior. |
| `degenbot-cli` | Argv spelling, interaction, rendering, progress, SIGINT handling, and sink installation. It must not re-encode domain behavior. |
| `degenbot-cli-core` | What every console command means and returns. It is deliberately independent of clap and indicatif. |

### Integration and domain crates

These crates compose shared capabilities into stateful or operational domain
behavior.

| Crate | Capability ownership |
| --- | --- |
| `degenbot-bot` | Rust-owned `BotState`, unified Uniswap engine behavior, and bot composition. |
| `degenbot-strategy` | Strategy vocabulary, host, and concrete executable strategy compositions. |
| `degenbot-arbitrage` | Settlement-arbitrage searcher policy: pre/post balance checks, profit and priority-fee policy, and dispatch categorization. |
| `degenbot-simulation` | The generic in-process revm executor and simulation dispatch. It does not own searcher-specific policy. |
| `degenbot-solvers` | Value-only multi-hop solve math over injected hop state. |
| `degenbot-execution` | The user-owned execution-adapter seam and its value/protocol types; it ships no default strategy. |
| `degenbot-submission` | EIP-1559 signing, fee finalization, and typed transaction envelopes. |
| `degenbot-ingestion` | WebSocket block/log intake, filtering, backfill, and watchdog policy. |
| `degenbot-pool-updater` | Typed pool-event fetching and transactional chunk application. |
| `degenbot-aave` | Aave V3 updater, position analysis, and Aave fixed-point math integration. |
| `degenbot-price` | On-chain Chainlink and Aave-oracle price readers. |
| `degenbot-fork` | Anvil fork lifecycle and development RPC support. |
| `degenbot-runs` | Per-session run artifacts (stdout log and trace JSONL). |
| `degenbot-workers` | Worker-role state machine, budget authority, priority dispatch, and cordon posture. |

Strategy and engine ownership is intentionally separate. See
[ADR-019](../docs/adr/ADR-019-in-process-revm-sole-simulation-executor-strategy-engine-separation.md):
the simulation executor stays generic, while searcher policy belongs to the
strategy layer.

### Foundation and shared domain substrate

These crates provide reusable values, codecs, state substrates, and protocol or
storage services. They are the normal home for a capability that is meaningful
without a complete bot runtime.

| Crate | Capability ownership |
| --- | --- |
| `degenbot-config` | Typed configuration, provenance-aware loading, and shared driver-domain resolvers. |
| `degenbot-core` | Foundational errors, address/hex/runtime utilities, observability vocabulary, and other low-level shared types. |
| `degenbot-abi` | ABI types, encode/decode, and function-signature parsing. |
| `degenbot-decoders` | Uniswap V2/V3/V4 event-log decoding. |
| `degenbot-math` | Canonical AMM invariant math for V2, concentrated liquidity, Curve, Balancer, and Solidly families. |
| `degenbot-pools` | Value-only pool identity/state and stateless swap simulation. |
| `degenbot-uniswap` | Uniswap protocol identity presets and V2 swap calldata encoding. |
| `degenbot-rpc` | Ethereum provider, contract interfaces, and subscription core. |
| `degenbot-db` | Rust-owned SQLite schema, migrations, file operations, and persistence substrate. |
| `degenbot-pathfinding` | Zero-dependency arbitrage graph and path enumeration. |
| `degenbot-order-index` | Net-profit order indexing under variable gas price. |
| `degenbot-executor` | Cmd-executor domain layout and storage math. |
| `degenbot-eventhub` | Transport-pure event vocabulary and overflow policy for in-process fan-out. |

### Non-publishable samples

| Package | Purpose |
| --- | --- |
| `degenbot-execution-sample` | A standalone user-defined `ExecutionAdapter` for a foreign executor contract. It demonstrates the execution seam and is not a product dependency. |
| `degenbot-settlement-bot-example` | The complete external-consumer bot described above. |

All other current workspace members are publishable. Internal path
dependencies carry a version requirement in `[workspace.dependencies]`, in
lockstep with `[workspace.package].version`, so a publish dry run can resolve
the internal graph. `just publish-dry-run` is the publication oracle.

## Dependency direction and placement rules

The normal direction is:

```text
external Rust driver                 Python driver
         |                                |
         v                                v
      degenbot                     degenbot_rs (PyO3)
         |                                |
         +-----------> integration/domain crates
                              |
                              v
                     foundation/substrate crates

degenbot-cli (argv/render) -> degenbot-cli-core (semantics) -> domain crates
Python argv passthrough ----^                         |
                                                   v
                                      the same Rust domain crates
```

Enforced consequences:

- Core crates are PyO3-free under their default features. `degenbot_rs` is the
  only member allowed to depend on PyO3; it may enable a core's `pyo3` feature
  solely to obtain boundary conversions.
- A foundation or domain crate never depends on `degenbot`, `degenbot_rs`,
  `degenbot-cli`, or `degenbot-cli-core`. Dependencies point down to reusable
  capabilities, never back up to a shell or facade.
- `degenbot` is an outward-facing facade, not the home for implementation. Put
  behavior in the owning core, then re-export it.
- `degenbot-cli-core` owns command semantics and must remain free of clap and
  indicatif. `degenbot-cli` owns argv and presentation and may reach command
  behavior through `degenbot-cli-core`, not by reimplementing it or bypassing
  the semantics crate to call a domain engine.
- Generic simulation/execution mechanisms stay separate from searcher policy.
  Do not move strategy-specific profit rules, funding assumptions, or operator
  policy into the simulation substrate.
- Workspace dependency declarations are centralized in the root manifest.
  Keep package metadata and lint policy inherited from the workspace; keep
  implementation-specific dependencies in the owning crate unless there is a
  deliberate shared policy.

### Where should a new capability go?

Use this order:

1. **Stateless value, codec, math, or reusable service:** put it in the existing
   foundation/domain owner (`degenbot-math`, `degenbot-abi`,
   `degenbot-decoders`, `degenbot-rpc`, `degenbot-db`, and so on). Create a
   focused crate only when the capability has a coherent public API and clear
   dependency direction.
2. **Stateful engine or cross-crate workflow:** put it in the integration crate
   that owns the lifecycle. Keep lower-level mechanisms in foundation crates.
3. **Product capability needed by a Rust consumer:** implement it in a core
   crate and re-export it from `degenbot`. Prove reachability with the umbrella
   smoke or the external-consumer example.
4. **Python extraction, conversion, or wrapper ergonomics:** put only the thin
   adaptation in `degenbot_rs`; keep the behavior in Rust core code.
5. **Console meaning versus console UX:** put semantics/results in
   `degenbot-cli-core`; put argv, prompts, progress, and rendering in
   `degenbot-cli`.
6. **Searcher-specific policy:** keep it in the strategy layer or the owning
   strategy package, not in the generic simulation or execution substrate.

When adding a workspace member, update the root member list, its publication
intent, and the relevant architecture-gate census in the same change.

## Feature ownership

A feature is defined by the crate that owns the implementation. Consumers may
select or forward it, but must not duplicate its gate list.

- Core boundary conversions are opt-in `pyo3` features on the few crates that
  need them (`degenbot-core`, `degenbot-price`, and `degenbot-submission`).
  `degenbot_rs` enables them at the binding boundary.
- Test-only constructors are feature-gated on their owning crate, notably
  `degenbot-rpc/test-utils` and `degenbot-fork/test-utils`.
- Performance and telemetry switches belong to their implementation crates:
  `degenbot-bot` owns hotpath, allocator-control, OpenTelemetry, and Prometheus
  features; `degenbot-solvers` owns hotpath/telemetry features; `degenbot-runs`
  owns its tracing feature; `degenbot-order-index` owns its envelope feature.
- `degenbot/otel` is a narrow passthrough to `degenbot-bot/otel`, so the
  feature is reachable from the external product facade.
- `degenbot_rs` owns Python domain selection, `extension-module`, PyO3
  auto-initialization, and the development-only mimalloc passthrough. Its
  `extension-module` feature forwards to `pyo3/extension-module`.
- Feature-matrix validation is lane-based. Never use `--all-features` as the
  default or release gate: it enables test-only and mutually exclusive variants.

Tokio's full runtime metrics use its unstable API. The repository deliberately
has no global rustflag, so hotpath lanes set
`RUSTFLAGS=--cfg tokio_unstable` explicitly. Direct hotpath Cargo or maturin
commands must do the same.

## Fixture and oracle ownership

Fixtures live with the contract they exercise, while genuinely cross-consumer
oracles have one shared source of truth.

- **Crate-local behavior:** put a fixture under that crate's
  `tests/fixtures/`. Examples include solver captures under
  `degenbot-solvers`, strategy frame captures under `degenbot-strategy`, and
  observability snapshots under `degenbot-bot`.
- **Rust/Python or umbrella parity:** put a shared oracle under
  `tests/standalone_parity/fixtures/` or `tests/fixtures/`, then have both
  consumers load the same file. Do not copy and hand-tune divergent expected
  outputs.
- **Database schema:** `degenbot-db/tests/fixtures/` owns schema and migration
  fixtures because `degenbot-db` owns the schema. Other offline consumers may
  reference the committed read-only database instead of duplicating it.
- **Real-EVM Tier-3 oracles:** `tier3-oracle/` owns Solidity/Vyper harness
  sources and committed artifacts. `degenbot-simulation` owns the reusable
  revm/oracle driver; its tests consume the committed artifacts. Regenerate or
  verify them through the recipes in [ADR-020](../docs/adr/ADR-020-tier3-onchain-accuracy-oracle.md)
  and the [`justfile`](../justfile). The default Rust suite runs against
  committed bytecode and does not require solc, forge, or vyper.
- Runtime hot paths must not read test fixtures. Keep ordinary fixture
  readers, generators, and heavyweight dependencies under tests/examples or an
  explicit dev-only feature. The published `degenbot-simulation::harness` helper
  is a deliberate exception: it is an investigation/oracle surface that loads
  committed `tier3-oracle/artifacts/`, not a production execution path.

## Build, feature, and verification matrix

The repository root pins Rust 1.98.1 for local development and releases; the
workspace MSRV is Rust 1.97. `just toolchain` prints the policy. Local builds
use `[profile.dev]` (`opt-level = 1`, line-tables-only debug information, no
LTO/stripping). Release builds use thin LTO, stripping, and
`codegen-units = 1` for the final link; the explicitly listed core-library
overrides use 16 codegen units and are reconverged by the final link. Do not
change those values without the build-policy decision recorded in
[AGENTS.md](../AGENTS.md).

Unqualified Cargo commands use only `degenbot` and `degenbot-cli`, the two
pure-Rust defaults. Use an explicit package selector for the PyO3 shell and
samples, and `--workspace` when a command must cover every member.

| Purpose | Command |
| --- | --- |
| Inspect the authoritative graph | `cargo metadata --manifest-path rust/Cargo.toml --format-version 1 --no-deps --locked` |
| Build the two default Rust entry points | `cargo build --manifest-path rust/Cargo.toml` |
| Check pure-Rust consumer defaults | `just check-rust-consumer` |
| Check binding defaults without extension link mode | `just check-rust-binding-default` |
| Check the exact development-wheel features | `just check-rust-dev-features` |
| Check the release-equivalent extension set | `just check-rust-extension-release` |
| Exhaustive diagnostic only | `just check-rust-all-features` |
| Build the extension with release-equivalent features | `just build-rust-extension` |
| Build/install the development Python extension | `just dev` |
| Authoritative default-feature lint and architecture subset | `just lint-rust-check` |
| Check Rust formatting | `cargo fmt --manifest-path rust/Cargo.toml --all -- --check` or `just fmt-check` |
| Standalone consumer plus full workspace tests | `just test-rust` |
| Verify publishability | `just publish-dry-run` |

The development-wheel feature list is exactly `extension-module`,
`degenbot-bot/hotpath`, `degenbot-bot/hotpath-prometheus`,
`degenbot-solvers/hotpath`, `degenbot-bot/allocator-ctrl`, `otel`, and
`mimalloc`. Release wheels retain `degenbot_rs` defaults and
`pyo3/extension-module`; they exclude development-only profiling,
telemetry, allocator-control, and allocator-selection features.

### Architecture gates

The gate bodies live in
[`crates/degenbot/tests/architecture_gates.rs`](crates/degenbot/tests/architecture_gates.rs).
The recipes make the following rules executable:

| Recipe | Rule |
| --- | --- |
| `just check-no-pyo3-in-cores` | Every core crate is PyO3-free under default features. Add new core crates to the census. |
| `just check-cli-core-purity` | `degenbot-cli-core` remains free of clap and indicatif. |
| `just check-cli-shell-purity` | `degenbot-cli` names only workspace members and its explicit argv/sink-plumbing allowlist. |
| `just check-engine-impl-blocks` | The engine has one implementation block and the PyO3 shell has only the compatibility name string. |
| `just check-no-inner-allow` | Rust sources do not use file-level inner `allow` attributes. |

`just lint-rust-check` runs the applicable architecture subset before the
workspace default-feature Clippy gate. The feature lanes above must remain
separate so an exhaustive diagnostic cannot hide a default or release
regression.

## Python extension freshness

After a Rust source change, do not trust a fast or cached maturin/uv build by
itself. Run `just verify-build-fresh`. The `degenbot_rs` build script writes the
repo-root `.build-number` receipt and fingerprints its own sources, every
sibling crate under `rust/crates`, the workspace manifests/lockfile, and Cargo
configuration. The installed extension carries the same values, allowing
`just verify-build-fresh` to detect stale cached wheels.

If the check reports staleness, rebuild with:

```bash
uv sync --reinstall-package degenbot
just verify-build-fresh
```

The full rationale and raw build-info API are in
[AGENTS.md](../AGENTS.md#rebuilding-the-rust-so-after-edits).
