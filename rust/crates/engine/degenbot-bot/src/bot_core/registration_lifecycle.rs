//! Registration verify-lifecycle core ownership (ADR-022).
//!
//! Owns the per-pool `quarantine → drain+pin → two-step verify → Live`
//! sequence (the Python reference is
//! `src/degenbot/arbitrage/engine_registry.py::register_v3/v4_pool`).
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
//!   must pass before `Live` (no pool becomes solvable on unverified state);
//! - a **tracked** pool whose post-drain pin is INCOMPLETE (the drain held a
//!   pump event back at/below the map's own liquidity clock) is neither
//!   verified nor promoted: the pin parks it in `VerificationDeferred` and the
//!   machine answers `Ok`, deferring to a later call whose drain completed that
//!   block;
//! - a key that names **no registered `Tracked` pool** at a step that requires
//!   one is neither verified nor promoted either: the machine answers
//!   [`LifecycleOutcome::NotOwed`]. `Ok` therefore means "this lifecycle call
//!   finished", never unconditionally "verified" — a missing required step is
//!   never a completed one.
//!
//! # One owner for the verification inputs
//!
//! The pinned snapshot seed and the pinned post-drain `(tick_data, block)` pair
//! are the inputs of this machine, and the call that reaches for them consumes
//! them: the seed is taken under its own short write whether or not a snapshot
//! block makes it verifiable, and the post-drain pair is minted and taken under
//! the SAME write hold that drained the map, so a `Pinned` verdict and the map
//! it froze cannot be observed apart and no arm can leave a pair behind. A
//! tracked pool therefore reaches `Live` with both slots empty — nothing a later
//! call could mistake for an owed (or a freshly minted) verification.
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
//! block).

use hashbrown::HashMap;
use std::future::Future;

use degenbot_substrate::state_lock::StateLock;
use degenbot_substrate::PoolLookup;

use degenbot_rpc::provider::AlloyProvider;

use super::liquidity_verifier::verify_snapshot_map;
use super::registration_ledger::{LifecyclePrecondition, RefusalKind, RegistrationPhase};
use degenbot_pools::PoolTickCoverage;
use degenbot_pools::TickInfo;
use degenbot_substrate::concentrated_liquidity_orchestration::PostDrainPin;
use degenbot_substrate::BotState;

/// What one registration verify-lifecycle call established — the whole outcome
/// space of the one ladder, so a caller cannot read a missing verification as a
/// finished one.
///
/// `Completed` reached the end of the ladder this call owed (a `Tracked` pool
/// verified and `Live`, or a `Sparse` pool whose coverage owes no verification);
/// `Deferred` found the post-drain pin incomplete and left the pool parked with
/// its verification still owed; `NotOwed` found no registered `Tracked` pool to
/// verify at the moment a step required one. Only `Completed` may be recorded as
/// a durable "verified" fact (see [`Self::records_verified_fact`]): a `Deferred`
/// or `NotOwed` pool must stay retriable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleOutcome {
    /// The lifecycle ran to its end: verified, or the coverage owed nothing.
    Completed,
    /// The drain was incomplete, so nothing was verified and nothing was
    /// promoted. The pool remains parked and a later call retries it.
    Deferred,
    /// Nothing was verified because nothing was there to verify: the key named
    /// no registered `Tracked` pool when a step required one (unregistered, or
    /// the pool left the registry mid-call), so no map could be pinned. Nothing
    /// was promoted, and the next call re-runs the ladder.
    NotOwed,
}

impl LifecycleOutcome {
    /// Whether this outcome may record the durable verified-pool fact — the ONE
    /// spelling of that rule. A `Deferred` or `NotOwed` call still owes its
    /// verification, so memoizing one would skip the verify the pool needs.
    #[must_use]
    pub const fn records_verified_fact(self) -> bool {
        matches!(self, Self::Completed)
    }
}

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
/// # Pin verdicts (the whole space)
///
/// Step-2 needs a `(tick_data, block)` pair the drain can back: the drained map
/// must hold every buffered pump event at/below its own liquidity clock. The
/// pin's answer, together with the pair it minted, decides the rest
/// exhaustively:
///
/// - [`PostDrainPin::Pinned`] carries the map, so step-2 runs at the pin's own
///   captured block;
/// - [`PostDrainPin::Incomplete`] stores nothing (dropping any stale pair),
///   parks the pool as `VerificationDeferred`, and returns
///   [`LifecycleOutcome::Deferred`] WITHOUT invoking `verify_post_drain` and
///   WITHOUT `set_live`. A later call whose drain has completed that block
///   re-quarantines, re-pins, verifies and promotes it;
/// - [`PostDrainPin::NotPinnable`] means the key stopped naming a registered
///   `Tracked` pool, so there is no map and no verification to report: the
///   machine returns [`LifecycleOutcome::NotOwed`], never a success.
///
/// # Lock-scope contract
///
/// No guard is held across the `verify_seed`/`verify_post_drain` `.await`s
/// (each take runs under a short `core.write()` then drops it), and the drain
/// (`apply_backfill_buffer` → `apply_pump_buffer`) + pin + take run under ONE
/// `core.write()` hold — the step-2 rolling-start race fix.
///
/// # Errors
///
/// Returns the closure error type `E` if either verify closure returns `Err`
/// — a verification failure aborts BEFORE `set_live`, leaving the tracked pool
/// `Quarantined` (the tripwire; never auto-repair). No error on the Sparse /
/// no-op paths, nor on the deferred (incomplete-drain) path.
///
/// # Returns
///
/// [`LifecycleOutcome::Completed`] when the call ran to the end of the ladder it
/// owed (a verified `Tracked` pool, or a `Sparse` pool that owes no
/// verification), [`LifecycleOutcome::Deferred`] when the post-drain pin was
/// incomplete (the pool stays parked and a later call retries it), or
/// [`LifecycleOutcome::NotOwed`] when the key named no registered `Tracked` pool
/// (nothing verified, nothing promoted). A caller must record a durable
/// "verified" fact only for `Completed` — see
/// [`LifecycleOutcome::records_verified_fact`].
///
pub async fn run_verify_lifecycle<F1, Fut1, F2, Fut2, E>(
    core: &StateLock<BotState>,
    key: PoolLookup,
    snapshot_block: Option<u64>,
    verify_seed: F1,
    verify_post_drain: F2,
) -> Result<LifecycleOutcome, E>
where
    F1: FnOnce(HashMap<i32, TickInfo>, u64) -> Fut1 + Send,
    Fut1: Future<Output = Result<(), E>> + Send,
    F2: FnOnce(HashMap<i32, TickInfo>, u64) -> Fut2 + Send,
    Fut2: Future<Output = Result<(), E>> + Send,
{
    // Coverage branch up-front. A Sparse pool stays `Live`, receives NO
    // verification deferral and NO verify RPC, but its buffered backfill/pump
    // events (buffered while the pool was still unregistered) must still be
    // drained onto the pool — draining is not verification, and the ladder a
    // Sparse pool owes is empty, so it ends `Completed`. A key that names no
    // registered concentrated-liquidity pool owes nothing AND proved nothing:
    // `NotOwed`, never `Completed`, so a caller cannot memoize an unverified
    // pool as verified.
    let coverage = {
        let state = core.read_at(degenbot_substrate::state_lock::LockSite::Registration);
        key.coverage(&state)
    };
    match coverage {
        None => return Ok(LifecycleOutcome::NotOwed),
        Some(PoolTickCoverage::Sparse) => {
            let mut state = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
            key.drain(&mut state);
            return Ok(LifecycleOutcome::Completed);
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

    // Step-1: verify the pinned snapshot SEED @ snapshot block. The step runs
    // only when a snapshot block is supplied (the seam's gated-skip posture:
    // with no block there is nothing to compare the seed against), but the seed
    // is the input this ONE call owns either way, so it is taken under the same
    // short write: a skipped step-1 drops it instead of leaving a pinned map for
    // a later call to verify against a clock that has since moved. It is
    // consumed exactly once, so memory is bounded. The comparison is
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
    let seed = {
        let mut state = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
        key.take_seed(&mut state)
    };
    if let (Some(snapshot_block), Some(seed)) = (snapshot_block, seed) {
        // Single acquisition by construction: the leased accessor chains
        // key → liquidity clock inside ONE `read_with(Registration)`.
        // Never recompose the two single-step reads here — nested reads
        // are a self-deadlock when a writer queues between them.
        let own = key.own_liquidity_clock(core);
        let seed_block = if own > 0 { own } else { snapshot_block };
        verify_seed(seed, seed_block).await?;
    }

    // Drain + pin + take under a SINGLE `core.write()` hold: backfill buffer,
    // pump buffer, freeze the post-drain `(tick_data, block)` pair atomically
    // with the drain (the step-2 rolling-start race fix), and take the pair the
    // pin just minted. Bonding the take to the mint is what makes the pair the
    // property of THIS call: a `Pinned` verdict cannot be observed without the
    // map it froze, and every arm below leaves the pool's post-drain slot empty.
    let (pin, minted) = {
        let mut state = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
        key.drain(&mut state);
        let pin = key.pin_post_drain(&mut state);
        let minted = key.take_post_drain(&mut state);
        (pin, minted)
    };

    // Every pin answer is handled here, and only `Pinned` — with the very map
    // it minted — may continue to the required step-2.
    let (tick_data, pinned_block) = match (pin, minted) {
        (PostDrainPin::Pinned { clock, .. }, Some((tick_data, pinned_block))) => {
            debug_assert_eq!(
                clock, pinned_block,
                "the pin stamps the map with its own liquidity clock"
            );
            (tick_data, pinned_block)
        }
        // An INCOMPLETE drain is neither verifiable nor promotable: the pin
        // stored no pair (dropping any stale one), so return WITHOUT step-2 and
        // WITHOUT `set_live`. The pin has already parked the pool in
        // `RegistrationLifecycle::VerificationDeferred` — verification is still
        // owed, so its live events keep buffering exactly as under
        // `Quarantined` and the orphan sweep skips it (`VerificationDeferred` is
        // not an orphan). A later call whose drain has completed that block
        // re-quarantines, re-pins, verifies and promotes it.
        (PostDrainPin::Incomplete { .. }, _) => return Ok(LifecycleOutcome::Deferred),
        // Two shapes, ONE disposition: nothing was verified, so nothing may be
        // promoted or recorded.
        // - `NotPinnable`: the key stopped naming a registered `Tracked` pool
        //   between the coverage read and the drain (unregistered, or its entry
        //   no longer carries a complete map), so no step ran at all;
        // - `Pinned` with no map: unreachable by construction, because the pair
        //   is taken under the hold that minted it — and the safe answer if it
        //   ever is reached, since a missing map is NOT a verification.
        (PostDrainPin::NotPinnable, _) | (PostDrainPin::Pinned { .. }, None) => {
            return Ok(LifecycleOutcome::NotOwed);
        }
    };

    // Step-2: verify the pinned POST-DRAIN pair @ the pin's OWN captured block
    // (the `tick_data_block` — liquidity clock, two-stamp rule). Comparing
    // against a caller-supplied constant would fabricate a mismatch on active
    // pools.
    verify_post_drain(tick_data, pinned_block).await?;

    // Tripwire passed (ADR-022 D2) — the final gate before `Live`. Reaching
    // here means a Tracked pool's required verification succeeded; `Live` is
    // the last transition.
    {
        let mut state = core.write_at(degenbot_substrate::state_lock::LockSite::Registration);
        key.mark_registration_live(&mut state);
    }
    Ok(LifecycleOutcome::Completed)
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
/// A tracked pool whose drain is incomplete answers
/// `Ok(`[`LifecycleOutcome::Deferred`]`)` with the pool still parked (a deferred
/// retry — see [`run_verify_lifecycle`]), and a key that names no registered
/// tracked pool answers `Ok(`[`LifecycleOutcome::NotOwed`]`)`. Neither is an
/// established "verified" fact: only [`LifecycleOutcome::records_verified_fact`]
/// may be recorded.
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
) -> Result<LifecycleOutcome, RefusalKind> {
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
    use degenbot_substrate::concentrated_liquidity_route::{ApplyOutcome, BufferKind};
    use degenbot_substrate::state_lock::{LockSite, StateLock};

    use alloy::primitives::I256;

    use degenbot_decoders::v4_swap_decoder::V4PoolId;
    use degenbot_pools::V4PoolKey;

    use crate::bot_core::{LifecyclePrecondition, RefusalKind};
    use degenbot_pools::RegisterV4PoolParams;
    use degenbot_pools::TickInfo;
    use degenbot_pools::{PoolTickCoverage, RegisterV3PoolParams, RegistrationLifecycle};
    use degenbot_substrate::{BotState, PoolLookup};

    use super::{run_verify_lifecycle, LifecycleOutcome};

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

    /// A key that names NO registered concentrated-liquidity pool owes no
    /// verification — and proved none: the machine answers
    /// [`LifecycleOutcome::NotOwed`], never `Completed`, so a caller cannot
    /// record an unverified pool as verified. Both closures are `unreachable!`:
    /// there is no state to verify.
    #[tokio::test]
    async fn an_unregistered_key_owes_no_verification() {
        let core = new_core();
        let result = run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            PoolLookup::from_address(Address::from([0x90u8; 20])),
            Some(123),
            |_, _| async move { unreachable!("no seed verify for an unregistered key") },
            |_, _| async move { unreachable!("no post-drain verify for an unregistered key") },
        )
        .await;
        assert_eq!(
            result.expect("an unregistered key is not a refusal"),
            LifecycleOutcome::NotOwed,
            "nothing was owed and nothing was verified"
        );
    }

    /// A `Tracked` pool that leaves the registry while its seed is being
    /// verified has no map left to pin: the pin answers
    /// [`PostDrainPin::NotPinnable`] and the machine must answer
    /// [`LifecycleOutcome::NotOwed`] — never `Completed` — because the required
    /// post-drain verification never ran. The step-1 closure is the caller's one
    /// interleave point, so it unregisters the pool.
    #[tokio::test]
    async fn a_tracked_pool_that_leaves_the_registry_before_the_pin_owes_no_verification() {
        let core = new_core();
        let addr = Address::from([0x91u8; 20]);
        {
            let mut c = core.write_at(LockSite::Registration);
            reg_v3(&mut c, addr, PoolTickCoverage::Tracked);
        }
        let unregistering = Arc::clone(&core);
        let result = run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            PoolLookup::from_address(addr),
            Some(42),
            move |_seed, _block| {
                let core = Arc::clone(&unregistering);
                async move {
                    core.write_at(LockSite::Registration)
                        .unregister_pool(PoolLookup::from_address(addr));
                    Ok(())
                }
            },
            |_, _| async move { unreachable!("no post-drain verify: the pin had no map") },
        )
        .await;
        assert_eq!(
            result.expect("a vanished pool is not a refusal"),
            LifecycleOutcome::NotOwed,
            "a missing required verification is not a completed one"
        );
        let c = core.read_at(LockSite::Registration);
        assert_eq!(
            c.pool_id_by_address(&addr),
            None,
            "precondition: the pool left the registry mid-flight"
        );
    }

    /// A promoted pool carries NO verification residue: the seed and the
    /// post-drain pair are the inputs of the one call that owns them, so a Live
    /// pool never keeps a pinned map for a later call to verify against a clock
    /// that has since moved.
    #[tokio::test]
    async fn a_promoted_tracked_pool_keeps_no_verification_residue() {
        let core = new_core();
        let addr = Address::from([0x92u8; 20]);
        let key = PoolLookup::from_address(addr);
        let pid = {
            let mut c = core.write_at(LockSite::Registration);
            reg_v3_clocked(&mut c, addr, 100)
        };
        {
            let c = core.read_at(LockSite::Registration);
            assert!(
                c.snapshot_seed(key).is_some(),
                "precondition: a Tracked registration pins its seed"
            );
        }
        // No snapshot block means step-1 has no block to compare the seed at, so
        // the call owns the seed without verifying it — and drops it.
        let result = run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            key,
            None,
            |_, _| async move { unreachable!("no snapshot block, so no seed verify") },
            |_, _| async move { Ok(()) },
        )
        .await;
        assert_eq!(
            result.expect("the lifecycle completes"),
            LifecycleOutcome::Completed
        );
        let mut c = core.write_at(LockSite::Registration);
        assert_eq!(
            lifecycle_v3(&c, pid),
            RegistrationLifecycle::Live,
            "precondition: the pool is promoted"
        );
        assert_eq!(
            key.take_seed(&mut c),
            None,
            "a Live pool keeps no snapshot seed"
        );
        assert_eq!(
            key.take_post_drain(&mut c),
            None,
            "a Live pool keeps no pinned post-drain pair"
        );
    }

    /// The V4 twin of [`a_promoted_tracked_pool_keeps_no_verification_residue`]:
    /// one residue rule for both designs.
    #[tokio::test]
    async fn a_promoted_tracked_v4_pool_keeps_no_verification_residue() {
        let core = new_core();
        let pm = Address::from([0x93u8; 20]);
        let pid_bytes = [0x94u8; 32];
        let key = PoolLookup::V4 {
            pool_manager: pm,
            pool_id: pid_bytes,
        };
        let pid = {
            let mut c = core.write_at(LockSite::Registration);
            reg_v4_clocked(&mut c, pm, pid_bytes, 100)
        };
        {
            let c = core.read_at(LockSite::Registration);
            assert!(
                c.snapshot_seed(key).is_some(),
                "precondition: a Tracked V4 registration pins its seed"
            );
        }
        let result = run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            key,
            None,
            |_, _| async move { unreachable!("no snapshot block, so no seed verify") },
            |_, _| async move { Ok(()) },
        )
        .await;
        assert_eq!(
            result.expect("the V4 lifecycle completes"),
            LifecycleOutcome::Completed
        );
        let mut c = core.write_at(LockSite::Registration);
        assert_eq!(lifecycle_v4(&c, pid), RegistrationLifecycle::Live);
        assert_eq!(key.take_seed(&mut c), None, "no V4 seed residue on Live");
        assert_eq!(
            key.take_post_drain(&mut c),
            None,
            "no V4 post-drain pair residue on Live"
        );
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

    /// The tracked seed tick map: tick 60 initialized with gross/net 100.
    fn seed_tick_60() -> HashMap<i32, TickInfo> {
        let mut tick_data = HashMap::new();
        tick_data.insert(
            60,
            TickInfo {
                liquidity_gross: U128::from(100),
                liquidity_net: 100i128,
                block: 0,
            },
        );
        tick_data
    }

    /// Register a `Tracked` V3 pool whose imported seed stamps its tick map at
    /// `clock` — the DB-row shape, with no engine witnesses yet.
    fn reg_v3_clocked(core: &mut BotState, address: Address, clock: u64) -> u64 {
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
            tick_data: seed_tick_60(),
            update_block: clock,
            tick_data_block: Some(clock),
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            ..Default::default()
        })
        .expect("test setup: V3 tracked registration")
    }

    /// The V4 twin of [`reg_v3_clocked`].
    fn reg_v4_clocked(core: &mut BotState, pm: Address, pid: V4PoolId, clock: u64) -> u64 {
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
            tick_data: seed_tick_60(),
            update_block: clock,
            tick_data_block: Some(clock),
            coverage: PoolTickCoverage::Tracked,
            fetcher: None,
            known_bitmap_words: Vec::new(),
        })
        .expect("test setup: V4 tracked registration")
    }

    /// Stage one pump liquidity event at `block` on a quarantined pool and
    /// tombstone only `block - 1`, so the lifecycle's drain holds the event
    /// back — the shape whose pin is `Incomplete`.
    fn hold_back_pump_event(
        core: &mut BotState,
        key: PoolLookup,
        update: PoolLiquidityUpdate,
        block: u64,
    ) {
        assert_eq!(
            core.apply_liquidity_update(key, update),
            Ok(ApplyOutcome::Buffered(BufferKind::Pump)),
            "a quarantined pool's liquidity event is staged, not dropped"
        );
        core.concentrated_liquidity_mut()
            .advance_pump_complete_cutoff(block - 1);
    }

    /// An INCOMPLETE post-drain pin defers: the drain held a pump event back at
    /// the map's own clock, so no block is complete. The lifecycle must answer
    /// `Ok` WITHOUT running step-2 and WITHOUT promoting the pool — and must
    /// leave no pair behind for a later call to mistake for this drain's state.
    #[tokio::test]
    async fn tracked_v3_incomplete_drain_defers_without_verify_or_live() {
        let core = new_core();
        let addr = Address::from([0x31u8; 20]);
        let pid = {
            let mut c = core.write_at(LockSite::Registration);
            let pid = reg_v3_clocked(&mut c, addr, 100);
            hold_back_pump_event(
                &mut c,
                PoolLookup::from_address(addr),
                PoolLiquidityUpdate::new(-60, 60, 500_i128, 100),
                100,
            );
            pid
        };
        let post_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&post_calls);
        let result = run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            PoolLookup::from_address(addr),
            None, // step-1 out of the way: this test is about the drain's verdict
            |_, _| async move { Ok(()) },
            move |tick_data, block| {
                let seen = seen;
                async move {
                    seen.lock().unwrap().push((block, tick_data.len()));
                    Ok(())
                }
            },
        )
        .await;
        assert_eq!(
            result.expect("a deferred drain is Ok, not a refusal"),
            LifecycleOutcome::Deferred,
            "the machine reports the deferral instead of a silent no-op"
        );
        assert!(
            post_calls.lock().unwrap().is_empty(),
            "step-2 must not verify a map the drain itself contradicts"
        );
        {
            let c = core.read_at(LockSite::Registration);
            assert_eq!(
                lifecycle_v3(&c, pid),
                RegistrationLifecycle::VerificationDeferred,
                "an incomplete pin parks the pool: never Live, and never swept"
            );
        }
        {
            let mut c = core.write_at(LockSite::Registration);
            assert_eq!(
                PoolLookup::from_address(addr).take_post_drain(&mut c),
                None,
                "the deferred call leaves no post-drain pair behind"
            );
            assert_eq!(
                c.apply_liquidity_update(
                    PoolLookup::from_address(addr),
                    PoolLiquidityUpdate::new(-60, 60, 100_i128, 108),
                ),
                Ok(ApplyOutcome::Buffered(BufferKind::Pump)),
                "a parked pool keeps buffering its live events"
            );
        }
    }

    /// The retry after a deferral: once the tombstone closes the held-back
    /// block, the next lifecycle call drains it, verifies the DRAINED map at its
    /// own clock (never the stale pre-drain pair) and reaches `Live`.
    #[tokio::test]
    async fn tracked_v3_next_complete_drain_verifies_and_goes_live() {
        let core = new_core();
        let addr = Address::from([0x32u8; 20]);
        let pid = {
            let mut c = core.write_at(LockSite::Registration);
            let pid = reg_v3_clocked(&mut c, addr, 100);
            hold_back_pump_event(
                &mut c,
                PoolLookup::from_address(addr),
                PoolLiquidityUpdate::new(-60, 60, 500_i128, 100),
                100,
            );
            pid
        };
        let deferred = run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            PoolLookup::from_address(addr),
            None,
            |_, _| async move { Ok(()) },
            |_, _| async move { unreachable!("step-2 must not run while the drain is incomplete") },
        )
        .await;
        assert_eq!(
            deferred.expect("the first call defers"),
            LifecycleOutcome::Deferred
        );

        // The pump's next tombstone closes block 100, so the retry's drain may
        // release the Mint.
        {
            let mut c = core.write_at(LockSite::Registration);
            c.concentrated_liquidity_mut()
                .advance_pump_complete_cutoff(100);
        }

        let post_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&post_calls);
        let result = run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            PoolLookup::from_address(addr),
            None,
            |_, _| async move { Ok(()) },
            move |tick_data, block| {
                let seen = seen;
                async move {
                    seen.lock().unwrap().push((
                        block,
                        tick_data.len(),
                        tick_data.contains_key(&-60),
                    ));
                    Ok(())
                }
            },
        )
        .await;
        assert_eq!(
            result.expect("the retry completes"),
            LifecycleOutcome::Completed
        );
        assert_eq!(
            *post_calls.lock().unwrap(),
            vec![(100u64, 2usize, true)],
            "step-2 verifies the drained map at its own clock, WITH the Mint@100's tick — a \
             seed-only pair from before the drain would be (100, 1, false), so the stale pair \
             was not reused"
        );
        {
            let c = core.read_at(LockSite::Registration);
            assert_eq!(
                lifecycle_v3(&c, pid),
                RegistrationLifecycle::Live,
                "the completed retry promotes the pool"
            );
        }
    }

    /// The V4 twin of [`tracked_v3_incomplete_drain_defers_without_verify_or_live`].
    #[tokio::test]
    async fn tracked_v4_incomplete_drain_defers_without_verify_or_live() {
        let core = new_core();
        let pm = Address::from([0x45u8; 20]);
        let pid_bytes = [0x73u8; 32];
        let key = PoolLookup::V4 {
            pool_manager: pm,
            pool_id: pid_bytes,
        };
        let pid = {
            let mut c = core.write_at(LockSite::Registration);
            let pid = reg_v4_clocked(&mut c, pm, pid_bytes, 100);
            hold_back_pump_event(
                &mut c,
                key,
                PoolLiquidityUpdate::from_v4_wire(
                    -60,
                    60,
                    I256::try_from(500_i128).expect("500 fits i128"),
                    100,
                )
                .expect("valid V4 ModifyLiquidity wire update"),
                100,
            );
            pid
        };
        let post_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&post_calls);
        let result = run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            key,
            None,
            |_, _| async move { Ok(()) },
            move |tick_data, block| {
                let seen = seen;
                async move {
                    seen.lock().unwrap().push((block, tick_data.len()));
                    Ok(())
                }
            },
        )
        .await;
        assert_eq!(
            result.expect("a deferred drain is Ok, not a refusal"),
            LifecycleOutcome::Deferred,
            "the V4 machine reports the deferral too"
        );
        assert!(post_calls.lock().unwrap().is_empty());
        {
            let c = core.read_at(LockSite::Registration);
            assert_eq!(
                lifecycle_v4(&c, pid),
                RegistrationLifecycle::VerificationDeferred,
                "an incomplete V4 pin parks the pool: never Live, and never swept"
            );
        }
        {
            let mut c = core.write_at(LockSite::Registration);
            assert_eq!(
                key.take_post_drain(&mut c),
                None,
                "the deferred call leaves no post-drain pair behind"
            );
            assert_eq!(
                c.apply_liquidity_update(
                    key,
                    PoolLiquidityUpdate::from_v4_wire(
                        -60,
                        60,
                        I256::try_from(100_i128).expect("100 fits i128"),
                        108,
                    )
                    .expect("valid V4 ModifyLiquidity wire update"),
                ),
                Ok(ApplyOutcome::Buffered(BufferKind::Pump)),
                "a parked V4 pool keeps buffering its live events"
            );
        }
    }

    /// The V4 twin of [`tracked_v3_next_complete_drain_verifies_and_goes_live`].
    #[tokio::test]
    async fn tracked_v4_next_complete_drain_verifies_and_goes_live() {
        let core = new_core();
        let pm = Address::from([0x46u8; 20]);
        let pid_bytes = [0x74u8; 32];
        let key = PoolLookup::V4 {
            pool_manager: pm,
            pool_id: pid_bytes,
        };
        let pid = {
            let mut c = core.write_at(LockSite::Registration);
            let pid = reg_v4_clocked(&mut c, pm, pid_bytes, 100);
            hold_back_pump_event(
                &mut c,
                key,
                PoolLiquidityUpdate::from_v4_wire(
                    -60,
                    60,
                    I256::try_from(500_i128).expect("500 fits i128"),
                    100,
                )
                .expect("valid V4 ModifyLiquidity wire update"),
                100,
            );
            pid
        };
        let deferred = run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            key,
            None,
            |_, _| async move { Ok(()) },
            |_, _| async move { unreachable!("step-2 must not run while the drain is incomplete") },
        )
        .await;
        assert_eq!(
            deferred.expect("the first call defers"),
            LifecycleOutcome::Deferred
        );

        {
            let mut c = core.write_at(LockSite::Registration);
            c.concentrated_liquidity_mut()
                .advance_pump_complete_cutoff(100);
        }

        let post_calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = Arc::clone(&post_calls);
        let result = run_verify_lifecycle::<_, _, _, _, ()>(
            &core,
            key,
            None,
            |_, _| async move { Ok(()) },
            move |tick_data, block| {
                let seen = seen;
                async move {
                    seen.lock().unwrap().push((
                        block,
                        tick_data.len(),
                        tick_data.contains_key(&-60),
                    ));
                    Ok(())
                }
            },
        )
        .await;
        assert_eq!(
            result.expect("the V4 retry completes"),
            LifecycleOutcome::Completed
        );
        assert_eq!(
            *post_calls.lock().unwrap(),
            vec![(100u64, 2usize, true)],
            "step-2 verifies the drained V4 map at its own clock, WITH the ModifyLiquidity's \
             tick — the stale seed-only pair was not reused"
        );
        {
            let c = core.read_at(LockSite::Registration);
            assert_eq!(
                lifecycle_v4(&c, pid),
                RegistrationLifecycle::Live,
                "the completed V4 retry promotes the pool"
            );
        }
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
            let n = c.registered_v4_pool_count();
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

    /// The nested-read lock cycle on the real lock type: read#1 held while a
    /// second `read_at(Registration)` is requested, with a writer queued in
    /// between. `parking_lot` parks a new read behind a queued writer, and the
    /// writer waits on read#1 — a cycle. The fused accessors take one guard
    /// and cannot form it; this test pins the hazard that motivated the
    /// fusion.
    ///
    /// The scenario is established by events, never by waiting: the writer's
    /// QUEUE is proven by a probe read failing (a queuing writer owns the
    /// writer bit while it drains readers, so `try_read` fails exactly while a
    /// writer is queued or holding), and read#2's attempt is a channel
    /// barrier. read#1 lives on its own thread apart from read#2 so dropping
    /// it can break the cycle cleanly — the original same-thread shape held
    /// read#1 as an expression temporary and could not be interrupted.
    #[test]
    fn nested_read_with_queued_writer_is_a_lock_cycle() {
        // A deadlock guard only: the ordering evidence is every channel event
        // and recorded instant below, never this bound.
        let join_guard = std::time::Duration::from_secs(30);

        let core = new_core();
        let read1 = core.read_at(LockSite::Registration); // read#1

        let (writer_started_tx, writer_started_rx) = std::sync::mpsc::channel::<()>();
        let (writer_acquired_tx, writer_acquired_rx) = std::sync::mpsc::channel::<()>();
        let (release_writer_tx, release_writer_rx) = std::sync::mpsc::channel::<()>();
        let (writer_done_tx, writer_done_rx) = std::sync::mpsc::channel::<()>();
        let writer_core = Arc::clone(&core);
        std::thread::spawn(move || {
            writer_started_tx.send(()).expect("signal writer start");
            let mut guard = writer_core.write_at(LockSite::Registration);
            writer_acquired_tx
                .send(())
                .expect("signal writer acquisition");
            // The hold window is the only place where read#2 being stuck
            // behind the writer is observable exactly, so the test owns it.
            release_writer_rx.recv().expect("release the writer's hold");
            guard
                .concentrated_liquidity_mut()
                .advance_pump_complete_cutoff(1);
            drop(guard);
            writer_done_tx.send(()).expect("signal writer completion");
        });
        writer_started_rx
            .recv_timeout(join_guard)
            .expect("writer thread started");

        // The writer's QUEUE is the load-bearing fact. A queuing writer owns
        // the writer bit while it waits for readers, and a reader can never
        // hold that bit — so a probe read failing while read#1 is held IS the
        // queued-writer signal. The bound only guards a deadlock.
        let probe_deadline = std::time::Instant::now() + join_guard;
        while core.try_read_at(LockSite::Registration).is_some() {
            assert!(
                std::time::Instant::now() < probe_deadline,
                "the writer never queued behind read#1"
            );
            std::thread::yield_now();
        }

        let (reader_started_tx, reader_started_rx) = std::sync::mpsc::channel::<()>();
        let (read2_done_tx, read2_done_rx) =
            std::sync::mpsc::channel::<(std::time::Duration, std::time::Instant)>();
        let reader_core = Arc::clone(&core);
        // read#2 reports (blocked, acquired_at): both stamps are monotonic,
        // so comparing them across threads is exact. The assertions pin the
        // ORDER, never a duration.
        let read2 = std::thread::spawn(move || {
            reader_started_tx.send(()).expect("signal read#2 attempt");
            let t0 = std::time::Instant::now();
            let _g = reader_core.read_at(LockSite::Registration); // read#2
            read2_done_tx
                .send((t0.elapsed(), std::time::Instant::now()))
                .expect("deliver read#2's stamps");
        });
        reader_started_rx
            .recv_timeout(join_guard)
            .expect("read#2 thread started its attempt");

        // With the writer queued and read#1 held, read#2 cannot acquire — it
        // parks behind the writer: the nested-read cycle the fused accessors
        // remove.
        assert!(
            !read2.is_finished(),
            "read#2 must park behind the queued writer while read#1 is held"
        );

        let dropped_at = std::time::Instant::now();
        drop(read1); // breaks the cycle: the queued writer runs first

        // The queued writer takes the lock ahead of read#2, and while it
        // holds, read#2 still cannot finish — the cycle's writer link is
        // read#2 waiting on the writer, not merely on read#1's guard.
        writer_acquired_rx
            .recv_timeout(join_guard)
            .expect("the queued writer acquired once read#1 dropped");
        assert!(
            !read2.is_finished(),
            "read#2 must still be parked behind the writer's hold"
        );

        release_writer_tx
            .send(())
            .expect("release the writer's hold");
        let (blocked, acquired_at) = read2_done_rx
            .recv_timeout(join_guard)
            .expect("read#2 completes once the writer releases");
        writer_done_rx
            .recv_timeout(join_guard)
            .expect("the writer completes");
        assert!(
            acquired_at >= dropped_at,
            "read#2 only advanced after read#1 dropped ({blocked:?}) — that is the cycle"
        );
    }
}
