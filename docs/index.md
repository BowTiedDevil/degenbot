# degenbot documentation

A Rust MEV-bot core with a first-class Python driver shell, for Uniswap (V2, V3, V4), Curve V1, Solidly V2, Balancer V2, and Aave V3 integrations on EVM-compatible blockchains.

The Rust core is the engine; Python is a driver shell. Pool/token state, swap math, event decoding, solvers, the pump loop, and swap encoding all live in the pyo3-free Rust crates; the Python layer provides the user-facing API, orchestration, and configuration through a thin PyO3 binding layer.

**Where the docs live:**

- **This site** — Python API reference, architecture, ADRs, operations playbooks.
- **[docs.rs/degenbot](https://docs.rs/degenbot)** — rustdoc for the published umbrella crate and the pyo3-free core crates.

```{toctree}
:caption: Getting started
:maxdepth: 1

```

```{toctree}
:caption: Architecture & decisions
:maxdepth: 1
:glob:

architecture/*
adr/*
```

```{toctree}
:caption: Operations
:maxdepth: 1
:glob:

cli/*
logging
failure-policy
execution-strategy
telemetry-latency-playbook
```

```{toctree}
:caption: Releases
:maxdepth: 1
:glob:

release-notes/*
releases/*
```

---

Autodoc of the compiled PyO3 module (`degenbot_rs`) is intentionally **not** wired up yet: the binding layer is `publish = false` and building the extension on RTD would need a Rust toolchain + maturin on the build host. The follow-up is to either (a) point sphinx-autoapi at `src/degenbot/*.pyi` (docstrings already live there per repo convention) for a build that needs no native module, or (b) add a docs build job that installs the wheel via maturin and uses `sphinx.ext.autodoc`.