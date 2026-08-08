//! Registration verify-lifecycle core ownership (IKGQ6F / ADR-022).
//!
//! Owns the per-pool `quarantine → drain+pin → two-step verify → Live`
//! sequence that was previously orchestrated in Python
//! (`src/degenbot/arbitrage/engine_registry.py::register_v3/v4_pool`).
//!
//! Decision D4 (lifecycle), per ADR-022:
//! - a **sparse** pool is `Live` immediately, receives **no verification
//!   deferral and no RPC** (DFQYM5 — it stays `Live`);
//! - a **tracked** pool is `Live` only after its liquidity-map verification
//!   passes, with the verification `MismatchError` as the **tripwire** that
//!   must pass before `Live` (no pool becomes solvable on unverified state).
//!
//! The module is deliberately **runtime-agnostic**: the two RPC-bound verify
//! steps (`verify_seed`, `verify_post_drain`) are supplied as async closures,
//! so the core state-machine is unit-testable without a live provider. The
//! concrete provider-backed adapters (`run_v3_registration_lifecycle` /
//! `run_v4_registration_lifecycle`) wire them to `liquidity_verifier` with the
//! bot's **single** `AlloyProvider` passed-in (ADR-022 D3 — one provider per
//! bot; never stored on `BotState`, which is I/O-free per ADR-001).
//!
//! **Lock discipline (preserved exactly):** no guard is held across the
//! `verify_seed` / `verify_post_drain` `.await`s (take-pin-then-drop); the
//! drain + pin run under ONE `core.write()` hold (the step-2 rolling-start race
//! fix); step-1 verifies the pinned snapshot seed @ snapshot block (CBCH6H);
//! step-2 verifies the pin's own captured block (never a constant backfill
//! block — the 2026-06-29 crash class).

use std::collections::HashMap;
use std::future::Future;

use alloy::primitives::Address;
use parking_lot::RwLock;

use degenbot_decoders::v4_swap_decoder::V4PoolId;
use degenbot_rpc::provider::AlloyProvider;

use super::liquidity_verifier::{
    verify_v3_liquidity_map, verify_v4_liquidity_map, LiquidityVerifyError,
};
use super::{BotState, PoolTickCoverage, TickInfo};

/// Error from a concrete registration-lifecycle run.
///
/// - [`RegistrationLifecycleError::Verify`] wraps the underlying liquidity-verify
///   error. A [`LiquidityVerifyError::Mismatch`] is the **fatal tripwire** that
///   must block `Live` (never auto-repair); an [`LiquidityVerifyError::Rpc`] is
///   a transient transport failure.
/// - [`RegistrationLifecycleError::MissingStateView`] is the D-C no-config
///   fail-fast: a **tracked** V4 pool requires a `state_view` contract address
///   (the `eth_call` target for V4 verification) and none was supplied. An
///   unverifiable tracked pool must never reach `Live`.
#[derive(Debug)]
pub enum RegistrationLifecycleError {
    /// A seed or post-drain verify step failed (mismatch = fatal; rpc =
    /// transient).
    Verify(LiquidityVerifyError),
    /// A tracked V4 pool needs a `state_view` address but none was supplied.
    MissingStateView,
    /// A **tracked** pool needs a verify provider but none was configured
    /// (D-C fail-fast: no verify-disabled mode for tracked). Only fires when a
    /// Tracked pool actually reaches a verify step — Sparse / unregistered /
    /// no-pin no-op paths never need a provider.
    MissingProvider,
}

impl std::fmt::Display for RegistrationLifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistrationLifecycleError::Verify(e) => write!(f, "registration verify failed: {e}"),
            RegistrationLifecycleError::MissingStateView => write!(
                f,
                "registration verify requires a StateView contract address for V4 pools"
            ),
            RegistrationLifecycleError::MissingProvider => write!(
                f,
                "registration verify requires an RPC provider for tracked pools — configure the bot's single provider"
            ),
        }
    }
}

impl std::error::Error for RegistrationLifecycleError {}

/// Drive a V3 CL pool through the registration verify-lifecycle, branching on
/// coverage (D4 / DFQYM5): **Sparse → immediate no-op** (already `Live`, no
/// verification deferral, no RPC); **Tracked → quarantine → (step-1 seed
/// verify) → drain+pin → (step-2 post-drain verify) → `set_live`**.
///
/// The two RPC-bound verify steps are supplied as async closures so the
/// state-machine is testable without a live provider. Any `Err` from a verify
/// closure aborts BEFORE `set_live` (the tripwire — never auto-repair); a
/// Sparse pool never invokes either closure.
///
/// *Passed-in by the caller*: a core Rust consumer or the `PyO3` wrapper drives
/// this on whatever tokio runtime owns registration; `core` is the `BotState`
/// `RwLock`, and the closures capture the verify access (provider).
///
/// # Lock-scope contract
///
/// No guard is held across the `verify_seed`/`verify_post_drain` `.await`s
/// (each take runs under a short `core.write()` then drops it), and the drain
/// (`apply_backfill_buffer_v3` → `apply_pump_buffer_v3`) + pin run under ONE
/// `core.write()` hold — the step-2 rolling-start race fix.
///
/// # Errors
///
/// Returns the closure error type `E` if either verify closure returns `Err`
/// — a verification failure aborts BEFORE `set_live`, leaving the tracked pool
/// `Quarantined` (the tripwire; never auto-repair). No error on the Sparse /
/// no-op paths.
pub async fn run_cl_v3_lifecycle<F1, Fut1, F2, Fut2, E>(
    core: &RwLock<BotState>,
    address: Address,
    snapshot_block: Option<u64>,
    verify_seed: F1,
    verify_post_drain: F2,
) -> Result<(), E>
where
    F1: FnOnce(HashMap<i32, TickInfo>, u64) -> Fut1 + Send,
    Fut1: Future<Output = Result<(), E>> + Send,
    F2: FnOnce(HashMap<i32, TickInfo>, u64) -> Fut2 + Send,
    Fut2: Future<Output = Result<(), E>> + Send,
{
    // Coverage branch up-front (D4 / DFQYM5). A Sparse pool stays `Live`,
    // receives NO verification deferral and NO verify RPC, but its buffered
    // backfill/pump events (buffered while the pool was still unregistered)
    // must still be drained onto the pool — draining is not verification and
    // preserves the perm-V2-V2-V3 apply-buffer behavior. An unregistered /
    // non-V3 pool → no-op Ok.
    let coverage = core.read().v3_pool_coverage(address);
    match coverage {
        None => return Ok(()),
        Some(PoolTickCoverage::Sparse) => {
            let mut guard = core.write();
            guard.apply_backfill_buffer_v3(&address);
            guard.apply_pump_buffer_v3(&address);
            return Ok(());
        }
        Some(PoolTickCoverage::Tracked) => {}
    }

    // Quarantine BEFORE the first RPC await (6N7XVR): defers the pool's live
    // Swap/Mint/Burn to the pump buffer so the pin's `update_block` cannot
    // outrun `last_complete_block` during the drain+pin+verify window.
    core.write().set_v3_pool_quarantined(address);

    // Step-1: verify the pinned snapshot SEED @ snapshot block (CBCH6H). Only
    // when a snapshot block is supplied (the seam's gated-skip posture); the
    // seed is consumed exactly once so memory is bounded. The comparison is
    // seed-vs-on-chain@snapshot, NOT engine-current (which would
    // false-mismatch every active pool under a rolling start).
    if let Some(snapshot_block) = snapshot_block {
        let seed = { core.write().take_v3_snapshot_seed(address) };
        if let Some(seed) = seed {
            verify_seed(seed, snapshot_block).await?;
        }
    }

    // Drain + pin under a SINGLE `core.write()` hold: backfill buffer, then
    // pump buffer, then capture the frozen post-drain `(tick_data, block)`
    // pair atomically with the drain (the step-2 rolling-start race fix).
    {
        let mut guard = core.write();
        guard.apply_backfill_buffer_v3(&address);
        guard.apply_pump_buffer_v3(&address);
        guard.pin_v3_post_drain_snapshot(address);
    }

    // Step-2: verify the pinned POST-DRAIN pair @ the pin's OWN captured block
    // (the `tick_data_block` — liquidity clock, two-stamp OB7UNY). Comparing
    // against a caller-supplied constant would fabricate a mismatch on active
    // pools (the 2026-06-29 crash). The pin is consumed exactly once.
    let pin = { core.write().take_v3_post_drain_snapshot(address) };
    if let Some((tick_data, pinned_block)) = pin {
        verify_post_drain(tick_data, pinned_block).await?;
    }

    // Tripwire passed (ADR-022 D2) — the final gate before `Live`. Reaching
    // here means a Tracked pool's verification succeeded; `Live` is the last
    // transition.
    core.write().set_v3_pool_live(address);
    Ok(())
}

/// V4 twin of [`run_cl_v3_lifecycle`] — same D4 coverage branch + two-step
/// verify + tripwire-gated `Live`, keyed by (`pool_manager`, `pool_id`).
///
/// Same lock-scope contract: no guard across the verify `.await`s; drain + pin
/// under ONE `core.write()` hold.
///
/// # Errors
///
/// Returns the closure error type `E` if either verify closure returns `Err`
/// — a verification failure aborts before `set_live`, leaving the tracked pool
/// `Quarantined`.
pub async fn run_cl_v4_lifecycle<F1, Fut1, F2, Fut2, E>(
    core: &RwLock<BotState>,
    pool_manager: Address,
    pool_id: V4PoolId,
    snapshot_block: Option<u64>,
    verify_seed: F1,
    verify_post_drain: F2,
) -> Result<(), E>
where
    F1: FnOnce(HashMap<i32, TickInfo>, u64) -> Fut1 + Send,
    Fut1: Future<Output = Result<(), E>> + Send,
    F2: FnOnce(HashMap<i32, TickInfo>, u64) -> Fut2 + Send,
    Fut2: Future<Output = Result<(), E>> + Send,
{
    // Coverage branch up-front (DFQYM5). A Sparse V4 pool stays `Live`, no
    // verification deferral / RPC, but its buffered events are still drained;
    // unregistered / non-V4 → no-op Ok.
    let coverage = core.read().v4_pool_coverage(pool_manager, &pool_id);
    match coverage {
        None => return Ok(()),
        Some(PoolTickCoverage::Sparse) => {
            let mut guard = core.write();
            guard.apply_backfill_buffer_v4(pool_manager, pool_id);
            guard.apply_pump_buffer_v4(pool_manager, pool_id);
            return Ok(());
        }
        Some(PoolTickCoverage::Tracked) => {}
    }

    // Quarantine before the first RPC await (6N7XVR).
    core.write().set_v4_pool_quarantined(pool_manager, pool_id);

    // Step-1: verify the pinned snapshot seed @ snapshot block (CBCH6H).
    if let Some(snapshot_block) = snapshot_block {
        let seed = { core.write().take_v4_snapshot_seed(pool_manager, &pool_id) };
        if let Some(seed) = seed {
            verify_seed(seed, snapshot_block).await?;
        }
    }

    // Drain + pin under a SINGLE `core.write()` hold (step-2 race fix).
    {
        let mut guard = core.write();
        guard.apply_backfill_buffer_v4(pool_manager, pool_id);
        guard.apply_pump_buffer_v4(pool_manager, pool_id);
        guard.pin_v4_post_drain_snapshot(pool_manager, &pool_id);
    }

    // Step-2: verify the pinned post-drain pair @ the pin's OWN block.
    let pin = {
        core.write()
            .take_v4_post_drain_snapshot(pool_manager, &pool_id)
    };
    if let Some((tick_data, pinned_block)) = pin {
        verify_post_drain(tick_data, pinned_block).await?;
    }

    // Tripwire passed → Live.
    core.write().set_v4_pool_live(pool_manager, pool_id);
    Ok(())
}

/// Concrete V3 registration-lifecycle: same choreography as
/// [`run_cl_v3_lifecycle`], but the two verify steps run through
/// `liquidity_verifier::verify_v3_liquidity_map` against the bot's **single**
/// `AlloyProvider` (ADR-022 D3 — one provider per bot/chain, passed-in). V3
/// needs no `state_view` (the per-pool verify reads `pool.ticks()` directly).
/// A `None` provider only matters for a **Tracked** pool (which reaches a
/// verify step and fails fast with
/// [`RegistrationLifecycleError::MissingProvider`] — D-C); Sparse /
/// unregistered no-op paths never need one.
///
/// # Errors
///
/// Returns [`RegistrationLifecycleError`]: *Verify* wraps the liquidity-verify
/// error (mismatch = fatal tripwire), `MissingProvider` when a tracked pool
/// needs to verify with no provider configured.
pub async fn run_v3_registration_lifecycle(
    core: &RwLock<BotState>,
    provider: Option<&AlloyProvider>,
    address: Address,
    snapshot_block: Option<u64>,
) -> Result<(), RegistrationLifecycleError> {
    let provider = provider.cloned();
    run_cl_v3_lifecycle(
        core,
        address,
        snapshot_block,
        |seed, block| {
            let provider = provider.clone();
            async move {
                let p = provider.ok_or(RegistrationLifecycleError::MissingProvider)?;
                verify_v3_liquidity_map(&p, address, &seed, block)
                    .await
                    .map_err(RegistrationLifecycleError::Verify)
            }
        },
        |tick_data, block| {
            let provider = provider.clone();
            async move {
                let p = provider.ok_or(RegistrationLifecycleError::MissingProvider)?;
                verify_v3_liquidity_map(&p, address, &tick_data, block)
                    .await
                    .map_err(RegistrationLifecycleError::Verify)
            }
        },
    )
    .await
}

/// Concrete V4 registration-lifecycle: V4 twin of
/// [`run_v3_registration_lifecycle`], with the `state_view` contract address
/// required for the on-chain comparison. A **tracked** V4 pool with no
/// `state_view` yields [`RegistrationLifecycleError::MissingStateView`] (D-C
/// no-config fail-fast — a tracked pool never reaches `Live` unverified);
/// Sparse pools never reach the verify step and never require `state_view`.
///
/// # Errors
///
/// Returns [`RegistrationLifecycleError`]: *Verify* wraps the liquidity-verify
/// error (mismatch = fatal tripwire), `MissingStateView` when a tracked V4
/// pool is verified with no `state_view` supplied.
pub async fn run_v4_registration_lifecycle(
    core: &RwLock<BotState>,
    provider: Option<&AlloyProvider>,
    pool_manager: Address,
    pool_id: V4PoolId,
    state_view: Option<Address>,
    snapshot_block: Option<u64>,
) -> Result<(), RegistrationLifecycleError> {
    let provider = provider.cloned();
    run_cl_v4_lifecycle(
        core,
        pool_manager,
        pool_id,
        snapshot_block,
        |seed, block| {
            let provider = provider.clone();
            async move {
                let p = provider.ok_or(RegistrationLifecycleError::MissingProvider)?;
                let state_view = state_view.ok_or(RegistrationLifecycleError::MissingStateView)?;
                verify_v4_liquidity_map(&p, state_view, pool_id, &seed, block)
                    .await
                    .map_err(RegistrationLifecycleError::Verify)
            }
        },
        |tick_data, block| {
            let provider = provider.clone();
            async move {
                let p = provider.ok_or(RegistrationLifecycleError::MissingProvider)?;
                let state_view = state_view.ok_or(RegistrationLifecycleError::MissingStateView)?;
                verify_v4_liquidity_map(&p, state_view, pool_id, &tick_data, block)
                    .await
                    .map_err(RegistrationLifecycleError::Verify)
            }
        },
    )
    .await
}

#[expect(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use alloy::primitives::{Address, I256, U128, U256};
    use parking_lot::RwLock;

    use degenbot_decoders::v4_swap_decoder::V4PoolId;
    use degenbot_pools::v4_state::V4PoolKey;

    use super::{run_cl_v3_lifecycle, run_cl_v4_lifecycle, RegistrationLifecycleError};
    use crate::bot_core::{
        BotState, PoolTickCoverage, RegisterV3PoolParams, RegisterV4PoolParams,
        RegistrationLifecycle, TickInfo,
    };

    fn new_core() -> Arc<RwLock<BotState>> {
        Arc::new(RwLock::new(BotState::new()))
    }

    fn reg_v3(core: &mut BotState, address: Address, coverage: PoolTickCoverage) -> u64 {
        let mut tick_data = HashMap::new();
        if coverage == PoolTickCoverage::Tracked {
            tick_data.insert(
                60,
                TickInfo {
                    liquidity_gross: U128::from(100),
                    liquidity_net: I256::try_from(100i128).unwrap(),
                    block: 0,
                },
            );
        }
        core.register_v3_pool(&RegisterV3PoolParams {
            address,
            token0: Address::ZERO,
            token1: Address::from([1u8; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 0,
            tick_data_block: None,
            coverage,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 registration")
    }

    fn reg_v4(core: &mut BotState, pm: Address, pid: V4PoolId, coverage: PoolTickCoverage) -> u64 {
        let mut tick_data = HashMap::new();
        if coverage == PoolTickCoverage::Tracked {
            tick_data.insert(
                60,
                TickInfo {
                    liquidity_gross: U128::from(100),
                    liquidity_net: I256::try_from(100i128).unwrap(),
                    block: 0,
                },
            );
        }
        core.register_v4_pool(&RegisterV4PoolParams {
            pool_manager: pm,
            pool_id: pid,
            pool_key: V4PoolKey {
                currency0: Address::ZERO,
                currency1: Address::from([1u8; 20]),
                fee: 500,
                tick_spacing: 10,
                hooks: Address::ZERO,
            },
            hook_flags: 0,
            protocol_fee: 0,
            sqrt_price_x96: U256::from(1u128) << 96,
            liquidity: 1_000_000,
            tick: 0,
            tick_data,
            update_block: 0,
            tick_data_block: None,
            coverage,
            fetcher: None,
        })
        .expect("test setup: V4 registration")
    }

    fn lifecycle_v3(core: &BotState, pool_id: u64) -> RegistrationLifecycle {
        core.get_v3_pool(pool_id)
            .expect("registered V3 pool")
            .registration_lifecycle
    }

    fn lifecycle_v4(core: &BotState, pool_id: u64) -> RegistrationLifecycle {
        core.get_v4_pool(pool_id)
            .expect("registered V4 pool")
            .registration_lifecycle
    }

    /// A Sparse V3 pool goes straight to the end: the lifecycle is an
    /// immediate no-op — `Live`, no verify closure invoked, no RPC (DFQYM5).
    #[tokio::test]
    async fn sparse_v3_lifecycle_is_immediate_no_verify() {
        let core = new_core();
        let addr = Address::from([0x10u8; 20]);
        let pid = {
            let mut c = core.write();
            reg_v3(&mut c, addr, PoolTickCoverage::Sparse)
        };
        // Both closures `unreachable!` — the invariant is that Sparse never
        // invokes the verify RPC path.
        let result = run_cl_v3_lifecycle::<_, _, _, _, ()>(
            &core,
            addr,
            Some(123),
            |_, _| async move { unreachable!("no seed verify for Sparse") },
            |_, _| async move { unreachable!("no post-drain verify for Sparse") },
        )
        .await;
        assert!(result.is_ok(), "sparse lifecycle must be Ok");
        let c = core.read();
        assert_eq!(lifecycle_v3(&c, pid), RegistrationLifecycle::Live);
    }

    /// A Sparse V4 pool is also an immediate no-op (no RPC).
    #[tokio::test]
    async fn sparse_v4_lifecycle_is_immediate_no_verify() {
        let core = new_core();
        let pm = Address::from([0x44u8; 20]);
        let pid = [0xabu8; 32];
        {
            let mut c = core.write();
            reg_v4(&mut c, pm, pid, PoolTickCoverage::Sparse);
        }
        let result = run_cl_v4_lifecycle::<_, _, _, _, ()>(
            &core,
            pm,
            pid,
            Some(123),
            |_, _| async move { unreachable!("no seed verify for Sparse") },
            |_, _| async move { unreachable!("no post-drain verify for Sparse") },
        )
        .await;
        assert!(result.is_ok());
    }

    /// A Tracked V3 pool is deferred: it must run seed verify (@ snapshot
    /// block), drain+pin, post-drain verify (@ the pin's own block), then
    /// reach `Live` — with both verify closures fed the correct data+block.
    #[tokio::test]
    async fn tracked_v3_verifies_seed_then_post_drain_then_live() {
        let core = new_core();
        let addr = Address::from([0x20u8; 20]);
        let pid = {
            let mut c = core.write();
            reg_v3(&mut c, addr, PoolTickCoverage::Tracked)
        };
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_seed = Arc::clone(&calls);
        let calls_post = Arc::clone(&calls);
        let result = run_cl_v3_lifecycle::<_, _, _, _, ()>(
            &core,
            addr,
            Some(42),
            move |seed, block| {
                let calls = calls_seed;
                async move {
                    calls.lock().unwrap().push(("seed", block, seed.len()));
                    Ok(())
                }
            },
            move |tick_data, block| {
                let calls = calls_post;
                async move {
                    calls.lock().unwrap().push(("post", block, tick_data.len()));
                    Ok(())
                }
            },
        )
        .await;
        assert!(result.is_ok());
        let c = core.read();
        assert_eq!(lifecycle_v3(&c, pid), RegistrationLifecycle::Live);
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2, "both verify closures must run for Tracked");
        assert_eq!(calls[0].0, "seed");
        assert_eq!(calls[0].1, 42, "seed verify runs @ snapshot block");
        assert_eq!(calls[1].0, "post");
        // post-drain verify runs @ the pin's OWN block (the drain block), NOT
        // the snapshot block 42 — the rolling-start race fix.
        assert_ne!(calls[1].1, 42);
    }

    /// A Tracked V3 pool whose seed verify mismatches must NEVER reach `Live`
    /// — the mismatch is the tripwire (ADR-022 D2, no auto-repair).
    #[tokio::test]
    async fn tracked_v3_seed_mismatch_tripwire_blocks_live() {
        let core = new_core();
        let addr = Address::from([0x30u8; 20]);
        let pid = {
            let mut c = core.write();
            reg_v3(&mut c, addr, PoolTickCoverage::Tracked)
        };
        let result = run_cl_v3_lifecycle::<_, _, _, _, String>(
            &core,
            addr,
            Some(42),
            move |_, _| async move { Err("seed mismatch".to_string()) },
            move |_, _| async move { unreachable!("post-drain must not run after seed fail") },
        )
        .await;
        assert!(result.is_err());
        let c = core.read();
        assert_eq!(
            lifecycle_v3(&c, pid),
            RegistrationLifecycle::Quarantined,
            "a mismatched tracked pool must stay Quarantined, never Live"
        );
    }

    /// A Tracked V3 pool whose post-drain verify mismatches must ALSO never
    /// reach `Live` (the post-drain step-2 is the tripwire's final gate).
    #[tokio::test]
    async fn tracked_v3_post_drain_mismatch_tripwire_blocks_live() {
        let core = new_core();
        let addr = Address::from([0x31u8; 20]);
        let pid = {
            let mut c = core.write();
            reg_v3(&mut c, addr, PoolTickCoverage::Tracked)
        };
        let result = run_cl_v3_lifecycle::<_, _, _, _, String>(
            &core,
            addr,
            Some(42),
            move |_, _| async move { Ok(()) },
            move |_, _| async move { Err("post-drain mismatch".to_string()) },
        )
        .await;
        assert!(result.is_err());
        let c = core.read();
        assert_eq!(
            lifecycle_v3(&c, pid),
            RegistrationLifecycle::Quarantined,
            "post-drain mismatch must also keep the pool Quarantined"
        );
    }

    /// The drain invariant that moved core-side (the perm-V2-V2-V3 class): a
    /// backfill Mint/Burn buffered before the pool was registered must be
    /// drained onto the pool's snapshot seed by the lifecycle. A tracked pool
    /// (Quarantined) with a buffered Burn: after the lifecycle, the burned
    /// tick's zeroed liquidity is `REMOVED` from `tick_data` (it is not stranded
    /// in the buffer) — exactly what the on-chain verifier reproduces.
    #[tokio::test]
    async fn tracked_v3_lifecycle_drains_buffered_backfill() {
        let core = new_core();
        let addr = Address::from([0x50u8; 20]);
        let pid = {
            let mut c = core.write();
            // Seed tick -201000 with gross/net 100 (a snapshot seed).
            reg_v3(&mut c, addr, PoolTickCoverage::Tracked)
        };
        // A Burn during backfill, BEFORE the pool is live-registered: the pool
        // is Quarantined (Tracked) so this BUFFERS rather than applies.
        {
            let mut c = core.write();
            c.buffer_backfill_v3_liquidity_update(addr, -201_000, -200_990, -100, 5);
            // Verify it buffered (Quarantined → not applied yet).
            assert_eq!(
                c.get_v3_pool(pid).unwrap().tick_data.len(),
                1,
                "must be buffered"
            );
        }
        let result = run_cl_v3_lifecycle::<_, _, _, _, ()>(
            &core,
            addr,
            None, // no snapshot block → step-1 skipped; step-2 (post-drain) still runs
            |_, _| async move { Ok(()) },
            |_, _| async move { Ok(()) },
        )
        .await;
        assert!(result.is_ok());
        let c = core.read();
        let state = c.get_v3_pool(pid).unwrap();
        // The burn zeroed gross → tick removed (not stranded in the buffer).
        assert!(
            !state.tick_data.contains_key(&-201_000),
            "buffered burn must be drained onto the seed, removing the zeroed tick"
        );
    }

    /// A missing `state_view` for a TRACKED V4 pool is the D-C no-config
    /// fail-fast: the tripwire fires and the pool never reaches `Live`.
    #[tokio::test]
    async fn tracked_v4_missing_state_view_blocks_live() {
        let core = new_core();
        let pm = Address::from([0x44u8; 20]);
        let pid = [0xbu8; 32];
        {
            let mut c = core.write();
            reg_v4(&mut c, pm, pid, PoolTickCoverage::Tracked);
        }
        // Emulate the production adapter's closure: no state_view → Err.
        let result = run_cl_v4_lifecycle::<_, _, _, _, RegistrationLifecycleError>(
            &core,
            pm,
            pid,
            Some(42),
            |_, _| async move { Err(RegistrationLifecycleError::MissingStateView) },
            |_, _| async move { unreachable!("post-drain must not run after seed fail") },
        )
        .await;
        assert!(matches!(
            result,
            Err(RegistrationLifecycleError::MissingStateView)
        ));
        let c = core.read();
        assert_eq!(
            c.v4_pool_id_by_key(pm, &pid).map(|id| lifecycle_v4(&c, id)),
            Some(RegistrationLifecycle::Quarantined),
            "tracked V4 without state_view must stay Quarantined"
        );
    }

    /// WSLCD2: the terminal `release_all_v3_v4_quarantined` is an ORPHAN sweep
    /// only — it must NOT be the productivity gate. A tracked pool released to
    /// `Live` by the per-path lifecycle stays `Live` (and solvable) regardless
    /// of whether the terminal batch ever runs; the batch only touches pools
    /// still `Quarantined` (orphaned: built but whose path never completed
    /// registration). This pins that per-path release does not duplicate or
    /// depend on the batch release policy.
    #[tokio::test]
    async fn per_path_released_pool_is_untouched_by_orphan_sweep() {
        let core = new_core();
        // A tracked V3 pool released per-path via the lifecycle.
        let tracked_addr = Address::from([0x60u8; 20]);
        let tracked_pid = {
            let mut c = core.write();
            reg_v3(&mut c, tracked_addr, PoolTickCoverage::Tracked)
        };
        // A sparse V3 pool (already Live, never quarantined).
        let sparse_addr = Address::from([0x61u8; 20]);
        let sparse_pid = {
            let mut c = core.write();
            reg_v3(&mut c, sparse_addr, PoolTickCoverage::Sparse)
        };
        // A genuinely orphaned tracked V4 (never released by any per-path
        // lifecycle) — the only pool the terminal sweep should flush.
        let orphan_vm = Address::from([0x62u8; 20]);
        let orphan_pid = [0xcu8; 32];
        {
            let mut c = core.write();
            reg_v4(&mut c, orphan_vm, orphan_pid, PoolTickCoverage::Tracked);
        }

        // Run the per-path lifecycle on the tracked V3 -> Live.
        run_cl_v3_lifecycle::<_, _, _, _, ()>(
            &core,
            tracked_addr,
            Some(42),
            |_, _| async move { Ok(()) },
            |_, _| async move { Ok(()) },
        )
        .await
        .expect("tracked per-path lifecycle must release to Live");

        // Confirm the productive pools are Live BEFORE any batch runs.
        {
            let c = core.read();
            assert_eq!(lifecycle_v3(&c, tracked_pid), RegistrationLifecycle::Live);
            assert_eq!(lifecycle_v3(&c, sparse_pid), RegistrationLifecycle::Live);
        }

        // The terminal orphan sweep runs (as it would after a single-pass
        // discovery completes). It must NOT re-touch the per-path Live pools
        // and must flush only the orphaned quarantined V4.
        {
            let mut c = core.write();
            c.release_all_v3_v4_quarantined();
        }

        let c = core.read();
        // The per-path gate held: both productive pools are still Live (the
        // batch did not duplicate release nor silently quarantine them).
        assert_eq!(lifecycle_v3(&c, tracked_pid), RegistrationLifecycle::Live);
        assert_eq!(lifecycle_v3(&c, sparse_pid), RegistrationLifecycle::Live);
        // The orphan was swept by the batch (the only legitimate use).
        assert_eq!(
            lifecycle_v4(&c, c.v4_pool_id_by_key(orphan_vm, &orphan_pid).unwrap()),
            RegistrationLifecycle::Live
        );
    }

    /// Regression for the lock-scope contract: while a tracked pool's verify
    /// is `.await`-ing (its RPC stand-in), a concurrent writer on `BotState`
    /// must NOT deadlock — i.e. the lifecycle holds no core guard across the
    /// await. The concurrent write is bounded by a timeout so a regression
    /// (a guard leaked across the await) fails fast instead of hanging.
    #[tokio::test]
    async fn no_guard_held_across_verify_await() {
        let core = new_core();
        let addr = Address::from([0x40u8; 20]);
        {
            let mut c = core.write();
            reg_v3(&mut c, addr, PoolTickCoverage::Tracked);
        }
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let core_task = Arc::clone(&core);
        let task = tokio::spawn(async move {
            run_cl_v3_lifecycle::<_, _, _, _, ()>(
                &core_task,
                addr,
                Some(42),
                move |_, _| async move { Ok(()) },
                move |_, _| {
                    let started = started_tx;
                    let release = release_rx;
                    async move {
                        let _ = started.send(());
                        let _ = release.await; // RPC stand-in: block until released
                        Ok(())
                    }
                },
            )
            .await
        });
        // Wait until the lifecycle is parked inside the post-drain verify await.
        let _ = started_rx.await;
        // Concurrent write must complete promptly (no guard held across await).
        let write = tokio::time::timeout(std::time::Duration::from_millis(500), async {
            let c = core.write();
            let n = c.v4_pool_count();
            std::hint::black_box(n);
        })
        .await;
        assert!(
            write.is_ok(),
            "no BotState guard may be held across the verify await"
        );
        let _ = release_tx.send(());
        assert!(task.await.is_ok(), "lifecycle must complete Ok");
        assert_eq!(
            lifecycle_v3(&core.read(), core.read().pool_id_by_address(&addr).unwrap()),
            RegistrationLifecycle::Live
        );
    }
}
