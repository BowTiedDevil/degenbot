# Consolidate `just test-*` verbs into a single `just test` funnel

## What changed

**justfile** — added a `test` funnel (standalone smoke + `cargo test --workspace` + full
`pytest` under one entrypoint, with a header explaining that "run the tests" is no longer a
language choice since Python is a driver shell over the Rust core). Removed three redundant
recipes:
- `test-all` → superseded by `test`
- `test-rust-python` → redundant subset of `test-python` (`test-python` runs the whole
  `tests/`, including `tests/rust`)
- `test-offline-parity` → redundant subset of `test-python` (the `onchain_oracle` marker is
  already in the default offline `-m "not slow and not base and not online_rpc"` filter)

Retained as CI + pre-push-hook subunits (so the python-version matrix and job partitioning keep
working): `test-rust`, `test-python`, `test-rust-nextest`. Gated suites stay out of `test`:
`test-tier3`/`verify-tier3-*` (toolchain), `record-golden`/`verify-deployments` (network).

**Docs / references updated** (10 tracked files + this result file) so nothing points at a
retired recipe: CONTEXT.md, three-layer-transition.md, hop-encoding-relay-retirement.md,
pools-extraction-inventory.md, chain-bootstrap-tick-map.md, sealed-pool-seam.md,
clamp-consumed-inputs-executor-forward.md, revm-inspector-diagnostics.md,
rg-results mstat2 + rnzquo, .scratch/adr-019-plan.md.

## Not changed (intentional)

- `ci.yml` and `prek.toml` keep calling `test-rust`/`test-python` — these subunits still exist.
- No full local test run (deferred to CI, per plan).

## Validation

- `just --list` parses; `test` resolves to `test-rust test-python` (verified via `--dry-run`).
- Every `just X` referenced by `ci.yml` + `prek.toml` resolves against `just --list`.
- Repo-wide `rg` for `test-all|test-rust-python|test-offline-parity` returns zero hits.
