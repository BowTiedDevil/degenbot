//! BAL-accelerated anchored read prefetch: one anchor's EIP-7928 block access
//! list turned into [`ReadHub`](super::read_hub::ReadHub) assistance.
//!
//! A block access list records every storage slot the block touched. Two facts
//! make it useful to a hub anchored at that block: each written slot's LAST
//! change is the anchor's post-block value (the exact answer
//! `eth_getStorageAt` would give), and each read-only slot is a slot the fan-out
//! is likely to ask for — an identity worth joining the hub's next batch. This
//! module turns a list into those two sets; the hub's `seed_bal` /
//! `prefetch_bal` apply them through the ordinary memo and batch/drain paths.
//!
//! # One list, two official representations
//!
//! A node may serve the list as the RLP bytes of `debug_getRawBlockAccessList`
//! or as the official JSON array of `eth_getBlockAccessList`. [`fetch_bal_seed`]
//! is the ONE acquisition path both BAL consumers share — the read hub's
//! best-effort prefetch and the canonical head's parent-linked prepare — so
//! neither can be left on one representation. The provider prefers the raw
//! representation and falls back to the JSON one only when the node does not
//! serve the raw method; whichever answers, the SAME fold turns it into the same
//! [`BalSeed`], so the two codecs ([`decode_bal_seed`], [`decode_bal_seed_json`])
//! are interchangeable at every seam below them.
//!
//! # The contract is best-effort, all-or-nothing, storage-only
//!
//! Assistance never fails a read. A missing anchor hash (the request is
//! exact-hash anchored, never a mutable tag), a refused or failed request, or a
//! malformed list is a [`BalSkip`] with NOTHING seeded: the input decodes
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

use alloy::primitives::{Address, B256, U256};
use alloy_eip7928::bal::{Bal, DecodedBal};
use alloy_eip7928::AccountChanges;
use degenbot_rpc::provider::{AlloyProvider, BlockAccessListResponse};
use serde::Deserialize as _;

/// Why a BAL assist stage seeded nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BalSkip {
    /// The anchor carries no block hash, so the exact-hash request cannot be made.
    #[error("the anchor carries no block hash, so the exact-hash BAL request cannot be made")]
    NoAnchorHash,
    /// The block access list request failed: NEITHER representation served the
    /// method, a transport failure, no list at that hash — any classified
    /// provider error. A failure of the representation actually chosen is
    /// reported as-is and never retried through the other one.
    #[error("the block access list request failed: {0}")]
    Rpc(String),
    /// The answer did not decode as one complete EIP-7928 list: malformed RLP
    /// from the raw representation, malformed or ill-typed JSON from the
    /// official one. Nothing is seeded from either.
    #[error("the block access list did not decode: {0}")]
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

/// The answer did not decode as one complete EIP-7928 block access list.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("malformed block access list: {0}")]
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

/// Acquire ONE exact block's block access list and decode it into a [`BalSeed`]
/// under `plan` — the ONE acquisition path both production BAL consumers share.
///
/// The transport negotiation belongs to the provider
/// ([`AlloyProvider::get_block_access_list`]): the raw RLP representation is
/// asked first and the official JSON one only when the node does not serve the
/// raw method, a classification that is sticky for the provider's session.
/// Whichever representation answers, the decode is the same fold over the same
/// account-change model, so a raw list and the JSON list of one block are
/// indistinguishable below this function.
///
/// # Errors
///
/// [`BalSkip::Rpc`] when the acquisition itself failed — every failure of the
/// representation actually chosen, a `null` answer included, is the
/// acquisition's own and is NEVER retried through the other representation —
/// and [`BalSkip::Decode`] when the answer is not one complete list, in either
/// representation.
pub async fn fetch_bal_seed(
    provider: &AlloyProvider,
    block_hash: B256,
    plan: &BalPlan,
) -> Result<BalSeed, BalSkip> {
    let response = provider
        .get_block_access_list(block_hash)
        .await
        .map_err(|error| BalSkip::Rpc(error.to_string()))?;
    match response {
        BlockAccessListResponse::Raw(raw) => decode_bal_seed(&raw, plan),
        BlockAccessListResponse::Json(json) => decode_bal_seed_json(&json, plan),
    }
    .map_err(|error| BalSkip::Decode(error.to_string()))
}

/// Decode one RAW block access list (the `debug_getRawBlockAccessList`
/// representation) and fold it into a [`BalSeed`] under `plan` — the
/// decode-first, mutate-never seam. The whole input must decode (canonical
/// [`DecodedBal`], trailing bytes rejected) before any entry is planned, so
/// malformed bytes yield [`BalDecodeError`] and an empty seed.
///
/// # Errors
///
/// [`BalDecodeError`] when `raw` is not exactly one EIP-7928 list.
pub fn decode_bal_seed(raw: &[u8], plan: &BalPlan) -> Result<BalSeed, BalDecodeError> {
    let decoded =
        DecodedBal::from_rlp_bytes_as::<Bal>(alloy::primitives::Bytes::copy_from_slice(raw))
            .map_err(|error| BalDecodeError(error.to_string()))?;
    Ok(seed_from_accounts(decoded.as_bal(), plan))
}

/// Decode one OFFICIAL JSON block access list (the `eth_getBlockAccessList`
/// representation) and fold it into a [`BalSeed`] under `plan` — the same fold,
/// and therefore the same seed, as [`decode_bal_seed`]'s raw representation.
///
/// The official account shape carries all SIX fields (`address`,
/// `storageChanges`, `storageReads`, `balanceChanges`, `nonceChanges`,
/// `codeChanges`), and a missing field, a wrong type, or an unparseable quantity
/// is a [`BalDecodeError`] with an empty seed — the same closed behavior
/// malformed RLP gets. The three non-storage change lists are decoded (their
/// presence must never fail the list) and then ignored: balance/nonce/code
/// changes form no storage identity, so those accounts stay wholly on the
/// dynamic path.
///
/// # Errors
///
/// [`BalDecodeError`] when `json` is not one official block access list.
pub fn decode_bal_seed_json(
    json: &serde_json::Value,
    plan: &BalPlan,
) -> Result<BalSeed, BalDecodeError> {
    let bal = Bal::deserialize(json).map_err(|error| BalDecodeError(error.to_string()))?;
    Ok(seed_from_accounts(&bal, plan))
}

/// Fold one decoded list into a [`BalSeed`] under `plan` — the ONE fold both
/// representations share, so the raw bytes and the official JSON of one block
/// yield identical seeds (the same final writes, zeros included, and the same
/// read-only identities, in list order).
fn seed_from_accounts(accounts: &[AccountChanges], plan: &BalPlan) -> BalSeed {
    let mut seed = BalSeed::default();
    for account in accounts {
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
    seed
}
