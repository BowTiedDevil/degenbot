---
name: rust-rebuild
description: How degenbot's build receipt proves the installed Python extension (.so) was built from the current Rust sources, and how to diagnose stale builds. Use when just verify-build-fresh or just rebuild-if-stale fails, when a Python run behaves as if recent Rust edits are absent, or before trusting any maturin/uv rebuild.
---

# Rust Rebuild & Build-Receipt Mechanics

Do not trust a silent "successful" rebuild — Maturin and Cargo can serve cached artifacts, so an apparently successful rebuild can still ship a **stale `.so`**. Verify with the receipt, never by inferring freshness from command output.

## The workflow

After any Rust edit: `just rebuild-if-stale`. It runs the receipt check and rebuilds (`just dev`) only when the installed extension is stale. Only trust a bot run (or a pytest suite) once the check exits 0.

## How the receipt works

Every compile of `degenbot_rs` runs `rust/crates/shells/degenbot-python/build.rs`, which fingerprints the shell's sources **plus every crate under the explicit `rust/crates/{foundation,engine,integrations,shells,facade}` roles and the workspace manifests / repo-root `.cargo` config that its build could link** (via the shared `build_scan.rs` scanner, with per-file + per-tree `cargo:rerun-if-changed` re-triggering — a build that emits no rerun triggers would let dep-only edits never advance the receipt and a stale wheel pass). It writes `<count> <fingerprint>` to a receipt file (`.build-number`, gitignored, at the repo root), embedding both values in the compiled library. The counter advances only when the fingerprint (source content) changes, so test/feature-variant rebuilds never false-positive.

The check compares the installed fingerprint against the repo receipt, so any material built from different sources than the installed artifact (the cached-wheel failure mode) is caught, and no-change recompiles stay fresh.

## Verification entry points

```bash
just verify-build-fresh                          # exit 1 if stale
uv run --no-sync python -m degenbot.build_info   # same check
# from Python: from degenbot.build_info import verify_build_fresh; verify_build_fresh()
# raw values: degenbot._ffi.build_number() / degenbot._ffi.build_fingerprint()
```

`pytest tests/test_build_info.py` gates on this too — a stale `.so` fails the suite.

## Failure-mode notes

- After Rust edits, expect the gate to flag staleness until you rebuild — that is the detector working. Run the rebuild, not a skip.
- A reported number of **0** (or a missing fingerprint) means `build.rs` did not run — investigate before trusting the build.
- The receipt lives outside `rust/target`, so `cargo clean` and `just gc-target` can never roll it back.
- A tree-wide rebuild advances `.build-number` after `degenbot-cli` embeds it, so the receipt gate can fail once on the next compile. Whole-workspace suites run with `--no-fail-fast` so one such failure does not abort the run.
