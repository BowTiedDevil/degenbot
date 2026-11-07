//! Session-scoped ERC-20 account live state: the per-block caches for
//! balances, allowances, and total supply.
//!
//! A token's account values are LIVE STATE — they decay with every block —
//! and their cache belongs to the session that fetched them, not to the
//! token's identity and not to the stateless read module. This is that
//! session's store: one per [`BotState`](crate::BotState), keyed by the
//! registry-owned canonical token key
//! ([`TokenIdentity::key`](crate::session_registry::TokenIdentity::key))
//! plus the account side of the question, holding
//! nothing but already-read values.
//!
//! Every stream is a per-block FIFO of depth
//! [`ACCOUNT_CACHE_DEPTH`]: entries are keyed by block number, a re-store at
//! an already-cached block overwrites the value in place (the entry keeps its
//! position), and inserting past the depth evicts the OLDEST insertion —
//! so a cache that only ever sees fresh blocks keeps exactly the most recent
//! depth blocks and a session-lifetime stream cannot grow without bound.
//!
//! The read-through (cache hit, else fetch, else store) is the caller's
//! choreography; this module stores and answers, and never performs I/O.

use std::collections::VecDeque;

use alloy::primitives::{Address, U256};
use hashbrown::HashMap;
use parking_lot::Mutex;

/// How many per-block entries one account stream keeps: the FIFO depth for
/// balances, allowances, and total supply alike.
pub const ACCOUNT_CACHE_DEPTH: usize = 8;

/// One account stream's per-block FIFO cache.
///
/// Insertion-ordered, keyed by block number. Overwriting a cached block does
/// not move its position; inserting past [`ACCOUNT_CACHE_DEPTH`] evicts the
/// oldest insertion first.
#[derive(Debug, Default)]
struct BlockFifo {
    entries: VecDeque<(u64, U256)>,
}

impl BlockFifo {
    /// The cached value at `block`, if this stream holds one.
    fn get(&self, block: u64) -> Option<U256> {
        self.entries
            .iter()
            .find(|(entry_block, _)| *entry_block == block)
            .map(|(_, value)| *value)
    }

    /// Record `value` at `block`: overwriting in place when the block is
    /// already cached, appending (and evicting past the depth) otherwise.
    fn store(&mut self, block: u64, value: U256) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|(entry_block, _)| *entry_block == block)
        {
            entry.1 = value;
            return;
        }
        self.entries.push_back((block, value));
        while self.entries.len() > ACCOUNT_CACHE_DEPTH {
            self.entries.pop_front();
        }
    }
}

/// The session's ERC-20 account-value caches: one per-block stream per
/// question, bounded and read-only to I/O.
///
/// A balance stream is keyed by `(token, owner)`, an allowance stream by
/// `(token, owner, spender)` — each its own depth-8 FIFO, so one hot pair
/// cannot evict another's history — and a total-supply stream by `token`.
/// The token key is the registry-owned canonical projection
/// (`TokenIdentity::key`: `token:erc20:<address>` / `token:native`) — the one
/// token-key spelling, owned by the registry that mints the identity — which
/// is how the conceptual native token participates in account caching exactly
/// like an ERC-20 token without a protocol address of its own, as its own
/// distinct stream.
#[derive(Debug, Default)]
pub struct AccountCache {
    /// `(token key, owner)` → the per-block balance stream.
    balances: Mutex<HashMap<(String, Address), BlockFifo>>,
    /// `(token key, owner, spender)` → the per-block allowance stream.
    allowances: Mutex<HashMap<(String, Address, Address), BlockFifo>>,
    /// `token key` → the per-block total-supply stream.
    total_supplies: Mutex<HashMap<String, BlockFifo>>,
}

impl AccountCache {
    /// An empty cache holding no streams.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The cached balance of `owner` in the `token_key` stream at `block`, if
    /// cached.
    #[must_use]
    pub fn balance(&self, token_key: &str, owner: Address, block: u64) -> Option<U256> {
        self.balances
            .lock()
            .get(&(token_key.to_owned(), owner))
            .and_then(|stream| stream.get(block))
    }

    /// Record `owner`'s balance in the `token_key` stream at `block`.
    pub fn store_balance(&self, token_key: &str, owner: Address, block: u64, value: U256) {
        self.balances
            .lock()
            .entry((token_key.to_owned(), owner))
            .or_default()
            .store(block, value);
    }

    /// The cached allowance of `owner` to `spender` in the `token_key` stream
    /// at `block`, if cached.
    #[must_use]
    pub fn allowance(
        &self,
        token_key: &str,
        owner: Address,
        spender: Address,
        block: u64,
    ) -> Option<U256> {
        self.allowances
            .lock()
            .get(&(token_key.to_owned(), owner, spender))
            .and_then(|stream| stream.get(block))
    }

    /// Record the allowance of `owner` to `spender` in the `token_key` stream
    /// at `block`.
    pub fn store_allowance(
        &self,
        token_key: &str,
        owner: Address,
        spender: Address,
        block: u64,
        value: U256,
    ) {
        self.allowances
            .lock()
            .entry((token_key.to_owned(), owner, spender))
            .or_default()
            .store(block, value);
    }

    /// The cached total supply of the `token_key` stream at `block`, if
    /// cached.
    #[must_use]
    pub fn total_supply(&self, token_key: &str, block: u64) -> Option<U256> {
        self.total_supplies
            .lock()
            .get(token_key)
            .and_then(|stream| stream.get(block))
    }

    /// Record the total supply of the `token_key` stream at `block`.
    pub fn store_total_supply(&self, token_key: &str, block: u64, value: U256) {
        self.total_supplies
            .lock()
            .entry(token_key.to_owned())
            .or_default()
            .store(block, value);
    }
}

#[cfg(test)]
#[expect(clippy::unwrap_used)]
mod tests {
    use alloy::primitives::{address, Address, U256};

    use super::{AccountCache, ACCOUNT_CACHE_DEPTH};
    use crate::session_registry::TokenIdentity;

    const TOKEN: TokenIdentity =
        TokenIdentity::Erc20(address!("11111111111111111111111111111111111111a1"));
    const OTHER_TOKEN: TokenIdentity =
        TokenIdentity::Erc20(address!("22222222222222222222222222222222222222b2"));
    const OWNER: Address = address!("3333333333333333333333333333333333333333");
    const SPENDER: Address = address!("4444444444444444444444444444444444444444");

    #[test]
    fn a_cached_block_answers_and_an_uncached_block_does_not() {
        let cache = AccountCache::new();
        cache.store_balance(&TOKEN.key(), OWNER, 100, U256::from(7u64));

        assert_eq!(
            cache.balance(&TOKEN.key(), OWNER, 100),
            Some(U256::from(7u64))
        );
        assert_eq!(cache.balance(&TOKEN.key(), OWNER, 101), None);
        assert_eq!(cache.balance(&OTHER_TOKEN.key(), OWNER, 100), None);
    }

    #[test]
    fn a_balance_stream_keeps_the_depth_eight_most_recent_insertions() {
        let cache = AccountCache::new();
        for block in 0..(ACCOUNT_CACHE_DEPTH as u64 + 2) {
            cache.store_balance(&TOKEN.key(), OWNER, block, U256::from(block));
        }

        // The two oldest insertions were evicted; everything else is cached.
        assert_eq!(cache.balance(&TOKEN.key(), OWNER, 0), None);
        assert_eq!(cache.balance(&TOKEN.key(), OWNER, 1), None);
        for block in 2..(ACCOUNT_CACHE_DEPTH as u64 + 2) {
            assert_eq!(
                cache.balance(&TOKEN.key(), OWNER, block),
                Some(U256::from(block))
            );
        }
    }

    #[test]
    fn overwriting_a_cached_block_keeps_its_position_in_the_fifo() {
        let cache = AccountCache::new();
        for block in 1..=ACCOUNT_CACHE_DEPTH as u64 {
            cache.store_balance(&TOKEN.key(), OWNER, block, U256::from(block));
        }
        // Re-store the OLDEST block: the value overwrites in place and the
        // entry keeps its position at the front of the FIFO — so it is still
        // the first one evicted when the stream overflows, and the block
        // behind it survives.
        cache.store_balance(&TOKEN.key(), OWNER, 1, U256::from(111u64));
        cache.store_balance(&TOKEN.key(), OWNER, 9, U256::from(9u64));

        assert_eq!(cache.balance(&TOKEN.key(), OWNER, 1), None);
        assert_eq!(cache.balance(&TOKEN.key(), OWNER, 2), Some(U256::from(2)));
        assert_eq!(
            cache.balance(&TOKEN.key(), OWNER, 9),
            Some(U256::from(9u64))
        );
    }

    #[test]
    fn a_total_supply_stream_is_bounded_the_same_way() {
        let cache = AccountCache::new();
        for block in 0..=(ACCOUNT_CACHE_DEPTH as u64) {
            cache.store_total_supply(&TOKEN.key(), block, U256::from(block));
        }

        assert_eq!(cache.total_supply(&TOKEN.key(), 0), None);
        assert_eq!(
            cache.total_supply(&TOKEN.key(), ACCOUNT_CACHE_DEPTH as u64),
            Some(U256::from(ACCOUNT_CACHE_DEPTH as u64))
        );
    }

    #[test]
    fn an_allowance_stream_is_bounded_per_owner_spender_pair() {
        let cache = AccountCache::new();
        for block in 0..=(ACCOUNT_CACHE_DEPTH as u64) {
            cache.store_allowance(&TOKEN.key(), OWNER, SPENDER, block, U256::from(block));
        }
        // A second pair stores at the FIRST block only: its own stream, so
        // the first pair's overflow cannot evict it.
        cache.store_allowance(
            &TOKEN.key(),
            OWNER,
            TOKEN.address().unwrap(),
            0,
            U256::from(500u64),
        );

        // The first pair's oldest block evicted; the second pair unaffected.
        assert_eq!(cache.allowance(&TOKEN.key(), OWNER, SPENDER, 0), None);
        assert_eq!(
            cache.allowance(&TOKEN.key(), OWNER, SPENDER, ACCOUNT_CACHE_DEPTH as u64),
            Some(U256::from(ACCOUNT_CACHE_DEPTH as u64))
        );
        assert_eq!(
            cache.allowance(&TOKEN.key(), OWNER, TOKEN.address().unwrap(), 0),
            Some(U256::from(500u64))
        );
    }

    #[test]
    fn the_native_token_participates_in_account_caching_without_an_address() {
        let cache = AccountCache::new();
        let native = TokenIdentity::native();
        assert_eq!(native.address(), None);

        cache.store_balance(&native.key(), OWNER, 100, U256::from(5u64));
        cache.store_total_supply(&native.key(), 100, U256::MAX);

        assert_eq!(
            cache.balance(&native.key(), OWNER, 100),
            Some(U256::from(5u64))
        );
        assert_eq!(cache.total_supply(&native.key(), 100), Some(U256::MAX));
        // Native is its own stream: an ERC-20 identity at any address never
        // answers for it.
        assert_eq!(cache.balance(&TOKEN.key(), OWNER, 100), None);
    }
}
