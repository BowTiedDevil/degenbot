# Rust build/lint/check performance baseline

Captured in the devcontainer (24 cores, plain `ld`, rustc 1.96.1, 1.9T disk).
Established before any performance levers are pulled. Each experiment should
re-measure the relevant rows and record before/after deltas here.

## Hardware / toolchain at baseline

- CPU: 24 cores
- Linker: system `ld` (no mold/lld installed)
- rustc: 1.96.1 (Fedora)
- `cargo-nextest`, `cargo-machete`, `cargo-shear`, `cargo-llvm-lines`: NOT installed
- Workspace: 25 crates under `rust/crates/`
- `[profile.dev]`: `codegen-units = 16` only (no `build-override`, no `debug`/`strip` tweaks)

## Measured baselines

| # | Operation | Wall | Notes |
|---|-----------|------|-------|
| B0 | Clean `cargo build --workspace` | **59s** | 315s unit-time across 329 crates |
| B1 | Warm `cargo check --workspace` (first after build) | **19s** | check-profile metadata tax |
| B6 | Warm `cargo check --workspace` (steady) | **2.7s** | true no-op floor |
| B3 | `cargo clippy --all-targets --all-features` (warm) | **20s** | re-checks 25 crates under all-features |
| B4 | `cargo test --workspace --no-run` (warm) | **64s** | links **52 test binaries** |
| B5 | Touch `degenbot-core/src/lib.rs` → `cargo check -p degenbot-core` | 15s | includes aws-lc-sys rebuild on profile switch |

## Clean-build long-pole crates (from `cargo build --timings`)

| Wall | Crate | Lever candidate |
|------|-------|-----------------|
| 27.0s | `degenbot_rs` (PyO3 cdylib link) | faster linker (mold/lld) |
| 10.1s | `aws-lc-sys` (C/C++ build) | rustls backend / aws-lc-rs feature / system lib |
| 7.9s | `libsqlite3-sys` (C build) | system sqlite / bundled toggle |
| 4.6s | `tokio` | feature prune |
| 4.3s | `syn` (proc-macro) | build-override opt-level=3 |
| 4.1s | `derive_more-impl` (proc-macro) | build-override opt-level=3 |
| 3.8s | `pyo3-macros-backend` (proc-macro) | build-override opt-level=3 |
| 3.8s | `pyo3` | feature prune |
| 3.8s | `degenbot-rpc` | — (workspace crate) |
| 3.7s | `degenbot-aave` | — (workspace crate) |
| 3.5s | `serde_derive` (proc-macro) | build-override opt-level=3 |
| 3.1s | `syn-solidity` (proc-macro) | build-override opt-level=3 |

Aggregate proc-macro time on clean build ≈ **19s** (syn + derive_more-impl +
pyo3-macros-backend + serde_derive + syn-solidity).

## Integration test files (combine-tests lever)

27 `.rs` files across 9 crates; each currently a separate link.

```
7  degenbot-db
6  degenbot-pools
4  degenbot
3  degenbot-executor
2  degenbot-python
2  degenbot-pool-updater
1  degenbot-solidly-math
1  degenbot-curve-math
1  degenbot-balancer-math
```

## Experiment queue (ranked by expected impact on this baseline)

1. **Faster linker (mold → lld)** — targets the 27s cdylib link + 52 test-binary
   links in B4. Lowest risk, no code change. Re-measure B0, B4.
2. **`[profile.dev.build-override] opt-level = 3`** — proc-macros are compiled
   *and* run; ≈19s aggregate on clean, also paid on every invalidation.
   Re-measure B0.
3. **`cargo-nextest` in `just test-rust`** — faster test *execution* + better
   build scheduling. Re-measure the `test-rust` wall (build+run).
4. **rust-analyzer `cargo.targetDir: true`** — kills profile cross-contamination
   (the B1 19s tax, and cdylib rebuild churn from `uv sync`). Re-measure B1
   after an r-a run, and the `uv sync` cdylib rebuild.
5. **Combine integration tests into one binary per crate** — 27 links → 9.
   Moderate risk (must `pub` internals). Re-measure B4.
6. **`aws-lc-sys` / `libsqlite3-sys` C-build avoidance** — 18s combined on
   clean; investigate feature flags or system libs. Re-measure B0.
7. **Feature prune** (`cargo-machete`, `cargo-shear`, `cargo features prune`)
   — slim tokio/pyo3/alloy feature sets. Re-measure B0, B3.

## Reproducing a baseline number

```bash
cd rust
cargo clean
time cargo build --workspace --timings   # B0
# parse slowest crates:
python3 - <<'PY'
import re,glob
f=sorted(glob.glob('target/cargo-timings/*.html'))[-1]
d=open(f).read()
pairs=[(n,float(t)) for n,t in re.findall(r'"name":\s*"([^"]+)"[^}]*?"duration":\s*([0-9.]+)', d, re.S)]
for n,t in sorted(pairs,key=lambda x:-x[1])[:15]: print(f'{t:6.2f}s  {n}')
PY
```
