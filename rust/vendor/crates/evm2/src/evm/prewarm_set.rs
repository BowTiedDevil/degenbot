//! Pre-warmed address and storage-slot set for entries warmed before EVM execution begins.
//!
//! [`PrewarmSet`] holds the warm entries that come from chain/transaction configuration rather
//! than from an executing opcode: precompiles, the coinbase/beneficiary (EIP-3651), the EIP-2930
//! access list (addresses and their storage slots), and the non-revertible base warm accounts
//! (recipient, EIP-7702 authorities, system contracts). It is a port of revm's
//! `WarmAddresses`, adapted to evm2's primitive types.
//!
//! Warm addresses are tracked in two places. Short addresses — 18 leading zero bytes and a
//! last-two-byte value below [`SHORT_ADDRESS_CAP`], which is where precompiles and similar
//! low-numbered addresses fall — live in a bit vector for fast lookups. The first few other warm
//! addresses (the coinbase and the recipient) live in a small inline list that is
//! compared without hashing. Every remaining warm address, along with any address that carries warm
//! storage slots, lives in `access_list`.
//!
//! Note this is *not* the complete EIP-2929 initial warm set: the sender and recipient are also
//! warm from the start, but they are tracked per account by [`crate::evm::State`] instead — as is
//! all runtime warmth introduced while executing EVM code.

use crate::{interpreter::Word, utils::BitArray};
use alloy_primitives::{
    Address,
    map::{AddressMap, U256Set},
};

/// Short-address optimization cap. Addresses with 18 leading zero bytes whose last two bytes are
/// less than this value are tracked in a bit vector for fast warm lookups.
///
/// revm uses 300, which covers every precompile address in use. The bit array stores whole 64-bit
/// words, so 300 bits take five words anyway; 320 uses all of them at no extra cost.
const SHORT_ADDRESS_CAP: usize = 320;

/// Number of non-short warm addresses kept in the inline list before falling back to the map.
///
/// A transaction warms at most the coinbase and the recipient through
/// [`PrewarmSet::warm`]. Scanning this many addresses is cheaper than hashing.
const INLINE_CAP: usize = 3;

/// Bit vector indexed by short address. See [`short_address`].
pub(crate) type ShortAddresses = BitArray<{ SHORT_ADDRESS_CAP.div_ceil(64) }>;

/// Returns the short-address index for an address, if it qualifies.
///
/// A short address has 18 leading zero bytes and a last-two-byte value below [`SHORT_ADDRESS_CAP`].
#[inline]
pub(crate) fn short_address(address: &Address) -> Option<usize> {
    let (zeros, value) = address.split_at(18);
    if zeros.iter().all(|b| *b == 0) {
        let short_address = u16::from_be_bytes([value[0], value[1]]) as usize;
        if short_address < SHORT_ADDRESS_CAP {
            return Some(short_address);
        }
    }
    None
}

/// Stores addresses (and their storage slots) that are warm-loaded for the current transaction.
///
/// Warm addresses without warm storage are split by shape for lookup speed: short addresses (see
/// `SHORT_ADDRESS_CAP`) set a bit in `short_addresses`, the first few others fill `inline`, while
/// everything else — and any address carrying warm storage slots — is held in `access_list`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrewarmSet {
    /// Warm addresses keyed by address, each holding its warm storage slots.
    ///
    /// This holds the EIP-2930 access list, non-short warm addresses, and the non-revertible base
    /// warm entries (recipient, EIP-7702 authorities, system contracts) added via
    /// [`Self::warm`] and [`Self::warm_storage`]: they are warmed
    /// before or alongside execution and survive [`crate::evm::State::rollback`]. An address
    /// present with an empty slot set is warm with no warm storage slots.
    access_list: AddressMap<U256Set>,
    /// Bit vector of warm short addresses. An address shorter than [`SHORT_ADDRESS_CAP`] sets
    /// its bit here for faster access; precompiles and other low-numbered addresses land here.
    short_addresses: ShortAddresses,
    /// The first [`INLINE_CAP`] non-short warm addresses added via [`Self::warm`]. Only the first
    /// `inline_len` entries are meaningful.
    inline: [Address; INLINE_CAP],
    /// Number of occupied entries in `inline`.
    inline_len: usize,
    /// Whether any address in `access_list` carries warm storage slots. Most transactions have
    /// none, so storage warmth checks can skip the address lookup.
    has_storage: bool,
}

impl Default for PrewarmSet {
    fn default() -> Self {
        Self::new()
    }
}

impl PrewarmSet {
    /// Creates a new, empty warm-address set.
    #[inline]
    pub fn new() -> Self {
        Self {
            access_list: AddressMap::default(),
            short_addresses: BitArray::new(),
            inline: [Address::ZERO; INLINE_CAP],
            inline_len: 0,
            has_storage: false,
        }
    }

    /// Marks an address as warm.
    ///
    /// Short addresses set a bit in `short_addresses`, the first few others fill the inline list,
    /// and the rest are inserted into `access_list` with no warm storage slots. Used for
    /// precompiles, the coinbase/beneficiary, and the transaction-initial warmth established
    /// before any rollback checkpoint (recipient, EIP-7702 authorities, system
    /// contracts). This warmth survives [`crate::evm::State::rollback`] and is cleared per
    /// transaction by [`Self::clear`].
    #[inline]
    pub fn warm(&mut self, address: &Address) {
        if self.warm_short_address(address) {
            return;
        }
        if self.inline().contains(address) {
            return;
        }
        if let Some(slot) = self.inline.get_mut(self.inline_len) {
            *slot = *address;
            self.inline_len += 1;
            return;
        }
        self.access_list.entry(*address).or_default();
    }

    /// Marks an address and a set of storage slots as warm.
    ///
    /// The slots are unioned into any slots already warm for the address. See [`Self::warm`] for
    /// the rollback and clearing semantics.
    #[inline]
    pub fn warm_storage(&mut self, address: &Address, slots: impl IntoIterator<Item = Word>) {
        let mut slots = slots.into_iter().peekable();
        let has_slots = slots.peek().is_some();
        // A short address only needs a map entry once it carries storage slots.
        if self.warm_short_address(address) && !has_slots {
            return;
        }
        self.access_list.entry(*address).or_default().extend(slots);
        self.has_storage |= has_slots;
    }

    /// Marks every address in a short-address bit vector as warm.
    #[inline]
    pub(crate) fn warm_short_addresses(&mut self, addresses: &ShortAddresses) {
        self.short_addresses.union(addresses);
    }

    /// Marks a short address as warm.
    ///
    /// Returns whether the address was short.
    #[inline]
    fn warm_short_address(&mut self, address: &Address) -> bool {
        if let Some(short_address) = short_address(address) {
            self.short_addresses.set(short_address);
            true
        } else {
            false
        }
    }

    /// Removes an address and its warm storage slots, so it is cold unless warmed again.
    ///
    /// This cools a pre-warmed address, such as the recipient or an EIP-7702 authority, for
    /// example at a simulated transaction boundary.
    ///
    /// Like [`Self::warm`], this is not journaled and survives [`crate::evm::State::rollback`].
    /// The set does not record why an address is warm, so callers that keep precompiles, the
    /// coinbase, or access-list entries warm must warm them again.
    pub fn remove(&mut self, address: &Address) {
        if let Some(short_address) = short_address(address) {
            self.short_addresses.unset(short_address);
        }
        if let Some(index) = self.inline().iter().position(|a| a == address) {
            self.inline_len -= 1;
            self.inline.swap(index, self.inline_len);
        }
        // `has_storage` may stay set; it only lets lookups skip the map.
        self.access_list.remove(address);
    }

    /// Clears the per-transaction warm entries, leaving an empty set.
    #[inline]
    pub fn clear(&mut self) {
        self.short_addresses.clear();
        self.inline = [Address::ZERO; INLINE_CAP];
        self.inline_len = 0;
        self.access_list.clear();
        self.has_storage = false;
    }

    /// Returns whether the address is warm-loaded.
    pub fn is_warm(&self, address: &Address) -> bool {
        // check if it is a short address
        if let Some(short_address) = short_address(address)
            && self.short_addresses.get(short_address)
        {
            return true;
        }

        // then the inline list, then the access list
        self.inline().contains(address) || self.access_list.contains_key(address)
    }

    /// Returns the occupied part of the inline list.
    #[inline]
    fn inline(&self) -> &[Address] {
        &self.inline[..self.inline_len]
    }

    /// Returns whether the storage slot is warm-loaded.
    #[inline]
    pub fn is_storage_warm(&self, address: &Address, key: &Word) -> bool {
        self.has_storage && self.access_list.get(address).is_some_and(|slots| slots.contains(key))
    }

    /// Returns whether the address is cold-loaded.
    #[inline]
    pub fn is_cold(&self, address: &Address) -> bool {
        !self.is_warm(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::address;

    #[test]
    fn test_initialization() {
        let prewarm_set = PrewarmSet::new();
        assert_eq!(prewarm_set.short_addresses, BitArray::new());
        assert!(prewarm_set.access_list.is_empty());

        let default_addresses = PrewarmSet::default();
        assert_eq!(prewarm_set, default_addresses);
    }

    #[test]
    fn test_warm_short_addresses() {
        let mut prewarm_set = PrewarmSet::new();

        let short_addr1 = Address::with_last_byte(1);
        let short_addr2 = Address::with_last_byte(5);
        let mut bytes = [0u8; 20];
        bytes[18] = 1u8;
        bytes[19] = 63u8; // 319
        let last_short_addr = Address::from(bytes);

        prewarm_set.warm(&short_addr1);
        prewarm_set.warm(&short_addr2);
        prewarm_set.warm(&last_short_addr);

        assert!(prewarm_set.short_addresses.get(1));
        assert!(prewarm_set.short_addresses.get(5));
        assert!(prewarm_set.short_addresses.get(319));
        assert!(!prewarm_set.short_addresses.get(0));
        assert!(!prewarm_set.short_addresses.get(63));
        assert!(!prewarm_set.short_addresses.get(255));

        assert!(prewarm_set.is_warm(&short_addr1));
        assert!(prewarm_set.is_warm(&short_addr2));
        assert!(prewarm_set.is_warm(&last_short_addr));
        assert!(prewarm_set.is_cold(&Address::with_last_byte(20)));
        assert!(prewarm_set.access_list.is_empty());
    }

    #[test]
    fn test_warm_regular_addresses() {
        let mut prewarm_set = PrewarmSet::new();

        let regular_addr = address!("1234567890123456789012345678901234567890");
        let mut bytes = [0u8; 20];
        bytes[18] = 1u8;
        bytes[19] = 64u8; // 320
        let boundary_addr = Address::from(bytes);

        prewarm_set.warm(&regular_addr);
        prewarm_set.warm(&boundary_addr);

        // Non-short addresses are not tracked in the short-address bit vector.
        assert_eq!(prewarm_set.short_addresses, BitArray::new());

        assert!(prewarm_set.is_warm(&regular_addr));
        assert!(prewarm_set.is_warm(&boundary_addr));
        assert!(prewarm_set.is_cold(&address!("0987654321098765432109876543210987654321")));
    }

    #[test]
    fn test_warm_regular_addresses_beyond_inline_list() {
        let mut prewarm_set = PrewarmSet::new();
        let addresses = [0x10, 0x11, 0x12, 0x13, 0x14].map(Address::repeat_byte);

        for address in &addresses {
            prewarm_set.warm(address);
            prewarm_set.warm(address);
        }

        assert_eq!(prewarm_set.inline().len(), INLINE_CAP);
        assert_eq!(prewarm_set.access_list.len(), addresses.len() - INLINE_CAP);
        assert!(addresses.iter().all(|address| prewarm_set.is_warm(address)));

        prewarm_set.clear();
        assert_eq!(prewarm_set, PrewarmSet::new());
        assert!(addresses.iter().all(|address| prewarm_set.is_cold(address)));
    }

    #[test]
    fn test_clear() {
        let mut prewarm_set = PrewarmSet::new();
        let short_addr = Address::with_last_byte(1);
        let regular_addr = address!("1234567890123456789012345678901234567890");

        prewarm_set.warm(&short_addr);
        prewarm_set.warm(&regular_addr);
        assert!(prewarm_set.is_warm(&short_addr));
        assert!(prewarm_set.is_warm(&regular_addr));

        prewarm_set.clear();
        assert!(prewarm_set.is_cold(&short_addr));
        assert!(prewarm_set.is_cold(&regular_addr));
    }

    #[test]
    fn test_remove() {
        let mut prewarm_set = PrewarmSet::new();
        let short = Address::with_last_byte(1);
        let inline = [0x10, 0x11, 0x12].map(Address::repeat_byte);
        let mapped = Address::repeat_byte(0x13);
        let key = Word::from(7);
        prewarm_set.warm(&short);
        inline.iter().for_each(|address| prewarm_set.warm(address));
        prewarm_set.warm_storage(&mapped, [key]);

        prewarm_set.remove(&short);
        prewarm_set.remove(&inline[0]);
        prewarm_set.remove(&mapped);
        assert!(prewarm_set.is_cold(&short));
        assert!(prewarm_set.is_cold(&inline[0]));
        assert!(prewarm_set.is_cold(&mapped));
        assert!(!prewarm_set.is_storage_warm(&mapped, &key));
        assert!(prewarm_set.is_warm(&inline[1]) && prewarm_set.is_warm(&inline[2]));

        // The freed inline entry is reused.
        prewarm_set.warm(&mapped);
        assert!(prewarm_set.access_list.is_empty());
        assert!(prewarm_set.is_warm(&mapped));

        // Inline and short addresses that also carry warm storage lose their slots.
        prewarm_set.clear();
        prewarm_set.warm(&inline[0]);
        prewarm_set.warm_storage(&inline[0], [key]);
        prewarm_set.warm_storage(&short, [key]);
        prewarm_set.remove(&inline[0]);
        prewarm_set.remove(&short);
        for address in [inline[0], short] {
            assert!(prewarm_set.is_cold(&address));
            assert!(!prewarm_set.is_storage_warm(&address, &key));
        }
    }

    #[test]
    fn test_warm_storage_slots() {
        let mut prewarm_set = PrewarmSet::new();
        let addr = address!("1234567890123456789012345678901234567890");
        let key = Word::from(7);

        prewarm_set.warm_storage(&addr, [key]);

        assert!(prewarm_set.is_warm(&addr));
        assert!(prewarm_set.is_storage_warm(&addr, &key));
        assert!(!prewarm_set.is_storage_warm(&addr, &Word::from(8)));
    }
}
