# degenbot

A Rust MEV-bot core with a first-class Python driver shell, for Uniswap (V2, V3, V4), Curve V1, Solidly V2, Balancer V2, and Aave V3 integrations on EVM-compatible blockchains.

The Rust core is the engine; Python is a driver shell. Pool/token state, swap math, event decoding, solvers, the pump loop, and swap encoding all live in the pyo3-free Rust crates. Two equally first-class consumers share one engine:

- **Pure-Rust MEV bot** — `cargo add degenbot` and build a fully functional bot in Rust only. API: [docs.rs/degenbot](https://docs.rs/degenbot)
- **Python-driven MEV bot** — drive the same core through a thin PyO3 layer, `pip install degenbot`. API & docs: this site

Start with {doc}`getting-started` — or open {doc}`adr/README` if you want the design rationale first.

## Where everything lives

**Getting started** — {doc}`getting-started`: installation, a five-minute code tour, and the one rule that surprises people (pools can't be constructed directly).

**Architecture** — how the system is built:

- {doc}`architecture/io-free-pools` — the foundation: on-chain state injected at construction, pure math after
- {doc}`architecture/block-state-machine` — the pump's block clock as a finite state machine
- {doc}`architecture/executor-command-grammar` — the execution layer's command language
- {doc}`architecture/rust-owned-bot` — what moved into the Rust core and why
…plus 14 more in the sidebar: executor grammar usage, concentrated-liquidity tickmaps, Curve stableswap, inspector-captured sim diagnostics, storage layouts.

**{doc}`adr/README`** — the settled design decisions, grouped by whether they still bind; the "why" behind the code.

**Command line** — the `degenbot` CLI: {doc}`cli/pool` (pool inspection & management), {doc}`cli/database` (schema, migrations, cutover), {doc}`cli/aave` (Aave V3 tooling).

**Operations** — {doc}`logging` (log registry, Python-logging forwarding), {doc}`failure-policy` (reaction taxonomy for runtime failures), {doc}`execution-strategy` (strategy configuration), {doc}`execution-adapters` (plugging in your own execution contract), {doc}`telemetry-latency-playbook` (pump/solve latency from Jaeger & Prometheus), {doc}`llvm-release-ci` (the LLVM 23 toolchain the shipped extension and the umbrella crate link).

**Reference** — the catalogs and rules the repo runs on: {doc}`capabilities` (every capability’s Rust home, Python name, door items, and consumer probe), {doc}`rust-config-keys` (the generated operator key table), {doc}`comments` (comment hygiene and the first-home test), {doc}`gates` (the gate registry, section by section), {doc}`rust-cli` (the one `degenbot` console), {doc}`config-migration` (schema cutover history).

**Working notes & benchmarks** — lab reports and perf decisions: {doc}`cache-lab-report`, {doc}`rayon-parallelism-lab`, {doc}`mimalloc-purge-delay-decision`, {doc}`detached-solve-cycle-decision`, {doc}`gas-estimation` (the measured per-op gas decomposition behind the pre-sim admission floor).

```{toctree}
:caption: Getting started
:hidden:

getting-started
api
autoapi/degenbot/index
```

```{toctree}
:caption: Architecture
:hidden:
:glob:

architecture/*
```

```{toctree}
:caption: ADRs
:hidden:

adr/README
```

```{toctree}
:caption: Command line
:hidden:

cli/pool
cli/database
cli/aave
```

```{toctree}
:caption: Operations
:hidden:

logging
failure-policy
execution-strategy
execution-adapters
telemetry-latency-playbook
llvm-release-ci
backrun-driver-runbook
backrun-endpoint-policy
```

```{toctree}
:caption: Reference
:hidden:

capabilities
rust-config-keys
comments
gates
rust-cli
config-migration
```

```{toctree}
:caption: Working notes
:hidden:

cache-lab-report
hotpath-crossing-cache-verification
cow-protocol-survey
updater-rpc-sql-survey
rayon-parallelism-lab
mimalloc-purge-delay-decision
detached-solve-cycle-decision
gas-estimation
test-global-state-audit
```
