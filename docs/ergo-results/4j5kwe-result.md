# 4J5KWE result — config-derived-static ratchet

## What shipped

- `rust/crates/facade/degenbot/tests/architecture_gates.rs` (+265): the `config_derived_statics_are_classified` gate. A `static NAME: OnceLock|LazyLock|OnceCell|Once<..> = <init>;` is a candidate; a HIT is one whose initialization path (the declaration initializer PLUS every name-associated `get_or_init`/`set`/`lazy` argument in the same file, whitespace-tolerant) reads `holder::config(`, `holder::config_arc(`, or the substrate delegate `stance::config(`. Strings/comments masked; `#[cfg(test)]` regions and tests/benches/examples skipped. Reuses the file's existing scanner helpers.
- `rust/crates/facade/degenbot/tests/config_derived_statics_census.txt` (NEW): the census — 7 detected hits (SOLVE_WORKERS, AMBIENT_WORKERS, THRESHOLD_INIT, TRACE_BACKTRACES, DIAG_INIT, INIT_DONE, PERMYRIAD), each re-verified against source and classified `process-lifetime` with a justification; plus a documented blind-spot section (the canonical transitive miss `runtime.rs::RUNTIME` -> `build_runtime` -> `ambient_io_worker_count` -> holder, and the non-OnceLock republishable RwLock slots).
- Wiring: `just check-config-derived-statics` recipe, a dependency of `lint-rust-check` (justfile:215), plus the pre-push `rust-config-derived-statics` hook (prek.toml:181).
- Bidirectional: an unlisted hit fails; a listed-but-absent hit fails with a shrink message.

## PM review (all probes re-executed by the PM)

- Unclassified new static in a scratch source file -> gate RED naming exactly `pm_probe_tmp.rs:PM_PROBE_WORKERS`; removed -> green.
- Census line removed -> RED naming the now-unlisted static.
- Bogus census entry (`PM_GONE_STATIC`) -> RED with the stale-entry shrink message; restored -> green.
- Architecture-gates binary 20/20; recipe green; fmt-check + comment-hygiene green.

## Why a cargo gate

The target is Rust source and the repo already had the idiom (check-* gates as cargo tests with thin just recipes). The scanner rides every cargo-test track, CI, and pre-push.
