//! BAL-accelerated anchored read prefetch: one anchor's EIP-7928 block access
//! list turned into [`ReadHub`](super::read_hub::ReadHub) assistance.
//!
//! A block access list records every storage slot the block touched. Two facts
//! make it useful to a hub anchored at that block: each written slot's LAST
//! change is the anchor's post-block value (the exact answer
//! `eth_getStorageAt` would give), and each read-only slot is a slot the fan-out
//! is likely to ask for — an identity worth joining the hub's next batch. This
//! module turns raw list bytes into those two sets; the hub's `seed_bal` /
//! `prefetch_bal` apply them through the ordinary memo and batch/drain paths.
//!
//! # The contract is best-effort, all-or-nothing, storage-only
//!
//! Assistance never fails a read. A missing anchor hash (the request is
//! exact-hash anchored, never a mutable tag), a refused or failed request, or
//! malformed RLP is a [`BalSkip`] with NOTHING seeded: the input decodes
//! wholesale before any hub mutation, so a malformed list cannot half-seed the
//! memo. Account entries are ignored — balance/nonce/code changes do not form
//! an [`AccountRead`](degenbot_rpc::provider::AccountRead) — so accounts stay
//! wholly on the dynamic path. Seeded values belong to the hub's anchor alone
//! and die with it; they are never reused at another number or hash.
//!
//! # The plan bounds the blast radius
//!
//! A full-block list can carry thousands of slots the fan-out will never read.
//! [`BalPlan`] caps and/or address-filters the seeding so assistance turns into
//! work only where it can pay: capped or filtered entries are simply omitted
//! (counted, never hidden), and every omitted slot follows the ordinary dynamic
//! read path with unchanged semantics.

use alloy::primitives::{Address, U256};
use alloy_eip7928::bal::{Bal, DecodedBal};

/// Why a BAL assist stage seeded nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BalSkip {
    /// The anchor carries no block hash, so the exact-hash request cannot be made.
    #[error("the anchor carries no block hash, so the exact-hash BAL request cannot be made")]
    NoAnchorHash,
    /// The `debug_getRawBlockAccessList` request failed: unsupported method,
    /// transport failure, no list at that hash — any classified provider error.
    #[error("the raw block access list request failed: {0}")]
    Rpc(String),
    /// The returned bytes did not decode as one complete EIP-7928 list.
    #[error("the raw block access list did not decode: {0}")]
    Decode(String),
}

impl BalSkip {
    /// The closed-vocabulary label this skip reports — one label per VARIANT, so
    /// a diagnostic row names the cause a caller's collapsed `bal-unreadable`
    /// label hides.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::NoAnchorHash => "no-anchor-hash",
            Self::Rpc(_) => "rpc",
            Self::Decode(_) => "decode",
        }
    }
}

/// The bytes did not decode as one complete EIP-7928 block access list.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("malformed block access list RLP: {0}")]
pub struct BalDecodeError(String);

/// The bounded, optionally address-filtered seeding plan: what a full-block BAL
/// is allowed to turn into hub work. The defaults admit everything (a
/// prototype's posture); a caller that cares about batch size passes caps
/// and/or the addresses its fan-out actually touches.
#[derive(Debug, Clone, Default)]
pub struct BalPlan {
    /// When set, only these addresses' entries are seeded or queued; every
    /// other account's entries are counted as filtered.
    pub addresses: Option<Vec<Address>>,
    /// Cap on final write values seeded into the anchor memo.
    pub max_write_seeds: Option<usize>,
    /// Cap on read-only slot identities queued into the batch path.
    pub max_read_prefetches: Option<usize>,
}

impl BalPlan {
    fn admits(&self, address: Address) -> bool {
        self.addresses
            .as_ref()
            .is_none_or(|allowed| allowed.contains(&address))
    }
    fn accepts_write(&self, seeded: usize) -> bool {
        self.max_write_seeds.is_none_or(|cap| seeded < cap)
    }
    fn accepts_read(&self, queued: usize) -> bool {
        self.max_read_prefetches.is_none_or(|cap| queued < cap)
    }
}

/// One decoded, planned block access list: the values and identities worth
/// seeding, and what the plan dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BalSeed {
    /// Final post-block write values `(address, slot, value)`: the LAST change
    /// of each written slot, which is exactly the anchor's post-block word.
    pub writes: Vec<(Address, U256, U256)>,
    /// Read-only slot identities `(address, slot)`: values unknown, worth one
    /// batch member each through the hub's own queue.
    pub reads: Vec<(Address, U256)>,
    /// Entries dropped by the plan's address filter.
    pub filtered_entries: usize,
    /// Entries dropped by the plan's caps.
    pub capped_entries: usize,
}

/// What one seeding applied to a hub.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BalSeedReport {
    /// Final write values that became anchor-memo entries (settling any queued
    /// waiter with the very value its round would have fetched).
    pub writes_seeded: usize,
    /// Write identities left untouched because an in-flight round owns them
    /// (that round settles them with the same anchor value).
    pub writes_ignored_in_flight: usize,
    /// Write identities already in the anchor memo.
    pub writes_already_known: usize,
    /// Read-only identities queued into the next round's storage batch.
    pub reads_queued: usize,
    /// Read-only identities already memoized, queued, or in flight.
    pub reads_skipped_known: usize,
}

/// What one BAL assist stage did, or why it stepped aside.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BalPrefetchReport {
    /// Why the stage seeded nothing; `None` when it ran to completion (a plan
    /// may still have filtered or capped everything).
    pub skip: Option<BalSkip>,
    /// What reached the hub.
    pub seeded: BalSeedReport,
    /// Entries dropped by the plan's address filter.
    pub filtered_entries: usize,
    /// Entries dropped by the plan's caps.
    pub capped_entries: usize,
}

/// Decode one raw block access list and fold it into a [`BalSeed`] under
/// `plan` — the decode-first, mutate-never seam. The whole input must decode
/// (canonical [`DecodedBal`], trailing bytes rejected) before any entry is
/// planned, so malformed bytes yield [`BalDecodeError`] and an empty seed.
///
/// # Errors
///
/// [`BalDecodeError`] when `raw` is not exactly one EIP-7928 list.
pub fn decode_bal_seed(raw: &[u8], plan: &BalPlan) -> Result<BalSeed, BalDecodeError> {
    let decoded =
        DecodedBal::from_rlp_bytes_as::<Bal>(alloy::primitives::Bytes::copy_from_slice(raw))
            .map_err(|error| BalDecodeError(error.to_string()))?;
    let mut seed = BalSeed::default();
    for account in decoded.as_bal() {
        let entries = account.storage_post_states().count() + account.storage_reads().len();
        if !plan.admits(account.address) {
            seed.filtered_entries += entries;
            continue;
        }
        for (slot, value) in account.storage_post_states() {
            if !plan.accepts_write(seed.writes.len()) {
                seed.capped_entries += 1;
                continue;
            }
            seed.writes.push((account.address, slot, value));
        }
        for &slot in account.storage_reads() {
            if !plan.accepts_read(seed.reads.len()) {
                seed.capped_entries += 1;
                continue;
            }
            seed.reads.push((account.address, slot));
        }
    }
    Ok(seed)
}
