//! Transport-neutral hub event vocabulary.

use alloy::primitives::{Address, Bytes, B256, U256};

/// One unsigned pending transaction revealed by the `MEVBlocker` auction.
///
/// One frame of a pending-transaction feed: the feed parses the wire
/// notification into this vocabulary and publishes it on its source kind's
/// ring ([`crate::PendingTxPublisher`]).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingTx {
    /// The chain the frame is pinned to (the feed enforces one chain id).
    pub chain_id: u64,
    /// Sender address.
    pub from: Address,
    /// Recipient, or `None` for contract creation.
    pub to: Option<Address>,
    /// Native value transferred.
    pub value: U256,
    /// Calldata.
    pub data: Bytes,
    /// Gas limit.
    pub gas: u64,
    /// EIP-1559 max fee per gas (wei).
    pub max_fee_per_gas: u128,
    /// EIP-1559 max priority fee per gas (wei).
    pub max_priority_fee_per_gas: u128,
    /// Sender nonce.
    pub nonce: u64,
    /// Transaction hash.
    pub hash: B256,
    /// Raw access list as delivered (the wire JSON, not a re-derivation).
    pub access_list: serde_json::Value,
    /// EIP-2718 transaction type byte.
    pub tx_type: u8,
    /// Local receive time (unix ms) stamped by the feed.
    pub received_unix_ms: u64,
    /// The signed RLP wire bytes (`None` when the source reveals the frame
    /// unsigned — the `MEVBlocker` partial-pending stream). The txpool feed
    /// fills this so a bundle can carry the target's verbatim bytes.
    pub raw_signed_tx: Option<Bytes>,
}

/// Which pending-transaction feed a hub ring belongs to.
///
/// The two watched feeds publish the same frame vocabulary but never the same
/// events, and each arm's frame corpus must be exactly its own feed's — so the
/// hub keys its rings by source kind (one ring per value) instead of merging
/// both feeds into one ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PendingTxSource {
    /// The `MEVBlocker` searcher feed (the mevblocker arm).
    Mevblocker,
    /// The chain-node txpool feed (the txpool arm).
    Txpool,
}
