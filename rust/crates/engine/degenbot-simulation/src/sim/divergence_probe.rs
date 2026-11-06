//! The sim-vs-engine divergence observer — the env-gated, pure-observation
//! hook the in-process sim runs inside `BotStateDb::storage_ref`.
//!
//! When the sim SLOADs a storage slot, [`observe_storage_read`] compares the
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
//! Gated by `arm the sim-divergence probe` (set at launch). Default OFF → a
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
//! lag signal: does the engine trail the sim block?). The probe reads
//! these to pick fix path A (engine missing whole slot classes) / B
//! (shadow-RPC at sim block) / C (gated serve when caught up).
//!
//! A process-wide [`DivergenceTally`] is accumulated (slots compared,
//! divergent count, distinct divergent pools) and exposed via
//! [`divergence_tally_snapshot`] for a driver to assert + a periodic
//! [`dump_divergence_summary`] log line. That process-global is the PRODUCTION
//! default: an observation site handed a [`DivergenceTallyHandle`] accumulates
//! into that handle instead, so a caller that must read the counts of its own
//! reads owns them as a value and independent taps share nothing.

// The signed→unsigned bit-pattern casts in the tests (two's-complement tick
// packing) are intentional; clippy's `cast_sign_loss` suggestion is not a real
// std method.
#![cfg_attr(
    test,
    allow(clippy::unreadable_literal, clippy::decimal_bitwise_operands)
)]
use degenbot_core::{diag, op_info};

use std::sync::{Arc, Mutex, OnceLock};

use alloy::primitives::{Address, B256, U256};

use degenbot_substrate::{divergence_probe::TrackedSlotProbe, SimAnchorOracle};

/// The `[sim-divergence]` log prefix — verbatim so log greps return here.
const SIM_DIVERGENCE_LOG_PREFIX: &str = "[sim-divergence]";

/// The counts one accumulator has gathered (one entry per divergent slot
/// observation, in total and distinct sets). [`DivergenceTallyHandle::snapshot`]
/// reads a caller-owned one; [`divergence_tally_snapshot`] reads the
/// process-global; production logs a summary via [`dump_divergence_summary`].
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

/// A caller-owned divergence accumulator — the injection handle.
///
/// The process-global tally is the production default: an observation site
/// handed no handle accumulates there. A caller that must read the counts of
/// ITS OWN reads (a test, or a driver measuring one block) constructs a
/// handle, hands it to `BotStateDb::with_divergence_tally`, and reads it back
/// through [`Self::snapshot`]. A [`Clone`] shares this handle's accumulator —
/// one block's fan-outs land in one tally — while independent handles share
/// nothing, so a tap needs no serialization against other taps.
#[derive(Debug, Default, Clone)]
pub struct DivergenceTallyHandle {
    accum: Arc<Mutex<DivergenceTallyAccum>>,
}

impl DivergenceTallyHandle {
    /// An empty accumulator, independent of the process-global one.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a snapshot of this handle's counts (does NOT drain — the
    /// accumulator keeps counting).
    #[must_use]
    pub fn snapshot(&self) -> DivergenceTally {
        self.accum
            .lock()
            .map(|acc| acc.to_tally())
            .unwrap_or_default()
    }

    /// Fold one compared slot into this handle's counts.
    fn record(&self, address: Address, index: U256, matched: bool) {
        let Ok(mut acc) = self.accum.lock() else {
            return;
        };
        acc.slots_compared += 1;
        if !matched {
            acc.divergent_slots += 1;
            // The slot key for distinct-pair tracking: the index as a 32-byte
            // BE word (storage slots are 256-bit).
            let slot_bytes: [u8; 32] = index.to_be_bytes();
            acc.divergent_pairs
                .insert((address, B256::from(slot_bytes)));
            acc.divergent_pools.insert(address);
        }
    }
}

/// The process-global accumulator — the production default for every
/// observation site handed no handle.
static PROCESS_TALLY: OnceLock<DivergenceTallyHandle> = OnceLock::new();

fn process_tally() -> &'static DivergenceTallyHandle {
    PROCESS_TALLY.get_or_init(DivergenceTallyHandle::new)
}

/// Take a snapshot of the process-global divergence tally (does NOT drain —
/// the production tally keeps accumulating).
#[must_use]
pub fn divergence_tally_snapshot() -> DivergenceTally {
    process_tally().snapshot()
}

/// The masked RPC comparison: `true` iff the engine's tracked fields
/// (packed in `probe.engine_word`, untracked bits zeroed) match the RPC word
/// masked to the engine's tracked-bit range.
fn tracked_fields_match(probe: &TrackedSlotProbe, rpc_word: U256) -> bool {
    let mask = U256::from_be_bytes(probe.kind.tracked_bit_mask().0);
    let engine = U256::from_be_bytes(probe.engine_word.0);
    engine == (rpc_word & mask)
}

/// The pure-observation hook a `BotStateDb` storage read calls after fetching
/// the RPC value. If `address`+`index` maps to a tracked pool scalar slot, it
/// compares the packed engine word against the masked RPC word, logs a
/// `[sim-divergence]` line on disagreement, and accumulates `tally` (the
/// process-global one when the caller names none). The RPC value is returned
/// unchanged by the caller — this fn never affects what the sim reads.
///
/// `index` + `rpc_value` are the storage-read slot-key / slot-value words
/// (both `U256`) — taken as `U256` to bridge the `alloy` umbrella cleanly.
pub fn observe_storage_read(
    oracle: &dyn SimAnchorOracle,
    address: Address,
    index: U256,
    rpc_value: U256,
    tally: Option<&DivergenceTallyHandle>,
) {
    observe_storage_read_forced(oracle, address, index, rpc_value, tally);
}

/// VERIFY2 T2: on-demand form — the armed arm of the pair (a sim-failure
/// re-verify or a random spot-check), carrying the same tally contract and the
/// same pure-observation contract as `observe_storage_read` (the RPC value is
/// returned unchanged by the caller).
pub fn observe_storage_read_forced(
    oracle: &dyn SimAnchorOracle,
    address: Address,
    index: U256,
    rpc_value: U256,
    tally: Option<&DivergenceTallyHandle>,
) {
    // The engine's oracle is the build-time SNAPSHOT — scalar slots
    // compare against the engine-at-build words; tick slots are not
    // snapshotted and fall through. A state-less view (the boot registry)
    // answers `None`, leaving the observer inert.
    let Some(probe) = oracle.probe_tracked_storage_slot(address, index) else {
        // Not a tracked-pool scalar slot (non-pool contract, a V3/V4
        // fee-growth / tick-bitmap slot the engine doesn't carry, etc.) —
        // no comparison, no tally increment.
        return;
    };
    let matched = tracked_fields_match(&probe, rpc_value);
    tally
        .unwrap_or_else(|| process_tally())
        .record(address, index, matched);
    if matched {
        return;
    }
    op_info!(domain = sim, pool_addr = %format!("{address:?}"),
        slot = %index,
        kind = ?probe.kind,
        engine = %hex_padded(probe.engine_word),
        rpc = %hex_padded_u256(rpc_value),
        update_block = probe.update_block,
        "{SIM_DIVERGENCE_LOG_PREFIX}"
    );
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
/// call from a driver per-batch or at shutdown. Emitted at `debug`, so it is
/// silent unless the `sim` diagnostics stream is enabled at the sink.
pub fn dump_divergence_summary() {
    let tally = divergence_tally_snapshot();
    diag!(
        domain = sim,
        slots_compared = tally.slots_compared,
        divergent_slots = tally.divergent_slots,
        divergent_pairs = tally.divergent_pairs,
        divergent_pools = tally.divergent_pools,
        "{SIM_DIVERGENCE_LOG_PREFIX} summary"
    );
}

#[expect(clippy::expect_used, clippy::cast_sign_loss)]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::evm2::bot_state_db::BotStateDb;
    use alloy::primitives::{address, Address, B256, U256};
    use degenbot_pools::RegisterV3PoolParams;
    use degenbot_substrate::divergence_probe::TrackedSlotKind;
    use degenbot_substrate::{divergence_probe::TrackedSlotProbe, BotState, SimAnchorState};
    use hashbrown::HashMap;

    use ::evm2::evm::Database as _;

    const V3_ADDR: Address = address!("888888875ce34e0b60a4a79bb5bc5d34b7e5fab4");

    /// A mock `Database` that serves a FIXED storage value per (address,
    /// slot). Used to drive `get_storage` against a controlled RPC value so
    /// the divergence probe can be asserted deterministically.
    #[derive(Default)]
    struct FixedStorageDb {
        slots: std::collections::HashMap<(Address, U256), U256>,
    }

    impl ::evm2::evm::Database for FixedStorageDb {
        type Error = std::convert::Infallible;
        fn get_account(
            &mut self,
            _address: &Address,
        ) -> Result<Option<::evm2::evm::AccountInfo>, Self::Error> {
            Ok(None)
        }
        fn get_code_by_hash(
            &mut self,
            _code_hash: &B256,
        ) -> Result<::evm2::bytecode::Bytecode, Self::Error> {
            Ok(::evm2::bytecode::Bytecode::default())
        }
        fn get_storage(
            &mut self,
            address: &Address,
            index: &::evm2::interpreter::Word,
        ) -> Result<::evm2::interpreter::Word, Self::Error> {
            Ok(self
                .slots
                .get(&(*address, *index))
                .copied()
                .unwrap_or(U256::ZERO))
        }
        fn get_block_hash(
            &mut self,
            _number: &::evm2::interpreter::Word,
        ) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
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
            tick_data: HashMap::new(),
            update_block,
            coverage: degenbot_pools::PoolTickCoverage::Sparse,
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
        let anchor = SimAnchorState::snapshot(&core);
        // rpc serves slot0 = a DIFFERENT tick → would diverge IF the probe
        // compared; but the probe must NOT change the returned value either way
        // (regardless of the arm — observation only).
        let rpc_word = rpc_slot0_with_tick(U256::from(1u128) << 96, 5010);
        let tally = DivergenceTallyHandle::new();

        // BOTH observation arms: the ambient one and the armed one.
        for armed in [false, true] {
            let mut db = FixedStorageDb::default();
            db.slots.insert((V3_ADDR, U256::ZERO), rpc_word);
            let mut bot_db = BotStateDb::new(&anchor, db)
                .with_divergence_probe(armed)
                .with_divergence_tally(tally.clone());
            let got = bot_db
                .get_storage(&V3_ADDR, &U256::ZERO)
                .expect("storage read ok");
            assert_eq!(
                got, rpc_word,
                "rpc value returned unchanged (armed={armed})"
            );
        }
        assert_eq!(
            tally.snapshot().slots_compared,
            2,
            "each arm observes through the tally it was handed"
        );
    }

    #[test]
    fn observe_logs_divergence_when_engine_lags_rpc_and_tally_accumulates() {
        // Exercise the divergence path directly: an engine V3 pool whose tick
        // disagrees with a fixed rpc slot0, observed into a CALLER-OWNED tally.
        let core = v3_pool(U256::from(1u128) << 96, 1_000_000, -5010, 18_000_000);
        let anchor = SimAnchorState::snapshot(&core);
        let rpc_word = rpc_slot0_with_tick(U256::from(1u128) << 96, 5010);
        let tally = DivergenceTallyHandle::new();

        observe_storage_read(&anchor, V3_ADDR, U256::ZERO, rpc_word, Some(&tally));

        let counts = tally.snapshot();
        assert_eq!(counts.slots_compared, 1, "one tracked slot compared");
        assert_eq!(counts.divergent_slots, 1, "the tick diverged");
        assert_eq!(counts.divergent_pools, 1, "one distinct pool");
        assert_eq!(
            DivergenceTallyHandle::new().snapshot().slots_compared,
            0,
            "handles are independent — the observation landed only in the handed one"
        );
    }

    #[test]
    fn observe_never_complains_when_engine_matches_rpc() {
        let sqrt = U256::from(1u128) << 96;
        let core = v3_pool(sqrt, 1_000_000, -5010, 18_000_000);
        let anchor = SimAnchorState::snapshot(&core);
        // rpc slot0 with the SAME tick as the engine → no divergence.
        let rpc_word = rpc_slot0_with_tick(sqrt, -5010);
        let tally = DivergenceTallyHandle::new();

        observe_storage_read(&anchor, V3_ADDR, U256::ZERO, rpc_word, Some(&tally));
        let counts = tally.snapshot();
        assert_eq!(counts.slots_compared, 1, "compared once");
        assert_eq!(counts.divergent_slots, 0, "matched → not flagged");
    }

    #[test]
    fn observe_ignores_untracked_slot() {
        // feeGrowthGlobal0X128 (slot 1) is NOT tracked → observe does nothing.
        let core = v3_pool(U256::from(1u128) << 96, 1_000_000, 0, 18_000_000);
        let anchor = SimAnchorState::snapshot(&core);
        let tally = DivergenceTallyHandle::new();
        observe_storage_read(
            &anchor,
            V3_ADDR,
            U256::from(1u64),
            U256::from(0xdeadbeefu64),
            Some(&tally),
        );
        assert_eq!(
            tally.snapshot().slots_compared,
            0,
            "untracked slot never compared"
        );
    }
}
