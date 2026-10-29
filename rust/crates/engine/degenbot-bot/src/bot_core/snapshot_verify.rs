//! Concentrated-liquidity verification orchestration (ADR-006 D4).
//!
//! Extracted from `py_binding.rs` so the pure-logic core is testable without
//! `PyO3`. `py_binding.rs` (in `degenbot-python`) keeps the `PyO3` arg-parsing
//! + I/O (the `AlloyProvider`-based verify pipeline, whose closures capture
//!   on-chain RPC reads + `PyResult`) and delegates to this module, mapping
//!   its [`RefusalKind`] refusals to the typed Python exceptions.
//!
//! ## ADR-006 D4 relocation (this module's home)
//! These helpers were trapped in `solvers/arb_engine` for historical
//! ADR-005 reasons. D4 moves them onto the Bot / chain-pump layer — i.e.
//! `bot_core` — so the verify orchestrator is reachable from `PyBot` without
//! routing through the solver.

use hashbrown::HashMap;

use alloy::primitives::Address;

use crate::bot_core::registration_ledger::{RefusalKind, RegistrationPhase};
use degenbot_pools::TickInfo;
use degenbot_pools::V3PoolState;
use degenbot_pools::V4PoolState;

/// Error from `Bot::load_snapshot_from_db` / non-DB snapshot-load paths — the
/// DB read failure or the per-pool hex/tick decode failure. The refusal is
/// [`RefusalKind::SnapshotLoadFailed`] at the snapshot phase, carrying the
/// [`SnapshotLoadReason`] its consumer renders.
///
/// The verify pipeline's refusals are [`RefusalKind::VerifyFailed`] (with the
/// fatal-vs-transient classification riding `fatal` and the source on
/// [`VerifyOrigin`]) and [`RefusalKind::VerifyNotConfigured`]. The
/// [`VerifyRpc`] trait keeps the origins distinct so the bounded retry dance
/// (`crate::bot_core::verification_retry`) can branch on the fatal split and
/// the Python seam can fold the two transient origins into one retryable
/// exception type.
/// The minimal on-chain RPC surface the two-phase concentrated liquidity verification needs.
///
/// The pure orchestrator [`run_concentrated_liquidity_verification`] drives this trait without
/// knowing `AlloyProvider`, `tokio`, or `PyResult`. The single concrete impl
/// lives in `py_binding.rs`: it holds a cached `AlloyProvider`, lazily
/// constructs it from the configured `rpc_url`, `block_on`s the underlying
/// `crate::bot_core::liquidity_verifier` calls, and maps a map divergence
/// → [`VerifyOrigin::Mismatch`].
///
/// Methods are **sync** (the impl blocks internally):
/// keeping `async` out of the pure layer keeps `snapshot_verify.rs` tokio-free
/// at the type level.
pub trait VerifyRpc {
    /// Is on-chain verification configured (`rpc_url` set)? When `false`, the
    /// orchestrator skips both phases — the pure control-flow equivalent of
    /// the legacy `rpc_url: None` early return.
    fn enabled(&self) -> bool;

    /// Snapshot phase — verify a V3 pool's raw tick data (pre-buffer) against
    /// on-chain at `block`.
    ///
    /// # Errors
    ///
    /// Returns [`VerifyOrigin::Mismatch`] when the fetched on-chain tick data
    /// disagrees with `tick_data`.
    fn verify_v3_snapshot(
        &self,
        pool_address: Address,
        tick_data: &HashMap<i32, TickInfo>,
        block: u64,
    ) -> Result<(), RefusalKind>;

    /// Backfill phase — verify a V3 pool's post-buffer state against on-chain
    /// at `block`.
    ///
    /// # Errors
    ///
    /// Returns [`VerifyOrigin::Mismatch`] when the fetched on-chain state
    /// disagrees with `pools`.
    fn verify_v3_backfill(
        &self,
        pools: &HashMap<u64, V3PoolState>,
        block: u64,
    ) -> Result<(), RefusalKind>;

    /// Snapshot phase — verify a V4 pool's raw tick data (pre-buffer) against
    /// on-chain at `block`.
    ///
    /// # Errors
    ///
    /// Returns [`VerifyOrigin::Mismatch`] when the fetched on-chain tick data
    /// disagrees with `tick_data`.
    fn verify_v4_snapshot(
        &self,
        state_view: Address,
        pool_id: [u8; 32],
        tick_data: &HashMap<i32, TickInfo>,
        block: u64,
    ) -> Result<(), RefusalKind>;

    /// Backfill phase — verify a V4 pool's post-buffer state against on-chain
    /// at `block`.
    ///
    /// # Errors
    ///
    /// Returns [`VerifyOrigin::Mismatch`] when the fetched on-chain state
    /// disagrees with `pools`.
    fn verify_v4_backfill(
        &self,
        state_view: Address,
        pools: &HashMap<u64, V4PoolState>,
        block: u64,
    ) -> Result<(), RefusalKind>;
}

/// Run the two-phase concentrated liquidity verification (snapshot block + backfill block) if
/// configured, in order.
///
/// The CORE control flow is pure — decide whether to run (via
/// [`VerifyRpc::enabled`]) → run the snapshot-verify phase → run the
/// backfill-verify phase, in order. Only the `AlloyProvider`/`PyResult` I/O
/// stays behind in `py_binding.rs` (inside the `VerifyRpc` impl).
///
/// The per-call-site captured data (pool address, snapshot `tick_data`,
/// backfill pool map) is threaded through `verify_snapshot`/`verify_backfill`
/// closures that shrink to delegate to the trait — so the orchestrator stays
/// family-agnostic (one fn serves both the V3 and V4 register paths).
///
/// **Order is load-bearing**: the snapshot phase must run before the backfill
/// phase (a snapshot mismatch short-circuits before touching the post-buffer
/// state). Preserves the legacy `RuntimeError`-on-mismatch contract: a
/// mismatch refusal from either phase propagates immediately.
///
/// # Errors
///
/// Returns `Ok(())` when `VerifyRpc::enabled` is `false` or both phases pass.
/// Propagates the [`RefusalKind`] from `verify_snapshot` / `verify_backfill`
/// (a snapshot mismatch short-circuits before the backfill phase runs), or
/// [`RefusalKind::VerifyNotConfigured`] when verification is opted into but
/// never configured.
pub fn run_concentrated_liquidity_verification<R, F1, F2>(
    rpc: &R,
    snapshot_block: Option<u64>,
    backfill_block: Option<u64>,
    verify_snapshot: F1,
    verify_backfill: F2,
) -> Result<(), RefusalKind>
where
    R: VerifyRpc,
    F1: FnOnce(&R, u64) -> Result<(), RefusalKind>,
    F2: FnOnce(&R, u64) -> Result<(), RefusalKind>,
{
    if !rpc.enabled() {
        return Err(RefusalKind::verify_not_configured(
            RegistrationPhase::Verify,
            "verify_on_register is enabled but verification is not configured — \
             call set_verify_rpc_url() (and set_verify_state_view() for V4 pools) \
             before registering snapshot pools"
                .to_string(),
        ));
    }
    if let Some(block) = snapshot_block {
        verify_snapshot(rpc, block)?;
    }
    if let Some(block) = backfill_block {
        verify_backfill(rpc, block)?;
    }
    Ok(())
}

#[expect(clippy::panic)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::bot_core::registration_ledger::VerifyOrigin;
    use parking_lot::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    // -----------------------------------------------------------------
    // run_concentrated_liquidity_verification orchestrator
    // -----------------------------------------------------------------

    /// A fake `VerifyRpc` that records every call + can be made to fail at a
    /// chosen phase. No live RPC — drives the pure orchestrator's ordering,
    // early-return, and mismatch-propagation assertions.
    struct FakeVerifyRpc {
        enabled: bool,
        snapshot_calls: Mutex<Vec<u64>>,
        backfill_calls: Mutex<Vec<u64>>,
        /// `Some(refusal)` → the snapshot phase returns this refusal.
        fail_snapshot: Option<RefusalKind>,
        /// `Some(refusal)` → the backfill phase returns this refusal.
        fail_backfill: Option<RefusalKind>,
    }

    impl FakeVerifyRpc {
        fn new(enabled: bool) -> Self {
            Self {
                enabled,
                snapshot_calls: Mutex::new(Vec::new()),
                backfill_calls: Mutex::new(Vec::new()),
                fail_snapshot: None,
                fail_backfill: None,
            }
        }
    }

    impl VerifyRpc for FakeVerifyRpc {
        fn enabled(&self) -> bool {
            self.enabled
        }

        fn verify_v3_snapshot(
            &self,
            _pool_address: Address,
            _tick_data: &HashMap<i32, TickInfo>,
            block: u64,
        ) -> Result<(), RefusalKind> {
            self.snapshot_calls.lock().push(block);
            match &self.fail_snapshot {
                Some(refusal) => Err(refusal.clone()),
                None => Ok(()),
            }
        }

        fn verify_v3_backfill(
            &self,
            _pools: &HashMap<u64, V3PoolState>,
            block: u64,
        ) -> Result<(), RefusalKind> {
            self.backfill_calls.lock().push(block);
            match &self.fail_backfill {
                Some(refusal) => Err(refusal.clone()),
                None => Ok(()),
            }
        }

        fn verify_v4_snapshot(
            &self,
            _state_view: Address,
            _pool_id: [u8; 32],
            _tick_data: &HashMap<i32, TickInfo>,
            block: u64,
        ) -> Result<(), RefusalKind> {
            self.snapshot_calls.lock().push(block);
            match &self.fail_snapshot {
                Some(refusal) => Err(refusal.clone()),
                None => Ok(()),
            }
        }

        fn verify_v4_backfill(
            &self,
            _state_view: Address,
            _pools: &HashMap<u64, V4PoolState>,
            block: u64,
        ) -> Result<(), RefusalKind> {
            self.backfill_calls.lock().push(block);
            match &self.fail_backfill {
                Some(refusal) => Err(refusal.clone()),
                None => Ok(()),
            }
        }
    }

    /// A fatal verify mismatch at the verify phase.
    fn mismatch(detail: &str) -> RefusalKind {
        RefusalKind::verify_failed(
            RegistrationPhase::Verify,
            true,
            VerifyOrigin::Mismatch,
            detail.to_owned(),
        )
    }

    /// A transient verify failure at the verify phase.
    fn transient(origin: VerifyOrigin, detail: &str) -> RefusalKind {
        RefusalKind::verify_failed(RegistrationPhase::Verify, false, origin, detail.to_owned())
    }

    /// Opted-in-but-not-configured is a FATAL misconfiguration, not a silent
    /// skip. `run_concentrated_liquidity_verification` is only reached when the caller opted into
    /// verification (`verify_on_register = true`) for a snapshot-built
    /// (`Tracked`) pool. Reaching it with `enabled() == false` therefore means
    /// verification was requested but the RPC URL was never set — the bot must
    /// NOT proceed with an unverified tick map. Raises `NotConfigured` (loud)
    /// rather than returning `Ok(())` (silent), so "startup succeeded" can no
    /// longer mean "never verified".
    #[test]
    fn run_concentrated_liquidity_verification_errors_when_not_configured() {
        let rpc = FakeVerifyRpc::new(false);
        let snap = |_: &FakeVerifyRpc, b: u64| {
            panic!("snapshot phase must not run when not configured (block {b})")
        };
        let back = |_: &FakeVerifyRpc, b: u64| {
            panic!("backfill phase must not run when not configured (block {b})")
        };
        let res = run_concentrated_liquidity_verification(&rpc, Some(10), Some(20), snap, back);
        assert!(
            matches!(res, Err(RefusalKind::VerifyNotConfigured { .. })),
            "opted-in-but-no-url must raise NotConfigured, not return Ok"
        );
        assert!(rpc.snapshot_calls.lock().is_empty());
        assert!(rpc.backfill_calls.lock().is_empty());
    }

    /// Both phases run in order: snapshot at `snapshot_block`, then backfill at
    /// `backfill_block`.
    #[test]
    fn run_concentrated_liquidity_verification_runs_snapshot_then_backfill_in_order() {
        let rpc = FakeVerifyRpc::new(true);
        let order = Arc::new(AtomicUsize::new(0));
        let snap_order = Arc::clone(&order);
        let back_order = Arc::clone(&order);

        let snap = move |_: &FakeVerifyRpc, b: u64| {
            assert_eq!(
                snap_order.fetch_add(1, Ordering::SeqCst),
                0,
                "snapshot first"
            );
            assert_eq!(b, 10, "snapshot block");
            Ok(())
        };
        let back = move |_: &FakeVerifyRpc, b: u64| {
            assert_eq!(
                back_order.fetch_add(1, Ordering::SeqCst),
                1,
                "backfill second"
            );
            assert_eq!(b, 20, "backfill block");
            Ok(())
        };

        let res = run_concentrated_liquidity_verification(&rpc, Some(10), Some(20), snap, back);
        assert!(res.is_ok());
        assert_eq!(
            order.load(Ordering::SeqCst),
            2,
            "both phases ran exactly once"
        );
    }

    /// A snapshot-phase mismatch short-circuits before the backfill phase
    /// (order is load-bearing — a snapshot mismatch must NOT touch post-buffer
    /// state).
    #[test]
    fn run_concentrated_liquidity_verification_snapshot_mismatch_short_circuits() {
        let mut rpc = FakeVerifyRpc::new(true);
        rpc.fail_snapshot = Some(mismatch(
            "V3 pool 0x.. at snapshot block 10: tick data mismatch: lg=1 vs 2",
        ));

        let snap =
            |r: &FakeVerifyRpc, b: u64| r.verify_v3_snapshot(Address::ZERO, &HashMap::new(), b);
        let back = |_: &FakeVerifyRpc, b: u64| {
            panic!("backfill must not run after a snapshot mismatch (block {b})")
        };

        let res = run_concentrated_liquidity_verification(&rpc, Some(10), Some(20), snap, back);
        let Err(RefusalKind::VerifyFailed { origin, detail, .. }) = res else {
            panic!("a snapshot mismatch propagates: {res:?}");
        };
        assert_eq!(origin, VerifyOrigin::Mismatch);
        assert!(detail.contains("snapshot block 10"));
        assert_eq!(*rpc.snapshot_calls.lock(), vec![10]);
        assert!(rpc.backfill_calls.lock().is_empty(), "backfill skipped");
    }

    /// A backfill-phase mismatch propagates (snapshot phase ran first + ok).
    #[test]
    fn run_concentrated_liquidity_verification_backfill_mismatch_propagates() {
        let mut rpc = FakeVerifyRpc::new(true);
        rpc.fail_backfill = Some(mismatch(
            "V3 pool 0x.. at backfill block 20: tick data mismatch",
        ));

        let snap =
            |r: &FakeVerifyRpc, b: u64| r.verify_v3_snapshot(Address::ZERO, &HashMap::new(), b);
        let back = |r: &FakeVerifyRpc, b: u64| r.verify_v3_backfill(&HashMap::new(), b);

        let res = run_concentrated_liquidity_verification(&rpc, Some(10), Some(20), snap, back);
        let Err(RefusalKind::VerifyFailed { origin, detail, .. }) = res else {
            panic!("a backfill mismatch propagates: {res:?}");
        };
        assert_eq!(origin, VerifyOrigin::Mismatch);
        assert!(detail.contains("backfill block 20"));
        assert_eq!(*rpc.snapshot_calls.lock(), vec![10]);
        assert_eq!(*rpc.backfill_calls.lock(), vec![20]);
    }

    /// A provider construction failure from the snapshot phase propagates as
    /// a provider-construction refusal and still short-circuits the backfill
    /// phase.
    #[test]
    fn run_concentrated_liquidity_verification_provider_error_short_circuits() {
        let mut rpc = FakeVerifyRpc::new(true);
        rpc.fail_snapshot = Some(transient(
            VerifyOrigin::ProviderConstruction,
            "verify: 0x..: failed to create provider: connection refused",
        ));

        let snap =
            |r: &FakeVerifyRpc, b: u64| r.verify_v3_snapshot(Address::ZERO, &HashMap::new(), b);
        let back = |_: &FakeVerifyRpc, b: u64| {
            panic!("backfill must not run after provider failure (block {b})")
        };

        let res = run_concentrated_liquidity_verification(&rpc, Some(10), Some(20), snap, back);
        assert!(matches!(
            res,
            Err(RefusalKind::VerifyFailed {
                origin: VerifyOrigin::ProviderConstruction,
                ..
            })
        ));
        assert!(rpc.backfill_calls.lock().is_empty());
    }

    /// a per-call RPC transport failure from the snapshot phase
    /// propagates as a transport refusal — NOT flattened to a mismatch (which
    /// would conflate a transient transport error with a genuine mismatch and
    /// surface as `VerificationMismatchError` at the Python seam). The
    /// orchestrator must preserve the variant so `map_verify_err` can route it
    /// to `VerificationRpcError` (retryable) distinct from
    /// `VerificationMismatchError` (fatal).
    #[test]
    fn run_concentrated_liquidity_verification_rpc_error_propagates_unflattened() {
        let mut rpc = FakeVerifyRpc::new(true);
        rpc.fail_snapshot = Some(transient(
            VerifyOrigin::Transport,
            "V3 pool 0x.. at snapshot block 10: tickBitmap(0) RPC call failed: timeout",
        ));

        let snap =
            |r: &FakeVerifyRpc, b: u64| r.verify_v3_snapshot(Address::ZERO, &HashMap::new(), b);
        let back = |_: &FakeVerifyRpc, b: u64| {
            panic!("backfill must not run after a snapshot RPC failure (block {b})")
        };

        let res = run_concentrated_liquidity_verification(&rpc, Some(10), Some(20), snap, back);
        let Err(RefusalKind::VerifyFailed { origin, detail, .. }) = res else {
            panic!("a per-call RPC transport failure propagates: {res:?}");
        };
        assert_eq!(
            origin,
            VerifyOrigin::Transport,
            "per-call RPC transport failure must not flatten to a mismatch"
        );
        assert!(detail.contains("RPC call failed"));
        assert_eq!(*rpc.snapshot_calls.lock(), vec![10]);
        assert!(
            rpc.backfill_calls.lock().is_empty(),
            "backfill skipped after snapshot RPC failure"
        );
    }

    /// a per-call RPC transport failure from the backfill phase
    /// propagates as a transport refusal (snapshot phase ran first + ok).
    #[test]
    fn run_concentrated_liquidity_verification_backfill_rpc_error_propagates_unflattened() {
        let mut rpc = FakeVerifyRpc::new(true);
        rpc.fail_backfill = Some(transient(
            VerifyOrigin::Transport,
            "V3 pool 0x.. at backfill block 20: ticks(100) RPC call failed: connection reset",
        ));

        let snap =
            |r: &FakeVerifyRpc, b: u64| r.verify_v3_snapshot(Address::ZERO, &HashMap::new(), b);
        let back = |r: &FakeVerifyRpc, b: u64| r.verify_v3_backfill(&HashMap::new(), b);

        let res = run_concentrated_liquidity_verification(&rpc, Some(10), Some(20), snap, back);
        let Err(RefusalKind::VerifyFailed { origin, detail, .. }) = res else {
            panic!("a backfill RPC transport failure propagates: {res:?}");
        };
        assert_eq!(origin, VerifyOrigin::Transport);
        assert!(detail.contains("backfill block 20") && detail.contains("RPC call failed"));
        assert_eq!(*rpc.snapshot_calls.lock(), vec![10]);
        assert_eq!(*rpc.backfill_calls.lock(), vec![20]);
    }

    /// Missing snapshot block is a no-op for the snapshot phase but the
    /// backfill phase still runs (mirrors the legacy per-`Option<u64>` guards).
    #[test]
    fn run_concentrated_liquidity_verification_missing_snapshot_block_still_runs_backfill() {
        let rpc = FakeVerifyRpc::new(true);
        let snap = |_: &FakeVerifyRpc, b: u64| {
            panic!("snapshot phase must not run when its block is None (block {b})")
        };
        let back = |r: &FakeVerifyRpc, b: u64| r.verify_v3_backfill(&HashMap::new(), b);

        let res = run_concentrated_liquidity_verification(&rpc, None, Some(20), snap, back);
        assert!(res.is_ok());
        assert!(rpc.snapshot_calls.lock().is_empty());
        assert_eq!(*rpc.backfill_calls.lock(), vec![20]);
    }

    /// A `NotConfigured` returned by an opted-in phase (e.g. a V4 snapshot
    /// closure that found `state_view` unset) propagates to the caller — it is
    /// NOT swallowed. The register.rs V4 closures rely on this to surface a
    /// missing `set_verify_state_view()` as a loud fatal error.
    #[test]
    fn run_concentrated_liquidity_verification_propagates_not_configured_from_phase() {
        let rpc = FakeVerifyRpc::new(true);
        let snap = |_: &FakeVerifyRpc, _b: u64| {
            Err(RefusalKind::verify_not_configured(
                RegistrationPhase::Verify,
                "missing set_verify_state_view() for V4 pool".to_string(),
            ))
        };
        let back = |_: &FakeVerifyRpc, b: u64| {
            panic!("backfill must not run after a NotConfigured (block {b})")
        };

        let res = run_concentrated_liquidity_verification(&rpc, Some(10), Some(20), snap, back);
        let Err(RefusalKind::VerifyNotConfigured { detail, .. }) = res else {
            panic!("NotConfigured from a phase must propagate: {res:?}");
        };
        assert!(detail.contains("set_verify_state_view"));
    }
}
