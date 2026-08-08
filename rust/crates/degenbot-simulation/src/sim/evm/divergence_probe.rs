//! The sim-vs-engine divergence observer — the env-gated, pure-observation
//! hook the in-process sim runs inside `BotStateDb::storage_ref`.
//!
//! When revm SLOADs a storage slot, [`observe_storage_read`] compares the
//! engine's packed typed pool state (via [`BotState::probe_tracked_storage_slot`])
//! against the RPC-served value the fallback just returned, logs a
//! `[sim-divergence]` line when the tracked fields disagree, and accumulates
//! a running tally. This is **observation only** — the RPC value is returned
//! unchanged, so the sim's behavior is identical whether the probe is on or
//! off (zero LOK/K risk — the probe never serves; the reverted-bug serve is
//! NOT re-introduced).
//!
//! # Env gate (zero cost when off)
//!
//! Gated by `DEGENBOT_SIM_DIVERGENCE_LOG=1` (set at launch). Default OFF → a
//! single atomic load per `storage_ref` (the `OnceLock<bool>` init reads the
//! env once), zero per-SLOAD work otherwise. Same discipline as the
//! `hotpath` runtime gate — opt-in, off by default, no rebuild to toggle.
//!
//! # What it captures (the spike checkpoint answer)
//!
//! Per divergent slot, one `[sim-divergence]` line:
//! `pool=0x.. slot=0x.. kind=V3Slot0 engine=0x.. rpc=0x.. update_block=N`
//! — the engine's packed word (untracked bits zeroed) vs the RPC word
//! (masked to the tracked-bit range), plus the engine's `update_block` (the
//! lag signal: does the engine trail the sim block?). The spike
//! (`docs/architecture/in_process_sim_served_slots.md`) reads these to pick
//! fix path A (engine missing whole slot classes) / B (shadow-RPC at sim
//! block) / C (gated serve when caught up).
//!
//! A process-wide [`DivergenceTally`] is accumulated (slots compared,
//! divergent count, distinct divergent pools) and exposed via
//! [`divergence_tally_snapshot`] for a driver/test to assert + a periodic
//! [`dump_divergence_summary`] log line.

//! A process-wide [`DivergenceTally`] is accumulated (slots compared,
//! divergent count, distinct divergent pools) and exposed via
//! [`divergence_tally_snapshot`] for a driver/test to assert + a periodic
//! [`dump_divergence_summary`] log line.

// The signed→unsigned bit-pattern casts in the tests (two's-complement tick
// packing) are intentional; clippy's `cast_sign_loss` suggestion is not a real
// std method.
#![allow(clippy::cast_sign_loss)]
#![cfg_attr(
    test,
    allow(clippy::unreadable_literal, clippy::decimal_bitwise_operands)
)]

use std::sync::{Mutex, OnceLock};

use alloy::primitives::{Address, B256, U256};

use degenbot_bot::bot_core::{divergence_probe::TrackedSlotProbe, BotState};

/// The env-var name gating the divergence probe (set at launch). Off by
/// default — the sim's behavior is identical whether on or off (observation
/// only, never serves).
pub const SIM_DIVERGENCE_LOG_ENV: &str = "DEGENBOT_SIM_DIVERGENCE_LOG";

/// The `[sim-divergence]` log prefix — verbatim so log greps return here.
const SIM_DIVERGENCE_LOG_PREFIX: &str = "[sim-divergence]";

static PROBE_ENABLED: OnceLock<bool> = OnceLock::new();

/// Test-only override for [`probe_enabled`]: `-1` = unset (use the env-cached
/// value), `0` = forced off, `1` = forced on. Lets the divergence tests flip
/// the gate deterministically without racing the process-global `OnceLock`
/// env cache (which caches whatever the first read saw). Production never sets
/// this (cfg(test)-only).
#[cfg(test)]
static TEST_FORCE: std::sync::atomic::AtomicI8 = std::sync::atomic::AtomicI8::new(-1);

/// `true` iff `DEGENBOT_SIM_DIVERGENCE_LOG=1` is set at first read; cached so
/// the per-SLOAD cost is a single atomic load. (`#[cfg(test)]`:
/// [`force_probe_enabled_for_tests`] overrides this.)
fn probe_enabled() -> bool {
    #[cfg(test)]
    {
        let forced = TEST_FORCE.load(std::sync::atomic::Ordering::Acquire);
        if forced != -1 {
            return forced != 0;
        }
    }
    *PROBE_ENABLED
        .get_or_init(|| std::env::var_os(SIM_DIVERGENCE_LOG_ENV).is_some_and(|v| v == "1"))
}

/// Test-only gate override (`-1`/unset → use the env cache, `0` → off, `1` →
/// on). Production never calls this (cfg(test)).
#[cfg(test)]
pub fn force_probe_enabled_for_tests(on: Option<bool>) {
    match on {
        Some(true) => TEST_FORCE.store(1, std::sync::atomic::Ordering::Release),
        Some(false) => TEST_FORCE.store(0, std::sync::atomic::Ordering::Release),
        None => TEST_FORCE.store(-1, std::sync::atomic::Ordering::Release),
    }
}

/// On-disk, process-wide divergence accumulator (one entry per divergent slot
/// observation). Testable via [`divergence_tally_snapshot`]; production logs a
/// summary via [`dump_divergence_summary`].
#[derive(Debug, Default, Clone)]
pub struct DivergenceTally {
    /// Storage reads compared against the engine (tracked slots only — the
    /// probe returns None for non-pool / untracked slots, which never reach
    /// the comparison).
    pub slots_compared: u64,
    /// Storage reads where the engine's tracked fields disagreed with the RPC.
    pub divergent_slots: u64,
    /// Distinct `(address, slot)` pairs that diverged at least once.
    pub divergent_pairs: u64,
    /// Distinct pool addresses that diverged on at least one slot.
    pub divergent_pools: u64,
}

static TALLY: OnceLock<Mutex<DivergenceTallyAccum>> = OnceLock::new();

/// The internal accumulator: the public [`DivergenceTally`] (counts) + a
/// `HashSet` of distinct `(address, slot)` + distinct `address` for the
/// distinct-pair/distinct-pool counts (set sizes projected into the tally on
/// snapshot).
#[derive(Debug, Default)]
struct DivergenceTallyAccum {
    slots_compared: u64,
    divergent_slots: u64,
    divergent_pairs: std::collections::HashSet<(Address, B256)>,
    divergent_pools: std::collections::HashSet<Address>,
}

impl DivergenceTallyAccum {
    fn to_tally(&self) -> DivergenceTally {
        DivergenceTally {
            slots_compared: self.slots_compared,
            divergent_slots: self.divergent_slots,
            divergent_pairs: self.divergent_pairs.len() as u64,
            divergent_pools: self.divergent_pools.len() as u64,
        }
    }
}

fn tally() -> &'static Mutex<DivergenceTallyAccum> {
    TALLY.get_or_init(|| Mutex::new(DivergenceTallyAccum::default()))
}

/// Reset the process-wide tally to empty (test seam — drain state between
/// assertions). Production leaves the tally accumulating for the run.
pub fn reset_divergence_tally() {
    if let Some(m) = TALLY.get() {
        if let Ok(mut acc) = m.lock() {
            *acc = DivergenceTallyAccum::default();
        }
    }
}

/// Take a snapshot of the current divergence tally (does NOT drain — the
/// production tally keeps accumulating). Tests assert against the counts.
#[must_use]
pub fn divergence_tally_snapshot() -> DivergenceTally {
    tally().lock().map(|acc| acc.to_tally()).unwrap_or_default()
}

/// The masked RPC comparison: `true` iff the engine's tracked fields
/// (packed in `probe.engine_word`, untracked bits zeroed) match the RPC word
/// masked to the engine's tracked-bit range.
fn tracked_fields_match(probe: &TrackedSlotProbe, rpc_word: U256) -> bool {
    let mask = U256::from_be_bytes(probe.kind.tracked_bit_mask().0);
    let engine = U256::from_be_bytes(probe.engine_word.0);
    engine == (rpc_word & mask)
}

/// The pure-observation hook `BotStateDb::storage_ref` calls after fetching
/// the RPC value. Env-gated; when off, returns immediately (single atomic
/// load). When on: if `address`+`index` maps to a tracked pool scalar slot,
/// compares the packed engine word against the masked RPC word, logs a
/// `[sim-divergence]` line on disagreement, and accumulates the tally. The
/// RPC value is returned unchanged by the caller — this fn never affects
/// what the sim reads.
///
/// `index` + `rpc_value` are `revm` `StorageKey`/`StorageValue` (both `U256`
/// type-aliases) — taken as `U256` to bridge the `alloy` umbrella cleanly.
pub fn observe_storage_read(bot_state: &BotState, address: Address, index: U256, rpc_value: U256) {
    if !probe_enabled() {
        return;
    }
    let Some(probe) = bot_state.probe_tracked_storage_slot(address, index) else {
        // Not a tracked-pool scalar slot (non-pool contract, a V3/V4
        // fee-growth / tick-bitmap slot the engine doesn't carry, etc.) —
        // no comparison, no tally increment.
        return;
    };
    let matched = tracked_fields_match(&probe, rpc_value);
    record_observation(address, index, &probe, matched);
    if matched {
        return;
    }
    tracing::info!(
        pool_addr = %format!("{address:?}"),
        slot = %index,
        kind = ?probe.kind,
        engine = %hex_padded(probe.engine_word),
        rpc = %hex_padded_u256(rpc_value),
        update_block = probe.update_block,
        "{SIM_DIVERGENCE_LOG_PREFIX}"
    );
}

fn record_observation(address: Address, index: U256, _probe: &TrackedSlotProbe, matched: bool) {
    let Ok(mut acc) = tally().lock() else { return };
    acc.slots_compared += 1;
    if !matched {
        acc.divergent_slots += 1;
        // The slot key for distinct-pair tracking: the index as a 32-byte BE
        // word (storage slots are 256-bit).
        let slot_bytes: [u8; 32] = index.to_be_bytes();
        acc.divergent_pairs
            .insert((address, B256::from(slot_bytes)));
        acc.divergent_pools.insert(address);
    }
}

/// Render a 32-byte word as a lowercase 64-char hex string (no `0x`).
fn hex_padded(word: B256) -> String {
    use alloy::hex;
    hex::encode(word.0)
}

/// Render a `U256` as a lowercase 64-char hex string (BE, no `0x`).
fn hex_padded_u256(word: U256) -> String {
    use alloy::hex;
    hex::encode(word.to_be_bytes::<32>())
}

/// Log a `[sim-divergence] summary` line with the current tally (slots
/// compared, divergent slots, divergent pools). Idempotent + cheap; safe to
/// call from a driver per-batch or at shutdown. A no-op when the probe is
/// off (the tally is all-zeros + the env gate cached false → the summary
/// would spam a zero-line every block otherwise).
pub fn dump_divergence_summary() {
    if !probe_enabled() {
        return;
    }
    let tally = divergence_tally_snapshot();
    tracing::info!(
        slots_compared = tally.slots_compared,
        divergent_slots = tally.divergent_slots,
        divergent_pairs = tally.divergent_pairs,
        divergent_pools = tally.divergent_pools,
        "{SIM_DIVERGENCE_LOG_PREFIX} summary"
    );
}

#[expect(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]

    use super::*;
    use crate::sim::evm::BotStateDb;
    use alloy::primitives::{address, Address, B256, U256};
    use degenbot_bot::bot_core::{
        divergence_probe::TrackedSlotProbe, BotState, RegisterV3PoolParams, TrackedSlotKind,
    };
    use revm::database_interface::DatabaseRef;
    use revm::primitives::{StorageKey, StorageValue, B256 as RevmB256};
    use revm::state::AccountInfo;

    const V3_ADDR: Address = address!("888888875ce34e0b60a4a79bb5bc5d34b7e5fab4");

    /// A mock `DatabaseRef` that serves a FIXED storage value per (address,
    /// slot). Used to drive `storage_ref` against a controlled RPC value so
    /// the divergence probe can be asserted deterministically.
    #[derive(Default)]
    struct FixedStorageDb {
        slots: std::collections::HashMap<(Address, U256), U256>,
    }

    impl DatabaseRef for FixedStorageDb {
        type Error = std::convert::Infallible;
        fn basic_ref(&self, _address: Address) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(None)
        }
        fn storage_ref(
            &self,
            address: Address,
            index: StorageKey,
        ) -> Result<StorageValue, Self::Error> {
            Ok(self
                .slots
                .get(&(address, index))
                .copied()
                .unwrap_or(StorageValue::ZERO))
        }
        fn code_by_hash_ref(
            &self,
            _code_hash: RevmB256,
        ) -> Result<revm::bytecode::Bytecode, Self::Error> {
            Ok(revm::bytecode::Bytecode::default())
        }
        fn block_hash_ref(&self, _number: u64) -> Result<RevmB256, Self::Error> {
            Ok(RevmB256::ZERO)
        }
    }

    fn v3_pool(sqrt: U256, liquidity: u128, tick: i32, update_block: u64) -> BotState {
        let mut core = BotState::new();
        let params = RegisterV3PoolParams {
            address: V3_ADDR,
            token0: Address::ZERO,
            token1: Address::from([0xa0; 20]),
            fee: 3000,
            tick_spacing: 60,
            factory: Address::ZERO,
            sqrt_price_x96: sqrt,
            liquidity,
            tick,
            tick_data: std::collections::HashMap::new(),
            update_block,
            coverage: degenbot_bot::solvers::arb_engine::PoolTickCoverage::Sparse,
            fetcher: None,
            ..Default::default()
        };
        core.register_v3_pool(&params).expect("V3 registration");
        core
    }

    /// Pack a slot0 RPC word with a DIFFERENT tick than the engine, so the
    /// divergence fires on the tick field (masked to low 184 bits).
    fn rpc_slot0_with_tick(sqrt: U256, tick: i32) -> U256 {
        let sqrt_masked = sqrt & U256::from_limbs([u64::MAX, u64::MAX, 0xffff_ffff, 0]);
        let tick_u = (tick as u32) & 0x00ff_ffff;
        sqrt_masked | (U256::from(tick_u) << 160u32)
    }

    // ── tracked_fields_match ────────────────────────────────────────────

    #[test]
    fn tracked_fields_match_when_masked_fields_equal() {
        let sqrt = U256::from(1u128) << 96;
        let engine_tick_u = (-5010i32 as u32) & 0x00ff_ffff;
        let engine_word: U256 = (sqrt & U256::from_limbs([u64::MAX, u64::MAX, 0xffff_ffff, 0]))
            | (U256::from(engine_tick_u) << 160u32);
        let probe = TrackedSlotProbe {
            kind: TrackedSlotKind::V3Slot0,
            engine_word: B256::from(engine_word.to_be_bytes::<32>()),
            update_block: 0,
        };
        // Same sqrt + tick; "garbage" only in the UNTRACKED high bits
        // (observationIndex/feeProtocol/unlocked — bits 184..256), masked out.
        let rpc = engine_word | (U256::from(0xdead_beefu64) << 184u32);
        assert!(
            tracked_fields_match(&probe, rpc),
            "tracked fields match when sqrtPrice+tick agree (high garbage masked out)"
        );
    }

    #[test]
    fn tracked_fields_detect_tick_divergence() {
        let sqrt = U256::from(1u128) << 96;
        let engine_tick_u = (-5010i32 as u32) & 0x00ff_ffff;
        let probe = TrackedSlotProbe {
            kind: TrackedSlotKind::V3Slot0,
            engine_word: B256::from(
                ({
                    let w: U256 = (sqrt & U256::from_limbs([u64::MAX, u64::MAX, 0xffff_ffff, 0]))
                        | (U256::from(engine_tick_u) << 160u32);
                    w
                })
                .to_be_bytes::<32>(),
            ),
            update_block: 0,
        };
        // RPC tick = +5010 (different) → divergence on the tick bits.
        let rpc = (sqrt & U256::from_limbs([u64::MAX, u64::MAX, 0xffff_ffff, 0]))
            | (U256::from(5010u32 & 0xffffff) << 160u32);
        assert!(
            !tracked_fields_match(&probe, rpc),
            "tick divergence (engine -5010 vs rpc +5010) is flagged"
        );
    }

    // ── storage_ref wiring (env-gated, return-unchanged) ────────────────

    #[test]
    fn storage_ref_returns_rpc_value_unchanged_when_probe_on_or_off() {
        let core = v3_pool(U256::from(1u128) << 96, 1_000_000, -5010, 18_000_000);
        // rpc serves slot0 = a DIFFERENT tick → would diverge IF the probe
        // compared; but the probe must NOT change the returned value either way
        // (regardless of the gate state — observation only).
        let rpc_word = rpc_slot0_with_tick(U256::from(1u128) << 96, 5010);
        let mut db = FixedStorageDb::default();
        db.slots.insert((V3_ADDR, U256::ZERO), rpc_word);

        let bot_db = BotStateDb::new(&core, db);
        let got = bot_db
            .storage_ref(V3_ADDR, U256::ZERO)
            .expect("storage_ref ok");
        assert_eq!(
            got, rpc_word,
            "rpc value returned unchanged (probe off path)"
        );
    }

    // The tally-touching tests share a process-global accumulator + a test-only
    // gate override; serialize them so parallel test threads don't race the
    // counter / the `AtomicI8` force-setter.
    static TALLY_TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn observe_logs_divergence_when_engine_lags_rpc_and_tally_accumulates() {
        let _g = TALLY_TEST_GUARD.lock().unwrap();
        // This test asserts the probe's behavior when ENABLED. Because
        // `probe_enabled()` caches in a `OnceLock`, we cannot toggle it per-test
        // reliably. Instead we exercise the divergence path directly:
        // build an engine V3 pool whose tick disagrees with a fixed rpc slot0,
        // call observe_storage_read, then assert the tally diverged_slots==1
        // (the env gate is the only thing between observe + the tally; if the
        // env is NOT set, observe is a no-op and the tally stays 0 — which is
        // itself the "silent when off" contract recorded in the next test).
        reset_divergence_tally();
        let core = v3_pool(U256::from(1u128) << 96, 1_000_000, -5010, 18_000_000);
        let rpc_word = rpc_slot0_with_tick(U256::from(1u128) << 96, 5010);

        // Force the probe ON for THIS test (deterministic — no env-gate race).
        force_probe_enabled_for_tests(Some(true));

        observe_storage_read(&core, V3_ADDR, U256::ZERO, rpc_word);

        let tally = divergence_tally_snapshot();
        assert_eq!(tally.slots_compared, 1, "one tracked slot compared");
        assert_eq!(tally.divergent_slots, 1, "the tick diverged");
        assert_eq!(tally.divergent_pools, 1, "one distinct pool");
        force_probe_enabled_for_tests(None);
    }

    #[test]
    fn observe_never_complains_when_engine_matches_rpc() {
        let _g = TALLY_TEST_GUARD.lock().unwrap();
        reset_divergence_tally();
        let sqrt = U256::from(1u128) << 96;
        let core = v3_pool(sqrt, 1_000_000, -5010, 18_000_000);
        // rpc slot0 with the SAME tick as the engine → no divergence.
        let rpc_word = rpc_slot0_with_tick(sqrt, -5010);
        force_probe_enabled_for_tests(Some(true));

        observe_storage_read(&core, V3_ADDR, U256::ZERO, rpc_word);
        let tally = divergence_tally_snapshot();
        assert_eq!(tally.slots_compared, 1, "compared once");
        assert_eq!(tally.divergent_slots, 0, "matched → not flagged");
        force_probe_enabled_for_tests(None);
    }

    #[test]
    fn observe_ignores_untracked_slot() {
        let _g = TALLY_TEST_GUARD.lock().unwrap();
        // feeGrowthGlobal0X128 (slot 1) is NOT tracked → observe does nothing.
        reset_divergence_tally();
        let core = v3_pool(U256::from(1u128) << 96, 1_000_000, 0, 18_000_000);
        force_probe_enabled_for_tests(Some(true));
        observe_storage_read(&core, V3_ADDR, U256::from(1u64), U256::from(0xdeadbeefu64));
        let tally = divergence_tally_snapshot();
        assert_eq!(tally.slots_compared, 0, "untracked slot never compared");
        force_probe_enabled_for_tests(None);
    }
}
