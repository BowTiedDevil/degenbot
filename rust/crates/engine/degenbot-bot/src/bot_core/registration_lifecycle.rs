//! Registration verify-lifecycle core ownership (ADR-022).
//!
//! Owns the per-pool `quarantine → drain+pin → two-step verify → Live`
//! sequence that was previously orchestrated in Python
//! (`src/degenbot/arbitrage/engine_registry.py::register_v3/v4_pool`).
//!
//! The sequence is ONE machine ([`run_verify_lifecycle`]) over one pool key
//! ([`PoolLookup`]): a V3 pool is addressed by contract address, a V4 pool
//! by `(pool_manager, pool_id)`, and that is the only difference between the two
//! families here. [`run_registration_lifecycle`] is the provider-backed key
//! adapter over it, so a rule learned about this machine — the lock scope, the
//! tripwire, the seed-consumed-once discipline — holds for both designs by
//! construction rather than by a second copy agreeing (ADR-080 D2).
//!
//! Decision D4 (lifecycle), per ADR-022:
//! - a **sparse** pool is `Live` immediately, receives **no verification
//!   deferral and no RPC**;
//! - a **tracked** pool is `Live` only after its liquidity-map verification
//!   passes, with the verification `MismatchError` as the **tripwire** that
//!   must pass before `Live` (no pool becomes solvable on unverified state).
//!
//! The module is deliberately **runtime-agnostic**: the two RPC-bound verify
//! steps (`verify_seed`, `verify_post_drain`) are supplied as async closures,
//! so the core state-machine is unit-testable without a live provider. The
//! concrete provider-backed door ([`run_registration_lifecycle`]) wires them to
//! `liquidity_verifier` with the bot's **single** `AlloyProvider` passed-in
//! (ADR-022 D3 — one provider per bot; never stored on `BotState`, which is
//! I/O-free per ADR-001). It takes the same [`PoolLookup`] the machine runs
//! over, so both designs enter through one body.
//!
//! **Lock discipline (preserved exactly):** no guard is held across the
//! `verify_seed` / `verify_post_drain` `.await`s (take-pin-then-drop); the
//! drain + pin run under ONE `core.write()` hold (the step-2 rolling-start race
//! fix); step-1 verifies the pinned snapshot seed @ snapshot block;
//! step-2 verifies the pin's own captured block (never a constant backfill
//! block — the 2026-06-29 crash class).

use hashbrown::HashMap;
use std::future::Future;

use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::PoolLookup;

use degenbot_rpc::provider::AlloyProvider;

use super::liquidity_verifier::verify_snapshot_map;
use super::registration_ledger::{LifecyclePrecondition, RefusalKind, RegistrationPhase};
use degenbot_pools::PoolTickCoverage;
use degenbot_pools::TickInfo;
use degenbot_substrate::BotState;

/// A **tracked** pool reached a verify step with no provider configured (the
/// D-C fail-fast: no verify-disabled mode for tracked). Sparse / unregistered
/// / no-pin no-op paths never need a provider.
fn missing_verify_provider() -> RefusalKind {
    RefusalKind::lifecycle_precondition(LifecyclePrecondition::MissingVerifyProvider)
}

/// A registered pool's immutable tick spacing was unavailable at a verify
/// step. This is state corruption, never a reason to assume unit spacing.
fn missing_tick_spacing() -> RefusalKind {
    RefusalKind::lifecycle_precondition(LifecyclePrecondition::MissingTickSpacing)
}

/// Drive one concentrated-liquidity pool through the registration
/// verify-lifecycle, branching on `coverage`: **Sparse → immediate no-op**
/// (already `Live`, no verification deferral, no RPC); **Tracked → quarantine →
/// (step-1 seed verify) → drain+pin → (step-2 post-drain verify) → `set_live`**.
///
/// The key is a value ([`PoolLookup`]), so the V3 and V4 designs share
/// this one machine: a lifecycle rule learned here cannot hold for one design
/// and lapse for the other.
///
/// The two RPC-bound verify steps are supplied as async closures, so the
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
/// (`apply_backfill_buffer` → `apply_pump_buffer`) + pin run under ONE
/// `core.write()` hold — the step-2 rolling-start race fix.
///
/// # Errors
///
/// Returns the closure error type `E` if either verify closure returns `Err`
/// — a verification failure aborts BEFORE `set_live`, leaving the tracked pool
/// `Quarantined` (the tripwire; never auto-repair). No error on the Sparse /
/// no-op paths.
pub async fn run_verify_lifecycle<F1, Fut1, F2, Fut2, E>(
    core: &StateLock<BotState>,
    key: PoolLookup,
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
    // Coverage branch up-front. A Sparse pool stays `Live`, receives NO
    // verification deferral and NO verify RPC, but its buffered backfill/pump
    // events (buffered while the pool was still unregistered) must still be
    // drained onto the pool — draining is not verification. An unregistered /
    // non-matching pool → no-op Ok.
    let coverage = {
        let state = core.read_at(degenbot_substrate::state_lock::LockSite::Registration);
        key.coverage(&state)
    };
    match coverage {
        None => return Ok(()),
        Some(PoolTickCoverage::Sparse) => {
            let mut state = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            key.drain(&mut state);
            return Ok(());
        }
        Some(PoolTickCoverage::Tracked) => {}
    }

    // Quarantine BEFORE the first RPC await: defers the pool's live
    // Swap/Mint/Burn to the pump buffer so the pin's `update_block` cannot
    // outrun `last_complete_block` during the drain+pin+verify window.
    {
        let mut state = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
        key.begin_registration_quarantine(&mut state);
    }

    // Step-1: verify the pinned snapshot SEED @ snapshot block. Only when a
    // snapshot block is supplied (the seam's gated-skip posture); the seed is
    // consumed exactly once so memory is bounded. The comparison is
    // seed-vs-on-chain@snapshot, NOT engine-current (which would false-mismatch
    // every active pool under a rolling start).
    //
    // Anchored at the pool's OWN liquidity clock — the block its seed data
    // reflects (a DB-seeded `Tracked` pool's `liquidity_update_block`) — rather
    // than the aggregate snapshot-seed block `S`. The builder stamps a
    // DB-seeded pool's `tick_data_block` at that block, so a
    // stale-but-internally-consistent DB pool verifies against on-chain @ ITS
    // block (passes) instead of @ `S` (a fabricated mismatch on any tick that
    // moved in between). Falls back to `S` only when the pool clock is unset.
    if let Some(snapshot_block) = snapshot_block {
        let seed = {
            let mut state = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            key.take_seed(&mut state)
        };
        if let Some(seed) = seed {
            // Single acquisition by construction: the fused accessor chains
            // key → liquidity clock under ONE `read_at(Registration)`. Never
            // recompose the two single-step reads here — nested reads are a
            // self-deadlock when a writer queues between them (soak-2026-08-22).
            let own = key.own_liquidity_clock(core);
            let seed_block = if own > 0 { own } else { snapshot_block };
            verify_seed(seed, seed_block).await?;
        }
    }

    // Drain + pin under a SINGLE `core.write()` hold: backfill buffer, then
    // pump buffer, then capture the frozen post-drain `(tick_data, block)`
    // pair atomically with the drain (the step-2 rolling-start race fix).
    {
        let mut state = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
        key.drain(&mut state);
        key.pin_post_drain(&mut state);
    }

    // Step-2: verify the pinned POST-DRAIN pair @ the pin's OWN captured block
    // (the `tick_data_block` — liquidity clock, two-stamp rule). Comparing
    // against a caller-supplied constant would fabricate a mismatch on active
    // pools (the 2026-06-29 crash). The pin is consumed exactly once.
    let pin = {
        let mut state = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
        key.take_post_drain(&mut state)
    };
    if let Some((tick_data, pinned_block)) = pin {
        verify_post_drain(tick_data, pinned_block).await?;
    }

    // Tripwire passed (ADR-022 D2) — the final gate before `Live`. Reaching
    // here means a Tracked pool's verification succeeded; `Live` is the last
    // transition.
    {
        let mut state = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
        key.mark_registration_live(&mut state);
    }
    Ok(())
}

/// Drive one pool through the registration verify-lifecycle against the bot's
/// **single** `AlloyProvider` (ADR-022 D3 — one provider per bot/chain,
/// passed-in).
///
/// The key is a value ([`PoolLookup`]): a V3 pool by contract address, a V4
/// pool by `(pool_manager, pool_id)`. One body serves both designs, so the
/// verify steps, the tick-spacing read and the on-chain target cannot drift
/// apart per family (ADR-081/ADR-082).
///
/// A `None` provider only matters for a **Tracked** pool (which reaches a
/// verify step and fails fast with the missing-provider
/// [`LifecyclePrecondition`] — D-C); Sparse / unregistered no-op paths never
/// need one.
///
/// # Errors
///
/// Returns a [`RefusalKind`] at the lifecycle phase: a verify failure
/// (mismatch = fatal tripwire), or
/// [`LifecyclePrecondition::MissingVerifyProvider`] when a tracked pool needs
/// to verify with no provider configured.
pub async fn run_registration_lifecycle(
    core: &StateLock<BotState>,
    provider: Option<&AlloyProvider>,
    key: PoolLookup,
    snapshot_block: Option<u64>,
) -> Result<(), RefusalKind> {
    let provider = provider.cloned();
    // The spacing is read inside each closure, not up front: a Sparse pool
    // never verifies and so never needs it.
    run_verify_lifecycle(
        core,
        key,
        snapshot_block,
        |seed, block| {
            let provider = provider.clone();
            async move {
                let p = provider.ok_or_else(missing_verify_provider)?;
                let tick_spacing = key.tick_spacing(core).ok_or_else(missing_tick_spacing)?;
                verify_snapshot_map(&p, key, &seed, tick_spacing, block, "seed")
                    .await
                    .map_err(|kind| kind.at_phase(RegistrationPhase::Lifecycle))
            }
        },
        |tick_data, block| {
            let provider = provider.clone();
            async move {
                let p = provider.ok_or_else(missing_verify_provider)?;
                let tick_spacing = key.tick_spacing(core).ok_or_else(missing_tick_spacing)?;
                verify_snapshot_map(&p, key, &tick_data, tick_spacing, block, "post-drain")
                    .await
                    .map_err(|kind| kind.at_phase(RegistrationPhase::Lifecycle))
            }
        },
    )
    .await
}

#[expect(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    use hashbrown::HashMap;
    use std::sync::Arc;

    use alloy::primitives::{Address, U128, U256};
    use degenbot_pools::PoolLiquidityUpdate;
    use degenbot_substrate::state_lock::{LockSite, StateLock};

    use degenbot_decoders::v4_swap_decoder::V4PoolId;
    use degenbot_pools::V4PoolKey;

    use crate::bot_core::{LifecyclePrecondition, RefusalKind};
    use degenbot_pools::RegisterV4PoolParams;
    use degenbot_pools::TickInfo;
    use degenbot_pools::{PoolTickCoverage, RegisterV3PoolParams, RegistrationLifecycle};
    use degenbot_substrate::{BotState, PoolLookup};

    use super::run_verify_lifecycle;

    fn new_core() -> Arc<StateLock<BotState>> {
        Arc::new(StateLock::new(BotState::new()))
    }

    fn reg_v3(core: &mut BotState, address: Address, coverage: PoolTickCoverage) -> u64 {
        let mut tick_data = HashMap::new();
        if coverage == PoolTickCoverage::Tracked {
            tick_data.insert(
                60,
                TickInfo {
                    liquidity_gross: U128::from(100),
                    liquidity_net: 100i128,
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
                    liquidity_net: 100i128,
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
            state_view: Address::ZERO,
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
            known_bitmap_words: Vec::new(),
        })
        .expect("test setup: V4 registration")
    }

    fn lifecycle_v3(core: &BotState, pool_id: u64) -> RegistrationLifecycle {
        core.pool_slice::<degenbot_pools::V3PoolState>(pool_id)
            .expect("registered V3 pool")
            .registration_lifecycle
    }

    fn lifecycle_v4(core: &BotState, pool_id: u64) -> RegistrationLifecycle {
        core.pool_slice::<degenbot_pools::V4PoolState>(pool_id)
            .expect("registered V4 pool")
            .registration_lifecycle
    }

    /// A Sparse V3 pool goes straight to the end: the lifecycle is an
    /// immediate no-op — `Live`, no verify closure invoked, no RPC.
    #[tokio::test]
    async fn sparse_v3_lifecycle_is_immediate_no_verify() {
        let core = new_core();
        let addr = Address::from([0x10u8; 20]);
        let pid = {
            let mut c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            reg_v3(&mut c, addr, PoolTickCoverage::Sparse)
        };
        // Both closures `unreachable!` — the invariant is that Sparse never
        // invokes the verify RPC path.
        let result = run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            PoolLookup::from_address(addr),
            Some(123),
            |_, _| async move { unreachable!("no seed verify for Sparse") },
            |_, _| async move { unreachable!("no post-drain verify for Sparse") },
        )
        .await;
        assert!(result.is_ok(), "sparse lifecycle must be Ok");
        let c = core.read_at(degenbot_substrate::state_lock::LockSite::Registration);
        assert_eq!(lifecycle_v3(&c, pid), RegistrationLifecycle::Live);
    }

    /// A Sparse V4 pool is also an immediate no-op (no RPC).
    #[tokio::test]
    async fn sparse_v4_lifecycle_is_immediate_no_verify() {
        let core = new_core();
        let pm = Address::from([0x44u8; 20]);
        let pid = [0xabu8; 32];
        {
            let mut c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            reg_v4(&mut c, pm, pid, PoolTickCoverage::Sparse);
        }
        let result = run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            PoolLookup::V4 {
                pool_manager: pm,
                pool_id: pid,
            },
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
            let mut c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            reg_v3(&mut c, addr, PoolTickCoverage::Tracked)
        };
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_seed = Arc::clone(&calls);
        let calls_post = Arc::clone(&calls);
        let result = run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            PoolLookup::from_address(addr),
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
        let c = core.read_at(degenbot_substrate::state_lock::LockSite::Registration);
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

    /// the reactivated wrong-block seed-verify regression:
    /// a DB-seeded (`Tracked`) pool whose liquidity clock (`tick_data_block`,
    /// stamps the builder at the DB `liquidity_update_block`) trails the
    /// aggregate snapshot-seed block `S` must have step-1 anchor at the pool's
    /// OWN clock, NOT `S`. Before the fix step-1 always used `S`, fabricating a
    /// seed mismatch on any stale-DB pool that moved on-chain in `(own, S]`.
    #[tokio::test]
    async fn tracked_v3_seed_verify_anchors_at_pool_own_liquidity_clock_not_global_s() {
        let core = new_core();
        let addr = Address::from([0x70u8; 20]);
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: 100i128,
                block: 100,
            },
        );
        let _pid = {
            let mut c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            // DB-seeded Tracked pool whose liquidity clock is 100 (the DB
            // `liquidity_update_block`), far behind the global S = 200.
            c.register_v3_pool(&RegisterV3PoolParams {
                address: addr,
                token0: Address::ZERO,
                token1: Address::from([1u8; 20]),
                fee: 3000,
                tick_spacing: 60,
                factory: Address::ZERO,
                sqrt_price_x96: U256::from(1u128) << 96,
                liquidity: 1_000_000,
                tick: 0,
                tick_data,
                update_block: 200,
                tick_data_block: Some(100),
                coverage: PoolTickCoverage::Tracked,
                fetcher: None,
                ..Default::default()
            })
            .unwrap()
        };
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seed_calls = Arc::clone(&calls);
        let result = run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            PoolLookup::from_address(addr),
            Some(200), // global snapshot-seed block S
            move |seed, block| {
                let calls = seed_calls;
                async move {
                    calls.lock().unwrap().push(("seed", block, seed.len()));
                    Ok(())
                }
            },
            move |_tick_data, _block| async move { Ok(()) },
        )
        .await;
        assert!(result.is_ok());
        let calls = calls.lock().unwrap();
        assert_eq!(calls[0].0, "seed");
        assert_eq!(
            calls[0].1, 100,
            "step-1 seed verify must anchor at the pool's OWN liquidity clock (100), \
             NOT the global snapshot-seed block S (200)"
        );
    }

    /// A Tracked V3 pool whose seed verify mismatches must NEVER reach `Live`
    /// — the mismatch is the tripwire (ADR-022 D2, no auto-repair).
    #[tokio::test]
    async fn tracked_v3_seed_mismatch_tripwire_blocks_live() {
        let core = new_core();
        let addr = Address::from([0x30u8; 20]);
        let pid = {
            let mut c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            reg_v3(&mut c, addr, PoolTickCoverage::Tracked)
        };
        let result = run_verify_lifecycle::<_, _, _, _, String>(
            &core,
            PoolLookup::from_address(addr),
            Some(42),
            move |_, _| async move { Err("seed mismatch".to_string()) },
            move |_, _| async move { unreachable!("post-drain must not run after seed fail") },
        )
        .await;
        assert!(result.is_err());
        let c = core.read_at(degenbot_substrate::state_lock::LockSite::Registration);
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
            let mut c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            reg_v3(&mut c, addr, PoolTickCoverage::Tracked)
        };
        let result = run_verify_lifecycle::<_, _, _, _, String>(
            &core,
            PoolLookup::from_address(addr),
            Some(42),
            move |_, _| async move { Ok(()) },
            move |_, _| async move { Err("post-drain mismatch".to_string()) },
        )
        .await;
        assert!(result.is_err());
        let c = core.read_at(degenbot_substrate::state_lock::LockSite::Registration);
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
            let mut c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            // Seed tick -201000 with gross/net 100 (a snapshot seed).
            reg_v3(&mut c, addr, PoolTickCoverage::Tracked)
        };
        // A Burn during backfill, BEFORE the pool is live-registered: the pool
        // is Quarantined (Tracked) so this BUFFERS rather than applies.
        {
            let mut c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            c.buffer_backfill_liquidity_update(
                PoolLookup::from_address(addr),
                PoolLiquidityUpdate::new(-201_000, -200_990, -100, 5),
            );
            // Verify it buffered (Quarantined → not applied yet).
            assert_eq!(
                c.pool_slice::<degenbot_pools::V3PoolState>(pid)
                    .unwrap()
                    .tick_data
                    .len(),
                1,
                "must be buffered"
            );
        }
        let result = run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            PoolLookup::from_address(addr),
            None, // no snapshot block → step-1 skipped; step-2 (post-drain) still runs
            |_, _| async move { Ok(()) },
            |_, _| async move { Ok(()) },
        )
        .await;
        assert!(result.is_ok());
        let c = core.read_at(degenbot_substrate::state_lock::LockSite::Registration);
        let state = c.pool_slice::<degenbot_pools::V3PoolState>(pid).unwrap();
        // The burn zeroed gross → tick removed (not stranded in the buffer).
        assert!(
            !state.tick_data.contains_key(&-201_000),
            "buffered burn must be drained onto the seed, removing the zeroed tick"
        );
    }

    /// A missing provider for a TRACKED pool is a no-config fail-fast for
    /// either key shape: the tripwire fires and the pool never reaches `Live`.
    #[tokio::test]
    async fn tracked_missing_provider_blocks_live_for_both_pool_keys() {
        let core = new_core();
        let v3_addr = Address::from([0x45u8; 20]);
        let pm = Address::from([0x44u8; 20]);
        let pid = [0xbu8; 32];
        let v3_id = {
            let mut c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            reg_v3(&mut c, v3_addr, PoolTickCoverage::Tracked)
        };
        {
            let mut c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            reg_v4(&mut c, pm, pid, PoolTickCoverage::Tracked);
        }

        for key in [
            PoolLookup::from_address(v3_addr),
            PoolLookup::V4 {
                pool_manager: pm,
                pool_id: pid,
            },
        ] {
            let result =
                crate::bot_core::run_registration_lifecycle(&core, None, key, Some(42)).await;
            assert!(
                matches!(
                    result,
                    Err(RefusalKind::LifecyclePrecondition {
                        reason: LifecyclePrecondition::MissingVerifyProvider,
                        ..
                    })
                ),
                "pool {key} must fail fast on the missing verify provider"
            );
        }

        let c = core.read_at(degenbot_substrate::state_lock::LockSite::Registration);
        assert_eq!(
            lifecycle_v3(&c, v3_id),
            RegistrationLifecycle::Quarantined,
            "tracked V3 without a provider must stay Quarantined"
        );
        assert_eq!(
            c.pool_id_by_key(pm, &pid).map(|id| lifecycle_v4(&c, id)),
            Some(RegistrationLifecycle::Quarantined),
            "tracked V4 without a provider must stay Quarantined"
        );
    }

    /// the terminal `release_all_v3_v4_quarantined` is an ORPHAN sweep
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
            let mut c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            reg_v3(&mut c, tracked_addr, PoolTickCoverage::Tracked)
        };
        // A sparse V3 pool (already Live, never quarantined).
        let sparse_addr = Address::from([0x61u8; 20]);
        let sparse_pid = {
            let mut c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            reg_v3(&mut c, sparse_addr, PoolTickCoverage::Sparse)
        };
        // A genuinely orphaned tracked V4 (never released by any per-path
        // lifecycle) — the only pool the terminal sweep should flush.
        let orphan_vm = Address::from([0x62u8; 20]);
        let orphan_pid = [0xcu8; 32];
        {
            let mut c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            reg_v4(&mut c, orphan_vm, orphan_pid, PoolTickCoverage::Tracked);
        }

        // Run the per-path lifecycle on the tracked V3 -> Live.
        run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            PoolLookup::from_address(tracked_addr),
            Some(42),
            |_, _| async move { Ok(()) },
            |_, _| async move { Ok(()) },
        )
        .await
        .expect("tracked per-path lifecycle must release to Live");

        // Confirm the productive pools are Live BEFORE any batch runs.
        {
            let c = core.read_at(degenbot_substrate::state_lock::LockSite::Registration);
            assert_eq!(lifecycle_v3(&c, tracked_pid), RegistrationLifecycle::Live);
            assert_eq!(lifecycle_v3(&c, sparse_pid), RegistrationLifecycle::Live);
        }

        // The terminal orphan sweep runs (as it would after a single-pass
        // discovery completes). It must NOT re-touch the per-path Live pools
        // and must flush only the orphaned quarantined V4.
        {
            let mut c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            c.release_all_quarantined();
        }

        let c = core.read_at(degenbot_substrate::state_lock::LockSite::Registration);
        // The per-path gate held: both productive pools are still Live (the
        // batch did not duplicate release nor silently quarantine them).
        assert_eq!(lifecycle_v3(&c, tracked_pid), RegistrationLifecycle::Live);
        assert_eq!(lifecycle_v3(&c, sparse_pid), RegistrationLifecycle::Live);
        // The orphan was swept by the batch (the only legitimate use).
        assert_eq!(
            lifecycle_v4(&c, c.pool_id_by_key(orphan_vm, &orphan_pid).unwrap()),
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
            let mut c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            reg_v3(&mut c, addr, PoolTickCoverage::Tracked);
        }
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let core_task = Arc::clone(&core);
        let task = tokio::spawn(async move {
            run_verify_lifecycle::<_, _, _, _, ()>(
                &core_task,
                PoolLookup::from_address(addr),
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
            let c = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            let n = c.concentrated_liquidity().registered_pool_count();
            std::hint::black_box(n);
        })
        .await;
        assert!(
            write.is_ok(),
            "no BotState guard may be held across the verify await"
        );
        let _ = release_tx.send(());
        assert!(task.await.is_ok(), "lifecycle must complete Ok");
        // ONE read guard for the whole check: composing two `read_at` calls
        // is the nested-read cycle the fused accessors remove (see
        // `state_lock.rs`, "NEVER NEST ACQUISITIONS").
        let c = core.read_at(degenbot_substrate::state_lock::LockSite::Registration);
        let pid = c.pool_id_by_address(&addr).unwrap();
        assert_eq!(lifecycle_v3(&c, pid), RegistrationLifecycle::Live);
    }

    /// The soak-2026-08-22 cycle, modelled on the same `StateLock<BotState>`
    /// type: read#1 held while a second `read_at(Registration)` is
    /// requested, with a writer queued in between. `parking_lot` parks a new
    /// read behind a waiting writer, and the writer waits on read#1 — a
    /// cycle. The fused accessors take one guard and cannot form it; this
    /// test pins the hazard that motivated the fusion, on the real lock type.
    ///
    /// Design note: the old call-site shape held read#1 as an expression
    /// temporary in the SAME thread as read#2, which cannot be interrupted
    /// for cleanup — reproducing it here would hang the test harness.
    /// `parking_lot`'s `read()` does not distinguish reader threads (a read
    /// blocks whenever a writer is queued), so the cycle is identical with
    /// read#1 held here and read#2 on a sibling thread, and dropping read#1
    /// breaks it cleanly (no leaked threads).
    #[test]
    fn nested_read_with_queued_writer_is_a_lock_cycle() {
        let core = new_core();
        let read1 = core.read_at(LockSite::Registration); // read#1

        let (writer_started_tx, writer_started_rx) = std::sync::mpsc::channel::<()>();
        let writer_core = Arc::clone(&core);
        let writer = std::thread::spawn(move || {
            writer_started_tx.send(()).expect("signal writer start");
            writer_core
                .write_at(LockSite::Registration)
                .concentrated_liquidity_mut()
                .advance_pump_complete_cutoff(1);
        });
        writer_started_rx.recv().expect("writer thread started");
        // parking_lot exposes no "writer queued" signal, so give the writer
        // time to actually park in `write_at`; the read#2 observation below
        // is the assertion that it did.
        std::thread::sleep(std::time::Duration::from_millis(500));

        let reader_core = Arc::clone(&core);
        // read#2 returns (held, acquired_at): both stamps are monotonic, so
        // comparing them across threads is exact. The assertion below pins the
        // ORDER rather than a knife-edge duration (the old `blocked >= 500ms`
        // measured ~498.6ms against a 500ms hold and flaked under load).
        let read2 = std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            let _g = reader_core.read_at(LockSite::Registration); // read#2
            (t0.elapsed(), std::time::Instant::now())
        });
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(
            !read2.is_finished(),
            "read#2 must park behind the queued writer while read#1 is held —              the nested-read cycle the fused accessors remove"
        );

        let dropped_at = std::time::Instant::now();
        drop(read1); // breaks the cycle
        let (blocked, acquired_at) = read2.join().expect("read#2 completes once read#1 drops");
        writer.join().expect("writer completes");
        assert!(
            acquired_at >= dropped_at,
            "read#2 only advanced after read#1 dropped ({blocked:?}) — that is the cycle"
        );
    }
}
