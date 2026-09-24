//! Transport-neutral hub event vocabulary.

use alloy::primitives::{Address, Bytes, B256, U256};

/// One unsigned pending transaction revealed by the `MEVBlocker` auction.
///
/// Field-for-field the frame `degenbot-rpc::backrun_feed` parses today; the
/// hub re-exports it as `BackrunFeedEvent` so no consumer's frame shape moved.
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

/// The classes an intake can emit. One hub registration exists per class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HubClass {
    /// A new block header (the head clock).
    NewHead,
    /// A pool-relevant log (the settled-block intake).
    PoolEvent,
    /// An observed pending transaction (watched feeds).
    PendingTx,
}

/// A hub event, carrying exactly the fields its source carries today.
#[derive(Debug, Clone)]
pub enum HubEvent {
    /// A new block header. Mirrors the ingestion emitter's fields.
    NewHead {
        /// Block number.
        number: u64,
        /// Block timestamp (seconds).
        timestamp: u64,
        /// Base fee per gas (`None` pre-EIP-1559).
        base_fee_per_gas: Option<u64>,
        /// Gas used by the block.
        gas_used: u64,
        /// Gas limit of the block.
        gas_limit: u64,
    },
    /// A pool-relevant log. Mirrors the ingestion emitter's `PoolEvent`
    /// (`epoch`, `log_index`, raw `Log` payload); `pool_kind` is a
    /// decode-time fact the intake does not know, so it is not carried here.
    PoolEvent {
        /// The block epoch the log belongs to.
        epoch: u64,
        /// On-chain log index (`None` when the node omits it).
        log_index: Option<u64>,
        /// The raw log payload.
        payload: alloy::rpc::types::Log,
    },
    /// An observed pending transaction.
    PendingTx(PendingTx),
}
