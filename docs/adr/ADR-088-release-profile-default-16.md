# ADR-088: Release profile default 16 codegen units — one pin, no per-crate list

## Status

Accepted (2026-10-29).

## Context

The workspace `[profile.release]` exists to build exactly one shipped
artifact: the `degenbot_rs` cdylib (the PyO3 extension). It sets
`lto = "thin"` + `strip = true` + `codegen-units = 1` for that final link.
Cargo has no per-package `lto` or `strip`, so those are profile-level by
construction; `codegen-units` is the one knob with a per-package override,
and the manifest used it to keep library crates out of the strict-1 path.

That override list was maintained by accretion. Born at the six-crate split
with five entries, it gained one line per extraction and reached fourteen —
while the crates that arrived after the convention was established
(`degenbot-substrate` at 29.9k source lines, `degenbot-aave` at 19.3k,
`degenbot-simulation`, `degenbot-strategy`, `degenbot-workers`) never joined.
The drift is invisible in the file and real in the build: a 726-line crate
(`degenbot-price`) compiled at 16 units while a 29.9k-line crate compiled
strict-1. The list also still named `degenbot-concentrated-liquidity-math`
in its comment, a crate consolidated away by ADR-035.

Two facts about the override mechanism were verified empirically on the
pinned toolchain (cargo 1.98.1, scratch workspace; lab available in the
task record):

1. An explicit `[profile.release.package.<name>]` override applies to that
   package in every build role: as a dependency, as the built root
   (`cargo build -p <name>`), as a co-root under `--workspace`, and under
   release `cargo check`. The maturin invocation that builds the extension
   (`-p degenbot_rs`) therefore cannot lose a pin by making the binding the
   root package.
2. The `[profile.release.package."*"]` wildcard matches only non-workspace
   packages; workspace members are exempt by documented (and verified)
   rule. A wildcard therefore cannot express "every member crate", only
   "every third-party crate".

## Decision

**Every workspace library compiles at `codegen-units = 16` by default; the
shipped cdylib is pinned back to strict-1 by exactly one named override.**

1. `[profile.release]` keeps `lto = "thin"`, `strip = true`, and moves to
   `codegen-units = 16`. Present and future member libs inherit 16 with no
   manifest edit.
2. `[profile.release.package.degenbot_rs] codegen-units = 1` is the sole
   per-package override. The extension keeps maximal codegen granularity
   into the final thin-LTO link.
3. Third-party dependencies compile at 16 units pre-LTO. The final
   thin-LTO link re-converges them — the same reconvergence the fourteen
   listed member crates already shipped with.
4. If a binary member is ever built as a shipped artifact inside this
   workspace, it takes its own `codegen-units = 1` pin at that decision —
   the same one-line shape as the binding pin.
5. The profile shape is pinned by the `profile_release_policy_is_respected`
   gate on the umbrella crate's `architecture_gates` suite, wired as
   `just check-profile-policy` into `lint-rust-check`. The gate asserts the
   shape (root values, override set ⊆ `{degenbot_rs}` at 1), not a member
   enumeration, so crate splits stay gate-clean.

## Rejected alternative

**Wildcard strictness** — `[profile.release.package."*"] codegen-units = 1`
over a member-default of 16, keeping third-party codegen byte-identical to
the old list. Rejected: it leans on the wildcard's member-exemption
semantics (verified on 1.98.1 only; the MSRV lane compiles at 1.97.0), it
adds a second mechanism beside the pin, and it forfeits parallel codegen
for third-party dependencies — the largest share of a cold extension build
— for a byte-identity guarantee the final thin-LTO link makes moot.

## Consequences

- Member libraries previously absent from the list (substrate, aave,
  simulation, strategy, workers, and the small later crates) join the 16
  the listed fourteen already had; the manifest's 14 override sections
  collapse to one.
- The only maintenance trigger left is renaming the binding crate or
  shipping a second artifact — both deliberate events that touch the pin
  line under review.
- Wheel parity was checked with the replay bench
  (`just bench-updaters`, ADR-068 D6): baseline pool 279 / verify 282 /
  dense 120 / aave update 213 / aave config 190 chunks/sec; two post-flip
  runs read 273·263 / 265·268 / 125·117 / 189·176 / 187·170. Consecutive
  identical-configuration runs differ by up to ~9% on the small aave
  captures (the ledger's whole-millisecond statement quantization), so the
  flip's deltas sit inside run-to-run noise — no parity regression.
- `rust/README.md`'s build-matrix prose and the justfile's development
  header comment describe the default-16-plus-pin shape and cite this ADR.
- The prior prose rule ("do not change these release values without an
  explicit decision") was removed from AGENTS.md before this decision; the
  build-policy record now lives here, in the manifest comments, and in the
  gate.
