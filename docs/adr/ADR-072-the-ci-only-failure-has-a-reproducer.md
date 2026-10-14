# ADR-072: The CI-only failure has a reproducer — build-config divergence is accepted, `just test-rust-ci` is the contract

**Status: accepted (2026-10-14).**

## Context

CI builds test binaries with `CARGO_PROFILE_DEV_DEBUG=0` and
`CARGO_INCREMENTAL=0` (the ci.yml `env:` block: incremental state is never
reused on ephemeral runners and only bloats the rust-cache save; debuginfo
likewise shrinks rust/target and the cache). Local runs build with the
workspace's ordinary dev profile. The two are therefore *different binaries*:
proc-macro fn addresses, codegen layout, and location/panic metadata are all
build-config-sensitive.

This is not hypothetical. All three CI-only failures of 2026-10-14 needed
something beyond an ordinary local run to reproduce — including the ADR-071
D3 identity gate, where the same source produced a differently laid-out
constant depending on the codegen unit count, a pure build-config knob.

## Decision

**D1 — The divergence is accepted.** CI keeps the lean build config; local
keeps full debuginfo and incremental state. The price bought is cache size
and CI minutes; the price paid is one reproduction command, and D2 prices it.

**D2 — `just test-rust-ci` is the reproduction contract.** It runs the rust
suite under CI's build flags. For any "passes here, fails in CI" report, run
it *before* theorizing: if the failure reproduces there, the bug is real and
the build config is not the story; if it does not, the divergence is the
story, and the fix is D3.

**D3 — Layout-level assertions are bugs.** Tests and gates compare
source-level identity — fn-item *names* (ADR-071 amendment 2026-10-14),
constants, values — never addresses, offsets, or serialized layout, which are
build-config outputs.

## Consequences

- The reproduction ladder for a suspect failure is `just test-rust`
  (ordinary build) → `just test-rust-ci` (CI build) → theorize. AGENTS.md
  carries the pointer next to the test commands.
- The ci-parity brake (`scripts/hooks/ci-parity.sh`) keeps a named local
  entrypoint per CI job, so every CI failure has a local name to run — this
  ADR is what to do with that name first.
- The divergence is recorded where it is configured: the ci.yml `env:` block
  comment states what the flags buy.

## Related

- **ADR-071** (D2/D3, amendment 2026-10-14) — the fn-address gate that this
  build sensitivity surfaced in; its fix (compare names, not code) is D3's
  archetype.
