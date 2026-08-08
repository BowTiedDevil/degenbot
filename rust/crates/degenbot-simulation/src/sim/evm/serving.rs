//! The serving seam — make `BotStateDb::storage_ref` return the engine's
//! packed typed pool state for tracked slots, instead of forwarding to the
//! RPC fallback.
//!
//! # Status (POC — premise refuted)
//!
//! This seam was built to test the "stale engine state causes
//! `CurrencyNotSettled`" hypothesis (ergo `TR6GWT`, originally "path A"). That
//! premise was REFUTED by mainnet data: V3 hops matched the actual swap
//! output exactly (engine state is correct — stale state would diverge V3
//! too), while only the V4 swap diverged by 1-8 units (a solver calc
//! rounding divergence, not stale state). See
//! `docs/architecture/sim_v4_swap_step_rounding.md`. The seam stays gated OFF
//! in production and is retained as a dead switch for future re-probing; the
//! divergence probe (`super::divergence_probe`) proved the engine's tracked
//! scalar slots are byte-identical to the RPC at sim time.
//!
//! # Env gate (zero cost + zero risk when off — the production status quo)
//!
//! Gated by `DEGENBOT_SIM_SERVE_ENGINE_STATE=1` (set at launch). DEFAULT OFF →
//! `storage_ref` forwards every read to the RPC fallback (the safe behavior
//! that has held since the reverted serve was removed; a single atomic load
//! per `storage_ref`). Enabling the seam requires the engine to carry the FULL
//! slot set the pool's `swap()` callback reads — a partial serve
//! (slot0/liquidity/reserves WITHOUT `feeGrowthGlobal`/`tickBitmap`/
//! per-pair balances) reintroduces the documented K-invariant / `LOK` reverts
//! (the engine's served slot0 is read by the same `swap()` callback that reads
//! RPC-served fee-growth/bitmap → intra-sim inconsistency).
//!
//! # What it serves
//!
//! The mechanism is generic over [`BotState::probe_tracked_storage_slot`]:
//! any tracked slot the probe can pack is served when the gate is on. Today
//! the probe covers V2 reserves (slot 8), V3/V4 `slot0`/`liquidity`, and the
//! per-tick `ticks(tick)` slot+0 (`liquidityGross`/`liquidityNet`).
//!
//! # Log line
//!
//! When serving intercepts a slot, one `[bot-state-db]` line per cold read:
//! `pool=0x.. slot=0x.. kind=V2Reserves served=0x.. rpc=0x.. delta_xor=0x..
//! update_block=N` — the served engine word, the RPC fallback value, the
//! XOR delta (which bits differ), and the engine's `update_block` (the lag
//! signal). Independent of the divergence-observation gate
//! (`DEGENBOT_SIM_DIVERGENCE_LOG`): serving is a behavior change, observation
//! is not; both can be on together (dev) — the two log prefixes are distinct.

#![cfg_attr(test, allow(clippy::unreadable_literal))]

use std::sync::OnceLock;

use alloy::primitives::{Address, U256};

use degenbot_bot::bot_core::BotState;

/// The env-var name gating the serving seam (set at launch). DEFAULT OFF —
/// the sim forwards every read to the RPC (the safe status quo; the reverted
/// serve is NOT re-introduced). Enabling requires the engine to carry the
/// full slot set the pool's `swap()` callback reads (premise refuted; retained
/// as a dead switch).
pub const SIM_SERVE_ENGINE_STATE_ENV: &str = "DEGENBOT_SIM_SERVE_ENGINE_STATE";

/// The `[bot-state-db]` log prefix — verbatim so log greps return here.
const BOT_STATE_DB_LOG_PREFIX: &str = "[bot-state-db]";

static SERVE_ENABLED: OnceLock<bool> = OnceLock::new();

/// Test-only override for [`serve_enabled`]: `-1` = unset (use the env-cached
/// value), `0` = forced off, `1` = forced on. Lets the serving tests flip
/// the gate deterministically without racing the process-global `OnceLock`
/// env cache. Production never sets this (cfg(test)-only).
#[cfg(test)]
static TEST_FORCE: std::sync::atomic::AtomicI8 = std::sync::atomic::AtomicI8::new(-1);

/// `true` iff `DEGENBOT_SIM_SERVE_ENGINE_STATE=1` is set at first read;
/// cached so the per-SLOAD cost is a single atomic load. (`#[cfg(test)]`:
/// [`force_serve_enabled_for_tests`] overrides this.)
fn serve_enabled() -> bool {
    #[cfg(test)]
    {
        let forced = TEST_FORCE.load(std::sync::atomic::Ordering::Acquire);
        if forced != -1 {
            return forced != 0;
        }
    }
    *SERVE_ENABLED
        .get_or_init(|| std::env::var_os(SIM_SERVE_ENGINE_STATE_ENV).is_some_and(|v| v == "1"))
}

/// Test-only gate override (`-1`/unset → use the env cache, `0` → off, `1` →
/// on). Production never calls this (cfg(test)).
#[cfg(test)]
pub fn force_serve_enabled_for_tests(on: Option<bool>) {
    match on {
        Some(true) => TEST_FORCE.store(1, std::sync::atomic::Ordering::Release),
        Some(false) => TEST_FORCE.store(0, std::sync::atomic::Ordering::Release),
        None => TEST_FORCE.store(-1, std::sync::atomic::Ordering::Release),
    }
}

/// If serving is on AND `(address, index)` maps to a tracked pool slot the
/// engine carries authoritatively, return the on-chain-packed engine word
/// (the sim reads THIS instead of the RPC value) + log the served/rpc/delta
/// line. `None` when serving is off, the slot is untracked, or the address
/// is not a registered pool — the caller returns the RPC value unchanged.
///
/// `rpc_value` is the value the fallback just returned (logged as the
/// delta baseline). Pure return-shape: the packed engine word, untracked
/// bits zeroed (per [`BotState::probe_tracked_storage_slot`]). The sim's
/// `swap()` callback reads the served word as the storage slot value.
#[must_use]
pub fn serve_tracked_slot(
    bot_state: &BotState,
    address: Address,
    index: U256,
    rpc_value: U256,
) -> Option<U256> {
    if !serve_enabled() {
        return None;
    }
    let probe = bot_state.probe_tracked_storage_slot(address, index)?;
    let served = U256::from_be_bytes(probe.engine_word.0);
    let delta_xor = served ^ rpc_value;
    tracing::info!(
        pool_addr = %format!("{address:?}"),
        slot = %index,
        kind = ?probe.kind,
        served = %hex_padded_u256(served),
        rpc = %hex_padded_u256(rpc_value),
        delta_xor = %hex_padded_u256(delta_xor),
        update_block = probe.update_block,
        "{BOT_STATE_DB_LOG_PREFIX}"
    );
    Some(served)
}

/// Render a `U256` as a lowercase 64-char hex string (BE, no `0x`).
fn hex_padded_u256(word: U256) -> String {
    use alloy::hex;
    hex::encode(word.to_be_bytes::<32>())
}

#[expect(clippy::unwrap_used, clippy::expect_used)]
#[cfg(test)]
mod tests {
    #![allow(clippy::too_many_lines)]

    use super::*;
    use crate::sim::evm::BotStateDb;
    use alloy::primitives::{address, aliases::U112, Address, U256};
    use degenbot_bot::bot_core::{BotState, RegisterV2PoolParams};
    use revm::database_interface::DatabaseRef;
    use revm::primitives::{StorageKey, StorageValue, B256 as RevmB256};
    use revm::state::AccountInfo;

    /// The V2 pair reserves slot (slot 8) — `uint112 reserve0 | uint112
    /// reserve1 | uint32 blockTimestampLast`.
    const V2_RESERVES_SLOT: u64 = 8;

    const V2_ADDR: Address = address!("b4e16d0168e52d35cacd2c6185b44281ec28c9dc");

    /// A mock `DatabaseRef` serving a FIXED storage value per (address,
    /// slot) — the "RPC fallback" the test controls so it can return a
    /// DIFFERENT reserves value than the engine and prove serving returns
    /// the engine value, not the fallback's.
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

    /// Build a `BotState` with one registered V2 pair at known reserves.
    fn v2_bot_state(reserve0: u128, reserve1: u128, update_block: u64) -> BotState {
        let mut core = BotState::new();
        let params = RegisterV2PoolParams {
            address: V2_ADDR,
            token0: Address::ZERO,
            token1: Address::from([0xa0; 20]),
            reserve0: U112::from(reserve0),
            reserve1: U112::from(reserve1),
            fee_token0: (997, 1000),
            fee_token1: (997, 1000),
            factory: Address::ZERO,
            update_block,
            stable_swap: false,
            fee_denominator: None,
            ..Default::default()
        };
        core.register_v2_pool(&params).expect("V2 registration");
        core
    }

    /// Pack V2 reserves into the on-chain slot-8 word shape (reserve0 low
    /// 112, reserve1 bits 112..224, ts high 32 = 0) — mirrors the engine's
    /// `probe_tracked_storage_slot` packing so the test asserts the EXACT
    /// served word the sim will read.
    fn pack_v2_reserves(reserve0: u128, reserve1: u128) -> U256 {
        let r0 = U256::from(reserve0);
        let r1 = U256::from(reserve1);
        r0 | (r1 << 112u32)
    }

    // The serving tests force a process-global gate; serialize them so
    // parallel test threads don't race the `AtomicI8` force-setter.
    static SERVE_TEST_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn serve_returns_engine_v2_reserves_when_gate_on() {
        let _g = SERVE_TEST_GUARD.lock().unwrap();
        let core = v2_bot_state(1_000_000, 2_000_000, 18_012_345);
        // The fallback returns a DIFFERENT reserves value — simulating a
        // stale/divergent RPC read. Serving must return the engine value,
        // NOT the fallback's.
        let mut db = FixedStorageDb::default();
        let rpc_word = pack_v2_reserves(999_999, 1_999_999);
        db.slots
            .insert((V2_ADDR, U256::from(V2_RESERVES_SLOT)), rpc_word);

        force_serve_enabled_for_tests(Some(true));
        let bot_db = BotStateDb::new(&core, db);
        let got = bot_db
            .storage_ref(V2_ADDR, U256::from(V2_RESERVES_SLOT))
            .expect("storage_ref ok");
        force_serve_enabled_for_tests(None);

        let expected = pack_v2_reserves(1_000_000, 2_000_000);
        assert_eq!(
            got, expected,
            "serving returns the engine's packed reserves, not the RPC fallback's"
        );
        assert_ne!(
            got, rpc_word,
            "the served value must differ from the divergent RPC value"
        );
    }

    #[test]
    fn serve_falls_through_to_rpc_when_gate_off() {
        let _g = SERVE_TEST_GUARD.lock().unwrap();
        let core = v2_bot_state(1_000_000, 2_000_000, 18_012_345);
        // The fallback returns a DIFFERENT value; with serving OFF the sim
        // reads the RPC value (the safe status quo — serving is not enabled).
        let rpc_word = pack_v2_reserves(999_999, 1_999_999);
        let mut db = FixedStorageDb::default();
        db.slots
            .insert((V2_ADDR, U256::from(V2_RESERVES_SLOT)), rpc_word);

        force_serve_enabled_for_tests(Some(false));
        let bot_db = BotStateDb::new(&core, db);
        let got = bot_db
            .storage_ref(V2_ADDR, U256::from(V2_RESERVES_SLOT))
            .expect("storage_ref ok");
        force_serve_enabled_for_tests(None);
        assert_eq!(got, rpc_word, "gate off → RPC fallback value returned");
    }

    #[test]
    fn serve_falls_through_for_untracked_slot() {
        let _g = SERVE_TEST_GUARD.lock().unwrap();
        let core = v2_bot_state(1_000_000, 2_000_000, 18_012_345);
        // Slot 6 (price0CumulativeLast) is NOT tracked by the engine → even
        // with serving on, the RPC value is returned (the sim reads RPC for
        // every slot the engine doesn't carry).
        let rpc_word = U256::from(0xdeadbeefu64);
        let mut db = FixedStorageDb::default();
        db.slots.insert((V2_ADDR, U256::from(6u64)), rpc_word);

        force_serve_enabled_for_tests(Some(true));
        let bot_db = BotStateDb::new(&core, db);
        let got = bot_db
            .storage_ref(V2_ADDR, U256::from(6u64))
            .expect("storage_ref ok");
        force_serve_enabled_for_tests(None);
        assert_eq!(
            got, rpc_word,
            "untracked slot → RPC fallback (no engine word to serve)"
        );
    }

    #[test]
    fn serve_falls_through_for_unregistered_address() {
        let _g = SERVE_TEST_GUARD.lock().unwrap();
        let core = v2_bot_state(1_000_000, 2_000_000, 18_012_345);
        let unknown = address!("1111111111111111111111111111111111111111");
        let rpc_word = U256::from(0xcafeu64);
        let mut db = FixedStorageDb::default();
        db.slots.insert((unknown, U256::ZERO), rpc_word);

        force_serve_enabled_for_tests(Some(true));
        let bot_db = BotStateDb::new(&core, db);
        let got = bot_db
            .storage_ref(unknown, U256::ZERO)
            .expect("storage_ref ok");
        force_serve_enabled_for_tests(None);
        assert_eq!(
            got, rpc_word,
            "non-pool contract → RPC fallback (no engine state to serve)"
        );
    }

    #[test]
    fn serve_v2_reserves_zeroes_timestamp_bits() {
        let _g = SERVE_TEST_GUARD.lock().unwrap();
        // The engine does NOT track blockTimestampLast (high 32 bits). The
        // served word MUST have those bits zeroed (the sim's swap callback
        // does not read the timestamp; a stale/racy ts served from the engine
        // would be noise). Verifies the probe's zeroing + the mask the find
        // doc relies on.
        let core = v2_bot_state(1_000_000, 2_000_000, 18_012_345);
        // The RPC reserves word carries a NONZERO timestamp in the high 32.
        let rpc_with_ts =
            pack_v2_reserves(999_999, 1_999_999) | (U256::from(0x6543_2101u32) << 224u32);
        let mut db = FixedStorageDb::default();
        db.slots
            .insert((V2_ADDR, U256::from(V2_RESERVES_SLOT)), rpc_with_ts);

        force_serve_enabled_for_tests(Some(true));
        let bot_db = BotStateDb::new(&core, db);
        let got = bot_db
            .storage_ref(V2_ADDR, U256::from(V2_RESERVES_SLOT))
            .expect("storage_ref ok");
        force_serve_enabled_for_tests(None);
        assert_eq!(
            got >> 224u32,
            U256::ZERO,
            "served word's timestamp bits must be zeroed (engine doesn't track ts)"
        );
        assert_eq!(
            got & U256::from_limbs([u64::MAX, u64::MAX, u64::MAX, 0x0000_0000_ffff_ffff]),
            pack_v2_reserves(1_000_000, 2_000_000),
            "served reserves (low 224) match the engine"
        );
    }
}
