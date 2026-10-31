//! Path-derived storage-slot prefetch into the per-anchor [`StorageMemo`].
//!
//! The steady-state cold cost is a *serial* chain: each storage slot a
//! settlement frame touches is a separate `eth_getStorageAt` round trip, and the
//! frame cannot proceed to the next slot until the previous answer arrives
//! (`cold_miss_fanout_probe`, legs 1/7/8).
//!
//! But a candidate's PATH names its pools before the simulation starts
//! (`PathInfo` → pool addresses + family, token identities, the V4 pool id), and
//! the executor's fixed slots come from the batch's own context. So the subset
//! of slots the frame is *guaranteed* to read is computable upfront — and one
//! JSON-RPC batch
//! ([`AlloyProvider::get_storage_batch_at`]) reads all of it in ONE round trip
//! instead of one per slot. Seeded into the anchor's memo before the fan-out
//! runs, the frame's dependency chain finds the entries memoized.
//!
//! # Where the values come from (the node, never the engine)
//!
//! Every prefetch read pins the SAME anchor the RPC fallback pins
//! (`dispatch`'s `sim_block`), and seeds the memo the fallback itself feeds. A
//! seeded entry is therefore indistinguishable from one the fallback learned:
//! the node's full word at the anchor, no engine-projection masking. That is
//! the whole reason this lane reads the node — an engine-carried word has its
//! untracked bits zeroed, and serving it reverts.
//!
//! # Coverage, and what it deliberately does not cover
//!
//! Derived from the path ([`FamilySlotLayout::PRODUCTION`] + [`PrefetchFacts`]):
//!
//! | Family | Slots | Why derivable |
//! |---|---|---|
//! | V2 hop | `token0`, `token1`, `reserves` | fixed layout per pool address |
//! | V3 hop | `slot0`, `liquidity` | fixed layout per pool address |
//! | V4 hop | `S_state+0` (`slot0`), `S_state+3` (`liquidity`) | `S_state = keccak256(poolId, 6)`, pool id in the path |
//! | V2/V3 hop holding WETH | that pool's WETH `balanceOf` | WETH9's mapping base is a constant |
//! | executor | WETH `balanceOf(executor)`, PoolManager ERC6909 balances | fixed slots + addresses from the batch context |
//!
//! NOT covered, by design: ERC20 `balanceOf` slots of arbitrary path tokens
//! (the mapping base is the token's own layout, not the path's — so a pool's
//! non-WETH balance and the executor's non-WETH profit balance stay
//! underivable), and per-tick V3/V4 words (unbounded). Those still forward, one
//! round trip each — that is the residual serial chain this lane leaves.
//!
//! # The degrade contract
//!
//! A prefetch failure — no ambient runtime (the same precondition the blocking
//! read adapter needs) or an RPC error — NEVER fails the fan-out: the lane
//! returns a report with the reason and leaves the memo untouched, so the frame
//! runs the un-prefetched path exactly as it did before this lane existed.
//! A memo that already holds every derived slot dispatches nothing.
//!
//! # Staleness
//!
//! Entries are anchor-scoped by construction: the caller seeds the memo
//! [`StorageMemoCache::for_anchor`](super::StorageMemoCache::for_anchor) handed
//! it and reads pin the same anchor. A fan-out at the next anchor gets a fresh
//! memo (the cache retires the old one), so a prefetch at N can never serve
//! N+1.

use std::future::Future;

use alloy::primitives::{Address, B256, U256};
use degenbot_core::diag;
use degenbot_executor::composers::{HopInfo, PathInfo};
use degenbot_executor::{mapping_slot, WarmupSlots, WETH9_BALANCE_OF_SLOT};
use degenbot_pools::{
    v4_liquidity_slot, v4_pool_state_base_slot, v4_slot0_slot, V2_RESERVES_SLOT, V2_TOKEN0_SLOT,
    V2_TOKEN1_SLOT, V3_LIQUIDITY_SLOT, V3_SLOT0_SLOT,
};
use degenbot_rpc::provider::AlloyProvider;

use super::storage_memo::StorageMemo;

/// The most slots one prefetch batch may carry. A batch is ONE round trip, so
/// this is a transport-payload bound, not a latency bound: the widest fan-out
/// (`MAX_SIMULATE_CONCURRENT` candidates × a few hops × three slots) sits well
/// under it, and a path set beyond it simply leaves the tail to the fallback.
pub const MAX_PREFETCH_SLOTS: usize = 512;

/// A V2 (or V2-fork) pair's storage indices, as the pair contract declares
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V2SlotIndices {
    /// `address public token0`.
    pub token0: u64,
    /// `address public token1`.
    pub token1: u64,
    /// The packed `reserve0 | reserve1 | blockTimestampLast` word.
    pub reserves: u64,
}

/// A V3 (canonical Uniswap layout) pool's storage indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct V3SlotIndices {
    /// The packed `slot0` word (`sqrtPriceX96 | tick | ...`).
    pub slot0: u64,
    /// The active `liquidity` word.
    pub liquidity: u64,
}

/// The per-family slot indices a prefetch derives from a path.
///
/// Fork pools differ (a PancakeSwap V3 pool carries `liquidity` at 5, not 4),
/// so the table is explicitly the caller's statement of which layout its pools
/// actually use. [`FamilySlotLayout::PRODUCTION`] is the canonical
/// Uniswap-family mainnet table: V2 `token0`/`token1`/`reserves` at 6/7/8
/// (verified against a live canonical pair — `factory` at 5, then the two token
/// identities, then the packed reserves word), V3 `slot0`/`liquidity` at 0/4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FamilySlotLayout {
    /// The V2-family indices.
    pub v2: V2SlotIndices,
    /// The V3-family indices.
    pub v3: V3SlotIndices,
}

impl FamilySlotLayout {
    /// The canonical Uniswap-family mainnet layout (see the type docs).
    pub const PRODUCTION: Self = Self {
        v2: V2SlotIndices {
            token0: V2_TOKEN0_SLOT,
            token1: V2_TOKEN1_SLOT,
            reserves: V2_RESERVES_SLOT,
        },
        v3: V3SlotIndices {
            slot0: V3_SLOT0_SLOT,
            liquidity: V3_LIQUIDITY_SLOT,
        },
    };
}

impl Default for FamilySlotLayout {
    fn default() -> Self {
        Self::PRODUCTION
    }
}

/// The non-path facts a prefetch needs: the executor-side fixed slots every
/// `execute()` touches, plus the WETH9 address whose `balanceOf` mapping base
/// is a known constant (so a pool's own WETH balance is derivable too).
///
/// The addresses say WHERE each warmup slot lives (the slot values alone do
/// not carry their account).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrefetchFacts {
    /// WETH9's address (the executor's `balanceOf` slot lives here).
    pub weth_address: Address,
    /// The PoolManager singleton's address (the ERC6909 balance slots live
    /// here). `None` when the caller's session has no PoolManager to name (a
    /// lab fixture without one) — those two slots are then simply not derived.
    pub pool_manager_address: Option<Address>,
    /// The session's computed warmup slot addresses (the same set the state
    /// overrides install).
    pub warmup: WarmupSlots,
}

impl PrefetchFacts {
    /// The executor-side `(address, slot)` pairs: the WETH `balanceOf(executor)`
    /// word, plus the PoolManager ERC6909 balance words when a PoolManager is
    /// known.
    #[must_use]
    pub fn slots(&self) -> Vec<(Address, U256)> {
        let mut slots = vec![(self.weth_address, self.warmup.weth_balance)];
        if let Some(pool_manager) = self.pool_manager_address {
            slots.push((pool_manager, self.warmup.erc6909_weth));
            slots.push((pool_manager, self.warmup.erc6909_native));
        }
        slots
    }

    /// The WETH9 `balanceOf(holder)` slot — `keccak256(holder ‖ balanceOf)`.
    ///
    /// A V2/V3 pool measures its own token balances on every swap (the V2
    /// K-check, the V3 pre-swap balance), so a pool that holds WETH reads this
    /// word; WETH9's mapping base is a constant, which is what makes it
    /// derivable where an arbitrary ERC20's is not.
    #[must_use]
    pub fn weth_balance_slot(&self, holder: Address) -> U256 {
        mapping_slot(
            U256::from(WETH9_BALANCE_OF_SLOT),
            U256::from_be_bytes(holder.into_word().0),
        )
    }
}

/// Why a prefetch did not seed — the degrade paths, surfaced rather than
/// swallowed so an operator can tell a cold lane from a broken one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrefetchSkip {
    /// No ambient multi-threaded tokio runtime: the same precondition the
    /// blocking read adapter needs, so the fan-out cannot read at all here.
    NoRuntime,
    /// The batch read failed; the memo keeps whatever it already held and the
    /// frame forwards slot by slot.
    RpcFailed,
}

/// What one prefetch did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrefetchReport {
    /// The distinct `(address, slot)` pairs the path derivation produced.
    pub derived: usize,
    /// The subset the memo did not already hold (the batch's member count).
    pub requested: usize,
    /// The entries actually seeded.
    pub seeded: usize,
    /// The degrade reason, if the lane backed off.
    pub skipped: Option<PrefetchSkip>,
}

impl PrefetchReport {
    /// Whether the lane backed off (the fan-out runs the un-prefetched path).
    #[must_use]
    pub fn deferred(&self) -> bool {
        self.skipped.is_some()
    }
}

/// The distinct `(address, slot)` pairs a prefetch can compute from the paths
/// BEFORE the simulation runs, in first-touch order (deterministic for a given
/// batch).
///
/// One entry per pool-state word the frame is guaranteed to read, plus the
/// executor's fixed slots. Duplicates (two candidates sharing a pool) appear
/// once.
#[must_use]
pub fn path_prefetch_slots<'a>(
    paths: impl IntoIterator<Item = &'a PathInfo>,
    layout: &FamilySlotLayout,
    facts: &PrefetchFacts,
) -> Vec<(Address, U256)> {
    let mut slots: Vec<(Address, U256)> = Vec::new();
    let mut seen: std::collections::HashSet<(Address, U256)> = std::collections::HashSet::new();
    for path in paths {
        for hop in &path.hops {
            match hop {
                HopInfo::V2(v2) => {
                    for index in [layout.v2.token0, layout.v2.token1, layout.v2.reserves] {
                        let entry = (v2.pool_address, U256::from(index));
                        if slots.len() < MAX_PREFETCH_SLOTS && seen.insert(entry) {
                            slots.push(entry);
                        }
                    }
                    if holds_weth(&[v2.token0_address, v2.token1_address], facts) {
                        let entry = (facts.weth_address, facts.weth_balance_slot(v2.pool_address));
                        if slots.len() < MAX_PREFETCH_SLOTS && seen.insert(entry) {
                            slots.push(entry);
                        }
                    }
                }
                HopInfo::V3(v3) => {
                    for index in [layout.v3.slot0, layout.v3.liquidity] {
                        let entry = (v3.pool_address, U256::from(index));
                        if slots.len() < MAX_PREFETCH_SLOTS && seen.insert(entry) {
                            slots.push(entry);
                        }
                    }
                    if holds_weth(&[v3.token0_address, v3.token1_address], facts) {
                        let entry = (facts.weth_address, facts.weth_balance_slot(v3.pool_address));
                        if slots.len() < MAX_PREFETCH_SLOTS && seen.insert(entry) {
                            slots.push(entry);
                        }
                    }
                }
                HopInfo::V4(v4) => {
                    // The V4 pool's state lives in the PoolManager singleton at
                    // `S_state = keccak256(abi.encode(poolId, _pools[6]))`; the
                    // path carries the pool id.
                    let Some(pool_id) = parse_pool_id(&v4.pool_id_hex) else {
                        continue;
                    };
                    let base = v4_pool_state_base_slot(pool_id);
                    for slot in [v4_slot0_slot(base), v4_liquidity_slot(base)] {
                        let entry = (v4.pool_manager_address, slot);
                        if slots.len() < MAX_PREFETCH_SLOTS && seen.insert(entry) {
                            slots.push(entry);
                        }
                    }
                }
            }
        }
    }
    for entry in facts.slots() {
        if slots.len() < MAX_PREFETCH_SLOTS && seen.insert(entry) {
            slots.push(entry);
        }
    }
    slots
}

/// Whether a pool's token pair includes WETH — the precondition for the pool's
/// own WETH `balanceOf` word being derivable (see
/// [`PrefetchFacts::weth_balance_slot`]).
fn holds_weth(tokens: &[Address], facts: &PrefetchFacts) -> bool {
    tokens.contains(&facts.weth_address)
}

/// The 32-byte V4 pool id behind a path hop's `0x`-prefixed hex, if it is
/// well-formed. A malformed id derives no slots (the frame's own read would
/// fail long before a prefetch could help).
fn parse_pool_id(pool_id_hex: &str) -> Option<B256> {
    let bytes = alloy::hex::decode(pool_id_hex.trim_start_matches("0x")).ok()?;
    let array: [u8; 32] = bytes.try_into().ok()?;
    Some(B256::from(array))
}

/// Derive the batch's slot set and seed the anchor's memo from ONE
/// [`AlloyProvider::get_storage_batch_at`] round trip.
///
/// `anchor` must be the block the fan-out's RPC fallback pins (the memo's
/// validity domain), and `memo` the
/// [`StorageMemoCache::for_anchor`](super::StorageMemoCache::for_anchor) handle
/// for that same anchor.
///
/// Entries the memo already holds are skipped, so a second fan-out at a warm
/// anchor dispatches nothing. Failure degrades: see [`PrefetchSkip`].
pub fn prefetch_paths_into_memo<'a>(
    provider: &AlloyProvider,
    anchor: u64,
    paths: impl IntoIterator<Item = &'a PathInfo>,
    layout: &FamilySlotLayout,
    facts: &PrefetchFacts,
    memo: &StorageMemo,
) -> PrefetchReport {
    let slots = path_prefetch_slots(paths, layout, facts);
    prefetch_into_memo(provider, anchor, &slots, memo)
}

/// Seed `memo` with the node's values for `slots` at `anchor`, in ONE JSON-RPC
/// batch round trip.
///
/// The prefetch's core: dedup + drop what the memo already holds, read the rest
/// as one batch, seed every answer. Never fails — a skipped or failed read
/// returns a [`PrefetchReport`] carrying the reason, with the memo untouched
/// beyond whatever it already held.
pub fn prefetch_into_memo(
    provider: &AlloyProvider,
    anchor: u64,
    slots: &[(Address, U256)],
    memo: &StorageMemo,
) -> PrefetchReport {
    let mut wanted: Vec<(Address, U256)> = Vec::with_capacity(slots.len());
    let mut seen: std::collections::HashSet<(Address, U256)> = std::collections::HashSet::new();
    for &entry in slots {
        if !seen.insert(entry) {
            continue;
        }
        if memo.holds(entry.0, entry.1) {
            continue;
        }
        wanted.push(entry);
    }
    let derived = seen.len();
    if wanted.is_empty() {
        // A warm memo (every derived entry already known) dispatches nothing.
        return PrefetchReport {
            derived,
            requested: 0,
            seeded: 0,
            skipped: None,
        };
    }
    let Some(read) = block_on_ambient(provider.get_storage_batch_at(&wanted, anchor)) else {
        return PrefetchReport {
            derived,
            requested: wanted.len(),
            seeded: 0,
            skipped: Some(PrefetchSkip::NoRuntime),
        };
    };
    match read {
        Ok(values) => {
            let mut seeded = 0usize;
            for ((address, slot), value) in wanted.iter().zip(values) {
                memo.seed(*address, *slot, value);
                seeded += 1;
            }
            PrefetchReport {
                derived,
                requested: wanted.len(),
                seeded,
                skipped: None,
            }
        }
        Err(error) => {
            diag!(
                domain = sim,
                anchor,
                slots = wanted.len(),
                "storage prefetch degraded: {error}"
            );
            PrefetchReport {
                derived,
                requested: wanted.len(),
                seeded: 0,
                skipped: Some(PrefetchSkip::RpcFailed),
            }
        }
    }
}

/// Drive `future` on the ambient runtime the blocking read adapter uses, or
/// answer `None` when this thread has none (a current-thread runtime or no
/// runtime context at all).
///
/// The same precondition as
/// [`BlockingAsyncDb::new`](super::evm2::blocking_async_db::BlockingAsyncDb::new):
/// where the fan-out can read at all, the prefetch can too.
fn block_on_ambient<T>(future: impl Future<Output = T>) -> Option<T> {
    let handle = tokio::runtime::Handle::try_current().ok()?;
    if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::CurrentThread {
        return None;
    }
    Some(tokio::task::block_in_place(|| handle.block_on(future)))
}

#[cfg(test)]
#[expect(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use alloy::providers::RootProvider;
    use alloy::rpc::client::ClientBuilder;
    use alloy::rpc::json_rpc::{Response, ResponsePacket, ResponsePayload, SerializedRequest};
    use alloy::transports::{TransportError, TransportErrorKind, TransportFut};
    use degenbot_executor::composers::{V2HopInfo, V3HopInfo, V4HopInfo};
    use degenbot_executor::{compute_simulation_warmup_slots, mapping_slot};
    use degenbot_substrate::NO_SIM_ANCHOR;
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    use crate::sim::evm2::bot_state_db::BotStateDb;
    use crate::sim::StorageMemoCache;

    use ::evm2::bytecode::Bytecode;
    use ::evm2::evm::{AccountInfo, Database};
    use ::evm2::interpreter::Word;

    const EXECUTOR: Address = Address::new([0xaa; 20]);
    const WETH: Address = Address::new([0xbb; 20]);
    const POOL_MANAGER: Address = Address::new([0xcc; 20]);
    const TOKEN: Address = Address::new([0xdd; 20]);
    const POOL_1: Address = Address::new([0x01; 20]);
    const POOL_2: Address = Address::new([0x02; 20]);
    const POOL_3: Address = Address::new([0x03; 20]);

    /// The corpus "chain": a deterministic value per slot. The stub transport
    /// and the forwarding fallback both serve it, so "the prefetched value" and
    /// "the value the fallback would have served" are the same rule — the
    /// identity this lane must preserve.
    fn chain_value(slot: U256) -> U256 {
        slot * U256::from(7u64) + U256::from(3u64)
    }

    fn executor_facts() -> PrefetchFacts {
        PrefetchFacts {
            weth_address: WETH,
            pool_manager_address: Some(POOL_MANAGER),
            warmup: compute_simulation_warmup_slots(EXECUTOR, WETH),
        }
    }

    fn v2_hop(pool: Address) -> HopInfo {
        HopInfo::V2(V2HopInfo {
            pool_address: pool,
            token0_address: WETH,
            token1_address: TOKEN,
            fee: 30,
            zfo: true,
        })
    }

    fn v3_hop(pool: Address) -> HopInfo {
        HopInfo::V3(V3HopInfo {
            pool_address: pool,
            token0_address: WETH,
            token1_address: TOKEN,
            fee: 500,
            zfo: false,
        })
    }

    fn v4_hop(pool_id: B256) -> HopInfo {
        HopInfo::V4(V4HopInfo {
            pool_manager_address: POOL_MANAGER,
            pool_id_hex: format!("{pool_id}"),
            currency0_address: WETH,
            currency1_address: TOKEN,
            fee: 500,
            tick_spacing: 10,
            hook_address: Address::ZERO,
            zfo: true,
        })
    }

    fn path(hops: Vec<HopInfo>) -> PathInfo {
        PathInfo::new(hops)
    }

    // ── the derivation ───────────────────────────────────────────────────

    /// A two-hop V2 path derives its two pairs' identity + reserves words (in
    /// first-touch hop order), each WETH-holding pair's own WETH balance word,
    /// then the executor's fixed slots.
    #[test]
    fn a_v2_path_derives_identity_and_reserves_words() {
        let facts = executor_facts();
        let paths = [path(vec![v2_hop(POOL_1), v2_hop(POOL_2)])];
        let slots = path_prefetch_slots(&paths, &FamilySlotLayout::PRODUCTION, &facts);

        let expected_pool: Vec<(Address, U256)> = [POOL_1, POOL_2]
            .into_iter()
            .flat_map(|pool| {
                [
                    (pool, U256::from(6u64)),
                    (pool, U256::from(7u64)),
                    (pool, U256::from(8u64)),
                    (WETH, facts.weth_balance_slot(pool)),
                ]
            })
            .collect();
        let mut expected = expected_pool;
        expected.extend(facts.slots());
        assert_eq!(slots, expected, "V2 hops derive token0/token1/reserves");
    }

    /// A pool whose pair does NOT include WETH reads its tokens' balances
    /// through mapping bases the path does not know — so it derives no balance
    /// word, only its own state words.
    #[test]
    fn a_non_weth_pool_derives_no_balance_word() {
        let facts = executor_facts();
        let hop = HopInfo::V2(V2HopInfo {
            pool_address: POOL_2,
            token0_address: TOKEN,
            token1_address: Address::new([0xee; 20]),
            fee: 30,
            zfo: true,
        });
        let paths = [path(vec![hop])];
        let slots = path_prefetch_slots(&paths, &FamilySlotLayout::PRODUCTION, &facts);
        assert_eq!(
            &slots[..3],
            &[
                (POOL_2, U256::from(6u64)),
                (POOL_2, U256::from(7u64)),
                (POOL_2, U256::from(8u64)),
            ]
        );
        assert!(
            !slots.contains(&(WETH, facts.weth_balance_slot(POOL_2))),
            "the pool's own balance word is not derivable for a non-WETH pair"
        );
    }

    /// A WETH-holding pool's balance word is `keccak256(pool ‖ 3)` — the WETH9
    /// `balanceOf` layout, pinned against the executor's independent
    /// `mapping_slot` helper.
    #[test]
    fn a_weth_pool_balance_word_is_the_weth9_balance_of_slot() {
        let facts = executor_facts();
        let paths = [path(vec![v2_hop(POOL_1)])];
        let slots = path_prefetch_slots(&paths, &FamilySlotLayout::PRODUCTION, &facts);
        let expected = mapping_slot(U256::from(3u64), U256::from_be_bytes(POOL_1.into_word().0));
        assert!(
            slots.contains(&(WETH, expected)),
            "the pool's WETH balanceOf slot is derived"
        );
    }

    /// A V3 hop derives exactly `slot0` and `liquidity` — the two words a
    /// swap's price/liquidity read touches.
    #[test]
    fn a_v3_path_derives_slot0_and_liquidity() {
        let paths = [path(vec![v3_hop(POOL_3)])];
        let slots = path_prefetch_slots(&paths, &FamilySlotLayout::PRODUCTION, &executor_facts());
        assert_eq!(
            &slots[..2],
            &[(POOL_3, U256::from(0u64)), (POOL_3, U256::from(4u64))]
        );
    }

    /// A V4 hop's state lives in the PoolManager at
    /// `S_state = keccak256(abi.encode(poolId, 6))`; the derived slots are that
    /// base's `slot0` (+0) and `liquidity` (+3). The base is recomputed here
    /// through the executor's independent `mapping_slot` helper.
    #[test]
    fn a_v4_path_derives_the_pool_state_words() {
        let pool_id = B256::new([0x5a; 32]);
        let paths = [path(vec![v4_hop(pool_id)])];
        let slots = path_prefetch_slots(&paths, &FamilySlotLayout::PRODUCTION, &executor_facts());

        let base = mapping_slot(U256::from(6u64), U256::from_be_bytes(pool_id.0));
        assert_eq!(
            &slots[..2],
            &[
                (POOL_MANAGER, base),
                (POOL_MANAGER, base + U256::from(3u64))
            ]
        );
    }

    /// Two candidates sharing a pool derive that pool's words once — the batch
    /// carries one member per distinct `(address, slot)`.
    #[test]
    fn a_shared_pool_is_derived_once() {
        let paths = [
            path(vec![v2_hop(POOL_1), v2_hop(POOL_2)]),
            path(vec![v2_hop(POOL_2), v2_hop(POOL_1)]),
        ];
        let slots = path_prefetch_slots(&paths, &FamilySlotLayout::PRODUCTION, &executor_facts());
        let distinct: std::collections::HashSet<_> = slots.iter().copied().collect();
        assert_eq!(slots.len(), distinct.len(), "no duplicate batch members");
        assert_eq!(
            slots.len(),
            6 + 2 + 3,
            "two pools x three words + their two WETH balances + the executor set"
        );
    }

    /// A caller's layout override is honored (fork pools do not share the
    /// canonical indices).
    #[test]
    fn a_layout_override_is_honored() {
        let harness = FamilySlotLayout {
            v2: V2SlotIndices {
                token0: 0,
                token1: 1,
                reserves: 2,
            },
            ..FamilySlotLayout::PRODUCTION
        };
        let paths = [path(vec![v2_hop(POOL_1)])];
        let slots = path_prefetch_slots(&paths, &harness, &executor_facts());
        assert_eq!(
            &slots[..3],
            &[
                (POOL_1, U256::from(0u64)),
                (POOL_1, U256::from(1u64)),
                (POOL_1, U256::from(2u64)),
            ]
        );
    }

    /// A malformed V4 pool id derives nothing rather than a bogus slot.
    #[test]
    fn a_malformed_pool_id_derives_nothing() {
        let hop = HopInfo::V4(V4HopInfo {
            pool_manager_address: POOL_MANAGER,
            pool_id_hex: "0xnothex".to_string(),
            currency0_address: WETH,
            currency1_address: TOKEN,
            fee: 500,
            tick_spacing: 10,
            hook_address: Address::ZERO,
            zfo: true,
        });
        let paths = [path(vec![hop])];
        let slots = path_prefetch_slots(&paths, &FamilySlotLayout::PRODUCTION, &executor_facts());
        assert_eq!(slots, executor_facts().slots());
    }

    // ── the lane ─────────────────────────────────────────────────────────

    /// A storage-serving transport: answers `chain_value(slot)` for every
    /// `eth_getStorageAt`, and records each dispatched packet's member count.
    #[derive(Debug, Clone)]
    struct ChainStub {
        packets: Arc<Mutex<Vec<usize>>>,
        fail: bool,
    }

    impl ChainStub {
        fn new() -> Self {
            Self {
                packets: Arc::new(Mutex::new(Vec::new())),
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                fail: true,
                ..Self::new()
            }
        }

        fn packets(&self) -> Vec<usize> {
            self.packets.lock().unwrap().clone()
        }

        fn provider(&self) -> AlloyProvider {
            let client = ClientBuilder::default().transport(self.clone(), true);
            let root: RootProvider = RootProvider::new(client);
            AlloyProvider::from_provider(Arc::new(root))
        }
    }

    fn answer(req: &SerializedRequest) -> Response {
        assert_eq!(req.method(), "eth_getStorageAt");
        let params: serde_json::Value =
            serde_json::from_str(req.params().expect("params").get()).expect("params JSON");
        let slot = U256::from_str_radix(
            params[1]
                .as_str()
                .expect("the member's second param is the slot")
                .trim_start_matches("0x"),
            16,
        )
        .expect("a hex quantity");
        let result = serde_json::Value::String(format!("0x{:064x}", chain_value(slot)));
        Response {
            id: req.id().clone(),
            payload: ResponsePayload::Success(
                serde_json::value::to_raw_value(&result).expect("a JSON string"),
            ),
        }
    }

    impl tower::Service<alloy::rpc::json_rpc::RequestPacket> for ChainStub {
        type Response = ResponsePacket;
        type Error = TransportError;
        type Future = TransportFut<'static>;

        fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, req: alloy::rpc::json_rpc::RequestPacket) -> Self::Future {
            let stub = self.clone();
            Box::pin(async move {
                let (size, respond) = match req {
                    alloy::rpc::json_rpc::RequestPacket::Single(one) => {
                        (1, ResponsePacket::Single(answer(&one)))
                    }
                    alloy::rpc::json_rpc::RequestPacket::Batch(many) => (
                        many.len(),
                        ResponsePacket::Batch(many.iter().map(answer).collect()),
                    ),
                };
                stub.packets.lock().unwrap().push(size);
                if stub.fail {
                    return Err(TransportErrorKind::custom_str("stub transport down"));
                }
                Ok(respond)
            })
        }
    }

    /// The forwarding cold path the memo sits in front of: the same chain rule
    /// as the stub, counted.
    struct ChainDb {
        forwards: Arc<AtomicU64>,
    }

    impl Database for ChainDb {
        type Error = Infallible;

        fn get_account(&mut self, _address: &Address) -> Result<Option<AccountInfo>, Self::Error> {
            Ok(Some(AccountInfo::default()))
        }

        fn get_code_by_hash(&mut self, _code_hash: &B256) -> Result<Bytecode, Self::Error> {
            Ok(Bytecode::default())
        }

        fn get_storage(&mut self, _address: &Address, key: &Word) -> Result<Word, Self::Error> {
            self.forwards.fetch_add(1, Ordering::Relaxed);
            Ok(chain_value(*key))
        }

        fn get_block_hash(&mut self, _number: &Word) -> Result<B256, Self::Error> {
            Ok(B256::ZERO)
        }
    }

    /// Read every derived slot through the production memo seam
    /// (`BotStateDb::get_storage`), counting the fallback forwards.
    fn read_through_memo(
        slots: &[(Address, U256)],
        memo: &std::sync::Arc<StorageMemo>,
    ) -> (Vec<Word>, u64) {
        let forwards = Arc::new(AtomicU64::new(0));
        let mut db = BotStateDb::new(
            &NO_SIM_ANCHOR,
            ChainDb {
                forwards: Arc::clone(&forwards),
            },
        )
        .with_storage_memo(std::sync::Arc::clone(memo));
        let values = slots
            .iter()
            .map(|(address, slot)| db.get_storage(address, slot).expect("read"))
            .collect();
        (values, forwards.load(Ordering::Relaxed))
    }

    /// The lane's contract in one test: one batch round trip seeds the anchor's
    /// memo, every derived read finds its entry, and the served values are
    /// identical to the un-prefetched path's (verdict identity at the value
    /// seam: prefetch ≡ no-prefetch).
    #[tokio::test(flavor = "multi_thread")]
    async fn a_prefetch_seeds_the_anchor_memo_from_one_batch_round_trip() {
        let stub = ChainStub::new();
        let provider = stub.provider();
        let cache = StorageMemoCache::new();
        let anchor = 100u64;
        let memo = cache.for_anchor(anchor);
        let paths = [path(vec![v2_hop(POOL_1), v3_hop(POOL_3)])];
        let slots = path_prefetch_slots(&paths, &FamilySlotLayout::PRODUCTION, &executor_facts());

        let report = prefetch_paths_into_memo(
            &provider,
            anchor,
            &paths,
            &FamilySlotLayout::PRODUCTION,
            &executor_facts(),
            &memo,
        );
        assert_eq!(
            report,
            PrefetchReport {
                derived: slots.len(),
                requested: slots.len(),
                seeded: slots.len(),
                skipped: None,
            }
        );
        assert!(!report.deferred());
        assert_eq!(
            stub.packets(),
            vec![slots.len()],
            "one dispatched batch packet carrying every derived slot"
        );
        for (address, slot) in &slots {
            assert!(
                memo.holds(*address, *slot),
                "the seed installs every derived entry"
            );
        }

        // The un-prefetched run: same reads, one forward each.
        let cold = StorageMemoCache::new();
        let (forwarded_values, forwards) = read_through_memo(&slots, &cold.for_anchor(anchor));
        assert_eq!(
            usize::try_from(forwards).unwrap(),
            slots.len(),
            "the cold run forwards"
        );
        let (prefetched_values, prefetched_forwards) = read_through_memo(&slots, &memo);
        assert_eq!(
            prefetched_forwards, 0,
            "every prefetched read is served by the memo"
        );
        assert_eq!(
            prefetched_values, forwarded_values,
            "prefetch ≡ no-prefetch: the served values are the chain's own"
        );
        assert_eq!(cache.anchor(), Some(anchor));
    }

    /// A warm memo dispatches no second batch: the same anchor's later fan-out
    /// pays nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_warm_memo_dispatches_nothing() {
        let stub = ChainStub::new();
        let provider = stub.provider();
        let cache = StorageMemoCache::new();
        let paths = [path(vec![v2_hop(POOL_1)])];
        let facts = executor_facts();
        let first = prefetch_paths_into_memo(
            &provider,
            7,
            &paths,
            &FamilySlotLayout::PRODUCTION,
            &facts,
            &cache.for_anchor(7),
        );
        assert_eq!(first.seeded, first.derived);
        let second = prefetch_paths_into_memo(
            &provider,
            7,
            &paths,
            &FamilySlotLayout::PRODUCTION,
            &facts,
            &cache.for_anchor(7),
        );
        assert_eq!(
            second,
            PrefetchReport {
                derived: first.derived,
                requested: 0,
                seeded: 0,
                skipped: None,
            },
            "the warm memo's derived entries are all known"
        );
        assert_eq!(stub.packets().len(), 1, "exactly one batch over two calls");
    }

    /// A failed batch degrades the lane: nothing is seeded, the report carries
    /// the reason, and the frame then forwards every slot exactly as the
    /// un-prefetched path does.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_prefetch_degrades_to_the_forwarding_path() {
        let stub = ChainStub::failing();
        let provider = stub.provider();
        let memo = std::sync::Arc::new(StorageMemo::new());
        let paths = [path(vec![v2_hop(POOL_1)])];
        let facts = executor_facts();

        let report = prefetch_paths_into_memo(
            &provider,
            7,
            &paths,
            &FamilySlotLayout::PRODUCTION,
            &facts,
            &memo,
        );
        assert_eq!(report.skipped, Some(PrefetchSkip::RpcFailed));
        assert!(report.deferred());
        assert_eq!(report.seeded, 0);
        assert_eq!(memo.stats(), (0, 0), "a failed prefetch seeds nothing");

        let slots = path_prefetch_slots(&paths, &FamilySlotLayout::PRODUCTION, &facts);
        let (values, forwards) = read_through_memo(&slots, &memo);
        assert_eq!(
            usize::try_from(forwards).unwrap(),
            slots.len(),
            "the un-prefetched path forwards every slot"
        );
        let expected: Vec<Word> = slots.iter().map(|(_, slot)| chain_value(*slot)).collect();
        assert_eq!(values, expected, "the frame settles on the chain's values");
    }

    /// Without the ambient runtime the blocking adapter needs, the lane backs
    /// off instead of panicking — the fan-out is unchanged.
    #[test]
    fn no_ambient_runtime_skips_the_lane() {
        let stub = ChainStub::new();
        let provider = stub.provider();
        let memo = StorageMemo::new();
        let report = prefetch_into_memo(&provider, 7, &[(POOL_1, U256::from(8u64))], &memo);
        assert_eq!(report.skipped, Some(PrefetchSkip::NoRuntime));
        assert_eq!(report.seeded, 0);
        assert_eq!(memo.stats(), (0, 0));
        assert!(stub.packets().is_empty(), "no runtime, no dispatch");
    }

    /// A prefetch at anchor N never serves a fan-out at N+1: the cache retires
    /// the memo with its values, so the moved anchor forwards cold and reads the
    /// new anchor's own answers.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_prefetch_at_one_anchor_never_serves_the_next() {
        let stub = ChainStub::new();
        let provider = stub.provider();
        let cache = StorageMemoCache::new();
        let paths = [path(vec![v2_hop(POOL_1), v2_hop(POOL_2)])];
        let facts = executor_facts();
        let slots = path_prefetch_slots(&paths, &FamilySlotLayout::PRODUCTION, &facts);

        let seeded = cache.for_anchor(100);
        let report = prefetch_paths_into_memo(
            &provider,
            100,
            &paths,
            &FamilySlotLayout::PRODUCTION,
            &facts,
            &seeded,
        );
        assert_eq!(report.seeded, slots.len());

        // The anchor moves: the successor memo starts empty — no carried value.
        let moved = cache.for_anchor(101);
        assert!(
            !std::sync::Arc::ptr_eq(&seeded, &moved),
            "the moved anchor mints a fresh memo"
        );
        assert_eq!(moved.stats(), (0, 0));
        for (address, slot) in &slots {
            assert!(
                !moved.holds(*address, *slot),
                "a prefetch at 100 must not appear at 101"
            );
        }

        // The N+1 fan-out pays its own cold chain and settles on the chain's
        // own answers (a stale serve would have shadowed them).
        let (values, forwards) = read_through_memo(&slots, &moved);
        assert_eq!(
            usize::try_from(forwards).unwrap(),
            slots.len(),
            "the moved anchor forwards, never the retired memo's entries"
        );
        let expected: Vec<Word> = slots.iter().map(|(_, slot)| chain_value(*slot)).collect();
        assert_eq!(values, expected);
        // The retired memo keeps its own anchor's values, unreachable from the
        // new scope (the fan-outs already pinned at 100 still share them).
        for (address, slot) in &slots {
            assert!(seeded.holds(*address, *slot));
        }
    }
}
