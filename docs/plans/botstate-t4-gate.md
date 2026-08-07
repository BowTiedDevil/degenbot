# T4 — Pilot gate results (CL slice)

Run 2026-06-10 after the CL slice move (T2) with tests retained resident (T3 decision).

| Gate | Result |
|------|--------|
| `just test-rust` (workspace + standalone) | PASS — 111 test-result-ok suites, 0 failures, `standalone_consumer` Tier-0 runs to exit 0 |
| `cargo clippy --all-targets --all-features -- --deny warnings` | PASS — exit 0 |
| `just check-no-pyo3-in-cores` | PASS — cores + umbrella pyo3-free under default features |
| `cargo fmt --check` (degenbot-bot) | PASS — mod.rs + cl_orchestration.rs clean |
| PyO3 binding + umbrella compile (from workspace test) | PASS — compiled **untouched** (call-site neutrality) |

**Call-site neutrality confirmed:** the 58 CL methods moved are inherent `impl BotState`
methods; no call site (`PyBot`, umbrella re-exports, `standalone_consumer`, other bot_core
modules) changed. `use super::*`/explicit imports resolve all moved-method type references;
mod.rs's now-orphaned `state_history` imports (ScalarPriors/TickBefore/V3BlockDelta) removed.

**RED-neutral→GREEN rule held:** 410 `degenbot-bot` tests pass with zero test-body behavior
edits. No logic renamed/edited/reordered.

Note: the cross-crate `cargo test --workspace`/standalone gates were transiently red during the
run because a concurrent agent was mid-edit of `degenbot-rpc::provider.rs` (duplicate `rpc_url`);
once that file stabilized (not modified by this epic), the gates went green. Per instruction, the
concurrent agent's files were left untouched.
