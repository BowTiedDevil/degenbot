//! `BotCore` — the single owner of all runtime state.
//!
//! All pool data, token metadata, calculation methods, and swap encoding
//! live here. Python objects are thin `PyO3` handles carrying keys into
//! `BotCore`'s `HashMaps`.

use std::collections::HashMap;

use alloy::primitives::{Address, U256};

use crate::optimizers::mobius_int::IntHopState;
use crate::bot_core::state_history::{ReorgJournal, V2BlockDelta, V3BlockDelta, TickBefore, V3RestoreResult};
use crate::bot_core::v2_encoding::{encode_v2_swap, EncodedCall};

pub mod py_bot;
pub mod py_pool;
pub mod py_token;
pub mod state_history;
pub mod tick_bitmap;
pub mod v2_encoding;
pub mod v3_mint_burn_decoder;
pub mod v3_swap_decoder;
pub mod v4_swap_decoder;

// ---------------------------------------------------------------------------
// Pool state types
// ---------------------------------------------------------------------------

/// A single pool's state. Pool-type-specific fields are in the enum variants.
#[derive(Clone, Debug)]
pub enum PoolEntry {
    V2(V2PoolState),
    V3(V3PoolState),
}

/// State for a Uniswap V2-style constant-product pool.
#[derive(Clone, Debug)]
pub struct V2PoolState {
    /// Pool contract address.
    pub address: Address,
    /// Token0 contract address.
    pub token0: Address,
    /// Token1 contract address.
    pub token1: Address,
    /// Fee parameters for token0→token1 swaps: (`gamma_numer`, `fee_denom`).
    pub fee_token0: (u64, u64),
    /// Fee parameters for token1→token0 swaps: (`gamma_numer`, `fee_denom`).
    pub fee_token1: (u64, u64),
    /// Pool factory address.
    pub factory: Address,

    /// Current reserve of token0.
    pub reserve0: U256,
    /// Current reserve of token1.
    pub reserve1: U256,
    /// Block number of the last update.
    pub update_block: u64,

    /// Reorg journal — "before" values for rollback.
    /// V2 is the degenerate case: delta = full state (two reserves).
    pub journal: ReorgJournal<V2BlockDelta>,
}

/// Parameters for registering a V2 pool.
#[derive(Clone, Debug)]
pub struct RegisterV2PoolParams {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    pub fee_token0: (u64, u64),
    pub fee_token1: (u64, u64),
    pub factory: Address,
}

// ---------------------------------------------------------------------------
// V3 pool state
// ---------------------------------------------------------------------------

/// State for a Uniswap V3 concentrated-liquidity pool.
///
/// The current mutable fields (`sqrt_price_x96`, `liquidity`, `tick`,
/// `tick_bitmap`, `tick_data`) are the authoritative current values.
/// The journal stores "before" values for reorg rollback.
///
/// Swap calculations never touch the journal — they always read the
/// current mutable fields. Zero penalty on the hot path.
#[derive(Clone, Debug)]
pub struct V3PoolState {
    /// Pool contract address.
    pub address: Address,
    /// Token0 contract address.
    pub token0: Address,
    /// Token1 contract address.
    pub token1: Address,
    /// Pool fee in pips (e.g., 3000 = 0.3%).
    pub fee: u32,
    /// Tick spacing for this pool (e.g., 60 for 0.3% fee tier).
    pub tick_spacing: i32,
    /// Pool factory address.
    pub factory: Address,

    // --- Current mutable state (authoritative) ---

    /// Current sqrt price × 2^96.
    pub sqrt_price_x96: U256,
    /// Current active liquidity.
    pub liquidity: alloy::primitives::U128,
    /// Current tick.
    pub tick: i32,
    /// Tick bitmap: word position → bitmap value.
    /// `i16` matches the Python `BitmapWord` type (word position in the bitmap).
    pub tick_bitmap: std::collections::HashMap<i16, U256>,
    /// Tick data: tick index → (`liquidity_gross`, `liquidity_net`).
    /// Only initialized ticks are stored.
    pub tick_data: std::collections::HashMap<i32, TickInfo>,
    /// Block number of the last update.
    pub update_block: u64,

    /// Reorg journal — scalar priors + per-tick priors for rollback.
    pub journal: ReorgJournal<V3BlockDelta>,
}

/// Liquidity data at an initialized tick.
///
/// Mirrors the Python `LiquidityAtTick` from `concentrated/types.py`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TickInfo {
    /// The total liquidity that references this tick.
    pub liquidity_gross: alloy::primitives::U128,
    /// The liquidity delta for ticks entered from left to right.
    /// Positive for lower ticks, negative for upper ticks.
    pub liquidity_net: alloy::primitives::I256,
}

/// Parameters for registering a V3 pool.
#[derive(Clone, Debug)]
pub struct RegisterV3PoolParams {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub fee: u32,
    pub tick_spacing: i32,
    pub factory: Address,
    pub sqrt_price_x96: U256,
    pub liquidity: alloy::primitives::U128,
    pub tick: i32,
    pub tick_bitmap: std::collections::HashMap<i16, U256>,
    pub tick_data: std::collections::HashMap<i32, TickInfo>,
}

// ---------------------------------------------------------------------------
// Token state
// ---------------------------------------------------------------------------

/// ERC20 token metadata.
#[derive(Clone, Debug)]
pub struct TokenEntry {
    pub address: Address,
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    pub chain_id: u64,
}

// ---------------------------------------------------------------------------
// BotCore
// ---------------------------------------------------------------------------

/// The single owner of all runtime state.
///
/// All pool data, token metadata, engines, and encoded results live here.
/// Python holds `PyBotCore` — an `Arc` pointing here.
pub struct BotCore {
    /// Pool registry: `pool_id` → `PoolEntry`.
    pools: HashMap<u64, PoolEntry>,
    /// Pool contract address → `pool_id`.
    pool_addresses: HashMap<Address, u64>,
    /// Token registry: address → `TokenEntry`.
    tokens: HashMap<Address, TokenEntry>,
    /// Auto-incrementing pool ID.
    next_pool_id: u64,
}

impl BotCore {
    /// Create a new, empty `BotCore`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pools: HashMap::new(),
            pool_addresses: HashMap::new(),
            tokens: HashMap::new(),
            next_pool_id: 1,
        }
    }

    /// Register a V2 pool by contract address.
    ///
    /// Returns the auto-assigned pool ID.
    ///
    /// # Panics
    ///
    /// Panics if the pool address is already registered.
    pub fn register_v2_pool(&mut self, params: &RegisterV2PoolParams) -> u64 {
        assert!(
            !self.pool_addresses.contains_key(&params.address),
            "pool already registered: {}",
            params.address
        );

        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;

        let RegisterV2PoolParams {
            address,
            token0,
            token1,
            reserve0,
            reserve1,
            fee_token0,
            fee_token1,
            factory,
        } = *params;

        let journal = ReorgJournal::<V2BlockDelta>::new(8);

        self.pools.insert(
            pool_id,
            PoolEntry::V2(V2PoolState {
                address,
                token0,
                token1,
                fee_token0,
                fee_token1,
                factory,
                reserve0,
                reserve1,
                update_block: 0,
                journal,
            }),
        );
        self.pool_addresses.insert(address, pool_id);

        pool_id
    }

    /// Register a V3 pool by contract address.
    ///
    /// Returns the auto-assigned pool ID.
    ///
    /// # Panics
    ///
    /// Panics if the pool address is already registered.
    pub fn register_v3_pool(&mut self, params: &RegisterV3PoolParams) -> u64 {
        assert!(
            !self.pool_addresses.contains_key(&params.address),
            "pool already registered: {}",
            params.address
        );

        let pool_id = self.next_pool_id;
        self.next_pool_id += 1;

        let RegisterV3PoolParams {
            address,
            token0,
            token1,
            fee,
            tick_spacing,
            factory,
            sqrt_price_x96,
            liquidity,
            tick,
            tick_bitmap,
            tick_data,
        } = params.clone();

        let journal = ReorgJournal::<V3BlockDelta>::new(8);

        self.pools.insert(
            pool_id,
            PoolEntry::V3(V3PoolState {
                address,
                token0,
                token1,
                fee,
                tick_spacing,
                factory,
                sqrt_price_x96,
                liquidity,
                tick,
                tick_bitmap,
                tick_data,
                update_block: 0,
                journal,
            }),
        );
        self.pool_addresses.insert(address, pool_id);

        pool_id
    }

    /// Update a V2 pool's reserves from a Sync event.
    ///
    /// Looks up the pool by contract address. No-op if the pool is not registered.
    ///
    /// # Panics
    ///
    /// Panics if a `pool_id` is found in `pool_addresses` but not in `pools`
    /// (should never happen — they are inserted together).
    pub fn update_v2_pool(
        &mut self,
        pool_address: Address,
        reserve0: U256,
        reserve1: U256,
        block_number: u64,
    ) {
        let Some(&pool_id) = self.pool_addresses.get(&pool_address) else {
            return;
        };

        let Some(PoolEntry::V2(state)) = self.pools.get_mut(&pool_id) else {
            return;
        };

        // Stash "before" values in the reorg journal before updating
        state.journal.push_delta(V2BlockDelta {
            block: block_number,
            reserve0_before: state.reserve0,
            reserve1_before: state.reserve1,
        });

        state.reserve0 = reserve0;
        state.reserve1 = reserve1;
        state.update_block = block_number;
    }

    /// Update a V3 pool's state from a Swap event.
    ///
    /// Looks up the pool by contract address. No-op if the pool is not registered.
    ///
    /// Stashes scalar "before" values in the reorg journal before updating.
    /// Also stashes per-tick priors for any ticks in `tick_priors` that were
    /// modified during this block.
    pub fn update_v3_pool(
        &mut self,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: alloy::primitives::U128,
        tick: i32,
        block_number: u64,
        tick_priors: Vec<(i32, TickBefore)>,
    ) {
        let Some(&pool_id) = self.pool_addresses.get(&pool_address) else {
            return;
        };

        let Some(PoolEntry::V3(state)) = self.pools.get_mut(&pool_id) else {
            return;
        };

        // Stash "before" values in the reorg journal before updating
        state.journal.push_delta(V3BlockDelta {
            block: block_number,
            sqrt_price_x96_before: state.sqrt_price_x96,
            liquidity_before: state.liquidity,
            tick_before: state.tick,
            tick_priors,
        });

        state.sqrt_price_x96 = sqrt_price_x96;
        state.liquidity = liquidity;
        state.tick = tick;
        state.update_block = block_number;
    }

    /// Calculate the output token amount for a given input amount.
    ///
    /// Uses the constant-product invariant with EVM-exact integer arithmetic.
    ///
    /// Returns 0 if the pool is not found or the amount is 0.
    #[must_use]
    pub fn calculate_tokens_out(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount_in: U256,
    ) -> U256 {
        let Some(entry) = self.pools.get(&pool_id) else {
            return U256::ZERO;
        };

        match entry {
            PoolEntry::V2(state) => {
                if amount_in.is_zero() {
                    return U256::ZERO;
                }

                let (reserve_in, reserve_out, gamma_numer, fee_denom) = if zero_for_one {
                    (
                        state.reserve0,
                        state.reserve1,
                        state.fee_token0.0,
                        state.fee_token0.1,
                    )
                } else {
                    (
                        state.reserve1,
                        state.reserve0,
                        state.fee_token1.0,
                        state.fee_token1.1,
                    )
                };

                let hop = IntHopState::new(reserve_in, reserve_out, gamma_numer, fee_denom);
                hop.swap(amount_in)
            }
            // V3 concentrated-liquidity math is not yet implemented (Slice 7)
            PoolEntry::V3(_) => U256::ZERO,
        }
    }

    /// Calculate the input token amount required for a given output amount.
    ///
    /// Uses the constant-product invariant with EVM-exact integer arithmetic.
    ///
    /// Returns 0 if the pool is not found, the amount is 0,
    /// or the output exceeds available reserves.
    #[must_use]
    pub fn calculate_tokens_in(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: U256,
    ) -> U256 {
        let Some(entry) = self.pools.get(&pool_id) else {
            return U256::ZERO;
        };

        match entry {
            PoolEntry::V2(state) => {
                if amount_out.is_zero() {
                    return U256::ZERO;
                }

                let (reserve_in, reserve_out, gamma_numer, fee_denom) = if zero_for_one {
                    (
                        state.reserve0,
                        state.reserve1,
                        state.fee_token0.0,
                        state.fee_token0.1,
                    )
                } else {
                    (
                        state.reserve1,
                        state.reserve0,
                        state.fee_token1.0,
                        state.fee_token1.1,
                    )
                };

                if amount_out >= reserve_out {
                    return U256::ZERO;
                }

                // constant_product_calc_exact_out:
                // amount_in = 1 + (reserve_in * amount_out * fee_denom) //
                //   ((reserve_out - amount_out) * gamma_numer)
                let numerator = U256::from(reserve_in)
                    .saturating_mul(amount_out)
                    .saturating_mul(U256::from(fee_denom));
                let denominator = (reserve_out.saturating_sub(amount_out))
                    .saturating_mul(U256::from(gamma_numer));

                if denominator.is_zero() {
                    return U256::ZERO;
                }

                U256::from(1) + numerator / denominator
            }
            // V3 concentrated-liquidity math is not yet implemented (Slice 7)
            PoolEntry::V3(_) => U256::ZERO,
        }
    }

    /// Get the pool ID for a given contract address.
    #[must_use]
    pub fn pool_id_by_address(&self, address: &Address) -> Option<u64> {
        self.pool_addresses.get(address).copied()
    }

    /// Number of registered pools.
    #[must_use]
    pub fn pool_count(&self) -> usize {
        self.pools.len()
    }

    /// Check if a pool ID is registered.
    #[must_use]
    pub fn has_pool(&self, pool_id: u64) -> bool {
        self.pools.contains_key(&pool_id)
    }

    /// Check if a token address is registered.
    #[must_use]
    pub fn has_token(&self, address: &Address) -> bool {
        self.tokens.contains_key(address)
    }

    /// Get the number of deltas in the reorg journal for a V2 pool.
    ///
    /// Returns 0 if the pool ID is not registered.
    #[must_use]
    pub fn v2_journal_len(&self, pool_id: u64) -> usize {
        match self.pools.get(&pool_id) {
            Some(PoolEntry::V2(state)) => state.journal.len(),
            _ => 0,
        }
    }

    /// Discard V2 reorg journal deltas earlier than the given block.
    ///
    /// No-op if the pool ID is not registered.
    ///
    /// # Panics
    ///
    /// Panics if all deltas are before the target block.
    pub fn v2_discard_before_block(&mut self, pool_id: u64, block: u64) {
        let Some(PoolEntry::V2(state)) = self.pools.get_mut(&pool_id) else {
            return;
        };
        state.journal.discard_before_block(block);
    }

    /// Restore V2 pool state prior to a target block.
    ///
    /// Pops reorg journal deltas at/after the target block and restores
    /// "before" values into the current state.
    ///
    /// Returns `(reserve0, reserve1, block)` of the restored state, or `None`
    /// if the pool ID is not registered.
    ///
    /// # Panics
    ///
    /// Panics if no delta exists before the target block.
    pub fn v2_restore_before_block(
        &mut self,
        pool_id: u64,
        block: u64,
    ) -> Option<(U256, U256, u64)> {
        let PoolEntry::V2(state) = self.pools.get_mut(&pool_id)? else {
            return None;
        };
        let (r0, r1, blk) = state.journal.restore_before_block(block);
        // Sync the pool's current reserves with the restored "before" values
        state.reserve0 = r0;
        state.reserve1 = r1;
        state.update_block = blk;
        Some((r0, r1, blk))
    }

    // --- V3 journal methods ---

    /// Get the number of deltas in the reorg journal for a V3 pool.
    ///
    /// Returns 0 if the pool ID is not registered or is not a V3 pool.
    #[must_use]
    pub fn v3_journal_len(&self, pool_id: u64) -> usize {
        match self.pools.get(&pool_id) {
            Some(PoolEntry::V3(state)) => state.journal.len(),
            _ => 0,
        }
    }

    /// Discard V3 reorg journal deltas earlier than the given block.
    ///
    /// No-op if the pool ID is not registered or is not a V3 pool.
    ///
    /// # Panics
    ///
    /// Panics if all deltas are before the target block.
    pub fn v3_discard_before_block(&mut self, pool_id: u64, block: u64) {
        let Some(PoolEntry::V3(state)) = self.pools.get_mut(&pool_id) else {
            return;
        };
        state.journal.discard_before_block(block);
    }

    /// Restore V3 pool state prior to a target block.
    ///
    /// Pops reorg journal deltas at/after the target block, restores
    /// scalar "before" values into the current state, and reverse-applies
    /// tick priors to the current `tick_data` map.
    ///
    /// Returns `V3RestoreResult` with the before-values, or `None`
    /// if the pool ID is not registered or is not a V3 pool.
    ///
    /// # Panics
    ///
    /// Panics if no delta exists before the target block.
    pub fn v3_restore_before_block(
        &mut self,
        pool_id: u64,
        block: u64,
    ) -> Option<V3RestoreResult> {
        let PoolEntry::V3(state) = self.pools.get_mut(&pool_id)? else {
            return None;
        };
        let result = state.journal.restore_before_block(block);

        // Sync scalar fields
        state.sqrt_price_x96 = result.sqrt_price_x96_before;
        state.liquidity = result.liquidity_before;
        state.tick = result.tick_before;
        state.update_block = result.block;

        // Reverse-apply tick priors
        for (tick_idx, tick_before) in &result.tick_priors {
            match tick_before.liquidity_gross_before {
                Some(gross_before) => {
                    // Tick existed before — restore its prior values
                    state.tick_data.insert(
                        *tick_idx,
                        TickInfo {
                            liquidity_gross: gross_before,
                            liquidity_net: tick_before.liquidity_net_before,
                        },
                    );
                }
                None => {
                    // Tick was newly initialized in this block — remove it
                    state.tick_data.remove(tick_idx);
                }
            }
        }

        Some(result)
    }

    /// Encode a V2 swap call for the given pool.
    ///
    /// Produces pre-encoded calldata for `swap(uint256,uint256,address,bytes)`
    /// that is ready for on-chain submission.
    ///
    /// Returns `None` if the pool ID is not registered.
    #[must_use]
    pub fn encode_swap(
        &self,
        pool_id: u64,
        zero_for_one: bool,
        amount_out: U256,
        recipient: Address,
    ) -> Option<EncodedCall> {
        let entry = self.pools.get(&pool_id)?;
        match entry {
            PoolEntry::V2(state) => {
                let call = encode_v2_swap(state.address, zero_for_one, amount_out, recipient)
                    .ok()?;
                Some(call)
            }
            // V3 encoding is not yet implemented
            PoolEntry::V3(_) => None,
        }
    }

    /// Get the pool address for a given pool ID.
    #[must_use]
    pub fn pool_address(&self, pool_id: u64) -> Option<Address> {
        match self.pools.get(&pool_id)? {
            PoolEntry::V2(state) => Some(state.address),
            PoolEntry::V3(state) => Some(state.address),
        }
    }

    /// Register a token.
    ///
    /// # Panics
    ///
    /// Panics if the token address is already registered.
    pub fn register_token(
        &mut self,
        address: Address,
        name: String,
        symbol: String,
        decimals: u8,
        chain_id: u64,
    ) {
        assert!(
            !self.tokens.contains_key(&address),
            "token already registered: {address}"
        );

        self.tokens.insert(
            address,
            TokenEntry {
                address,
                name,
                symbol,
                decimals,
                chain_id,
            },
        );
    }
}

impl Default for BotCore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEE_03: (u64, u64) = (997, 1000);

    fn make_pool_addr() -> Address {
        Address::from([0xaa; 20])
    }
    fn make_token0() -> Address {
        Address::from([0xbb; 20])
    }
    fn make_token1() -> Address {
        Address::from([0xcc; 20])
    }
    fn make_factory() -> Address {
        Address::from([0xdd; 20])
    }

    fn make_params(r0: U256, r1: U256) -> RegisterV2PoolParams {
        RegisterV2PoolParams {
            address: make_pool_addr(),
            token0: make_token0(),
            token1: make_token1(),
            reserve0: r0,
            reserve1: r1,
            fee_token0: FEE_03,
            fee_token1: FEE_03,
            factory: make_factory(),
        }
    }

    #[test]
    fn register_v2_pool_and_calculate_tokens_out() {
        let mut core = BotCore::new();
        let pool_id = core.register_v2_pool(&make_params(U256::from(1000), U256::from(2000)));

        // Python reference: constant_product_calc_exact_in(100, 1000, 2000, 3/1000) = 181
        let amount_out = core.calculate_tokens_out(pool_id, true, U256::from(100));
        assert_eq!(amount_out, U256::from(181));
    }

    #[test]
    fn calculate_tokens_out_reverse_direction() {
        let mut core = BotCore::new();
        let pool_id = core.register_v2_pool(&make_params(U256::from(2000), U256::from(1000)));

        // Python reference: constant_product_calc_exact_in(100, 1000, 2000, 3/1000) = 181
        let amount_out = core.calculate_tokens_out(pool_id, false, U256::from(100));
        assert_eq!(amount_out, U256::from(181));
    }

    #[test]
    fn update_v2_pool_changes_calculation_result() {
        let mut core = BotCore::new();
        let pool_id = core.register_v2_pool(&make_params(U256::from(1000), U256::from(2000)));

        // Before update: swap 100 token0 → 181 token1
        let before = core.calculate_tokens_out(pool_id, true, U256::from(100));
        assert_eq!(before, U256::from(181));

        // Update reserves: now reserve0=2000, reserve1=1000
        core.update_v2_pool(make_pool_addr(), U256::from(2000), U256::from(1000), 42);

        // After update: Python: constant_product_calc_exact_in(100, 2000, 1000, 3/1000) = 47
        let after = core.calculate_tokens_out(pool_id, true, U256::from(100));
        assert_eq!(after, U256::from(47));
    }

    #[test]
    fn calculate_tokens_in_for_v2_pool() {
        let mut core = BotCore::new();
        let pool_id = core.register_v2_pool(&make_params(U256::from(1000), U256::from(2000)));

        // Python: constant_product_calc_exact_out(50, 1000, 2000, 3/1000) = 26
        let amount_in = core.calculate_tokens_in(pool_id, true, U256::from(50));
        assert_eq!(amount_in, U256::from(26));

        // Reverse: Python: constant_product_calc_exact_out(10, 2000, 1000, 3/1000) = 21
        let amount_in_rev = core.calculate_tokens_in(pool_id, false, U256::from(10));
        assert_eq!(amount_in_rev, U256::from(21));
    }

    #[test]
    fn calculate_tokens_out_realistic_amounts() {
        let mut core = BotCore::new();

        // Realistic: 1.5M USDC / 800 WETH, 0.3% fee
        let reserve0 = U256::from(1_500_000_000_000u64); // 1.5M USDC (6dp)
        let reserve1 = U256::from(800u128) * U256::from(10u64).pow(U256::from(18)); // 800 WETH

        let params = RegisterV2PoolParams {
            address: make_pool_addr(),
            token0: make_token0(),
            token1: make_token1(),
            reserve0,
            reserve1,
            fee_token0: FEE_03,
            fee_token1: FEE_03,
            factory: make_factory(),
        };
        let pool_id = core.register_v2_pool(&params);

        // Swap 1000 USDC for WETH
        // Python reference: 531380142665175213
        let amount_in = U256::from(1_000_000_000u64); // 1000 USDC (6dp)
        let amount_out = core.calculate_tokens_out(pool_id, true, amount_in);
        assert_eq!(amount_out, U256::from(531380142665175213u64));
    }
}
